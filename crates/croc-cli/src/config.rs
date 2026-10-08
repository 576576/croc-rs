//! `--remember` 与 `--classic`：配置文件的位置、格式与读写。
//!
//! 目录沿用 Go 的 `utils.GetConfigDir`（`utils.go:64`）：`CROC_CONFIG_DIR` →
//! `XDG_CONFIG_HOME/croc` → `~/.config/croc`。文件也是同一套：
//!
//! | 文件 | 谁写 | 谁读 |
//! |------|------|------|
//! | `send.json` | `send --remember` | `send`（只在本次**没给** `--remember` 时） |
//! | `receive.json` | 接收端 `--remember` | 接收（同上） |
//! | `classic_enabled` | `--classic` | 收发两端（决定要不要拒绝命令行口令码） |
//!
//! # 字段名照抄 Go
//!
//! `Remembered` 的 JSON 字段名是 Go `croc.Options` 的名字（`SharedSecret`、
//! `RelayAddress`……），而且只写 Go 也会**应用**的那些字段：两个实现在同一台机器上
//! 共用同一份 `send.json`，谁写谁读都不会把对方的设置清成零值。
//!
//! 中继地址用 Go 的编码记「用户改过没有」：`"default"` 或 `"non-default: <地址>"`。
//! 只存地址本身的话，就没法区分「上次用的是默认中继」与「上次没设过中继」——前者该不该
//! 覆盖这一次的默认值，是两回事。

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// 读哪一侧的配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// `send.json`。
    Send,
    /// `receive.json`。
    Receive,
}

impl Side {
    const fn file_name(self) -> &'static str {
        match self {
            Self::Send => "send.json",
            Self::Receive => "receive.json",
        }
    }
}

/// 中继地址「还是默认值」的标记。
const DEFAULT_MARKER: &str = "default";

/// 中继地址「被用户改过」的前缀。Go 的 `saveConfig`（`cli.go:946`）。
const NON_DEFAULT_PREFIX: &str = "non-default:";

/// 记住的那几项。
///
/// `#[serde(rename)]` 里的名字是 Go `croc.Options` 的字段名，不要改。
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools, reason = "每一项对应一个 Go 的布尔 flag，合并成位标志只会更难与上游对照")]
pub struct Remembered {
    /// 口令码。**只有命令行确实给了才写**（Go 的 `saveConfig` 同）。
    #[serde(rename = "SharedSecret", default)]
    pub shared_secret: String,
    /// 中继地址，编码见模块说明。
    #[serde(rename = "RelayAddress", default)]
    pub relay_address: String,
    /// IPv6 中继地址，编码同上。
    #[serde(rename = "RelayAddress6", default)]
    pub relay_address6: String,
    /// 中继密码。
    #[serde(rename = "RelayPassword", default)]
    pub relay_password: String,
    /// `--yes`。
    #[serde(rename = "NoPrompt", default)]
    pub no_prompt: bool,
    /// `--overwrite`。
    #[serde(rename = "Overwrite", default)]
    pub overwrite: bool,
    /// `--rename`。
    #[serde(rename = "Rename", default)]
    pub rename: bool,
    /// `--curve`。
    #[serde(rename = "Curve", default)]
    pub curve: String,
    /// `--local`。
    #[serde(rename = "OnlyLocal", default)]
    pub only_local: bool,
    /// `--hash`。
    #[serde(rename = "HashAlgorithm", default)]
    pub hash_algorithm: String,
    /// `--git`。
    #[serde(rename = "GitIgnore", default)]
    pub git_ignore: bool,
    /// `--disable-clipboard`。
    #[serde(rename = "DisableClipboard", default)]
    pub disable_clipboard: bool,
    /// `--no-local`。
    #[serde(rename = "DisableLocal", default)]
    pub disable_local: bool,
    /// `--transport`。Go 的 `TransportMode` 也是 `"auto"` / `"relay"` / `"derp"`。
    #[serde(rename = "Transport", default, skip_serializing_if = "String::is_empty")]
    pub transport: String,
}

impl Remembered {
    /// 读上次存的。
    ///
    /// 读不到、读坏了、字段对不上都当「没有」——它只是省打字，不该让命令起不来。
    #[must_use]
    pub fn load(side: Side) -> Self {
        let Some(path) = config_dir().map(|dir| dir.join(side.file_name())) else {
            return Self::default();
        };
        std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// 写下来。写不进去只提示一句，**不影响这次传输**。
    pub fn save(&self, side: Side, quiet: bool) {
        let Some(path) = config_dir().map(|dir| dir.join(side.file_name())) else {
            return;
        };
        let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned());
        if let Err(error) = create_private_dirs_and_write(&path, text.as_bytes())
            && !quiet
        {
            eprintln!("croc-rs: 无法写入 {}：{error}", path.display());
        }
    }
}

/// 把一个中继地址编成配置文件里的写法。
///
/// 用户改过的地址带 [`NON_DEFAULT_PREFIX`]：读的时候只有带了前缀才会覆盖默认值。
#[must_use]
pub fn encode_relay(current: &str, default: &str) -> String {
    if current == default { DEFAULT_MARKER.to_owned() } else { format!("{NON_DEFAULT_PREFIX} {current}") }
}

/// 从配置文件里的写法取回地址；`"default"` 与空白值都返回 `None`。
#[must_use]
pub fn decode_relay(remembered: &str) -> Option<&str> {
    remembered.strip_prefix(NON_DEFAULT_PREFIX).map(str::trim).filter(|address| !address.is_empty())
}

/// classic 模式开着吗（标记文件存在）。
#[must_use]
pub fn classic_enabled() -> bool {
    config_dir().map(|dir| dir.join("classic_enabled")).is_some_and(|path| path.is_file())
}

/// 打开或关掉 classic 模式。
pub fn set_classic(enabled: bool, quiet: bool) {
    let Some(path) = config_dir().map(|dir| dir.join("classic_enabled")) else {
        return;
    };
    let result = if enabled {
        create_private_dirs_and_write(&path, b"enabled")
    } else {
        match std::fs::remove_file(&path) {
            // 本来就没有：关掉它是幂等的。
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    };
    if let Err(error) = result
        && !quiet
    {
        eprintln!("croc-rs: 无法更新 {}：{error}", path.display());
    }
}

/// `--internal-dns` 的持久化标记路径。Go 就叫这个名字（`constants.go:55`）。
fn internal_dns_marker() -> Option<PathBuf> {
    config_dir().map(|dir| dir.join("internal-dns"))
}

/// 上次有没有用 `--remember --internal-dns` 把它记下来。
#[must_use]
pub fn internal_dns_enabled() -> bool {
    internal_dns_marker().is_some_and(|path| path.is_file())
}

/// 把 `--internal-dns` 记下来（配合 `--remember`）。
///
/// Go 的触发条件是「`--remember` 出现在 argv 里」——**不带 `--internal-dns` 也会把标记
/// 写上**，于是一次 `croc --remember file.txt` 就把内置 DNS 永久打开了。这里要求两个
/// 开关同时给出，差异见 docs/agent/cli.md 的行为差异一节。
pub fn remember_internal_dns(quiet: bool) {
    let Some(path) = internal_dns_marker() else {
        return;
    };
    // 内容无所谓，Go 也是建个空文件——`Exists` 就是全部的语义。
    if let Err(error) = create_private_dirs_and_write(&path, b"")
        && !quiet
    {
        eprintln!("croc-rs: 无法写入 {}：{error}", path.display());
    }
}

/// 配置目录，对应 Go 的 `utils.GetConfigDir`（`utils.go:64`）。
///
/// `CROC_CONFIG_DIR` 是**整个目录**（不再往下拼 `croc`），后两个才是。
pub(crate) fn config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CROC_CONFIG_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("croc"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config").join("croc"))
}

/// 建目录并写文件，Unix 上限定 `0600`。
///
/// 这些文件里有口令码与中继密码，Go 的 `writePrivateConfigFile`（`cli.go:916`）同样
/// 只给属主权限。
fn create_private_dirs_and_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_relay_is_recorded_as_default() {
        // 与 Go 的 `saveConfig` 一致：只有改过的地址才带前缀，读回去也只认带前缀的。
        assert_eq!(encode_relay("croc.schollz.com:9009", "croc.schollz.com:9009"), "default");
        assert_eq!(decode_relay("default"), None);
        let custom = encode_relay("10.0.0.1:9100", "croc.schollz.com:9009");
        assert_eq!(custom, "non-default: 10.0.0.1:9100");
        assert_eq!(decode_relay(&custom), Some("10.0.0.1:9100"));
    }

    #[test]
    fn an_empty_remembered_address_is_ignored() {
        // 空白格子的写法不止一种，读的时候都要当「没记过」。
        assert_eq!(decode_relay("non-default:"), None);
        assert_eq!(decode_relay("non-default:   "), None);
        assert_eq!(decode_relay(""), None);
    }

    #[test]
    fn the_field_names_are_the_go_ones() {
        // 两个实现共用同一份 send.json，字段名改一个对方就读不到了。
        let remembered = Remembered {
            shared_secret: "abc".to_owned(),
            relay_address: "non-default: 10.0.0.1:9100".to_owned(),
            no_prompt: true,
            hash_algorithm: "xxhash".to_owned(),
            ..Remembered::default()
        };
        let json = serde_json::to_string(&remembered).expect("序列化");
        for key in [
            "\"SharedSecret\"",
            "\"RelayAddress\"",
            "\"NoPrompt\"",
            "\"HashAlgorithm\"",
            "\"RelayPassword\"",
            "\"OnlyLocal\"",
        ] {
            assert!(json.contains(key), "缺少 Go 的字段名 {key}：{json}");
        }
        assert!(!json.contains("\"transport\""), "字段名不该回落到 Rust 的写法：{json}");
    }

    #[test]
    fn an_unknown_field_does_not_break_loading() {
        // Go 会写整个 `Options`，里面必然有我们不认识的字段。
        let remembered: Remembered =
            serde_json::from_str(r#"{"SharedSecret":"abc","TestFlag":true,"ExtendedClipboard":true,"Curve":"p256"}"#)
                .expect("应当忽略不认识的字段");
        assert_eq!(remembered.shared_secret, "abc");
        assert_eq!(remembered.curve, "p256");
    }
}
