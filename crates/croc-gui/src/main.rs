//! `crocw-rs`：桌面入口。
//!
//! 这个文件极简是刻意的：所有能测的东西都在 `croc_gui` 库里（见 `commands.rs` /
//! `state.rs` / `offers.rs`），桌面这里只负责装日志、拉起 [`croc_gui::app`]。
//! 移动端不编译这个文件——Android 走 [`croc_gui::run`]。

// Windows 上不要弹出控制台窗口（release 构建）。调试构建保留控制台，
// 否则 `tracing` 的输出没人看得到。
//
// 条件里带上 `target_os = "windows"`：只写 `not(debug_assertions)` 时，这个属性会被
// 原样带到 Android/iOS 的编译单元里，rustc 在非 Windows target 上处理它会触发内部
// 错误（android target 编译 bin 时实测 panic）。
#![cfg_attr(all(not(debug_assertions), target_os = "windows"), windows_subsystem = "windows")]

use std::process::ExitCode;

fn main() -> ExitCode {
    croc_gui::init_tracing();
    let result = croc_gui::app().run(tauri::generate_context!());

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
