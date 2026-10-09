//! 领域新类型。
//!
//! 这些类型存在的理由是让错误不可写：房间名、PAKE 口令、中继口令都是 `String`，
//! 但它们绝不能互换。用 newtype 把"传错参数"变成编译错误，同时把"日志里不能出现
//! 明文"这条约束钉在 `Debug` 实现上——而不是靠每个调用点自觉。
//!
//! > 引擎层的 ID（`ConnId` / `FileId` / `TransferId`）等到 `croc-engine` 落地时再引入，
//! > 免得现在先摆一堆没人用的类型。

use std::borrow::Cow;
use std::fmt;
use zeroize::Zeroize as _;

/// 中继房间名：`hex(sha256(选择子 + "croc"))`，恒为 64 个小写十六进制字符。
///
/// # 为什么 `Debug` 也是脱敏的
///
/// 房间名由码的第一部分派生。日志里泄露出完整房间名，等于把码的熵从三个词降到
/// 两个词——对暴力猜测是实打实的帮助。所以日志里要关联房间时用
/// [`fingerprint`](Self::fingerprint)，不要用完整值。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoomName(String);

impl RoomName {
    /// 包一个已经算好的房间名。
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// 完整房间名。只有真的要和中继说话时才用它。
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// 取出内层字符串。
    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }

    /// 给日志用的短指纹。
    ///
    /// 房间名本身是哈希输出，取前 8 个十六进制字符足够在同一份日志里区分房间，
    /// 又不足以用来加入房间。
    #[must_use]
    pub fn fingerprint(&self) -> &str {
        self.0.get(..FINGERPRINT_LEN).unwrap_or(&self.0)
    }
}

/// [`RoomName::fingerprint`] 的长度。
const FINGERPRINT_LEN: usize = 8;

impl fmt::Debug for RoomName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "RoomName({})", self.fingerprint())
    }
}

impl fmt::Display for RoomName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.fingerprint())
    }
}

impl AsRef<str> for RoomName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl From<String> for RoomName {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// PAKE 口令。
///
/// # 为什么是字节而不是 `String`
///
/// 旧格式的切分是按字节做的（房间取 `secret[:4]`，口令取 `secret[5:]`），而用户
/// 给的码未必落在 UTF-8 边界上——`"abcdé-fgh"` 的第 5 个字节就在 `é` 中间。Go 那边
/// 就是裸字节切片，`[]byte(passphrase)` 拿到的正是那两个半截字节。这里用字节保持
/// 逐字节一致；需要展示时走 [`to_string_lossy`](Self::to_string_lossy)。
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(Box<[u8]>);

impl Secret {
    /// 从任意字节序列构造。
    #[must_use]
    pub fn from_bytes(value: impl Into<Box<[u8]>>) -> Self {
        Self(value.into())
    }

    /// 参与 PAKE 的原始字节。
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    /// 供展示/日志匹配用的有损字符串。
    ///
    /// 只在"要把秘密从一段文本里抹掉"这类场景使用：文本本身是 UTF-8，所以非 UTF-8
    /// 的秘密根本不可能出现在里面，有损转换不会漏掉命中。
    #[must_use]
    pub fn to_string_lossy(&self) -> Cow<'_, str> {
        String::from_utf8_lossy(&self.0)
    }

    /// 是否为空口令。
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// 口令长度（字节）。
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Secret([REDACTED])")
    }
}

impl fmt::Display for Secret {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("[REDACTED]")
    }
}

impl Drop for Secret {
    fn drop(&mut self) {
        // 口令在内存里停留的时间越短越好。`Secret` 会被 clone（PAKE 两边各持一份），
        // 所以每次析构都要擦。
        self.0.zeroize();
    }
}

impl From<&str> for Secret {
    fn from(value: &str) -> Self {
        Self::from_bytes(value.as_bytes())
    }
}

impl From<String> for Secret {
    fn from(value: String) -> Self {
        Self::from_bytes(value.into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_name_never_leaks_the_full_value() {
        let room = RoomName::new("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");
        let shown = format!("{room} {room:?}");
        assert!(shown.contains("01234567"), "应能看到短指纹：{shown}");
        assert!(!shown.contains("89abcdef"), "不应泄露完整房间名：{shown}");
    }

    #[test]
    fn room_name_fingerprint_tolerates_short_values() {
        assert_eq!(RoomName::new("abc").fingerprint(), "abc");
    }

    #[test]
    fn secret_never_leaks_in_debug_or_display() {
        let secret = Secret::from("correct horse battery staple");
        let shown = format!("{secret} {secret:?}");
        assert!(!shown.contains("correct"), "口令不该出现在任何展示里：{shown}");
        assert_eq!(secret.as_bytes(), b"correct horse battery staple");
        assert_eq!(secret.len(), 28);
        assert!(!secret.is_empty());
    }

    #[test]
    fn secret_round_trips_non_utf8_bytes() {
        let raw = vec![0x61, 0xc3, 0x28, 0xff];
        let secret = Secret::from_bytes(raw.clone());
        assert_eq!(secret.as_bytes(), raw.as_slice());
        // 有损展示只是给人看的，原始字节一个都没变。
        assert!(secret.to_string_lossy().contains('\u{fffd}'));
    }
}
