//! 引擎的失败模式。
//!
//! 与 `croc-core::SessionError` 的分工：那个管**协议违规**（消息顺序、确认标签），
//! 这个管**副作用失败**（连不上、读不到文件、磁盘满了）。两者最终都会被
//! `croc-cli` 折成一个退出码 + 一行脱敏过的消息。

use croc_core::SessionError;

/// 一次传输失败的原因。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum EngineError {
    /// 中继连不上，或者握手期间被断开。
    #[error("relay unreachable: {address}（{reason}）")]
    RelayUnreachable {
        /// 目标地址。
        address: String,
        /// 底层原因。
        reason: String,
    },

    /// 中继层面的拒绝或异常。
    #[error("relay error: {0}")]
    Relay(String),

    /// 中继地址写得不对。
    #[error("invalid relay address {0:?}: expected host:port（IPv6 要写成 [::1]:9009）")]
    BadRelayAddress(String),

    /// 文件系统出错。
    #[error("{path}: {reason}")]
    File {
        /// 出问题的路径。
        path: String,
        /// 底层原因。
        reason: String,
    },

    /// 线路或 IO 出错。
    #[error("io error: {0}")]
    Io(String),

    /// 协议层失败。
    #[error(transparent)]
    Session(#[from] SessionError),

    /// 对端拒绝了这次传输。
    #[error("the peer refused the transfer")]
    Refused,

    /// 被取消。
    #[error("cancelled")]
    Cancelled,

    /// 这个哈希算法本实现还不支持。
    #[error("unsupported hash algorithm {0:?}（目前只支持 xxhash）")]
    UnsupportedHash(String),

    /// 配置/参数不合法。
    #[error("{0}")]
    Config(String),
}

impl EngineError {
    /// 造一个文件错误。
    pub fn file(path: impl AsRef<std::path::Path>, error: impl std::fmt::Display) -> Self {
        Self::File { path: path.as_ref().display().to_string(), reason: error.to_string() }
    }

    /// 这个错误是否值得重试（目前只有"连不上中继"算）。
    #[must_use]
    pub const fn is_retryable(&self) -> bool {
        matches!(self, Self::RelayUnreachable { .. })
    }
}

impl From<std::io::Error> for EngineError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<croc_net::CrocIoError> for EngineError {
    fn from(error: croc_net::CrocIoError) -> Self {
        Self::Io(error.to_string())
    }
}

impl From<croc_protocol::ProtocolError> for EngineError {
    fn from(error: croc_protocol::ProtocolError) -> Self {
        Self::Session(SessionError::Protocol(error))
    }
}

impl From<croc_protocol::manifest::ManifestError> for EngineError {
    fn from(error: croc_protocol::manifest::ManifestError) -> Self {
        Self::Session(SessionError::Manifest(error))
    }
}

impl From<croc_pake::PakeError> for EngineError {
    fn from(error: croc_pake::PakeError) -> Self {
        Self::Session(SessionError::Pake(error))
    }
}

/// 把 `SessionError` 里"对端拒绝"这一种翻成 [`EngineError::Refused`]，
/// 其余原样透传——调用方要能一眼看出"是对方不要"还是"协议炸了"。
impl From<croc_core::Outcome> for EngineError {
    fn from(outcome: croc_core::Outcome) -> Self {
        match outcome {
            croc_core::Outcome::Completed => Self::Config("internal: completed is not an error".to_owned()),
            croc_core::Outcome::Refused | croc_core::Outcome::Declined => Self::Refused,
            croc_core::Outcome::Cancelled => Self::Cancelled,
            croc_core::Outcome::Failed(reason) => Self::Session(SessionError::BadConfig(reason)),
            // `Outcome` 是 `#[non_exhaustive]`：将来新增的结论一律当失败处理，
            // 总比让编译器逼着每个 match 都去改一遍好。
            other => Self::Config(format!("unexpected outcome: {other:?}")),
        }
    }
}
