//! croc 的异步 IO 原语：副作用边缘的第一层。
//!
//! 这里只放"谁都需要、但谁都不该自己写一遍"的东西：
//!
//! - [`Framed`]：把任意 `AsyncRead + AsyncWrite` 包成 comm 帧的读写端。中继服务端和
//!   客户端引擎都用它，于是"帧怎么读"只有一份实现。
//! - [`OsRandomness`]：把操作系统随机源接到 [`croc_protocol::Randomness`] 上。
//!
//! # 为什么单独一个 crate
//!
//! `croc-relay` 与 `croc-engine` 都需要这两样东西，但让中继依赖引擎（或反过来）都是
//! 错的依赖方向。把它们放进 `croc-protocol` 又违反"纯 crate 不许碰 IO"的硬约束
//! （`cargo xtask purity` 会拦下来）。所以单独一层，谁都能用，谁都不欠谁。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod framed;
pub mod random;

pub use framed::Framed;
pub use random::OsRandomness;

/// IO 层的失败模式。
///
/// 这里不把 `std::io::Error` 包进来做统一错误类型——调用方几乎总是想把它原样往
/// 上传（或者按 `ErrorKind` 分流），包一层只会让人再拆一层。所以 `Framed` 的错误是
/// 这个枚举，而底层 IO 错误通过 [`CrocIoError::Io`] 原样透传。
#[derive(Debug, thiserror::Error)]
pub enum CrocIoError {
    /// 对端在一条帧的中途关闭了连接。
    ///
    /// 这与"干净地读到 EOF"不同——干净的 EOF 是 [`Framed::read_frame`] 返回
    /// `Ok(None)`，而这个说明对方在发到一半时断了（或有人在主动截断流量）。
    #[error("peer closed the connection in the middle of a frame: {buffered} bytes buffered")]
    TruncatedFrame {
        /// 断线时已经缓冲、但不足以组成一条帧的字节数。
        buffered: usize,
    },

    /// 帧本身不合法（magic 不对、声明长度超上限）。
    #[error(transparent)]
    Protocol(#[from] croc_protocol::ProtocolError),

    /// 底层 IO 失败。
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
