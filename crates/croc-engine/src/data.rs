//! 数据面：N 条流上的分块搬运。
//!
//! 数据端口上跑的还是 comm 帧，但帧体是裸的 AEAD 密文（不走 JSON，也不走消息层）：
//!
//! ```text
//! 明文 = u64 LE 偏移 ‖ 数据          （可选：DEFLATE 包一层）
//! 帧体 = nonce(12) ‖ ciphertext ‖ tag(16)
//! ```
//!
//! 密钥就是 PAKE 派生出的那个 `encryptionKey`，与消息层共用。压缩开关按文件决定
//! （`FileInfo.IsCompressed` 与全局 `--no-compress` 一起看），两端必须一致，否则解出来
//! 是乱码。
//!
//! # 为什么每条流自己 `claim()`
//!
//! Go 用一个带锁的 `requestedChunkQueue` 把偏移分给 worker，后加入的流不会抢走已被
//! 认领的块。这里照做：队列在锁内只做整数运算，锁外才读写磁盘——持锁做 IO 会让所有
//! 流串行化，那正是多流想避免的事。

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use croc_core::RoomName;
use croc_core::chunk::{CHUNK_SIZE, ChunkRanges};
use croc_net::Framed;
use croc_protocol::{codec, encrypt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::dns;
use crate::error::EngineError;
use crate::files::{self, Sink};
use crate::relay;
use crate::throttle::Throttle;
use crate::{EngineEvent, EventSink, emit};

/// 一条数据流的上限：与 Go 的 `min(len(RelayPorts), 8)` 同一个理由——再多也不会更快。
pub const MAX_STREAMS: u8 = 8;

/// 数据面连接的持有者：一次传输只建一次，全程复用。
///
/// # 为什么必须复用
///
/// 数据端口的房间名是 `{room}-{j}`，而中继的一个房间只容得下两端各一条连接。要是每条
/// 文件都重建一次，第二条文件就会撞上自己上一条文件还开着的连接，中继回 `room full`
/// —— 表现就是"单文件传得好好的，目录一传就断在第二个文件上"。
///
/// Go 也是这么做的：连接在 PAKE 完成时建一次（`openRelayChannels()`），留到整个 attempt
/// 结束（`c.dataConns`）。
///
/// # 为什么可以复用
///
/// 发送方只发被请求的块（[`ChunkQueue`] 是认领制，一块不多），接收方数够就收工，
/// 所以文件之间不会留下半条帧。即便有（中继会往数据连接里插 keepalive），
/// [`Framed`] 的缓冲是持久的、`read_frame()` 会从缓冲里接着解析，也不会串包。
///
/// # 连接什么时候关
///
/// 这个结构体被 `drop` 的时候——也就是整个传输结束、`finished` 已交换之后。
/// 中途关连接会让中继把还没读走的数据连同连接一起丢掉（见 [`send_file`] 的说明）。
pub struct DataPlane {
    relay: String,
    password: String,
    room: RoomName,
    ports: Vec<u16>,
    streams: u8,
    proxy: crate::proxy::Proxy,
    dns: dns::Mode,
    conns: Mutex<Option<Vec<Framed<TcpStream>>>>,
}

impl core::fmt::Debug for DataPlane {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("DataPlane")
            .field("ports", &self.ports)
            .field("streams", &self.streams)
            .field("connected", &self.conns.lock().is_ok_and(|guard| guard.is_some()))
            .finish_non_exhaustive()
    }
}

impl DataPlane {
    /// 建一个还没连的持有者。
    #[must_use]
    pub fn new(
        relay: String,
        password: String,
        room: RoomName,
        ports: Vec<u16>,
        streams: u8,
        proxy: crate::proxy::Proxy,
        dns: dns::Mode,
    ) -> Self {
        Self { relay, password, room, ports, streams, proxy, dns, conns: Mutex::new(None) }
    }

    /// 要开几条流（也等于 `ports` 里实际会用的条数）。
    #[must_use]
    pub fn width(&self) -> usize {
        usize::from(self.streams).min(self.ports.len())
    }

    /// 取走连接；还没建过就现建。
    ///
    /// # Errors
    ///
    /// 中继地址不合法，或任意一条数据连接握不上手。
    pub async fn acquire(&self) -> Result<Vec<Framed<TcpStream>>, EngineError> {
        // 锁只圈住"取走"这一步。建连接要走 TCP + 中继握手，几百毫秒起步；`MutexGuard`
        // 又恰好不是 `Send`，持着它跨 `await` 连编译都过不去。
        let reused = {
            let mut guard = self.conns.lock().map_err(|_| EngineError::Config("data plane poisoned".to_owned()))?;
            guard.take()
        };
        if let Some(conns) = reused {
            return Ok(conns);
        }

        let (host, _) = relay::split_host_port(&self.relay)?;
        let mut conns = Vec::with_capacity(self.width());
        for position in 0..self.width() {
            let port = *self.ports.get(position).unwrap_or(&0);
            let index = u8::try_from(position).unwrap_or(u8::MAX);
            conns.push(
                relay::connect_data(&host, port, &self.password, &self.room, index, &self.proxy, self.dns).await?,
            );
        }
        Ok(conns)
    }

    /// 把连接放回去。不是关闭——见类型头的说明。
    pub fn release(&self, conns: Vec<Framed<TcpStream>>) {
        match self.conns.lock() {
            Ok(mut guard) => *guard = Some(conns),
            // 锁中毒说明别处 panic 过；这时把连接丢掉就行（`drop` 会关 socket）。
            Err(_) => drop(conns),
        }
    }
}

/// "还差哪些块"的游标。
///
/// 内部就是线路形态的 `Vec<i64>` 加一个游标，`claim()` 每次吐一个字节偏移。
/// 不做预展开：一个 10 GB 文件有 30 万个块，展开成 `Vec<u64>` 是 2.4 MB 的浪费，
/// 而这个结构体只需要几十字节。
#[derive(Debug, Clone)]
pub struct ChunkQueue {
    wire: Vec<i64>,
    /// 当前段在 `wire` 里的下标（指向 `start`，所以是奇数）。
    next: usize,
    /// 当前段已经发出去几块。
    step: i64,
    /// 总共要发几块。
    total: u64,
    /// 已经发出去几块。
    claimed: u64,
}

impl ChunkQueue {
    /// 从分片区间构造。空列表按"整个文件"展开（与 Go 的 `newRequestedChunkQueue` 一致）。
    #[must_use]
    pub fn new(ranges: &ChunkRanges, file_size: u64) -> Self {
        let materialized = ranges.materialize(file_size);
        let wire = materialized.as_wire().to_vec();
        let total = wire
            .iter()
            .skip(2)
            .step_by(2)
            .filter_map(|count| u64::try_from(*count).ok())
            .filter(|count| *count > 0)
            .sum();
        Self { wire, next: 1, step: 0, total, claimed: 0 }
    }

    /// 取下一个要发的偏移。取完返回 `None`。
    pub fn claim(&mut self) -> Option<u64> {
        while self.next + 1 < self.wire.len() {
            let start = *self.wire.get(self.next).unwrap_or(&0);
            let count = *self.wire.get(self.next + 1).unwrap_or(&0);
            if self.step < count {
                let chunk_size = *self.wire.first().unwrap_or(&(CHUNK_SIZE as i64));
                let offset = start.checked_add(self.step.checked_mul(chunk_size)?)?;
                self.step += 1;
                self.claimed += 1;
                return u64::try_from(offset).ok();
            }
            self.step = 0;
            self.next += 2;
        }
        None
    }

    /// 总共要发几块。
    #[must_use]
    pub const fn total(&self) -> u64 {
        self.total
    }

    /// 已经取走几块。
    #[must_use]
    pub const fn claimed(&self) -> u64 {
        self.claimed
    }
}

/// 接收方的进度：已写几块、已写几字节。
pub struct Progress {
    /// 已写块数。
    pub chunks: AtomicU64,
    /// 已写字节数。
    pub bytes: AtomicU64,
    /// 这个文件该有多少字节，用于上报百分比。
    total: u64,
    /// 上报节流：与发送侧一样 100 ms 一次。
    last: Mutex<Instant>,
    /// 事件出口。`None` 表示这次接收不需要进度（例如内部调用）。
    sink: Option<EventSink>,
}

impl Progress {
    /// 收一个 `total` 字节的文件。`sink` 为 `None` 时只计数、不上报。
    #[must_use]
    pub fn new(total: u64, sink: Option<EventSink>) -> Self {
        Self { chunks: AtomicU64::new(0), bytes: AtomicU64::new(0), total, last: Mutex::new(Instant::now()), sink }
    }

    /// 记下一块，必要时上报一次。
    pub fn add(&self, bytes: u64) {
        self.chunks.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(bytes, Ordering::Relaxed);
        let Some(sink) = self.sink.as_ref() else { return };
        let Ok(mut last) = self.last.lock() else { return };
        let now = Instant::now();
        if now.saturating_duration_since(*last) < Duration::from_millis(100) {
            return;
        }
        *last = now;
        emit(sink, EngineEvent::FileProgress { transferred: self.bytes.load(Ordering::Relaxed), total: self.total });
    }

    /// 收完了吗。
    ///
    /// Go 的判据是 `TotalChunksTransferred == CurrentFileChunkCount || TotalSent == Size`。
    /// 两条都要，因为空列表请求（"整个文件"）在 `ChunkRangesCount` 里折算的块数可能与
    /// 实际发出的块数不同。
    #[must_use]
    pub fn is_complete(&self, expected_chunks: u64, expected_bytes: u64) -> bool {
        let chunks = self.chunks.load(Ordering::Relaxed);
        let bytes = self.bytes.load(Ordering::Relaxed);
        (expected_chunks > 0 && chunks >= expected_chunks) || (expected_bytes > 0 && bytes >= expected_bytes)
    }
}

/// 发送一个文件。返回数据连接，调用方必须继续持有它们。
///
/// # 为什么要把连接还回去
///
/// 数据写完就 `drop` 掉 `Framed`，socket 会立刻关闭：数据还在中继那侧的内核缓冲里没
/// 被读走，Windows 于是发 RST 而不是 FIN——
/// 中继的 `copy_bidirectional` 直接报错退出，已经写出去的分块全部丢弃。
/// 表现就是"发送方说发完了、接收方一个字节没收到"。
///
/// Go 的发送方同样把数据连接留到整个 attempt 结束（`c.dataConns`），不是在 worker
/// 收工时关。所以这里把连接原样交回调用方，由它持到传输结束。
///
/// # Errors
///
/// 读文件失败、写 socket 失败。被取消时返回 [`EngineError::Cancelled`]。
pub async fn send_file(
    conns: Vec<Framed<TcpStream>>,
    path: PathBuf,
    queue: Arc<Mutex<ChunkQueue>>,
    key: [u8; 32],
    compress: bool,
    throttle: Option<Arc<Throttle>>,
    cancel: CancellationToken,
) -> Result<Vec<Framed<TcpStream>>, EngineError> {
    let file = Arc::new(File::open(&path).map_err(|error| EngineError::file(&path, error))?);
    let mut handles = Vec::with_capacity(conns.len());

    for conn in conns {
        let file = Arc::clone(&file);
        let queue = Arc::clone(&queue);
        let cancel = cancel.clone();
        let path = path.clone();
        let throttle = throttle.clone();
        handles.push(tokio::spawn(async move {
            let result = send_stream(conn, file, queue, key, compress, throttle, cancel, path).await;
            // 无论成败都把连接还回去——关连接是调用方的事，见函数头的说明。
            result
        }));
    }

    let mut kept = Vec::with_capacity(handles.len());
    let mut first_error = None;
    for handle in handles {
        match handle.await {
            Ok((conn, Ok(()))) => kept.push(conn),
            Ok((conn, Err(error))) => {
                kept.push(conn);
                first_error = first_error.or(Some(error));
            }
            Err(join_error) => first_error = first_error.or_else(|| Some(EngineError::Io(join_error.to_string()))),
        }
    }
    first_error.map_or(Ok(kept), Err)
}

/// 一条数据流上的搬运循环。
#[allow(clippy::too_many_arguments, reason = "这些参数各自独立，打包成结构体只是把同一个列表挪个地方")]
async fn send_stream(
    mut conn: Framed<TcpStream>,
    file: Arc<File>,
    queue: Arc<Mutex<ChunkQueue>>,
    key: [u8; 32],
    compress: bool,
    throttle: Option<Arc<Throttle>>,
    cancel: CancellationToken,
    path: PathBuf,
) -> (Framed<TcpStream>, Result<(), EngineError>) {
    let buffer = vec![0u8; usize::try_from(CHUNK_SIZE).unwrap_or(32 * 1024)];
    let mut payload = vec![0u8; 8 + buffer.len()];
    let mut randomness = croc_net::OsRandomness;

    loop {
        // 锁内只做整数运算；读写磁盘与网络都在锁外。
        // 用块把锁的生命周期收干净：`MutexGuard` 不是 `Send`，只要它"可能活到
        // await 之后"，整个 future 就不是 `Send`，`tokio::spawn` 直接编不过。
        let claimed = {
            let Ok(mut guard) = queue.lock() else {
                return (conn, Err(EngineError::Config("chunk queue poisoned".to_owned())));
            };
            guard.claim()
        };
        let Some(offset) = claimed else {
            // 队列空了：这条流收工。
            return (conn, Ok(()));
        };

        let want = buffer.len();
        let read = tokio::select! {
            () = cancel.cancelled() => return (conn, Err(EngineError::Cancelled)),
            result = tokio::task::spawn_blocking({
                let file = Arc::clone(&file);
                move || {
                    let mut slice = vec![0u8; want];
                    let read = files::read_at(&file, &mut slice, offset)?;
                    slice.truncate(read);
                    Ok::<Vec<u8>, std::io::Error>(slice)
                }
            }) => match result {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error)) => return (conn, Err(EngineError::file(&path, error))),
                Err(error) => return (conn, Err(EngineError::file(&path, error))),
            },
        };
        if read.is_empty() {
            return (conn, Err(EngineError::file(&path, format!("short read at offset {offset}"))));
        }

        // `--throttleUpload`：令牌在读完之后、加密之前领，与 Go 的位置一致
        // 。桶是跨流共享的，限的是整次上传的速率。
        if let Some(throttle) = &throttle {
            let delay = throttle.reserve(u64::try_from(read.len()).unwrap_or(u64::MAX));
            if !delay.is_zero() {
                tokio::select! {
                    () = cancel.cancelled() => return (conn, Err(EngineError::Cancelled)),
                    () = tokio::time::sleep(delay) => {}
                }
            }
        }

        // 前 8 字节是偏移（小端），后面是数据。
        let mut header = [0u8; 8];
        header.copy_from_slice(&offset.to_le_bytes());
        payload.truncate(8 + read.len());
        match payload.get_mut(..8) {
            Some(slot) => slot.copy_from_slice(&header),
            None => return (conn, Err(EngineError::Config("payload header".to_owned()))),
        }
        match payload.get_mut(8..) {
            Some(slot) => slot.copy_from_slice(&read),
            None => return (conn, Err(EngineError::Config("payload body".to_owned()))),
        }

        let plain = match if compress { codec::compress(&payload) } else { Ok(payload.clone()) } {
            Ok(plain) => plain,
            Err(error) => return (conn, Err(error.into())),
        };
        let nonce = croc_protocol::random::nonce(&mut randomness);
        let frame = match encrypt(&plain, &key, &nonce) {
            Ok(frame) => frame,
            Err(error) => return (conn, Err(error.into())),
        };

        tokio::select! {
            () = cancel.cancelled() => return (conn, Err(EngineError::Cancelled)),
            result = conn.write_frame(&frame) => {
                if let Err(error) = result {
                    return (conn, Err(error.into()));
                }
            }
        }
        payload.truncate(8 + buffer.len());
    }
}

/// 接收一个文件。返回数据连接（连接是复用的，必须交回 [`DataPlane`]）与进度。
///
/// 收全与否由调用方判断，不在这里判——理由见下面这段。
///
/// # 为什么收全判定不在这里
///
/// 一条流写完最后一块时，另外几条可能正阻塞在 `read_frame` 上。如果每条流都自己
/// 判"没写全 = 断线"，那它们会在计数器更新之前抢到 EOF，把一次成功的传输报成失败。
/// 所以：任何流看到 EOF 都算正常结束，是否收全留给调用方在全部流收工之后判定——
/// 那才是唯一没有竞态的时刻。
///
/// # Errors
///
/// 解密失败（对端用了别的密钥 / 压缩设置不一致）、写盘失败、偏移越界。
/// 对端提前关闭连接不算错误（见上）。
#[allow(clippy::too_many_arguments, reason = "这条路径上的参数都是独立事实，打包成结构体只是换个地方写")]
pub async fn receive_file(
    conns: Vec<Framed<TcpStream>>,
    sink: Arc<Sink>,
    expected_chunks: u64,
    expected_bytes: u64,
    key: [u8; 32],
    compress: bool,
    cancel: CancellationToken,
    events: Option<EventSink>,
) -> Result<(Vec<Framed<TcpStream>>, Arc<Progress>), EngineError> {
    let progress = Arc::new(Progress::new(expected_bytes, events));
    // 收全之后用它把还在阻塞的流叫醒——否则小文件会让另外几条流一直挂着，
    // 直到发送方关掉连接（那可能要等一整个 RTT）。
    let done = CancellationToken::new();
    let mut handles = Vec::with_capacity(conns.len());

    for mut conn in conns {
        let sink = Arc::clone(&sink);
        let progress = Arc::clone(&progress);
        let cancel = cancel.clone();
        let done = done.clone();
        handles.push(tokio::spawn(async move {
            let result = receive_stream(
                &mut conn,
                &sink,
                &progress,
                expected_chunks,
                expected_bytes,
                key,
                compress,
                &cancel,
                &done,
            )
            .await;
            // 无论成败都把连接还回去——关连接是 [`DataPlane`] 的事。
            (conn, result)
        }));
    }

    let mut kept = Vec::with_capacity(handles.len());
    let mut first_error = None;
    for handle in handles {
        match handle.await {
            Ok((conn, Ok(()))) => kept.push(conn),
            Ok((conn, Err(error))) => {
                kept.push(conn);
                first_error = first_error.or(Some(error));
            }
            Err(join_error) => first_error = first_error.or_else(|| Some(EngineError::Io(join_error.to_string()))),
        }
    }
    // 出错时连接就地丢掉：调用方会中止整个传输，没什么可复用的了。
    first_error.map_or_else(|| Ok((kept, progress)), Err)
}

/// 一条接收流上的搬运循环。
#[allow(clippy::too_many_arguments, reason = "这些都是独立事实；打包成结构体只是把同一串东西挪个地方写")]
async fn receive_stream(
    conn: &mut Framed<TcpStream>,
    sink: &Sink,
    progress: &Progress,
    expected_chunks: u64,
    expected_bytes: u64,
    key: [u8; 32],
    compress: bool,
    cancel: &CancellationToken,
    done: &CancellationToken,
) -> Result<(), EngineError> {
    loop {
        if progress.is_complete(expected_chunks, expected_bytes) {
            done.cancel();
            return Ok(());
        }
        let frame = tokio::select! {
            () = cancel.cancelled() => return Err(EngineError::Cancelled),
            () = done.cancelled() => return Ok(()),
            result = conn.read_frame() => result?,
        };
        let Some(frame) = frame else {
            // 对端把数据连接关了：正常收尾。
            return Ok(());
        };
        if croc_protocol::is_keepalive(&frame) {
            continue;
        }

        let plain = codec::decrypt(&frame, &key)?;
        let plain = if compress { codec::decompress(&plain, MAX_CHUNK_PLAINTEXT)? } else { plain };
        if plain.len() < 8 {
            return Err(EngineError::Io(format!("data chunk is too short: {} bytes", plain.len())));
        }
        let (header, data) = plain.split_at(8);
        let offset = u64::from_le_bytes(header.try_into().map_err(|_| EngineError::Io("chunk offset".to_owned()))?);
        sink.write_at(offset, data)?;

        progress.add(data.len() as u64);
    }
}

/// 一块明文的上限：32 KB 数据 + 8 字节偏移，再留一倍余量给"压缩后反而变大"的情况。
const MAX_CHUNK_PLAINTEXT: usize = 2 * (CHUNK_SIZE as usize + 8);

/// 数据流要开几条。
#[must_use]
pub fn stream_count(relay_ports: usize) -> u8 {
    u8::try_from(relay_ports.min(usize::from(MAX_STREAMS))).unwrap_or(MAX_STREAMS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_range_list_means_the_whole_file() {
        let mut queue = ChunkQueue::new(&ChunkRanges::whole(), CHUNK_SIZE * 3);
        assert_eq!(queue.total(), 3);
        assert_eq!(queue.claim(), Some(0));
        assert_eq!(queue.claim(), Some(CHUNK_SIZE));
        assert_eq!(queue.claim(), Some(2 * CHUNK_SIZE));
        assert_eq!(queue.claim(), None);
        assert_eq!(queue.claimed(), 3);
    }

    #[test]
    fn explicit_ranges_are_walked_in_order() {
        // 块大小 32 KiB，请求 [块 1] 与 [块 3..4]。
        let wire = vec![CHUNK_SIZE as i64, CHUNK_SIZE as i64, 1, 3 * CHUNK_SIZE as i64, 2];
        let mut queue = ChunkQueue::new(&ChunkRanges::from_wire(wire), CHUNK_SIZE * 8);
        assert_eq!(queue.total(), 3);
        assert_eq!(queue.claim(), Some(CHUNK_SIZE));
        assert_eq!(queue.claim(), Some(3 * CHUNK_SIZE));
        assert_eq!(queue.claim(), Some(4 * CHUNK_SIZE));
        assert_eq!(queue.claim(), None);
    }

    #[test]
    fn a_zero_size_file_yields_nothing() {
        let mut queue = ChunkQueue::new(&ChunkRanges::whole(), 0);
        assert_eq!(queue.total(), 0);
        assert_eq!(queue.claim(), None);
    }

    #[test]
    fn progress_completes_on_either_criterion() {
        let progress = Progress::new(100, None);
        assert!(!progress.is_complete(4, 100));
        progress.chunks.store(4, Ordering::Relaxed);
        assert!(progress.is_complete(4, 100), "块数够了就算完");

        let progress = Progress::new(100, None);
        progress.bytes.store(100, Ordering::Relaxed);
        assert!(progress.is_complete(4, 100), "字节数够了也算完");
    }

    #[test]
    fn stream_counts_are_capped() {
        assert_eq!(stream_count(0), 0);
        assert_eq!(stream_count(4), 4);
        assert_eq!(stream_count(8), 8);
        assert_eq!(stream_count(100), 8);
    }
}
