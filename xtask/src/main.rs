//! `cargo xtask`：本仓库的构建期杂活。
//!
//! 用 `xtask` 而不是散落的 shell 脚本，是为了让门禁**跨平台**：CI 跑在
//! Linux / macOS / Windows 三种 runner 上，`grep -r` 那套在 Windows 上就不是同一件事了。
//!
//! ```console
//! $ cargo xtask purity
//! $ cargo xtask deps
//! $ cargo xtask ci-lint
//! ```
//!
//! 子命令分两类：
//!
//! - **架构门禁**（`purity` / `deps`）—— CI 的 `arch` job 跑它们，把架构约束变成硬错误；
//! - **本地 CI 等价命令**（`ci-*`）—— 因为质量门禁不跟 push / PR，本地必须能一键跑完
//!   等价检查，否则只能靠推 `main` 才发现红灯。

// 测试代码里 unwrap / expect / panic / 索引都是正常表达方式，在 crate 根部统一豁免
// （与各业务 crate 的做法一致）。生产代码里这些 lint 仍然是硬门禁。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

mod ci;
mod deps;
mod lex;
mod purity;

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("purity") => purity::run(),
        Some("deps") => deps::run(),
        Some("ci-lint") => ci::lint(),
        Some("ci-build") => ci::build(),
        Some("ci-interop") => ci::interop(),
        Some("help" | "--help" | "-h") | None => {
            print_help();
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("xtask: 未知子命令 {other:?}\n");
            print_help();
            ExitCode::FAILURE
        }
    }
}

fn print_help() {
    println!(
        "cargo xtask <子命令>

架构门禁：
  purity      croc-siec / croc-pake / croc-protocol 里不许有 IO、时钟、网络与 OS 随机源
  deps        核心 crate 的依赖图里不许有 openssl / ring / aws-lc-* 这类 C 加密后端

本地 CI 等价命令（质量门禁不跟 push / PR，推 main 前请先跑）：
  ci-lint     = lint.yml 的 fmt / clippy / test / arch
  ci-build    = build.yml 在本机 target 上的发布构建
  ci-interop  = lint.yml 的 interop job（需本机有 Go 与 croc 源码）

  help        显示这段帮助"
    );
}
