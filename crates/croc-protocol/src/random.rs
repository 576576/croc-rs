//! 随机字节的来源。
//!
//! 本 crate 是纯的：`cargo xtask purity` 不允许出现 `thread_rng` / `getrandom` 之类
//! 的调用。随机源由 `croc-engine` 注入，测试注入确定性序列——于是整条握手变成可以
//! 逐字节重放的纯状态转移，出错时能精确复现。
//!
//! # 为什么不直接收 `&[u8; N]`
//!
//! 握手需要的随机字节数量随路径而变（服务端只在收到 `ping` 之外的帧时才需要临时标量；
//! 口令错误时不签发 banner nonce）。把"取随机数"做成按需接口，比让调用方预判路径、
//! 提前抽一堆用不上的随机数更诚实。

/// 一个可以按需填充随机字节的来源。
///
/// 实现方**必须**用密码学安全的随机源。这里的 `fill` 在 trait 上无法强制这一点，
/// 所以生产实现（`croc-engine` 里的 `OsRandomness`）在文档里写明来源，测试实现则
/// 明确标注"仅测试"。
pub trait Randomness {
    /// 用随机字节填满 `buffer`。
    fn fill(&mut self, buffer: &mut [u8]);
}

/// 取一个 `N` 字节的随机数组。
pub fn array<const N: usize>(randomness: &mut impl Randomness) -> [u8; N] {
    let mut buffer = [0u8; N];
    randomness.fill(&mut buffer);
    buffer
}

/// 取一个 AEAD nonce。
///
/// AES-GCM 的 nonce 一旦复用，攻击者可以 XOR 两条明文并恢复认证密钥，所以每次调用
/// 都必须拿到全新的字节——这也是为什么这里没有"计数器式"实现。
pub fn nonce(randomness: &mut impl Randomness) -> [u8; crate::codec::AEAD_NONCE_LEN] {
    array(randomness)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Counting(u8);

    impl Randomness for Counting {
        fn fill(&mut self, buffer: &mut [u8]) {
            for byte in buffer.iter_mut() {
                *byte = self.0;
                self.0 = self.0.wrapping_add(1);
            }
        }
    }

    #[test]
    fn array_fills_the_whole_buffer() {
        let value: [u8; 4] = array(&mut Counting(0x10));
        assert_eq!(value, [0x10, 0x11, 0x12, 0x13]);
    }

    #[test]
    fn nonce_has_the_aead_length() {
        assert_eq!(nonce(&mut Counting(0)).len(), crate::codec::AEAD_NONCE_LEN);
    }
}
