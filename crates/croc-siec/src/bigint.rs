//! Go `math/big` 语义的整数助手。
//!
//! 这一层存在的唯一理由是逐字节兼容：croc 的 PAKE 线路值直接来自 Go
//! `math/big`，而 `big.Int` 的取模语义和 Rust 的 `%` 不一样。把这套差异封在三个
//! 小函数里，上面的曲线代码就能写得像普通数学，而不是满篇 `+ m` 的补丁。

use num_bigint::{BigInt, Sign};
use num_traits::{One, Signed, Zero};

/// Go `(*big.Int).Mod` 的语义：余数恒落在 `[0, |m|)`。
///
/// Rust 的 `%` 余数符号跟随被除数（`-7 % 3 == -1`），Go 的 `Mod` 跟随除数
/// （`-7 mod 3 == 2`）。SIEC255 的加法公式会算出负的中间值，这一步必须对齐，
/// 否则 `λ` 会差一个 `p`，进而 `x₃` 完全错位。
///
/// 前置条件：`modulus` 非零。
pub fn emod(value: &BigInt, modulus: &BigInt) -> BigInt {
    let remainder = value % modulus;
    if remainder.sign() == Sign::Minus { remainder + modulus } else { remainder }
}

/// Go `(*big.Int).ModInverse` 的语义：返回 `value⁻¹ mod |modulus|`，结果落在
/// `[0, |modulus|)`；当 `gcd(value, modulus) != 1` 时返回 `None`。
///
/// 用扩展欧几里得实现。Go 的 `ModInverse` 对负数被模数会先归一化，这里用 [`emod`]
/// 做同样的事。
pub fn mod_inverse(value: &BigInt, modulus: &BigInt) -> Option<BigInt> {
    if modulus.is_zero() {
        return None;
    }
    let modulus = modulus.abs();
    if modulus.is_one() {
        return Some(BigInt::zero());
    }
    let mut remainder = emod(value, &modulus);
    if remainder.is_zero() {
        return None;
    }
    let mut next_remainder = modulus.clone();
    let mut coefficient = BigInt::one();
    let mut next_coefficient = BigInt::zero();

    while !next_remainder.is_zero() {
        let quotient = &remainder / &next_remainder;
        let shifted_remainder = &remainder - &quotient * &next_remainder;
        remainder = std::mem::replace(&mut next_remainder, shifted_remainder);
        let shifted_coefficient = &coefficient - &quotient * &next_coefficient;
        coefficient = std::mem::replace(&mut next_coefficient, shifted_coefficient);
    }

    if !remainder.is_one() {
        return None;
    }
    Some(emod(&coefficient, &modulus))
}

/// Go `(*big.Int).Bytes()` 的语义：大端、无前导零、零值对应空切片。
///
/// `num-bigint` 的 `to_bytes_be()` 对零返回 `vec![0]`，Go 返回 `nil`。这个差异会直接
/// 体现在 PAKE 会话密钥的转录哈希里（`Xᵤ.Bytes()`），所以必须归一化。
pub fn go_bytes(value: &BigInt) -> Vec<u8> {
    if value.is_zero() {
        return Vec::new();
    }
    value.to_bytes_be().1
}

/// 定长大端编码（左补零），对应 Go `(*big.Int).FillBytes`。
///
/// 值比 `len` 宽时按 Go 的语义保留低位 `len` 字节（`FillBytes` 只在"一定放得下"时
/// 被调用，这里为了不 panic 选择截断）。
pub fn to_be_fixed(value: &BigInt, len: usize) -> Vec<u8> {
    let bytes = go_bytes(value);
    if bytes.len() >= len {
        let mut bytes = bytes;
        return bytes.split_off(bytes.len() - len);
    }
    let mut out = vec![0u8; len - bytes.len()];
    out.extend_from_slice(&bytes);
    out
}

/// 解析十进制或 `0x` 十六进制字面量，供曲线参数表使用。
pub fn parse_int(literal: &str) -> Option<BigInt> {
    let trimmed = literal.trim();
    let (digits, radix) =
        trimmed.strip_prefix("0x").or_else(|| trimmed.strip_prefix("0X")).map_or((trimmed, 10), |hex| (hex, 16));
    BigInt::parse_bytes(digits.as_bytes(), radix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn big(value: i64) -> BigInt {
        BigInt::from(value)
    }

    #[test]
    fn emod_matches_go_euclidean_semantics() {
        // Go: (-7).Mod(3) == 2
        assert_eq!(emod(&big(-7), &big(3)), big(2));
        assert_eq!(emod(&big(7), &big(3)), big(1));
        assert_eq!(emod(&big(0), &big(3)), big(0));
        assert_eq!(emod(&big(-3), &big(3)), big(0));
        // Rust 的 % 给出 -1，确认我们没有直接用它。
        assert_eq!(big(-7) % big(3), big(-1));
    }

    #[test]
    fn mod_inverse_round_trips() {
        let modulus = parse_int("28948022309329048855892746252183396360603931420023084536990047309120118726721")
            .expect("SIEC255 素数");
        for value in [big(1), big(2), big(5), big(12), &modulus - 1] {
            let inverse = mod_inverse(&value, &modulus).expect("与素数互素");
            assert_eq!(emod(&(&value * &inverse), &modulus), big(1));
        }
    }

    #[test]
    fn mod_inverse_handles_negative_input() {
        let modulus = big(7);
        // -3 ≡ 4 (mod 7)，4 的逆是 2（4*2 = 8 ≡ 1）。
        assert_eq!(mod_inverse(&big(-3), &modulus), Some(big(2)));
    }

    #[test]
    fn mod_inverse_rejects_non_coprime() {
        assert_eq!(mod_inverse(&big(6), &big(9)), None);
        assert_eq!(mod_inverse(&big(0), &big(7)), None);
    }

    #[test]
    fn go_bytes_is_empty_for_zero() {
        assert!(go_bytes(&big(0)).is_empty());
        assert_eq!(go_bytes(&big(255)), vec![0xff]);
        assert_eq!(go_bytes(&big(256)), vec![0x01, 0x00]);
    }

    #[test]
    fn to_be_fixed_left_pads() {
        assert_eq!(to_be_fixed(&big(1), 4), vec![0, 0, 0, 1]);
        assert_eq!(to_be_fixed(&big(0), 2), vec![0, 0]);
        assert_eq!(to_be_fixed(&big(0x0102), 2), vec![0x01, 0x02]);
        assert_eq!(to_be_fixed(&big(0x010203), 2), vec![0x02, 0x03]);
    }

    #[test]
    fn parse_int_reads_both_radixes() {
        assert_eq!(parse_int("19"), Some(big(19)));
        assert_eq!(parse_int("0x13"), Some(big(19)));
        assert_eq!(parse_int("  0X13 "), Some(big(19)));
        assert_eq!(parse_int("nope"), None);
    }
}
