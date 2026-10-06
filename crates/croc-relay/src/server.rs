//! 接受循环与单连接处理。
//!
//! # 一条连接的完整生命周期
//!
//! ```text
//! accept
//!   ├─ 抢握手名额（满了就直接断开，连房间都不占）
//!   ├─ 握手（整体有 deadline；ping 走探活分支，回 pong 后断开）
//!   ├─ 限流判定（超限 ⇒ 加密回 "rate limited"）
//!   ├─ 房间准入
//!   │    ├─ 房间已满      ⇒ 加密回 "room full"
//!   │    ├─ 第二位        ⇒ 加密回 "ok"，然后把连接交给第一位，本任务结束
//!   │    └─ 第一位        ⇒ 加密回 "ok"，每秒发一次保活，直到第二位到来
//!   └─ 裸转发（双向 copy），结束或出错时删掉房间
//! ```
//!
//! # 为什么"第二位"要把连接送出去
//!
//! 见 [`crate::room`] 的模块文档。简单说：Rust 里 socket 的所有权没法在 task 之间借，
//! 所以由第一位的 task 拿着两条连接做转发，第二位把自己的连接通过 `oneshot` 送过去。

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use croc_net::{Framed, OsRandomness};
use croc_protocol::{Admission, KEEPALIVE, Randomness, ServerHandshake, ServerOutcome, ServerSession};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::admission::{AdmissionLimiter, canonical_source};
use crate::config::RelayConfig;
use crate::room::{RoomJoin, RoomRegistry};

/// croc 中继服务端。
#[derive(Debug)]
pub struct Relay {
    config: RelayConfig,
    rooms: RoomRegistry,
    admission: AdmissionLimiter,
    handshake_slots: Arc<Semaphore>,
}

impl Relay {
    /// 用给定配置创建中继。
    ///
    /// 创建本身不做 IO，也不启动后台任务——真正的副作用发生在 [`Relay::run`]。
    #[must_use]
    pub fn new(config: RelayConfig) -> Self {
        let handshake_slots = Arc::new(Semaphore::new(config.max_pending_handshakes()));
        let rooms = RoomRegistry::new(config.max_rooms_open());
        let admission =
            AdmissionLimiter::new(config.source_join_limit(), config.room_join_limit(), config.join_limit_window());
        Self { config, rooms, admission, handshake_slots }
    }

    /// 配置。
    #[must_use]
    pub fn config(&self) -> &RelayConfig {
        &self.config
    }

    /// 当前还在等配对的房间数（诊断用）。
    #[must_use]
    pub fn waiting_rooms(&self) -> usize {
        self.rooms.waiting()
    }

    /// 绑定一个监听地址。
    ///
    /// # Errors
    ///
    /// 地址不合法或端口被占用。
    pub async fn bind(addr: impl tokio::net::ToSocketAddrs) -> io::Result<TcpListener> {
        TcpListener::bind(addr).await
    }

    /// 跑起来：起后台清理任务，然后为每个 listener 起一个接受循环。
    ///
    /// 返回时所有 listener 都已停止。`shutdown` 一旦被取消，接受循环、后台清理、以及
    /// 所有还在等配对的房间都会结束。
    ///
    /// # Errors
    ///
    /// 接受连接时的致命错误（例如 listener 被外部关闭）。
    pub async fn run(self: &Arc<Self>, listeners: Vec<TcpListener>, shutdown: CancellationToken) -> io::Result<()> {
        let cleanup = tokio::spawn(cleanup_loop(Arc::clone(self), shutdown.clone()));

        let mut acceptors = Vec::with_capacity(listeners.len());
        for listener in listeners {
            acceptors.push(tokio::spawn(accept_loop(Arc::clone(self), listener, shutdown.clone())));
        }

        let mut first_error = None;
        for acceptor in acceptors {
            match acceptor.await {
                Ok(Ok(())) => {}
                Ok(Err(error)) => first_error = first_error.or(Some(error)),
                Err(join_error) => {
                    first_error = first_error.or_else(|| Some(io::Error::other(join_error)));
                }
            }
        }

        cleanup.abort();
        self.rooms.clear();
        first_error.map_or(Ok(()), Err)
    }

    /// 处理一条已完成握手的连接：限流 → 房间准入 → 等待/对接。
    async fn admit_and_pair(self: &Arc<Self>, mut framed: Framed<TcpStream>, session: ServerSession) {
        let mut randomness = OsRandomness;
        let room = session.room.clone();

        match self.rooms.join(&room, Instant::now()) {
            RoomJoin::Full => {
                tracing::debug!(%room, "房间已满，拒绝");
                write_admission(&mut framed, &session, Admission::RoomFull, &mut randomness).await;
            }
            RoomJoin::Second(waiter) => {
                // 顺序很重要：先把准入帧发出去（`write_frame` 会 flush），再把连接交给
                // 第一位。反过来的话，第一位发来的应用帧可能抢先到达，被第二位的客户端
                // 当成中继的响应。
                if write_admission(&mut framed, &session, Admission::Allowed, &mut randomness).await {
                    tracing::debug!(%room, "房间配对成功");
                    if waiter.send(framed).is_err() {
                        tracing::debug!(%room, "第一位已经离开，丢弃配对的连接");
                    }
                }
            }
            RoomJoin::First { mut receiver, evicted } => {
                if let Some(evicted) = evicted {
                    tracing::debug!(room = %evicted, "等待房间数到达上限，淘汰最老的房间");
                }
                if !write_admission(&mut framed, &session, Admission::Allowed, &mut randomness).await {
                    self.rooms.remove(&room);
                    return;
                }
                tracing::debug!(%room, "房间已建立，等待第二位");

                let Some(partner) = self.wait_for_partner(&mut framed, &mut receiver, &room).await else {
                    self.rooms.remove(&room);
                    return;
                };

                // 房间要留到转发结束才删：留着它，第三个客户端才会得到 "room full"
                // 而不是"房间不存在"从而自己新建一个。Go 也是转发完才 `deleteRoom`。
                tracing::debug!(%room, "开始转发");
                if let Err(error) = pipe(framed, partner).await {
                    tracing::debug!(%room, %error, "转发结束");
                }
                self.rooms.remove(&room);
            }
        }
    }

    /// 等第二位连接，同时按配置的间隔发保活帧。
    ///
    /// 返回 `None` 表示应当放弃这个房间：连接断了、或者房间被清理/淘汰了。
    async fn wait_for_partner(
        &self,
        framed: &mut Framed<TcpStream>,
        receiver: &mut tokio::sync::oneshot::Receiver<Framed<TcpStream>>,
        room: &str,
    ) -> Option<Framed<TcpStream>> {
        let mut keepalive = tokio::time::interval(self.config.keepalive_interval());
        // 默认行为是"落后就追赶"，那会在网络抖动后一次性补发好几个保活帧。
        // 保活只需要"让 NAT 别掐连接"，补发没有意义。
        keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // `interval` 的第一次 tick 立刻完成，丢掉它。
        keepalive.tick().await;

        loop {
            tokio::select! {
                result = &mut *receiver => {
                    if result.is_err() {
                        tracing::debug!(%room, "房间已被清理或淘汰");
                    }
                    break result.ok();
                }
                _ = keepalive.tick() => {
                    if framed.write_frame(&KEEPALIVE).await.is_err() {
                        tracing::debug!(%room, "等待配对时连接断开");
                        break None;
                    }
                }
            }
        }
    }
}

/// 一个监听地址上的接受循环。
async fn accept_loop(relay: Arc<Relay>, listener: TcpListener, shutdown: CancellationToken) -> io::Result<()> {
    loop {
        let accepted = tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            result = listener.accept() => result,
        };

        let (stream, peer) = match accepted {
            Ok(pair) => pair,
            Err(error) => {
                // 单条连接 accept 失败（例如对端在我们 accept 之前就断了）不该拖垮整个
                // listener。Go 版在这里直接返回错误并停掉服务器，那太脆了。
                tracing::debug!(%error, "接受连接失败");
                continue;
            }
        };

        // 握手并发闸门：满了就直接断开，不让它占内存和房间。
        let Ok(permit) = Arc::clone(&relay.handshake_slots).try_acquire_owned() else {
            tracing::debug!(%peer, "并发握手数到达上限，拒绝");
            continue;
        };

        let relay = Arc::clone(&relay);
        tokio::spawn(async move {
            handle_connection(relay, stream, peer, permit).await;
        });
    }
}

/// 处理一条新连接。
async fn handle_connection(relay: Arc<Relay>, stream: TcpStream, peer: SocketAddr, _permit: OwnedSemaphorePermit) {
    // 握手帧都很小，Nagle 只会平白加上几十毫秒。
    let _ = stream.set_nodelay(true);
    let mut framed = Framed::new(stream);

    // `tokio::time::Instant` 而不是 `std::time::Instant`：只有前者能在 tokio 的
    // 时间暂停（`start_paused`）下被测试驱动。
    let deadline = tokio::time::Instant::now() + relay.config.handshake_timeout();
    let mut handshake =
        ServerHandshake::new(relay.config.password().to_owned(), relay.config.banner().to_owned(), peer.to_string());

    let Some(session) = drive_handshake(&mut framed, &mut handshake, deadline).await else {
        return;
    };

    let source = canonical_source(peer);
    if !relay.admission.allow(&source, &session.room) {
        tracing::debug!(%source, "触发准入限流");
        let mut randomness = OsRandomness;
        write_admission(&mut framed, &session, Admission::RateLimited, &mut randomness).await;
        return;
    }

    relay.admit_and_pair(framed, session).await;
}

/// 把握手跑到终态。返回 `None` 表示连接应当被关闭。
async fn drive_handshake(
    framed: &mut Framed<TcpStream>,
    handshake: &mut ServerHandshake,
    deadline: tokio::time::Instant,
) -> Option<ServerSession> {
    let mut randomness = OsRandomness;
    loop {
        let frame = match tokio::time::timeout_at(deadline, framed.read_frame()).await {
            Ok(Ok(Some(frame))) => frame,
            Ok(Ok(None)) => return None,
            Ok(Err(error)) => {
                tracing::debug!(%error, "握手期间读取失败");
                return None;
            }
            Err(_) => {
                tracing::debug!("握手超时");
                return None;
            }
        };

        let step = match handshake.step(&frame, &mut randomness) {
            Ok(step) => step,
            Err(error) => {
                tracing::debug!(%error, "握手状态机失败");
                return None;
            }
        };

        for outgoing in &step.send {
            if let Err(error) = framed.write_frame(outgoing).await {
                tracing::debug!(%error, "握手期间写入失败");
                return None;
            }
        }

        match step.outcome {
            Some(ServerOutcome::Accepted(session)) => return Some(session),
            Some(ServerOutcome::Ping) => {
                tracing::trace!("收到探活请求");
                return None;
            }
            Some(ServerOutcome::Rejected(error)) => {
                tracing::debug!(%error, "握手被拒绝");
                return None;
            }
            None => {}
        }
    }
}

/// 加密回送一条准入结果。返回是否成功。
async fn write_admission(
    framed: &mut Framed<TcpStream>,
    session: &ServerSession,
    outcome: Admission,
    randomness: &mut impl Randomness,
) -> bool {
    match session.admit(outcome, randomness) {
        Ok(frame) => framed.write_frame(&frame).await.is_ok(),
        Err(error) => {
            tracing::debug!(%error, "无法加密准入结果");
            false
        }
    }
}

/// 把两条连接对接起来。
///
/// 先把各自缓冲里残留的字节转发过去：握手是按帧读的，但一次底层读可能顺手把下一帧的
/// 开头也拉进缓冲，这些字节属于应用数据，丢了就会让对端卡在等半个帧上。
async fn pipe(mut left: Framed<TcpStream>, mut right: Framed<TcpStream>) -> io::Result<()> {
    let left_pending = left.take_buffered();
    if !left_pending.is_empty() {
        right.get_mut().write_all(&left_pending).await?;
    }
    let right_pending = right.take_buffered();
    if !right_pending.is_empty() {
        left.get_mut().write_all(&right_pending).await?;
    }

    tokio::io::copy_bidirectional(left.get_mut(), right.get_mut()).await?;
    Ok(())
}

/// 定期清掉超过 TTL 还没配对的房间。
async fn cleanup_loop(relay: Arc<Relay>, shutdown: CancellationToken) {
    let interval = relay.config.room_cleanup_interval();
    let ttl = relay.config.room_ttl();
    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    ticker.tick().await;

    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticker.tick() => {
                let expired = relay.rooms.expire(Instant::now(), ttl);
                for room in &expired {
                    tracing::debug!(%room, "房间超时被清理");
                }
                if !expired.is_empty() {
                    tracing::debug!(count = expired.len(), interval = ?interval, "清理了过期房间");
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_relay_has_no_rooms() {
        let relay = Relay::new(RelayConfig::default());
        assert_eq!(relay.waiting_rooms(), 0);
    }

    #[test]
    fn the_handshake_gate_matches_the_configured_limit() {
        let relay = Relay::new(RelayConfig::default().with_max_pending_handshakes(3));
        assert_eq!(relay.handshake_slots.available_permits(), 3);
    }
}
