//! 房间准入限流：滑动窗口，按来源地址与房间名分别计数。
//!
//! 对应 Go 的 `src/tcp/admission.go`。这个限流器防的不是暴力破解（中继密码校验在前），
//! 而是房间名枚举：不设限的话，一个客户端可以每秒尝试几十万个房间名，撞上别人正在
//! 等待的房间就能插进别人的传输。按房间名限流让"猜房间"变得不划算，按来源地址限流让
//! "批量猜"变得不划算。
//!
//! # 与 Go 版的一处刻意保留的细节
//!
//! Go 在来源地址超限时直接拒绝，但在房间超限时仍然把来源计数加上了。这不是
//! bug——它意味着"狂猜房间名"的人会更快耗尽自己的来源配额。这里照抄，并把它写成一条
//! 测试，免得日后有人"顺手修正"。

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// 每多少次检查做一次全表清理。Go 里写死 256。
const FULL_PRUNE_EVERY: u64 = 256;

/// 滑动窗口限流器。
#[derive(Debug)]
pub struct AdmissionLimiter {
    state: Mutex<State>,
    source_limit: usize,
    room_limit: usize,
    window: Duration,
}

#[derive(Debug, Default)]
struct State {
    sources: HashMap<String, VecDeque<Instant>>,
    rooms: HashMap<String, VecDeque<Instant>>,
    checks: u64,
}

impl AdmissionLimiter {
    /// 创建一个限流器。两个上限都会被夹到至少 1。
    #[must_use]
    pub fn new(source_limit: usize, room_limit: usize, window: Duration) -> Self {
        Self {
            state: Mutex::new(State::default()),
            source_limit: source_limit.max(1),
            room_limit: room_limit.max(1),
            window,
        }
    }

    /// 按当前时间判断是否放行。
    #[must_use]
    pub fn allow(&self, source: &str, room: &str) -> bool {
        self.allow_at(source, room, Instant::now())
    }

    /// 按指定时间判断是否放行。
    ///
    /// 暴露出来是为了让测试能推进时间而不真的睡觉——滑动窗口的边界行为（"刚好在窗口
    /// 边缘的那次算不算"）最容易写错，必须能精确验证。
    #[must_use]
    pub fn allow_at(&self, source: &str, room: &str, now: Instant) -> bool {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // 先把 `MutexGuard` 重借成一个普通的 `&mut State`：否则下面同时借 `sources`
        // 与 `rooms` 时会走 `DerefMut`，借用检查器认为是对同一个地方借了两次。
        let state = &mut *guard;
        let cutoff = now.checked_sub(self.window).unwrap_or(now);

        prune(&mut state.sources, source, cutoff);
        prune(&mut state.rooms, room, cutoff);

        let source_events = state.sources.get(source).map_or(0, VecDeque::len);
        if source_events >= self.source_limit {
            return false;
        }
        state.sources.entry(source.to_owned()).or_default().push_back(now);

        let room_events = state.rooms.get(room).map_or(0, VecDeque::len);
        if room_events >= self.room_limit {
            // 注意：来源计数已经加上去了。见模块文档。
            return false;
        }
        state.rooms.entry(room.to_owned()).or_default().push_back(now);

        state.checks = state.checks.wrapping_add(1);
        if state.checks.is_multiple_of(FULL_PRUNE_EVERY) {
            let (sources, rooms) = (&mut state.sources, &mut state.rooms);
            prune_all(sources, cutoff);
            prune_all(rooms, cutoff);
        }
        true
    }

    /// 当前跟踪的来源地址数（诊断用）。
    #[must_use]
    pub fn tracked_sources(&self) -> usize {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).sources.len()
    }
}

/// 去掉 `key` 对应队列里所有不晚于 `cutoff` 的时间点。
fn prune(map: &mut HashMap<String, VecDeque<Instant>>, key: &str, cutoff: Instant) {
    let Some(events) = map.get_mut(key) else {
        return;
    };
    while events.front().is_some_and(|event| *event <= cutoff) {
        events.pop_front();
    }
    if events.is_empty() {
        map.remove(key);
    }
}

/// 全表清理：把每个键的过期时间点去掉，空掉的键一并删除。
fn prune_all(map: &mut HashMap<String, VecDeque<Instant>>, cutoff: Instant) {
    map.retain(|_, events| {
        while events.front().is_some_and(|event| *event <= cutoff) {
            events.pop_front();
        }
        !events.is_empty()
    });
}

/// 把对端地址归一化成"来源标识"：去掉端口，并把 IPv4-mapped IPv6 还原成 IPv4。
///
/// 不做 `::ffff:1.2.3.4` → `1.2.3.4` 的还原，同一个客户端就能用两种写法拿到双份配额。
#[must_use]
pub fn canonical_source(peer: SocketAddr) -> String {
    match peer.ip() {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or_else(|| v6.to_string(), |v4| v4.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn the_source_limit_is_enforced() {
        let limiter = AdmissionLimiter::new(3, 100, Duration::from_secs(60));
        let now = Instant::now();
        for index in 0..3 {
            assert!(limiter.allow_at("1.2.3.4", &format!("room-{index}"), now), "第 {index} 次应放行");
        }
        assert!(!limiter.allow_at("1.2.3.4", "room-4", now), "超过来源上限应拒绝");
        // 换个来源不受影响。
        assert!(limiter.allow_at("5.6.7.8", "room-4", now));
    }

    #[test]
    fn the_room_limit_is_enforced_across_sources() {
        let limiter = AdmissionLimiter::new(100, 2, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("1.1.1.1", "popular", now));
        assert!(limiter.allow_at("2.2.2.2", "popular", now));
        assert!(!limiter.allow_at("3.3.3.3", "popular", now), "超过房间上限应拒绝");
    }

    #[test]
    fn a_denied_room_still_consumes_source_quota() {
        // Go 的行为：房间超限时来源计数已经加上了。这条测试把这个"意外但有意"的细节固定下来。
        let limiter = AdmissionLimiter::new(2, 1, Duration::from_secs(60));
        let now = Instant::now();
        assert!(limiter.allow_at("1.1.1.1", "popular", now));
        assert!(!limiter.allow_at("1.1.1.1", "popular", now), "房间超限");
        // 来源配额已经被上面那次消耗掉了。
        assert!(!limiter.allow_at("1.1.1.1", "another", now), "来源配额应已被消耗");
    }

    #[test]
    fn the_window_slides() {
        let limiter = AdmissionLimiter::new(1, 1, Duration::from_secs(60));
        let start = Instant::now();
        assert!(limiter.allow_at("1.1.1.1", "room", start));
        assert!(!limiter.allow_at("1.1.1.1", "room", start + Duration::from_secs(59)));
        // 窗口边缘：Go 用 `!event.After(cutoff)`，也就是"恰好等于 cutoff 的算过期"。
        assert!(limiter.allow_at("1.1.1.1", "room", start + Duration::from_secs(60)));
    }

    #[test]
    fn a_keys_own_history_is_pruned_on_each_check() {
        let limiter = AdmissionLimiter::new(5, 5, Duration::from_secs(10));
        let start = Instant::now();
        for index in 0..5 {
            assert!(limiter.allow_at("1.1.1.1", "room", start + Duration::from_secs(index)), "第 {index} 次应放行");
        }
        assert!(!limiter.allow_at("1.1.1.1", "room", start + Duration::from_secs(5)), "5 次之后应拒绝");
        // 窗口滑过去之后同一个来源又能进来——历史是按时间点逐个过期的，不是整批清零。
        assert!(limiter.allow_at("1.1.1.1", "room", start + Duration::from_secs(30)));
    }

    #[test]
    fn the_table_is_pruned_periodically_so_it_cannot_grow_forever() {
        // 只靠"访问某个键时顺手清它自己"是不够的：被猜过的房间名不会再被访问，会永久
        // 留在表里。所以每 256 次检查要做一次全表清理。
        let limiter = AdmissionLimiter::new(1, 1, Duration::from_secs(10));
        let start = Instant::now();
        for index in 0..300u64 {
            let source = format!("10.{}.{}.{}", index / 65536, (index / 256) % 256, index % 256);
            assert!(limiter.allow_at(&source, "room", start + Duration::from_secs(index * 20)));
        }
        assert!(limiter.tracked_sources() < 100, "全表清理没生效，表里还留着 {} 个来源", limiter.tracked_sources());
    }

    #[test]
    fn canonical_source_strips_the_port() {
        let a: SocketAddr = "203.0.113.7:1234".parse().expect("地址应合法");
        let b: SocketAddr = "203.0.113.7:5678".parse().expect("地址应合法");
        assert_eq!(canonical_source(a), canonical_source(b));
        assert_eq!(canonical_source(a), "203.0.113.7");
    }

    #[test]
    fn canonical_source_unmaps_ipv4_mapped_ipv6() {
        // 不做这一步的话，`::ffff:1.2.3.4` 和 `1.2.3.4` 会各拿一份配额。
        let mapped = SocketAddr::new(IpAddr::V6(Ipv4Addr::new(1, 2, 3, 4).to_ipv6_mapped()), 9000);
        assert_eq!(canonical_source(mapped), "1.2.3.4");
    }

    #[test]
    fn canonical_source_keeps_real_ipv6_distinct() {
        let v6: SocketAddr = "[2001:db8::1]:9000".parse().expect("地址应合法");
        assert_eq!(canonical_source(v6), "2001:db8::1");
    }
}
