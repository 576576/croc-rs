//! `crocw-rs` 的 Rust 侧：Tauri 命令、传输状态与事件桥。
//!
//! # 这一层有多薄
//!
//! 它不实现任何协议：所有传输都走 `croc-engine`，与 CLI 完全同源。这里只做三件事：
//!
//! 1. 把 `croc-engine` 的 [`EngineEvent`] 转成 Tauri event 推给前端；
//! 2. 维护 `HashMap<TransferId, CancellationToken>` —— UI 状态归前端，Rust 侧只记
//!    "哪些传输可以被取消"，别的一概不存；
//! 3. 把 `Result<T, EngineError>` 折成 `Result<T, String>`（Tauri 命令的错误必须可序列化）。
//!
//! 协议只在 `croc-engine` 里有一份，GUI 与 CLI 的行为因此不可能漂移。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

// 命令返回的错误与结束摘要走同一套 locale 文件（`locales/*.yml`），键名与前端的
// `ui/src/locales.ts` 对齐。语言由前端用 `set_locale` 切换。
rust_i18n::i18n!("locales", fallback = "en");

pub mod android;
pub mod commands;
pub mod offers;
pub mod state;
pub mod store;

pub use commands::{
    FinishedEvent, OfferEvent, OfferFile, ProgressEvent, ReceiveArgs, SendArgs, cancel_transfer, export_file,
    generate_code, list_transfers, receive, respond_to_offer, send, set_locale, share_file,
};
pub use offers::OfferRegistry;
pub use state::{TransferId, TransferRegistry, TransferStatus};
pub use store::{StoreArgs, StoredResult, store_upload};

/// 前端事件名。
///
/// 常量集中在这里，免得 Rust 侧改了名字、前端还在听旧的——那类 bug 只在运行时
/// 表现为"进度条不动"，最难查。
pub mod event {
    /// 传输进展（状态、文件名、进度）。
    pub const PROGRESS: &str = "croc://progress";
    /// 传输结束（成功或失败）。
    pub const FINISHED: &str = "croc://finished";
    /// 收到清单，等用户决定收不收。
    pub const OFFER: &str = "croc://offer";
}

/// 组装 Tauri 应用。桌面入口（`main.rs`）与移动端入口（[`run`]）共用这一个 Builder，
/// 免得两条入口各自进化出一份命令表。
pub fn app() -> tauri::Builder<tauri::Wry> {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_opener::init())
        .manage(std::sync::Arc::new(TransferRegistry::new()))
        .manage(std::sync::Arc::new(OfferRegistry::new()))
        .invoke_handler(tauri::generate_handler![
            generate_code,
            send,
            receive,
            cancel_transfer,
            list_transfers,
            respond_to_offer,
            set_locale,
            store_upload,
            export_file,
            share_file,
        ])
}

/// 移动端入口。名字与所在 crate 决定了 JNI 的符号名，由 Tauri 的 `android_entry_point`
/// 体系按下 [Android 工程的包名](https://v2.tauri.app/develop/gradle-setup/) 反查，
/// 所以不要改它的可见性与签名。
#[cfg_attr(mobile, tauri::mobile_entry_point)]
// 移动端入口没有返回错误的出口（宿主是 Activity），出错时只能 panic 给系统。
#[allow(clippy::panic, reason = "移动端入口没有返回错误的出口")]
pub fn run() {
    init_tracing();
    if let Err(error) = app().run(tauri::generate_context!()) {
        panic!("crocw-rs: {error}");
    }
}

/// 装 tracing。由入口决定要不要装：库 crate 里装了 subscriber，嵌入它的宿主就失去
/// 控制权了。
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
}
