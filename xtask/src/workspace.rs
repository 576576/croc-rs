//! workspace 位置与 cargo 可执行文件。

use std::path::{Path, PathBuf};

/// cargo 可执行文件名。
///
/// 优先用 `CARGO` 环境变量：cargo 自己会把它设成当前可执行文件的绝对路径，
/// 这样在 rustup 的 shim 与自定义工具链目录下都能拿到正确的那一个。
#[must_use]
pub fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// workspace 根目录。
#[must_use]
pub fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap_or(Path::new(".")).to_path_buf()
}
