//! 中继连接：控制端口与数据端口。
//!
//! 两处的握手是同一套（[`croc_protocol::relay::ClientHandshake`]），区别只在房间名：
//!
//! | 连接 | 房间名 | 用途 |
//! |------|--------|------|
//! | 控制端口（`banner` 里的第一个端口） | `{room}` | 消息层（PAKE、清单、收尾） |
//! | 数据端口（第 j 个） | `{room}-{j}` | 只搬文件字节，中继不解析 |
//!
//! 控制端口的 banner 里带着**数据端口列表**（逗号分隔）——这是 Go 的约定：
//! 中继 `WithBanner(tcpPorts)`，客户端 `strings.Split(route.banner, ",")`。
//!
//! 握手完成后连接就变成裸转发，中继会周期性插入 keepalive（帧体是单字节 `0x01`）。
//! **读循环必须跳过它**，否则会被当成一条空消息喂给状态机。

use std::time::Duration;

use croc_core::RoomName;
use croc_net::{Framed, OsRandomness};
use croc_protocol::relay::{ClientHandshake, RelayError};
use tokio::net::TcpStream;

use crate::error::EngineError;

/// 连接超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 一次握手最多收几帧。正常只要 2 帧（B 与 banner）+ 1 帧准入，给足余量防死循环。
const MAX_HANDSHAKE_FRAMES: usize = 32;

/// 一条已完成握手的中继连接。
pub struct RelayLink {
    /// 帧读写端。握手之后它就是一条普通的加密隧道。
    pub framed: Framed<TcpStream>,
    /// 中继 banner。控制连接上它是**逗号分隔的数据端口列表**。
    pub banner: String,
    /// 中继看到的本端地址（`host:port`）。
    pub external_ip: String,
}

impl core::fmt::Debug for RelayLink {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("RelayLink")
            .field("banner", &self.banner)
            .field("external_ip", &self.external_ip)
            .finish()
    }
}

impl RelayLink {
    /// banner 里的数据端口列表。
    ///
    /// 解析失败（不是数字、超出 `u16`）的条目被跳过而不是报错——对端可能是新版中继，
    /// banner 里多带一段我们看不懂的东西，不该因此整条连接作废。
    #[must_use]
    pub fn data_ports(&self) -> Vec<u16> {
        parse_data_ports(&self.banner)
    }
}

/// 解析 banner 里的数据端口列表。
///
/// 抽成纯函数是为了能脱离 socket 测试——一个真实 `TcpStream` 要在 tokio 运行时里才
/// 构造得出来，为了测五行的字符串切分去起一个运行时不值得。
#[must_use]
pub fn parse_data_ports(banner: &str) -> Vec<u16> {
    banner.split(',').filter_map(|piece| piece.trim().parse::<u16>().ok()).filter(|port| *port != 0).collect()
}

/// 连到中继并完成握手。
///
/// `host_port` 形如 `croc.schollz.com:9009` 或 `127.0.0.1:9009`。
///
/// # Errors
///
/// 连不上、超时、口令不对、房间已满、被限流，或者对端不是 croc 中继。
pub async fn connect(host_port: &str, password: &str, room: &RoomName) -> Result<RelayLink, EngineError> {
    let (host, port) = split_host_port(host_port)?;
    // **先把数字 IP 直接解析成 `SocketAddr`**，不要交给 `ToSocketAddrs`。
    // Windows 上 `getaddrinfo("127.0.0.1")` 会绕一圈 DNS 客户端，实测每次 ~370ms——
    // 数据面要连 5 条连接，那就是 1.8 秒的纯等待，足以让对端先超时。
    // 主机名（`croc.schollz.com`）仍然走解析。
    let target = match host.parse::<std::net::IpAddr>() {
        Ok(ip) => std::net::SocketAddr::new(ip, port),
        Err(_) => tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| EngineError::RelayUnreachable {
                address: host_port.to_owned(),
                reason: error.to_string(),
            })?
            .next()
            .ok_or_else(|| EngineError::RelayUnreachable {
                address: host_port.to_owned(),
                reason: "the host name resolved to nothing".to_owned(),
            })?,
    };
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(target))
        .await
        .map_err(|_| EngineError::RelayUnreachable {
            address: host_port.to_owned(),
            reason: "connect timed out".to_owned(),
        })?
        .map_err(|error| EngineError::RelayUnreachable { address: host_port.to_owned(), reason: error.to_string() })?;
    // 中继的 keepalive 是 1 秒一次，禁用 Nagle 能让握手少等几十毫秒。
    let _ = stream.set_nodelay(true);

    let mut framed = Framed::new(stream);
    let mut randomness = OsRandomness;
    let mut handshake = ClientHandshake::new(password.as_bytes(), room.as_str(), &mut randomness)?;

    let first = handshake.start()?;
    framed.write_frame(&first).await?;

    for _ in 0..MAX_HANDSHAKE_FRAMES {
        let Some(frame) = framed.read_frame().await? else {
            return Err(EngineError::Relay("the relay closed the connection during the handshake".to_owned()));
        };
        // 准入帧可能紧跟在 keepalive 后面排队过来，跳过它继续等。
        if croc_protocol::is_keepalive(&frame) {
            continue;
        }
        let step = handshake.step(&frame, &mut randomness)?;
        for outgoing in &step.send {
            framed.write_frame(outgoing).await?;
        }
        if let Some(session) = step.session {
            return Ok(RelayLink { framed, banner: session.banner, external_ip: session.ip_address });
        }
    }
    Err(EngineError::Relay("the relay handshake did not converge".to_owned()))
}

/// 连一条数据端口。房间名是 `{room}-{index}`。
///
/// # Errors
///
/// 同 [`connect`]。
pub async fn connect_data(
    host: &str,
    port: u16,
    password: &str,
    room: &RoomName,
    index: u8,
) -> Result<Framed<TcpStream>, EngineError> {
    let address = format!("{host}:{port}");
    let room = RoomName::new(format!("{}-{index}", room.as_str()));
    Ok(connect(&address, password, &room).await?.framed)
}

/// 拆 `host:port`。IPv6 字面量要带方括号（`[::1]:9009`）。
fn split_host_port(value: &str) -> Result<(String, u16), EngineError> {
    let bad = || EngineError::BadRelayAddress(value.to_owned());

    let (host, port) = if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(bad)?;
        (host.to_owned(), tail.strip_prefix(':').ok_or_else(bad)?)
    } else {
        let (host, port) = value.rsplit_once(':').ok_or_else(bad)?;
        (host.to_owned(), port)
    };
    if host.is_empty() {
        return Err(bad());
    }
    let port: u16 = port.trim().parse().map_err(|_| bad())?;
    Ok((host, port))
}

impl From<RelayError> for EngineError {
    fn from(error: RelayError) -> Self {
        Self::Relay(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_port_splitting_handles_ipv4_hostnames_and_ipv6() {
        assert_eq!(split_host_port("127.0.0.1:9009").expect("v4"), ("127.0.0.1".to_owned(), 9009));
        assert_eq!(split_host_port("croc.schollz.com:9009").expect("hostname"), ("croc.schollz.com".to_owned(), 9009));
        assert_eq!(split_host_port("[::1]:9009").expect("v6"), ("::1".to_owned(), 9009));
    }

    #[test]
    fn bad_addresses_are_rejected() {
        assert!(split_host_port("no-port").is_err());
        assert!(split_host_port(":9009").is_err());
        assert!(split_host_port("host:not-a-port").is_err());
        assert!(split_host_port("host:99999").is_err());
        assert!(split_host_port("[::1:9009").is_err());
    }

    #[test]
    fn the_banner_yields_the_data_ports() {
        assert_eq!(parse_data_ports("9009,9010,9011,9012,9013"), vec![9009, 9010, 9011, 9012, 9013]);
        // 看不懂的条目被跳过，而不是让整条连接作废——对端可能是新版中继，
        // banner 里多带一段我们还不认识的东西。
        assert_eq!(parse_data_ports("9009, 9010 ,oops,0,99999"), vec![9009, 9010]);
        assert!(parse_data_ports("").is_empty());
    }
}
