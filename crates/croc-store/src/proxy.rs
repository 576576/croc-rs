//! 出站代理：`--socks5` / `--connect`。
//!
//! store 的请求也走这两个开关——Go 那边是全局的 `comm.Dial`，store 客户端与普通传输
//! 共用同一份代理配置。
//!
//! 两个都给时 **socks5 优先**（Go 的 `comm.Dial` 就是 if / else if）。

/// 出站代理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Proxy {
    /// SOCKS5（`--socks5`）。
    Socks5(String),
    /// HTTP CONNECT 隧道（`--connect`）。
    Http(String),
}

impl Proxy {
    /// 从两个 flag 里挑一个。空串按"没给"处理。
    #[must_use]
    pub fn from_flags(socks5: Option<&str>, connect: Option<&str>) -> Option<Self> {
        if let Some(address) = usable(socks5) {
            return Some(Self::Socks5(address.to_owned()));
        }
        usable(connect).map(|address| Self::Http(address.to_owned()))
    }

    /// 交给 `reqwest` 的代理 URL。
    ///
    /// socks5 用 `socks5h`：域名交给**代理**去解析。用 `socks5` 的话本地会先解析一遍，
    /// DNS 请求就漏到网外了——`--socks5` 的典型用途（Tor）要的正是避免这个。
    #[must_use]
    pub fn url(&self) -> String {
        match self {
            Self::Socks5(address) => format!("socks5h://{}", strip_scheme(address)),
            Self::Http(address) => format!("http://{}", strip_scheme(address)),
        }
    }
}

/// 空串与全空白都算"没给"。
fn usable(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.trim().is_empty())
}

/// 用户可能已经写了 `socks5://` 或 `http://`，去掉它再拼我们自己的。
fn strip_scheme(address: &str) -> &str {
    address.split_once("://").map_or(address, |(_, rest)| rest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks5_wins_when_both_are_given() {
        // Go 的 `comm.Dial` 先查 Socks5Proxy（`src/comm/comm.go:67`）。
        assert_eq!(
            Proxy::from_flags(Some("127.0.0.1:9050"), Some("127.0.0.1:8080")),
            Some(Proxy::Socks5("127.0.0.1:9050".to_owned()))
        );
        assert_eq!(Proxy::from_flags(None, Some("127.0.0.1:8080")), Some(Proxy::Http("127.0.0.1:8080".to_owned())));
        assert_eq!(Proxy::from_flags(None, None), None);
    }

    #[test]
    fn blank_flags_mean_no_proxy() {
        // 环境变量没设时拿到的是空串，不能当成"给了个空代理"。
        assert_eq!(Proxy::from_flags(Some(""), Some("  ")), None);
    }

    #[test]
    fn the_scheme_is_normalised() {
        assert_eq!(Proxy::Socks5("socks5://127.0.0.1:9050".to_owned()).url(), "socks5h://127.0.0.1:9050");
        assert_eq!(Proxy::Socks5("127.0.0.1:9050".to_owned()).url(), "socks5h://127.0.0.1:9050");
        assert_eq!(Proxy::Http("http://127.0.0.1:8080".to_owned()).url(), "http://127.0.0.1:8080");
        assert_eq!(Proxy::Http("127.0.0.1:8080".to_owned()).url(), "http://127.0.0.1:8080");
    }
}
