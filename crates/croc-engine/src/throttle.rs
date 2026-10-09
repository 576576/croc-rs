//! `--throttleUpload`：发送方向的令牌桶限速。
//!
//! 参数与 Go 的 `rate.NewLimiter(rate.Every(time.Second/limit), burst)`
//! 对齐：速率 `limit` 字节/秒，桶容量取 `max(65536, limit)`（Go 的
//! `models.TCP_BUFFER_SIZE` 与 `uploadLimit` 取大者）。
//!
//! 桶初始是满的，这一点不是细节：一个 100 kB/s 的限制如果从空桶起步，头一个 64 KiB
//! 的块就要白等 0.64 秒才开始发，对端会先以为断了。Go 的 `rate.NewLimiter` 同样是满桶。
//!
//! # 令牌是允许为负的
//!
//! 这是 Go 的 `rate.Limiter.ReserveN` 的做法，也是唯一不会让速率翻倍的做法：领走一笔
//! 就走成负数，等负数按速率自己涨回 0 才放行。反过来，如果领不动时把余额清成 0
//! 再睡一觉，那段时间长出来的令牌会在下一次 `reserve` 里按 `elapsed` 再算一遍——
//! 实测等效速率正好是两倍。多条流共享一个桶时更糟：每条流都在重复这件事。

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// 桶容量下限。Go 的 `models.TCP_BUFFER_SIZE`——一个块最大就这么大，桶比它小会让
/// 每个块都得等，等于把"突发"这件事取消掉。
pub const BURST_FLOOR: u64 = 64 * 1024;

/// 桶容量：速率与 [`BURST_FLOOR`] 取大者。
#[must_use]
pub fn burst_for(bytes_per_second: u64) -> u64 {
    bytes_per_second.max(BURST_FLOOR)
}

/// 令牌桶的当前状态。
#[derive(Debug)]
struct State {
    /// 桶里的令牌（字节）。可以是负数：那是已经领超、等着按速率补上的欠账。
    tokens: f64,
    /// 上次结算的时刻。
    updated: Instant,
}

/// 跨流共享的上行限速器。
///
/// 数据面有多条流，Go 那边也只有一个 `c.limiter`，所以限速是
/// "整个上传"的速率，不是"每条流"的——否则 `--throttleUpload 100k` 在 4 条流下会跑出
/// 400 kB/s。
#[derive(Debug)]
pub struct Throttle {
    /// 每秒放行的字节数。
    rate: f64,
    /// 桶容量（突发上限）。
    capacity: f64,
    state: Mutex<State>,
}

impl Throttle {
    /// 建一个初始满桶的限速器。
    #[must_use]
    pub fn new(bytes_per_second: u64) -> Self {
        let rate = bytes_per_second as f64;
        let capacity = burst_for(bytes_per_second) as f64;
        Self { rate, capacity, state: Mutex::new(State { tokens: capacity, updated: Instant::now() }) }
    }

    /// 领 `bytes` 个令牌，返回需要等多久才能发。
    ///
    /// 先扣费、后等待：调用方拿到时长再自己 sleep。这样一条流在睡的时候，另一条流
    /// 拿到的是已经扣过费的桶——限速是对整次上传生效的。
    #[must_use]
    pub fn reserve(&self, bytes: u64) -> Duration {
        // 锁中毒说明别处 panic 过。这时不能返回零延迟——那等于把限速关掉，
        // 用户给的是"别占满带宽"，静默失效比报错更难查。取出内层状态继续算。
        let mut state = self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(state.updated).as_secs_f64();
        state.updated = now;
        state.tokens = (state.tokens + elapsed * self.rate).min(self.capacity);

        state.tokens -= bytes as f64;
        if state.tokens >= 0.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64(-state.tokens / self.rate)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_burst_floor_matches_go() {
        // Go: `minBurstSize := models.TCP_BUFFER_SIZE; if int(uploadLimit) > minBurstSize {...}`
        assert_eq!(burst_for(1), BURST_FLOOR);
        assert_eq!(burst_for(BURST_FLOOR - 1), BURST_FLOOR);
        assert_eq!(burst_for(BURST_FLOOR), BURST_FLOOR);
        // 100 KiB > 64 KiB，所以桶就是它自己。
        assert_eq!(burst_for(100 * 1024), 100 * 1024);
        assert_eq!(burst_for(BURST_FLOOR + 1), BURST_FLOOR + 1);
        assert_eq!(burst_for(4 * 1024 * 1024), 4 * 1024 * 1024);
    }

    #[test]
    fn the_bucket_starts_full() {
        // 速率低于下限时桶按 64 KiB 算，所以一个满块不该有任何等待。
        let throttle = Throttle::new(1000);
        assert_eq!(throttle.reserve(BURST_FLOOR), Duration::ZERO);
    }

    #[test]
    fn draining_the_bucket_costs_one_rate_window() {
        let throttle = Throttle::new(1000);
        assert_eq!(throttle.reserve(BURST_FLOOR), Duration::ZERO);
        // 桶已空，再要 1000 字节就得等满一秒。留出一点余量：调用之间流逝的时间会按
        // 速率回补少量令牌。
        let delay = throttle.reserve(1000);
        assert!(delay <= Duration::from_secs(1), "不该超过一个速率窗口：{delay:?}");
        assert!(delay >= Duration::from_millis(900), "不该明显短于一个速率窗口：{delay:?}");
    }

    #[test]
    fn the_aggregate_rate_is_not_doubled() {
        // 盯的是"等待期间长出来的令牌被算两遍"那个错。只能实测耗时：把余额清成 0 的
        // 写法只在跨 reserve 的真实等待里露馅——一次 reserve 的返回值看不出区别。
        let rate = 100_000;
        let throttle = Throttle::new(rate);
        assert_eq!(throttle.reserve(burst_for(rate)), Duration::ZERO, "先把桶清空");
        let (chunk, rounds) = (20_000_u64, 4);
        let start = std::time::Instant::now();
        for _ in 0..rounds {
            std::thread::sleep(throttle.reserve(chunk));
        }
        let elapsed = start.elapsed();
        let expected = Duration::from_secs_f64((rounds * chunk) as f64 / rate as f64);
        assert!(elapsed >= expected.mul_f64(0.8), "等效速率偏高：{elapsed:?}，应接近 {expected:?}");
    }

    #[test]
    fn a_large_rate_uses_its_own_burst() {
        // 速率高于下限时桶就是速率本身，所以一个 2 MiB 的突发能一次放行。
        let rate = 8 * 1024 * 1024;
        let throttle = Throttle::new(rate);
        assert_eq!(throttle.reserve(rate), Duration::ZERO);
    }
}
