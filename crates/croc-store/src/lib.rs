//! `croc-store`：croc 的「暂存传输」（stored transfer）客户端。
//!
//! 协议名 `croc-store-v1`，分两层：
//!
//! - 加密格式（[`crypto`] / [`manifest`]）：服务端只见到密文。清单与数据分片各用一条从主密钥
//!   派生出来的密钥做 AES-256-GCM，分片的位置信息进 AAD，所以服务端既看不到文件名与内容，
//!   也不能调换分片顺序、或把一次传输的分片挪到另一次；
//! - HTTP 接口（[`client`]）：7 个端点，见该模块的说明。
//!
//! 收件方拿到的是 [`share::Share`]（来源 + 对象 ID + 主密钥），由 [`share::Share::browser_url`]
//! 编成浏览器链接，或由 [`share::Share::cli_token`] 编成命令行用的 token。

#![warn(missing_docs)]
// 测试代码里 unwrap / expect / panic / 索引都是正常表达方式，与各业务 crate 的做法一致。
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

mod base64url;
mod http;

pub mod client;
pub mod crypto;
pub mod expiration;
pub mod manifest;
pub mod proxy;
pub mod share;

pub use expiration::parse_expiration;

/// 协议名。同时用作 CLI token 的前缀与 HKDF 的 info 前缀。
pub const PROTOCOL: &str = "croc-store-v1";

/// 清单里的格式版本号。
pub const VERSION: u32 = 1;

/// 分片大小（明文）。固定 4 MiB，清单里也带这个值，两端必须一致。
pub const CHUNK_SIZE: i64 = 4 * 1024 * 1024;

/// 主密钥长度。HKDF 与 AES-256 都用它。
pub const KEY_SIZE: usize = 32;

/// 传输 ID 的字节数。公开值，不是密钥。
pub const TRANSFER_ID_LEN: usize = 16;

/// 一次传输最多包含的文件数。
pub const MAX_FILES: usize = 100;

/// 清单 JSON 的字节上限。清单里只有文件名与哈希，正常远小于这个值。
pub const MAX_MANIFEST_BYTES: i64 = 1 << 40;

/// 这一层的错误。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 格式不合法：长度不对、版本不认识、清单字段自相矛盾。
    #[error("stored transfer format: {0}")]
    Format(String),
    /// 有效期的写法不合法。消息原样透出，不加前缀——它要跟 Go 的
    /// `storeapi.ParseExpiration` 逐字一致。
    #[error("{0}")]
    Expiration(String),
    /// 认证失败。密文被改过，或者密钥不对。
    #[error("stored transfer authentication failed")]
    Authentication,
    /// 取操作系统随机数失败。
    #[error("stored transfer randomness: {0}")]
    Random(String),
    /// 服务端返回了非 2xx。
    #[error("stored transfer service returned HTTP {status}")]
    Service {
        /// HTTP 状态码。
        status: u16,
        /// 服务端给的消息体，可能是空的。
        message: String,
        /// 响应里的 `Retry-After`，可能没有。
        retry_after: Option<String>,
    },
    /// 本地文件或目录的读写失败。
    #[error("stored transfer io: {0}")]
    Io(#[from] std::io::Error),
    /// JSON 编解码失败。
    #[error("stored transfer json: {0}")]
    Json(#[from] serde_json::Error),
    /// HTTP 传输层失败：连不上、TLS 握手失败、报文不合法。
    #[error("stored transfer transport: {0}")]
    Transport(String),
}
