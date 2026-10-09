//! croc 中继服务端：与 Go 版 `croc relay` 线路兼容的自建中继。
//!
//! # 它做什么
//!
//! 两台机器要传文件，但互相不可达。它们各自连到中继，报上同一个房间名，中继把
//! 两条连接对接起来，之后只是双向转发字节——内容早已端到端加密，中继看不到文件名，
//! 也看不到数据。
//!
//! # 它怎么和 Go 版保持一致
//!
//! 握手逻辑全部在 [`croc_protocol::relay`] 里，是一对 sans-io 状态机；本 crate 只负责
//! IO：accept、超时、并发闸门、限流、房间表、转发。这样"协议对不对"和"服务器写得对不对"
//! 是两件可以分别验证的事——前者有单元测试逐帧核对，后者有端到端测试。
//!
//! # 端口
//!
//! 默认 `9009` 是控制端口，`9010..=9013` 是数据端口。数据端口跑的是同一套握手，
//! 区别只在客户端怎么用它：控制端口走 PAKE 协商，数据端口拿到房间后就传文件。
//! 本 crate 不区分端口语义，同一份 [`Relay`] 可以同时挂在所有端口上。
//!
//! # 用法
//!
//! ```no_run
//! # async fn run() -> std::io::Result<()> {
//! use croc_relay::{Relay, RelayConfig};
//! use std::sync::Arc;
//! use tokio_util::sync::CancellationToken;
//!
//! let relay = Arc::new(Relay::new(RelayConfig::new().with_password("secret")));
//! let listeners = vec![Relay::bind("0.0.0.0:9009").await?];
//! relay.run(listeners, CancellationToken::new()).await?;
//! # Ok(())
//! # }
//! ```
//!
//! # 与 Go 版的两处有意的省略
//!
//! - "快速准入"能力：Go 的升级版中继会给客户端签发一个绑定源地址的 HMAC 令牌，
//!   让它在数据端口跳过 PAKE。这是纯优化，线格式完全兼容——我们既不签发也不请求。
//! - 房间协议观测：Go 可以在配对后偷看第一条帧，识别出"这是 SSH 共享会话"用于
//!   遥测。Go 的默认配置同样是关着的（`roomProtocol == nil`），所以这不是兼容性缺口。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod admission;
pub mod config;
pub mod room;
pub mod server;

pub use admission::{AdmissionLimiter, canonical_source};
pub use config::RelayConfig;
pub use room::{RoomJoin, RoomRegistry};
pub use server::Relay;

/// 默认控制端口。
pub const DEFAULT_CONTROL_PORT: u16 = 9009;

/// 默认数据端口。
pub const DEFAULT_DATA_PORTS: [u16; 4] = [9010, 9011, 9012, 9013];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_ports_match_the_go_relay() {
        assert_eq!(DEFAULT_CONTROL_PORT, 9009);
        assert_eq!(DEFAULT_DATA_PORTS, [9010, 9011, 9012, 9013]);
    }
}
