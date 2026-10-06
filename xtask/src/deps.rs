//! `cargo xtask deps`：守住"纯 Rust 加密栈"这条线。
//!
//! 核心 crate 最终要交叉编译到 Android（`aarch64-linux-android` / `x86_64-linux-android`）。
//! 一旦依赖图里混进 `openssl` / `ring` 这类带 C 源码的包，就必须为每个目标平台准备一套
//! NDK 工具链与 `CC_*` 环境变量，构建从"能跑"退化成"能跑但没人敢动"。
//!
//! 所以这里把"依赖图里不许出现 C 加密后端"写成门禁：**用 `cargo tree` 看真实解析结果**，
//! 而不是读 `Cargo.toml` 的字面依赖 —— 真正的凶手往往是三层之外的传递依赖
//! （比如某个 `reqwest` 的默认 feature 拉进 `native-tls` → `openssl`）。
//!
//! `croc-gui` 不在检查范围内：Tauri 自带 WebView / 系统集成那一整套，与核心链路无关。

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 必须能不带 C 工具链编译的 crate。
///
/// `croc-engine` / `croc-cli` 也在列：它们进最终二进制，链接期同样躲不开。
pub const CORE_CRATES: [&str; 9] = [
    "croc-siec",
    "croc-pake",
    "croc-pakekey",
    "croc-protocol",
    "croc-net",
    "croc-core",
    "croc-relay",
    "croc-engine",
    "croc-cli",
];

/// 依赖图里不许出现的包名。
///
/// 前四个是 C/汇编实现的 TLS 与加密后端；`ring` 与 `aws-lc-*` 是 `rustls` 的两条
/// 可选后端，都要显式换成纯 Rust 的 `RustCrypto` 实现。
pub const FORBIDDEN_PACKAGES: [&str; 5] = ["openssl", "openssl-sys", "ring", "aws-lc-rs", "aws-lc-sys"];

/// 一处违规。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 谁的依赖图里出现了它。
    pub crate_name: String,
    /// 出现的包名。
    pub package: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} 依赖了 {}", self.crate_name, self.package)
    }
}

/// 扫描 workspace 里所有存在的核心 crate。
///
/// # Errors
///
/// `cargo tree` 跑不起来（没有 cargo、锁文件损坏、需要联网但拿不到）时返回错误。
/// **不静默放过**：一个"跑不起来就跳过"的门禁等于没有门禁。
pub fn check(workspace_root: &Path) -> Result<Vec<Violation>, String> {
    let mut violations = Vec::new();
    for name in CORE_CRATES {
        if !workspace_root.join("crates").join(name).join("Cargo.toml").is_file() {
            continue;
        }
        let packages = cargo_tree(workspace_root, name)?;
        for package in packages {
            if FORBIDDEN_PACKAGES.contains(&package.as_str()) {
                violations.push(Violation { crate_name: name.to_owned(), package });
            }
        }
    }
    violations.sort_by(|left, right| left.crate_name.cmp(&right.crate_name).then(left.package.cmp(&right.package)));
    violations.dedup();
    Ok(violations)
}

/// 跑一次 `cargo tree`，返回该 crate 正常依赖（不含 dev / build）里的包名。
fn cargo_tree(workspace_root: &Path, crate_name: &str) -> Result<Vec<String>, String> {
    let output = Command::new(cargo())
        .args(["tree", "--edges", "normal", "--prefix", "none", "--package", crate_name])
        .current_dir(workspace_root)
        .output()
        .map_err(|error| format!("无法执行 cargo tree（{crate_name}）：{error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("cargo tree -p {crate_name} 失败：{}", stderr.trim()));
    }
    Ok(parse_packages(&String::from_utf8_lossy(&output.stdout)))
}

/// 从 `cargo tree --prefix none` 的输出里取出包名。
///
/// 每行形如 `sha2 v0.10.9` 或 `croc-core v0.1.0 (/path)`，所以第一个空白分隔的词
/// 就是包名。`--prefix none` 已经去掉了树形前缀与缩进。
fn parse_packages(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| !name.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// cargo 可执行文件名。
///
/// 优先用 `CARGO` 环境变量：cargo 自己会把它设成当前可执行文件的绝对路径，
/// 这样在 rustup 的 shim 与自定义工具链目录下都能拿到正确的那一个。
pub fn cargo() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// 找到 workspace 根。
#[must_use]
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap_or(Path::new(".")).to_path_buf()
}

/// 跑检查并按结果打印报告。
#[must_use]
pub fn run() -> std::process::ExitCode {
    let root = workspace_root();
    let violations = match check(&root) {
        Ok(violations) => violations,
        Err(error) => {
            eprintln!("deps: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if violations.is_empty() {
        println!("deps: {} 个核心 crate 没有 C 加密后端依赖", CORE_CRATES.len());
        return std::process::ExitCode::SUCCESS;
    }
    let mut report = String::new();
    let _ = writeln!(report, "deps: 发现 {} 处违规", violations.len());
    for violation in &violations {
        let _ = writeln!(report, "  {violation}");
    }
    let _ = write!(
        report,
        "\n核心 crate 只能用纯 Rust 加密栈（RustCrypto），否则 Android 交叉编译会缺 C 工具链。\n\
         常见修法：把 `native-tls` 换成 `rustls`（并关掉 `ring` / `aws-lc-rs` feature），\
         或直接依赖 `sha2` / `aes-gcm` 这类纯 Rust 实现。\n详见 docs/agent/plan/03-architecture.md §8.4。"
    );
    eprintln!("{report}");
    std::process::ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tree_output_is_parsed_by_first_token() {
        let stdout = "croc-core v0.1.0 (/workspace/crates/croc-core)\n\
                      sha2 v0.10.9\n\
                      time v0.3.45\n";
        assert_eq!(parse_packages(stdout), vec!["croc-core", "sha2", "time"]);
    }

    #[test]
    fn blank_lines_are_ignored() {
        assert_eq!(parse_packages("\n  \nsha2 v0.10.9\n"), vec!["sha2"]);
    }

    #[test]
    fn the_core_crates_do_not_pull_a_c_crypto_backend() {
        // 这个测试就是门禁本身。没有 cargo 时（极少见，毕竟我们是 `cargo xtask` 起来的）
        // 直接失败而不是跳过 —— 见 `check` 的注释。
        let violations = check(&workspace_root()).expect("应能读取依赖图");
        assert!(violations.is_empty(), "核心 crate 引入了 C 加密后端：\n{}", render(&violations));
    }

    fn render(violations: &[Violation]) -> String {
        violations.iter().map(|violation| format!("  {violation}\n")).collect()
    }
}
