//! `crocw-rs`：桌面入口。
//!
//! 这个文件极简是**刻意的**：所有能测的东西都在 `croc_gui` 库里（见 `commands.rs` /
//! `state.rs`），`main.rs` 只剩"装日志、起 Tauri、注册命令"。
//! 移动端不编译这个文件——Android 走 `lib.rs` 里的 JNI 入口。

// Windows 上不要弹出控制台窗口（release 构建）。调试构建保留控制台，
// 否则 `tracing` 的输出没人看得到。
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    croc_gui::init_tracing();
    let result = tauri::Builder::default()
        .manage(std::sync::Arc::new(croc_gui::TransferRegistry::new()))
        .invoke_handler(tauri::generate_handler![
            croc_gui::generate_code,
            croc_gui::send,
            croc_gui::receive,
            croc_gui::cancel_transfer,
            croc_gui::list_transfers,
        ])
        .run(tauri::generate_context!());

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            // 走到这里通常是"没有可用的 WebView"或"窗口创建失败"。用 eprintln 而不是
            // panic：release 构建把控制台藏了，但用户从终端启动时仍然看得到。
            eprintln!("crocw-rs: {error}");
            ExitCode::FAILURE
        }
    }
}
