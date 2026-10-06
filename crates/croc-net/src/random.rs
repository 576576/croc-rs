//! 操作系统随机源。
//!
//! [`croc_protocol::Randomness`] 的唯一生产实现。选 `getrandom` 而不是 `rand`：它没有
//! 任何中间抽象，在 Windows / Linux / macOS / Android 上都是同一套调用，而且不会像
//! `rand::thread_rng` 那样带一个线程局部状态——后者正是 `cargo xtask purity` 要拦的
//! 东西。
//!
//! 测试不要用这个类型：它的输出不可复现，会让握手测试变成"偶尔失败"的噪声源。测试里
//! 用逐字节递增的假实现（见 `croc-protocol::relay` 的测试）。

use croc_protocol::Randomness;

/// 从操作系统取随机字节。
#[derive(Debug, Clone, Copy, Default)]
pub struct OsRandomness;

impl Randomness for OsRandomness {
    // `getrandom::fill` 只会在"这个平台上根本没有随机源"时失败，那是不可恢复的环境
    // 错误；继续往下走只会拿弱随机去加密。所以这里直接 panic。
    //
    // 这不符合库代码"不 panic"的惯例，但把 `Randomness::fill` 改成返回 `Result` 会让
    // 每一个调用点都长出一条永远不会走的错误分支——而那个 trait 的语义本来就假设
    // "随机源可用"。
    #[expect(clippy::panic, reason = "没有可用随机源时继续运行只会产出不安全的密钥")]
    fn fill(&mut self, buffer: &mut [u8]) {
        if let Err(error) = getrandom::fill(buffer) {
            panic!("the operating system has no usable randomness source: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_actually_fills_the_buffer() {
        let mut first = [0u8; 32];
        let mut second = [0u8; 32];
        let mut source = OsRandomness;
        source.fill(&mut first);
        source.fill(&mut second);
        assert_ne!(first, [0u8; 32], "随机源没写入任何东西");
        assert_ne!(first, second, "两次取随机数得到了同样的结果");
    }

    #[test]
    fn an_empty_buffer_is_fine() {
        OsRandomness.fill(&mut []);
    }
}
