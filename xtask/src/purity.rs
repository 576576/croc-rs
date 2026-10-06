//! `cargo xtask purity`：守住"纯核心"这条架构线。
//!
//! `croc-siec` / `croc-pake` / `croc-protocol` 必须在**没有 IO、没有时钟、没有随机源**
//! 的前提下编译运行。这不是洁癖：
//!
//! - 它们要交叉编译到 Android（`aarch64-linux-android` 等），少一层运行时少一堆坑；
//! - 状态机要能在单元测试里**确定性重放**（见 `docs/agent/plan/03-architecture.md` §2）。
//!   一个 `Instant::now()` 就足以让"同样的输入序列得到同样的输出"不再成立。
//!
//! 检查两件事：
//!
//! 1. 这三个 crate 的 `Cargo.toml` 里不许出现运行时/IO 依赖；
//! 2. 它们的 `.rs` 源码里（**去掉注释与字符串之后**，见 [`crate::lex`]）不许出现
//!    IO、网络、时钟、OS 随机源。
//!
//! 规则本身故意写得保守：宁可漏掉一个新出现的 API，也不要因为文档注释里提了一句
//! `std::fs` 就报错——一个会误报的门禁很快就会被 `#[allow]` 掉，然后就形同虚设。

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use crate::lex;

/// 必须保持纯的 crate。
pub const PURE_CRATES: [&str; 3] = ["croc-siec", "croc-pake", "croc-protocol"];

/// 源码里不许出现的 API。
pub const FORBIDDEN_APIS: [&str; 8] =
    ["tokio::", "std::fs", "std::net", "std::process", "std::thread", "Instant::now", "SystemTime::now", "thread_rng"];

/// `Cargo.toml` 里不许出现的依赖。
///
/// 匹配的是依赖条目的名字本身（`tokio = …` / `tokio.workspace = true`），所以
/// `tokio-util` 这类兄弟 crate 要单独列出来。
pub const FORBIDDEN_DEPS: [&str; 5] = ["tokio", "tokio-util", "getrandom", "rand", "socket2"];

/// 一处违规。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// 出问题的文件，相对 workspace 根。
    pub file: String,
    /// 第几行（1 起）。
    pub line: usize,
    /// 命中的关键词或依赖名。
    pub needle: String,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{}: {}", self.file, self.line, self.needle)
    }
}

/// 扫描整个 workspace。
///
/// # Errors
///
/// 读不到 `crates/` 目录或某个文件时返回 IO 错误。
pub fn check(workspace_root: &Path) -> std::io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    for name in PURE_CRATES {
        let crate_root = workspace_root.join("crates").join(name);
        if !crate_root.is_dir() {
            continue;
        }
        violations.extend(check_manifest(&crate_root, workspace_root)?);
        violations.extend(check_sources(&crate_root, workspace_root)?);
    }
    violations.sort_by(|left, right| left.file.cmp(&right.file).then(left.line.cmp(&right.line)));
    Ok(violations)
}

/// 检查一个 crate 的 `Cargo.toml`。
///
/// 文件不存在时返回空 —— `check` 已经确认过 crate 目录存在，这里只是防御。
/// 读失败要**向上报**：读不到清单就没法断言它干净，静默放过等于门禁失效。
fn check_manifest(crate_root: &Path, workspace_root: &Path) -> std::io::Result<Vec<Violation>> {
    let manifest = crate_root.join("Cargo.toml");
    if !manifest.is_file() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&manifest)?;
    let file = relative(&manifest, workspace_root);
    let mut violations = Vec::new();
    for (index, line) in text.lines().enumerate() {
        for needle in forbidden_deps_in_line(line) {
            violations.push(Violation { file: file.clone(), line: index + 1, needle });
        }
    }
    Ok(violations)
}

/// 一行 `Cargo.toml` 里出现的禁用依赖名。
fn forbidden_deps_in_line(line: &str) -> Vec<String> {
    let trimmed = line.trim();
    FORBIDDEN_DEPS
        .iter()
        .filter(|dep| {
            trimmed.strip_prefix(*dep).is_some_and(|rest| {
                // 依赖条目的形状是 `name = …` 或 `name.workspace = true`。
                rest.starts_with([' ', '\t', '=', '.'])
            })
        })
        .map(|dep| (*dep).to_owned())
        .collect()
}

/// 递归检查一个 crate 下所有 `.rs` 文件。
///
/// `tests/` 目录被跳过：集成测试要读 golden 向量，本来就得碰文件系统。门禁守的是
/// **库代码**不许有 IO，不是"连测试都不许读文件"。
fn check_sources(crate_root: &Path, workspace_root: &Path) -> std::io::Result<Vec<Violation>> {
    let mut violations = Vec::new();
    let mut pending = vec![crate_root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory)? {
            let path = entry?.path();
            if path.is_dir() {
                // `target/` 不该出现在源码树里，但真出现了也没必要扫。
                let skip = path.file_name().is_some_and(|name| name == "target" || name == "tests");
                if skip {
                    continue;
                }
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                violations.extend(check_file(&path, workspace_root)?);
            }
        }
    }
    Ok(violations)
}

/// 检查单个源文件。
fn check_file(path: &Path, workspace_root: &Path) -> std::io::Result<Vec<Violation>> {
    let source = fs::read_to_string(path)?;
    let code = lex::strip_non_code(&source);
    let file = relative(path, workspace_root);
    let mut violations = Vec::new();
    for needle in FORBIDDEN_APIS {
        for offset in find_all(&code, needle) {
            violations.push(Violation { file: file.clone(), line: line_at(&code, offset), needle: needle.to_owned() });
        }
    }
    Ok(violations)
}

/// `code` 里 `needle` 出现的所有偏移。
fn find_all(code: &[u8], needle: &str) -> Vec<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() || needle.len() > code.len() {
        return Vec::new();
    }
    code.windows(needle.len()).enumerate().filter(|(_, window)| *window == needle).map(|(index, _)| index).collect()
}

/// 某个偏移落在第几行（1 起）。
fn line_at(code: &[u8], offset: usize) -> usize {
    code.get(..offset).map_or(0, |head| head.iter().filter(|byte| **byte == b'\n').count()) + 1
}

/// 相对 workspace 根的路径，斜杠统一成 `/`。
fn relative(path: &Path, workspace_root: &Path) -> String {
    path.strip_prefix(workspace_root).unwrap_or(path).to_string_lossy().replace('\\', "/")
}

/// 找到 workspace 根：`xtask` 的上一级。
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
            eprintln!("purity: 扫描失败：{error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if violations.is_empty() {
        println!("purity: {} 个纯 crate 干净", PURE_CRATES.len());
        return std::process::ExitCode::SUCCESS;
    }
    let mut report = String::new();
    let _ = writeln!(report, "purity: 发现 {} 处违规", violations.len());
    for violation in &violations {
        let _ = writeln!(report, "  {violation}");
    }
    let _ = write!(
        report,
        "\n{} 必须保持纯：不许出现 IO / 时钟 / 网络 / OS 随机源。\n详见 docs/agent/plan/03-architecture.md §8.5。",
        PURE_CRATES.join(" / ")
    );
    eprintln!("{report}");
    std::process::ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pure_crates_are_actually_pure() {
        // 这个测试就是门禁本身：`cargo test` 会跑它，CI 也跑 `cargo xtask purity`。
        let violations = check(&workspace_root()).expect("应能扫描 workspace");
        assert!(violations.is_empty(), "纯 crate 里出现了 IO/时钟 API：\n{}", render(&violations));
    }

    fn render(violations: &[Violation]) -> String {
        violations.iter().map(|violation| format!("  {violation}\n")).collect()
    }

    #[test]
    fn dependency_lines_are_matched_by_shape() {
        assert_eq!(forbidden_deps_in_line("tokio.workspace = true"), vec!["tokio"]);
        assert_eq!(forbidden_deps_in_line("tokio = \"1\""), vec!["tokio"]);
        assert_eq!(forbidden_deps_in_line("  getrandom = \"0.3\""), vec!["getrandom"]);
        // 名字只是前缀相同的不算。
        assert!(forbidden_deps_in_line("tokio-util = \"0.7\"").contains(&"tokio-util".to_owned()));
        assert!(forbidden_deps_in_line("rand_core = \"0.6\"").is_empty());
        assert!(forbidden_deps_in_line("croc-tokio = \"1\"").is_empty());
        assert!(forbidden_deps_in_line("# tokio = \"1\"").is_empty());
    }

    #[test]
    fn violations_carry_a_line_number() {
        let code = lex::strip_non_code("let a = 1;\nlet b = tokio::spawn();\n");
        let offsets = find_all(&code, "tokio::");
        assert_eq!(offsets.len(), 1);
        assert_eq!(line_at(&code, offsets.first().copied().unwrap_or(0)), 2);
    }

    #[test]
    fn documentation_mentions_do_not_count_as_violations() {
        let code = lex::strip_non_code("//! 这里没有 tokio:: 也没有 std::fs\nfn pure() {}\n");
        assert!(find_all(&code, "tokio::").is_empty());
        assert!(find_all(&code, "std::fs").is_empty());
    }
}
