//! `croc-rs relay`：自建中继。
//!
//! 参数与 Go 版逐个对齐（`--host` / `--ports` / `--port` / `--transfers` /
//! `--max-rooms-open` / `--max-pending-handshakes` / `--handshake-timeout` /
//! `--source-join-limit` / `--room-join-limit` / `--join-limit-window`），
//! 因为**中继是要被 Go 客户端连的**，flag 名对不上就等于自建中继没法用。
//!
//! # banner 就是端口列表
//!
//! 客户端从握手 banner 里读数据端口（`strings.Split(banner, ",")`），所以这里必须把
//! `--ports` 原样写进 [`croc_relay::RelayConfig::with_banner`]。忘了这一步的症状是
//! "客户端连上了但一个文件都传不动"——它不知道数据端口在哪。

use std::process::ExitCode;
use std::sync::Arc;

use clap::Args;
use croc_relay::{Relay, RelayConfig};
use tokio_util::sync::CancellationToken;

/// 默认端口列表，与 Go 的 `--ports` 默认值一致。
const DEFAULT_PORTS: &str = "9009,9010,9011,9012,9013";

#[derive(Debug, Clone, Args)]
pub struct RelayArgs {
    /// 监听地址。空 = 所有网卡。
    #[arg(long, default_value = "")]
    host: String,

    /// 端口列表（逗号分隔）。第一个是控制端口，其余是数据端口。
    #[arg(long, default_value = DEFAULT_PORTS, env = "CROC_PORTS")]
    ports: String,

    /// 基准端口。只在没显式给 `--ports` 时生效（与 Go 的 `if c.IsSet("ports")` 一致）。
    #[arg(long, default_value_t = 9009, env = "CROC_PORT")]
    port: u16,

    /// 从 `--port` 起连续开几个端口。
    #[arg(long, default_value_t = 5)]
    transfers: u16,

    /// 每个端口最多同时等几个房间。
    #[arg(long, default_value_t = croc_relay::config::DEFAULT_MAX_ROOMS_OPEN, env = "CROC_MAX_ROOMS_OPEN")]
    max_rooms_open: usize,

    /// 每个端口最多同时有几个没握完手的连接。
    #[arg(
        long,
        default_value_t = croc_relay::config::DEFAULT_MAX_PENDING_HANDSHAKES,
        env = "CROC_MAX_PENDING_HANDSHAKES"
    )]
    max_pending_handshakes: usize,

    /// 单次握手的超时。Go 风格的时长，如 `5m` / `300s`。
    #[arg(long, default_value = "5m0s", env = "CROC_HANDSHAKE_TIMEOUT")]
    handshake_timeout: String,

    /// 准入窗口内单个来源地址最多加入几次。
    #[arg(long, default_value_t = croc_relay::config::DEFAULT_SOURCE_JOIN_LIMIT, env = "CROC_SOURCE_JOIN_LIMIT")]
    source_join_limit: usize,

    /// 准入窗口内单个房间最多被加入几次。
    #[arg(long, default_value_t = croc_relay::config::DEFAULT_ROOM_JOIN_LIMIT, env = "CROC_ROOM_JOIN_LIMIT")]
    room_join_limit: usize,

    /// 准入限流的滑动窗口。Go 风格的时长，如 `1m` / `60s`。
    #[arg(long, default_value = "1m0s", env = "CROC_JOIN_LIMIT_WINDOW")]
    join_limit_window: String,
}

impl RelayArgs {
    /// 端口列表。
    ///
    /// 与 Go 一致：显式给了 `--ports` 就用它，否则从 `--port` 起连续 `--transfers` 个。
    #[must_use]
    pub fn resolved_ports(&self, ports_explicit: bool) -> Vec<u16> {
        if ports_explicit {
            return self
                .ports
                .split(',')
                .filter_map(|piece| piece.trim().parse::<u16>().ok())
                .filter(|port| *port > 0)
                .collect();
        }
        (0..self.transfers.max(1)).filter_map(|offset| self.port.checked_add(offset)).collect()
    }

    /// 中继配置。
    ///
    /// # Errors
    ///
    /// 两个时长开关不是 Go 风格的时长时返回原因。
    pub fn config(&self, password: &str, ports: &[u16]) -> Result<RelayConfig, String> {
        let handshake_timeout = crate::duration::parse(&self.handshake_timeout)?;
        let join_limit_window = crate::duration::parse(&self.join_limit_window)?;
        let banner = ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
        Ok(RelayConfig::new()
            .with_password(password)
            .with_banner(banner)
            .with_max_rooms_open(self.max_rooms_open)
            .with_max_pending_handshakes(self.max_pending_handshakes)
            .with_handshake_timeout(handshake_timeout)
            .with_admission_limits(self.source_join_limit, self.room_join_limit, join_limit_window))
    }
}

/// 跑起来。
///
/// `password` 由调用方从全局 `--pass` 传进来（它已经解析过 `CROC_PASS` 环境变量与默认值）。
/// **不要在这里再读一次环境变量**——那样 `croc-rs --pass x relay` 会被悄悄忽略，
/// 症状是对端一律 `bad password`。
///
/// `ports_explicit` 是"命令行上给了 `--ports` 吗"，决定端口从 `--ports` 来还是从
/// `--port` + `--transfers` 推（与 Go 的 `c.IsSet("ports")` 一致）。
pub async fn run(args: RelayArgs, password: &str, ports_explicit: bool, cancel: CancellationToken) -> ExitCode {
    let ports = args.resolved_ports(ports_explicit);
    if ports.len() < 2 {
        eprintln!("croc-rs relay: 至少要两个端口（一个控制 + 至少一个数据）");
        return ExitCode::FAILURE;
    }

    let config = match args.config(password, &ports) {
        Ok(config) => config,
        Err(reason) => {
            eprintln!("croc-rs relay: {reason}");
            return ExitCode::FAILURE;
        }
    };
    let relay = Arc::new(Relay::new(config));

    let host = if args.host.is_empty() { "0.0.0.0" } else { args.host.as_str() };
    let mut listeners = Vec::with_capacity(ports.len());
    for port in &ports {
        match Relay::bind((host, *port)).await {
            Ok(listener) => listeners.push(listener),
            Err(error) => {
                eprintln!("croc-rs relay: 绑不上 {host}:{port}：{error}");
                return ExitCode::FAILURE;
            }
        }
    }
    println!(
        "croc-rs relay 已启动：{}",
        ports.iter().map(|port| format!("{host}:{port}")).collect::<Vec<_>>().join(", ")
    );

    match relay.run(listeners, cancel).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("croc-rs relay: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args() -> RelayArgs {
        RelayArgs {
            host: String::new(),
            ports: DEFAULT_PORTS.to_owned(),
            port: 9009,
            transfers: 5,
            max_rooms_open: 128,
            max_pending_handshakes: 64,
            handshake_timeout: "5m0s".to_owned(),
            source_join_limit: 30,
            room_join_limit: 30,
            join_limit_window: "1m0s".to_owned(),
        }
    }

    #[test]
    fn the_default_port_list_matches_go() {
        assert_eq!(args().resolved_ports(false), vec![9009, 9010, 9011, 9012, 9013]);
    }

    #[test]
    fn an_explicit_ports_list_wins_over_port_and_transfers() {
        // 与 Go 的 `if c.IsSet("ports")` 一致：`--ports` 给了就完全按它来。
        let args = RelayArgs { ports: "7000,7001".to_owned(), ..args() };
        assert_eq!(args.resolved_ports(true), vec![7000, 7001]);
        // 没给的话，`--port` 只是默认值的来源，仍然走 `--ports`。
        assert_eq!(args.resolved_ports(false), vec![9009, 9010, 9011, 9012, 9013]);
    }

    #[test]
    fn port_plus_transfers_expands_to_a_range() {
        let args = RelayArgs { port: 7000, transfers: 3, ..args() };
        assert_eq!(args.resolved_ports(false), vec![7000, 7001, 7002]);
    }

    #[test]
    fn garbage_ports_are_dropped_and_a_short_list_is_rejected() {
        let args = RelayArgs { ports: "9009,,oops,0".to_owned(), ..args() };
        assert_eq!(args.resolved_ports(true), vec![9009]);
        assert!(args.resolved_ports(true).len() < 2, "调用方会据此拒绝启动");
    }

    #[test]
    fn the_banner_carries_the_port_list() {
        // 客户端靠 banner 找数据端口；这一步错了就是"连得上但传不动"。
        let ports = vec![9009u16, 9010, 9011];
        let config = args().config("secret", &ports).expect("默认值是合法的 Go 时长");
        assert_eq!(config.banner(), "9009,9010,9011");
        assert_eq!(config.password(), "secret");
    }

    #[test]
    fn the_password_comes_from_the_caller_not_the_environment() {
        // 曾经的 bug：这里自己读 `CROC_PASS`，于是 `croc-rs --pass x relay` 被忽略，
        // 对端一律 `bad password`。现在密码是入参，`args.config()` 只做搬运。
        let config = args().config("from-the-flag", &[9009, 9010]).expect("默认值是合法的 Go 时长");
        assert_eq!(config.password(), "from-the-flag");
    }

    #[test]
    fn a_bad_duration_is_rejected_before_binding_anything() {
        let args = RelayArgs { handshake_timeout: "5 minutes".to_owned(), ..args() };
        assert!(args.config("secret", &[9009, 9010]).is_err());
    }
}
