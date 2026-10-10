//! 分片文件清单：`chunked-fileinfo-v1` 的线路格式。
//!
//! 一个目录可能有几十万个文件，整份 `SenderInfo` 塞进一条 `fileinfo` 消息会撞上 64 MiB
//! 的帧上限。所以协商到这个能力之后，清单被拆成三条消息：
//!
//! ```text
//! fileinfo-start   {"fc":2,"ec":1,"tf":3,"mi":"…",…}        ← 只报数量与发送方设置
//! fileinfo-batch 0 {"k":"f","e":[{…},{…}]}                  ← 文件条目，最多 8 MiB 一批
//! fileinfo-batch 1 {"k":"d","e":[{…}]}                      ← 空目录条目
//! fileinfo-end     (无载荷，Num = 批次数)                    ← 收尾，校验数量对不对
//! ```
//!
//! `Num` 从 0 连续递增，`fileinfo-end` 的 `Num` 就是批次数。接收方拿 `fc`/`ec` 当上限
//! 卡住每一批，收尾时再核对总数——任何一处对不上都整份作废。
//!
//! # 为什么批次是手拼 JSON 而不是 `serde_json::to_vec`
//!
//! 批次要按字节数装箱，所以必须先知道每个条目编出来多长。Go 的做法是把前缀
//! `{"k":"f","e":[`、逐条编码、再用 `,` 和 `]}` 拼起来。这里照做——顺带保证了产物与
//! Go 的 `json.Marshal(fileInfoManifestBatch)` 逐字节相同（字段顺序 `k`, `e`）。

use serde::{Deserialize, Serialize};

use crate::ProtocolError;
use crate::payload::{FileInfo, HashAlgorithm, SenderInfo};

/// 单批 JSON 的上限，对应 Go 的 `maxFileInfoBatchJSONSize`。
pub const MAX_BATCH_JSON_SIZE: usize = 8 * 1024 * 1024;

/// 清单条目总数上限，对应 Go 的 `maxChunkedManifestEntries`。
pub const MAX_MANIFEST_ENTRIES: usize = 1_000_000;

/// 整份清单 JSON 的累计上限，对应 Go 的 `maxChunkedManifestJSONSize`。
pub const MAX_MANIFEST_JSON_SIZE: u64 = 512 * 1024 * 1024;

/// 一批条目的种类。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BatchKind {
    /// `"f"`：要传的文件。
    Files,
    /// `"d"`：要建的空目录。
    EmptyFolders,
}

impl BatchKind {
    /// 线路上的单字符。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Files => "f",
            Self::EmptyFolders => "d",
        }
    }

    /// 从线路上的单字符解析。不认识的返回 `None`。
    #[must_use]
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "f" => Some(Self::Files),
            "d" => Some(Self::EmptyFolders),
            _ => None,
        }
    }
}

impl std::fmt::Display for BatchKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// `fileinfo-start` 的载荷。
///
/// 它是 `SenderInfo` 去掉两个条目列表之后的形状——数量字段顶替了列表本身。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestHeader {
    /// `fc`：文件条目数。
    #[serde(rename = "fc", default)]
    pub file_count: i64,

    /// `ec`：空目录条目数。
    #[serde(rename = "ec", default)]
    pub empty_folder_count: i64,

    /// `tf`：目录总数，用来算进度。
    #[serde(rename = "tf", default)]
    pub total_number_folders: i64,

    /// `mi`：发送方的机器指纹。
    #[serde(rename = "mi", default)]
    pub machine_id: String,

    /// `ip`：发送方看到的外网 IP。空值不出现。
    #[serde(rename = "ip", default, skip_serializing_if = "String::is_empty")]
    pub external_ip: String,

    /// `a`：要不要先问接收方一句。
    #[serde(rename = "a", default)]
    pub ask: bool,

    /// `st`：这次传的是文本。
    #[serde(rename = "st", default)]
    pub sending_text: bool,

    /// `nc`：用户关掉了压缩。
    #[serde(rename = "nc", default)]
    pub no_compress: bool,

    /// `ha`：哈希算法。
    #[serde(rename = "ha", default)]
    pub hash_algorithm: HashAlgorithm,

    /// `rv`：重连协议版本。
    #[serde(rename = "rv", default)]
    pub reconnect_version: i64,

    /// `nr`：重连要用的下一个房间名。空值不出现。
    #[serde(rename = "nr", default, skip_serializing_if = "String::is_empty")]
    pub next_reconnect_room: String,

    /// `f`：能力列表。空列表不出现。
    #[serde(rename = "f", default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

impl ManifestHeader {
    /// 从完整的 `SenderInfo` 取头部。
    #[must_use]
    pub fn from_sender_info(info: &SenderInfo) -> Self {
        Self {
            file_count: len_as_i64(info.files_to_transfer.len()),
            empty_folder_count: len_as_i64(info.empty_folders_to_transfer.len()),
            total_number_folders: info.total_number_folders,
            machine_id: info.machine_id.clone(),
            external_ip: info.external_ip.clone(),
            ask: info.ask,
            sending_text: info.sending_text,
            no_compress: info.no_compress,
            hash_algorithm: info.hash_algorithm.clone(),
            reconnect_version: info.reconnect_version,
            next_reconnect_room: info.next_reconnect_room.clone(),
            features: info.features.clone(),
        }
    }

    /// 把头部与收齐的条目列表拼回完整的 `SenderInfo`。
    #[must_use]
    pub fn into_sender_info(self, files: Vec<FileInfo>, empty_folders: Vec<FileInfo>) -> SenderInfo {
        SenderInfo {
            files_to_transfer: files,
            empty_folders_to_transfer: empty_folders,
            total_number_folders: self.total_number_folders,
            machine_id: self.machine_id,
            external_ip: self.external_ip,
            ask: self.ask,
            sending_text: self.sending_text,
            no_compress: self.no_compress,
            hash_algorithm: self.hash_algorithm,
            reconnect_version: self.reconnect_version,
            next_reconnect_room: self.next_reconnect_room,
            features: self.features,
        }
    }

    /// 校验数量字段：不能为负，且两者之和不超过 `max_entries`。
    ///
    /// # Errors
    ///
    /// 数量为负或超出上限时返回 [`ManifestError`]。
    pub fn validate(&self, max_entries: usize) -> Result<(), ManifestError> {
        if self.file_count < 0 || self.empty_folder_count < 0 {
            return Err(ManifestError::NegativeEntryCount);
        }
        let files = usize_from_i64(self.file_count);
        let folders = usize_from_i64(self.empty_folder_count);
        // 用减法而不是加法：`max_entries - files` 在 `files > max_entries` 时下溢，
        // 而 Rust 的 `usize` 下溢在 debug 下会 panic。先比 `files` 就避开了。
        if files > max_entries || folders > max_entries.saturating_sub(files) {
            return Err(ManifestError::HeaderExceedsEntries { max: max_entries });
        }
        Ok(())
    }
}

/// 一批条目的载荷。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestBatch {
    /// `k`：`"f"` 或 `"d"`。
    #[serde(rename = "k", default)]
    pub kind: String,

    /// `e`：条目。
    #[serde(rename = "e", default)]
    pub entries: Vec<FileInfo>,
}

/// 清单的容量上限。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestLimits {
    /// 单批 JSON 上限。
    pub max_batch_json_size: usize,
    /// 条目总数上限。
    pub max_entries: usize,
    /// 整份清单 JSON 的累计上限。
    pub max_manifest_json_size: u64,
}

impl Default for ManifestLimits {
    fn default() -> Self {
        Self {
            max_batch_json_size: MAX_BATCH_JSON_SIZE,
            max_entries: MAX_MANIFEST_ENTRIES,
            max_manifest_json_size: MAX_MANIFEST_JSON_SIZE,
        }
    }
}

impl ManifestLimits {
    /// 校验上限本身是合理的。
    ///
    /// # Errors
    ///
    /// 任一项取到无意义的值时返回 [`ManifestError`]。
    pub const fn validate(self) -> Result<(), ManifestError> {
        if self.max_batch_json_size == 0 {
            return Err(ManifestError::BatchLimitNotPositive);
        }
        if self.max_manifest_json_size == 0 {
            return Err(ManifestError::MetadataLimitNotPositive);
        }
        Ok(())
    }
}

/// 上线路之前要清掉的字段。
///
/// `FolderSource` 是发送方本地的目录布局，接收方用不上；`IsIgnored` 是发送方
/// `.crocignore` 的结果，同样不该泄露。Go 在发送前统一清，这里也一样。
#[must_use]
pub fn for_wire(info: &FileInfo) -> FileInfo {
    FileInfo { folder_source: String::new(), is_ignored: false, ..info.clone() }
}

/// 对整批条目做 [`for_wire`]。
#[must_use]
pub fn for_wire_all(infos: &[FileInfo]) -> Vec<FileInfo> {
    infos.iter().map(for_wire).collect()
}

/// 一份拆好的清单：头部载荷 + 各批载荷。
///
/// 发送方按顺序发 `fileinfo-start(header)`、`fileinfo-batch(i, batches[i])`、
/// `fileinfo-end(num = batches.len(), 无载荷)`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestPayloads {
    /// `fileinfo-start` 的载荷。
    pub header: Vec<u8>,
    /// `fileinfo-batch` 的载荷，下标就是消息的 `Num`。
    pub batches: Vec<Vec<u8>>,
}

impl ManifestPayloads {
    /// 批次数，也就是 `fileinfo-end` 要带的 `Num`。
    #[must_use]
    pub fn batch_count(&self) -> i64 {
        len_as_i64(self.batches.len())
    }
}

/// 老式单条 `fileinfo` 的载荷：整份 `SenderInfo` 直接序列化成一条 JSON。
///
/// 对端没声明 `chunked-fileinfo-v1` 时只认这一条（Go 的 `sendLegacySenderInfo`），
/// 浏览器客户端就是这种情况——发分块三连它会读不到清单。
///
/// # Errors
///
/// 序列化失败时返回 [`ManifestError`]。
pub fn encode_legacy_manifest(info: &SenderInfo) -> Result<Vec<u8>, ManifestError> {
    to_json(info)
}

/// 把 `SenderInfo` 拆成三条消息的载荷序列。
///
/// 这个函数是纯的：不发送、不分配 socket，只产出字节。发送顺序由调用方保证。
///
/// 整份清单本来就已经在内存里（`info` 是引用），所以批次也一并放在内存里返回。
///
/// # Errors
///
/// 条目数超限、单条编不进一批、累计超过清单上限时返回 [`ManifestError`]。
pub fn encode_manifest(info: &SenderInfo, limits: ManifestLimits) -> Result<ManifestPayloads, ManifestError> {
    limits.validate()?;

    let file_count = info.files_to_transfer.len();
    let folder_count = info.empty_folders_to_transfer.len();
    if file_count > limits.max_entries || folder_count > limits.max_entries.saturating_sub(file_count) {
        return Err(ManifestError::TooManyEntries {
            files: file_count,
            folders: folder_count,
            max: limits.max_entries,
        });
    }

    let header = ManifestHeader::from_sender_info(info);
    let header_payload = to_json(&header)?;
    let mut decoded_bytes = u64_from_usize(header_payload.len());
    if decoded_bytes > limits.max_manifest_json_size {
        return Err(ManifestError::MetadataTooLarge { actual: decoded_bytes, limit: limits.max_manifest_json_size });
    }

    let mut batches = Vec::new();
    for (infos, kind) in
        [(&info.files_to_transfer, BatchKind::Files), (&info.empty_folders_to_transfer, BatchKind::EmptyFolders)]
    {
        for payload in encode_batches(infos, kind, limits.max_batch_json_size)? {
            decoded_bytes = decoded_bytes.saturating_add(u64_from_usize(payload.len()));
            if decoded_bytes > limits.max_manifest_json_size {
                return Err(ManifestError::MetadataTooLarge {
                    actual: decoded_bytes,
                    limit: limits.max_manifest_json_size,
                });
            }
            batches.push(payload);
        }
    }

    Ok(ManifestPayloads { header: header_payload, batches })
}

/// 把条目切成不超过 `max_payload_size` 的 JSON 批次。
///
/// 前缀 `{"k":"<kind>","e":[` 与后缀 `]}` 是手写的，中间夹着逐条编码的结果——这样
/// 产物与 Go 的 `json.Marshal(ManifestBatch)` 逐字节相同，而装箱时又知道每条的准确
/// 长度。条目之间的 `,` 也算进长度里。
///
/// # Errors
///
/// 上限小到装不下前后缀，或单条条目自己就超限时返回 [`ManifestError`]。
pub fn encode_batches(
    infos: &[FileInfo],
    kind: BatchKind,
    max_payload_size: usize,
) -> Result<Vec<Vec<u8>>, ManifestError> {
    let prefix = format!(r#"{{"k":"{}","e":["#, kind.as_str()).into_bytes();
    let suffix = b"]}".to_vec();
    let overhead = prefix.len() + suffix.len();
    if max_payload_size < overhead {
        return Err(ManifestError::BatchLimitTooSmall { limit: max_payload_size });
    }

    let mut batches: Vec<Vec<u8>> = Vec::new();
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut batch_size = overhead;

    for info in infos {
        let encoded = to_json(&for_wire(info))?;
        // 条目之间要插一个 `,`，所以第 2 条起多算一个字节。
        let mut entry_size = encoded.len() + usize::from(!pending.is_empty());
        if batch_size + entry_size > max_payload_size {
            if pending.is_empty() {
                return Err(ManifestError::EntryTooLarge { actual: batch_size + entry_size, limit: max_payload_size });
            }
            batches.push(assemble(&prefix, &suffix, &pending));
            pending.clear();
            batch_size = overhead;
            entry_size = encoded.len();
        }
        if batch_size + entry_size > max_payload_size {
            return Err(ManifestError::EntryTooLarge { actual: batch_size + entry_size, limit: max_payload_size });
        }
        pending.push(encoded);
        batch_size += entry_size;
    }
    if !pending.is_empty() {
        batches.push(assemble(&prefix, &suffix, &pending));
    }
    Ok(batches)
}

/// 拼出一个批次的字节。
fn assemble(prefix: &[u8], suffix: &[u8], entries: &[Vec<u8>]) -> Vec<u8> {
    let capacity = prefix.len() + suffix.len() + entries.iter().map(Vec::len).sum::<usize>() + entries.len();
    let mut payload = Vec::with_capacity(capacity);
    payload.extend_from_slice(prefix);
    for (index, entry) in entries.iter().enumerate() {
        if index > 0 {
            payload.push(b',');
        }
        payload.extend_from_slice(entry);
    }
    payload.extend_from_slice(suffix);
    payload
}

/// 接收方的清单累加器。
///
/// 三条消息按顺序喂进来：`start` 构造它，`batch` 累积，`finish` 消费掉并给出完整的
/// `SenderInfo`。任何一步对不上都会整体作废（返回 `Err`，调用方丢弃这个累加器）。
///
/// 这里不检查角色、能力协商与信道加密状态——那是会话级的事，归调用方。
#[derive(Debug)]
pub struct IncomingManifest {
    header: ManifestHeader,
    limits: ManifestLimits,
    next_batch: i64,
    decoded_bytes: u64,
    files: Vec<FileInfo>,
    empty_folders: Vec<FileInfo>,
}

impl IncomingManifest {
    /// 处理 `fileinfo-start`。
    ///
    /// # Errors
    ///
    /// 上限非法、头部解不出、数量字段越界时返回 [`ManifestError`]。
    pub fn start(payload: &[u8], limits: ManifestLimits) -> Result<Self, ManifestError> {
        limits.validate()?;
        let header: ManifestHeader = from_json(payload).map_err(ManifestError::BadHeader)?;
        header.validate(limits.max_entries)?;
        if u64_from_usize(payload.len()) > limits.max_manifest_json_size {
            return Err(ManifestError::MetadataTooLarge {
                actual: u64_from_usize(payload.len()),
                limit: limits.max_manifest_json_size,
            });
        }
        Ok(Self {
            header,
            limits,
            next_batch: 0,
            decoded_bytes: u64_from_usize(payload.len()),
            files: Vec::new(),
            empty_folders: Vec::new(),
        })
    }

    /// 头部。收到 `start` 之后就能读到 `MachineID` / `ReconnectVersion` 等字段。
    #[must_use]
    pub const fn header(&self) -> &ManifestHeader {
        &self.header
    }

    /// 下一个期待的批次号。
    #[must_use]
    pub const fn next_batch(&self) -> i64 {
        self.next_batch
    }

    /// 已经收下的文件条目数。
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// 已经收下的空目录条目数。
    #[must_use]
    pub fn empty_folder_count(&self) -> usize {
        self.empty_folders.len()
    }

    /// 处理 `fileinfo-batch`。
    ///
    /// # Errors
    ///
    /// 批次号跳号、单批超限、累计超限、JSON 解不出、条目数超过头部声明时返回
    /// [`ManifestError`]。
    pub fn batch(&mut self, num: i64, payload: &[u8]) -> Result<(), ManifestError> {
        if num != self.next_batch {
            return Err(ManifestError::UnexpectedBatch { actual: num, expected: self.next_batch });
        }
        if payload.len() > self.limits.max_batch_json_size {
            return Err(ManifestError::BatchTooLarge { actual: payload.len(), limit: self.limits.max_batch_json_size });
        }
        let total = self.decoded_bytes.saturating_add(u64_from_usize(payload.len()));
        if total > self.limits.max_manifest_json_size {
            return Err(ManifestError::MetadataTooLarge { actual: total, limit: self.limits.max_manifest_json_size });
        }

        let batch: ManifestBatch = from_json(payload).map_err(ManifestError::BadBatch)?;
        if batch.entries.is_empty() {
            return Err(ManifestError::EmptyBatch);
        }
        let entries: Vec<FileInfo> = batch.entries.iter().map(for_wire).collect();
        let kind = BatchKind::parse(batch.kind.as_str()).ok_or(ManifestError::UnknownBatchKind(batch.kind))?;
        let (target, declared, label) = match kind {
            BatchKind::Files => (&mut self.files, self.header.file_count, "files"),
            BatchKind::EmptyFolders => (&mut self.empty_folders, self.header.empty_folder_count, "empty folders"),
        };
        if entries.len() > usize_from_i64(declared).saturating_sub(target.len()) {
            return Err(ManifestError::MoreThanDeclared { kind: label });
        }
        target.extend(entries);

        self.decoded_bytes = total;
        self.next_batch += 1;
        Ok(())
    }

    /// 处理 `fileinfo-end`，返回收齐的 `SenderInfo`。
    ///
    /// # Errors
    ///
    /// 批次号不符、`end` 带了载荷、或收下的条目数与头部声明不一致时返回
    /// [`ManifestError`]。
    pub fn finish(self, num: i64, payload: &[u8]) -> Result<SenderInfo, ManifestError> {
        if num != self.next_batch {
            return Err(ManifestError::UnexpectedEnd { actual: num, expected: self.next_batch });
        }
        if !payload.is_empty() {
            return Err(ManifestError::EndHasData);
        }
        let expected_files = usize_from_i64(self.header.file_count);
        let expected_folders = usize_from_i64(self.header.empty_folder_count);
        if self.files.len() != expected_files || self.empty_folders.len() != expected_folders {
            return Err(ManifestError::Incomplete {
                files: self.files.len(),
                expected_files,
                folders: self.empty_folders.len(),
                expected_folders,
            });
        }
        Ok(self.header.into_sender_info(self.files, self.empty_folders))
    }
}

/// 清单的失败模式。错误消息与 Go 的 `fmt.Errorf` 逐条对应。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// 单批上限为 0。
    #[error("file metadata batch limit must be positive")]
    BatchLimitNotPositive,

    /// 清单累计上限为 0。
    #[error("file manifest metadata limit must be positive")]
    MetadataLimitNotPositive,

    /// 条目总数超过上限。
    #[error("file manifest contains {files} files and {folders} empty folders; maximum combined entry count is {max}")]
    TooManyEntries {
        /// 文件条目数。
        files: usize,
        /// 空目录条目数。
        folders: usize,
        /// 允许的总数。
        max: usize,
    },

    /// 头部声明的条目数超过上限。
    #[error("file manifest entry count exceeds maximum of {max}")]
    HeaderExceedsEntries {
        /// 允许的总数。
        max: usize,
    },

    /// 头部里有负数条目数。
    #[error("file manifest contains a negative entry count")]
    NegativeEntryCount,

    /// 单批上限小到装不下 JSON 骨架。
    #[error("file metadata batch limit {limit} is too small")]
    BatchLimitTooSmall {
        /// 配置的上限。
        limit: usize,
    },

    /// 一批超过上限。
    #[error("file metadata batch exceeds maximum size: {actual} > {limit}")]
    BatchTooLarge {
        /// 实际的字节数。
        actual: usize,
        /// 允许的上限。
        limit: usize,
    },

    /// 单条条目自己就超过一批的上限。
    #[error("single file metadata entry exceeds batch limit: {actual} > {limit}")]
    EntryTooLarge {
        /// 加上骨架后的字节数。
        actual: usize,
        /// 允许的上限。
        limit: usize,
    },

    /// 整份清单的 JSON 累计超过上限。
    #[error("file manifest metadata exceeds maximum size: {actual} > {limit}")]
    MetadataTooLarge {
        /// 累计的字节数。
        actual: u64,
        /// 允许的上限。
        limit: u64,
    },

    /// 头部 JSON 解不出。
    #[error("decode file manifest header: {0}")]
    BadHeader(String),

    /// 批次 JSON 解不出。
    #[error("decode file manifest batch: {0}")]
    BadBatch(String),

    /// 批次里一条条目都没有。
    #[error("file manifest batch is empty")]
    EmptyBatch,

    /// 批次种类不是 `f` 也不是 `d`。
    #[error("unknown file manifest batch kind {0:?}")]
    UnknownBatchKind(String),

    /// 某一类条目比头部声明的还多。
    #[error("file manifest contains more {kind} than declared")]
    MoreThanDeclared {
        /// `"files"` 或 `"empty folders"`。
        kind: &'static str,
    },

    /// 批次号跳号。
    #[error("unexpected file manifest batch {actual}; wanted {expected}")]
    UnexpectedBatch {
        /// 收到的批次号。
        actual: i64,
        /// 期待的批次号。
        expected: i64,
    },

    /// 收尾的批次号不符。
    #[error("unexpected file manifest end {actual}; wanted {expected}")]
    UnexpectedEnd {
        /// 收到的批次号。
        actual: i64,
        /// 期待的批次号。
        expected: i64,
    },

    /// `fileinfo-end` 不该带载荷。
    #[error("file manifest end contains unexpected data")]
    EndHasData,

    /// 收下的条目数与头部声明不一致。
    #[error(
        "incomplete file manifest: received {files}/{expected_files} files and {folders}/{expected_folders} empty folders"
    )]
    Incomplete {
        /// 收下的文件数。
        files: usize,
        /// 声明的文件数。
        expected_files: usize,
        /// 收下的空目录数。
        folders: usize,
        /// 声明的空目录数。
        expected_folders: usize,
    },
}

impl From<ProtocolError> for ManifestError {
    fn from(error: ProtocolError) -> Self {
        Self::BadBatch(error.to_string())
    }
}

fn to_json<T: Serialize>(value: &T) -> Result<Vec<u8>, ManifestError> {
    serde_json::to_vec(value).map_err(|error| ManifestError::BadBatch(error.to_string()))
}

fn from_json<'de, T: Deserialize<'de>>(json: &'de [u8]) -> Result<T, String> {
    serde_json::from_slice(json).map_err(|error| error.to_string())
}

/// `usize` → `i64`。条目数上限是 100 万，塞得下。
fn len_as_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// `i64` → `usize`。负值由调用方先挡掉，这里兜底成 0。
fn usize_from_i64(value: i64) -> usize {
    usize::try_from(value).unwrap_or(0)
}

/// `usize` → `u64`。`usize` 不会比 `u64` 宽（本项目的目标平台里没有 128 位）。
fn u64_from_usize(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(name: &str, size: i64) -> FileInfo {
        FileInfo { name: name.to_owned(), size, ..FileInfo::default() }
    }

    fn sender_info(files: Vec<FileInfo>, folders: Vec<FileInfo>) -> SenderInfo {
        SenderInfo {
            files_to_transfer: files,
            empty_folders_to_transfer: folders,
            total_number_folders: 1,
            machine_id: "machine".into(),
            hash_algorithm: HashAlgorithm::Xxhash,
            features: vec!["chunked-fileinfo-v1".into()],
            ..SenderInfo::default()
        }
    }

    #[test]
    fn batch_kind_names_match_the_go_constants() {
        assert_eq!(BatchKind::Files.as_str(), "f");
        assert_eq!(BatchKind::EmptyFolders.as_str(), "d");
        assert_eq!(BatchKind::parse("f"), Some(BatchKind::Files));
        assert_eq!(BatchKind::parse("d"), Some(BatchKind::EmptyFolders));
        assert_eq!(BatchKind::parse("x"), None);
    }

    #[test]
    fn header_wire_names_match_go() {
        let header = ManifestHeader::from_sender_info(&sender_info(vec![], vec![]));
        let json = String::from_utf8(to_json(&header).expect("应能编码")).expect("JSON 是 UTF-8");
        assert_eq!(
            json,
            r#"{"fc":0,"ec":0,"tf":1,"mi":"machine","a":false,"st":false,"nc":false,"ha":"xxhash","rv":0,"f":["chunked-fileinfo-v1"]}"#
        );
    }

    #[test]
    fn header_round_trips_through_sender_info() {
        let original = sender_info(vec![info("a", 1)], vec![info("d", 0)]);
        let header = ManifestHeader::from_sender_info(&original);
        assert_eq!(header.file_count, 1);
        assert_eq!(header.empty_folder_count, 1);
        let rebuilt =
            header.into_sender_info(original.files_to_transfer.clone(), original.empty_folders_to_transfer.clone());
        assert_eq!(rebuilt, original);
    }

    #[test]
    fn for_wire_strips_sender_local_fields() {
        let local =
            FileInfo { name: "a".into(), folder_source: "secret/dir".into(), is_ignored: true, ..FileInfo::default() };
        let wire = for_wire(&local);
        assert!(wire.folder_source.is_empty());
        assert!(!wire.is_ignored);
        assert_eq!(wire.name, "a");
        assert_eq!(for_wire_all(std::slice::from_ref(&local)).len(), 1);
    }

    #[test]
    fn a_batch_is_byte_identical_to_a_plain_serialization() {
        let entries = vec![info("a.txt", 3), info("b.txt", 4)];
        let batches = encode_batches(&entries, BatchKind::Files, 1024).expect("应能装箱");
        assert_eq!(batches.len(), 1);
        let expected = to_json(&ManifestBatch { kind: "f".into(), entries: entries.iter().map(for_wire).collect() })
            .expect("应能编码");
        assert_eq!(batches[0], expected);
    }

    #[test]
    fn batching_respects_the_size_limit() {
        let entries: Vec<FileInfo> = (0..50).map(|index| info(&format!("file-{index:03}.txt"), index)).collect();
        let one = to_json(&for_wire(&entries[0])).expect("应能编码");
        // 每批只放得下 3 条（骨架约 13 字节，条目之间的逗号也算）。
        let limit = 13 + 4 * one.len();
        let batches = encode_batches(&entries, BatchKind::Files, limit).expect("应能装箱");
        assert!(batches.len() > 1);
        let mut total = 0;
        for payload in &batches {
            assert!(payload.len() <= limit, "批次 {} 超过上限 {}", payload.len(), limit);
            let decoded: ManifestBatch = from_json(payload).expect("应能解码");
            total += decoded.entries.len();
            assert_eq!(decoded.kind, "f");
        }
        assert_eq!(total, entries.len(), "装箱不该丢条目");
    }

    #[test]
    fn an_empty_entry_list_produces_no_batches() {
        assert!(encode_batches(&[], BatchKind::Files, 1024).expect("应能装箱").is_empty());
    }

    #[test]
    fn a_batch_limit_that_cannot_hold_the_skeleton_is_rejected() {
        assert_eq!(
            encode_batches(&[info("a", 1)], BatchKind::Files, 4),
            Err(ManifestError::BatchLimitTooSmall { limit: 4 })
        );
    }

    #[test]
    fn an_entry_that_cannot_fit_alone_is_rejected() {
        let big = FileInfo { name: "x".repeat(200), ..FileInfo::default() };
        assert!(matches!(encode_batches(&[big], BatchKind::Files, 64), Err(ManifestError::EntryTooLarge { .. })));
    }

    #[test]
    fn limits_validate_rejects_zero() {
        let base = ManifestLimits::default();
        assert!(base.validate().is_ok());
        assert_eq!(
            ManifestLimits { max_batch_json_size: 0, ..base }.validate(),
            Err(ManifestError::BatchLimitNotPositive)
        );
        assert_eq!(
            ManifestLimits { max_manifest_json_size: 0, ..base }.validate(),
            Err(ManifestError::MetadataLimitNotPositive)
        );
    }

    #[test]
    fn a_manifest_round_trips_through_the_three_messages() {
        let original = sender_info(
            (0..7).map(|index| info(&format!("f{index}"), index)).collect(),
            vec![FileInfo { name: "empty".into(), mode: 1 << 31, ..FileInfo::default() }],
        );
        let limits = ManifestLimits { max_batch_json_size: 13 + 3 * 40, ..ManifestLimits::default() };
        let payloads = encode_manifest(&original, limits).expect("应能拆分");
        assert!(payloads.batches.len() >= 3, "7 个文件 + 1 个空目录应该不止两批");

        let mut incoming = IncomingManifest::start(&payloads.header, limits).expect("头部应能解出");
        assert_eq!(incoming.header().file_count, 7);
        assert_eq!(incoming.header().empty_folder_count, 1);
        assert_eq!(incoming.header().machine_id, "machine");
        for (num, payload) in payloads.batches.iter().enumerate() {
            incoming.batch(len_as_i64(num), payload).expect("批次应能收下");
        }
        let rebuilt = incoming.finish(payloads.batch_count(), &[]).expect("应能收尾");
        assert_eq!(rebuilt, original);
    }

    #[test]
    fn a_manifest_with_no_entries_still_has_a_header_and_an_end() {
        let payloads = encode_manifest(&sender_info(vec![], vec![]), ManifestLimits::default()).expect("应能拆分");
        assert!(payloads.batches.is_empty());
        assert_eq!(payloads.batch_count(), 0);
        let incoming = IncomingManifest::start(&payloads.header, ManifestLimits::default()).expect("头部应能解出");
        assert_eq!(incoming.finish(0, &[]).expect("应能收尾").files_to_transfer.len(), 0);
    }

    #[test]
    fn too_many_entries_are_rejected_before_anything_is_encoded() {
        let limits = ManifestLimits { max_entries: 3, ..ManifestLimits::default() };
        let many_files = sender_info((0..4).map(|index| info(&format!("f{index}"), 0)).collect(), vec![]);
        assert_eq!(
            encode_manifest(&many_files, limits),
            Err(ManifestError::TooManyEntries { files: 4, folders: 0, max: 3 })
        );
        // 文件 2 个 + 空目录 2 个 = 4 > 3，同样拒绝。
        let split = sender_info(
            (0..2).map(|i| info(&format!("f{i}"), 0)).collect(),
            (0..2).map(|i| info(&format!("d{i}"), 0)).collect(),
        );
        assert_eq!(
            encode_manifest(&split, limits),
            Err(ManifestError::TooManyEntries { files: 2, folders: 2, max: 3 })
        );
    }

    #[test]
    fn a_negative_entry_count_in_the_header_is_rejected() {
        let header = ManifestHeader { file_count: -1, ..ManifestHeader::default() };
        assert_eq!(header.validate(10), Err(ManifestError::NegativeEntryCount));
        let payload = to_json(&header).expect("应能编码");
        assert_eq!(
            IncomingManifest::start(&payload, ManifestLimits::default()).err(),
            Some(ManifestError::NegativeEntryCount)
        );
    }

    #[test]
    fn a_header_claiming_more_than_the_limit_is_rejected() {
        let header = ManifestHeader { file_count: 11, ..ManifestHeader::default() };
        assert_eq!(header.validate(10), Err(ManifestError::HeaderExceedsEntries { max: 10 }));
        // `usize` 下溢防护：`files > max` 时不能去算 `max - files`。
        let huge = ManifestHeader { file_count: i64::MAX, ..ManifestHeader::default() };
        assert_eq!(huge.validate(10), Err(ManifestError::HeaderExceedsEntries { max: 10 }));
    }

    #[test]
    fn batches_must_arrive_in_order() {
        let payloads =
            encode_manifest(&sender_info(vec![info("a", 1)], vec![]), ManifestLimits::default()).expect("应能拆分");
        let mut incoming = IncomingManifest::start(&payloads.header, ManifestLimits::default()).expect("头部应能解出");
        assert_eq!(
            incoming.batch(1, &payloads.batches[0]),
            Err(ManifestError::UnexpectedBatch { actual: 1, expected: 0 })
        );
        incoming.batch(0, &payloads.batches[0]).expect("批次应能收下");
        assert_eq!(incoming.next_batch(), 1);
        assert_eq!(incoming.finish(0, &[]), Err(ManifestError::UnexpectedEnd { actual: 0, expected: 1 }));
    }

    #[test]
    fn a_batch_with_an_unknown_kind_is_rejected() {
        let payload = br#"{"k":"z","e":[{"n":"a","c":false}]}"#;
        let header = ManifestHeader { file_count: 1, ..ManifestHeader::default() };
        let mut incoming = IncomingManifest::start(&to_json(&header).expect("应能编码"), ManifestLimits::default())
            .expect("头部应能解出");
        assert_eq!(incoming.batch(0, payload), Err(ManifestError::UnknownBatchKind("z".into())));
    }

    #[test]
    fn an_empty_batch_is_rejected() {
        let payload = br#"{"k":"f","e":[]}"#;
        let mut incoming =
            IncomingManifest::start(&to_json(&ManifestHeader::default()).expect("应能编码"), ManifestLimits::default())
                .expect("头部应能解出");
        assert_eq!(incoming.batch(0, payload), Err(ManifestError::EmptyBatch));
    }

    #[test]
    fn more_entries_than_declared_is_rejected() {
        let entries: Vec<FileInfo> = (0..3).map(|index| info(&format!("f{index}"), 0)).collect();
        let payload = to_json(&ManifestBatch { kind: "f".into(), entries }).expect("应能编码");
        let header = ManifestHeader { file_count: 2, ..ManifestHeader::default() };
        let mut incoming = IncomingManifest::start(&to_json(&header).expect("应能编码"), ManifestLimits::default())
            .expect("头部应能解出");
        assert_eq!(incoming.batch(0, &payload), Err(ManifestError::MoreThanDeclared { kind: "files" }));
    }

    #[test]
    fn an_incomplete_manifest_is_rejected_at_the_end() {
        let entries = vec![info("a", 1)];
        let payload = to_json(&ManifestBatch { kind: "f".into(), entries }).expect("应能编码");
        let header = ManifestHeader { file_count: 2, ..ManifestHeader::default() };
        let mut incoming = IncomingManifest::start(&to_json(&header).expect("应能编码"), ManifestLimits::default())
            .expect("头部应能解出");
        incoming.batch(0, &payload).expect("批次应能收下");
        assert_eq!(
            incoming.finish(1, &[]),
            Err(ManifestError::Incomplete { files: 1, expected_files: 2, folders: 0, expected_folders: 0 })
        );
    }

    #[test]
    fn an_end_carrying_a_payload_is_rejected() {
        let incoming =
            IncomingManifest::start(&to_json(&ManifestHeader::default()).expect("应能编码"), ManifestLimits::default())
                .expect("头部应能解出");
        assert_eq!(incoming.finish(0, b"oops"), Err(ManifestError::EndHasData));
    }

    #[test]
    fn oversized_payloads_are_rejected_at_the_door() {
        let limits = ManifestLimits { max_batch_json_size: 32, ..ManifestLimits::default() };
        let mut incoming = IncomingManifest::start(&to_json(&ManifestHeader::default()).expect("应能编码"), limits)
            .expect("头部应能解出");
        assert_eq!(incoming.batch(0, &[b'x'; 33]), Err(ManifestError::BatchTooLarge { actual: 33, limit: 32 }));
    }

    #[test]
    fn the_cumulative_metadata_limit_is_enforced() {
        let header = ManifestHeader { file_count: 2, ..ManifestHeader::default() };
        let header_payload = to_json(&header).expect("应能编码");
        let payload = to_json(&ManifestBatch { kind: "f".into(), entries: vec![info("a", 1)] }).expect("应能编码");
        assert!(payload.len() > 8);

        // 头部自己就超限 → 连累加器都建不起来。
        let tiny = ManifestLimits { max_manifest_json_size: 8, ..ManifestLimits::default() };
        assert!(matches!(IncomingManifest::start(&header_payload, tiny), Err(ManifestError::MetadataTooLarge { .. })));

        // 刚好装得下头部，但再多一批就超了。
        let tight = ManifestLimits {
            max_manifest_json_size: u64_from_usize(header_payload.len()) + 8,
            ..ManifestLimits::default()
        };
        let mut incoming = IncomingManifest::start(&header_payload, tight).expect("头部应能解出");
        assert!(matches!(incoming.batch(0, &payload), Err(ManifestError::MetadataTooLarge { .. })));
    }

    #[test]
    fn malformed_json_is_reported_with_context() {
        assert!(matches!(
            IncomingManifest::start(b"not json", ManifestLimits::default()),
            Err(ManifestError::BadHeader(_))
        ));
        let mut incoming =
            IncomingManifest::start(&to_json(&ManifestHeader::default()).expect("应能编码"), ManifestLimits::default())
                .expect("头部应能解出");
        assert!(matches!(incoming.batch(0, b"not json"), Err(ManifestError::BadBatch(_))));
    }
}
