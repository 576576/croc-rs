//! croc 的**副作用解释器**。
//!
//! 这一层是 `croc-core::Session` 的对面：状态机说"发这一帧""传这个文件"，这里就真的去
//! 连中继、读写磁盘、开数据流。它**不做协议判断**——所有"该发什么"的决定都在纯核心里。
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
//! 两边的控制连接都只有**一条**，数据面才开 N 条（`min(中继数据端口数, 8)`）。
//!
//! # 取消
//!
//! 所有可能长期等待的地方都挂在同一个 [`CancellationToken`] 上。取消时：
//! 数据流立即停、控制循环收到 [`InEvent::Cancel`]、会话进入
//! [`croc_core::Outcome::Cancelled`]。**不做"半截文件"的清理**——接收方用的是
//! `truncate` + 偏移写，半截文件天然就是下次续传的起点，删掉反而丢了进度。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod data;
pub mod dns;
pub mod error;
pub mod files;
pub mod hash;
pub mod local;
pub mod proxy;
pub mod receive;
pub mod relay;
pub mod send;
pub mod throttle;

pub use error::EngineError;
pub use receive::{ReceiveRequest, Summary as ReceiveSummary, receive};
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
    /// 开始处理某个文件。
    FileStarted {
        /// 文件名。
        name: String,
        /// 字节数。
        size: u64,
    },
    /// 进度。**由调用方节流**——引擎每次落盘都会报一次。
    Progress {
        /// 已经搬完的字节。
        transferred: u64,
        /// 总字节。
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
