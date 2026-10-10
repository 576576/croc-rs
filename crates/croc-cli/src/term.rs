//! 终端样式与进度条：照 Go 的 `src/termui`。
//!
//! 两段规则各自独立：
//!
//! * 要不要上色：stderr 是终端、没设 `NO_COLOR`、`TERM` 不是 `dumb`。
//!   Go 在此之上还用 `colorable` 在 Windows 上开 VT——本实现不打 ANSI 就无从谈起，
//!   而 Windows Terminal / VS Code 那类现代终端本来就开着 VT。
//! * 进度条的格子数：20，与 Go 的 `progressBarWidth` 一致。

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const RESET: &str = "\x1b[0m";
const BOLD: &str = "\x1b[1m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const CYAN: &str = "\x1b[36m";

/// 进度条的格子数（Go 的 `progressBarWidth`）。
const BAR_WIDTH: usize = 20;
/// 上报间隔：两次渲染之间至少隔这么久，免得把终端刷爆。
const THROTTLE: Duration = Duration::from_millis(100);

/// 是否使用 ANSI 样式。
#[must_use]
pub fn color_enabled() -> bool {
    std::io::stderr().is_terminal()
        && std::env::var_os("NO_COLOR").is_none()
        && !std::env::var("TERM").is_ok_and(|term| term.trim().eq_ignore_ascii_case("dumb"))
}

fn paint(text: &str, style: &str) -> String {
    if color_enabled() { format!("{style}{text}{RESET}") } else { text.to_owned() }
}

/// 口令码：Go 用黄色高亮它（`termui.Secret`）。
#[must_use]
pub fn secret(text: &str) -> String {
    paint(text, YELLOW)
}

/// 文件名：Go 用粗体，颜色交给终端自己的前景色（`termui.Filename`）。
#[must_use]
pub fn filename(text: &str) -> String {
    paint(text, BOLD)
}

/// 进度条：一个文件一条，从 0 走到这个文件的大小——Go 也是每个文件重置一次。
///
/// 形状照 `schollz/progressbar`：`{文件名} {百分比}% |{格子}| ({已传}/{总量}, {速率}/s)`。
pub struct Bar {
    description: String,
    total: u64,
    transferred: u64,
    rate: f64,
    sample: (Instant, u64),
    last_paint: Instant,
    /// 上一次渲染的长度（按字符数算），用来补齐短行后面的残字。
    last_width: usize,
}

impl Bar {
    /// 一条新的进度条。`total` 为 0（空文件）时按已完成渲染。
    #[must_use]
    pub fn new(description: String, total: u64) -> Self {
        let now = Instant::now();
        Self {
            description,
            total,
            transferred: 0,
            rate: 0.0,
            sample: (now, 0),
            last_paint: now - THROTTLE,
            last_width: 0,
        }
    }

    /// 这条的满格值。
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// 更新已传字节数并顺手重算速率。
    pub fn set(&mut self, transferred: u64) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(self.sample.0);
        if elapsed >= THROTTLE {
            let delta = transferred.saturating_sub(self.sample.1);
            // 指数平滑：单个采样窗口的瞬时值抖得没法看。
            let instant = delta as f64 / elapsed.as_secs_f64();
            self.rate = if self.rate == 0.0 { instant } else { self.rate.mul_add(0.7, instant * 0.3) };
            self.sample = (now, transferred);
        }
        self.transferred = transferred;
    }

    /// 渲染一行（不含 `\r` 与换行）。返回 `None` 表示这次不必重画。
    #[must_use]
    pub fn line(&mut self, finished: bool) -> Option<String> {
        let now = Instant::now();
        if !finished && now.saturating_duration_since(self.last_paint) < THROTTLE {
            return None;
        }
        self.last_paint = now;
        let percent = percent_of(self.transferred, self.total);
        let filled = usize::try_from(percent).unwrap_or(usize::MAX).min(100) * BAR_WIDTH / 100;
        let done = "█".repeat(filled);
        let rest = " ".repeat(BAR_WIDTH - filled);
        let bar = if color_enabled() {
            // 满了就把 cyan 换成 green——Go 在 writer 里做的同一件事。
            let color = if percent >= 100 { GREEN } else { CYAN };
            format!("|{color}{done}{RESET}{rest}|")
        } else {
            format!("|{done}{rest}|")
        };
        let prefix = format!(
            "{} {percent:3}% {bar} ({}/{}, {}/s)",
            filename(&self.description),
            crate::notice::byte_count_decimal(self.transferred),
            crate::notice::byte_count_decimal(self.total),
            crate::notice::byte_count_decimal(self.rate as u64),
        );
        let visible = prefix.chars().count();
        // 新行比上一行短时，尾巴上会留下上一行的字。
        let padding = self.last_width.saturating_sub(visible);
        self.last_width = visible;
        Some(format!("{prefix}{}", " ".repeat(padding)))
    }
}

/// 发送还是接收：只决定那一行提示的措辞与箭头方向。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `Sending (->对端)`。
    Send,
    /// `Receiving (<-对端)`。
    Receive,
}

impl Direction {
    /// Go 的措辞。
    const fn label(self) -> &'static str {
        match self {
            Self::Send => "Sending",
            Self::Receive => "Receiving",
        }
    }

    /// 箭头指向对端。
    const fn arrow(self) -> &'static str {
        match self {
            Self::Send => "->",
            Self::Receive => "<-",
        }
    }
}

/// 传输期的终端显示：一行方向提示 + 每个文件一条进度条。
///
/// 事件出口是 `Fn`（不能捕获 `&mut`），跨事件要留的东西只能放在这里面用锁带着。
pub struct Meter {
    peer: Arc<Mutex<String>>,
    bar: Arc<Mutex<Option<Bar>>>,
    directed: Arc<AtomicBool>,
    direction: Direction,
}

impl Meter {
    /// 一个方向的显示。
    #[must_use]
    pub fn new(direction: Direction) -> Self {
        Self {
            peer: Arc::new(Mutex::new(String::new())),
            bar: Arc::new(Mutex::new(None)),
            directed: Arc::new(AtomicBool::new(false)),
            direction,
        }
    }

    /// 记下对端地址。
    pub fn set_peer(&self, address: String) {
        if let Ok(mut slot) = self.peer.lock() {
            *slot = address;
        }
    }

    /// 开一个新文件的进度条。返回值是要整行打印的方向提示——一趟传输只打一次。
    pub fn start(&self, name: String, size: u64) -> Option<String> {
        let header = if self.directed.swap(true, Ordering::Relaxed) {
            None
        } else {
            let peer = self.peer.lock().map(|slot| slot.clone()).unwrap_or_default();
            Some(format!("\n{} ({}{})", self.direction.label(), self.direction.arrow(), peer))
        };
        if let Ok(mut slot) = self.bar.lock() {
            *slot = Some(Bar::new(name, size));
        }
        header
    }

    /// 文件内的进度。
    pub fn tick(&self, transferred: u64) {
        if let Ok(mut slot) = self.bar.lock()
            && let Some(bar) = slot.as_mut()
        {
            bar.set(transferred);
            if let Some(line) = bar.line(false) {
                eprint!("\r{line}");
            }
        }
    }

    /// 收尾：把条推到满格并换行。
    pub fn finish(&self) {
        if let Ok(mut slot) = self.bar.lock()
            && let Some(bar) = slot.as_mut()
        {
            let total = bar.total();
            bar.set(total);
            if let Some(line) = bar.line(true) {
                eprint!("\r{line}");
                eprintln!();
            }
        }
    }
}

/// 百分比。`total` 为 0 时算 100%（空文件）。
#[must_use]
pub fn percent_of(transferred: u64, total: u64) -> u64 {
    if total == 0 {
        return 100;
    }
    transferred.saturating_mul(100).checked_div(total).unwrap_or(100).min(100)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_files_are_reported_complete() {
        assert_eq!(percent_of(0, 0), 100);
    }

    #[test]
    fn the_percentage_is_capped_at_one_hundred() {
        assert_eq!(percent_of(9, 10), 90);
        assert_eq!(percent_of(10, 10), 100);
        assert_eq!(percent_of(11, 10), 100);
    }

    #[test]
    fn a_bar_renders_the_go_shape() {
        let mut bar = Bar::new("a.bin".to_owned(), 1024);
        bar.set(512);
        let line = bar.line(true).expect("首帧应当渲染");
        // 去色后应当能拼出 Go 的那一段形状。
        let plain = line.replace("\x1b[1m", "").replace("\x1b[36m", "").replace("\x1b[0m", "");
        assert!(plain.starts_with("a.bin  50% |"), "{plain}");
        assert!(plain.contains("| (512 B/1.0 kB,"), "{plain}");
        assert!(plain.ends_with("/s)"), "{plain}");
    }
}
