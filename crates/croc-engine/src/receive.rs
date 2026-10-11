//! 接收方驱动：收清单、逐个文件请求并落盘。

use std::path::PathBuf;
use std::sync::Arc;

use croc_core::codephrase::Components;
use croc_core::policy;
use croc_core::policy::Preferences;
use croc_core::{Effect, Event, InEvent, Outcome, Role, Session, SessionConfig};
use croc_net::OsRandomness;
use croc_protocol::{FileInfo, SenderInfo, TailcatDeclaration, declare};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::data::{DataPlane, receive_file, stream_count};
use crate::dns;
use crate::error::EngineError;
use crate::files::{self, Plan, Sink};
use crate::local;
use crate::relay;
use crate::{EngineEvent, EventSink, emit};

/// 收到清单后要不要收：`true` = 开始接收。
///
/// 做成回调是因为"要不要收"的答案只能由用户给，而清单要等握手完成才拿得到——
/// 在命令行参数解析阶段问是问不出来的。
pub type ConfirmSink = Arc<dyn Fn(&SenderInfo) -> bool + Send + Sync>;

/// [`AsyncConfirmSink`] 的返回值。
pub type ConfirmFuture = std::pin::Pin<Box<dyn std::future::Future<Output = bool> + Send>>;

/// 需要等用户点按钮的调用方（GUI）用的确认回调。
///
/// CLI 那边的终端交互是同步的，配 [`ConfirmSink`] 就够；GUI 要等界面上的按钮，而在一个
/// tokio worker 上同步阻塞会让整个 runtime 停摆，所以给一条 async 路径。
pub type AsyncConfirmSink = Arc<dyn Fn(&SenderInfo) -> ConfirmFuture + Send + Sync>;

/// 收完要不要把内容打到标准输出。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum StdoutMode {
    /// 看情况：普通文件不打，文本传输打（Go 的 `--text` 会顺手把 `Stdout` 打开）。
    #[default]
    Auto,
    /// `--stdout`：一律打。
    Always,
    /// 一律不打，文件留着：GUI 的标准输出没人看，打了等于把内容丢掉。
    Never,
}

/// 一次接收所需的全部参数。
pub struct ReceiveRequest {
    /// 中继地址（`host:port`）。
    pub relay: String,
    /// 中继密码。
    pub relay_password: String,
    /// 从口令码解出的房间名与 PAKE 口令。
    pub components: Components,
    /// 本端选择的曲线。由接收方决定，发送方照着用。
    pub curve: croc_pake::CurveKind,
    /// 落盘目录。
    pub out_dir: PathBuf,
    /// `--overwrite` / `--rename`。
    pub preferences: Preferences,
    /// `--no-compress`：对端压了也不解（等价 Go 的同名开关）。
    pub no_compress: bool,
    /// `--local`：只走局域网——先组播找出发送方，用它的地址当中继，不连公网中继。
    pub local: bool,
    /// 出站代理（`--socks5` / `--connect`）。
    pub proxy: crate::proxy::Proxy,
    /// `--multicast`：`--local` 发现用的组播组地址。
    pub multicast: std::net::Ipv4Addr,
    /// `--relay6`：IPv6 中继地址（与 `--relay` 一起做 Happy Eyeballs）。
    pub relay6: String,
    /// `--ip`：已知的对方地址。给了就直连它，跳过局域网发现。
    pub ip: String,
    /// 收完要不要把内容打到标准输出（打完就删，见 [`print_received`]）。
    ///
    /// 三态而不是布尔：文本传输在 CLI 上要自动开（`--text` 的那一半协议），但 GUI 的
    /// 标准输出没人看，它必须能明确说"别打、文件留着"。
    pub stdout: StdoutMode,
    /// `--internal-dns`：用自带的 DNS stub resolver，而不是操作系统的解析器。
    pub internal_dns: bool,
    /// `--no-multi`：禁用多端口复用，只用一条数据流。
    ///
    /// 必须与发送方一致：流数不在握手里协商，两端各自按中继 banner 推导，只有一端
    /// 设了的话两边的数据面对不上。
    pub no_multi: bool,
    /// 本端机器指纹。
    pub machine_id: String,
    /// 收到清单后问一句；`None` 表示直接接受。
    pub confirm: Option<ConfirmSink>,
    /// 收到清单后异步地问一句；`None` 表示直接接受。
    ///
    /// 与 [`confirm`](Self::confirm) 二选一：两者都给了时以 [`confirm`](Self::confirm) 为准。
    pub confirm_async: Option<AsyncConfirmSink>,
    /// 事件出口。
    pub events: EventSink,
}

impl core::fmt::Debug for ReceiveRequest {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ReceiveRequest")
            .field("relay", &self.relay)
            .field("out_dir", &self.out_dir)
            .field("preferences", &self.preferences)
            .finish_non_exhaustive()
    }
}

/// 接收结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// 新收下来的文件数。
    pub received: usize,
    /// 因为内容一致而跳过的文件数。
    pub skipped: usize,
    /// 用户拒绝的文件数。
    pub declined: usize,
    /// 落盘的字节数。
    pub bytes: u64,
}

/// 把一个收完的文件打到标准输出，然后删掉它（`--stdout`，文本传输会自动开它）。
///
/// 打完就删是 Go 的行为（`croc.go:2339`：`Stdout && !IsSender` 时 `root.Remove`）：
/// 内容已经出去了，再留一份在磁盘上，用户看到的就是"收到了一个 `croc-stdin-*` 文件"
/// 而不是那段文本。Go 只删最后处理的那一个（它的代码只认 `FilesToTransferCurrentNum`），
/// 这里每个打出去的都删——多文件时只删最后一个会留下一半。
///
/// 时机照 Go：每个文件收完就打（`receiveData` 里 `finished` 那一支），不是等整次
/// 传输结束再打——`croc-rs receive --stdout | jq` 这类用法要的是边收边出。
///
/// # Errors
///
/// 读不回来时返回错误（文件被跳过时会 open 失败，那是正常的，直接跳过）。
fn print_received(info: &FileInfo, out_dir: &std::path::Path) -> Result<(), EngineError> {
    use std::io::Write as _;

    let path = files::destination_of(info, out_dir)?;
    let Ok(mut file) = std::fs::File::open(&path) else {
        return Ok(());
    };
    let mut stdout = std::io::stdout().lock();
    std::io::copy(&mut file, &mut stdout).map_err(|error| EngineError::file(&path, error))?;
    let _ = stdout.flush();
    drop(file);
    // Windows 上文件还开着就删不掉，所以上面先 drop。删不掉不值得让整次传输失败。
    let _ = std::fs::remove_file(&path);
    Ok(())
}

/// # Errors
///
/// 中继连不上、口令不对、协议违规、写盘失败，或者被取消。
pub async fn receive(request: ReceiveRequest, cancel: CancellationToken) -> Result<Summary, EngineError> {
    // `--local`：组播找发送方，拿它的地址当中继。找不到就是"局域网里没人在发"，
    // 直接失败——不能悄悄退回公网中继，那正是用户用 `--local` 明确拒绝的事。
    let relay_addresses = if !request.ip.trim().is_empty() {
        // `--ip`：既然知道对方在哪，就直连它——跳过局域网发现。
        vec![request.ip.clone()]
    } else if request.local {
        emit(&request.events, EngineEvent::Status("looking for the sender on the local network…".to_owned()));
        vec![local::find_peer(local::DISCOVERY_TIMEOUT, request.multicast).await?]
    } else {
        relay::candidate_addresses("", &request.relay, &request.relay6)
    };
    let resolve_with = if request.internal_dns { dns::Mode::Internal } else { dns::Mode::System };
    let (mut link, relay_address) = relay::connect_any(
        &relay_addresses,
        &request.relay_password,
        &request.components.room_name,
        &request.proxy,
        resolve_with,
    )
    .await?;
    let ports = link.data_ports();
    if ports.is_empty() {
        return Err(EngineError::Relay("the relay banner carried no data ports".to_owned()));
    }
    emit(&request.events, EngineEvent::Status("connecting…".to_owned()));

    let ports_to_use = if request.no_multi { 1 } else { ports.len() };
    let streams = stream_count(ports_to_use);
    let session_config = SessionConfig {
        role: Role::Receiver,
        room: request.components.room_name.as_str(),
        passphrase: request.components.pake_passphrase.as_bytes(),
        curve: request.curve,
        features: declare(TailcatDeclaration::Unsupported),
        relay_ports: streams,
        machine_id: request.machine_id.clone(),
        external_ip: link.external_ip.clone(),
    };
    let mut session = Session::new_receiver(&session_config)?;
    let mut randomness = OsRandomness;

    let (data_tx, mut data_rx) = mpsc::channel::<InEvent>(4);
    // 数据连接整个传输只建一次：数据端口的房间名是 `{room}-{j}`，一个房间只容得下
    // 两端各一条连接；每条文件重建一次就会撞上自己上一条文件留下的连接，中继回 `room full`。
    let data_plane = Arc::new(DataPlane::new(
        relay_address,
        request.relay_password.clone(),
        request.components.room_name.clone(),
        ports.clone(),
        streams,
        request.proxy.clone(),
        resolve_with,
    ));

    // 文件循环的状态。
    let mut manifest: Option<Box<SenderInfo>> = None;
    let mut pending: Vec<u64> = Vec::new();
    let mut awaiting_close = false;
    let mut summary = Summary { received: 0, skipped: 0, declined: 0, bytes: 0 };
    let mut transferred: u64 = 0;

    let mut effects = session.handle(InEvent::PeerConnected, &mut randomness)?;
    loop {
        // 先处理效果，再决定"下一步该喂什么事件"——顺序反了会在还没拿到清单时就去挑文件。
        for effect in core::mem::take(&mut effects) {
            match effect {
                Effect::Send(frame) => link.framed.write_frame(&frame).await?,
                Effect::SendFile { .. } => {
                    return Err(EngineError::Config("the receiver was asked to send a file".to_owned()));
                }
                Effect::Emit(event) => match event {
                    Event::Manifest(info) => {
                        // 对端把地址内联在清单里（`inline-peer-metadata-v1`）时，
                        // 这是唯一能拿到它的地方——`externalip` 那条路不会走。
                        if !info.external_ip.is_empty() {
                            emit(&request.events, EngineEvent::PeerAddress(info.external_ip.clone()));
                        }
                        manifest = Some(info.clone());
                        emit(&request.events, EngineEvent::Status("received the file list".to_owned()));
                    }
                    Event::FileClosed { index } => {
                        awaiting_close = false;
                        if let Some(entry) = manifest
                            .as_ref()
                            .and_then(|info| info.files_to_transfer.get(usize::try_from(index).unwrap_or(usize::MAX)))
                        {
                            emit(&request.events, EngineEvent::FileDone { name: entry.name.clone() });
                            emit(&request.events, EngineEvent::Progress { transferred, total: total_of(&manifest) });
                            if wants_stdout(&request, manifest.as_deref()) {
                                print_received(entry, &request.out_dir)?;
                            }
                        }
                    }
                    _ => {}
                },
                Effect::Finish(outcome) => {
                    return match outcome {
                        Outcome::Completed => Ok(summary),
                        other => Err(other.into()),
                    };
                }
            }
        }

        // 拿到清单 ⇒ 问用户 ⇒ 规划每个文件。
        if session.stage() == croc_core::Stage::AwaitingDecision {
            let info = manifest.clone().ok_or_else(|| EngineError::Config("no manifest to decide on".to_owned()))?;
            let accept = match &request.confirm {
                Some(confirm) => confirm(&info),
                None => match &request.confirm_async {
                    Some(ask) => ask(&info).await,
                    None => true,
                },
            };
            if accept {
                let info = rename_text_payload(info);
                let (plan, planned) = plan_files(&request, &info)?;
                summary.skipped = planned.skipped;
                summary.declined = planned.declined;
                transferred = planned.done_bytes;
                pending = plan;
                // 改过名的清单要放回去：文件循环与 `FileClosed` 都从它取名。
                manifest = Some(info);
            }
            effects = session.handle(InEvent::Decision(answer(accept)), &mut randomness)?;
            continue;
        }

        // 接收方在文件循环里：挑下一个，或者收尾。
        if session.stage() == croc_core::Stage::Transferring && !awaiting_close {
            let info = manifest.clone().ok_or_else(|| EngineError::Config("no manifest".to_owned()))?;
            let Some(index) = pending.first().copied() else {
                effects = session.handle(InEvent::AllFilesDone, &mut randomness)?;
                continue;
            };
            let entry = info
                .files_to_transfer
                .get(usize::try_from(index).unwrap_or(usize::MAX))
                .ok_or_else(|| EngineError::Config(format!("file #{index} vanished")))?
                .clone();
            let plan = files::plan(
                &entry,
                &request.out_dir,
                request.preferences,
                info.sending_text,
                info.hash_algorithm.as_str(),
                true,
            )?;

            // 不需要数据面的三种直接结算，继续挑下一个。
            let (sink, ranges) = match plan {
                Plan::AlreadyCurrent => {
                    pending.remove(0);
                    summary.skipped += 1;
                    continue;
                }
                Plan::Declined => {
                    pending.remove(0);
                    summary.declined += 1;
                    continue;
                }
                Plan::CreateDirectly => {
                    let _ = files::open_sink(&entry, &request.out_dir, None)?;
                    pending.remove(0);
                    summary.skipped += 1;
                    continue;
                }
                Plan::Renamed(name) => {
                    (files::open_sink(&entry, &request.out_dir, Some(&name))?, croc_core::chunk::ChunkRanges::whole())
                }
                Plan::Receive(ranges) => (files::open_sink(&entry, &request.out_dir, None)?, ranges),
            };

            emit(
                &request.events,
                EngineEvent::Status(format!(
                    "准备接收 {}（{} 块 / {} 字节, is_compressed={}），获取 {} 条数据连接…",
                    entry.name,
                    ranges.count(u64::try_from(entry.size).unwrap_or(0)),
                    ranges.bytes(u64::try_from(entry.size).unwrap_or(0)),
                    entry.is_compressed,
                    streams
                )),
            );
            // 先拿到数据连接再发 recipientready：对端一收到请求就会立刻开始发数据，
            // 数据端口上还没连上就会白丢几块（中继不会为缺席的端口缓冲）。
            // 连接由 `DataPlane` 管，第一个文件才真正建，之后一直复用。
            let conns = data_plane.acquire().await?;
            emit(&request.events, EngineEvent::Status(format!("数据连接已就绪（{} 条）", conns.len())));
            effects =
                session.handle(InEvent::RequestFile { index, ranges: ranges.as_wire().to_vec() }, &mut randomness)?;
            // 照抄 Go 的 `currentFileUsesCompression`：对端没声明逐文件压缩能力就
            // 一律当压缩的来解（老对端只认全局开关）。
            let compress =
                if request.no_compress { false } else { !session.peer_per_file_compression() || entry.is_compressed };
            spawn_receive(
                ReceiveJob {
                    conns,
                    plane: Arc::clone(&data_plane),
                    sink,
                    entry: entry.clone(),
                    ranges,
                    key: session.data_key(),
                    compress,
                    events: request.events.clone(),
                },
                &data_tx,
                &cancel,
            )?;
            // 划掉待办：不划掉的话对端回 `close-recipient` 之后会再挑同一个文件，
            // 那时它已经落盘且哈希一致，会被当成"跳过"——一次成功的传输报成"收了 1 跳了 1"。
            pending.remove(0);
            awaiting_close = true;
            summary.received += 1;
            summary.bytes += u64::try_from(entry.size).unwrap_or(0);
            continue;
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

/// 规划结果：待传的下标，以及"跳过 / 拒绝 / 已经不用传的字节数"。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Planned {
    /// 已经一致、不需要传的字节数。
    done_bytes: u64,
    /// 因为内容一致而跳过的文件数。
    skipped: usize,
    /// 用户拒绝的文件数。
    declined: usize,
}

/// 按清单算出"还要传哪些文件"，顺带统计跳过的。
fn plan_files(request: &ReceiveRequest, info: &SenderInfo) -> Result<(Vec<u64>, Planned), EngineError> {
    let mut pending = Vec::new();
    let mut planned = Planned::default();

    // 空目录不进文件循环（没有数据要传），但必须在本地建出来——否则一个只含空目录的
    // 目录树传过去会变成"什么都没有"。
    for folder in &info.empty_folders_to_transfer {
        let destination = files::folder_destination_of(folder, &request.out_dir)?;
        std::fs::create_dir_all(&destination).map_err(|error| EngineError::file(&destination, error))?;
    }

    for (index, entry) in info.files_to_transfer.iter().enumerate() {
        let plan = files::plan(
            entry,
            &request.out_dir,
            request.preferences,
            info.sending_text,
            info.hash_algorithm.as_str(),
            true,
        )?;
        match plan {
            Plan::AlreadyCurrent => {
                planned.skipped += 1;
                planned.done_bytes += u64::try_from(entry.size).unwrap_or(0);
            }
            Plan::Declined => planned.declined += 1,
            Plan::CreateDirectly => {
                // 空文件 / 符号链接：本地直接造出来，不走数据面。
                let _ = files::open_sink(entry, &request.out_dir, None)?;
                planned.skipped += 1;
            }
            Plan::Renamed(_) | Plan::Receive(_) => pending.push(u64::try_from(index).unwrap_or(u64::MAX)),
        }
    }
    Ok((pending, planned))
}

/// 把文本载荷的落盘名从发送方的临时名换成接收方自己的名字。
///
/// `croc-stdin-*` 是发送方的临时文件名（见 [`croc_engine::text`]），对用户毫无意义——
/// 不改的话收下来的就是一个叫 `croc-stdin-1759…` 的文件。Go 在拿到清单时用
/// `os.CreateTemp` 换掉它（`croc.go:2518`），这里换成 [`policy::text_destination_name`]。
///
/// 判据必须和 [`policy::is_text_artifact`] 一致（"这次是文本传输"且名字带前缀）：
/// 换名的前提是协议保证接收方自己生成这个名字，少一半判据就会把用户一个真叫
/// `croc-stdin-*` 的文件改掉。
fn rename_text_payload(mut info: Box<croc_protocol::SenderInfo>) -> Box<croc_protocol::SenderInfo> {
    if !info.sending_text {
        return info;
    }
    let replacement = policy::text_destination_name();
    for file in &mut info.files_to_transfer {
        if policy::is_text_artifact(true, &file.name) {
            file.name = replacement.clone();
        }
    }
    info
}

/// 这次接收要不要把内容打到标准输出。
fn wants_stdout(request: &ReceiveRequest, manifest: Option<&croc_protocol::SenderInfo>) -> bool {
    match request.stdout {
        StdoutMode::Always => true,
        StdoutMode::Never => false,
        StdoutMode::Auto => manifest.is_some_and(|info| info.sending_text),
    }
}

/// 一个文件的接收作业。
///
/// 打包成结构体而不是九个参数：这条路径上的东西（连接、连接池、落盘目标、清单条目、
/// 区间、密钥、压缩判定）本来就是"一次接收"的组成部分，散成参数只会让调用点变成一长串。
struct ReceiveJob {
    conns: Vec<croc_net::Framed<tokio::net::TcpStream>>,
    /// 连接用完还到这里——不关，下一个文件接着用。
    plane: Arc<DataPlane>,
    sink: Sink,
    entry: FileInfo,
    ranges: croc_core::chunk::ChunkRanges,
    key: Option<[u8; croc_protocol::ENCRYPTION_KEY_LEN]>,
    /// 已经算好的压缩判定（调用点照抄 Go 的 `currentFileUsesCompression`）。
    compress: bool,
    /// 进度与"开收了"要往哪儿报。
    events: EventSink,
}

/// 开收数据的流；收完（或出错）后给控制循环发通知。
fn spawn_receive(
    job: ReceiveJob,
    notify: &mpsc::Sender<InEvent>,
    cancel: &CancellationToken,
) -> Result<(), EngineError> {
    let ReceiveJob { conns, plane, sink, entry, ranges, key, compress, events } = job;
    let key = key.ok_or_else(|| EngineError::Config("the data key is not ready yet".to_owned()))?;
    let size = u64::try_from(entry.size).unwrap_or(0);
    let expected_chunks = ranges.count(size);
    let expected_bytes = ranges.bytes(size);

    emit(&events, EngineEvent::FileStarted { name: entry.name.clone(), size });
    let sink = Arc::new(sink);
    let notify = notify.clone();
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let result = receive_file(
            conns,
            Arc::clone(&sink),
            expected_chunks,
            expected_bytes,
            key,
            compress,
            cancel,
            Some(events),
        )
        .await;
        let event = match result {
            // 收全判定放在全部流收工之后：这是唯一没有竞态的时刻。
            Ok((conns, progress)) if progress.is_complete(expected_chunks, expected_bytes) => {
                let _ = sink.finalize(&entry);
                plane.release(conns);
                InEvent::DataFinished
            }
            Ok((conns, _)) => {
                tracing::warn!(%entry.name, "the data plane stopped before the file was complete");
                plane.release(conns);
                InEvent::Cancel
            }
            // 出错时连接已随 `receive_file` 的 `Err` 一起丢掉（传输要中止了，没什么可复用）。
            Err(error) => {
                tracing::warn!(%error, "data plane failed");
                InEvent::Cancel
            }
        };
        let _ = notify.send(event).await;
    });
    Ok(())
}

fn answer(accepted: bool) -> croc_core::policy::Answer {
    if accepted { croc_core::policy::Answer::Accept } else { croc_core::policy::Answer::Decline }
}

fn total_of(manifest: &Option<Box<SenderInfo>>) -> u64 {
    manifest
        .as_ref()
        .map_or(0, |info| info.files_to_transfer.iter().filter_map(|entry| u64::try_from(entry.size).ok()).sum())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(sending_text: bool, names: &[&str]) -> Box<SenderInfo> {
        Box::new(SenderInfo {
            sending_text,
            files_to_transfer: names
                .iter()
                .map(|name| FileInfo { name: (*name).to_owned(), ..FileInfo::default() })
                .collect(),
            ..SenderInfo::default()
        })
    }

    #[test]
    fn a_text_payload_loses_the_senders_temp_name() {
        let renamed = rename_text_payload(manifest(true, &["croc-stdin-1759"]));
        let name = &renamed.files_to_transfer[0].name;
        assert!(!name.starts_with("croc-stdin-"), "{name} 还是发送方的临时名");
        assert!(name.ends_with(".txt"), "{name} 没有后缀，用户双击打不开");
    }

    #[test]
    fn a_plain_file_keeps_its_name() {
        let renamed = rename_text_payload(manifest(false, &["report.pdf"]));
        assert_eq!(renamed.files_to_transfer[0].name, "report.pdf");
    }

    #[test]
    fn a_user_file_that_happens_to_be_named_croc_stdin_is_left_alone() {
        // 不是文本传输就不该动：用户真有一个叫 `croc-stdin-*` 的文件时，改掉它就错了。
        let renamed = rename_text_payload(manifest(false, &["croc-stdin-report"]));
        assert_eq!(renamed.files_to_transfer[0].name, "croc-stdin-report");
    }
}
