//! 局域网探测（local probe）：正式 PAKE 之前的那一次"你在不在本地"的交换。
//!
//! 这一步是 Go 版 `croc.go` 的 `SimpleMessage` 协议。它不是 `Message`：
//!
//! - 帧体是裸 JSON（不 DEFLATE、不进消息层）；
//! - 前两帧（`pake1` / `pake2`）不加密；
//! - 之后两帧（`ips?` 与 IP 列表）用探测自己派生出的密钥加密。
//!
//! ```text
//! 接收方                                            发送方
//! ─────────────────────────────────────────────────────────────────────────
//! ──{"Bytes":A,"Kind":"pake1","Version":2,"Curve":"p256"}──▶   （裸 JSON）
//!                            B = pakekey(pass, 1, PurposeLocalProbe, room)
//!                            B.Update(A)；salt ←$ 32B
//! ◀──{"Bytes":B,"Bytes2":salt,"Kind":"pake2",…}──
//! 双方 Derive ⇒ kLocal（用途是 local-ip-probe，不是 peer-transfer）
//! ──Enc("ips?")──▶
//! ◀──Enc(json([端口, ip, ip, …]))──
//! ──"handshake"──▶                              （裸帧，回到正常流程）
//! ```
//!
//! # 为什么这一层必须实现
//!
//! Go 的接收方在连上中继之后默认先做这一步（除非 `--no-local` 或给了 `--ip`）。
//! 不实现它，Go 的接收方会卡在这里等 `pake2`，而 Rust 的发送方会把这段 JSON 当成
//! `Message` 去 DEFLATE 解压，报 `corrupt deflate stream` —— 两个方向都断在第一帧上。
//!
//! # 本实现只做应答方
//!
//! 我们是发送方时必须应答（否则 Go 的接收方连不上）；我们是接收方时主动跳过
//! （等价于 Go 的 `--no-local`）——本项目还没有本地中继，探到了也没得连。
//! 跳过是协议允许的：`--no-local` 的 Go 客户端就是这么做的。

use serde::{Deserialize, Serialize};

use crate::ProtocolError;
use crate::json::base64_bytes;

/// 接收方问"你的本地地址是什么"。
pub const IP_REQUEST: &[u8] = b"ips?";

/// 发起方的第一帧。
pub const KIND_PAKE1: &str = "pake1";

/// 应答方的第二帧。
pub const KIND_PAKE2: &str = "pake2";

/// Go 的 `croc.SimpleMessage`。
///
/// 字段名是 Go 的结构体字段名（没有 json tag），而且 Go 的 `json.Marshal` 没有
/// `omitempty`——五个键永远都出现（空切片编成 `null`）。这里不去复刻 `null`：
/// 对方用 `json.Unmarshal` 读，缺键与 `null` 都得到零值，互通无碍。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeMessage {
    /// SPAKE2 的公开值。
    #[serde(rename = "Bytes", default, with = "base64_bytes")]
    pub bytes: Vec<u8>,

    /// 盐（只有 `pake2` 有）。
    #[serde(rename = "Bytes2", default, with = "base64_bytes")]
    pub bytes2: Vec<u8>,

    /// `"pake1"` / `"pake2"`。
    #[serde(rename = "Kind", default)]
    pub kind: String,

    /// PAKE 协议版本。
    #[serde(rename = "Version", default)]
    pub version: i64,

    /// 曲线名。
    #[serde(rename = "Curve", default)]
    pub curve: String,
}

impl ProbeMessage {
    /// `pake1`：发起方的公开值。
    #[must_use]
    pub fn pake1(bytes: Vec<u8>, version: i64, curve: &str) -> Self {
        Self { bytes, kind: KIND_PAKE1.to_owned(), version, curve: curve.to_owned(), ..Self::default() }
    }

    /// `pake2`：应答方的公开值 + 盐。
    #[must_use]
    pub fn pake2(bytes: Vec<u8>, salt: Vec<u8>, version: i64, curve: &str) -> Self {
        Self { bytes, bytes2: salt, kind: KIND_PAKE2.to_owned(), version, curve: curve.to_owned() }
    }

    /// 编成帧体。
    ///
    /// # Errors
    ///
    /// 序列化失败时返回 [`ProtocolError::MalformedJson`]（实际不会发生）。
    pub fn to_bytes(&self) -> Result<Vec<u8>, ProtocolError> {
        serde_json::to_vec(self).map_err(|error| ProtocolError::MalformedJson(error.to_string()))
    }

    /// 从帧体解出。
    ///
    /// # Errors
    ///
    /// 不是合法 JSON 时返回 [`ProtocolError::MalformedJson`]。
    pub fn from_bytes(payload: &[u8]) -> Result<Self, ProtocolError> {
        serde_json::from_slice(payload).map_err(|error| ProtocolError::MalformedJson(error.to_string()))
    }

    /// 这段字节像不像一条探测消息。
    ///
    /// 用来把探测帧与 `Message` 帧分开：`Message` 是 DEFLATE 过的，第一个字节几乎
    /// 不可能是 `{`；而探测帧是裸 JSON，必然以 `{` 开头。这个判断只需要"够准"，
    /// 因为解析失败时调用方会落回正常路径。
    #[must_use]
    pub fn looks_like_probe(payload: &[u8]) -> bool {
        payload.first() == Some(&b'{')
    }
}

/// 把 IP 列表编成 `ips?` 的应答体（`[端口, ip, ip, …]`）。
///
/// 本实现永远传空列表：我们没有本地中继，探到了也没得连。Go 的 `--no-local` 发送方
/// 行为相同（`ips` 保持 nil），接收方看到 `len(ips) <= 1` 就直接回落到中继路径。
///
/// # Errors
///
/// 序列化失败时返回 [`ProtocolError::MalformedJson`]。
pub fn encode_endpoints(port: u16, ips: &[String]) -> Result<Vec<u8>, ProtocolError> {
    let mut list: Vec<String> = Vec::with_capacity(ips.len() + 1);
    if !ips.is_empty() {
        list.push(port.to_string());
        list.extend(ips.iter().cloned());
    }
    serde_json::to_vec(&list).map_err(|error| ProtocolError::MalformedJson(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_match_the_go_struct() {
        // Go 的 `SimpleMessage` 没有 json tag，所以键名就是字段名，大小写敏感。
        let message = ProbeMessage::pake2(b"resp".to_vec(), vec![7u8; 32], 2, "p256");
        let json = String::from_utf8(message.to_bytes().expect("编码")).expect("UTF-8");
        assert!(json.contains("\"Bytes\":"), "{json}");
        assert!(json.contains("\"Bytes2\":"), "{json}");
        assert!(json.contains("\"Kind\":\"pake2\""), "{json}");
        assert!(json.contains("\"Version\":2"), "{json}");
        assert!(json.contains("\"Curve\":\"p256\""), "{json}");
        // `Bytes` 是 Go 的 `[]byte`，在 JSON 里是标准 base64 带 padding。
        assert!(json.contains("\"Bytes\":\"cmVzcA==\""), "base64 带 padding：{json}");
    }

    #[test]
    fn a_probe_round_trips() {
        let original = ProbeMessage::pake1(vec![1, 2, 3], 2, "siec");
        let decoded = ProbeMessage::from_bytes(&original.to_bytes().expect("编码")).expect("解码");
        assert_eq!(decoded, original);
    }

    #[test]
    fn probe_frames_are_recognised_by_their_first_byte() {
        // `Message` 帧是 DEFLATE 过的，首字节几乎不可能是 `{`。
        assert!(ProbeMessage::looks_like_probe(b"{\"Kind\":\"pake1\"}"));
        assert!(!ProbeMessage::looks_like_probe(b""));
        assert!(!ProbeMessage::looks_like_probe(&[0x01]));
        assert!(!ProbeMessage::looks_like_probe(b"handshake"));
    }

    #[test]
    fn malformed_probe_json_is_reported() {
        assert!(ProbeMessage::from_bytes(b"{not json").is_err());
        assert!(ProbeMessage::from_bytes(b"").is_err());
    }

    #[test]
    fn an_empty_endpoint_list_serialises_as_an_empty_array() {
        // 接收方看到 `len(ips) <= 1` 就会回落到中继路径——这正是我们要的。
        assert_eq!(encode_endpoints(9010, &[]).expect("编码"), b"[]");
        let with_ips = encode_endpoints(9010, &["192.168.1.5".to_owned()]).expect("编码");
        assert_eq!(String::from_utf8(with_ips).expect("UTF-8"), "[\"9010\",\"192.168.1.5\"]");
    }
}
