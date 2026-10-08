//! `cargo xtask ci-*`：本地跑 CI 的等价检查。
//!
//! 因为 `lint.yml` **不跟 push / PR**（门禁只挂在发布链路上，见
//! `docs/agent/build-and-ci.md` §1.2），本地必须能一键跑完等价检查，
//! 否则只能靠推 `main` 才发现红灯。
//!
//! 三个子命令对应三个 workflow：
//!
//! | 子命令 | 对应 |
//!|--------|------|
//! | `ci-lint` | `lint.yml` 的 fmt / clippy / test / arch |
//! | `ci-build` | `build.yml` 在本机 target 上的编译与打包 |
//! | `ci-interop` | 与 Go 版的互通矩阵（**不在 CI 里**，见下） |
//!
//! MSRV 需要额外工具链，不在这里跑（见 `release.md` §4）。
//! 互通矩阵只在本地跑（`ci-interop`），**有意不进 CI** —— 决策与理由见
//! `docs/agent/build-and-ci.md` §5.3。

use std::path::Path;
use std::process::{Command, ExitCode};

use crate::{purity, workspace};

/// 一条命令的执行结果。
struct Step {
    title: &'static str,
    passed: bool,
}

/// 跑一条命令，打印标题并回传是否成功。
fn step(root: &Path, title: &'static str, program: &str, args: &[&str]) -> Step {
    println!("\n=== {title} ===");
    println!("$ {program} {}", args.join(" "));
    let passed = match Command::new(program).args(args).current_dir(root).status() {
        Ok(status) if status.success() => true,
        Ok(status) => {
            eprintln!("{title}: 失败（{status}）");
            false
        }
        Err(error) => {
            eprintln!("{title}: 无法执行 {program}：{error}");
            false
        }
    };
    Step { title, passed }
}

/// 打印汇总，返回退出码。
fn summarize(steps: &[Step]) -> ExitCode {
    let failed: Vec<&Step> = steps.iter().filter(|step| !step.passed).collect();
    println!();
    if failed.is_empty() {
        println!("全部通过（{} 项）", steps.len());
        return ExitCode::SUCCESS;
    }
    eprintln!("失败 {} / {} 项：", failed.len(), steps.len());
    for step in failed {
        eprintln!("  - {}", step.title);
    }
    ExitCode::FAILURE
}

/// `cargo xtask ci-lint`：跑完 `lint.yml` 里除 MSRV 之外的全部 job。
///
/// **不早退**：本地跑一次就想拿到全部红灯，所以在第一个失败处继续往下跑
/// （对应 CI 里那几个并行的 job）。
#[must_use]
pub fn lint() -> ExitCode {
    let root = workspace::root();
    let cargo = workspace::cargo();
    let mut steps = vec![
        step(&root, "rustfmt", &cargo, &["fmt", "--all", "--check"]),
        step(&root, "clippy", &cargo, &["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]),
        // --no-fail-fast：默认在第一个失败的测试二进制处就收工，一轮要拿到全部失败。
        step(&root, "test", &cargo, &["test", "--workspace", "--no-fail-fast"]),
    ];

    // 架构门禁直接在进程内跑：它们本来就是本 crate 的函数，不必再起一层 cargo。
    println!("\n=== 架构门禁 ===");
    steps.push(Step { title: "xtask purity", passed: purity::run() == ExitCode::SUCCESS });

    summarize(&steps)
}

/// `cargo xtask ci-build`：按本机 target 跑一次发布构建。
#[must_use]
pub fn build() -> ExitCode {
    let root = workspace::root();
    let mut args = vec!["build", "--release"];
    // 只构建存在的产物 crate：`croc-gui` 依赖系统 WebView 库，不是每台机器都装了。
    for name in ["croc-cli", "croc-relay", "croc-gui"] {
        if root.join("crates").join(name).join("Cargo.toml").is_file() {
            args.push("--package");
            args.push(name);
        }
    }
    let steps = vec![step(&root, "release build", &workspace::cargo(), &args)];
    summarize(&steps)
}

/// `cargo xtask ci-interop`：跑与 Go 版的互通矩阵。
///
/// **不在 CI 里**，这是有意的决定（`docs/agent/build-and-ci.md` §5.3）。
/// 需要两样东西：本机的 Go 版 croc 二进制，以及能跑的 `tests/interop/run.sh`。
/// 缺哪样就明确报出来，不静默跳过。
#[must_use]
pub fn interop() -> ExitCode {
    let root = workspace::root();

    let mut steps = vec![step(&root, "release build", &workspace::cargo(), &["build", "--release", "-p", "croc-cli"])];

    // Go 版 croc 二进制：优先用环境变量给的，否则现编一份。
    let go_bin = match resolve_go_bin(&root) {
        Ok(bin) => bin,
        Err(error) => {
            eprintln!("\ninterop: {error}");
            return summarize(&steps);
        }
    };
    println!("\nGo 版 croc：{}", go_bin.display());

    let rust_bin = root.join("target").join("release").join(if cfg!(windows) { "croc-rs.exe" } else { "croc-rs" });
    let go_bin_str = go_bin.to_string_lossy().into_owned();
    let rust_bin_str = rust_bin.to_string_lossy().into_owned();

    println!("\n=== 互通矩阵 ===");
    // 默认跳过需要外网的 I6（官方 relay）——本地门禁不该因为网络抖动变红。
    // 想真跑 I6 就显式把 CROC_INTEROP_SKIP_NETWORK 设成空串。
    let skip_network = std::env::var("CROC_INTEROP_SKIP_NETWORK").unwrap_or_else(|_| "1".to_owned());
    let script = root.join("tests").join("interop").join("run.sh");
    let bash = match resolve_bash() {
        Ok(bash) => bash,
        Err(error) => {
            eprintln!("\ninterop: {error}");
            steps.push(Step { title: "tests/interop/run.sh", passed: false });
            return summarize(&steps);
        }
    };
    let passed = Command::new(&bash)
        .arg(to_shell_path(&script))
        .current_dir(&root)
        .env("CROC_GO_BIN", &go_bin_str)
        .env("CROC_RS_BIN", &rust_bin_str)
        .env("CROC_INTEROP_SKIP_NETWORK", skip_network)
        .status()
        .is_ok_and(|status| status.success());
    steps.push(Step { title: "tests/interop/run.sh", passed });

    summarize(&steps)
}

/// 找到能跑 `run.sh` 的 bash。
///
/// Windows 上不能直接 `Command::new("bash")`：`PATH` 里排在前面的往往是
/// `C:\Windows\System32\bash.exe`（**WSL 的 bash**），它把 `D:\repo\run.sh` 的反斜杠
/// 当转义符，路径被吃掉，报的是 `No such file or directory`——看着像脚本不存在，
/// 其实是 bash 选错了。所以这里显式找 Git for Windows 的 bash。
fn resolve_bash() -> Result<std::path::PathBuf, String> {
    if cfg!(windows) {
        if let Ok(explicit) = std::env::var("CROC_INTEROP_BASH") {
            let path = std::path::PathBuf::from(explicit);
            if path.is_file() {
                return Ok(path);
            }
            return Err(format!("CROC_INTEROP_BASH 指向的文件不存在：{}", path.display()));
        }

        // Git for Windows 的常见安装位置。
        let mut roots = Vec::new();
        if let Ok(from_env) = std::env::var("ProgramFiles") {
            roots.push(std::path::PathBuf::from(from_env).join("Git").join("bin"));
        }
        if let Ok(from_env) = std::env::var("ProgramW6432") {
            roots.push(std::path::PathBuf::from(from_env).join("Git").join("bin"));
        }
        if let Ok(from_env) = std::env::var("LOCALAPPDATA") {
            roots.push(std::path::PathBuf::from(from_env).join("Programs").join("Git").join("bin"));
        }
        roots.push(std::path::PathBuf::from(r"C:\Program Files\Git\bin"));
        roots.push(std::path::PathBuf::from(r"C:\Program Files (x86)\Git\bin"));

        for root in roots {
            let candidate = root.join("bash.exe");
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
        return Err(
            "找不到 Git for Windows 的 bash（试过 Program Files / LOCALAPPDATA 下的 Git\\bin\\bash.exe）。\r\n        \
             互通矩阵是 bash 脚本，请安装 Git for Windows，或用 CROC_INTEROP_BASH 显式指一个 bash。\r\n        \
             注意 C:\\Windows\\System32\\bash.exe 是 WSL 的 bash，不是它。"
                .to_owned(),
        );
    }
    Ok(std::path::PathBuf::from("bash"))
}

/// 把路径转成 bash 认的形式。Windows 上是 `D:/a/b.sh`——正斜杠，WSL 与 Git Bash 都认。
fn to_shell_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// 定位 Go 版 `croc` 二进制。
///
/// 顺序：`CROC_GO_BIN` → `../croc-playground/croc.exe`（平级的独立仓库，平时把 Go 版
/// 二进制扔在那儿）→ 用 `../croc` 源码现编 → 报错。
fn resolve_go_bin(root: &Path) -> Result<std::path::PathBuf, String> {
    if let Ok(value) = std::env::var("CROC_GO_BIN") {
        let path = std::path::PathBuf::from(value);
        if path.is_file() {
            return Ok(path);
        }
        return Err(format!("CROC_GO_BIN 指向的文件不存在：{}", path.display()));
    }

    let sibling = root.parent().unwrap_or(root).join("croc-playground");
    for name in ["croc.exe", "croc"] {
        let candidate = sibling.join(name);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    let source = root.parent().unwrap_or(root).join("croc");
    if !source.join("go.mod").is_file() {
        return Err(format!(
            "找不到 Go 版 croc：既没有 {} 下的 croc[.exe]，也没有 {} 下的源码，也没有设置 CROC_GO_BIN",
            sibling.display(),
            source.display()
        ));
    }
    if Command::new("go").arg("version").stdout(std::process::Stdio::null()).status().is_err() {
        return Err("本机没有 go 命令，无法编译 Go 版 croc".to_owned());
    }

    let output = root.join("target").join("interop");
    std::fs::create_dir_all(&output).map_err(|error| format!("创建 {} 失败：{error}", output.display()))?;
    let bin = output.join(if cfg!(windows) { "croc-go.exe" } else { "croc-go" });
    println!("编译 Go 版 croc：{}", source.display());
    let status = Command::new("go")
        .args(["build", "-o"])
        .arg(&bin)
        .arg(".")
        .current_dir(&source)
        .status()
        .map_err(|error| format!("执行 go build 失败：{error}"))?;
    if !status.success() {
        return Err(format!("go build 失败（{status}）"));
    }
    Ok(bin)
}
