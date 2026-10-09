//! 把版本号烘进二进制。
//!
//! 版本号不是 `Cargo.toml` 里那个 `version`——那个是包版本，与发布通道无关，任何一次构建
//! 打出来都一样，分不清手上的二进制是哪次提交。真正的版本号由 `release.yml` 的 `prep` job
//! 从提交数算出（alpha `r{count+1000}`，其余 `1.{count/100}.{count%100}`），经
//! `CROC_VERSION_NAME` 传给 `cargo build`。
//!
//! 本地构建没有那个变量，这里用同一个公式从提交数推一遍（取不到 git 才退回包版本）。

use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=CROC_VERSION_NAME");
    // 界面文案由 `i18n!` 这个 proc macro 在编译期读进去，而 proc macro 没法自己声明依赖，
    // 改了 yml 不重跑就会一直用旧表（症状是界面上直接出现键名）。让 cargo 监视整个目录。
    println!("cargo:rerun-if-changed=locales");
    watch_git_head();
    println!("cargo:rustc-env=CROC_CLI_VERSION={}", version());
}

/// 版本号。
fn version() -> String {
    if let Some(name) =
        std::env::var("CROC_VERSION_NAME").ok().map(|name| name.trim().to_owned()).filter(|name| !name.is_empty())
    {
        return name;
    }
    // 本地构建：用提交数推，形式与 CI 的 beta/release 一致（本地谈不上通道）。
    commit_count().map_or_else(
        || std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned()),
        |count| format!("1.{}.{:02}", count / 100, count % 100),
    )
}

/// 当前提交数（`git rev-list --count HEAD`）。
fn commit_count() -> Option<u64> {
    let output = Command::new("git").args(["rev-list", "--count", "HEAD"]).output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

/// 让 cargo 在提交数变了的时候重跑本脚本。
///
/// 只声明 `rerun-if-env-changed` 的话，本地每次提交都不会重跑，版本号会冻在第一次构建的
/// 那个值上。提交只动 `.git/refs/heads/<branch>`（`.git/HEAD` 只在切分支时变），所以两个
/// 都得盯。
fn watch_git_head() {
    let Some(git_dir) = git_dir() else {
        return;
    };
    let head = git_dir.join("HEAD");
    if !head.is_file() {
        return;
    }
    println!("cargo:rerun-if-changed={}", head.display());
    // HEAD 通常只有一行 `ref: refs/heads/<branch>`。
    if let Ok(text) = std::fs::read_to_string(&head)
        && let Some(reference) = text.trim().strip_prefix("ref: ")
    {
        let path = git_dir.join(reference);
        // ref 可能还没落地（首次提交之前），也可能被打包进 `packed-refs`。
        let watched = if path.is_file() { path } else { git_dir.join("packed-refs") };
        println!("cargo:rerun-if-changed={}", watched.display());
    }
}

/// 从本 crate 往上找 `.git`（worktree 里它是个文件，指向真正的 git 目录）。
fn git_dir() -> Option<PathBuf> {
    let mut current = Some(Path::new(env!("CARGO_MANIFEST_DIR")));
    while let Some(dir) = current {
        let candidate = dir.join(".git");
        if candidate.is_dir() {
            return Some(candidate);
        }
        if candidate.is_file() {
            // `gitdir: /path/to/.git/worktrees/<name>`
            return std::fs::read_to_string(&candidate)
                .ok()?
                .trim()
                .strip_prefix("gitdir: ")
                .map(|path| PathBuf::from(path.trim()));
        }
        current = dir.parent();
    }
    None
}
