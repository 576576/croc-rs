//! 发送方驱动：把 `croc-core::Session` 的效果落到 socket 与磁盘上。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use croc_core::codephrase::Components;
use croc_core::{Effect, Event, InEvent, Outcome, Role, Session, SessionConfig};
use croc_net::OsRandomness;
use croc_protocol::{TailcatDeclaration, declare};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::data::{ChunkQueue, send_file, stream_count};
use crate::error::EngineError;
use crate::files;
use crate::relay;
use crate::{EngineEvent, EventSink, emit};

/// 一次发送所需的全部参数。
#[derive(Clone)]
pub struct SendRequest {
    /// 中继地址（`host:port`）。
    pub relay: String,
    /// 中继密码。
    pub relay_password: String,
    /// 从口令码解出的房间名与 PAKE 口令。
    pub components: Components,
    /// 原始口令码，用来打印给用户。**不是秘密**（要交给对方），但它是这次会话的
    /// 唯一凭据，所以只打印、不进日志。
    pub code: String,
    /// 本端选择的曲线。接收方会覆盖它（曲线由接收方决定）。
    pub curve: croc_pake::CurveKind,
    /// 要发送的路径（文件或目录）。
    pub paths: Vec<PathBuf>,
    /// 哈希算法（目前只支持 `xxhash`）。
    pub hash_algorithm: String,
    /// 关掉压缩。
    pub no_compress: bool,
    /// 本端机器指纹。
    pub machine_id: String,
    /// 事件出口。
    pub events: EventSink,
}

impl core::fmt::Debug for SendRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // `events` 是 `dyn Fn`，没有 `Debug`；其余字段逐个列出就够了。
        formatter
            .debug_struct("SendRequest")
            .field("relay", &self.relay)
            .field("paths", &self.paths)
            .field("hash_algorithm", &self.hash_algorithm)
            .finish_non_exhaustive()
    }
}

/// 发送结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// 实际传出去的文件数。
    pub files: usize,
    /// 实际传出去的字节数。
    pub bytes: u64,
}

/// 跑一次发送。
///
/// # Errors
///
/// 中继连不上、对端拒绝、协议违规、读文件失败，或者被取消。
pub async fn send(request: SendRequest, cancel: CancellationToken) -> Result<Summary, EngineError> {
    // 清单在连中继之前就准备好：算哈希要读一遍全部文件，出问题（路径不存在、算法不支持）
    // 应当在这一步就暴露，而不是等对端等在那儿的时候才失败。
    emit(&request.events, EngineEvent::Status("hashing the files…".to_owned()));
    let (mut entries, empty_folders) = files::collect(&request.paths, &request.hash_algorithm)?;
    let compress = !request.no_compress;
    for entry in &mut entries {
        // 逐文件压缩开关：接收方靠这个字段决定要不要解压，所以必须随清单一起过去。
        entry.info.is_compressed = compress && entry.info.size > 0;
    }
    let total_bytes: u64 = entries.iter().filter_map(|entry| u64::try_from(entry.info.size).ok()).sum();
    let info = files::sender_info(&entries, empty_folders, &request.machine_id);

    let mut link = relay::connect(&request.relay, &request.relay_password, &request.components.room_name).await?;
    let ports = link.data_ports();
    if ports.is_empty() {
        return Err(EngineError::Relay("the relay banner carried no data ports".to_owned()));
    }
    emit(&request.events, EngineEvent::Status("waiting for the recipient…".to_owned()));

    let streams = stream_count(ports.len());
    let session_config = SessionConfig {
        role: Role::Sender,
        room: request.components.room_name.as_str(),
        passphrase: request.components.pake_passphrase.as_bytes(),
        curve: request.curve,
        features: declare(TailcatDeclaration::Unsupported),
        relay_ports: streams,
        machine_id: request.machine_id.clone(),
        external_ip: link.external_ip.clone(),
    };
    let mut session = Session::new_sender(&session_config, info)?;
    let mut randomness = OsRandomness;

    // 数据面完成的通知走这条通道。控制循环用 `select!` 同时等它和对端的帧，
    // 所以不需要把 `Framed` 拆成两半（拆开会丢掉握手时顺手读进缓冲的那半条帧）。
    let (data_tx, mut data_rx) = mpsc::channel::<InEvent>(4);
    let mut transferred: u64 = 0;

    let mut effects = session.handle(InEvent::PeerConnected, &mut randomness)?;
    loop {
        for effect in core::mem::take(&mut effects) {
            match effect {
                Effect::Send(frame) => link.framed.write_frame(&frame).await?,
                Effect::SendFile { index, ranges, streams } => {
                    let entry = entries.get(usize::try_from(index).unwrap_or(usize::MAX)).ok_or_else(|| {
                        EngineError::Config(format!("the peer asked for file #{index}, which does not exist"))
                    })?;
                    emit(
                        &request.events,
                        EngineEvent::FileStarted {
                            name: entry.info.name.clone(),
                            size: u64::try_from(entry.info.size).unwrap_or(0),
                        },
                    );
                    spawn_sender(
                        &request,
                        &ports,
                        entry.path.clone(),
                        &ranges,
                        u64::try_from(entry.info.size).unwrap_or(0),
                        entry.info.is_compressed,
                        streams,
                        session.data_key(),
                        data_tx.clone(),
                        cancel.clone(),
                    )
                    .await?;
                }
                Effect::Emit(Event::FileClosed { index }) => {
                    if let Some(entry) = entries.get(usize::try_from(index).unwrap_or(usize::MAX)) {
                        transferred += u64::try_from(entry.info.size).unwrap_or(0);
                        emit(&request.events, EngineEvent::FileDone { name: entry.info.name.clone() });
                        emit(&request.events, EngineEvent::Progress { transferred, total: total_bytes });
                    }
                }
                Effect::Emit(_) => {}
                Effect::Finish(outcome) => {
                    return match outcome {
                        Outcome::Completed => Ok(Summary { files: entries.len(), bytes: transferred }),
                        other => Err(other.into()),
                    };
                }
            }
        }

        effects = tokio::select! {
            frame = link.framed.read_frame() => match frame? {
                Some(frame) if croc_protocol::is_keepalive(&frame) => Vec::new(),
                Some(frame) => session.handle(InEvent::Frame(frame), &mut randomness)?,
                None => return Err(EngineError::Io("the relay closed the control connection".to_owned())),
            },
            Some(event) = data_rx.recv() => session.handle(event, &mut randomness)?,
            () = cancel.cancelled() => session.handle(InEvent::Cancel, &mut randomness)?,
        };
    }
}

/// 开数据流并把一个文件传出去；全部流结束（或出错）后给控制循环发通知。
#[allow(clippy::too_many_arguments, reason = "这条路径上的参数都是独立事实，打包成结构体只是换个地方写")]
async fn spawn_sender(
    request: &SendRequest,
    ports: &[u16],
    path: PathBuf,
    ranges: &[i64],
    file_size: u64,
    compress: bool,
    streams: u8,
    key: Option<[u8; croc_protocol::ENCRYPTION_KEY_LEN]>,
    notify: mpsc::Sender<InEvent>,
    cancel: CancellationToken,
) -> Result<(), EngineError> {
    let key = key.ok_or_else(|| EngineError::Config("the data key is not ready yet".to_owned()))?;
    let (host, _) = split_host(&request.relay)?;
    let count = usize::from(streams).min(ports.len());

    let mut conns = Vec::with_capacity(count);
    for (index, port) in ports.iter().take(count).enumerate() {
        let index = u8::try_from(index).unwrap_or(u8::MAX);
        conns.push(
            relay::connect_data(&host, *port, &request.relay_password, &request.components.room_name, index).await?,
        );
    }

    let queue =
        Arc::new(Mutex::new(ChunkQueue::new(&croc_core::chunk::ChunkRanges::from_wire(ranges.to_vec()), file_size)));
    let path_for_task = path.clone();
    tokio::spawn(async move {
        match send_file(conns, path_for_task, queue, key, compress, cancel).await {
            // 发完就**什么都不用报**：发送方的收尾由对端的 `close-sender` 驱动
            // （数据面发完 ≠ 对端收完，最后一块可能还在中继的缓冲里）。
            // 报一个事件反而会让状态机在 `Transferring` 上收到不认识的东西。
            Ok(()) => {}
            Err(error) => {
                tracing::warn!(%error, "data plane failed");
                let _ = notify.send(InEvent::Cancel).await;
            }
        }
    });
    Ok(())
}

/// 从 `host:port` 里取 host。
fn split_host(value: &str) -> Result<(String, u16), EngineError> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(|| EngineError::BadRelayAddress(value.to_owned()))?;
        let port = tail.trim_start_matches(':').parse().map_err(|_| EngineError::BadRelayAddress(value.to_owned()))?;
        return Ok((host.to_owned(), port));
    }
    let (host, port) = value.rsplit_once(':').ok_or_else(|| EngineError::BadRelayAddress(value.to_owned()))?;
    Ok((host.to_owned(), port.parse().map_err(|_| EngineError::BadRelayAddress(value.to_owned()))?))
}

/// 给接收方用：一个文件是否走数据面（空文件 / 符号链接不走）。
#[must_use]
pub fn needs_data_plane(info: &croc_protocol::FileInfo) -> bool {
    !croc_core::policy::is_empty_or_symlink(info)
}
