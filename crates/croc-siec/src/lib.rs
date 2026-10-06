//! croc 的底层椭圆曲线算术。
//!
//! 这一层只做一件事：把 Go 版 croc 依赖的两套曲线实现，用同一组 Rust 类型复现到
//! **字节级等价**。
//!
//! | Go 侧 | 本 crate |
//! |---|---|
//! | `tscholl2/siec` 的 `SIEC255Params` | [`WeierstrassCurve`] + [`params::SIEC255`] |
//! | `crypto/elliptic` 的 `P256/P384/P521` | [`WeierstrassCurve`] + `params::NIST_*` |
//! | `schollz/pake` 的 `Edwards25519Curve` | [`edwards`] |
//!
//! # 为什么是仿射坐标 + 扩展欧几里得
//!
//! 参考实现的输出只取决于仿射坐标：Go 的 `crypto/elliptic` 在 P 曲线上走 `nistec`
//! （Jacobian、恒定时间），`siec` 走朴素仿射公式，但两者吐出来的 `(x, y)` 是同一个
//! 数。PAKE 一次握手只做几百次点运算，一次点加一次求逆的开销完全可以忽略，换来的是
//! 公式能和 Go 源码逐行对照。
//!
//! # 三条必须照抄的怪癖
//!
//! 1. **欧几里得取模**。Go 的 `Mod` 结果恒非负，Rust 的 `%` 跟随被除数符号 →
//!    [`emod`]。
//! 2. **标量约简的不对称**。`nistCurve.normalizeScalar` 只对过长标量取模，过短的只
//!    左补零；`siec.ScalarMult` 完全不约简 → [`WeierstrassCurve::normalize_scalar`]
//!    与 [`WeierstrassCurve::normalize_scalars`] 开关。
//! 3. **无穷远点是 `(0, 0)`**，不是 `None` → [`AffinePoint::identity`]。
//!
//! 这些差异都封在类型和函数里，而不是散落成注释，见
//! `docs/agent/plan/02-crypto.md` §6。

#![cfg_attr(
    test,
    allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing, clippy::similar_names,)
)]

pub mod affine;
pub mod bigint;
pub mod edwards;
pub mod params;
pub mod weierstrass;

pub use affine::AffinePoint;
pub use bigint::{emod, go_bytes, mod_inverse, parse_int, to_be_fixed};
pub use params::{NIST_P256, NIST_P384, NIST_P521, SIEC255};
pub use weierstrass::{CurveSpec, WeierstrassCurve};

/// 曲线算术的失败模式。
///
/// 注意这里**没有**"点不在曲线上"之外的用法错误：曲线参数是编译期常量，不可能
/// 运行期失效。真正的输入校验（对方送来的点是否在曲线上）由 `croc-pake` 负责。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CurveError {
    /// 点不在曲线上。
    #[error("point is not on the curve")]
    NotOnCurve,
    /// 标量在该曲线上的编码非法。
    #[error("scalar is not a valid encoding for this curve")]
    InvalidScalar,
    /// 压缩点无法解压。
    #[error("point encoding does not decompress to a curve point")]
    InvalidPointEncoding,
}
