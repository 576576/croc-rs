//! `message.Message`：帧体里那层 JSON，以及它的编解码流水线。
//!
//! 对应 Go 的 `src/message`。线路上的三步是固定的：
//!
//! ```text
//! 发送：Message ──serde_json──▶ JSON ──DEFLATE──▶ [──AES-256-GCM──▶] 帧体
//! 接收：帧体 [──AES-256-GCM──▶] ──INFLATE──▶ JSON ──serde_json──▶ Message
//! ```
//!
//! # `omitempty` 是协议的一部分
//!
//! Go 的每个字段都带 `omitempty`，所以空字段根本不出现在 JSON 里。这里用
//! `skip_serializing_if` 逐个复刻，字段声明顺序也照抄 `t, v, m, b, b2, n, f`——
//! serde_json 按声明顺序输出，于是同一份 `Message` 在两边编出的 JSON 字节相同
//! （唯一的例外见下）。顺序不影响解码，但能让 golden 向量做字节级比较。
//!
//! # 与 Go 的一处字节差异（不影响互通）
//!
//! Go 的 `encoding/json` 默认会把 `<`、`>`、`&` 转义成 `\u003c`、`\u003e`、`\u0026`，
//! 并把 U+2028/U+2029 转义；serde_json 不转义。解码端不关心转义形式，所以两边
//! 都能读对方的产品。只有拿原始字节做断言的 golden 测试要注意：比较解码后的
//! `Message`，别比较 JSON 字符串。
//!
//! # 明文只允许两条消息
//!
//! Go 在收包时硬性拒绝明文状态下收到的非 PAKE 消息（`"unencrypted communication
//! rejected"`）。这个不变量在 Rust 侧被 [`SessionKey`] 类型固化：`Plain` 状态既不肯
//! 签出非 PAKE 消息，也拒收非 PAKE 消息。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ProtocolError;
use crate::codec::{self, AEAD_NONCE_LEN, ENCRYPTION_KEY_LEN, MAX_DECOMPRESSED_SIZE};

/// JSON 明文的上限，与 Go 的 `message.maxDecompressedMessageSize` 同源。
///
/// Go 在编码和解码两侧都拿它兜底：编码时防止发出一个解压后撑爆对端内存的消息，解码时
/// 防压缩炸弹。
pub const MAX_JSON_SIZE: usize = MAX_DECOMPRESSED_SIZE;

/// 消息类型。
///
/// Go 里是 `type Type string`——未知类型必须能原样透传。新版对端可能发来本端还不
/// 认识的消息，而 Go 的 `switch` 没有 `default` 分支，会安静忽略。这里把这件事显式化：
/// 认识的走枚举分支，不认识的落进 [`MessageType::Unknown`] 并保留原文，方便日志和
/// 将来排查"为什么这条消息没被处理"。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum MessageType {
    /// PAKE 握手第一步（发起方）。
    Pake,
    /// PAKE 握手确认。
    PakeConfirm,
    /// 提议切换到 tailcat 数据通道。
    TailcatOffer,
    /// tailcat 通道状态回报。
    TailcatStatus,
    /// 选定数据通道。
    TransportSelect,
    /// 对端观测到的本端外网 IP。
    ExternalIp,
    /// 传输完成。
    Finished,
    /// 对端报错，收到即中止。
    Error,
    /// 接收方关闭。
    CloseRecipient,
    /// 发送方关闭。
    CloseSender,
    /// 接收方就绪，可以开传。
    RecipientReady,
    /// 文件清单（单条，旧格式）。
    FileInfo,
    /// 分块文件清单的开头。
    FileInfoStart,
    /// 分块文件清单的一批。
    FileInfoBatch,
    /// 分块文件清单的结尾。
    FileInfoEnd,
    /// 某个文件已准备就绪。
    FilePrepared,
    /// 请求精确哈希。
    ExactHashRequest,
    /// 精确哈希结果。
    ExactHashResult,
    /// 中转通道待命。
    RelayStandby,
    /// 中转通道放量。
    RelayRamp,
    /// SSH 授权请求。
    SshAuthorize,
    /// SSH 会话邀请。
    SshOffer,
    /// 本端不认识的消息类型，原文保留。
    Unknown(String),
}

impl MessageType {
    /// 全部已知类型，顺序与 Go 的常量声明一致。
    ///
    /// 不含 [`MessageType::Unknown`]：它不是"一种"类型，而是兜底。
    pub const ALL: [Self; 22] = [
        Self::Pake,
        Self::PakeConfirm,
        Self::TailcatOffer,
        Self::TailcatStatus,
        Self::TransportSelect,
        Self::ExternalIp,
        Self::Finished,
        Self::Error,
        Self::CloseRecipient,
        Self::CloseSender,
        Self::RecipientReady,
        Self::FileInfo,
        Self::FileInfoStart,
        Self::FileInfoBatch,
        Self::FileInfoEnd,
        Self::FilePrepared,
        Self::ExactHashRequest,
        Self::ExactHashResult,
        Self::RelayStandby,
        Self::RelayRamp,
        Self::SshAuthorize,
        Self::SshOffer,
    ];

    /// 线上的类型名，必须与 Go 的常量逐字相同。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Pake => "pake",
            Self::PakeConfirm => "pake-confirm",
            Self::TailcatOffer => "tailcat-offer",
            Self::TailcatStatus => "tailcat-status",
            Self::TransportSelect => "transport-select",
            Self::ExternalIp => "externalip",
            Self::Finished => "finished",
            Self::Error => "error",
            Self::CloseRecipient => "close-recipient",
            Self::CloseSender => "close-sender",
            Self::RecipientReady => "recipientready",
            Self::FileInfo => "fileinfo",
            Self::FileInfoStart => "fileinfo-start",
            Self::FileInfoBatch => "fileinfo-batch",
            Self::FileInfoEnd => "fileinfo-end",
            Self::FilePrepared => "file-prepared",
            Self::ExactHashRequest => "exact-hash-request",
            Self::ExactHashResult => "exact-hash-result",
            Self::RelayStandby => "relay-standby",
            Self::RelayRamp => "relay-ramp",
            Self::SshAuthorize => "ssh-authorize",
            Self::SshOffer => "ssh-offer",
            Self::Unknown(raw) => raw,
        }
    }

    /// 只有 PAKE 握手的两条消息允许在明文通道上传输。
    ///
    /// 这是 Go 收包侧的硬性检查（`croc.go` 里的 `"unencrypted communication
    /// rejected"`）。类型缺失时（`Type("")`）Go 同样拒绝，所以这里返回 `false`。
    #[must_use]
    pub fn may_be_unencrypted(&self) -> bool {
        matches!(self, Self::Pake | Self::PakeConfirm)
    }
}

impl From<&str> for MessageType {
    fn from(raw: &str) -> Self {
        Self::ALL.iter().find(|known| known.as_str() == raw).cloned().unwrap_or_else(|| Self::Unknown(raw.to_owned()))
    }
}

impl From<String> for MessageType {
    fn from(raw: String) -> Self {
        Self::from(raw.as_str())
    }
}

impl std::fmt::Display for MessageType {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for MessageType {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for MessageType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// 一条消息。字段与 Go 的 `message.Message` 一一对应。
///
/// 全部字段都带 `omitempty` 语义，所以 `Message::default()` 编出来就是 `{}`。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    /// `t`：消息类型。缺失表示 Go 的 `Type("")`。
    #[serde(rename = "t", default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<MessageType>,

    /// `v`：协议版本。`omitempty` ⇒ 0 不出现。
    #[serde(rename = "v", default, skip_serializing_if = "is_zero")]
    pub version: i64,

    /// `m`：文本载荷（错误信息、文件名、外网 IP 等）。
    #[serde(rename = "m", default, skip_serializing_if = "String::is_empty")]
    pub message: String,

    /// `b`：二进制载荷，JSON 里是标准 base64（带 padding）。
    #[serde(rename = "b", default, with = "crate::json::base64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub bytes: Vec<u8>,

    /// `b2`：第二段二进制载荷，用途同 `b`。
    #[serde(rename = "b2", default, with = "crate::json::base64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub bytes2: Vec<u8>,

    /// `n`：数值载荷（文件序号、分块计数等）。
    #[serde(rename = "n", default, skip_serializing_if = "is_zero")]
    pub num: i64,

    /// `f`：能力列表。
    #[serde(rename = "f", default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl Message {
    /// 只带类型的最小消息，等价于 Go 的 `message.Message{Type: t}`。
    #[must_use]
    pub fn new(kind: MessageType) -> Self {
        Self { kind: Some(kind), ..Self::default() }
    }

    /// 这条消息是否允许走明文通道。
    #[must_use]
    pub fn may_be_unencrypted(&self) -> bool {
        self.kind.as_ref().is_some_and(MessageType::may_be_unencrypted)
    }

    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    #[must_use]
    pub fn with_bytes(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.bytes = bytes.into();
        self
    }

    #[must_use]
    pub fn with_bytes2(mut self, bytes: impl Into<Vec<u8>>) -> Self {
        self.bytes2 = bytes.into();
        self
    }

    #[must_use]
    pub fn with_num(mut self, num: i64) -> Self {
        self.num = num;
        self
    }

    #[must_use]
    pub fn with_version(mut self, version: i64) -> Self {
        self.version = version;
        self
    }

    #[must_use]
    pub fn with_features(mut self, features: Vec<String>) -> Self {
        self.features = features;
        self
    }
}

/// 会话的加密状态。
///
/// PAKE 完成后从 [`SessionKey::Plain`] 单向切到 [`SessionKey::Aead`]。把"是否已加密"
/// 做成一个值而不是 `Option<[u8; 32]>` 散落在各处，是为了让"忘了加密"和"收明文"这两类
/// 错误在类型上就写不出来。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionKey {
    /// PAKE 阶段：明文。只允许 `pake` / `pake-confirm`。
    Plain,
    /// 握手完成后：AES-256-GCM。
    Aead([u8; ENCRYPTION_KEY_LEN]),
}

impl SessionKey {
    /// 是否已经建立会话密钥。
    #[must_use]
    pub const fn is_encrypted(&self) -> bool {
        matches!(self, Self::Aead(_))
    }

    /// 打包一条消息。
    ///
    /// 明文状态下只有 PAKE 消息能通过——否则返回
    /// [`ProtocolError::PlaintextRejected`]。`nonce` 在明文状态下被忽略。
    pub fn seal(&self, message: &Message, nonce: &[u8; AEAD_NONCE_LEN]) -> Result<Vec<u8>, ProtocolError> {
        match self {
            Self::Plain if !message.may_be_unencrypted() => {
                Err(ProtocolError::PlaintextRejected { kind: kind_name(message) })
            }
            Self::Plain => encode_plain(message),
            Self::Aead(key) => encode_encrypted(message, key, nonce),
        }
    }

    /// 拆开一条消息。
    ///
    /// 明文状态下收到非 PAKE 消息会被拒绝，与 Go 的
    /// `"unencrypted communication rejected"` 对齐。
    pub fn open(&self, payload: &[u8]) -> Result<Message, ProtocolError> {
        match self {
            Self::Plain => {
                let message = decode_plain(payload)?;
                if message.may_be_unencrypted() {
                    Ok(message)
                } else {
                    Err(ProtocolError::PlaintextRejected { kind: kind_name(&message) })
                }
            }
            Self::Aead(key) => decode_encrypted(payload, key),
        }
    }
}

/// 明文阶段的线路负载：`JSON → DEFLATE`。
pub fn encode_plain(message: &Message) -> Result<Vec<u8>, ProtocolError> {
    encode_plain_with_limit(message, MAX_JSON_SIZE)
}

/// 明文阶段的线路负载，带自定义 JSON 上限。
pub fn encode_plain_with_limit(message: &Message, limit: usize) -> Result<Vec<u8>, ProtocolError> {
    codec::compress(&to_json(message, limit)?)
}

/// 加密阶段的线路负载：`JSON → DEFLATE → AEAD`。
pub fn encode_encrypted(message: &Message, key: &[u8], nonce: &[u8; AEAD_NONCE_LEN]) -> Result<Vec<u8>, ProtocolError> {
    encode_encrypted_with_limit(message, key, nonce, MAX_JSON_SIZE)
}

/// 加密阶段的线路负载，带自定义 JSON 上限。
pub fn encode_encrypted_with_limit(
    message: &Message,
    key: &[u8],
    nonce: &[u8; AEAD_NONCE_LEN],
    limit: usize,
) -> Result<Vec<u8>, ProtocolError> {
    codec::encrypt(&codec::compress(&to_json(message, limit)?)?, key, nonce)
}

/// 解出明文阶段的线路负载：`INFLATE → JSON`。
pub fn decode_plain(payload: &[u8]) -> Result<Message, ProtocolError> {
    decode_plain_with_limit(payload, MAX_JSON_SIZE)
}

/// 解出明文阶段的线路负载，带自定义 JSON 上限。
pub fn decode_plain_with_limit(payload: &[u8], limit: usize) -> Result<Message, ProtocolError> {
    from_json(&codec::decompress(payload, limit)?)
}

/// 解出加密阶段的线路负载：`AEAD → INFLATE → JSON`。
pub fn decode_encrypted(payload: &[u8], key: &[u8]) -> Result<Message, ProtocolError> {
    decode_encrypted_with_limit(payload, key, MAX_JSON_SIZE)
}

/// 解出加密阶段的线路负载，带自定义 JSON 上限。
pub fn decode_encrypted_with_limit(payload: &[u8], key: &[u8], limit: usize) -> Result<Message, ProtocolError> {
    from_json(&codec::decompress(&codec::decrypt(payload, key)?, limit)?)
}

fn to_json(message: &Message, limit: usize) -> Result<Vec<u8>, ProtocolError> {
    let json = serde_json::to_vec(message).map_err(|error| ProtocolError::MalformedJson(error.to_string()))?;
    if json.len() > limit {
        return Err(ProtocolError::MessageTooLarge { actual: json.len(), limit });
    }
    Ok(json)
}

fn from_json(json: &[u8]) -> Result<Message, ProtocolError> {
    serde_json::from_slice(json).map_err(|error| ProtocolError::MalformedJson(error.to_string()))
}

fn kind_name(message: &Message) -> String {
    message.kind.as_ref().map_or_else(String::new, MessageType::to_string)
}

fn is_zero(value: &i64) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; ENCRYPTION_KEY_LEN] = [0x5a; ENCRYPTION_KEY_LEN];
    const NONCE: [u8; AEAD_NONCE_LEN] = [0x11; AEAD_NONCE_LEN];

    fn json_of(message: &Message) -> String {
        String::from_utf8(to_json(message, MAX_JSON_SIZE).expect("应能编码")).expect("JSON 应是 UTF-8")
    }

    #[test]
    fn wire_names_match_the_go_constants() {
        let expected = [
            ("pake", MessageType::Pake),
            ("pake-confirm", MessageType::PakeConfirm),
            ("tailcat-offer", MessageType::TailcatOffer),
            ("tailcat-status", MessageType::TailcatStatus),
            ("transport-select", MessageType::TransportSelect),
            ("externalip", MessageType::ExternalIp),
            ("finished", MessageType::Finished),
            ("error", MessageType::Error),
            ("close-recipient", MessageType::CloseRecipient),
            ("close-sender", MessageType::CloseSender),
            ("recipientready", MessageType::RecipientReady),
            ("fileinfo", MessageType::FileInfo),
            ("fileinfo-start", MessageType::FileInfoStart),
            ("fileinfo-batch", MessageType::FileInfoBatch),
            ("fileinfo-end", MessageType::FileInfoEnd),
            ("file-prepared", MessageType::FilePrepared),
            ("exact-hash-request", MessageType::ExactHashRequest),
            ("exact-hash-result", MessageType::ExactHashResult),
            ("relay-standby", MessageType::RelayStandby),
            ("relay-ramp", MessageType::RelayRamp),
            ("ssh-authorize", MessageType::SshAuthorize),
            ("ssh-offer", MessageType::SshOffer),
        ];
        for (name, kind) in &expected {
            assert_eq!(kind.as_str(), *name);
            assert_eq!(MessageType::from(*name), *kind);
            assert_eq!(kind.to_string(), *name);
        }
        assert_eq!(MessageType::ALL.len(), expected.len());
    }

    #[test]
    fn unknown_types_survive_a_round_trip() {
        let raw = "a-message-from-the-future";
        let kind = MessageType::from(raw);
        assert_eq!(kind, MessageType::Unknown(raw.to_owned()));
        assert_eq!(kind.as_str(), raw);

        let message = Message::new(kind);
        assert_eq!(json_of(&message), format!("{{\"t\":\"{raw}\"}}"));
        assert_eq!(from_json(json_of(&message).as_bytes()).expect("应能解码"), message);
    }

    #[test]
    fn empty_message_serialises_to_an_empty_object() {
        assert_eq!(json_of(&Message::default()), "{}");
        assert_eq!(from_json(b"{}").expect("应能解码"), Message::default());
    }

    #[test]
    fn field_order_follows_the_go_struct() {
        let message = Message::new(MessageType::Pake)
            .with_version(2)
            .with_message("hi")
            .with_bytes(b"one".to_vec())
            .with_bytes2(b"two".to_vec())
            .with_num(7)
            .with_features(vec!["staged-transport-v1".to_owned()]);
        assert_eq!(
            json_of(&message),
            concat!(r#"{"t":"pake","v":2,"m":"hi","b":"b25l","b2":"dHdv","n":7,"#, r#""f":["staged-transport-v1"]}"#,)
        );
    }

    #[test]
    fn omitempty_hides_every_empty_field() {
        // 逐个字段验证：只有非零值才出现。
        let only_version = Message { version: 11, ..Message::default() };
        assert_eq!(json_of(&only_version), r#"{"v":11}"#);

        let only_num = Message { num: 3, ..Message::default() };
        assert_eq!(json_of(&only_num), r#"{"n":3}"#);

        let only_message = Message::default().with_message("x");
        assert_eq!(json_of(&only_message), r#"{"m":"x"}"#);

        let only_features = Message::default().with_features(vec!["a".to_owned()]);
        assert_eq!(json_of(&only_features), r#"{"f":["a"]}"#);

        // 空切片 / 空字符串 / 0 都要消失。
        let empty = Message {
            kind: Some(MessageType::Finished),
            version: 0,
            message: String::new(),
            bytes: Vec::new(),
            bytes2: Vec::new(),
            num: 0,
            features: Vec::new(),
        };
        assert_eq!(json_of(&empty), r#"{"t":"finished"}"#);
    }

    #[test]
    fn bytes_use_standard_base64_with_padding() {
        // "a" 编码成 "YQ=="（带 padding），而不是 base64url 的 "YQ"。
        let message = Message::default().with_bytes(b"a".to_vec());
        assert_eq!(json_of(&message), r#"{"b":"YQ=="}"#);

        // 0xfb 0xff 在标准字母表里是 "+/8="，url 安全字母表会是 "-_8="。
        let message = Message::default().with_bytes(vec![0xfb, 0xff]);
        assert_eq!(json_of(&message), r#"{"b":"+/8="}"#);
    }

    #[test]
    fn null_bytes_decode_to_empty() {
        let decoded = from_json(br#"{"t":"finished","b":null,"b2":null}"#).expect("应能解码");
        assert!(decoded.bytes.is_empty());
        assert!(decoded.bytes2.is_empty());
        assert_eq!(decoded.kind, Some(MessageType::Finished));
    }

    #[test]
    fn malformed_json_is_reported_as_such() {
        assert!(matches!(from_json(b"{not json"), Err(ProtocolError::MalformedJson(_))));
        assert!(matches!(from_json(b""), Err(ProtocolError::MalformedJson(_))));
        // 类型不对：`n` 必须是数字。
        assert!(matches!(from_json(br#"{"n":"seven"}"#), Err(ProtocolError::MalformedJson(_))));
        // base64 坏了。
        assert!(matches!(from_json(br#"{"b":"!!!"}"#), Err(ProtocolError::MalformedJson(_))));
    }

    #[test]
    fn oversized_json_is_rejected_before_compression() {
        let message = Message::default().with_message("x".repeat(128));
        assert!(matches!(encode_plain_with_limit(&message, 64), Err(ProtocolError::MessageTooLarge { limit: 64, .. })));
    }

    #[test]
    fn plaintext_round_trips() {
        let message = Message::new(MessageType::Pake)
            .with_version(2)
            .with_bytes(b"initiator".to_vec())
            .with_bytes2(b"salt".to_vec())
            .with_features(vec!["inline-peer-metadata-v1".to_owned()]);

        let payload = encode_plain(&message).expect("应能编码");
        assert_ne!(payload, to_json(&message, MAX_JSON_SIZE).expect("应能编码"), "应已压缩");
        assert_eq!(decode_plain(&payload).expect("应能解码"), message);
    }

    #[test]
    fn encrypted_round_trips() {
        let message =
            Message::new(MessageType::FileInfo).with_num(42).with_message("payload").with_bytes(vec![0u8; 300]);

        let payload = encode_encrypted(&message, &KEY, &NONCE).expect("应能编码");
        assert_eq!(payload.get(..AEAD_NONCE_LEN), Some(NONCE.as_slice()), "nonce 必须在最前");
        assert_eq!(decode_encrypted(&payload, &KEY).expect("应能解码"), message);
    }

    #[test]
    fn encrypted_payload_is_not_readable_as_plaintext() {
        let message = Message::new(MessageType::Finished);
        let payload = encode_encrypted(&message, &KEY, &NONCE).expect("应能编码");
        assert!(decode_plain(&payload).is_err(), "密文不该能当明文解");
    }

    #[test]
    fn a_wrong_key_fails_authentication() {
        let payload = encode_encrypted(&Message::new(MessageType::Finished), &KEY, &NONCE).expect("应能编码");
        let mut wrong = KEY;
        wrong[0] ^= 0x01;
        assert_eq!(decode_encrypted(&payload, &wrong), Err(ProtocolError::AuthenticationFailed));
    }

    #[test]
    fn plain_session_only_allows_pake_messages() {
        let session = SessionKey::Plain;
        assert!(!session.is_encrypted());

        let pake = Message::new(MessageType::Pake);
        let payload = session.seal(&pake, &NONCE).expect("PAKE 应允许明文");
        assert_eq!(session.open(&payload).expect("应能解出"), pake);

        let confirm = Message::new(MessageType::PakeConfirm);
        assert!(session.seal(&confirm, &NONCE).is_ok());

        for kind in [MessageType::FileInfo, MessageType::Finished, MessageType::Error, MessageType::RelayStandby] {
            let message = Message::new(kind.clone());
            assert_eq!(
                session.seal(&message, &NONCE),
                Err(ProtocolError::PlaintextRejected { kind: kind.to_string() }),
                "{kind} 不该能在明文通道上发出"
            );
        }
    }

    #[test]
    fn plain_session_rejects_incoming_plaintext_non_pake() {
        // 复刻 Go 的 "unencrypted communication rejected"。
        let session = SessionKey::Plain;
        let payload = encode_plain(&Message::new(MessageType::FileInfo)).expect("应能编码");
        assert_eq!(session.open(&payload), Err(ProtocolError::PlaintextRejected { kind: "fileinfo".to_owned() }));
    }

    #[test]
    fn plain_session_rejects_messages_without_a_type() {
        // Go: `m.Type != TypePAKE && m.Type != TypePAKEConfirm && c.Key == nil` → 拒绝。
        let session = SessionKey::Plain;
        let payload = encode_plain(&Message::default()).expect("应能编码");
        assert_eq!(session.open(&payload), Err(ProtocolError::PlaintextRejected { kind: String::new() }));
    }

    #[test]
    fn encrypted_session_accepts_everything() {
        let session = SessionKey::Aead(KEY);
        assert!(session.is_encrypted());
        for kind in MessageType::ALL {
            let message = Message::new(kind.clone());
            let payload = session.seal(&message, &NONCE).expect("应能编码");
            assert_eq!(session.open(&payload).expect("应能解码"), message, "{kind} 应能往返");
        }
    }

    #[test]
    fn encrypted_session_rejects_plaintext_input() {
        let session = SessionKey::Aead(KEY);
        let payload = encode_plain(&Message::new(MessageType::Pake)).expect("应能编码");
        assert!(session.open(&payload).is_err());
    }

    #[test]
    fn large_binary_payloads_round_trip() {
        // 覆盖 base64 的 padding 边界和 DEFLATE 的分块。
        for size in [0usize, 1, 2, 3, 64, 65, 1024, 65535] {
            let bytes = (0..size).map(|index| u8::try_from(index % 251).unwrap_or(0)).collect::<Vec<_>>();
            let message = Message::new(MessageType::FileInfo).with_bytes(bytes);
            let payload = encode_encrypted(&message, &KEY, &NONCE).expect("应能编码");
            assert_eq!(decode_encrypted(&payload, &KEY).expect("应能解码"), message, "size={size}");
        }
    }
}
