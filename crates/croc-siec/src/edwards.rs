//! Ed25519 适配器：`schollz/pake` 里 `Edwards25519Curve` 的移植。
//!
//! Go 版把**整个 32 字节压缩点塞进 `x` 坐标**，`y` 恒为 0（见
//! `ed25519PointToBigInts`）。这个约定很怪，但它是线路契约的一部分，所以这里照抄：
//! [`AffinePoint::x`] 是压缩点的大端整数，[`AffinePoint::y`] 永远是 0。
//!
//! 另一处必须照抄的是**标量钳位**：Go 的 `SetBytesWithClamping` 先做
//! `s[0] &= 248; s[31] &= 127; s[31] |= 64`，再用 64 字节宽约简模 `L`。
//! 这里用 `clamp → Scalar::from_bytes_mod_order`，等价于 Go 的
//! `SetUniformBytes(clamp(x) ‖ 0³²)`。
//!
//! 已知偏差（记录在案，不做兼容）：Go 的 `Point.SetBytes` 拒绝非规范编码
//! （`y ≥ p`），`curve25519-dalek` 的 `decompress` 接受并把 `y` 隐式约简。这个差异
//! 只在对手方故意送非规范编码时才会被观察到，不影响诚实方的互通。

use curve25519_dalek::constants::ED25519_BASEPOINT_POINT;
use curve25519_dalek::edwards::{CompressedEdwardsY, EdwardsPoint};
use curve25519_dalek::scalar::Scalar;
use num_bigint::{BigInt, Sign};
use num_traits::Zero;

use crate::affine::AffinePoint;
use crate::bigint::go_bytes;

/// 压缩点长度。
const COMPRESSED_LEN: usize = 32;

/// 把 `x` 坐标（大端整数）还原成压缩点并解压。
///
/// Go 在 `len(xBytes) > 32` 时不会拷任何字节，于是拿到全零编码；这里直接判失败，
/// 行为等价于"解压不出点"。
fn decompress(x: &BigInt) -> Option<EdwardsPoint> {
    let bytes = go_bytes(x);
    if bytes.len() > COMPRESSED_LEN {
        return None;
    }
    let mut buffer = [0u8; COMPRESSED_LEN];
    let offset = COMPRESSED_LEN - bytes.len();
    let tail = buffer.get_mut(offset..)?;
    tail.copy_from_slice(&bytes);
    CompressedEdwardsY(buffer).decompress()
}

/// 把曲线点压回 `x` 坐标（`y` 恒为 0，见模块文档）。
fn compress(point: &EdwardsPoint) -> AffinePoint {
    let bytes = point.compress().to_bytes();
    AffinePoint::new(BigInt::from_bytes_be(Sign::Plus, &bytes), BigInt::zero())
}

/// Go `normalizeScalar` + `SetBytesWithClamping` 的合并语义。
///
/// 注意 `normalizeScalar` 的不对称：输入够长就取**前 32 字节**，不够长就**右对齐**
/// 左补零。两处都和直觉相反，照抄。
fn clamped_scalar(scalar: &[u8]) -> Scalar {
    let mut key = [0u8; COMPRESSED_LEN];
    if scalar.len() >= COMPRESSED_LEN {
        if let Some(head) = scalar.get(..COMPRESSED_LEN) {
            key.copy_from_slice(head);
        }
    } else {
        let offset = COMPRESSED_LEN - scalar.len();
        if let Some(tail) = key.get_mut(offset..) {
            tail.copy_from_slice(scalar);
        }
    }
    if let Some(first) = key.first_mut() {
        *first &= 248;
    }
    if let Some(last) = key.last_mut() {
        *last &= 127;
        *last |= 64;
    }
    Scalar::from_bytes_mod_order(key)
}

/// 该点是否为合法的压缩 Ed25519 点。
pub fn is_on_curve(point: &AffinePoint) -> bool {
    decompress(point.x()).is_some()
}

/// 点加。任一侧解压失败时返回 `(0, 0)`，与 Go 版一致。
///
/// **刻意不做无穷远点短路**：Go 的 `Edwards25519Curve.Add` 直接解压两个点相加，
/// 没有 `(0, 0)` 特判。加上短路反而会偏离参考实现。
pub fn add(first: &AffinePoint, second: &AffinePoint) -> AffinePoint {
    match (decompress(first.x()), decompress(second.x())) {
        (Some(left), Some(right)) => compress(&(left + right)),
        _ => AffinePoint::identity(),
    }
}

/// 点减，对应 Go 版专为 Ed25519 加的 `Subtract`。
pub fn subtract(first: &AffinePoint, second: &AffinePoint) -> AffinePoint {
    match (decompress(first.x()), decompress(second.x())) {
        (Some(left), Some(right)) => compress(&(left - right)),
        _ => AffinePoint::identity(),
    }
}

/// 基点标量乘 `k·G`。
pub fn scalar_mult_base(scalar: &[u8]) -> AffinePoint {
    compress(&(ED25519_BASEPOINT_POINT * clamped_scalar(scalar)))
}

/// 任意点标量乘 `k·P`。
pub fn scalar_mult(point: &AffinePoint, scalar: &[u8]) -> AffinePoint {
    decompress(point.x()).map_or_else(AffinePoint::identity, |base| compress(&(base * clamped_scalar(scalar))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> AffinePoint {
        scalar_mult_base(&[1u8; 32])
    }

    #[test]
    fn base_point_is_on_curve() {
        assert!(is_on_curve(&base()));
    }

    #[test]
    fn identity_encoding_is_not_rejected_by_decompress() {
        // 全零编码解出来的是 4 阶点 (√-1, 0)，不是无穷远点。Go 的 SetBytes 同样接受它，
        // 所以这里断言"能解出点"而不是"解不出点"。
        assert!(decompress(&BigInt::zero()).is_some());
    }

    #[test]
    fn addition_is_commutative() {
        let left = scalar_mult_base(&[3u8; 32]);
        let right = scalar_mult_base(&[5u8; 32]);
        assert_eq!(add(&left, &right), add(&right, &left));
    }

    #[test]
    fn subtracting_a_point_from_itself_gives_the_group_identity() {
        let point = scalar_mult_base(&[7u8; 32]);
        let difference = subtract(&point, &point);
        // 无穷远点的压缩编码是 0x01 00..00，即大端整数 2^248。
        let expected = AffinePoint::new(BigInt::from(1u8) << 248u32, BigInt::zero());
        assert_eq!(difference, expected);
    }

    #[test]
    fn scalar_multiplication_is_symmetric_in_its_operands() {
        // 钳位后标量必是 8 的倍数，所以没法构造"标量 2"去和 P + P 比对。
        // 改成验证标量可交换：[c₇]·([c₂]·G) == [c₂]·([c₇]·G)。
        let base = scalar_mult_base(&[2u8; 32]);
        let other = scalar_mult_base(&[7u8; 32]);
        assert_eq!(scalar_mult(&base, &[7u8; 32]), scalar_mult(&other, &[2u8; 32]));
    }

    #[test]
    fn clamping_is_applied_before_reduction() {
        // 钳位会把首字节低 3 位清零、末字节最高位清零次高位置一。
        // 两个只差这几位的数据应得到同一个标量。
        let mut low = [0u8; 32];
        let mut high = [0u8; 32];
        low[0] = 0x00;
        high[0] = 0x07;
        low[31] = 0x40;
        high[31] = 0xc0;
        assert_eq!(clamped_scalar(&low), clamped_scalar(&high));
    }
}
