//! 诊断文本的脱敏。
//!
//! 纯函数，没有任何状态：输入一段文本和若干秘密，输出抹掉秘密的文本。之所以要它，
//! 是因为错误信息经常是"把用户输入拼进去"的产物——而用户输入里就有码。
//!
//! > 这里只处理**文本**。错误类型的 `source` 链要保留，所以别把错误本身替换掉再
//! > 往上抛——那会丢掉 `ErrorKind`。渲染成字符串之后再脱敏。

use std::borrow::Cow;
use std::cmp::Reverse;

/// 替换串。与 Go 的 `redact.replacement` 一致——它会出现在用户看到的输出里。
pub const REDACTED: &str = "[REDACTED]";

/// 把 `text` 里出现的每个秘密替换成 [`REDACTED`]。
///
/// 规则与 Go 的 `redact.String` 一致：
///
/// - **空秘密被忽略**。否则每个字符之间都会被插进一个替换串。
/// - **重复秘密只处理一次**。
/// - **长秘密优先**。当一个秘密是另一个的子串时，先替换短的会把长的切碎，剩下的
///   碎片就漏出去了。
/// - 没命中任何秘密时返回借用，不做分配。
#[must_use]
pub fn redact<'a>(text: &'a str, secrets: &[&str]) -> Cow<'a, str> {
    let mut unique: Vec<&str> = Vec::with_capacity(secrets.len());
    for secret in secrets.iter().copied() {
        if secret.is_empty() || unique.contains(&secret) {
            continue;
        }
        unique.push(secret);
    }
    if unique.is_empty() {
        return Cow::Borrowed(text);
    }
    unique.sort_unstable_by_key(|secret| Reverse(secret.len()));

    let mut result = Cow::Borrowed(text);
    for secret in unique {
        if result.contains(secret) {
            result = Cow::Owned(result.replace(secret, REDACTED));
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_message_is_borrowed_not_copied() {
        let message = "connection refused";
        let redacted = redact(message, &["hunter2"]);
        assert!(matches!(redacted, Cow::Borrowed(_)), "没命中就不该分配");
        assert_eq!(redacted, message);
    }

    #[test]
    fn every_occurrence_is_replaced() {
        let redacted = redact("code hunter2 rejected hunter2", &["hunter2"]);
        assert_eq!(redacted, "code [REDACTED] rejected [REDACTED]");
    }

    #[test]
    fn empty_secrets_are_ignored() {
        let redacted = redact("abc", &[""]);
        assert_eq!(redacted, "abc");
        assert!(matches!(redacted, Cow::Borrowed(_)));
    }

    #[test]
    fn longer_secrets_win_over_their_own_prefixes() {
        // 如果先替换 "abc"，"abcdef" 就会剩下 "def" 漏出去。
        let redacted = redact("secret=abcdef", &["abc", "abcdef"]);
        assert_eq!(redacted, "secret=[REDACTED]");
        assert!(!redacted.contains("def"), "长秘密被切碎的碎片不该残留：{redacted}");
    }

    #[test]
    fn duplicate_secrets_are_only_applied_once() {
        let redacted = redact("hunter2", &["hunter2", "hunter2", "hunter2"]);
        assert_eq!(redacted, REDACTED);
    }

    #[test]
    fn no_secrets_means_no_work() {
        assert_eq!(redact("nothing to hide", &[]), "nothing to hide");
    }

    #[test]
    fn the_code_is_removed_from_an_error_message() {
        // 真实场景：错误串里嵌了用户给的码。
        let code = "academy-acoustic-accord";
        let message = format!("relay rejected room for {code}: bad password");
        let redacted = redact(&message, &[code, "bad password"]);
        assert!(!redacted.contains(code));
        assert!(!redacted.contains("bad password"));
        assert!(redacted.contains(REDACTED));
    }
}
