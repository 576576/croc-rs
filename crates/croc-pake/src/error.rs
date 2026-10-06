//! `croc-pake` 的失败模式。
//!
//! 全部都是**对方输入不可信**导致的，没有"我们自己的 bug"这一类：曲线参数是编译期
//! 常量，随机数由调用方提供。所以每个变体都能直接映射到 croc 的 `pakeHandshakeError`。

/// PAKE 握手中可以出现的错误。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PakeError {
    /// 曲线名不在 Go 版 `pake.AvailableCurves()` 里。
    #[error("unknown curve name {0:?}")]
    UnknownCurve(String),

    /// role 只能是 0（A 方）或 1（B 方）。
    #[error("role must be 0 (party A) or 1 (party B)")]
    InvalidRole,

    /// 对方声明的 role 与我们相同。Go 版 `Update` 里同样拒绝这种情况。
    #[error("peer advertised our own role")]
    RoleConflict,

    /// 参与者身份不能为空。
    #[error("both participant identities are required")]
    MissingIdentity,

    /// 对方的公开值不是合法 JSON，或结构不符合 `pake.Pake` 的公开字段。
    #[error("peer public value is not a well-formed pake public value")]
    MalformedWire,

    /// 对方的公开值缺少某个必需坐标。
    #[error("peer public value is missing the {0} coordinate")]
    MissingCoordinate(&'static str),

    /// 对方送来的点不在曲线上。
    #[error("peer point {0} is not on the curve")]
    PeerPointNotOnCurve(&'static str),

    /// 本次交换已经派生过会话密钥。Go 版靠 `HaveSessionKey()` 在调用方检查，
    /// 这里把它前移到类型内部，避免重复 `Update` 造成状态不一致。
    #[error("a session key has already been derived for this exchange")]
    AlreadyDerived,

    /// 还没有派生会话密钥。
    #[error("no session key has been derived yet")]
    NoSessionKey,
}
