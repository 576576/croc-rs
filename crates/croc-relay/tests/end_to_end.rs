#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "测试代码，断言失败就该 panic"
)]

//! 中继的端到端测试：真 TCP、真握手、真转发。
//!
//! 这里跑的是"两台 croc 客户端 + 一台 Rust 中继"的最小场景。客户端侧用的是
//! `croc_protocol::relay::ClientHandshake`——和 Go 版逐帧兼容的那个状态机，
//! 所以这些测试同时在验证"我们实现的中继能被 croc 客户端用"和"我们的客户端状态机
//! 能跟中继对上"。

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use croc_net::{Framed, OsRandomness};
use croc_protocol::relay::{ClientHandshake, ClientSession, PING, PONG, RelayError};
use croc_protocol::{KEEPALIVE, is_keepalive};
use croc_relay::{Relay, RelayConfig};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

const PASSWORD: &str = "relay-secret";
const ROOM: &str = "test-room-1234";

/// 起一台中继，返回它的地址与关闭令牌。
async fn spawn_relay(config: RelayConfig) -> (SocketAddr, CancellationToken) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("应能绑定端口");
    let addr = listener.local_addr().expect("应有本地地址");
    let relay = Arc::new(Relay::new(config));
    let shutdown = CancellationToken::new();
    let token = shutdown.clone();
    tokio::spawn(async move {
        relay.run(vec![listener], shutdown).await.expect("中继不该异常退出");
    });
    (addr, token)
}

/// 用客户端状态机跑完一次中继握手。
async fn join(addr: SocketAddr, password: &str, room: &str) -> (Framed<TcpStream>, ClientSession) {
    let stream = TcpStream::connect(addr).await.expect("应能连上中继");
    let mut framed = Framed::new(stream);
    let mut randomness = OsRandomness;
    let mut handshake = ClientHandshake::new(password.as_bytes(), room, &mut randomness).expect("构造握手");

    let mut outbox = vec![handshake.start().expect("start")];
    loop {
        for frame in std::mem::take(&mut outbox) {
            framed.write_frame(&frame).await.expect("应能写帧");
        }
        let frame = framed.read_frame().await.expect("应能读帧").expect("中继不该关闭连接");
        let step = handshake.step(&frame, &mut randomness).expect("握手不该失败");
        outbox = step.send;
        if let Some(session) = step.session {
            return (framed, session);
        }
    }
}

/// 读一条业务帧，跳过中继的保活帧。
///
/// 房间里的第一位在等配对时，中继每秒会发一个 `[0x01]` 保活帧。真实客户端也会跳过它，
/// 这里必须同样处理，否则测试会因为"收到一个字节"而失败。
async fn read_data(framed: &mut Framed<TcpStream>) -> Option<Vec<u8>> {
    loop {
        let frame = framed.read_frame().await.expect("应能读帧")?;
        if !is_keepalive(&frame) {
            return Some(frame);
        }
    }
}

#[tokio::test]
async fn two_peers_meet_and_exchange_bytes() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD).with_banner("hello croc")).await;

    // A 先进房间，然后卡在等配对。
    let (mut a, session_a) = join(addr, PASSWORD, ROOM).await;
    assert_eq!(session_a.banner, "hello croc");

    // B 进来，配对成功。
    let (mut b, session_b) = join(addr, PASSWORD, ROOM).await;
    assert_eq!(session_b.banner, "hello croc");

    // 双向转发。
    a.write_frame(b"ping from A").await.expect("A 应能写");
    assert_eq!(read_data(&mut b).await, Some(b"ping from A".to_vec()));

    b.write_frame(b"pong from B").await.expect("B 应能写");
    assert_eq!(read_data(&mut a).await, Some(b"pong from B".to_vec()));

    // 大一点的数据也要原样过去（验证转发不是按帧解析的）。
    let payload = vec![0x5au8; 300 * 1024];
    a.write_frame(&payload).await.expect("A 应能写大帧");
    assert_eq!(read_data(&mut b).await, Some(payload));

    shutdown.cancel();
}

#[tokio::test]
async fn a_wrong_relay_password_is_rejected() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD)).await;

    let stream = TcpStream::connect(addr).await.expect("应能连上");
    let mut framed = Framed::new(stream);
    let mut randomness = OsRandomness;
    let mut handshake = ClientHandshake::new(b"wrong", ROOM, &mut randomness).expect("构造握手");

    let mut outbox = vec![handshake.start().expect("start")];
    let error = loop {
        for frame in std::mem::take(&mut outbox) {
            framed.write_frame(&frame).await.expect("应能写帧");
        }
        let frame = framed.read_frame().await.expect("应能读帧").expect("中继在报错之前不该关闭连接");
        match handshake.step(&frame, &mut randomness) {
            Ok(step) => {
                outbox = step.send;
                assert!(step.session.is_none(), "口令不对不该握手成功");
            }
            Err(error) => break error,
        }
    };

    assert_eq!(error, RelayError::BadPassword);
    shutdown.cancel();
}

#[tokio::test]
async fn a_third_peer_is_told_the_room_is_full() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD)).await;

    let (_a, _) = join(addr, PASSWORD, ROOM).await;
    let (_b, _) = join(addr, PASSWORD, ROOM).await;

    // 第三个客户端：房间已经被上面那对占住了。
    let stream = TcpStream::connect(addr).await.expect("应能连上");
    let mut framed = Framed::new(stream);
    let mut randomness = OsRandomness;
    let mut handshake = ClientHandshake::new(PASSWORD.as_bytes(), ROOM, &mut randomness).expect("构造握手");

    let mut outbox = vec![handshake.start().expect("start")];
    let error = loop {
        for frame in std::mem::take(&mut outbox) {
            framed.write_frame(&frame).await.expect("应能写帧");
        }
        let frame = framed.read_frame().await.expect("应能读帧").expect("中继不该关闭连接");
        match handshake.step(&frame, &mut randomness) {
            Ok(step) => outbox = step.send,
            Err(error) => break error,
        }
    };

    assert_eq!(error, RelayError::RoomFull);
    shutdown.cancel();
}

#[tokio::test]
async fn ping_gets_a_pong_and_no_room() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD)).await;

    let stream = TcpStream::connect(addr).await.expect("应能连上");
    let mut framed = Framed::new(stream);
    framed.write_frame(PING).await.expect("应能发探活");

    assert_eq!(framed.read_frame().await.expect("应能读"), Some(PONG.to_vec()));
    // 中继回完 pong 就断开。
    assert_eq!(framed.read_frame().await.expect("应能读"), None);
    shutdown.cancel();
}

#[tokio::test]
async fn separate_rooms_do_not_cross_talk() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD)).await;

    let (mut a1, _) = join(addr, PASSWORD, "room-one").await;
    let (mut b1, _) = join(addr, PASSWORD, "room-one").await;
    let (mut a2, _) = join(addr, PASSWORD, "room-two").await;
    let (mut b2, _) = join(addr, PASSWORD, "room-two").await;

    a1.write_frame(b"one").await.expect("应能写");
    a2.write_frame(b"two").await.expect("应能写");

    assert_eq!(read_data(&mut b1).await, Some(b"one".to_vec()));
    assert_eq!(read_data(&mut b2).await, Some(b"two".to_vec()));

    shutdown.cancel();
}

#[tokio::test]
async fn the_default_banner_is_used_when_none_is_configured() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD)).await;
    let (_, session) = join(addr, PASSWORD, ROOM).await;
    assert_eq!(session.banner, "ok");
    assert!(session.ip_address.starts_with("127.0.0.1:"), "中继应回报对端地址");
    shutdown.cancel();
}

#[tokio::test]
async fn a_waiting_room_expires_and_drops_the_first_peer() {
    let (addr, shutdown) = spawn_relay(
        RelayConfig::new()
            .with_password(PASSWORD)
            .with_room_ttl(Duration::from_millis(50))
            .with_room_cleanup_interval(Duration::from_millis(20)),
    )
    .await;

    let (mut waiting, _) = join(addr, PASSWORD, ROOM).await;

    // 等清理任务把房间收掉。保活帧会先来几个，最后一条读到的应该是 EOF。
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match waiting.read_frame().await {
                Ok(Some(frame)) if is_keepalive(&frame) => {}
                Ok(Some(_)) => panic!("房间里不该有别的数据"),
                Ok(None) | Err(_) => return true,
            }
        }
    })
    .await
    .expect("房间应在超时后被清掉");

    assert!(closed);
    shutdown.cancel();
}

#[tokio::test]
async fn admission_limiting_rejects_repeated_joins() {
    let (addr, shutdown) =
        spawn_relay(RelayConfig::new().with_password(PASSWORD).with_admission_limits(2, 2, Duration::from_secs(60)))
            .await;

    // 前两次加入同一个来源地址是允许的。
    let (_first, _) = join(addr, PASSWORD, "room-a").await;
    let (_second, _) = join(addr, PASSWORD, "room-b").await;

    let stream = TcpStream::connect(addr).await.expect("应能连上");
    let mut framed = Framed::new(stream);
    let mut randomness = OsRandomness;
    let mut handshake = ClientHandshake::new(PASSWORD.as_bytes(), "room-c", &mut randomness).expect("构造握手");

    let mut outbox = vec![handshake.start().expect("start")];
    let error = loop {
        for frame in std::mem::take(&mut outbox) {
            framed.write_frame(&frame).await.expect("应能写帧");
        }
        let frame = framed.read_frame().await.expect("应能读帧").expect("中继不该关闭连接");
        match handshake.step(&frame, &mut randomness) {
            Ok(step) => outbox = step.send,
            Err(error) => break error,
        }
    };

    assert_eq!(error, RelayError::RateLimited);
    shutdown.cancel();
}

#[tokio::test]
async fn closing_one_side_tears_down_the_pair() {
    let (addr, shutdown) = spawn_relay(RelayConfig::new().with_password(PASSWORD)).await;

    let (a, _) = join(addr, PASSWORD, ROOM).await;
    let (mut b, _) = join(addr, PASSWORD, ROOM).await;

    drop(a);

    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match b.read_frame().await {
                Ok(Some(frame)) if is_keepalive(&frame) => {}
                Ok(_) | Err(_) => return true,
            }
        }
    })
    .await
    .expect("对端断开后这一侧也应结束");

    assert!(ended);
    shutdown.cancel();
}

#[tokio::test]
async fn keepalive_frames_reach_the_waiting_peer() {
    let (addr, shutdown) =
        spawn_relay(RelayConfig::new().with_password(PASSWORD).with_keepalive_interval(Duration::from_millis(20)))
            .await;

    let (mut waiting, _) = join(addr, PASSWORD, ROOM).await;

    let frame = tokio::time::timeout(Duration::from_secs(2), waiting.read_frame())
        .await
        .expect("应能在超时前收到保活帧")
        .expect("应能读帧")
        .expect("不该是 EOF");
    assert_eq!(frame, KEEPALIVE.to_vec(), "保活帧的帧体必须是单个 0x01");

    shutdown.cancel();
}
