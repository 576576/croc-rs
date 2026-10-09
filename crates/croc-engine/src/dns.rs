//! `--internal-dns`：自带的 DNS stub resolver。
//!
//! 默认走操作系统的解析器。打开 `--internal-dns` 后改成直接问公共 DNS 服务器，
//! 绕过 hosts 文件、`systemd-resolved` 这类本机配置——本机 DNS 被改坏或压根没配的场景下，
//! 只有这条路还通。
//!
//! # 范围
//!
//! 只做 A / AAAA（外加 CNAME 链），也就是"把一个中继主机名变成 IP"所需的那点东西。
//! 这不是通用解析器：没有 EDNS0、没有 DNSSEC、没有 SRV/MX，也不做 0x20 随机化。
//! 每次查询用一个新的已连接 UDP socket，所以响应里的 ID 只用于防串话，不承担安全性。
//!
//! 走代理连接时不使用这里的解析：把域名交给代理去解析才是对的，本地解析会把 DNS 请求漏到网外。

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

use croc_protocol::Randomness;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::task::JoinSet;

use crate::error::EngineError;

/// Go 的 `models.publicDNS`，顺序与文本都照抄——包括 IPv6 的方括号。
pub const PUBLIC_DNS: [&str; 18] = [
    "1.0.0.1",                // Cloudflare
    "1.1.1.1",                // Cloudflare
    "[2606:4700:4700::1111]", // Cloudflare
    "[2606:4700:4700::1001]", // Cloudflare
    "8.8.4.4",                // Google
    "8.8.8.8",                // Google
    "[2001:4860:4860::8844]", // Google
    "[2001:4860:4860::8888]", // Google
    "9.9.9.9",                // Quad9
    "149.112.112.112",        // Quad9
    "[2620:fe::fe]",          // Quad9
    "[2620:fe::fe:9]",        // Quad9
    "8.26.56.26",             // Comodo
    "8.20.247.20",            // Comodo
    "208.67.220.220",         // Cisco OpenDNS
    "208.67.222.222",         // Cisco OpenDNS
    "[2620:119:35::35]",      // Cisco OpenDNS
    "[2620:119:53::53]",      // Cisco OpenDNS
];

/// 单个查询的超时。Go 的 `500 * time.Millisecond`。
const QUERY_TIMEOUT: Duration = Duration::from_millis(500);

/// 报文头长度：ID + flags + 四个计数字段。
const HEADER_LEN: usize = 12;

/// UDP 响应缓冲。一条 AAAA 答案远小于这个数；超过就说明服务器在做别的事。
const MAX_UDP_RESPONSE: usize = 4096;

const TYPE_A: u16 = 1;
const TYPE_CNAME: u16 = 5;
const TYPE_AAAA: u16 = 28;
const CLASS_IN: u16 = 1;
const DNS_PORT: u16 = 53;

/// `QR` 位（响应）。
const FLAG_RESPONSE: u16 = 0x8000;
/// `TC` 位（截断）。
const FLAG_TRUNCATED: u16 = 0x0200;
/// `RCODE` 掩码。
const RCODE_MASK: u16 = 0x000F;

/// 解析模式。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Mode {
    /// 操作系统自己的解析器。
    #[default]
    System,
    /// 直接问 [`PUBLIC_DNS`]。
    Internal,
}

/// 把一个主机名解析成全部候选地址。
///
/// 返回列表而不是单个地址：第一条连不上时还有下一条可试，这与 Go 的
/// `net.Dialer.DialContext` 依次尝试每个地址是同一种行为。
///
/// # Errors
///
/// 解析彻底失败（系统解析器报错，或公共 DNS 全都答不上来）时返回
/// [`EngineError::RelayUnreachable`]——它 [`is_retryable`](EngineError::is_retryable)，
/// 与"连不上中继"归到同一类。
pub async fn resolve(host: &str, port: u16, mode: Mode) -> Result<Vec<SocketAddr>, EngineError> {
    let display = format!("{host}:{port}");
    // 数字 IP 直接构造，不走解析。Windows 上 `getaddrinfo("127.0.0.1")` 会绕一圈 DNS
    // 客户端，实测每次 ~370ms；数据面要连好几条连接，那是成秒的纯等待。
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(vec![SocketAddr::new(ip, port)]);
    }
    let failure = |reason: String| EngineError::RelayUnreachable { address: display.clone(), reason };

    let addresses = match mode {
        Mode::System => {
            let resolved = tokio::net::lookup_host((host, port)).await.map_err(|error| failure(error.to_string()))?;
            resolved.collect::<Vec<_>>()
        }
        Mode::Internal => internal_lookup(host).await.into_iter().map(|ip| SocketAddr::new(ip, port)).collect(),
    };
    if addresses.is_empty() {
        return Err(failure("the host name resolved to nothing".to_owned()));
    }
    Ok(addresses)
}

/// 并发问所有公共 DNS 服务器，取第一个给出非空答案的。
///
/// 与 Go 的 `lookup`同一个形状：所有服务器同时发，谁先答上来用谁，
/// 不等其余的——多等只会让"最快的那个慢服务器"拖住整次连接。
async fn internal_lookup(host: &str) -> Vec<IpAddr> {
    let mut servers = JoinSet::new();
    let name = host.to_owned();
    for server in PUBLIC_DNS {
        let name = name.clone();
        servers.spawn(async move { ask_server(server, &name).await });
    }
    while let Some(joined) = servers.join_next().await {
        if let Ok(addresses) = joined
            && !addresses.is_empty()
        {
            servers.abort_all();
            return addresses;
        }
    }
    Vec::new()
}

/// 问一台服务器要这个主机名的地址。
///
/// A 与 AAAA 各发一条（一次查询只能带一个 QTYPE），同样是谁先答上来用谁。
async fn ask_server(server: &str, host: &str) -> Vec<IpAddr> {
    let Some(address) = parse_server(server) else {
        return Vec::new();
    };
    let mut records = JoinSet::new();
    for record in [Record::Aaaa, Record::A] {
        let host = host.to_owned();
        records.spawn(async move { query(address, &host, record).await });
    }
    while let Some(joined) = records.join_next().await {
        if let Ok(addresses) = joined
            && !addresses.is_empty()
        {
            records.abort_all();
            return addresses;
        }
    }
    Vec::new()
}

/// `"1.1.1.1"` 或 `"[2606:4700:4700::1111]"` → 服务器地址。
///
/// 方括号是 Go 的写法（它靠 `net.JoinHostPort` 拼端口），`IpAddr` 的 `FromStr` 不认，
/// 所以这里自己剥。
fn parse_server(server: &str) -> Option<IpAddr> {
    let text = server.strip_prefix('[').and_then(|rest| rest.strip_suffix(']')).unwrap_or(server);
    text.parse().ok()
}

/// 要查的记录类型。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Record {
    /// IPv4。
    A,
    /// IPv6。
    Aaaa,
}

impl Record {
    const fn code(self) -> u16 {
        match self {
            Self::A => TYPE_A,
            Self::Aaaa => TYPE_AAAA,
        }
    }
}

/// 发一条查询并取回地址。
async fn query(server: IpAddr, host: &str, record: Record) -> Vec<IpAddr> {
    let mut id_bytes = [0u8; 2];
    croc_net::OsRandomness.fill(&mut id_bytes);
    let id = u16::from_be_bytes(id_bytes);
    let Some(request) = build_query(id, host, record) else {
        return Vec::new();
    };
    let Some(response) = exchange(server, &request).await else {
        return Vec::new();
    };
    parse_answers(&response, id, host)
}

/// 一次往返：先 UDP，被截断才改走 TCP。
async fn exchange(server: IpAddr, request: &[u8]) -> Option<Vec<u8>> {
    let udp = tokio::time::timeout(QUERY_TIMEOUT, over_udp(server, request)).await.ok().flatten();
    if let Some(response) = udp
        && !is_truncated(&response)
    {
        return Some(response);
    }
    // 截断说明 TCP 才有完整答案。这条路径很少走，但少了它，答案超过 512 字节的域名
    // 会静默解析失败——看起来就像"这个域名不存在"。
    tokio::time::timeout(QUERY_TIMEOUT, over_tcp(server, request)).await.ok().flatten()
}

async fn over_udp(server: IpAddr, request: &[u8]) -> Option<Vec<u8>> {
    let bind = match server {
        IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
        IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
    };
    let socket = UdpSocket::bind(bind).await.ok()?;
    // `connect` 让内核只把对端发来的包交给这个 socket：不然网卡上任何一个陌生响应
    // 都可能被当成答案。
    socket.connect(SocketAddr::new(server, DNS_PORT)).await.ok()?;
    socket.send(request).await.ok()?;
    let mut buffer = vec![0u8; MAX_UDP_RESPONSE];
    let read = socket.recv(&mut buffer).await.ok()?;
    buffer.truncate(read);
    Some(buffer)
}

async fn over_tcp(server: IpAddr, request: &[u8]) -> Option<Vec<u8>> {
    let mut stream = TcpStream::connect(SocketAddr::new(server, DNS_PORT)).await.ok()?;
    // TCP 上的 DNS 报文自带两字节长度前缀，与 UDP 的裸报文不同。
    let length = u16::try_from(request.len()).ok()?;
    stream.write_all(&length.to_be_bytes()).await.ok()?;
    stream.write_all(request).await.ok()?;
    let mut header = [0u8; 2];
    stream.read_exact(&mut header).await.ok()?;
    let mut response = vec![0u8; usize::from(u16::from_be_bytes(header))];
    stream.read_exact(&mut response).await.ok()?;
    Some(response)
}

/// 拼一条查询报文。
///
/// 只置 `RD`（期望递归）：stub resolver 不做迭代查询，也没必要带 EDNS0——A/AAAA 的
/// 答案不会有 512 字节。
fn build_query(id: u16, host: &str, record: Record) -> Option<Vec<u8>> {
    let mut request = Vec::with_capacity(32);
    request.extend_from_slice(&id.to_be_bytes());
    request.extend_from_slice(&0x0100u16.to_be_bytes());
    request.extend_from_slice(&1u16.to_be_bytes());
    // ANCOUNT / NSCOUNT / ARCOUNT 全为 0。
    request.extend_from_slice(&[0; 6]);
    // 末尾的点是 FQDN 写法，编码时不产生额外的空标签。
    for label in host.strip_suffix('.').unwrap_or(host).split('.') {
        if label.is_empty() || label.len() > 63 {
            return None;
        }
        request.push(u8::try_from(label.len()).ok()?);
        request.extend_from_slice(label.as_bytes());
    }
    request.push(0);
    request.extend_from_slice(&record.code().to_be_bytes());
    request.extend_from_slice(&CLASS_IN.to_be_bytes());
    Some(request)
}

/// 读一个域名，返回 `(名字, 名字之后的下一个字节位置)`。
///
/// 压缩指针（高两位 `11`）要跟着跳，但返回的位置是第一次遇到指针之前的那个位置
/// ——记录里跟在名字后面的是 TYPE/CLASS，它们在报文里的实际偏移，不是指针指向的地方。
fn read_name(message: &[u8], start: usize) -> Option<(String, usize)> {
    let mut labels: Vec<&str> = Vec::new();
    let mut position = start;
    let mut resume = None;
    let mut jumps = 0;
    loop {
        let length = usize::from(*message.get(position)?);
        if length == 0 {
            position += 1;
            break;
        }
        if length & 0xC0 == 0xC0 {
            let low = usize::from(*message.get(position.checked_add(1)?)?);
            let target = ((length & 0x3F) << 8) | low;
            // 指针必须指回前面才算压缩；指到自己或后面就是环。
            if target >= position {
                return None;
            }
            resume.get_or_insert(position + 2);
            jumps += 1;
            if jumps > 16 {
                return None;
            }
            position = target;
            continue;
        }
        if length > 63 {
            return None;
        }
        let end = position.checked_add(1)?.checked_add(length)?;
        labels.push(core::str::from_utf8(message.get(position + 1..end)?).ok()?);
        position = end;
    }
    Some((labels.join("."), resume.unwrap_or(position)))
}

/// 取报文里属于 `qname` 及其 CNAME 链的 A / AAAA 记录。
fn parse_answers(message: &[u8], id: u16, qname: &str) -> Vec<IpAddr> {
    collect_answers(message, id, qname).unwrap_or_default()
}

fn collect_answers(message: &[u8], id: u16, qname: &str) -> Option<Vec<IpAddr>> {
    if be_u16(message, 0)? != id {
        return None;
    }
    let flags = be_u16(message, 2)?;
    // 必须是响应（QR=1）且 NOERROR。NXDOMAIN / SERVFAIL 的答案段是空的，走完也没地址，
    // 早点退出省得去解析一个格式还不一定的报文。
    if flags & FLAG_RESPONSE == 0 || flags & RCODE_MASK != 0 {
        return None;
    }
    let questions = usize::from(be_u16(message, 4)?);
    let answers = usize::from(be_u16(message, 6)?);

    let mut position = HEADER_LEN;
    for _ in 0..questions {
        let (_, next) = read_name(message, position)?;
        position = next.checked_add(4)?;
    }

    let mut accepted = vec![qname.to_ascii_lowercase()];
    let mut addresses = Vec::new();
    for _ in 0..answers {
        let (owner, next) = read_name(message, position)?;
        let kind = be_u16(message, next)?;
        let rdlength = usize::from(be_u16(message, next + 8)?);
        let rdata = next + 10;
        let end = rdata.checked_add(rdlength)?;
        if end > message.len() {
            return None;
        }
        position = end;

        let owner = owner.to_ascii_lowercase();
        // 一个响应里可能夹着别的名字的记录（尤其走 TCP 复用连接时），只认自己这条链
        // 上的，不然会把别人的地址当成答案。
        if !accepted.contains(&owner) {
            continue;
        }
        match kind {
            TYPE_CNAME => {
                let (target, _) = read_name(message, rdata)?;
                let target = target.to_ascii_lowercase();
                if !accepted.contains(&target) {
                    accepted.push(target);
                }
            }
            TYPE_A if rdlength == 4 => {
                addresses.push(IpAddr::V4(Ipv4Addr::from(<[u8; 4]>::try_from(message.get(rdata..rdata + 4)?).ok()?)));
            }
            TYPE_AAAA if rdlength == 16 => {
                addresses.push(IpAddr::V6(Ipv6Addr::from(<[u8; 16]>::try_from(message.get(rdata..rdata + 16)?).ok()?)));
            }
            _ => {}
        }
    }
    Some(addresses)
}

fn be_u16(message: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(message.get(at..at + 2)?.try_into().ok()?))
}

fn is_truncated(response: &[u8]) -> bool {
    be_u16(response, 2).is_some_and(|flags| flags & FLAG_TRUNCATED != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 断言用的不变量：这条列表的文本是 Go 那份的照抄，改一个字符都得是有意的。
    #[test]
    fn the_server_list_matches_go() {
        assert_eq!(PUBLIC_DNS.len(), 18);
        assert_eq!(PUBLIC_DNS[0], "1.0.0.1");
        assert_eq!(PUBLIC_DNS[2], "[2606:4700:4700::1111]");
        assert_eq!(PUBLIC_DNS[11], "[2620:fe::fe:9]");
        assert_eq!(PUBLIC_DNS[17], "[2620:119:53::53]");
    }

    #[test]
    fn bracketed_servers_parse() {
        assert_eq!(parse_server("1.1.1.1"), Some(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert_eq!(parse_server("[2606:4700:4700::1111]"), "2606:4700:4700::1111".parse().ok());
        assert_eq!(parse_server("dns.example"), None);
    }

    #[test]
    fn a_query_is_a_well_formed_a_question() {
        let request = build_query(0x1234, "relay.example", Record::A).expect("拼得出");
        let mut expected = vec![0x12, 0x34, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        expected.extend_from_slice(b"\x05relay\x07example\x00");
        expected.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        assert_eq!(request, expected);
    }

    #[test]
    fn the_trailing_dot_does_not_add_an_empty_label() {
        let with_dot = build_query(1, "relay.example.", Record::A).expect("拼得出");
        let without = build_query(1, "relay.example", Record::A).expect("拼得出");
        assert_eq!(with_dot, without);
    }

    #[test]
    fn the_record_type_is_carried_in_the_question() {
        let aaaa = build_query(1, "relay.example", Record::Aaaa).expect("拼得出");
        assert_eq!(aaaa.get(aaaa.len() - 4..), Some([0x00, 0x1C, 0x00, 0x01].as_slice()));
    }

    #[test]
    fn malformed_names_are_rejected_before_sending() {
        assert_eq!(build_query(1, "a..b", Record::A), None);
        assert_eq!(build_query(1, "", Record::A), None);
        let long = "a".repeat(64);
        assert_eq!(build_query(1, &long, Record::A), None);
    }

    #[test]
    fn a_query_id_that_does_not_match_is_ignored() {
        let response = answer(&[("relay.example", TYPE_A, &[10, 0, 0, 1])], 0x1234);
        assert!(parse_answers(&response, 0x1235, "relay.example").is_empty());
        assert_eq!(parse_answers(&response, 0x1234, "relay.example"), vec![IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))]);
    }

    #[test]
    fn an_error_response_yields_nothing() {
        let mut response = answer(&[], 7);
        // RCODE = NXDOMAIN，并且把响应位清掉（一个"看起来像请求"的报文不该被采信）。
        response[2..4].copy_from_slice(&0x0003u16.to_be_bytes());
        assert!(parse_answers(&response, 7, "relay.example").is_empty());
        response[2..4].copy_from_slice(&0x8003u16.to_be_bytes());
        assert!(parse_answers(&response, 7, "relay.example").is_empty());
    }

    #[test]
    fn records_from_a_cname_chain_are_accepted() {
        // CNAME 指向的 A 记录才算答案：链上第一个名字是别名，不是被查的名字。
        let mut message = header(9, 0, 3);
        message.extend_from_slice(&question("relay.example"));
        message.extend_from_slice(&record("relay.example", TYPE_CNAME, &encode_name("edge.example")));
        message.extend_from_slice(&record("edge.example", TYPE_A, &[203, 0, 113, 7]));
        // 一个不属于这条链的记录，必须被忽略。
        message.extend_from_slice(&record("elsewhere.example", TYPE_A, &[203, 0, 113, 9]));
        assert_eq!(parse_answers(&message, 9, "relay.example"), vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 7))]);
    }

    #[test]
    fn compression_pointers_in_answers_are_followed() {
        // 答案里的名字用指针指回问题段的名字——这是压缩最常见的用法，必须跟着跳，
        // 同时"下一个字段的位置"仍然是指针之后。
        let mut message = header(11, 0, 1);
        message.extend_from_slice(&question("relay.example"));
        // 指针写的是 0x0C，正好是问题段里那个名字的偏移。
        assert_eq!(message.len(), HEADER_LEN + question("relay.example").len());
        message.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x1C, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x10]);
        message.extend_from_slice(&"2001:db8::1".parse::<Ipv6Addr>().expect("常量").octets());
        assert_eq!(
            parse_answers(&message, 11, "relay.example"),
            vec![IpAddr::V6("2001:db8::1".parse().expect("常量"))]
        );
    }

    #[test]
    fn a_pointer_that_loops_is_refused() {
        let mut message = header(13, 0, 1);
        message.extend_from_slice(&question("relay.example"));
        // 名字直接指回自己：不是压缩，是环。
        let at = u16::try_from(message.len()).expect("短报文");
        message
            .extend_from_slice(&[0xC0 | u8::try_from(at >> 8).expect("高位"), u8::try_from(at & 0xFF).expect("低位")]);
        message.extend_from_slice(&[0x00, 0x1C, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0x00, 0x10]);
        message.extend_from_slice(&[0u8; 16]);
        assert!(parse_answers(&message, 13, "relay.example").is_empty());
    }

    #[test]
    fn rdlength_past_the_end_is_refused() {
        let mut message = header(17, 0, 1);
        message.extend_from_slice(&question("relay.example"));
        message.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01, 0x00, 0x00, 0x00, 0x3C, 0xFF, 0xFF]);
        assert!(parse_answers(&message, 17, "relay.example").is_empty());
    }

    #[test]
    fn the_truncated_flag_is_recognised() {
        let mut response = answer(&[], 5);
        assert!(!is_truncated(&response));
        response[2..4].copy_from_slice(&0x8300u16.to_be_bytes());
        assert!(is_truncated(&response));
        assert!(!is_truncated(&[0x00]));
    }

    /// 报文头：`id`、给定 flags、`questions` 个问题、`answers` 条回答。
    fn header(id: u16, flags: u16, answers: u16) -> Vec<u8> {
        header_with(id, 0x8000 | flags, 1, answers)
    }

    fn header_with(id: u16, flags: u16, questions: u16, answers: u16) -> Vec<u8> {
        let mut message = Vec::new();
        message.extend_from_slice(&id.to_be_bytes());
        message.extend_from_slice(&flags.to_be_bytes());
        message.extend_from_slice(&questions.to_be_bytes());
        message.extend_from_slice(&answers.to_be_bytes());
        message.extend_from_slice(&[0, 0, 0, 0]);
        message
    }

    fn encode_name(name: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for label in name.split('.') {
            bytes.push(u8::try_from(label.len()).expect("短标签"));
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.push(0);
        bytes
    }

    fn question(name: &str) -> Vec<u8> {
        let mut bytes = encode_name(name);
        bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        bytes
    }

    fn record(name: &str, kind: u16, rdata: &[u8]) -> Vec<u8> {
        let mut bytes = encode_name(name);
        bytes.extend_from_slice(&kind.to_be_bytes());
        bytes.extend_from_slice(&[0x00, 0x01, 0x00, 0x00, 0x00, 0x3C]);
        bytes.extend_from_slice(&u16::try_from(rdata.len()).expect("短数据").to_be_bytes());
        bytes.extend_from_slice(rdata);
        bytes
    }

    /// 一个"问题 + N 条回答"的完整响应。
    fn answer(records: &[(&str, u16, &[u8])], id: u16) -> Vec<u8> {
        let mut message = header_with(id, 0x8000, 1, u16::try_from(records.len()).expect("少量记录"));
        message.extend_from_slice(&question("relay.example"));
        for (name, kind, rdata) in records {
            message.extend_from_slice(&record(name, *kind, rdata));
        }
        message
    }
}
