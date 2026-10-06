//! croc 的线路层：`comm` 帧、消息编解码、压缩与 AEAD。
//!
//! 这一层是 **sans-io** 的：没有任何 `TcpStream`、没有 `tokio`、没有 `std::fs`、
//! 没有 `Instant::now`。所有函数要么是纯函数（`encode_frame` / `decode_frame` /
//! `compress` / `decompress`），要么把不确定性显式收进参数（AEAD 的 nonce、压缩后的
//! 密钥）。读写字节流、管理缓冲区、计时都由 `croc-engine` 负责。
//!
//! # 一个消息在线上长什么样
//!
//! ```text
//! ┌─ comm 帧 ────────────────────────────────────────────────────────────────┐
//! │ "croc" (4B) │ u32 LE 长度 │ 消息体                                       │
//! └──────────────────────────────────────────────────────────────────────────┘
//!                              │
//!                              ├─ 未加密阶段（PAKE）：DEFLATE(JSON)
//!                              └─ 加密阶段：AES-256-GCM 密钥来自 pakekey
//!                                 payload = nonce(12) ‖ ciphertext ‖ tag(16)
//!                                 plaintext = DEFLATE(JSON(Message))
//! ```
//!
//! JSON 那一步是 Go `encoding/json` 对 `message.Message` 的编码：
//!
//! ```json
//! {"t":"pake","v":2,"m":"...","b":"<base64>","b2":"<base64>","n":0,"f":["..."]}
//! ```
//!
//! 所有字段都带 `omitempty`，所以空字段根本不出现。`b`/`b2` 是 Go 的 `[]byte`，
//! 在 JSON 里是**标准 base64（带 padding）**——这也是为什么本 crate 手写这两个字段
//! 的 serde 适配器，而不是直接给 `Vec<u8>` 派生。
//!
//! # 与 Go 版的差异
//!
//! 只有一处：压缩级别。Go 用 `flate.HuffmanOnly`（只做霍夫曼编码，不做 LZ77 匹配），
//! 为的是省 CPU。DEFLATE 是自描述格式，**解码端与压缩级别无关**，所以 Rust 侧用
//! `Compression::fast()` 的产物 Go 一样能解开，反之亦然。差异只体现在线上字节数，
//! 不影响互通。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod codec;
pub mod features;
pub mod frame;
pub mod json;
pub mod manifest;
pub mod message;
pub mod payload;
pub mod random;
pub mod relay;

pub use codec::{AEAD_NONCE_LEN, ENCRYPTION_KEY_LEN, compress, decompress, decrypt, encrypt};
pub use features::{Feature, TailcatDeclaration, declare, known, supports};
pub use frame::{HEADER_LEN, KEEPALIVE, MAGIC, MAX_MESSAGE_SIZE, decode_frame, encode_frame, is_keepalive};
pub use json::ModTime;
pub use manifest::{
    BatchKind, IncomingManifest, ManifestBatch, ManifestError, ManifestHeader, ManifestLimits, ManifestPayloads,
    encode_batches, encode_manifest, for_wire, for_wire_all,
};
pub use message::{
    MAX_JSON_SIZE, Message, MessageType, SessionKey, decode_encrypted, decode_encrypted_with_limit, decode_plain,
    decode_plain_with_limit, encode_encrypted, encode_encrypted_with_limit, encode_plain, encode_plain_with_limit,
};
pub use payload::{Chunk, FileInfo, RemoteFileRequest, SenderInfo};
pub use random::Randomness;
pub use relay::{
    Admission, ClientHandshake, ClientSession, ClientStep, RelayError, RelayKey, ServerHandshake, ServerOutcome,
    ServerSession, ServerStep,
};

/// 线路层的失败模式。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolError {
    /// 帧头的前 4 个字节不是 `croc`。
    #[error("frame does not start with the croc magic bytes: {0:02x?}")]
    BadMagic([u8; 4]),

    /// 帧体超过了 [`MAX_MESSAGE_SIZE`]，或者接收方给了更小的上限。
    #[error("message is too large: {actual} > {limit}")]
    MessageTooLarge {
        /// 声明或实际的长度。
        actual: usize,
        /// 允许的上限。
        limit: usize,
    },

    /// 解压后的 JSON 超过了上限。防的是压缩炸弹。
    #[error("decompressed message is too large: {actual} > {limit}")]
    DecompressedTooLarge {
        /// 实际解出的字节数（可能被上限截断）。
        actual: usize,
        /// 允许的上限。
        limit: usize,
    },

    /// DEFLATE 流损坏。
    #[error("compression error: {0}")]
    Compression(String),

    /// JSON 不是合法的 `message.Message`。
    #[error("message JSON is malformed: {0}")]
    MalformedJson(String),

    /// 密钥长度不对（AES-256-GCM 要 32 字节）。
    #[error("encryption key must be {expected} bytes, got {actual}")]
    InvalidKeyLength {
        /// 期望长度。
        expected: usize,
        /// 实际长度。
        actual: usize,
    },

    /// AEAD 校验失败：口令不对，或消息被篡改。
    ///
    /// Go 版在 `crypt.Decrypt` 里把这个情况报成 `"incorrect passphrase"`，croc 上层
    /// 靠错误文本里的 `"bad password"` 分流——Rust 侧用类型而不是字符串。
    #[error("message authentication failed (wrong password or tampered message)")]
    AuthenticationFailed,

    /// 密文太短，连 nonce 加 tag 都不够。
    #[error("ciphertext is too short: {0} bytes")]
    CiphertextTooShort(usize),

    /// 会话还是明文状态，却出现了非 PAKE 消息。
    ///
    /// 对应 Go 收包时的 `"unencrypted communication rejected"`。`kind` 是消息类型名，
    /// 空字符串表示消息根本没有 `t` 字段。
    #[error("unencrypted communication rejected: {kind:?} may only travel over a secured channel")]
    PlaintextRejected {
        /// 触发拒绝的消息类型名。
        kind: String,
    },
}
