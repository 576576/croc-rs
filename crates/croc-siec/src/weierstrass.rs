//! 通用短 Weierstrass 曲线算术：`y² = x³ + A·x + B (mod p)`。
//!
//! 用一条实现覆盖 SIEC255 与 NIST P-256/384/521，理由是**参考实现的输出只取决于
//! 仿射坐标**：Go 的 `crypto/elliptic` 在 P 曲线上走 `nistec`（Jacobian，恒定时间），
//! `tscholl2/siec` 走朴素仿射公式，但两者吐出来的 `(x, y)` 是同一个数。
//!
//! 这里选「仿射坐标 + 扩展欧几里得」：一次点加一次求逆，对 PAKE 那几百次点运算来说
//! 完全够用，而且公式能和 Go 源码逐行对上，便于核对。
//!
//! **标量约简的不对称性是契约的一部分**：Go 的 `nistCurve.normalizeScalar` 只对
//! *过长* 的标量取模，*过短* 的只左补零；而 `siec.ScalarMult` **完全不做约简**。
//! 两条路径都用 [`WeierstrassCurve::normalize_scalars`] 开关区分，见
//! `docs/agent/plan/02-crypto.md` §2.2。

use num_bigint::{BigInt, Sign};
use num_traits::Zero;

use crate::affine::AffinePoint;
use crate::bigint::{emod, go_bytes, mod_inverse, to_be_fixed};

/// 一条短 Weierstrass 曲线的全部参数。
///
/// 字段私有：外部只能通过 [`crate::params`] 里那几个已验证过的常量拿到实例。
#[derive(Clone, Debug)]
pub struct WeierstrassCurve {
    name: &'static str,
    prime: BigInt,
    order: BigInt,
    coefficient_a: BigInt,
    coefficient_b: BigInt,
    base_x: BigInt,
    base_y: BigInt,
    bit_size: u32,
    normalize_scalars: bool,
}

/// 曲线参数的原始字面量形式。
///
/// 所有数值都用字符串给出：曲线常量动辄 150 位十进制，写成整数数组既不可读也没法
/// 人工核对。`a` 用 `i64`，因为 croc 用到的曲线只有 `A = 0`（SIEC255）和 `A = -3`
/// （NIST 系列）两种。
#[derive(Clone, Copy, Debug)]
pub struct CurveSpec {
    /// 曲线名，必须与 Go 版 `pake.AvailableCurves()` 中的字符串一致。
    pub name: &'static str,
    /// 域特征 `p`。
    pub prime: &'static str,
    /// 基点阶 `n`。
    pub order: &'static str,
    /// 曲线系数 `A`。
    pub coefficient_a: i64,
    /// 曲线系数 `B`。
    pub coefficient_b: &'static str,
    /// 基点 x 坐标。
    pub base_x: &'static str,
    /// 基点 y 坐标。
    pub base_y: &'static str,
    /// 域位宽，只用于自检。
    pub bit_size: u32,
    /// 是否按 Go `nistCurve.normalizeScalar` 预处理标量。
    pub normalize_scalars: bool,
}

impl WeierstrassCurve {
    /// 从字面量参数表构造曲线。
    ///
    /// 字面量解析失败会退化成 0 而不是 panic——参数表是编译期常量，写错了由
    /// `params` 模块里的自检测试（基点在曲线上）兜住，这样生产路径上没有任何
    /// `unwrap`/`panic`。
    pub fn from_spec(spec: &CurveSpec) -> Self {
        let prime = crate::bigint::parse_int(spec.prime).unwrap_or_default();
        Self {
            name: spec.name,
            order: crate::bigint::parse_int(spec.order).unwrap_or_default(),
            coefficient_a: emod(&BigInt::from(spec.coefficient_a), &prime),
            coefficient_b: crate::bigint::parse_int(spec.coefficient_b).unwrap_or_default(),
            base_x: crate::bigint::parse_int(spec.base_x).unwrap_or_default(),
            base_y: crate::bigint::parse_int(spec.base_y).unwrap_or_default(),
            prime,
            bit_size: spec.bit_size,
            normalize_scalars: spec.normalize_scalars,
        }
    }

    /// 曲线名（与 Go 版 `--curve` 取值一致）。
    pub fn name(&self) -> &'static str {
        self.name
    }

    /// 域特征 `p`。
    pub fn prime(&self) -> &BigInt {
        &self.prime
    }

    /// 基点阶 `n`。
    pub fn order(&self) -> &BigInt {
        &self.order
    }

    /// 曲线系数 `A`（已约简到 `[0, p)`）。
    pub fn coefficient_a(&self) -> &BigInt {
        &self.coefficient_a
    }

    /// 曲线系数 `B`。
    pub fn coefficient_b(&self) -> &BigInt {
        &self.coefficient_b
    }

    /// 域位宽。
    pub fn bit_size(&self) -> u32 {
        self.bit_size
    }

    /// 是否按 Go `nistCurve.normalizeScalar` 预处理标量。
    pub fn normalize_scalars(&self) -> bool {
        self.normalize_scalars
    }

    /// 基点 `G`。
    pub fn base_point(&self) -> AffinePoint {
        AffinePoint::new(self.base_x.clone(), self.base_y.clone())
    }

    /// `(x, y)` 是否落在曲线上。
    ///
    /// `(0, 0)` 一律判否：Go 的 `nistCurve.IsOnCurve` 显式拒绝它，而 SIEC255 的
    /// `B = 19 != 0` 让它自然不成立。
    pub fn is_on_curve(&self, point: &AffinePoint) -> bool {
        if point.is_identity() {
            return false;
        }
        let left = emod(&(point.y() * point.y()), &self.prime);
        let x_squared = point.x() * point.x();
        let right = emod(&(x_squared * point.x() + &self.coefficient_a * point.x() + &self.coefficient_b), &self.prime);
        left == right
    }

    /// 点的加法逆元 `-P = (x, -y)`。
    pub fn negate(&self, point: &AffinePoint) -> AffinePoint {
        AffinePoint::new(point.x().clone(), emod(&(-point.y()), &self.prime))
    }

    /// 点加，对应 Go `Add(x1, y1, x2, y2)`。
    pub fn add(&self, first: &AffinePoint, second: &AffinePoint) -> AffinePoint {
        if first.is_identity() {
            return second.clone();
        }
        if second.is_identity() {
            return first.clone();
        }
        if first.x() == second.x() && first.y() == second.y() {
            return self.double(first);
        }
        let delta_x = second.x() - first.x();
        if delta_x.is_zero() {
            // 同 x 异 y：两点互为逆元，和为无穷远点。
            return AffinePoint::identity();
        }
        let Some(inverse) = mod_inverse(&delta_x, &self.prime) else {
            return AffinePoint::identity();
        };
        let slope = emod(&((second.y() - first.y()) * inverse), &self.prime);
        let x3 = emod(&(&slope * &slope - first.x() - second.x()), &self.prime);
        let y3 = emod(&(&slope * (first.x() - &x3) - first.y()), &self.prime);
        AffinePoint::new(x3, y3)
    }

    /// 点倍，对应 Go `Double(x1, y1)`。
    ///
    /// 切线斜率 `λ = (3x² + A) / (2y)`——注意分子是 `3x² + A`，**不是** `3x² + A·x`：
    /// 那是 `x³ + A·x + B` 的导数。SIEC255 的 `A = 0` 会让这个笔误完全隐身，只有
    /// NIST 系列（`A = -3`）才暴露出来，所以测试里必须有一条非零 `A` 的用例。
    /// `y == 0` 时切线与曲线交于无穷远点。
    pub fn double(&self, point: &AffinePoint) -> AffinePoint {
        if point.y().is_zero() {
            return AffinePoint::identity();
        }
        let numerator = emod(&(BigInt::from(3) * (point.x() * point.x()) + &self.coefficient_a), &self.prime);
        let denominator = emod(&(BigInt::from(2) * point.y()), &self.prime);
        let Some(inverse) = mod_inverse(&denominator, &self.prime) else {
            return AffinePoint::identity();
        };
        // Go 在这一步之后没有再取模，靠后面的 `Exp(λ, 2, p)` 顺带约简。这里保留
        // 不约简的写法，让两边表达式逐项对应。
        let slope = numerator * inverse;
        let x3 = emod(&(&slope * &slope - point.x() - point.x()), &self.prime);
        let y3 = emod(&(&slope * (point.x() - &x3) - point.y()), &self.prime);
        AffinePoint::new(x3, y3)
    }

    /// 标量乘，MSB-first double-and-add。
    ///
    /// **不做任何约简**——这是 `tscholl2/siec` 的行为，而 `siec` 是 croc 测试里
    /// 用得最多的曲线。NIST 曲线的约简由 [`Self::normalize_scalar`] 在调用前完成。
    pub fn scalar_mult(&self, point: &AffinePoint, scalar: &[u8]) -> AffinePoint {
        let mut accumulator = AffinePoint::identity();
        for byte in scalar {
            let mut bits = *byte;
            for _ in 0..8 {
                accumulator = self.double(&accumulator);
                if bits & 0x80 != 0 {
                    accumulator = self.add(point, &accumulator);
                }
                bits <<= 1;
            }
        }
        accumulator
    }

    /// 基点标量乘 `k·G`。
    pub fn scalar_mult_base(&self, scalar: &[u8]) -> AffinePoint {
        self.scalar_mult(&self.base_point(), scalar)
    }

    /// 阶的字节宽度，即 Go `normalizeScalar` 里的 `byteSize`。
    pub fn scalar_byte_size(&self) -> usize {
        (self.order.bits() as usize).div_ceil(8)
    }

    /// 按 Go `nistCurve.normalizeScalar` 预处理标量。
    ///
    /// 注意那个不对称：`len == byteSize` 原样返回；`len < byteSize` 只左补零；
    /// `len > byteSize` 才取模。写错这一条，`--curve p256` 的互通就会在最外层
    /// 的字节上翻车。
    pub fn normalize_scalar(&self, scalar: &[u8]) -> Vec<u8> {
        let byte_size = self.scalar_byte_size();
        if scalar.len() == byte_size {
            return scalar.to_vec();
        }
        let value = BigInt::from_bytes_be(Sign::Plus, scalar);
        let value = if scalar.len() > byte_size { emod(&value, &self.order) } else { value };
        to_be_fixed(&value, byte_size)
    }

    /// 大端字节编码（Go `(*big.Int).Bytes()`），供转录哈希使用。
    pub fn encode_coordinate(value: &BigInt) -> Vec<u8> {
        go_bytes(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::params::{NIST_P256, NIST_P384, NIST_P521, SIEC255};

    fn curve() -> WeierstrassCurve {
        WeierstrassCurve::from_spec(&SIEC255)
    }

    #[test]
    fn siec255_base_point_is_on_curve() {
        let curve = curve();
        assert!(curve.is_on_curve(&curve.base_point()));
        assert_eq!(curve.base_point().x(), &BigInt::from(5));
        assert_eq!(curve.base_point().y(), &BigInt::from(12));
    }

    #[test]
    fn p256_base_point_is_on_curve() {
        let curve = WeierstrassCurve::from_spec(&NIST_P256);
        assert!(curve.is_on_curve(&curve.base_point()));
        assert_eq!(curve.scalar_byte_size(), 32);
    }

    #[test]
    fn identity_is_not_on_curve() {
        let curve = curve();
        assert!(!curve.is_on_curve(&AffinePoint::identity()));
    }

    #[test]
    fn identity_is_additive_unit() {
        let curve = curve();
        let g = curve.base_point();
        assert_eq!(curve.add(&AffinePoint::identity(), &g), g);
        assert_eq!(curve.add(&g, &AffinePoint::identity()), g);
    }

    #[test]
    fn adding_a_point_to_its_negation_yields_identity() {
        let curve = curve();
        let g = curve.base_point();
        assert_eq!(curve.add(&g, &curve.negate(&g)), AffinePoint::identity());
    }

    #[test]
    fn group_law_holds_for_small_scalars() {
        let curve = curve();
        let g = curve.base_point();
        for left in 1u8..12 {
            for right in 1u8..12 {
                let sum_of_scalars = curve.scalar_mult(&g, &[left + right]);
                let sum_of_points = curve.add(&curve.scalar_mult(&g, &[left]), &curve.scalar_mult(&g, &[right]));
                assert_eq!(sum_of_scalars, sum_of_points, "{left} + {right}");
            }
        }
    }

    #[test]
    fn scalar_mult_is_commutative_in_scalar_multiplication() {
        let curve = curve();
        let g = curve.base_point();
        let left = curve.scalar_mult(&g, &[3]);
        assert_eq!(curve.scalar_mult(&left, &[5]), curve.scalar_mult(&g, &[15]));
    }

    #[test]
    fn group_law_holds_for_curves_with_nonzero_coefficient_a() {
        // 这几条曲线的 `A = -3`。`double` 的分子必须是 `3x² + A`，写成 `3x² + A·x`
        // 在 `A = 0` 的 SIEC255 上完全看不出来——这就是这条测试存在的理由。
        for spec in [&NIST_P256, &NIST_P384, &NIST_P521] {
            let curve = WeierstrassCurve::from_spec(spec);
            let g = curve.base_point();
            assert!(curve.is_on_curve(&curve.double(&g)), "{} 的 2G 不在曲线上", spec.name);
            for left in 1u8..8 {
                for right in 1u8..8 {
                    let sum_of_scalars = curve.scalar_mult(&g, &[left + right]);
                    let sum_of_points = curve.add(&curve.scalar_mult(&g, &[left]), &curve.scalar_mult(&g, &[right]));
                    assert_eq!(sum_of_scalars, sum_of_points, "{} 上 {left}G + {right}G", spec.name);
                }
            }
        }
    }

    #[test]
    fn scalar_mult_lands_on_curve_for_full_width_scalars() {
        for spec in [&NIST_P256, &NIST_P384, &NIST_P521, &SIEC255] {
            let curve = WeierstrassCurve::from_spec(spec);
            let normalized = curve.normalize_scalar(&[0x11u8; 32]);
            assert_eq!(normalized.len(), curve.scalar_byte_size(), "{} 的标量宽度", spec.name);
            let point = curve.scalar_mult_base(&normalized);
            assert!(curve.is_on_curve(&point), "{} 的 [k]G 不在曲线上", spec.name);
        }
    }

    #[test]
    fn scalar_mult_by_zero_is_identity() {
        let curve = curve();
        assert_eq!(curve.scalar_mult(&curve.base_point(), &[0]), AffinePoint::identity());
        assert_eq!(curve.scalar_mult(&curve.base_point(), &[]), AffinePoint::identity());
    }

    #[test]
    fn doubling_matches_addition() {
        let curve = curve();
        let g = curve.base_point();
        assert_eq!(curve.double(&g), curve.add(&g, &g));
    }

    #[test]
    fn normalize_scalar_pads_short_input() {
        let curve = WeierstrassCurve::from_spec(&NIST_P256);
        // 32 字节原样返回。
        let full = vec![0x42u8; 32];
        assert_eq!(curve.normalize_scalar(&full), full);
        // 过短：左补零，不取模。
        let short = vec![0x01u8, 0x02];
        let padded = curve.normalize_scalar(&short);
        assert_eq!(padded.len(), 32);
        assert_eq!(padded.first(), Some(&0u8));
        assert_eq!(padded.last(), Some(&0x02u8));
    }

    #[test]
    fn normalize_scalar_reduces_overlong_input() {
        let curve = WeierstrassCurve::from_spec(&NIST_P256);
        let overlong = vec![0xffu8; 40];
        let normalized = curve.normalize_scalar(&overlong);
        assert_eq!(normalized.len(), 32);
        let value = BigInt::from_bytes_be(Sign::Plus, &normalized);
        assert!(value < *curve.order());
    }

    #[test]
    fn siec_scalars_are_never_normalized() {
        // SIEC255 的开关必须是 false，否则和 Go 版 `siec.ScalarMult` 分道扬镳。
        assert!(!curve().normalize_scalars());
        assert!(WeierstrassCurve::from_spec(&NIST_P256).normalize_scalars());
    }
}
