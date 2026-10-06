//! `crocw-rs` 的 Rust 侧：Tauri 命令、传输状态与事件桥。
//!
//! # 这一层有多薄
//!
//! 它**不实现任何协议**：所有传输都走 `croc-engine`，与 CLI 完全同源。这里只做三件事：
//!
//! 1. 把 `croc-engine` 的 [`EngineEvent`] 转成 Tauri event 推给前端；
//! 2. 维护 `HashMap<TransferId, CancellationToken>` —— **UI 状态归前端**，Rust 侧只记
//!    "哪些传输可以被取消"，别的一概不存；
//! 3. 把 `Result<T, EngineError>` 折成 `Result<T, String>`（Tauri 命令的错误必须可序列化）。
//!
//! 这样做的直接好处：GUI 与 CLI 的**行为不可能漂移**。FlCroc 那套（Flutter + Go CGO）
//! 之所以难维护，正是因为同一个协议在两端各有一份状态机。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod commands;
pub mod state;

pub use commands::{ReceiveArgs, SendArgs, cancel_transfer, generate_code, list_transfers, receive, send};
pub use state::{TransferId, TransferRegistry, TransferStatus};

/// 前端事件名。
///
/// 常量集中在这里，免得 Rust 侧改了名字、前端还在听旧的——那类 bug 只在运行时
/// 表现为"进度条不动"，最难查。
pub mod event {
    /// 传输进展（状态、文件名、进度）。
    pub const PROGRESS: &str = "croc://progress";
    /// 传输结束（成功或失败）。
    pub const FINISHED: &str = "croc://finished";
}

/// 装 tracing。Tauri 应用里**不装 subscriber**，由 `main.rs` 决定——库 crate 装
/// subscriber 会让嵌入它的宿主失去控制权。
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
}
