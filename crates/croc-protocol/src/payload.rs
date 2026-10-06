//! 协议载荷：消息体里那些**有结构**的 JSON。
//!
//! [`Message`](crate::Message) 只承载"类型 + 几段不透明字节"。真正带结构的东西——文件
//! 清单、分片请求、数据块——都在这里。它们是 Go 侧 `croc.Chunk` / `croc.FileInfo` /
//! `croc.RemoteFileRequest` / `croc.SenderInfo` 的逐字段对应物。
//!
//! # `omitempty` 在这里比在 `Message` 上更要紧
//!
//! Go 的这些结构体里，字段**有的带 `omitempty` 有的不带**，而两者都会体现在字节上：
//!
//! - `FileInfo.IsCompressed` 的 tag 是 `json:"c"`——**没有** `omitempty`，所以 `"c"`
//!   永远出现，哪怕值是 `false`；
//! - `SenderInfo.MachineID` 没有 tag，所以空字符串也会编成 `"MachineID":""`；
//! - 而 `SenderInfo.ExternalIP` 的 tag 是 `json:",omitempty"`，空值整字段消失。
//!
//! 逐字段照抄这些差异是这里唯一的工作。
//!
//! # 一处刻意的字节差异（不影响互通）
//!
//! Go 把 nil 切片编成 `null`、把空切片编成 `[]`。Rust 的 `Vec` 只有一种"空"，统一编成
//! `[]`。Go 解码时两者都得到长度为 0 的切片，而所有消费方判的都是长度，所以互通无碍。
//! 好在实际路径上 Go 也总是送 `[]`（接收方初始化的就是 `[]int64{}`）。

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::json::{ModTime, base64_bytes};

/// 一段数据块。
///
/// 线路上的 `b` 是 `u64 LE 偏移 ‖ 数据`（见 `croc-engine`），所以 [`bytes`](Self::bytes)
/// 的前 8 个字节是块内偏移，后面才是文件内容。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Chunk {
    /// `b`：块的原始字节，JSON 里是标准 base64。
    #[serde(rename = "b", default, with = "base64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub bytes: Vec<u8>,

    /// `l`：块在文件里的偏移。`0` 会被 `omitempty` 吃掉——这没问题，因为 0 就是默认值。
    #[serde(rename = "l", default, skip_serializing_if = "is_zero_i64")]
    pub location: i64,
}

impl Chunk {
    /// 数据块的字节数：Go 侧固定 `TCP_BUFFER_SIZE / 2`。
    pub const DATA_SIZE: usize = 32 * 1024;

    /// 块体里偏移字段的长度。
    pub const OFFSET_LEN: usize = 8;

    /// 一块"空"数据——接收方用它表示"这一块我有了"。
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }
}

/// 一个待传输文件（或空目录）的元信息。
///
/// `declined` 是接收方的本地状态（用户拒绝覆盖/续传），Go 里是不导出字段，**不上线路**。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileInfo {
    /// `n`：文件名（不含目录）。
    #[serde(rename = "n", default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    /// `fr`：接收方的相对目录。
    #[serde(rename = "fr", default, skip_serializing_if = "String::is_empty")]
    pub folder_remote: String,

    /// `fs`：发送方的相对目录。
    #[serde(rename = "fs", default, skip_serializing_if = "String::is_empty")]
    pub folder_source: String,

    /// `h`：文件内容的哈希，JSON 里是标准 base64。
    #[serde(rename = "h", default, with = "base64_bytes", skip_serializing_if = "Vec::is_empty")]
    pub hash: Vec<u8>,

    /// `s`：文件字节数。
    #[serde(rename = "s", default, skip_serializing_if = "is_zero_i64")]
    pub size: i64,

    /// `m`：修改时间。零值时整字段消失（Go 的 `omitzero`）。
    #[serde(rename = "m", default, with = "crate::json::mod_time", skip_serializing_if = "ModTime::is_zero")]
    pub mod_time: ModTime,

    /// `c`：文件是否被压缩过。**没有 `omitempty`**，所以这个字段永远出现在线上。
    #[serde(rename = "c", default)]
    pub is_compressed: bool,

    /// `e`：文件是否被加密过。
    #[serde(rename = "e", default, skip_serializing_if = "is_false")]
    pub is_encrypted: bool,

    /// `sy`：符号链接的目标。非空表示这不是普通文件。
    #[serde(rename = "sy", default, skip_serializing_if = "String::is_empty")]
    pub symlink: String,

    /// `md`：Unix 权限位，Go 侧是 `os.FileMode`（裸 `uint32`，含类型位）。
    #[serde(rename = "md", default, skip_serializing_if = "is_zero_u32")]
    pub mode: u32,

    /// `tf`：这是不是一个临时文件。
    #[serde(rename = "tf", default, skip_serializing_if = "is_false")]
    pub temp_file: bool,

    /// `ig`：这个文件被 `.crocignore` 之类忽略了。
    #[serde(rename = "ig", default, skip_serializing_if = "is_false")]
    pub is_ignored: bool,

    /// `p`：发送方已经完成哈希等准备工作。
    #[serde(rename = "p", default, skip_serializing_if = "is_false")]
    pub prepared: bool,

    /// 接收方拒绝了这个文件（用户选了"不覆盖"），所以什么都没写。
    ///
    /// Go 里的不导出字段 `declined`，**不上线路**——这是接收方自己的账。
    #[serde(skip)]
    pub declined: bool,
}

impl FileInfo {
    /// 文件的 Unix 权限位（剥掉类型位），对应 Go 的 `FileMode.Perm()`。
    #[must_use]
    pub const fn permissions(&self) -> u32 {
        self.mode & MODE_PERMISSION_MASK
    }

    /// 是不是一个目录。
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.mode & MODE_DIR != 0
    }

    /// 是不是一个符号链接。
    #[must_use]
    pub fn is_symlink(&self) -> bool {
        !self.symlink.is_empty()
    }
}

/// Go `os.ModePerm`：低 9 位是权限，高位的类型位不算。
const MODE_PERMISSION_MASK: u32 = 0o777;

/// Go `os.ModeDir`：`1 << 31`。
///
/// 公开是因为 `croc-engine` 在构造"空目录"条目时要自己带上这一位——目录没有内容，
/// 但对端要靠这一位决定"建目录"而不是"建空文件"。
pub const MODE_DIR: u32 = 1 << 31;

/// 接收方对"要哪些字节"的请求。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteFileRequest {
    /// 请求的分片区间，扁平三元组布局：`[chunkSize, start0, count0, …]`。
    ///
    /// **空列表表示"整个文件"**。详见 `croc-core` 的 `chunk` 模块。
    #[serde(rename = "CurrentFileChunkRanges", default)]
    pub current_file_chunk_ranges: Vec<i64>,

    /// 当前正在传第几个文件（`FilesToTransfer` 的下标）。
    #[serde(rename = "FilesToTransferCurrentNum", default)]
    pub files_to_transfer_current_num: i64,

    /// 接收方的机器指纹，用于识别"同一次会话的重连"。
    #[serde(rename = "MachineID", default)]
    pub machine_id: String,

    /// 接收方看到的外网 IP。空值不出现。
    #[serde(rename = "ExternalIP", default, skip_serializing_if = "String::is_empty")]
    pub external_ip: String,

    /// 重连协议版本。
    #[serde(rename = "ReconnectVersion", default)]
    pub reconnect_version: i64,

    /// 能力列表。空列表不出现。
    #[serde(rename = "Features", default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl RemoteFileRequest {
    /// 编成 `recipientready` 消息的 `b` 字段内容。
    ///
    /// Go 是 `json.Marshal(remoteFile)` 之后塞进 `Message.Bytes`，所以这里产出的是**裸
    /// JSON 字节**，不经过 base64——base64 那一步由 [`Message`](crate::Message) 的
    /// `b` 字段适配器负责。
    ///
    /// # Errors
    ///
    /// 序列化失败（实际不会发生，字段都是可序列化的）时返回
    /// [`ProtocolError::MalformedJson`](crate::ProtocolError::MalformedJson)。
    pub fn to_bytes(&self) -> Result<Vec<u8>, crate::ProtocolError> {
        serde_json::to_vec(self).map_err(|error| crate::ProtocolError::MalformedJson(error.to_string()))
    }

    /// 从 `recipientready` 的 `b` 字段解出。
    ///
    /// # Errors
    ///
    /// JSON 不合法或字段类型不符时返回
    /// [`ProtocolError::MalformedJson`](crate::ProtocolError::MalformedJson)。
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, crate::ProtocolError> {
        serde_json::from_slice(bytes).map_err(|error| crate::ProtocolError::MalformedJson(error.to_string()))
    }
}

/// 发送方的文件清单。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SenderInfo {
    /// 要传的文件。
    #[serde(rename = "FilesToTransfer", default)]
    pub files_to_transfer: Vec<FileInfo>,

    /// 要建的空目录（也走 `FileInfo`，只是 `Size` 为 0）。
    #[serde(rename = "EmptyFoldersToTransfer", default)]
    pub empty_folders_to_transfer: Vec<FileInfo>,

    /// 目录总数，用来算进度。
    #[serde(rename = "TotalNumberFolders", default)]
    pub total_number_folders: i64,

    /// 发送方的机器指纹。
    #[serde(rename = "MachineID", default)]
    pub machine_id: String,

    /// 发送方看到的外网 IP。空值不出现。
    #[serde(rename = "ExternalIP", default, skip_serializing_if = "String::is_empty")]
    pub external_ip: String,

    /// 要不要在传之前问接收方一句"收不收"。
    #[serde(rename = "Ask", default)]
    pub ask: bool,

    /// 这次传的是文本（`--text`）而不是文件。
    #[serde(rename = "SendingText", default)]
    pub sending_text: bool,

    /// 用户关掉了压缩。
    #[serde(rename = "NoCompress", default)]
    pub no_compress: bool,

    /// 使用的哈希算法。
    #[serde(rename = "HashAlgorithm", default)]
    pub hash_algorithm: HashAlgorithm,

    /// 重连协议版本。
    #[serde(rename = "ReconnectVersion", default)]
    pub reconnect_version: i64,

    /// 重连时要用的下一个房间名。
    #[serde(rename = "NextReconnectRoom", default)]
    pub next_reconnect_room: String,

    /// 能力列表。空列表不出现。
    #[serde(rename = "Features", default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// 文件哈希算法。
///
/// Go 侧是裸字符串，所以未知值必须能原样透传——新版对端可能用上本端还不认识的算法。
/// 这一点和 [`MessageType`](crate::MessageType) 是同一个理由。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum HashAlgorithm {
    /// 空串：由对端补上默认值。
    #[default]
    Unspecified,
    /// 默认算法。
    Xxhash,
    /// MD5。
    Md5,
    /// HighwayHash。
    Highway,
    /// imohash：只采样文件头尾与少量中间块。
    Imohash,
    /// imohash 的 v2 变体，逐块渐进式。
    ImohashV2,
    /// 本端还不认识的算法。
    Unknown(String),
}

/// 默认哈希算法，对应 Go 的 `defaultHashAlgorithm`。
pub const DEFAULT_HASH_ALGORITHM: HashAlgorithm = HashAlgorithm::Xxhash;

/// 渐进式哈希算法，对应 Go 的 `progressiveHashAlgorithm`。
pub const PROGRESSIVE_HASH_ALGORITHM: HashAlgorithm = HashAlgorithm::ImohashV2;

impl HashAlgorithm {
    /// 线路上的字符串。
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Unspecified => "",
            Self::Xxhash => "xxhash",
            Self::Md5 => "md5",
            Self::Highway => "highway",
            Self::Imohash => "imohash",
            Self::ImohashV2 => "imohash-v2",
            Self::Unknown(raw) => raw,
        }
    }
}

impl std::fmt::Display for HashAlgorithm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl From<&str> for HashAlgorithm {
    fn from(raw: &str) -> Self {
        match raw {
            "" => Self::Unspecified,
            "xxhash" => Self::Xxhash,
            "md5" => Self::Md5,
            "highway" => Self::Highway,
            "imohash" => Self::Imohash,
            "imohash-v2" => Self::ImohashV2,
            other => Self::Unknown(other.to_owned()),
        }
    }
}

impl From<String> for HashAlgorithm {
    fn from(raw: String) -> Self {
        Self::from(raw.as_str())
    }
}

impl Serialize for HashAlgorithm {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for HashAlgorithm {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        String::deserialize(deserializer).map(Self::from)
    }
}

/// `omitempty` 对 `bool`：`false` 不出现。
fn is_false(value: &bool) -> bool {
    !*value
}

/// `omitempty` 对 `i64`：`0` 不出现。
fn is_zero_i64(value: &i64) -> bool {
    *value == 0
}

/// `omitempty` 对 `u32`：`0` 不出现。
fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn json_of<T: Serialize>(value: &T) -> String {
        serde_json::to_string(value).expect("应能编码")
    }

    #[test]
    fn chunk_wire_names_and_omissions_match_go() {
        assert_eq!(json_of(&Chunk::empty()), "{}");
        // 偏移 0 被 `omitempty` 吃掉，数据仍然在。
        let chunk = Chunk { bytes: vec![0xfb, 0xff, 0x00], location: 0 };
        assert_eq!(json_of(&chunk), r#"{"b":"+/8A"}"#);
        let moved = Chunk { bytes: vec![1], location: 32768 };
        assert_eq!(json_of(&moved), r#"{"b":"AQ==","l":32768}"#);
    }

    #[test]
    fn an_empty_file_info_keeps_only_the_compression_flag() {
        // `IsCompressed` 的 tag 没有 `omitempty`，所以 `"c"` 永远出现。
        assert_eq!(json_of(&FileInfo::default()), r#"{"c":false}"#);
    }

    #[test]
    fn file_info_wire_names_match_go() {
        let info = FileInfo {
            name: "a.txt".into(),
            folder_remote: "dir".into(),
            folder_source: "src".into(),
            hash: vec![0xde, 0xad],
            size: 42,
            is_compressed: true,
            mode: 0o644,
            ..FileInfo::default()
        };
        assert_eq!(json_of(&info), r#"{"n":"a.txt","fr":"dir","fs":"src","h":"3q0=","s":42,"c":true,"md":420}"#);
    }

    #[test]
    fn file_info_round_trips() {
        let info = FileInfo {
            name: "link".into(),
            symlink: "../target".into(),
            mode: MODE_DIR | 0o755,
            temp_file: true,
            is_ignored: true,
            prepared: true,
            is_encrypted: true,
            declined: true,
            ..FileInfo::default()
        };
        let json = json_of(&info);
        let decoded: FileInfo = serde_json::from_str(&json).expect("应能解码");
        // `declined` 是本地状态，不上线路，所以解回来一定是 false。
        assert_eq!(decoded, FileInfo { declined: false, ..info.clone() });
        assert!(decoded.is_dir());
        assert_eq!(decoded.permissions(), 0o755);
        assert!(decoded.is_symlink());
    }

    #[test]
    fn mod_time_is_omitted_when_zero_and_present_otherwise() {
        assert_eq!(json_of(&FileInfo::default()), r#"{"c":false}"#);
        let instant = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).expect("合法时间戳");
        let info = FileInfo { mod_time: ModTime::new(instant), ..FileInfo::default() };
        let json = json_of(&info);
        assert!(json.contains(r#""m":"2023-11-14T22:13:20Z""#), "{json}");
    }

    #[test]
    fn remote_file_request_always_carries_every_field() {
        // Go 侧这些字段都没有 `omitempty`，所以空值也会出现。
        assert_eq!(
            json_of(&RemoteFileRequest::default()),
            r#"{"CurrentFileChunkRanges":[],"FilesToTransferCurrentNum":0,"MachineID":"","ReconnectVersion":0}"#
        );
    }

    #[test]
    fn remote_file_request_round_trips_a_resume_request() {
        let request = RemoteFileRequest {
            current_file_chunk_ranges: vec![32768, 0, 2, 65536, 1],
            files_to_transfer_current_num: 3,
            machine_id: "abc123".into(),
            external_ip: "203.0.113.7".into(),
            reconnect_version: 1,
            features: vec!["chunked-fileinfo-v1".into()],
        };
        let json = json_of(&request);
        let decoded: RemoteFileRequest = serde_json::from_str(&json).expect("应能解码");
        assert_eq!(decoded, request);
    }

    #[test]
    fn sender_info_wire_names_match_go() {
        let info = SenderInfo { hash_algorithm: HashAlgorithm::Xxhash, ..SenderInfo::default() };
        assert_eq!(
            json_of(&info),
            r#"{"FilesToTransfer":[],"EmptyFoldersToTransfer":[],"TotalNumberFolders":0,"MachineID":"","Ask":false,"SendingText":false,"NoCompress":false,"HashAlgorithm":"xxhash","ReconnectVersion":0,"NextReconnectRoom":""}"#
        );
    }

    #[test]
    fn sender_info_round_trips() {
        let info = SenderInfo {
            files_to_transfer: vec![FileInfo { name: "a".into(), size: 1, ..FileInfo::default() }],
            empty_folders_to_transfer: vec![FileInfo { name: "d".into(), mode: MODE_DIR, ..FileInfo::default() }],
            total_number_folders: 2,
            machine_id: "machine".into(),
            external_ip: "198.51.100.4".into(),
            ask: true,
            sending_text: true,
            no_compress: true,
            hash_algorithm: HashAlgorithm::ImohashV2,
            reconnect_version: 1,
            next_reconnect_room: "room".into(),
            features: vec!["staged-transport-v1".into()],
        };
        let json = json_of(&info);
        let decoded: SenderInfo = serde_json::from_str(&json).expect("应能解码");
        assert_eq!(decoded, info);
    }

    #[test]
    fn hash_algorithm_names_match_the_go_strings() {
        let expected = [
            ("", HashAlgorithm::Unspecified),
            ("xxhash", HashAlgorithm::Xxhash),
            ("md5", HashAlgorithm::Md5),
            ("highway", HashAlgorithm::Highway),
            ("imohash", HashAlgorithm::Imohash),
            ("imohash-v2", HashAlgorithm::ImohashV2),
        ];
        for (name, algorithm) in &expected {
            assert_eq!(algorithm.as_str(), *name);
            assert_eq!(&HashAlgorithm::from(*name), algorithm);
        }
        assert_eq!(HashAlgorithm::default(), HashAlgorithm::Unspecified);
        assert_eq!(DEFAULT_HASH_ALGORITHM, HashAlgorithm::Xxhash);
        assert_eq!(PROGRESSIVE_HASH_ALGORITHM, HashAlgorithm::ImohashV2);
    }

    #[test]
    fn an_unknown_hash_algorithm_survives_a_round_trip() {
        let decoded: SenderInfo = serde_json::from_str(r#"{"HashAlgorithm":"blake3-v9"}"#).expect("应能解码未知算法");
        assert_eq!(decoded.hash_algorithm, HashAlgorithm::Unknown("blake3-v9".into()));
        assert_eq!(json_of(&decoded.hash_algorithm), r#""blake3-v9""#);
    }

    #[test]
    fn a_missing_field_falls_back_to_the_zero_value() {
        // Go 的 `encoding/json` 对缺失字段不做任何事，等价于保留零值。
        let decoded: FileInfo = serde_json::from_str("{}").expect("应能解码");
        assert_eq!(decoded, FileInfo::default());
        let sender: SenderInfo = serde_json::from_str("{}").expect("应能解码");
        assert_eq!(sender, SenderInfo::default());
    }

    #[test]
    fn chunk_sizes_match_the_go_constants() {
        // Go: 块体是 8 字节偏移 + 数据，数据段 `TCP_BUFFER_SIZE / 2`。
        assert_eq!(Chunk::DATA_SIZE, 65536 / 2);
        assert_eq!(Chunk::OFFSET_LEN, 8);
    }
}
