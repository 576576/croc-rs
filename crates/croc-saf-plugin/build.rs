//! 让 `tauri-build` 找到本插件的 Android 库与命令清单。
//!
//! `android_path("android")` 是必须的：它把 `android/` 目录的位置以
//! `cargo:android_library_path=...` 输出，`tauri-build` 靠这个环境变量生成
//! `tauri.settings.gradle` 里的 `include ':tauri-plugin-saf'`。没有它，Gradle 不会
//! 编本插件，Rust 那边的 `run_mobile_plugin` 在真机上找不到类。
//!
//! `COMMANDS` 里的名字要与 Rust 侧 `#[tauri::command]` 的函数名逐字一致，
//! `tauri-plugin` 按它生成 ACL 权限条目（`_` 会转成 `-`，所以命令名用下划线写、
//! 权限标识符是 kebab，与上游插件一致）。

#![allow(clippy::expect_used, clippy::panic, reason = "构建脚本：出错就该让构建失败，而不是把问题推到运行期")]

const COMMANDS: &[&str] = &["pick_directory"];

fn main() {
    // 生成失败就让构建停在这里：清单缺失时 ACL 会把命令挡掉，表现为"插件装了但调不通"，
    // 比构建期报错难查得多。
    tauri_plugin::Builder::new(COMMANDS).android_path("android").try_build().expect("生成插件清单");
}
