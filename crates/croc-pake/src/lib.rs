//! `schollz/pake` v3 的线路兼容移植 —— croc 对端认证用的 SPAKE2。
//!
//! # 这个 crate 负责什么
//!
//! croc 的握手分两层：
//!
//! 1. **本 crate**：把「口令」变成一把 32 字节的 SPAKE2 会话密钥。双方的公开值走
//!    JSON（见 [`wire`]），会话密钥走 SHA-256 转录哈希（见 [`spake2`]）。
//! 2. **`croc-pakekey`**：把会话密钥经 HKDF 扩展成流量密钥与双向确认标签，并把
//!    「房间名 / 用途 / 曲线 / 双方公开值」绑进转录。
//!
//! 这里只做第 1 层。
//!
//! # 用法
//!
//! ```no_run
//! use croc_pake::{CurveKind, Identities, Initiator, Responder};
//!
//! # fn main() -> Result<(), croc_pake::PakeError> {
//! // A 方（croc 里的接收方）
//! let mut alice = Initiator::start(b"pass123", CurveKind::P256, Identities::bound(b"receiver", b"sender"), [0u8; 32])?;
//! let alice_value = alice.public_bytes();
//!
//! // B 方（croc 里的发送方）
//! let mut bob = Responder::start(b"pass123", CurveKind::P256, Identities::bound(b"receiver", b"sender"))?;
//! bob.accept_request(alice_value.as_bytes(), [1u8; 32])?;
//! let bob_value = bob.public_bytes();
//!
//! alice.accept_response(bob_value.as_bytes())?;
//! assert_eq!(alice.session_key(), bob.session_key());
//! # Ok(())
//! # }
//! ```
//!
//! # 设计约束
//!
//! 这个 crate **没有** `tokio`、`std::fs`、`Instant::now`（`cargo xtask purity` 会把
//! 它们挡在门外）。随机数不在这里取，由调用方通过 `ephemeral` 参数注入，所以整个
//! crate 是确定性的、可回归测试的。
//!
//! # 与 Go 版的三处刻意的结构差异
//!
//! 1. **A/B 拆成两个类型**（[`Initiator`] / [`Responder`]），而不是一个带 `Role`
//!    字段的结构 + 运行期分支。调错方向编译不过。
//! 2. **随机数注入**，而不是内部 `rand.Read`。
//! 3. **公开值编码手写**，而不是拉 `serde_json` + `arbitrary_precision`。
//!
//! 三处差异都只影响 Rust 侧的代码形状；线路字节、转录哈希、会话密钥全部与 Go 版一致。

#![allow(
    clippy::similar_names,
    reason = "SPAKE2 的参与者就叫 A 和 B、固定点就叫 U 和 V；把它们改成长名字只会让代码与 Go 源码的对应关系更难核对"
)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod curve;
pub mod error;
pub mod spake2;
pub mod wire;

pub use curve::{Curve, CurveKind};
pub use error::PakeError;
pub use spake2::{EPHEMERAL_LEN, Identities, Initiator, Responder};
pub use wire::PublicState;
