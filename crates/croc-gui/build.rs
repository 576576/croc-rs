//! Tauri 的构建脚本。
//!
//! 它做的事有两类：把 `tauri.conf.json` 编译成 Rust 常量、生成权限 schema、在移动端
//! 生成工程骨架；以及提醒一件配置看着没关系、产物跑起来才出事的事（见
//! [`warn_missing_custom_protocol`]）。不要在这里加业务逻辑——构建脚本跑在宿主上，
//! 而产物跑在目标上，两者的 `cfg` 不一样。

fn main() {
    warn_missing_custom_protocol();
    tauri_build::build();
}

/// release 构建缺 `--features custom-protocol` 时给出提示。
///
/// 少了它，`tauri` 按 dev 模式编：产物运行时去连 `tauri.conf.json` 的 `devUrl`
/// （http://localhost:1420），而不是用编译期嵌入的前端资源。dev server 没起，窗口里就是
/// 一句 localhost 拒绝连接。编译、链接都照过，只有启动时才看得出差别。
fn warn_missing_custom_protocol() {
    if std::env::var("PROFILE").as_deref() == Ok("release") && tauri_build::is_dev() {
        println!(
            "cargo:warning=release 构建缺少 --features custom-protocol：产物会去连 devUrl \
             http://localhost:1420 而不是用内嵌的前端资源，启动窗口里是 localhost 拒绝连接"
        );
    }
}
