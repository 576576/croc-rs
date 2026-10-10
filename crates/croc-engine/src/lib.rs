//! croc 的副作用解释器。
//!
//! 这一层是 `croc-core::Session` 的对面：状态机说"发这一帧""传这个文件"，这里就真的去
//! 连中继、读写磁盘、开数据流。它不做协议判断——所有"该发什么"的决定都在纯核心里。
//!
//! ```text
//! croc-cli ──► croc-engine ──► croc-core::Session（纯）
//!                  │
//!                  ├── tokio TcpStream + Framed    （relay）
//!                  ├── std::fs + spawn_blocking   （files）
//!                  └── N 条数据流                  （data）
//! ```
//!
//! # 两条路径
//!
//! | 入口 | 做什么 |
//! |------|--------|
//! | [`send`] | 收集文件 → 连中继 → PAKE → 发清单 → 按请求逐个文件传 |
//! | [`receive`] | 连中继 → PAKE → 收清单 → 用户决策 → 逐个文件请求并落盘 |
//!
//! 两边的控制连接都只有一条，数据面才开 N 条（`min(中继数据端口数, 8)`）。
//!
//! # 取消
//!
//! 所有可能长期等待的地方都挂在同一个 [`CancellationToken`] 上。取消时：
//! 数据流立即停、控制循环收到 [`InEvent::Cancel`]、会话进入
//! [`croc_core::Outcome::Cancelled`]。不做"半截文件"的清理——接收方用的是
//! `truncate` + 偏移写，半截文件天然就是下次续传的起点，删掉反而丢了进度。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod data;
pub mod dns;
pub mod error;
pub mod files;
pub mod hash;
pub mod local;
pub mod proxy;
pub mod public_relay;
pub mod receive;
pub mod relay;
pub mod send;
pub mod text;
pub mod throttle;

pub use error::EngineError;
pub use receive::{
    AsyncConfirmSink, ConfirmFuture, ConfirmSink, ReceiveRequest, StdoutMode, Summary as ReceiveSummary, receive,
};
pub use send::{SendRequest, Summary as SendSummary, send};

use std::sync::Arc;

/// 引擎往上报的进展。
///
/// 刻意做得很薄：UI 只关心"传到哪了"和"在干什么"，不关心协议细节。要诊断协议就开
/// `tracing`，别往事件里塞字段——事件会进 GUI，`tracing` 不会。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum EngineEvent {
    /// 一句人类可读的状态（"waiting for the recipient…"）。
    Status(String),
    /// 发送方已经清点完要发的东西，准备连中继了。
    ///
    /// UI 在这一刻把口令码打出来——Go 也是在进 `Client.Send()` 之前就拿到了完整的
    /// 文件信息，所以它能一边算哈希一边把码显示给用户。
    Sending {
        /// 只发一个文件时的文件名；多于一个时是 `None`（UI 改用 `files` 计数）。
        name: Option<String>,
        /// 文件数。
        files: u64,
        /// 目录数。
        folders: u64,
        /// 总字节数。
        bytes: u64,
    },
    /// 开始处理某个文件。
    FileStarted {
        /// 文件名。
        name: String,
        /// 字节数。
        size: u64,
    },
    /// 对端报了它的地址（`externalip`，或内联元数据里的 `ExternalIP`）。
    ///
    /// 只用于显示"发给谁 / 从谁收"（Go 的 `Sending (->10.0.0.2)`）。
    PeerAddress(String),
    /// 整次传输的累计进度。每搬完一个文件报一次。
    Progress {
        /// 已经搬完的字节。
        transferred: u64,
        /// 总字节。
        total: u64,
    },
    /// 当前文件内的进度。搬运过程中按 100 ms 节流上报。
    ///
    /// 与 [`EngineEvent::Progress`] 的区别就是 Go 的进度条与它的完成回调之间的区别：
    /// 条是每个文件一条、从 0 走到该文件大小；累计值则走到整次传输结束。
    FileProgress {
        /// 这个文件已经搬完的字节。
        transferred: u64,
        /// 这个文件的大小。
        total: u64,
    },
    /// 一个文件完成。
    FileDone {
        /// 文件名。
        name: String,
    },
}

/// 事件回调。引擎在 tokio 任务里调用它，所以必须 `Send + Sync`。
///
/// 用 `Arc<dyn Fn>` 而不是 `async fn`：进度事件不该让传输去等 UI 渲染。
pub type EventSink = Arc<dyn Fn(EngineEvent) + Send + Sync>;

/// 一个什么都不做的事件出口。
#[must_use]
pub fn silent_sink() -> EventSink {
    Arc::new(|_| {})
}

/// 把一条事件发出去。回调 panic 不该把传输带走，所以这里吞掉 panic。
pub(crate) fn emit(sink: &EventSink, event: EngineEvent) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| sink(event)));
}
