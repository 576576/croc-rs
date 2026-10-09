//! 发送方驱动：把 `croc-core::Session` 的效果落到 socket 与磁盘上。

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use croc_core::codephrase::Components;
use croc_core::{Effect, Event, InEvent, Outcome, Role, Session, SessionConfig};
use croc_net::OsRandomness;
use croc_protocol::{TailcatDeclaration, declare};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::data::{ChunkQueue, DataPlane, send_file, stream_count};
use crate::dns;
use crate::error::EngineError;
use crate::files;
use crate::local;
use crate::relay;
use crate::throttle::Throttle;
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
    /// `--exclude`：相对路径里**包含**任一子串就跳过（大小写不敏感）。
    ///
    /// 与 [`Self::exclude_file`] 不是一回事：这个是子串，`--exclude node_modules` 会连
    /// `my_node_modules_backup/` 一起排掉。
    pub exclude: Vec<String>,
    /// `--exclude-file`：相对路径**精确等于**任一项就跳过。
    pub exclude_file: Vec<String>,
    /// `--git`：尊重被传目录下的 `.gitignore`。
    pub git: bool,
    /// `--zip`：目录先打成 zip 再发（临时 zip 随本次传输结束而删）。
    pub zip: bool,
    /// 出站代理（`--socks5` / `--connect`）。
    pub proxy: crate::proxy::Proxy,
    /// `--multicast`：`--local` 发现用的组播组地址。
    pub multicast: std::net::Ipv4Addr,
    /// `--relay6`：IPv6 中继地址（与 `--relay` 一起做 Happy Eyeballs）。
    pub relay6: String,
    /// `--ip`：已知的对方地址。给了就只用它。
    pub ip: String,
    /// `--ask`：让接收方也确认一次（随清单发过去）。
    pub ask: bool,
    /// `--text`：内容是命令行给的一段文本，而不是 stdin 或文件。
    ///
    /// 只影响"要发什么"那一行里显示的名字（`text` 而不是 `stdin`）——两者都落到
    /// `croc-stdin-` 开头的临时文件上，光看名字分不出来。
    pub sending_text: bool,
    /// `--internal-dns`：用自带的 DNS stub resolver，而不是操作系统的解析器。
    pub internal_dns: bool,
    /// `--throttleUpload`：上行限速（字节/秒）。`None` = 不限速。
    ///
    /// 只影响发送方向——Go 也只是在 `IsSender` 时才建限速器（`croc.go:451`）。
    pub throttle_upload: Option<u64>,
    /// 哈希算法：`xxhash` / `md5` / `imohash` / `highway`。
    pub hash_algorithm: String,
    /// `--local`：只走局域网——本端起一个内建中继，**不连公网中继**。
    pub local: bool,
    /// `--port`：`--local` 下内建中继的**基准**端口（被占就顺延）。
    ///
    /// 只在 `--local` 下有意义：连外部中继时端口由中继的 banner 说了算，Go 也是这样
    /// （`croc.go:1521` 之后 `--port` 给的数组会被覆盖掉）。
    pub relay_base_port: Option<u16>,
    /// `--transfers`：`--local` 下内建中继开几个数据端口，也就是数据面并行几条流。
    ///
    /// 同样只在 `--local` 下有意义，理由同上。
    pub transfers: usize,
    /// `--no-multi`：禁用多端口复用，只用一条数据流。
    ///
    /// 两端要**各自**设：流数不在握手里协商，各自按中继 banner 推导，只有一端设了的话
    /// 两边的数据面对不上。Go 只把它挂在 `send` 子命令上（`src/cli/cli.go:101`）。
    pub no_multi: bool,
    /// 关掉压缩。
    pub no_compress: bool,
    /// 本端机器指纹。
    pub machine_id: String,
    /// 事件出口。
    pub events: EventSink,
}

/// `--zip` 现造的临时文件，跟随作用域清理。
struct ZipCleanup(Vec<PathBuf>);

impl Drop for ZipCleanup {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = fs::remove_file(path);
        }
    }
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
    // `--local`：本端自己当中继，连自己的回环地址，端口由组播喊出去给对端。
    // 中继得先起来才有端口可喊，所以这一步在连中继与算哈希之前。
    // 它一直活到 `send()` 返回（drop 即停），正好覆盖整次传输。
    // `--transfers` 决定内建中继开几个数据端口；`--no-multi` 只要一条流。
    // 走外部中继时这两个值都用不上（端口由中继 banner 给），所以只在 `--local` 下算。
    let local_streams = if request.no_multi { 1 } else { request.transfers.max(1) };
    let local_relay = if request.local {
        Some(
            local::announce(
                &request.relay_password,
                local_streams,
                request.relay_base_port,
                request.multicast,
                cancel.child_token(),
            )
            .await?,
        )
    } else {
        None
    };
    // `--local` 时中继是本端的回环；否则按 `--ip` / `--relay6` / `--relay` 挑。
    let relay_addresses = match &local_relay {
        Some(relay) => vec![relay.loopback_address()],
        None => relay::candidate_addresses(&request.ip, &request.relay, &request.relay6),
    };

    // 先**只清点**一遍（stat，不读内容），把"要发什么"报给 UI。Go 在进 `Client.Send()`
    // 之前就已经从 CLI 拿到了完整的文件信息，所以它能在算哈希的同时把口令码打出来；
    // 合在一起就得等哈希算完才有东西可显示。这一趟只 stat，代价可以忽略。
    let survey_paths = request.paths.clone();
    let options = files::CollectOptions {
        exclude: request.exclude.clone(),
        exclude_file: request.exclude_file.clone(),
        git: request.git,
        zip: request.zip,
    };
    let surveyed = tokio::task::spawn_blocking(move || files::survey(&survey_paths, &options))
        .await
        .map_err(|error| EngineError::Config(format!("the file survey task {error}")))?;
    let (mut entries, empty_folders) = surveyed?;
    let total_bytes: u64 = entries.iter().filter_map(|entry| u64::try_from(entry.info.size).ok()).sum();
    emit(
        &request.events,
        EngineEvent::Sending {
            name: single_name(&entries, request.sending_text),
            files: u64::try_from(entries.len()).unwrap_or(u64::MAX),
            folders: u64::try_from(files::folder_count(&entries, &empty_folders)).unwrap_or(0),
            bytes: total_bytes,
        },
    );

    // 算哈希与连中继**并行**（Go 把前者丢进后台 goroutine，`croc.go:1792`）。
    // 清单要带哈希，所以得读一遍全部文件；大目录在 Windows 上十几秒，串在连接之前
    // 就会让我们进房间进得晚——对端那段时间只能干等，它的局域网探测也会因等不到
    // 应答而超时（见 `Session::try_local_probe()`）。
    //
    // 坏路径依旧早暴露：任一边先出错就立刻返回，另一半被 drop，半开的中继连接随之关掉。
    let algorithm = request.hash_algorithm.clone();
    // `hash_all` 是同步的阻塞式文件 IO，直接放在 async 上下文里会把 worker 线程占住，
    // 连中继的 future 就没机会推进——并行也就名存实亡。
    let hashing = tokio::task::spawn_blocking(move || {
        files::hash_all(&mut entries, &algorithm)?;
        Ok::<_, EngineError>(entries)
    });
    let resolve_with = resolve_mode(request.internal_dns);
    let connecting = relay::connect_any(
        &relay_addresses,
        &request.relay_password,
        &request.components.room_name,
        &request.proxy,
        resolve_with,
    );
    let (hashed, (mut link, relay_address)) = tokio::try_join!(
        async { hashing.await.map_err(|error| EngineError::Config(format!("the hashing task {error}")))? },
        connecting,
    )?;
    let mut entries = hashed;
    // `--zip` 现造的临时 zip 跟着本次 `send()` 的作用域清理。用 Drop 而不是在每个返回点
    // 手写删除：这函数的返回点很多，漏一个就在磁盘上留一个几百 MB 的垃圾。
    let _zip_cleanup =
        ZipCleanup(entries.iter().filter(|entry| entry.temporary).map(|entry| entry.path.clone()).collect());
    let compress = !request.no_compress;
    for entry in &mut entries {
        // 逐文件压缩开关：接收方靠这个字段决定要不要解压，所以必须随清单一起过去。
        entry.info.is_compressed = compress && entry.info.size > 0;
    }
    let info = files::sender_info(&entries, empty_folders, &request.machine_id, &request.hash_algorithm, request.ask);

    let ports = link.data_ports();
    if ports.is_empty() {
        return Err(EngineError::Relay("the relay banner carried no data ports".to_owned()));
    }

    // `--no-multi` 把 banner 里的多个数据端口砍成一个，与 Go 的 `croc.go:1522` 一致。
    let ports_to_use = if request.no_multi { 1 } else { ports.len() };
    let streams = stream_count(ports_to_use);
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
    // **数据连接整个传输只建一次**：数据端口的房间名是 `{room}-{j}`，一个房间只容得下
    // 两端各一条连接，每条文件重建一次就会撞上自己上一条文件留下的连接。
    // 连接留在池子里直到 `send()` 返回，正好等价于 Go 的 `c.dataConns`。
    let data_plane = Arc::new(DataPlane::new(
        relay_address,
        request.relay_password.clone(),
        request.components.room_name.clone(),
        ports.clone(),
        streams,
        request.proxy.clone(),
        resolve_with,
    ));
    // `--throttleUpload`：整个上传共用一个桶，数据面的每条流都从它领令牌。
    let throttle = request.throttle_upload.map(|rate| Arc::new(Throttle::new(rate)));
    let mut transferred: u64 = 0;

    let mut effects = session.handle(InEvent::PeerConnected, &mut randomness)?;
    loop {
        for effect in core::mem::take(&mut effects) {
            match effect {
                Effect::Send(frame) => link.framed.write_frame(&frame).await?,
                Effect::SendFile { index, ranges, streams, peer_per_file_compression } => {
                    // `streams` 与 `data_plane.width()` 是同一个数：会话按 `min(relay_ports, 8)`
                    // 算，数据面按 `min(端口数, 8)` 算，而 `relay_ports` 就是端口数。留个断言，
                    // 免得将来两边各改各的却没人发现。
                    debug_assert_eq!(usize::from(streams), data_plane.width(), "会话与数据面的并行流数应当一致");
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
                    // 照抄 Go 的 `currentFileUsesCompression`：
                    //   1. `--no-compress` 关掉就是关掉；
                    //   2. 对端没声明 `per-file-compression-v1` ⇒ **一律压缩**
                    //      （老对端只认全局开关）；
                    //   3. 声明了才看每个文件自己的标志。
                    let compress = if request.no_compress {
                        false
                    } else if !peer_per_file_compression {
                        true
                    } else {
                        entry.info.is_compressed
                    };
                    let conns = data_plane.acquire().await?;
                    spawn_sender(
                        entry.path.clone(),
                        &ranges,
                        u64::try_from(entry.info.size).unwrap_or(0),
                        compress,
                        session.data_key(),
                        conns,
                        Arc::clone(&data_plane),
                        data_tx.clone(),
                        throttle.clone(),
                        cancel.clone(),
                    )?;
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
                    // 到这一步对端已经确认收完（`finished` 已交换）。`data_plane` 随本函数
                    // 返回而析构，数据连接那时才关——这正是 Go 把 `c.dataConns` 留到最后的理由。
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

/// 把一个文件交给数据面去传；传完（或出错）后给控制循环发通知。
///
/// 连接由 [`DataPlane`] 管：这里只是"借"出来，传完还回去。**不在这里关连接**——
/// 见 [`send_file`] 关于 RST 的说明。
#[allow(clippy::too_many_arguments, reason = "这条路径上的参数都是独立事实，打包成结构体只是换个地方写")]
fn spawn_sender(
    path: PathBuf,
    ranges: &[i64],
    file_size: u64,
    compress: bool,
    key: Option<[u8; croc_protocol::ENCRYPTION_KEY_LEN]>,
    conns: Vec<croc_net::Framed<tokio::net::TcpStream>>,
    plane: Arc<DataPlane>,
    notify: mpsc::Sender<InEvent>,
    throttle: Option<Arc<Throttle>>,
    cancel: CancellationToken,
) -> Result<(), EngineError> {
    let key = key.ok_or_else(|| EngineError::Config("the data key is not ready yet".to_owned()))?;
    let queue =
        Arc::new(Mutex::new(ChunkQueue::new(&croc_core::chunk::ChunkRanges::from_wire(ranges.to_vec()), file_size)));

    tokio::spawn(async move {
        match send_file(conns, path, queue, key, compress, throttle, cancel).await {
            // 发完就**什么都不用报**：发送方的收尾由对端的 `close-sender` 驱动
            // （数据面发完 ≠ 对端收完，最后一块可能还在中继的缓冲里）。
            // 报一个事件反而会让状态机在 `Transferring` 上收到不认识的东西。
            Ok(conns) => plane.release(conns),
            Err(error) => {
                tracing::warn!(%error, "data plane failed");
                let _ = notify.send(InEvent::Cancel).await;
            }
        }
    });
    Ok(())
}

/// 只发一个文件时的显示名；多于一个文件时返回 `None`（UI 改用文件数）。
///
/// Go 的规则（`croc.go:1340`）：名字以 `croc-stdin-` 开头说明内容来自 stdin 或
/// `--text`，这两种都不显示那个临时文件名。
fn single_name(entries: &[files::SourceEntry], sending_text: bool) -> Option<String> {
    if entries.len() != 1 {
        return None;
    }
    let name = entries.first()?.info.name.as_str();
    Some(if name.starts_with("croc-stdin-") {
        if sending_text { "text".to_owned() } else { "stdin".to_owned() }
    } else {
        name.to_owned()
    })
}

/// `--internal-dns` → 解析模式。
fn resolve_mode(internal: bool) -> dns::Mode {
    if internal { dns::Mode::Internal } else { dns::Mode::System }
}

/// 给接收方用：一个文件是否走数据面（空文件 / 符号链接不走）。
#[must_use]
pub fn needs_data_plane(info: &croc_protocol::FileInfo) -> bool {
    !croc_core::policy::is_empty_or_symlink(info)
}
