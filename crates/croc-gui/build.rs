//! Tauri 的构建脚本。
//!
//! 它做的事只有一件：把 `tauri.conf.json` 编译成 Rust 常量、生成权限 schema，
//! 并在移动端生成工程骨架。**不要在这里加业务逻辑**——构建脚本跑在宿主上，
//! 而产物跑在目标上，两者的 `cfg` 不一样。

fn main() {
    tauri_build::build();
}
