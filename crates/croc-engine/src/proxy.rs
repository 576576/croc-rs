//! 出站代理的配置：`--socks5` / `--connect`。
//!
//! 拨号本身在 [`crate::relay`] 里——那里已经有一个"数字 IP 直连"的优化和一个连接超时，
//! 拨号逻辑放一起才不用把这两样东西复制一份。

use crate::error::EngineError;

/// 出站代理。
///
/// 两个 flag 都给时 socks5 优先——Go 的 `comm.Dial` 就是 if / else if
/// 。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Proxy {
    /// 直连。
    #[default]
    Direct,
    /// SOCKS5（`--socks5`）。
    Socks5(String),
    /// HTTP CONNECT 隧道（`--connect`）。
    Http(String),
}

impl Proxy {
    /// 从两个 flag 里挑一个。空串按"没给"处理。
    #[must_use]
    pub fn from_flags(socks5: Option<&str>, connect: Option<&str>) -> Self {
        if let Some(address) = usable(socks5) {
            return Self::Socks5(address.to_owned());
        }
        if let Some(address) = usable(connect) {
            return Self::Http(address.to_owned());
        }
        Self::Direct
    }

    /// 代理地址（`host:port`，scheme 已去掉）。
    ///
    /// # Errors
    ///
    /// 地址里带了 `user:pass@`。本实现不做代理认证——与其静默把认证信息丢掉、让用户对着
    /// 一句"连不上"排查半天，不如在这里说清楚。
    pub fn address(&self) -> Result<Option<&str>, EngineError> {
        let raw = match self {
            Self::Direct => return Ok(None),
            Self::Socks5(address) | Self::Http(address) => address.as_str(),
        };
        let host_port = raw.split_once("://").map_or(raw, |(_, rest)| rest);
        if host_port.contains('@') {
            return Err(EngineError::Config("代理地址里带了 `user:pass@`：本实现不支持代理认证".to_owned()));
        }
        Ok(Some(host_port))
    }
}

/// 空串与全空白都算"没给"。
fn usable(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socks5_wins_when_both_are_given() {
        // Go 的 `comm.Dial` 先查 Socks5Proxy。
        assert_eq!(
            Proxy::from_flags(Some("127.0.0.1:9050"), Some("127.0.0.1:8080")),
            Proxy::Socks5("127.0.0.1:9050".to_owned())
        );
        assert_eq!(Proxy::from_flags(None, Some("127.0.0.1:8080")), Proxy::Http("127.0.0.1:8080".to_owned()));
        assert_eq!(Proxy::from_flags(None, None), Proxy::Direct);
    }

    #[test]
    fn blank_flags_mean_no_proxy() {
        // 环境变量没设时拿到的是空串，不能当成"给了个空代理"。
        assert_eq!(Proxy::from_flags(Some(""), Some("  ")), Proxy::Direct);
    }

    #[test]
    fn the_scheme_is_stripped_to_a_bare_host_port() {
        assert_eq!(
            Proxy::Socks5("socks5://127.0.0.1:9050".to_owned()).address().expect("解析"),
            Some("127.0.0.1:9050")
        );
        assert_eq!(Proxy::Http("http://127.0.0.1:8080".to_owned()).address().expect("解析"), Some("127.0.0.1:8080"));
        assert_eq!(Proxy::Socks5("127.0.0.1:9050".to_owned()).address().expect("解析"), Some("127.0.0.1:9050"));
        assert_eq!(Proxy::Direct.address().expect("解析"), None);
    }

    #[test]
    fn proxy_credentials_are_refused_rather_than_ignored() {
        let error = Proxy::Socks5("user:pass@127.0.0.1:9050".to_owned()).address().expect_err("应当报错");
        assert!(error.to_string().contains("代理认证"), "{error}");
    }
}
