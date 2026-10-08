//! `--disable-clipboard` / `--extended-clipboard`：把口令码送进系统剪贴板。
//!
//! 挑程序的口径照抄 Go 的 `copyToClipboard`（`croc.go:3868`）：Windows 一定有 `clip`；
//! macOS 用 `pbcopy`；Linux 与各 BSD 按 `XDG_SESSION_TYPE` 在 Wayland（`wl-copy` /
//! `waycopy`）与 X11（`xclip` / `xsel`）里挑，都没有再退到 Termux 的
//! `termux-clipboard-set`。
//!
//! 与 Go 不同的是**不做 `PATH` 预探测**：直接按顺序试着启动，第一个跑通的算数。效果一样
//! （跑不起来的那个会立刻返回错误），少一份自己实现的 `LookPath`。
//!
//! 挑不到任何程序就返回 `false`——**这不是错误**，只是没这份方便。

use std::io::Write as _;
use std::process::{Command, Stdio};

/// 把 `text` 送进剪贴板。
///
/// 返回是否成功。失败**不打日志、不影响退出码**：复制是顺手做的事，它失败了也不该让一次
/// 传输看起来像是出了问题。
#[must_use]
pub fn copy(text: &str) -> bool {
    candidates().into_iter().any(|(program, args)| feed(program, args, text))
}

/// 这个平台上按优先级排好的候选命令。
fn candidates() -> Vec<(&'static str, &'static [&'static str])> {
    if cfg!(target_os = "windows") {
        // Windows 自带 `clip.exe`，不用挑。
        return vec![("clip", &[])];
    }
    if cfg!(target_os = "macos") {
        return vec![("pbcopy", &[])];
    }
    match std::env::var("XDG_SESSION_TYPE").as_deref() {
        Ok("wayland") => vec![("wl-copy", &[]), ("waycopy", &[]), ("xclip", &["-selection", "clipboard"])],
        Ok("x11" | "xorg") => vec![("xclip", &["-selection", "clipboard"]), ("xsel", &["-b"]), ("wl-copy", &[])],
        // 这个变量在 SSH / 容器里常常是空的。Go 这时只试 Termux 那一个，这里把常见的
        // 几种都试一遍——排在后面，选错了也只是这一次没复制上。
        _ => vec![
            ("termux-clipboard-set", &[]),
            ("wl-copy", &[]),
            ("waycopy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["-b"]),
        ],
    }
}

/// 跑一个候选程序，把文本从 stdin 喂进去。
fn feed(program: &str, args: &[&str], text: &str) -> bool {
    // 子进程的 stdout / stderr 一律丢弃：`clip` 之类在出错时会往 stderr 写东西，
    // 那行输出对用户没有意义，却会混进传输日志里。
    let Ok(mut child) =
        Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
    else {
        return false;
    };
    // stdin 在闭包里落进作用域，出了闭包就关掉——不关的话 `xclip` 这类会一直等更多输入。
    let written = child.stdin.take().is_some_and(|mut stdin| stdin.write_all(text.as_bytes()).is_ok());
    let status = child.wait();
    written && status.is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_platform_gets_at_least_one_candidate() {
        assert!(!candidates().is_empty());
    }

    #[test]
    fn a_missing_program_is_not_an_error() {
        assert!(!feed("croc-rs-no-such-clipboard-helper", &[], "x"));
    }

    #[test]
    fn a_failing_program_is_not_an_error() {
        // 进程能起来但退出码非 0：与"找不到程序"一样只算失败。
        let failing = if cfg!(target_os = "windows") { "cmd" } else { "false" };
        let args: &[&str] = if cfg!(target_os = "windows") { &["/c", "exit 1"] } else { &[] };
        assert!(!feed(failing, args, "x"));
    }
}
