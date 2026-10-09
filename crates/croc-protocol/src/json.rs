//! 把 Go `encoding/json` 的几处约定搬过来。
//!
//! 这些都不是实现细节，而是线路契约的一部分：同一个字段在 Go 里编出来是什么样，
//! 这里就得是什么样，否则对端解不出来。
//!
//! | Go 侧 | 线路形态 | 本模块 |
//! |---|---|---|
//! | `[]byte` | 标准 base64（带 padding）；`null` 解成 nil 切片 | [`base64_bytes`] |
//! | `time.Time`（`json:"m,omitzero"`） | RFC3339Nano；零值整字段消失 | [`ModTime`] + [`mod_time`] |
//! | `os.FileMode` | 裸 `uint32`（`0` 被 `omitempty` 吃掉） | 直接用 `u32` + `skip_serializing_if` |

use base64::engine::general_purpose::STANDARD as BASE64;
use serde::{Deserialize, Deserializer, Serializer};
use time::OffsetDateTime;

/// Go 的 `[]byte` 字段。
pub(crate) mod base64_bytes {
    use super::{BASE64, Deserialize, Deserializer, Serializer};
    use base64::Engine as _;

    /// 编码成标准 base64（带 padding）。
    pub(crate) fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&BASE64.encode(bytes))
    }

    /// 从 base64 解码。`null` 落成空向量——Go 的 `[]byte` 收到 `null` 会解成 nil 切片。
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
        Option::<String>::deserialize(deserializer)?
            .map_or_else(|| Ok(Vec::new()), |text| BASE64.decode(text.as_bytes()).map_err(serde::de::Error::custom))
    }
}

/// Go 的 `time.Time`，语义对齐 `json:"m,omitzero"`。
///
/// `None` 表示 Go 的零值时间（`0001-01-01T00:00:00Z`），此时整个字段不出现。
///
/// # 为什么不能直接存 `OffsetDateTime`
///
/// Go 的零值时间不是 Unix epoch，而是公元 1 年 1 月 1 日——`OffsetDateTime` 的默认值
/// 表达不了它。所以这里用 `Option` 把"零值"这件事显式化。
///
/// # 时区偏移
///
/// Go 的 `time.Time.MarshalJSON` 会带上时间点所在的时区偏移（`+08:00`），这里同样保留
/// `OffsetDateTime` 自带的偏移。两边的时间点相同，而 Go 的 `time.Equal` 比的是时间点
/// 而不是表示形式，所以互通无碍。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct ModTime(Option<OffsetDateTime>);

impl ModTime {
    /// Go 的零值时间：字段会被省略。
    pub const ZERO: Self = Self(None);

    /// 包一个时间点。传入 Go 的零值时间等价于 [`ZERO`](Self::ZERO)。
    #[must_use]
    pub fn new(value: OffsetDateTime) -> Self {
        Self((!is_go_zero(value)).then_some(value))
    }

    /// 时间点。零值时返回 `None`。
    #[must_use]
    pub fn get(self) -> Option<OffsetDateTime> {
        self.0
    }

    /// 是否是 Go 的零值时间。
    #[must_use]
    pub fn is_zero(&self) -> bool {
        self.0.is_none()
    }
}

impl From<OffsetDateTime> for ModTime {
    fn from(value: OffsetDateTime) -> Self {
        Self::new(value)
    }
}

/// Go 的 `time.Time` 零值：公元 1 年 1 月 1 日 00:00:00 UTC。
fn is_go_zero(value: OffsetDateTime) -> bool {
    let date = value.date();
    date.year() == 1 && date.month() == time::Month::January && date.day() == 1 && value.time() == time::Time::MIDNIGHT
}

/// [`ModTime`] 的 serde 适配器，用法是 `#[serde(with = "mod_time")]`。
pub(crate) mod mod_time {
    use super::{Deserializer, ModTime, Serializer};
    use time::serde::rfc3339;

    /// 按 RFC3339Nano 编码。
    pub(crate) fn serialize<S: Serializer>(value: &ModTime, serializer: S) -> Result<S::Ok, S::Error> {
        match value.get() {
            Some(instant) => rfc3339::serialize(&instant, serializer),
            // 走不到：带 `with` 的字段同时也有 `skip_serializing_if`，零值根本不会被编。
            // 但为了不给生产代码留 panic 点，这里退回 `null`（Go 解 `null` 会报错，
            // 所以真走到这一步是 bug，只是不会把进程带走）。
            None => serializer.serialize_none(),
        }
    }

    /// 按 RFC3339Nano 解码。`null` 与 Go 的零值时间都落成 [`ModTime::ZERO`]。
    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ModTime, D::Error> {
        // 必须走 `rfc3339::option`：直接解 `Option<OffsetDateTime>` 会用到 `time` 的
        // 默认 serde 表示（结构体），而不是 RFC3339 字符串。
        let instant = rfc3339::option::deserialize(deserializer)?;
        Ok(instant.map_or(ModTime::ZERO, ModTime::new))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Bytes {
        #[serde(with = "base64_bytes", default, skip_serializing_if = "Vec::is_empty")]
        b: Vec<u8>,
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Time {
        #[serde(with = "mod_time", default, skip_serializing_if = "ModTime::is_zero")]
        m: ModTime,
    }

    #[test]
    fn bytes_use_padded_standard_base64() {
        let json = serde_json::to_string(&Bytes { b: vec![0xfb, 0xff, 0x00] }).expect("应能编码");
        // Go: base64.StdEncoding.EncodeToString([]byte{0xfb, 0xff, 0x00}) == "+/8A"
        assert_eq!(json, r#"{"b":"+/8A"}"#);
    }

    #[test]
    fn empty_bytes_disappear() {
        assert_eq!(serde_json::to_string(&Bytes { b: Vec::new() }).expect("应能编码"), "{}");
    }

    #[test]
    fn null_bytes_decode_to_empty() {
        let decoded: Bytes = serde_json::from_str(r#"{"b":null}"#).expect("应能解码");
        assert_eq!(decoded.b, Vec::<u8>::new());
    }

    #[test]
    fn a_zero_mod_time_disappears() {
        assert_eq!(serde_json::to_string(&Time { m: ModTime::ZERO }).expect("应能编码"), "{}");
        // Go 的零值时间字面量解出来就是零值。
        let decoded: Time = serde_json::from_str(r#"{"m":"0001-01-01T00:00:00Z"}"#).expect("应能解码");
        assert_eq!(decoded.m, ModTime::ZERO);
    }

    #[test]
    fn mod_time_round_trips_and_keeps_the_offset() {
        let original: Time = serde_json::from_str(r#"{"m":"2026-10-06T20:31:00.123456789+08:00"}"#).expect("应能解码");
        let instant = original.m.get().expect("不是零值");
        // 时间点保持不变：本地时间 20:31+08:00 就是 UTC 12:31。
        assert_eq!(instant.offset().whole_hours(), 8);
        assert_eq!(instant.to_offset(time::UtcOffset::UTC).hour(), 12);
        // Go 会保留原时区偏移，这里也一样。
        let json = serde_json::to_string(&original).expect("应能编码");
        assert_eq!(json, r#"{"m":"2026-10-06T20:31:00.123456789+08:00"}"#);
        // 再解一遍得到同一个时间点。
        let again: Time = serde_json::from_str(&json).expect("应能解码");
        assert_eq!(again.m.get().expect("不是零值"), instant);
    }

    #[test]
    fn mod_time_parses_go_style_whole_seconds() {
        let decoded: Time = serde_json::from_str(r#"{"m":"2024-02-29T00:00:00Z"}"#).expect("应能解码");
        let instant = decoded.m.get().expect("不是零值");
        assert_eq!(instant.year(), 2024);
        assert_eq!(instant.unix_timestamp(), 1_709_164_800);
    }
}
