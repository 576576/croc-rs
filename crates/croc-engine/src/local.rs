//! 局域网直连（`--local`）：内建中继 + UDP 组播发现。
//!
//! # 协议（照 Go 版）
//!
//! 发送方起一个**只活在这次传输里**的中继，然后通过 UDP 组播把自己的控制端口喊出去：
//!
//! | 角色 | 组播载荷 | 说明 |
//! |---|---|---|
//! | 发送方 | `croc` + 控制端口号 | 例如 `croc49123`；不带 `--local` 时喊 30 秒就停 |
//! | 接收方 | `ok` | 只喊 200 ms，只为让对端看见自己 |
//!
//! 组播组与端口照 Go 的默认：`239.255.255.250:9999`（IPv4）、`[ff02::c]:9999`（IPv6）。
//! 载荷是**明文**且没有任何认证——它只能用来"知道去哪个地址连"，真正的机密性由后续
//! 的 PAKE 提供。所以解析结果一律当不可信输入：只取 `croc` 后面的数字。
//!
//! # 为什么中继要监听 `0.0.0.0`
//!
//! 发送方自己连的是 `127.0.0.1:<控制端口>`，但对端连的是**发送方的局域网 IP** + 同一个
//! 端口。绑到回环上，对端就永远连不进来。

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::error::EngineError;

/// 组播组的默认值，对应 Go `--multicast` 的默认（`src/cli/cli.go:209`）。
///
/// 可以用 `--multicast` 改掉，所以它只是默认值而不是唯一可选值。
pub const MULTICAST_GROUP_V4: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);

/// 组播端口。Go 用的 `peerdiscovery` 默认值是 9999。
pub const MULTICAST_PORT: u16 = 9999;

/// GUI 起内建中继时默认开几条数据端口（等价 `--transfers` 的默认值 4）。
///
/// 取中继的默认数据端口数，让 `--local` 与自建中继的并行流数一致。
pub const LOCAL_DATA_PORTS: usize = croc_relay::DEFAULT_DATA_PORTS.len();

/// `--port` 给了之后往后扫多少个端口位找空位。
///
/// 与 Go 的 `FindOpenPorts` 同值（`src/utils/utils.go:642`，那里也是扫 200 个位置）。
const PORT_SEARCH_LIMIT: usize = 200;

/// 发送方广播的前缀。对端靠它认出"这是 croc 在喊，后面跟着端口"。
const BEACON_PREFIX: &str = "croc";

/// 接收方广播的载荷——Go 的 `discoverReceivePeers()` 发的是裸 `ok`（`croc.go:1973`）。
const PROBE_PAYLOAD: &str = "ok";

/// 广播间隔。Go 用 20 ms（`croc.go:1447`）。
const BEACON_INTERVAL: Duration = Duration::from_millis(20);

/// 不带 `--local` 时广播多久就停（Go 的 `croc.go:1440`）。
const BEACON_LIMIT: Duration = Duration::from_secs(30);

/// 接收方等多久的广播（Go 的 `receivePeerDiscoveryTimeout`，`croc.go:1935`）。
pub const DISCOVERY_TIMEOUT: Duration = Duration::from_millis(500);

/// 组播发现到的一个对端。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Discovered {
    /// 对端的地址（组播包的源地址）。
    pub address: SocketAddr,
    /// 对端喊出来的控制端口号。
    pub control_port: u16,
}

impl Discovered {
    /// 拼出该连的中继地址（`host:port`）。
    #[must_use]
    pub fn relay_address(&self) -> String {
        format!("{}:{}", self.address.ip(), self.control_port)
    }
}

/// 一个只在本次传输期间存活的中继。
///
/// 析构（或 [`Self::shutdown`]）会停掉它——`--local` 的语义是"不碰公网中继"，
/// 这东西绝不能比这次传输活得更久。
pub struct LocalRelay {
    /// 控制端口。
    control_port: u16,
    /// 数据端口，与中继 banner 一致。
    data_ports: Vec<u16>,
    /// 组播广播的停止开关。广播不该比中继活得更久——对端已经连上了还在喊，
    /// 只会招来别的接收方。
    beacon: CancellationToken,
    shutdown: CancellationToken,
    server: Option<JoinHandle<()>>,
}

impl LocalRelay {
    /// 控制端口。
    #[must_use]
    pub const fn control_port(&self) -> u16 {
        self.control_port
    }

    /// 数据端口。
    #[must_use]
    pub fn data_ports(&self) -> &[u16] {
        &self.data_ports
    }

    /// 本端自己该连的地址（回环）。
    #[must_use]
    pub fn loopback_address(&self) -> String {
        format!("127.0.0.1:{}", self.control_port)
    }

    /// 停掉它。不 await 那个任务——它要等所有连接自然结束，而这里只负责发信号。
    pub fn shutdown(&mut self) {
        self.beacon.cancel();
        self.shutdown.cancel();
        self.server = None;
    }
}

impl Drop for LocalRelay {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// 绑一个端口：`advance` 为真时从 `*candidate` 起往后找，否则交给内核分配。
///
/// 找端口时**上限是 [`PORT_SEARCH_LIMIT`] 个**：一段端口全被占满时不该在这里转一整天，
/// 报个错让用户换个 `--port` 更好。
async fn bind_next(candidate: &mut u16, advance: bool) -> Result<(TcpListener, u16), EngineError> {
    if !advance {
        let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0))
            .await
            .map_err(|error| EngineError::Relay(format!("cannot bind a local relay port: {error}")))?;
        let port = listener.local_addr().map_err(|error| EngineError::Relay(error.to_string()))?.port();
        return Ok((listener, port));
    }
    let start = *candidate;
    for _ in 0..PORT_SEARCH_LIMIT {
        let port = *candidate;
        if let Ok(listener) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, port)).await {
            *candidate = candidate.saturating_add(1);
            return Ok((listener, port));
        }
        *candidate =
            candidate.checked_add(1).ok_or_else(|| EngineError::Relay("no free port left above --port".to_owned()))?;
    }
    Err(EngineError::Relay(format!("no free port in the {PORT_SEARCH_LIMIT} ports above {start}")))
}

/// 起一个内建中继。
///
/// `base_port` 为 `None` 时端口由内核分配（`0.0.0.0:0`），不会跟机器上别的服务抢——
/// 代价是每次传输的端口都不同，这也正是要靠组播把端口喊出去的原因。
///
/// 给了 `base_port`（`--port`）就从它开始往后找：Go 的 `--port` 是"基准端口"而不是
/// 硬要求（`utils.FindOpenPorts`，`croc.go:1404`），被占了就顺延。硬绑一个被占的端口
/// 而报错，会让"--port 9009"在跑过一次之后就再也用不了。
///
/// # Errors
///
/// 端口绑不上，或者数据端口不够 `streams` 个。
pub async fn start_local_relay(
    password: &str,
    streams: usize,
    base_port: Option<u16>,
) -> Result<LocalRelay, EngineError> {
    // 控制端口 + `streams` 个数据端口。`streams` 就是 `--transfers`，Go 那边端口数组
    // 的长度同样是 `transfers + 1`（`src/cli/cli.go:642`）。
    let count = streams + 1;
    let mut listeners = Vec::with_capacity(count);
    let mut ports = Vec::with_capacity(count);
    let mut candidate = base_port.unwrap_or(0);
    for _ in 0..count {
        let (listener, port) = bind_next(&mut candidate, base_port.is_some()).await?;
        ports.push(port);
        listeners.push(listener);
    }

    // banner 只列数据端口——控制端口已经在地址里了，与 Go 的 `RelayPorts` 一致。
    let data_ports = ports.get(1..).unwrap_or_default();
    let banner = data_ports.iter().map(u16::to_string).collect::<Vec<_>>().join(",");
    let config = croc_relay::RelayConfig::new().with_banner(banner).with_password(password);
    let relay = Arc::new(croc_relay::Relay::new(config));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn({
        let relay = Arc::clone(&relay);
        let shutdown = shutdown.clone();
        async move {
            if let Err(error) = relay.run(listeners, shutdown).await {
                warn!(%error, "the local relay stopped with an error");
            }
        }
    });

    let control_port = ports.first().copied().unwrap_or_default();
    Ok(LocalRelay {
        control_port,
        data_ports: data_ports.to_vec(),
        beacon: CancellationToken::new(),
        shutdown,
        server: Some(task),
    })
}

/// 把自己的控制端口喊出去，直到 `stop` 被取消或超过 [`BEACON_LIMIT`]。
///
/// 这跑在后台任务里，调用方不用 await 它。
pub fn advertise(control_port: u16, group: Ipv4Addr, stop: CancellationToken) {
    tokio::spawn(async move {
        let payload = format!("{BEACON_PREFIX}{control_port}").into_bytes();
        let socket = match beacon_socket() {
            Ok(socket) => socket,
            Err(error) => {
                warn!(%error, "cannot open a multicast socket, --local discovery is off");
                return;
            }
        };
        let deadline = tokio::time::Instant::now() + BEACON_LIMIT;
        loop {
            if stop.is_cancelled() || tokio::time::Instant::now() >= deadline {
                return;
            }
            // 组播发出去失败（网卡没起来、权限不够）不该让传输失败——它只是"可能
            // 找不到对端"，而"找不到"和"发不出去"在用户看来是同一件事，等超时即可。
            if let Err(error) = socket.send_to(&payload, (group, MULTICAST_PORT)) {
                warn!(%error, "cannot send the local beacon");
                return;
            }
            tokio::select! {
                () = tokio::time::sleep(BEACON_INTERVAL) => {}
                () = stop.cancelled() => return,
            }
        }
    });
}

/// 在局域网上找一个正在喊的发送方。
///
/// 边听边发 `ok`（Go 的接收方也是这样：`peerdiscovery.Discover` 在听的同时广播自己的
/// 载荷）。等 [`DISCOVERY_TIMEOUT`] 之后把收集到的结果交回去。
///
/// # Errors
///
/// 组播套接字开不起来时返回错误——这时 `--local` 确实做不到，早失败比干等好。
/// 单纯"没找到"则返回空 `Vec`。
pub async fn discover(timeout: Duration, group: Ipv4Addr) -> Result<Vec<Discovered>, EngineError> {
    let socket = listen_socket(group)?;
    // 非阻塞：接收循环要能同时发 `ok` 并在超时后收工。
    socket.set_nonblocking(true).map_err(|error| EngineError::Relay(format!("cannot set nonblocking: {error}")))?;

    let mut found: Vec<Discovered> = Vec::new();
    let mut buffer = [0u8; 128];
    let deadline = tokio::time::Instant::now() + timeout;
    let mut next_probe = tokio::time::Instant::now();

    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Ok(found);
        }

        match socket.recv_from(&mut buffer) {
            Ok((len, from)) => {
                if let Some(payload) = buffer.get(..len).and_then(parse_beacon) {
                    let discovered = Discovered { address: from, control_port: payload };
                    if !found.contains(&discovered) {
                        found.push(discovered);
                    }
                }
            }
            // 非阻塞下"还没数据"就是这个错误码，正常。
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => warn!(%error, "the multicast socket failed"),
        }

        if now >= next_probe {
            // 让对端知道这儿有人在找它；失败不影响"听"这件事。
            let _ = socket.send_to(PROBE_PAYLOAD.as_bytes(), (group, MULTICAST_PORT));
            next_probe = now + BEACON_INTERVAL;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 建一个**只发不收**的组播套接字。
///
/// 绑的是临时端口而不是 [`MULTICAST_PORT`]：组播包按**目的**端口投递，所以发信方
/// 不需要占着 9999；而同一台机器上 sender 与 receiver 常常是同时跑的，两边都去绑
/// 9999 会直接 `EADDRINUSE`（Windows 上没有 `SO_REUSEPORT`）。
fn beacon_socket() -> Result<UdpSocket, EngineError> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .map_err(|error| EngineError::Relay(format!("cannot open a multicast socket: {error}")))?;
    Ok(socket)
}

/// 建一个**收组播**的套接字。
///
/// 必须绑到 [`MULTICAST_PORT`]：组播只投递到加入了该组、且监听着该端口的套接字。
fn listen_socket(group: Ipv4Addr) -> Result<UdpSocket, EngineError> {
    let socket = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, Some(socket2::Protocol::UDP))
        .map_err(|error| EngineError::Relay(format!("cannot open a multicast socket: {error}")))?;
    // `SO_REUSEADDR`：Go 的 peerdiscovery 在**发送方**也占着 9999，同一台机器上跑互通
    // 测试时两边都要收这个端口。不复用就会 `EADDRINUSE`——而失败的一方恰好是接收方，
    // 症状是"找不到对端"，很容易被误判成组播不通。
    socket.set_reuse_address(true).map_err(|error| EngineError::Relay(error.to_string()))?;
    socket
        .bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, MULTICAST_PORT)).into())
        .map_err(|error| EngineError::Relay(format!("cannot bind the multicast port {MULTICAST_PORT}: {error}")))?;
    socket
        .join_multicast_v4(&group, &Ipv4Addr::UNSPECIFIED)
        .map_err(|error| EngineError::Relay(format!("cannot join the multicast group: {error}")))?;
    socket.set_nonblocking(true).map_err(|error| EngineError::Relay(error.to_string()))?;
    Ok(socket.into())
}

/// 从组播载荷里取出端口。**载荷不可信**：只认 `croc` 前缀加纯数字。
fn parse_beacon(payload: &[u8]) -> Option<u16> {
    let text = std::str::from_utf8(payload).ok()?;
    let digits = text.strip_prefix(BEACON_PREFIX)?;
    digits.parse().ok()
}

/// 发送方在 `--local` 下要连的入口。
///
/// 起中继、开始广播，返回"本端该连的地址"。返回的 [`LocalRelay`] 必须被持有到传输结束：
/// 它一析构，中继就停了。
///
/// # Errors
///
/// 中继起不来。
pub async fn announce(
    password: &str,
    streams: usize,
    base_port: Option<u16>,
    group: Ipv4Addr,
    stop: CancellationToken,
) -> Result<LocalRelay, EngineError> {
    let mut relay = start_local_relay(password, streams, base_port).await?;
    // 广播同时受两件事约束：用户取消（`stop`），以及本次传输结束（`relay` 被 drop）。
    relay.beacon = stop.child_token();
    advertise(relay.control_port(), group, relay.beacon.clone());
    Ok(relay)
}

/// 接收方在 `--local` 下要连的中继地址。
///
/// # Errors
///
/// 组播不可用，或者超时也没找到任何发送方。
pub async fn find_peer(timeout: Duration, group: Ipv4Addr) -> Result<String, EngineError> {
    let found = discover(timeout, group).await?;
    let peer = found
        .into_iter()
        // 组播的源地址可能是别的网卡（多网卡机器），但局域网只有一个网段时无所谓；
        // 按发现顺序取第一个，与 Go 的 `discoveries[0]` 一致。
        .next()
        .ok_or_else(|| EngineError::Relay("no sender answered on the local network".to_owned()))?;
    Ok(peer.relay_address())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beacon_parses_a_plain_port() {
        assert_eq!(parse_beacon(b"croc49123"), Some(49123));
    }

    #[test]
    fn beacon_rejects_anything_that_is_not_a_port() {
        // 组播载荷没有认证，恶意/损坏的内容都不能让解析出东西。
        assert_eq!(parse_beacon(b"croc"), None);
        assert_eq!(parse_beacon(b"croc49123junk"), None);
        assert_eq!(parse_beacon(b"croc-1"), None);
        assert_eq!(parse_beacon(b"ok"), None);
        assert_eq!(parse_beacon(b"croc99999"), None); // 超出 u16
        assert_eq!(parse_beacon(&[0xff, 0xfe]), None); // 非 UTF-8
    }

    #[test]
    fn discovered_builds_a_relay_address_from_the_source_ip() {
        let found = Discovered { address: "192.168.1.7:54321".parse().expect("addr"), control_port: 9009 };
        assert_eq!(found.relay_address(), "192.168.1.7:9009");
    }

    #[tokio::test]
    async fn a_local_relay_binds_a_control_port_and_data_ports() {
        let mut relay = start_local_relay("pass", 2, None).await.expect("relay starts");
        assert_ne!(relay.control_port(), 0);
        assert_eq!(relay.data_ports().len(), 2);
        // 中继真的在听：能连上控制端口。
        let probe = tokio::net::TcpStream::connect(relay.loopback_address()).await;
        assert!(probe.is_ok(), "the control port must accept connections: {probe:?}");
        relay.shutdown();
    }

    /// `--port` 是"基准端口"：从它开始往后排，且被占了会顺延而不是报错。
    #[tokio::test]
    async fn port_starts_at_the_base_and_skips_occupied_ones() {
        // 先探一个当前空闲的端口当 base（拿到就放手，下面再验证它确实被用上了）。
        let probe = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await.expect("bind");
        let base = probe.local_addr().expect("addr").port();
        drop(probe);
        let Some(next) = base.checked_add(1) else {
            return;
        };
        // 占住 base+1，逼数据端口顺延。
        let Ok(_squatter) = TcpListener::bind((Ipv4Addr::UNSPECIFIED, next)).await else {
            return;
        };

        let mut relay = start_local_relay("pass", 2, Some(base)).await.expect("relay starts");
        assert_eq!(relay.control_port(), base, "控制端口应当就落在 --port 上");
        let ports = relay.data_ports();
        assert_eq!(ports.len(), 2);
        assert!(ports.iter().all(|port| *port != next), "被占的端口必须跳过：{ports:?}");
        relay.shutdown();
    }
}
