//! 发送端的终端输出："要发什么"与"让对方执行什么"。
//!
//! 两行都打到 stderr：口令码不该混进 stdout——`croc-rs send ... > log` 时不该把凭据写进文件。
//!
//! 形状照 Go：
//!
//! ```text
//! Sending 'a.bin' (4.9 kB)
//! On the other computer, run:
//!   croc-rs abc-def-ghi
//!
//! Or open:
//!   https://getcroc.com/?code=abc-def-ghi
//! ```
//!
//! 那一行里默认中继不出现：Go 只在地址不是默认值时才写 `--relay`，
//! `--pass` 只在密码不是默认值时写。走公共中继池时更是必须省略——码里带的索引会决定
//! 对端连哪一台，写死一个 `--relay` 反而把它钉在错误的中继上。

use rust_i18n::t;

use croc_engine::SendRequest;

use crate::cli::{Clipboard, DEFAULT_PASSPHRASE, DEFAULT_RELAY};
use crate::locale::Locale;

/// 清掉上一行再重打的宽度。Go 在两条 `Sending` 之间打 33 个空格。
const CLEAR_WIDTH: usize = 33;

/// 发送提示。构造于 `parse()`，打印于第一次 `EngineEvent::Sending`。
///
/// 为什么要等到那一刻：Go 在进 `Client.Send()` 之前就已经拿到了完整的文件信息，所以
/// 它能在算哈希的同时把口令码显示出来。本实现把"清点"与"算哈希"拆成了两步，代价是
/// 口令码晚一瞬间出现，好处是这一行里的字节数是准的。
pub struct SendNotice {
    /// 口令码。
    code: String,
    /// 命令行里的开关（` --relay x` 等）。默认中继下为空。
    flags: String,
    /// `--ask`：把本机 ID 一起打出来，对端要核一下来的是哪台。
    ask: bool,
    machine_id: String,
    qrcode: bool,
    clipboard: Clipboard,
}

impl SendNotice {
    /// 从一次发送请求里取出提示所需的那几项。
    ///
    /// `public_relay` 表示这次的地址是从公共中继池里挑的：这种情况下不能写
    /// `--relay`——码里带的索引会决定对端连哪一台，写死一个地址反而把它送错。
    #[must_use]
    pub fn new(request: &SendRequest, public_relay: bool, qrcode: bool, clipboard: Clipboard) -> Self {
        Self {
            code: request.code.clone(),
            flags: flags(request.local, public_relay, &request.relay, &request.relay_password),
            ask: request.ask,
            machine_id: request.machine_id.clone(),
            qrcode,
            clipboard,
        }
    }

    /// 打出来。剪贴板先复制再把结果写进提示里——有没有复制上只有试过才知道。
    pub fn print(&self, locale: Locale) {
        let notice = match self.clipboard {
            Clipboard::Off => String::new(),
            choice => {
                if crate::clipboard::copy(&self.clipboard_text()) {
                    let key = match choice {
                        Clipboard::Command => "notice.clipboard_command",
                        _ => "notice.clipboard_code",
                    };
                    format!(" ({})", t!(key, locale = locale.tag()))
                } else {
                    String::new()
                }
            }
        };
        eprintln!("{}\n  croc-rs {}{}{notice}", t!("notice.run_heading", locale = locale.tag()), self.flags, self.code);
        // web 接收页。链接只由口令码拼成，跟中继是谁无关。
        let url = crate::qr::web_receive_url(&self.code);
        eprintln!("\n{}\n  {url}", t!("notice.or_open", locale = locale.tag()));
        if self.qrcode {
            match crate::qr::render(&url) {
                Ok(text) => println!("{text}"),
                Err(reason) => eprintln!("{reason}"),
            }
        }
        if self.ask {
            eprintln!("{}", t!("notice.machine_id", locale = locale.tag(), id = self.machine_id.as_str()));
        }
    }

    /// 要放进剪贴板的那段文本。
    ///
    /// `--extended-clipboard` 复制的是整条接收命令：口令码通过环境变量传，因此不会
    /// 进 shell 历史，也不会留在进程表里（这正是这个开关存在的理由）。
    fn clipboard_text(&self) -> String {
        match self.clipboard {
            Clipboard::Off | Clipboard::Code => self.code.clone(),
            // 命令形态不能有尾空格：贴到 shell 里执行时那只是噪声（Go 用
            // `strings.TrimSpace(flags)` 做同一件事）。
            Clipboard::Command => match self.flags.trim() {
                "" => format!("CROC_SECRET=\"{}\" croc-rs", self.code),
                flags => format!("CROC_SECRET=\"{}\" croc-rs {flags}", self.code),
            },
        }
    }
}

/// 提示里那段开关。`local` / `public_relay` / 中继地址 / 中继密码 → 直接可拼的字符串。
fn flags(local: bool, public_relay: bool, relay: &str, password: &str) -> String {
    let mut items: Vec<String> = Vec::new();
    if local {
        // `--local` 的中继是本端现起的、端口每次都不同，对端靠组播找过来；Go 没有这个
        // 开关（它默认就做局域网发现），所以这里是本实现自己的提示。
        items.push("--local".to_owned());
    } else if !public_relay && relay != DEFAULT_RELAY {
        items.push(format!("--relay {relay}"));
    }
    if password != DEFAULT_PASSPHRASE {
        items.push(format!("--pass {password}"));
    }
    // 尾空格是必须的：Go 的 `flags` 也是每项后面带一个，打印的模板里
    // 另有一个空格，于是"没有开关"拼出来是 `croc-rs <码>`，"有开关"是
    // `croc-rs --relay x <码>`——两种形状都与 Go 逐字相同。
    if items.is_empty() { String::new() } else { format!("{} ", items.join(" ")) }
}

/// 打一行"要发什么"。
///
/// Go 在算哈希期间反复重打这一行，这里只打最终那一次：中间态是
/// 一个不断变大的计数，落盘到文件里只是噪声。
pub fn print_sending(locale: Locale, name: Option<&str>, files: u64, folders: u64, bytes: u64) {
    let label = name.map_or_else(|| count_label(locale, files), |name| format!("'{name}'"));
    let size = byte_count_decimal(bytes);
    let line = if folders == 0 {
        t!("notice.sending", locale = locale.tag(), label = label, size = size)
    } else {
        t!("notice.sending_folders", locale = locale.tag(), label = label, size = size, folders = folders)
    };
    // 先退格再清空：直接打一行新的会在上一行后面留下残字。
    eprintln!("\r{:width$}\r{line}", "", width = CLEAR_WIDTH);
}

/// 多于一个文件时用计数代替文件名（Go 的 `fmt.Sprintf("%d files", n)`）。
fn count_label(locale: Locale, files: u64) -> String {
    t!("notice.files", locale = locale.tag(), count = files).into_owned()
}

/// 字节数。Go 的 `utils.ByteCountDecimal`：1024 进位，单位名
/// 仍是十进制写法（kB / MB / GB）。
#[must_use]
#[allow(clippy::cast_precision_loss, reason = "字节数超过 1024 时精度损失只影响小数位")]
pub fn byte_count_decimal(bytes: u64) -> String {
    const UNIT: u64 = 1024;
    if bytes < UNIT {
        return format!("{bytes} B");
    }
    let mut divisor = UNIT;
    let mut exponent = 0usize;
    let mut rest = bytes / UNIT;
    while rest >= UNIT {
        divisor = divisor.saturating_mul(UNIT);
        exponent += 1;
        rest /= UNIT;
    }
    let suffix = ["k", "M", "G", "T", "P", "E"].get(exponent).copied().unwrap_or("E");
    format!("{:.1} {suffix}B", bytes as f64 / divisor as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_flags_match_go_shape() {
        // 默认中继与默认密码都不写进提示里。
        assert_eq!(flags(false, true, DEFAULT_RELAY, DEFAULT_PASSPHRASE), "");
        assert_eq!(flags(false, false, DEFAULT_RELAY, DEFAULT_PASSPHRASE), "");
        assert_eq!(flags(false, false, "10.0.0.1:9009", DEFAULT_PASSPHRASE), "--relay 10.0.0.1:9009 ");
        assert_eq!(flags(false, false, DEFAULT_RELAY, "secret"), "--pass secret ");
        assert_eq!(flags(false, false, "10.0.0.1:9009", "secret"), "--relay 10.0.0.1:9009 --pass secret ");
        assert_eq!(flags(true, false, DEFAULT_RELAY, DEFAULT_PASSPHRASE), "--local ");
    }

    #[test]
    fn byte_counts_match_go() {
        assert_eq!(byte_count_decimal(0), "0 B");
        assert_eq!(byte_count_decimal(1023), "1023 B");
        assert_eq!(byte_count_decimal(1024), "1.0 kB");
        assert_eq!(byte_count_decimal(5000), "4.9 kB");
        assert_eq!(byte_count_decimal(1_500_000), "1.4 MB");
        assert_eq!(byte_count_decimal(1024 * 1024), "1.0 MB");
        assert_eq!(byte_count_decimal(1024 * 1024 * 1024), "1.0 GB");
    }
}
