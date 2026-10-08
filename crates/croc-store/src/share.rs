//! [`Share`]：一次暂存传输的取件凭证。
//!
//! 它由三部分组成：服务来源、服务端生成的对象 ID（公开）、以及**主密钥**（秘密本身）。
//! 同一个 `Share` 有两种编码：
//!
//! - 浏览器链接 `<origin>/s/<id>#v1.<密钥>`——密钥在 fragment 里，浏览器不会把它发出去；
//! - 命令行 token `croc-store-v1.<来源>.<id>.<密钥>`——贴进终端时不会被 shell 改动。

use crate::{Error, KEY_SIZE, PROTOCOL, TRANSFER_ID_LEN};

/// 一次暂存传输的取件凭证。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// 服务来源。只有 scheme 与 host，例如 `https://getcroc.com`。
    pub origin: String,
    /// 对象 ID，base64url。公开值。
    pub id: String,
    /// 主密钥。拿到它就等于拿到文件。
    pub master_key: Vec<u8>,
}

impl Share {
    /// 编成浏览器链接。
    ///
    /// # Errors
    ///
    /// 来源不合法（非 https、含路径或查询串等），或 ID / 密钥长度不对。
    pub fn browser_url(&self) -> Result<String, Error> {
        let origin = normalize_origin(&self.origin)?;
        validate(self)?;
        Ok(format!("{origin}/s/{}#v1.{}", self.id, crate::base64url::encode(&self.master_key)))
    }

    /// 编成命令行 token。
    ///
    /// # Errors
    ///
    /// 同 [`Share::browser_url`]。
    pub fn cli_token(&self) -> Result<String, Error> {
        let origin = normalize_origin(&self.origin)?;
        validate(self)?;
        Ok(format!(
            "{PROTOCOL}.{}.{}.{}",
            crate::base64url::encode(origin.as_bytes()),
            self.id,
            crate::base64url::encode(&self.master_key)
        ))
    }

    /// 解析浏览器链接或命令行 token。
    ///
    /// # Errors
    ///
    /// 两种格式都不是，或解析出来的内容不合法。
    pub fn parse(value: &str) -> Result<Self, Error> {
        let value = value.trim();
        let share = match value.strip_prefix(PROTOCOL).and_then(|rest| rest.strip_prefix('.')) {
            Some(rest) => parse_token(rest)?,
            None => parse_url(value)?,
        };
        let origin = normalize_origin(&share.origin)?;
        validate(&share)?;
        Ok(Self { origin, ..share })
    }
}

/// 这个字符串看起来像暂存传输的链接或 token 吗。
///
/// 用来把 store 凭证与普通口令码区分开：前者是**凭据**，不该当作命令行参数出现
/// （会进 shell 历史与进程表）。CLI 因此要求它走环境变量。
#[must_use]
pub fn is_stored_value(value: &str) -> bool {
    let value = value.trim();
    if value.strip_prefix(PROTOCOL).is_some_and(|rest| rest.starts_with('.')) {
        return true;
    }
    let Some((_, remainder)) = value.split_once("://") else {
        return false;
    };
    // 取路径部分（去掉查询串与 fragment），Go 那边是 `url.Parse` 后的 `.Path`。
    let without_suffix = remainder.split(['?', '#']).next().unwrap_or(remainder);
    without_suffix.split_once('/').is_some_and(|(_, path)| path.starts_with("s/"))
}

/// `croc-store-v1.` 之后的四段（来源、ID、密钥；前缀已经去掉，所以这里剩三段加一个空段）。
fn parse_token(rest: &str) -> Result<Share, Error> {
    let mut parts = rest.split('.');
    let (Some(encoded_origin), Some(id), Some(encoded_key), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(Error::Format("invalid stored-transfer token".to_owned()));
    };
    let origin = decode_text(encoded_origin, "origin")?;
    let master_key =
        crate::base64url::decode(encoded_key).map_err(|_| Error::Format("invalid stored-transfer key".to_owned()))?;
    Ok(Share { origin, id: id.to_owned(), master_key })
}

/// `<scheme>://<host>/s/<id>#v1.<密钥>`。
fn parse_url(value: &str) -> Result<Share, Error> {
    let Some((scheme, remainder)) = value.split_once("://") else {
        return Err(url_error());
    };
    // 链接里的用户名、查询串、路径都必须是空的；fragment 是密钥。
    let (before_fragment, fragment) =
        remainder.split_once('#').map_or((remainder, None), |(before, fragment)| (before, Some(fragment)));
    if before_fragment.contains(['?', '@']) {
        return Err(url_error());
    }
    let (authority, path) =
        before_fragment.split_once('/').map_or((before_fragment, ""), |(authority, path)| (authority, path));
    if authority.is_empty() {
        return Err(url_error());
    }
    // 路径必须是 `s/<id>`，去掉首尾的 `/`（Go 那边同样先 `Trim` 再切）。
    let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
    let [directory, id] = segments.as_slice() else {
        return Err(url_error());
    };
    if *directory != "s" || id.is_empty() {
        return Err(url_error());
    }
    let Some(fragment) = fragment else {
        return Err(url_error());
    };
    let Some(encoded_key) = fragment.strip_prefix("v1.") else {
        return Err(url_error());
    };
    let master_key = crate::base64url::decode(encoded_key)
        .map_err(|_| Error::Format("invalid stored-transfer URL key".to_owned()))?;
    Ok(Share { origin: format!("{scheme}://{authority}"), id: (*id).to_owned(), master_key })
}

/// 来源只允许 `scheme://host`，且必须是 https——只有回环地址才允许明文 http。
fn normalize_origin(value: &str) -> Result<String, Error> {
    let value = value.trim();
    let Some((scheme, remainder)) = value.split_once("://") else {
        return Err(origin_error());
    };
    if remainder.contains(['?', '#', '@']) {
        return Err(origin_error());
    }
    let (host, path) = remainder.split_once('/').map_or((remainder, ""), |(host, path)| (host, path));
    // Go 允许 `https://host/`（路径就是一个斜杠），但别的路径都不行。
    if host.is_empty() || !path.is_empty() {
        return Err(origin_error());
    }
    if scheme != "https" && !(scheme == "http" && is_loopback_host(hostname(host))) {
        return Err(origin_error());
    }
    Ok(format!("{scheme}://{host}"))
}

/// 从 `host[:port]` 里取出主机名（IPv6 字面量要脱掉方括号）。
fn hostname(authority: &str) -> &str {
    if let Some(rest) = authority.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest);
    }
    match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|character| character.is_ascii_digit()) => host,
        _ => authority,
    }
}

/// 回环地址是唯一允许明文 http 的地方——本机自托管不需要 TLS。
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// ID 必须是规范编码的 16 字节，密钥必须是 32 字节。
fn validate(share: &Share) -> Result<(), Error> {
    let id = crate::base64url::decode(&share.id).map_err(|_| Error::Format("invalid stored-transfer id".to_owned()))?;
    if id.len() != TRANSFER_ID_LEN || crate::base64url::encode(&id) != share.id {
        return Err(Error::Format("invalid stored-transfer id".to_owned()));
    }
    if share.master_key.len() != KEY_SIZE {
        return Err(Error::Format("invalid stored-transfer key length".to_owned()));
    }
    Ok(())
}

/// 解一段 base64url，再要求它是 UTF-8 文本。
fn decode_text(value: &str, what: &str) -> Result<String, Error> {
    let bytes =
        crate::base64url::decode(value).map_err(|_| Error::Format(format!("invalid stored-transfer {what}")))?;
    String::from_utf8(bytes).map_err(|_| Error::Format(format!("invalid stored-transfer {what}")))
}

fn url_error() -> Error {
    Error::Format("invalid stored-transfer URL".to_owned())
}

fn origin_error() -> Error {
    Error::Format("stored-transfer origin must contain only scheme and host".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "AAECAwQFBgcICQoLDA0ODw";

    fn share(origin: &str) -> Share {
        Share { origin: origin.to_owned(), id: ID.to_owned(), master_key: vec![7u8; KEY_SIZE] }
    }

    #[test]
    fn a_browser_url_round_trips() {
        let value = share("https://getcroc.com");
        let url = value.browser_url().expect("编码");
        assert!(url.starts_with("https://getcroc.com/s/"), "{url}");
        assert!(url.contains("#v1."));
        assert_eq!(Share::parse(&url).expect("解析"), value);
    }

    #[test]
    fn a_cli_token_round_trips() {
        let value = share("https://getcroc.com");
        let token = value.cli_token().expect("编码");
        assert!(token.starts_with("croc-store-v1."));
        assert_eq!(token.split('.').count(), 4, "前缀 + 三段");
        assert_eq!(Share::parse(&token).expect("解析"), value);
    }

    #[test]
    fn parsing_tolerates_surrounding_whitespace() {
        // 从终端里粘出来的东西常常带着换行。
        let token = share("https://getcroc.com").cli_token().expect("编码");
        assert!(Share::parse(&format!("  {token}\n")).is_ok());
    }

    #[test]
    fn a_non_https_origin_is_refused() {
        assert!(matches!(share("http://getcroc.com").browser_url(), Err(Error::Format(_))));
    }

    #[test]
    fn plain_http_is_allowed_only_on_loopback() {
        // 本机自托管不需要 TLS。
        assert!(share("http://127.0.0.1:8080").browser_url().is_ok());
        assert!(share("http://localhost:8080").browser_url().is_ok());
        assert!(share("http://[::1]:8080").browser_url().is_ok());
        assert!(matches!(share("http://192.168.1.5:8080").browser_url(), Err(Error::Format(_))));
        // 名字里带 localhost 不算回环。
        assert!(matches!(share("http://localhost.example.com").browser_url(), Err(Error::Format(_))));
    }

    #[test]
    fn an_origin_with_a_path_or_query_is_refused() {
        for origin in [
            "https://getcroc.com/prefix",
            "https://getcroc.com?x=1",
            "https://getcroc.com#f",
            "https://user@getcroc.com",
        ] {
            assert!(matches!(share(origin).browser_url(), Err(Error::Format(_))), "{origin}");
        }
        // 单独的根斜杠是允许的。
        assert!(share("https://getcroc.com/").browser_url().is_ok());
    }

    #[test]
    fn a_malformed_id_or_key_is_refused() {
        let mut wrong_id = share("https://getcroc.com");
        wrong_id.id = "AAAA".to_owned();
        assert!(matches!(wrong_id.browser_url(), Err(Error::Format(_))));

        let mut wrong_key = share("https://getcroc.com");
        wrong_key.master_key = vec![0u8; 31];
        assert!(matches!(wrong_key.browser_url(), Err(Error::Format(_))));
    }

    #[test]
    fn a_malformed_url_is_refused() {
        for url in [
            "https://getcroc.com/x/abc#v1.abc",  // 路径不是 /s/
            "https://getcroc.com/s/abc",         // 缺 fragment
            "https://getcroc.com/s/abc#v2.abc",  // 版本不对
            "https://getcroc.com/s/abc#v1.!!!!", // 密钥不是 base64url
            "not-a-url",
            "https://getcroc.com/s/a/b#v1.abc", // 路径段数不对
        ] {
            assert!(Share::parse(url).is_err(), "{url} 应当被拒");
        }
    }

    #[test]
    fn a_malformed_token_is_refused() {
        for token in ["croc-store-v1.a.b", "croc-store-v1.a.b.c.d", "croc-store-v1.!!!!.b.c"] {
            assert!(Share::parse(token).is_err(), "{token} 应当被拒");
        }
    }

    #[test]
    fn stored_values_are_recognised() {
        // CLI 靠它把 store 凭证挡在命令行之外。
        assert!(is_stored_value("croc-store-v1.abc.def.ghi"));
        assert!(is_stored_value("https://getcroc.com/s/abc#v1.key"));
        assert!(is_stored_value("  https://getcroc.com/s/abc#v1.key  "));
        assert!(!is_stored_value("acid-acorn-acre"));
        assert!(!is_stored_value("https://getcroc.com/xs/abc"));
        assert!(!is_stored_value("https://getcroc.com/"));
    }

    #[test]
    fn a_hostname_is_extracted_from_the_authority() {
        assert_eq!(hostname("example.com"), "example.com");
        assert_eq!(hostname("example.com:8443"), "example.com");
        assert_eq!(hostname("[::1]:8443"), "::1");
        assert_eq!(hostname("127.0.0.1"), "127.0.0.1");
    }
}
