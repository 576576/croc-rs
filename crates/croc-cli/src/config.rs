//! `--remember`：把地址类选项记到配置文件里，下次直接复用。
//!
//! 位置与 Go 一致（`<config>/croc/send.json`，`utils.GetConfigDir`）：`CROC_CONFIG_DIR`
//! → `XDG_CONFIG_HOME/croc` → `~/.config/croc`。
//!
//! 只记 `--relay` 与 `--relay6`。Go 会把整个 `Options` 写进去（那里面还有覆盖策略、
//! 曲线、`--git` 之类），而它们要么本来就有合理默认，要么每次该重问一遍——记下来反而
//! 会让"上次随手加的一个开关"悄悄影响后面的每一次传输。

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// 记住的那几项。`None` 表示"上次用的是默认值"。
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Remembered {
    /// 中继地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<String>,
    /// IPv6 中继地址。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay6: Option<String>,
}

impl Remembered {
    /// 读上次存的。
    ///
    /// 读不到或读坏了都当"没有"——它只是省打字，不该让命令起不来。
    #[must_use]
    pub fn load() -> Self {
        let Some(path) = config_file() else {
            return Self::default();
        };
        std::fs::read_to_string(&path).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    /// 写下来。写不进去只提示一句，**不影响这次传输**。
    pub fn save(&self, quiet: bool) {
        let Some(path) = config_file() else {
            return;
        };
        let write = || -> std::io::Result<()> {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let text = serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_owned());
            std::fs::write(&path, text)
        };
        if let Err(error) = write()
            && !quiet
        {
            eprintln!("croc-rs: 无法写入 {}：{error}", path.display());
        }
    }
}

/// 配置文件路径，对应 Go 的 `getSendConfigFile`（`cli.go:350`）。
fn config_file() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("CROC_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("send.json"));
    }
    if let Some(dir) = std::env::var_os("XDG_CONFIG_HOME") {
        return Some(PathBuf::from(dir).join("croc").join("send.json"));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".config").join("croc").join("send.json"))
}
