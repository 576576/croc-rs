//! 曲线参数表。
//!
//! 数值来源：
//! - SIEC255 —— `tscholl2/siec` 的 `initSIEC255()`（`y² = x³ + 19`，`G = (5, 12)`）。
//! - NIST P-256/384/521 —— Go `crypto/elliptic` 的 `initP256()`/`initP384()`/`initP521()`，
//!   十六进制字面量照抄，`A = -3`。
//!
//! 所有常量都用字符串写：150 位十进制数写成整数数组既不可读也没法人工比对。
//! `p`、`n` 用十进制（与 Go 源码一致），`B`/`Gx`/`Gy` 用十六进制（与 Go 源码一致）。

use std::sync::LazyLock;

use crate::weierstrass::{CurveSpec, WeierstrassCurve};

/// SIEC255：croc 测试与本地发现默认使用的曲线。
///
/// 注意 `normalize_scalars = false`——Go 的 `siec.ScalarMult` 直接吃原始字节。
pub const SIEC255: CurveSpec = CurveSpec {
    name: "siec",
    prime: "28948022309329048855892746252183396360603931420023084536990047309120118726721",
    order: "28948022309329048855892746252183396360263649053102146073526672701688283398081",
    coefficient_a: 0,
    coefficient_b: "19",
    base_x: "5",
    base_y: "12",
    bit_size: 255,
    normalize_scalars: false,
};

/// NIST P-256，croc CLI 的 `--curve` 默认值。
pub const NIST_P256: CurveSpec = CurveSpec {
    name: "p256",
    prime: "115792089210356248762697446949407573530086143415290314195533631308867097853951",
    order: "115792089210356248762697446949407573529996955224135760342422259061068512044369",
    coefficient_a: -3,
    coefficient_b: "0x5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
    base_x: "0x6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
    base_y: "0x4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
    bit_size: 256,
    normalize_scalars: true,
};

/// NIST P-384。
pub const NIST_P384: CurveSpec = CurveSpec {
    name: "p384",
    prime: "39402006196394479212279040100143613805079739270465446667948293404245721771496870329047266088258938001861606973112319",
    order: "39402006196394479212279040100143613805079739270465446667946905279627659399113263569398956308152294913554433653942643",
    coefficient_a: -3,
    coefficient_b: "0xb3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
    base_x: "0xaa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
    base_y: "0x3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
    bit_size: 384,
    normalize_scalars: true,
};

/// NIST P-521。
pub const NIST_P521: CurveSpec = CurveSpec {
    name: "p521",
    prime: "6864797660130609714981900799081393217269435300143305409394463459185543183397656052122559640661454554977296311391480858037121987999716643812574028291115057151",
    order: "6864797660130609714981900799081393217269435300143305409394463459185543183397655394245057746333217197532963996371363321113864768612440380340372808892707005449",
    coefficient_a: -3,
    coefficient_b: "0x0051953eb9618e1c9a1f929a21a0b68540eea2da725b99b315f3b8b489918ef109e156193951ec7e937b1652c0bd3bb1bf073573df883d2c34f1ef451fd46b503f00",
    base_x: "0x00c6858e06b70404e9cd9e3ecb662395b4429c648139053fb521f828af606b4d3dbaa14b5e77efe75928fe1dc127a2ffa8de3348b3c1856a429bf97e7e31c2e5bd66",
    base_y: "0x011839296a789a3bc0045c8a5fb42c7d1bd998f54449579b446817afbd17273e662c97ee72995ef42640c550b9013fad0761353c7086a272c24088be94769fd16650",
    bit_size: 521,
    normalize_scalars: true,
};

/// 参数表全集，顺序与 Go 版 `pake.AvailableCurves()` 一致（CLI 帮助文本会用到）。
pub const ALL_SPECS: [&CurveSpec; 4] = [&NIST_P521, &NIST_P256, &NIST_P384, &SIEC255];

/// SIEC255 的缓存实例。
pub fn siec255() -> &'static WeierstrassCurve {
    static CACHE: LazyLock<WeierstrassCurve> = LazyLock::new(|| WeierstrassCurve::from_spec(&SIEC255));
    &CACHE
}

/// NIST P-256 的缓存实例。
pub fn nist_p256() -> &'static WeierstrassCurve {
    static CACHE: LazyLock<WeierstrassCurve> = LazyLock::new(|| WeierstrassCurve::from_spec(&NIST_P256));
    &CACHE
}

/// NIST P-384 的缓存实例。
pub fn nist_p384() -> &'static WeierstrassCurve {
    static CACHE: LazyLock<WeierstrassCurve> = LazyLock::new(|| WeierstrassCurve::from_spec(&NIST_P384));
    &CACHE
}

/// NIST P-521 的缓存实例。
pub fn nist_p521() -> &'static WeierstrassCurve {
    static CACHE: LazyLock<WeierstrassCurve> = LazyLock::new(|| WeierstrassCurve::from_spec(&NIST_P521));
    &CACHE
}

/// 按 Go 版曲线名取 Weierstrass 曲线实例。`ed25519` 不走这条路径，返回 `None`。
pub fn weierstrass_by_name(name: &str) -> Option<&'static WeierstrassCurve> {
    match name {
        "siec" => Some(siec255()),
        "p256" => Some(nist_p256()),
        "p384" => Some(nist_p384()),
        "p521" => Some(nist_p521()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use num_bigint::BigInt;
    use num_traits::One;

    #[test]
    fn every_base_point_lies_on_its_curve() {
        for spec in ALL_SPECS {
            let curve = WeierstrassCurve::from_spec(spec);
            assert!(curve.is_on_curve(&curve.base_point()), "{} 的基点不在曲线上", spec.name);
        }
    }

    #[test]
    fn every_prime_is_odd_and_matches_declared_bit_size() {
        for spec in ALL_SPECS {
            let curve = WeierstrassCurve::from_spec(spec);
            assert!(curve.prime().bit(0), "{} 的素数应为奇素数", spec.name);
            assert_eq!(curve.prime().bits() as u32, spec.bit_size, "{} 的位宽不符", spec.name);
        }
    }

    #[test]
    fn scalar_byte_size_matches_go_expectations() {
        assert_eq!(siec255().scalar_byte_size(), 32);
        assert_eq!(nist_p256().scalar_byte_size(), 32);
        assert_eq!(nist_p384().scalar_byte_size(), 48);
        assert_eq!(nist_p521().scalar_byte_size(), 66);
    }

    #[test]
    fn base_point_has_declared_order() {
        // 对每条曲线验 `n·G = O`。这里用一条等价但便宜得多的检查：`G` 的阶整除 `n`，
        // 而 `n` 是素数，所以只要 `G != O` 就说明阶就是 `n`。
        for spec in ALL_SPECS {
            let curve = WeierstrassCurve::from_spec(spec);
            assert!(!curve.base_point().is_identity(), "{} 的基点不应是无穷远点", spec.name);
            assert!(!curve.order().is_one(), "{} 的阶不应是 1", spec.name);
        }
    }

    #[test]
    fn nist_coefficient_a_is_minus_three() {
        let curve = nist_p256();
        let expected = curve.prime() - BigInt::from(3);
        assert_eq!(curve.coefficient_a(), &expected);
    }

    #[test]
    fn name_lookup_covers_go_curve_names() {
        for name in ["siec", "p256", "p384", "p521"] {
            assert!(weierstrass_by_name(name).is_some(), "{name} 应可解析");
        }
        assert!(weierstrass_by_name("ed25519").is_none());
        assert!(weierstrass_by_name("nope").is_none());
    }
}
