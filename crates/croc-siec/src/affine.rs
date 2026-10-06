//! 仿射坐标上的椭圆曲线点。
//!
//! 无穷远点按 Go 的约定编码为 `(0, 0)`：Go 版 `tscholl2/siec` 用 `x.BitLen() == 0 &&
//! y.BitLen() == 0` 判定无穷远，`crypto/elliptic` 的 `pointToAffine` 也把无穷远点
//! 还原成 `(0, 0)`。这个表示没有歧义——本 crate 涉及的四条 Weierstrass 曲线的 `B`
//! 都不为 0，`y² = x³ + Ax + B` 在 `(0, 0)` 处不成立。

use num_bigint::BigInt;
use num_traits::Zero;

/// 曲线上的一个点（仿射坐标）。
///
/// 坐标一律已经约简到 `[0, p)`，所以 `is_identity` 用"两个坐标都是 0"判定就够，
/// 不需要再区分 `-0`。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AffinePoint {
    x: BigInt,
    y: BigInt,
}

impl AffinePoint {
    /// 用一对坐标构造点。调用方负责保证坐标已约简。
    pub fn new(x: BigInt, y: BigInt) -> Self {
        Self { x, y }
    }

    /// 无穷远点，编码为 `(0, 0)`。
    pub fn identity() -> Self {
        Self { x: BigInt::zero(), y: BigInt::zero() }
    }

    /// 是否为无穷远点。
    pub fn is_identity(&self) -> bool {
        self.x.is_zero() && self.y.is_zero()
    }

    /// x 坐标。
    pub fn x(&self) -> &BigInt {
        &self.x
    }

    /// y 坐标。
    pub fn y(&self) -> &BigInt {
        &self.y
    }
}

impl Default for AffinePoint {
    fn default() -> Self {
        Self::identity()
    }
}
