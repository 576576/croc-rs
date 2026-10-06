//! 数据面：N 条流上的分块搬运。
//!
//! 数据端口上跑的还是 comm 帧，但帧体是**裸的 AEAD 密文**（不走 JSON，也不走消息层）：
//!
//! ```text
//! 明文 = u64 LE 偏移 ‖ 数据          （可选：DEFLATE 包一层）
//! 帧体 = nonce(12) ‖ ciphertext ‖ tag(16)
//! ```
//!
//! 密钥就是 PAKE 派生出的那个 `encryptionKey`，与消息层共用。压缩开关**按文件**决定
//! （`FileInfo.IsCompressed` 与全局 `--no-compress` 一起看），两端必须一致，否则解出来
//! 是乱码。
//!
//! # 为什么每条流自己 `claim()`
//!
//! Go 用一个带锁的 `requestedChunkQueue` 把偏移分给 worker，后加入的流不会抢走已被
//! 认领的块。这里照做：队列在锁内只做整数运算，锁外才读写磁盘——**持锁做 IO 会让所有
//! 流串行化**，那正是多流想避免的事。

use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use croc_core::chunk::{CHUNK_SIZE, ChunkRanges};
use croc_net::Framed;
use croc_protocol::{codec, encrypt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

use crate::error::EngineError;
use crate::files::{self, Sink};

/// 一条数据流的上限：与 Go 的 `min(len(RelayPorts), 8)` 同一个理由——再多也不会更快。
pub const MAX_STREAMS: u8 = 8;

/// "还差哪些块"的游标。
///
/// 内部就是线路形态的 `Vec<i64>` 加一个游标，`claim()` 每次吐一个字节偏移。
/// **不做预展开**：一个 10 GB 文件有 30 万个块，展开成 `Vec<u64>` 是 2.4 MB 的浪费，
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
#[derive(Debug, Default)]
pub struct Progress {
    /// 已写块数。
    pub chunks: AtomicU64,
    /// 已写字节数。
    pub bytes: AtomicU64,
}

impl Progress {
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

/// 发送一个文件。
///
/// 所有流共享一个队列与一个文件句柄（`pread` 是线程安全的），谁先抢到谁发。
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
    cancel: CancellationToken,
) -> Result<(), EngineError> {
    let file = Arc::new(File::open(&path).map_err(|error| EngineError::file(&path, error))?);
    let mut handles = Vec::with_capacity(conns.len());

    for mut conn in conns {
        let file = Arc::clone(&file);
        let queue = Arc::clone(&queue);
        let cancel = cancel.clone();
        let path = path.clone();
        handles.push(tokio::spawn(async move {
            let buffer = vec![0u8; usize::try_from(CHUNK_SIZE).unwrap_or(32 * 1024)];
            let mut payload = vec![0u8; 8 + buffer.len()];
            let mut randomness = croc_net::OsRandomness;

            loop {
                // 锁内只做整数运算；读写磁盘与网络都在锁外。
                let Some(offset) =
                    queue.lock().map_err(|_| EngineError::Config("chunk queue poisoned".to_owned()))?.claim()
                else {
                    return Ok::<(), EngineError>(());
                };

                let want = buffer.len();
                let read = tokio::select! {
                    () = cancel.cancelled() => return Err(EngineError::Cancelled),
                    result = tokio::task::spawn_blocking({
                        let file = Arc::clone(&file);
                        move || {
                            let mut slice = vec![0u8; want];
                            let read = files::read_at(&file, &mut slice, offset)?;
                            slice.truncate(read);
                            Ok::<Vec<u8>, std::io::Error>(slice)
                        }
                    }) => {
                        result.map_err(|error| EngineError::file(&path, error))?
                            .map_err(|error| EngineError::file(&path, error))?
                    }
                };
                if read.is_empty() {
                    return Err(EngineError::file(&path, format!("short read at offset {offset}")));
                }

                // 前 8 字节是偏移（小端），后面是数据。
                payload.truncate(8 + read.len());
                payload
                    .get_mut(..8)
                    .ok_or_else(|| EngineError::Config("payload header".to_owned()))?
                    .copy_from_slice(&offset.to_le_bytes());
                payload
                    .get_mut(8..)
                    .ok_or_else(|| EngineError::Config("payload body".to_owned()))?
                    .copy_from_slice(&read);

                let plain = if compress { codec::compress(&payload)? } else { payload.clone() };
                let nonce = croc_protocol::random::nonce(&mut randomness);
                let frame = encrypt(&plain, &key, &nonce)?;

                tokio::select! {
                    () = cancel.cancelled() => return Err(EngineError::Cancelled),
                    result = conn.write_frame(&frame) => result?,
                }
                payload.truncate(8 + buffer.len());
            }
        }));
    }

    join_all(handles).await
}

/// 接收一个文件。返回进度，**由调用方**判断是否收全。
///
/// # 为什么收全判定不在这里
///
/// 一条流写完最后一块时，另外几条可能正阻塞在 `read_frame` 上。如果每条流都自己
/// 判"没写全 = 断线"，那它们会在计数器更新之前抢到 EOF，把一次成功的传输报成失败。
/// 所以：**任何流看到 EOF 都算正常结束**，是否收全留给调用方在全部流收工之后判定——
/// 那才是唯一没有竞态的时刻。
///
/// # Errors
///
/// 解密失败（对端用了别的密钥 / 压缩设置不一致）、写盘失败、偏移越界。
/// 对端提前关闭连接**不算错误**（见上）。
pub async fn receive_file(
    conns: Vec<Framed<TcpStream>>,
    sink: Arc<Sink>,
    expected_chunks: u64,
    expected_bytes: u64,
    key: [u8; 32],
    compress: bool,
    cancel: CancellationToken,
) -> Result<Arc<Progress>, EngineError> {
    let progress = Arc::new(Progress::default());
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
            loop {
                if progress.is_complete(expected_chunks, expected_bytes) {
                    done.cancel();
                    return Ok::<(), EngineError>(());
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
                let offset =
                    u64::from_le_bytes(header.try_into().map_err(|_| EngineError::Io("chunk offset".to_owned()))?);
                sink.write_at(offset, data)?;

                progress.chunks.fetch_add(1, Ordering::Relaxed);
                progress.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
            }
        }));
    }

    join_all(handles).await?;
    Ok(progress)
}

/// 一块明文的上限：32 KB 数据 + 8 字节偏移，再留一倍余量给"压缩后反而变大"的情况。
const MAX_CHUNK_PLAINTEXT: usize = 2 * (CHUNK_SIZE as usize + 8);

/// 等所有流结束；任意一条出错就整体失败（其余流会被 `cancel` 收掉）。
async fn join_all(handles: Vec<tokio::task::JoinHandle<Result<(), EngineError>>>) -> Result<(), EngineError> {
    let mut first_error = None;
    for handle in handles {
        match handle.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => first_error = first_error.or(Some(error)),
            Err(join_error) => {
                first_error = first_error.or_else(|| Some(EngineError::Io(join_error.to_string())));
            }
        }
    }
    first_error.map_or(Ok(()), Err)
}

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
        let progress = Progress::default();
        assert!(!progress.is_complete(4, 100));
        progress.chunks.store(4, Ordering::Relaxed);
        assert!(progress.is_complete(4, 100), "块数够了就算完");

        let progress = Progress::default();
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
