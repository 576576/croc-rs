//! 对端会话状态机：从"中继已配对"到"传输完成"的**纯状态转移**。
//!
//! 对应 Go `src/croc/croc.go` 里的 `Send` / `Receive` / `processMessage` / `updateState`
//! 那一大坨。Go 版把状态藏在 `for { data, err := conn.Receive(); … }` 的读循环里，一次
//! 迭代里混着解密、业务判断、进度上报与文件 IO —— 于是"为什么对端说密码错误"这种问题
//! 只能靠加日志猜。
//!
//! 这里把同一套流程写成 [`Session::handle`]：输入一个**已经发生的事实**，输出一串
//! **希望外部执行的效果**。不碰 socket、不碰文件、不看时钟，随机数由调用方注入。
//! 于是整条握手与清单交换可以在单元测试里用事件序列确定性重放（见本文件末尾的
//! `a_full_relay_session_runs_to_completion`）。
//!
//! # 覆盖范围
//!
//! 覆盖 **relay 传输路径**的完整消息序列：
//!
//! ```text
//! 接收方(A)                                    发送方(B)
//! ─────────────────────────────────────────────────────────────────────────
//! ──pake{ b=A公开值, b2=曲线名, f=能力 }──▶
//!                              B=responder(pass, 曲线, 房间)
//!                              B.Update(A公开值)；salt ←$ 32B
//! ◀──pake{ b=B公开值, b2=salt, f=能力 }──
//! A.Update(B公开值)；双方各自 Derive ⇒ encryptionKey / confirmA / confirmB
//! ──pake-confirm{ b=confirmA }──▶
//!                              ◀──pake-confirm{ b=confirmB }──
//! ── 此后全部加密 ──
//! ◀──fileinfo-start / fileinfo-batch* / fileinfo-end──
//! ──recipientready{ b=RemoteFileRequest JSON }──▶
//! ── 数据面（中继数据端口）双向搬运 ──
//! ◀──close-sender──
//! ──close-recipient──▶
//! ◀──finished──
//! ──finished──▶
//! ```
//!
//! # 明确不在本状态机里
//!
//! 这些是 `croc-engine` / 上层的事，或者属于后续增量（都在计划文档里有条目）：
//!
//! | 未覆盖 | 归谁 | 依据 |
//! |--------|------|------|
//! | 中继控制端口握手 | [`croc_protocol::relay`] | 它已经是独立状态机 |
//! | 数据面上的分块搬运 | `croc-engine` | 需要文件 IO 与并发 |
//! | 本地中继 / 局域网发现 | `croc-engine` | 需要 socket 与组播 |
//! | tailcat（`--transport derp`） | ⏸ pending | `docs/agent/plan/08-tailscale.md` |
//! | 断线重连、store、ssh | 后续增量 | 见 `docs/agent/plan/06-milestones.md` |
//!
//! `externalip` 只在**对端不支持** `inline-peer-metadata-v1` 时才会出现（Go 的
//! `finishDataTransportActivation`）。本状态机宣告该能力，但**能收也能回**，这样与
//! 老版本对端互通时不会卡住。

use croc_pake::{CurveKind, EPHEMERAL_LEN, Initiator, PakeError};
use croc_pakekey::{self as pakekey, Keys, PakeKeyError};
use croc_protocol::{
    IP_REQUEST, IncomingManifest, KIND_PAKE1, ManifestError, ManifestLimits, ManifestPayloads, Message, MessageType,
    ProbeMessage, Randomness, RemoteFileRequest, SenderInfo, SessionKey, encode_endpoints, encode_manifest,
};
use zeroize::Zeroize;

use crate::policy::Answer;

/// 本端在这次会话里扮演的角色。
///
/// **接收方永远是 PAKE 的发起方 A，发送方永远是响应方 B** —— 这是 croc 的协议约定，
/// 不是可以挑的（见 `croc-pakekey::identities`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// 发送文件的一方（PAKE 响应方 B）。
    Sender,
    /// 接收文件的一方（PAKE 发起方 A）。
    Receiver,
}

impl Role {
    /// 这个角色是不是 PAKE 的发起方。
    #[must_use]
    pub const fn is_initiator(self) -> bool {
        matches!(self, Self::Receiver)
    }
}

/// 会话处在哪个大阶段。只用于外部观测与进度展示，状态机内部用的是更细的私有状态。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Stage {
    /// 中继已配对，握手还没开始（发送方在等对方的 `pake`）。
    Idle,
    /// PAKE 进行中。
    Securing,
    /// 清单已交换，等决策：接收方等用户回答，发送方等对方的 `recipientready`。
    AwaitingDecision,
    /// 数据面已就绪，正在搬运。
    Transferring,
    /// 收尾（`close-sender` / `close-recipient` / `finished`）。
    Concluding,
    /// 结束。
    Done,
}

/// 接收方在 PAKE 之前先发的那个**裸帧**。
///
/// 它不是 [`Message`]——帧体就是这 9 个字节，不走 JSON、不走 DEFLATE、也不加密。
/// Go 侧是 `croc.go` 的 `handshakeRequest = []byte("handshake")`：
///
/// - 接收方连上中继、注册完房间之后，**第一件事**就是把它发出去；
/// - 发送方的 `senderWaitForHandshake` 循环读帧，看到它就跳出等待、开始处理 `pake`；
/// - 发送方**不回任何东西**。
///
/// 这一步的用途是"我到了，你可以开始了"。少了它，Go 的发送方会一直卡在
/// `senderWaitForHandshake` 里，而 Rust 的发送方会把 `"handshake"` 当成一条
/// JSON 消息去解压，报 `corrupt deflate stream`——两个方向都通不了。
pub const HANDSHAKE: &[u8] = b"handshake";

/// 进入状态机的事件——**全部是已经发生的事实**，不是待办。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InEvent {
    /// 中继握手完成，房间已注册。发送方只是等；接收方在这里发出第一条 `pake`。
    PeerConnected,
    /// 收到一整条帧的帧体（已从帧头里剥出来，尚未解密）。
    Frame(Vec<u8>),
    /// 用户对清单的决策（只有接收方需要）。
    Decision(Answer),
    /// 接收方挑好了下一个要传的文件。
    ///
    /// `ranges` 是[线路形态](croc_core::chunk::ChunkRanges)的分片区间；**空列表表示
    /// "整个文件"**。文件挑选与"缺哪些块"的扫描由调用方做完——状态机不碰文件系统。
    RequestFile {
        /// `SenderInfo::files_to_transfer` 的下标。
        index: u64,
        /// 要哪些块（扁平三元组布局）。
        ranges: Vec<i64>,
    },
    /// 接收方：所有文件都处理完了，可以发 `finished`。
    AllFilesDone,
    /// 接收方：当前文件的数据面已经收完，可以发 `close-sender`。
    DataFinished,
    /// 外部要求中止（Ctrl-C、GUI 的取消按钮）。
    Cancel,
}

/// 状态机希望外部执行的效果。
///
/// **`apply` 里不许做协议判断** —— 见 `docs/agent/plan/03-architecture.md` §7。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// 把这一帧发到控制连接上。
    Send(Vec<u8>),
    /// 发送方：在数据面上传第 `index` 个文件。
    ///
    /// 只传 `ranges` 指定的块；`ranges` 为空表示整个文件。`streams` 是并行流数
    /// （对齐 Go 的 `min(len(RelayPorts), 8)`）。
    SendFile {
        /// 文件下标。
        index: u64,
        /// 要传的块（扁平三元组布局）。
        ranges: Vec<i64>,
        /// 并行流数。
        streams: u8,
    },
    /// 上报一个进展事件。
    Emit(Event),
    /// 结束，带上最终结论。**这是最后一个效果**，之后状态机进入 [`Stage::Done`]。
    Finish(Outcome),
}

/// 对外上报的进展事件。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Event {
    /// 安全通道已建立（PAKE 双向确认完成）。
    Secured,
    /// 收到（接收方）或发出（发送方）了文件清单。
    Manifest(Box<SenderInfo>),
    /// 一个文件已经收尾（接收方收到 `close-recipient`，或发送方收到 `close-sender`）。
    FileClosed {
        /// 文件下标。
        index: u64,
    },
    /// 数据面开始搬运。
    DataStarted,
    /// 对端显式拒绝了这次传输。
    PeerRefused,
    /// 对端用 `externalip` 报了它看到的本端地址。
    PeerExternalIp(String),
}

/// 会话的最终结论。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// 正常完成。
    Completed,
    /// 对端拒绝接收。
    Refused,
    /// 本端用户拒绝接收。
    Declined,
    /// 被取消。
    Cancelled,
    /// 失败，附上经过脱敏的原因。
    Failed(String),
}

/// 状态机的失败模式。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum SessionError {
    /// 在这个状态下收到了不该出现的消息。
    #[error("unexpected {kind} while {state}")]
    UnexpectedMessage {
        /// 收到的消息类型名。
        kind: String,
        /// 当时的状态（人类可读）。
        state: &'static str,
    },

    /// 对端宣告的 PAKE 协议版本与本端不同。
    #[error("incompatible PAKE protocol version: peer={peer} local={local}")]
    VersionMismatch {
        /// 对端版本。
        peer: u64,
        /// 本端版本。
        local: u64,
    },

    /// 对端的 PAKE 确认标签验不过——口令不对，或者有人在中途改字节。
    #[error("PAKE confirmation failed: the code phrase does not match")]
    ConfirmationFailed,

    /// 握手流程本身出错（曲线名不认、公开值长度不对等）。
    #[error(transparent)]
    Pake(#[from] PakeError),

    /// 密钥编排失败。
    #[error(transparent)]
    PakeKey(#[from] PakeKeyError),

    /// 线路层失败（帧解不开、AEAD 校验不过）。
    #[error(transparent)]
    Protocol(#[from] croc_protocol::ProtocolError),

    /// 清单拆包失败。
    #[error(transparent)]
    Manifest(#[from] ManifestError),

    /// 对端发来的载荷本身不合法（不是"协议违规"，是"数据坏了"）。
    #[error("peer sent a malformed {kind} payload: {reason}")]
    Malformed {
        /// 消息类型名。
        kind: &'static str,
        /// 原因。
        reason: String,
    },

    /// 状态机已经结束，又收到了事件。
    #[error("the session is already finished")]
    AlreadyFinished,

    /// 会话配置本身不合法。
    #[error("invalid session configuration: {0}")]
    BadConfig(String),
}

/// 会话的静态配置。
#[derive(Debug, Clone)]
pub struct SessionConfig<'a> {
    /// 本端角色。
    pub role: Role,
    /// 房间名（`RoomName`，十六进制串）。PAKE 的转录里会绑定它。
    pub room: &'a str,
    /// PAKE 口令（从 codephrase 解出的部分）。
    pub passphrase: &'a [u8],
    /// 本端选择的曲线。接收方把它告诉发送方，所以**由接收方决定**。
    pub curve: CurveKind,
    /// 本端宣告的能力列表，见 [`croc_protocol::declare`]。
    pub features: Vec<String>,
    /// 中继 banner 里给出的数据端口数量。
    pub relay_ports: u8,
    /// 本端机器指纹，用于 `recipientready`。
    pub machine_id: String,
    /// 本端看到的外网 IP（`externalip` 用，可为空）。
    pub external_ip: String,
}

impl SessionConfig<'_> {
    /// 数据面要开几条流：与 Go 一致，`min(relay_ports, 8)`。
    #[must_use]
    pub const fn data_streams(&self) -> u8 {
        if self.relay_ports < 8 { self.relay_ports } else { 8 }
    }
}

/// 状态机内部的细状态。
#[derive(Debug)]
enum State {
    /// 等对端（发送方）或等自己发第一步（接收方）。
    Idle,
    /// 接收方：已发 `pake`，等发送方的 `pake`。
    AwaitingPakeResponse,
    /// 发送方：已回 `pake`，等 `pake-confirm`（A 的标签）。
    AwaitingConfirmationA,
    /// 接收方：已发 `pake-confirm`（A 的标签），等 B 的标签。
    AwaitingConfirmationB,
    /// 接收方：等清单。
    AwaitingManifest,
    /// 接收方：清单分批收集中。
    CollectingManifest(Box<IncomingManifest>),
    /// 接收方：清单已齐，等用户决策。
    AwaitingDecision,
    /// 文件循环中：接收方在挑/收文件，发送方在等请求。
    Transferring,
    /// 结束。
    Done,
}

impl State {
    /// 人类可读的状态名，用于错误信息。
    const fn name(&self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::AwaitingPakeResponse => "awaiting the PAKE response",
            Self::AwaitingConfirmationA => "awaiting the PAKE confirmation",
            Self::AwaitingConfirmationB => "awaiting the peer confirmation",
            Self::AwaitingManifest => "awaiting the file manifest",
            Self::CollectingManifest(_) => "collecting the file manifest",
            Self::AwaitingDecision => "awaiting the user's decision",
            Self::Transferring => "transferring",
            Self::Done => "finished",
        }
    }
}

/// 局域网探测的应答状态（**只有发送方会用到**）。
///
/// 探测有自己的用途串（`local-ip-probe`）与自己的密钥，与正式传输的那套完全独立——
/// 所以它必须单独存，不能塞进 `PakeState`。
struct LocalProbe {
    /// 探测密钥，用来加解密 `ips?` 与 IP 列表。
    key: [u8; croc_protocol::ENCRYPTION_KEY_LEN],
}

impl Drop for LocalProbe {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

/// 进行中的 PAKE。
///
/// 两端留的东西不一样（接收方要留着 [`Initiator`] 走 `accept_response`，发送方派生完
/// 就不需要了），所以用变体而不是一堆 `Option` —— 后者会让"发送方却拿着 Initiator"
/// 这种状态在类型上合法。
enum PakeState {
    /// 还没有开始。
    Fresh,
    /// 接收方（发起方 A）。
    Initiating {
        /// SPAKE2 发起方。
        exchange: Box<Initiator>,
        /// 发给对端的公开值，派生密钥时要用。
        initiator: Vec<u8>,
    },
    /// 已经用完（发送方派生完密钥后就丢掉响应方实例）。
    Consumed,
}

/// 对端会话状态机。
pub struct Session {
    config: SessionConfigOwned,
    state: State,
    pake: PakeState,
    keys: Option<Keys>,
    session: SessionKey,
    /// 发送方：预拆好的清单载荷。
    manifest: Option<ManifestPayloads>,
    /// 发送方：清单原文，用于 `Emit(Event::Manifest)`。
    sender_info: Option<Box<SenderInfo>>,
    /// 当前正在传的文件下标，收尾事件里要带出去。
    current_file: u64,
    /// 局域网探测的应答状态；`None` 表示还没开始或已经答完。
    local_probe: Option<LocalProbe>,
    limits: ManifestLimits,
}

/// [`SessionConfig`] 的拥有型版本——状态机不能借用调用方的字符串。
#[derive(Debug, Clone)]
struct SessionConfigOwned {
    role: Role,
    room: String,
    passphrase: Vec<u8>,
    curve: CurveKind,
    features: Vec<String>,
    relay_ports: u8,
    machine_id: String,
    external_ip: String,
}

impl SessionConfigOwned {
    /// 数据面要开几条流：与 Go 一致，`min(relay_ports, 8)`。
    const fn data_streams(&self) -> u8 {
        if self.relay_ports < 8 { self.relay_ports } else { 8 }
    }
}

impl Drop for SessionConfigOwned {
    fn drop(&mut self) {
        self.passphrase.zeroize();
    }
}

/// PAKE 协议版本号，转成 `Message.version` 要的 `i64`。
///
/// 常量是 `u64`（线路上的语义），而 `Message.version` 是 `i64`（Go 的 `int`）。
/// 版本号永远是个位数，这个转换不可能失败。
fn protocol_version() -> i64 {
    i64::try_from(pakekey::PROTOCOL_VERSION).unwrap_or(i64::MAX)
}

impl core::fmt::Debug for Session {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Session")
            .field("role", &self.config.role)
            .field("state", &self.state.name())
            .field("encrypted", &self.session.is_encrypted())
            .finish()
    }
}

impl Session {
    /// 构造一个发送方会话。
    ///
    /// 清单在这里就被拆成载荷（[`encode_manifest`] 是纯函数），所以"清单太大"这类问题
    /// 在**连中继之前**就会暴露，而不是传到一半才失败。
    ///
    /// # Errors
    ///
    /// 配置非法（房间名或口令为空）或清单超限时返回 [`SessionError`]。
    pub fn new_sender(config: &SessionConfig<'_>, manifest: SenderInfo) -> Result<Self, SessionError> {
        let owned = Self::validate(config, Role::Sender)?;
        let payloads = encode_manifest(&manifest, ManifestLimits::default())?;
        Ok(Self {
            config: owned,
            state: State::Idle,
            pake: PakeState::Fresh,
            keys: None,
            session: SessionKey::Plain,
            manifest: Some(payloads),
            sender_info: Some(Box::new(manifest)),
            current_file: 0,
            local_probe: None,
            limits: ManifestLimits::default(),
        })
    }

    /// 构造一个接收方会话。
    ///
    /// # Errors
    ///
    /// 配置非法（房间名或口令为空）时返回 [`SessionError`]。
    pub fn new_receiver(config: &SessionConfig<'_>) -> Result<Self, SessionError> {
        let owned = Self::validate(config, Role::Receiver)?;
        Ok(Self {
            config: owned,
            state: State::Idle,
            pake: PakeState::Fresh,
            keys: None,
            session: SessionKey::Plain,
            manifest: None,
            sender_info: None,
            current_file: 0,
            local_probe: None,
            limits: ManifestLimits::default(),
        })
    }

    fn validate(config: &SessionConfig<'_>, role: Role) -> Result<SessionConfigOwned, SessionError> {
        if config.room.is_empty() {
            return Err(SessionError::BadConfig("room name is required".to_owned()));
        }
        if config.passphrase.is_empty() {
            return Err(SessionError::BadConfig("passphrase is required".to_owned()));
        }
        if config.relay_ports == 0 {
            return Err(SessionError::BadConfig("at least one relay port is required".to_owned()));
        }
        Ok(SessionConfigOwned {
            role,
            room: config.room.to_owned(),
            passphrase: config.passphrase.to_vec(),
            curve: config.curve,
            features: config.features.clone(),
            relay_ports: config.relay_ports,
            machine_id: config.machine_id.clone(),
            external_ip: config.external_ip.clone(),
        })
    }

    /// 本端角色。
    #[must_use]
    pub const fn role(&self) -> Role {
        self.config.role
    }

    /// 当前阶段。
    #[must_use]
    pub const fn stage(&self) -> Stage {
        match self.state {
            State::Idle => Stage::Idle,
            State::AwaitingPakeResponse
            | State::AwaitingConfirmationA
            | State::AwaitingConfirmationB
            | State::AwaitingManifest
            | State::CollectingManifest(_) => Stage::Securing,
            State::AwaitingDecision => Stage::AwaitingDecision,
            State::Transferring => Stage::Transferring,
            State::Done => Stage::Done,
        }
    }

    /// 安全通道是否已建立。
    #[must_use]
    pub const fn is_secured(&self) -> bool {
        self.session.is_encrypted()
    }

    /// 数据面用的流量密钥。
    ///
    /// 数据端口上跑的**不是**消息层，而是裸的 `nonce ‖ AEAD(偏移 ‖ 数据)`，所以
    /// `croc-engine` 必须能拿到这把密钥。密钥从 PAKE 派生，与消息层共用同一把。
    ///
    /// 握手完成前返回 `None`——调用方拿不到密钥就不可能误在明文上发数据。
    #[must_use]
    pub const fn data_key(&self) -> Option<[u8; croc_protocol::ENCRYPTION_KEY_LEN]> {
        match &self.session {
            SessionKey::Aead(key) => Some(*key),
            SessionKey::Plain => None,
        }
    }

    /// 处理一个事件。
    ///
    /// # Errors
    ///
    /// 协议违规（消息顺序不对、版本不符、确认标签验不过）时返回 [`SessionError`]。
    /// **对端拒绝接收不是错误**——那是 [`Outcome::Refused`]。
    pub fn handle(&mut self, event: InEvent, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        if matches!(self.state, State::Done) {
            return Err(SessionError::AlreadyFinished);
        }
        match event {
            InEvent::Cancel => Ok(self.finish(Outcome::Cancelled)),
            InEvent::PeerConnected => self.on_peer_connected(randomness),
            InEvent::Frame(payload) => self.on_frame(&payload, randomness),
            InEvent::Decision(answer) => self.on_decision(answer, randomness),
            InEvent::RequestFile { index, ranges } => self.on_request_file(index, ranges, randomness),
            InEvent::AllFilesDone => self.on_all_files_done(randomness),
            InEvent::DataFinished => self.on_data_finished(randomness),
        }
    }

    // ─────────────────────────── 各事件的处理 ───────────────────────────

    /// 中继配对完成。接收方在这里发出第一条 `pake`；发送方只是等着。
    fn on_peer_connected(&mut self, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        if !matches!(self.state, State::Idle) {
            return Err(SessionError::UnexpectedMessage {
                kind: "peer-connected".to_owned(),
                state: self.state.name(),
            });
        }
        if self.config.role == Role::Sender {
            // 发送方是响应方：等接收方的 pake。
            return Ok(Vec::new());
        }

        let exchange = pakekey::initiator(
            &self.config.passphrase,
            self.config.curve,
            pakekey::PURPOSE_TRANSFER,
            &self.config.room,
            croc_protocol::random::array::<EPHEMERAL_LEN>(randomness),
        )?;
        let initiator = exchange.public_bytes().into_bytes();
        let message = Message::new(MessageType::Pake)
            .with_version(protocol_version())
            .with_bytes(initiator.clone())
            .with_bytes2(self.config.curve.name().as_bytes().to_vec())
            .with_features(self.config.features.clone());

        self.pake = PakeState::Initiating { exchange: Box::new(exchange), initiator };
        self.state = State::AwaitingPakeResponse;
        // 顺序要紧：先裸帧 `handshake`，紧接着才是 `pake`。两者之间没有往返——
        // 对端的发送方一看到 `handshake` 就跳出等待循环，随即在流里读到 `pake`。
        Ok(vec![Effect::Send(HANDSHAKE.to_vec()), Effect::Send(self.encode(&message, randomness)?)])
    }

    fn on_frame(&mut self, payload: &[u8], randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        // 正式 PAKE 之前还有一段**不属于消息层**的东西：局域网探测（`pake1`/`pake2`/
        // `ips?`）与裸帧 `handshake`。它们不 DEFLATE、可能是裸 JSON、也可能用探测密钥
        // 加密——全都不该拿去 `self.session.open()`。
        if let Some(effects) = self.try_local_probe(payload, randomness)? {
            return Ok(effects);
        }
        let message = self.session.open(payload)?;
        let kind = message.kind.clone().unwrap_or(MessageType::Unknown(String::new()));
        match kind {
            MessageType::Error => {
                let reason = message.message.clone();
                Ok(self.finish(Outcome::Failed(format!("peer error: {reason}"))))
            }
            MessageType::Pake => self.on_pake(&message, randomness),
            MessageType::PakeConfirm => self.on_pake_confirm(&message, randomness),
            MessageType::ExternalIp => self.on_external_ip(&message, randomness),
            MessageType::FileInfoStart
            | MessageType::FileInfoBatch
            | MessageType::FileInfoEnd
            | MessageType::FileInfo => self.on_manifest(&message, kind),
            MessageType::RecipientReady => self.on_recipient_ready(&message),
            MessageType::CloseSender => self.on_close_sender(randomness),
            MessageType::CloseRecipient => self.on_close_recipient(),
            MessageType::Finished => Ok(self.on_finished(randomness)?),
            other => Err(SessionError::UnexpectedMessage { kind: other.to_string(), state: self.state.name() }),
        }
    }

    /// 处理正式 PAKE 之前的那几帧。**只有发送方需要**。
    ///
    /// Go 的接收方在连上中继之后默认先做一次局域网探测（除非 `--no-local` 或给了
    /// `--ip`）：它发 `pake1`，等 `pake2`，再问 `ips?`。不答这一段，Go 的接收方会
    /// 卡在那里等 `pake2`，而我们会把那段裸 JSON 当成 `Message` 去解压，报
    /// `corrupt deflate stream`——两个方向都断在第一帧上。
    ///
    /// 返回 `None` 表示"这不是探测帧"，调用方继续走正常的消息路径。
    ///
    /// # Errors
    ///
    /// 曲线名不认识、SPAKE2 失败、密钥编排失败。
    fn try_local_probe(
        &mut self,
        payload: &[u8],
        randomness: &mut impl Randomness,
    ) -> Result<Option<Vec<Effect>>, SessionError> {
        // 裸帧 `handshake`：接收方在 PAKE 之前发的"我到了"。发送方什么都不用做，
        // 等 `pake` 就是。两种角色都可能收到它。
        if payload == HANDSHAKE {
            return Ok(Some(Vec::new()));
        }
        // 我们是接收方时不主动探测（等价于 Go 的 `--no-local`）：本项目还没有本地中继，
        // 探到了也没得连，白白多两个往返。
        if self.config.role != Role::Sender || !matches!(self.state, State::Idle) {
            return Ok(None);
        }

        // 已经派生过探测密钥：这一帧应该是加密的 `ips?`。
        if let Some(probe) = &self.local_probe {
            let Ok(plain) = croc_protocol::decrypt(payload, &probe.key) else {
                return Ok(None);
            };
            if plain != IP_REQUEST {
                return Ok(None);
            }
            // 回一个**空**地址列表。我们没有本地中继，接收方看到 `len(ips) <= 1`
            // 就会回落到中继路径——这正是 Go 的 `--no-local` 发送方的行为。
            let body = encode_endpoints(0, &[])?;
            let nonce = croc_protocol::random::nonce(randomness);
            let frame = croc_protocol::encrypt(&body, &probe.key, &nonce)?;
            self.local_probe = None;
            return Ok(Some(vec![Effect::Send(frame)]));
        }

        // `pake1`：探测的发起方是接收方，我们是应答方。
        if !ProbeMessage::looks_like_probe(payload) {
            return Ok(None);
        }
        let Ok(message) = ProbeMessage::from_bytes(payload) else {
            return Ok(None);
        };
        if message.kind != KIND_PAKE1 {
            return Ok(None);
        }

        let curve = CurveKind::from_name(&message.curve)?;
        let mut exchange =
            pakekey::responder(&self.config.passphrase, curve, pakekey::PURPOSE_LOCAL_PROBE, &self.config.room)?;
        exchange.accept_request(&message.bytes, croc_protocol::random::array::<EPHEMERAL_LEN>(randomness))?;
        let responder = exchange.public_bytes().into_bytes();
        let mut salt = vec![0u8; pakekey::SALT_SIZE];
        randomness.fill(&mut salt);

        // 注意用途串是 `local-ip-probe`，与正式传输的 `peer-transfer` 不是一套密钥。
        let keys = Self::derive_keys(
            &self.config.room,
            pakekey::PURPOSE_LOCAL_PROBE,
            exchange.session_key(),
            curve,
            &message.bytes,
            &responder,
            &salt,
        )?;
        let reply = ProbeMessage::pake2(responder, salt, protocol_version(), curve.name());
        self.local_probe = Some(LocalProbe { key: *keys.encryption_key() });

        Ok(Some(vec![Effect::Send(reply.to_bytes()?)]))
    }

    /// `pake`：两端都走这里，方向由角色决定。
    fn on_pake(&mut self, message: &Message, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        let version = u64::try_from(message.version).unwrap_or(u64::MAX);
        if version != pakekey::PROTOCOL_VERSION {
            return Err(SessionError::VersionMismatch { peer: version, local: pakekey::PROTOCOL_VERSION });
        }
        match (&mut self.state, self.config.role) {
            // 发送方收到接收方的发起值：算出响应值 + 盐，回一条 `pake`。
            (State::Idle, Role::Sender) => {
                let curve_name = String::from_utf8_lossy(&message.bytes2).into_owned();
                // 曲线由接收方挑，发送方照着用——与 Go 的 `c.pakeCurve = string(m.Bytes2)` 一致。
                let curve = CurveKind::from_name(&curve_name)?;
                let mut exchange =
                    pakekey::responder(&self.config.passphrase, curve, pakekey::PURPOSE_TRANSFER, &self.config.room)?;
                exchange.accept_request(&message.bytes, croc_protocol::random::array::<EPHEMERAL_LEN>(randomness))?;
                let responder = exchange.public_bytes().into_bytes();
                let mut salt = vec![0u8; pakekey::SALT_SIZE];
                randomness.fill(&mut salt);

                let keys = Self::derive_keys(
                    &self.config.room,
                    pakekey::PURPOSE_TRANSFER,
                    exchange.session_key(),
                    curve,
                    &message.bytes,
                    &responder,
                    &salt,
                )?;
                self.keys = Some(keys);

                let reply = Message::new(MessageType::Pake)
                    .with_version(protocol_version())
                    .with_bytes(responder)
                    .with_bytes2(salt)
                    .with_features(self.config.features.clone());

                // 响应方的公开值与盐已经写进线上字节，派生也做完了，实例可以丢。
                self.pake = PakeState::Consumed;
                self.state = State::AwaitingConfirmationA;
                Ok(vec![Effect::Send(self.encode(&reply, randomness)?)])
            }
            // 接收方收到发送方的响应值：算出密钥，回 `pake-confirm`（A 的标签）。
            (State::AwaitingPakeResponse, Role::Receiver) => {
                if message.bytes2.len() != pakekey::SALT_SIZE {
                    return Err(SessionError::Malformed {
                        kind: "pake",
                        reason: format!("salt must be {} bytes, got {}", pakekey::SALT_SIZE, message.bytes2.len()),
                    });
                }
                // 先把 `&mut self.pake` 的借用收在一个块里，派生要用到 `self.config`。
                let (shared, initiator) = match &mut self.pake {
                    PakeState::Initiating { exchange, initiator } => {
                        exchange.accept_response(&message.bytes)?;
                        (exchange.session_key(), initiator.clone())
                    }
                    PakeState::Fresh | PakeState::Consumed => {
                        return Err(SessionError::UnexpectedMessage {
                            kind: "pake".to_owned(),
                            state: "awaiting the PAKE response without an initiator",
                        });
                    }
                };
                let keys = Self::derive_keys(
                    &self.config.room,
                    pakekey::PURPOSE_TRANSFER,
                    shared,
                    self.config.curve,
                    &initiator,
                    &message.bytes,
                    &message.bytes2,
                )?;
                let confirm = Message::new(MessageType::PakeConfirm)
                    .with_version(protocol_version())
                    .with_bytes(keys.confirmation_a().to_vec());
                self.keys = Some(keys);

                self.state = State::AwaitingConfirmationB;
                Ok(vec![Effect::Send(self.encode(&confirm, randomness)?)])
            }
            _ => Err(SessionError::UnexpectedMessage { kind: "pake".to_owned(), state: self.state.name() }),
        }
    }

    /// `pake-confirm`：双方各自校验**对方角色**的标签，然后切换到加密通道。
    fn on_pake_confirm(
        &mut self,
        message: &Message,
        randomness: &mut impl Randomness,
    ) -> Result<Vec<Effect>, SessionError> {
        let version = u64::try_from(message.version).unwrap_or(u64::MAX);
        if version != pakekey::PROTOCOL_VERSION {
            return Err(SessionError::VersionMismatch { peer: version, local: pakekey::PROTOCOL_VERSION });
        }
        let keys = self.keys.as_ref().ok_or(SessionError::ConfirmationFailed)?;
        let mut effects = Vec::new();

        match (&self.state, self.config.role) {
            // 发送方校验 A 的标签，回自己的 B 标签，然后立刻开始发清单。
            (State::AwaitingConfirmationA, Role::Sender) => {
                if !pakekey::confirm(keys.confirmation_a(), &message.bytes) {
                    return Err(SessionError::ConfirmationFailed);
                }
                let reply = Message::new(MessageType::PakeConfirm)
                    .with_version(protocol_version())
                    .with_bytes(keys.confirmation_b().to_vec());
                effects.push(Effect::Send(self.encode(&reply, randomness)?));
            }
            // 接收方校验 B 的标签，之后就是加密通道了。
            (State::AwaitingConfirmationB, Role::Receiver) => {
                if !pakekey::confirm(keys.confirmation_b(), &message.bytes) {
                    return Err(SessionError::ConfirmationFailed);
                }
            }
            _ => {
                return Err(SessionError::UnexpectedMessage {
                    kind: "pake-confirm".to_owned(),
                    state: self.state.name(),
                });
            }
        }

        self.activate_cipher();
        effects.push(Effect::Emit(Event::Secured));

        if self.config.role == Role::Sender {
            effects.extend(self.send_manifest(randomness)?);
            self.state = State::Transferring;
        } else {
            self.state = State::AwaitingManifest;
        }
        Ok(effects)
    }

    /// `externalip`：老对端（不支持 `inline-peer-metadata-v1`）用它交换地址。
    fn on_external_ip(
        &mut self,
        message: &Message,
        randomness: &mut impl Randomness,
    ) -> Result<Vec<Effect>, SessionError> {
        let mut effects = vec![Effect::Emit(Event::PeerExternalIp(message.message.clone()))];
        // 接收方先报，发送方回一条——与 Go 的 `processExternalIP` 一致。
        if self.config.role == Role::Sender {
            let reply = Message::new(MessageType::ExternalIp).with_message(self.config.external_ip.clone());
            effects.push(Effect::Send(self.encode(&reply, randomness)?));
        }
        Ok(effects)
    }

    /// 清单：接收方的三条消息（`fileinfo` 是旧版单条格式，等价于 start+end）。
    fn on_manifest(&mut self, message: &Message, kind: MessageType) -> Result<Vec<Effect>, SessionError> {
        if self.config.role != Role::Receiver {
            return Err(SessionError::UnexpectedMessage { kind: kind.to_string(), state: self.state.name() });
        }
        match (kind.clone(), &mut self.state) {
            (MessageType::FileInfoStart, State::AwaitingManifest) => {
                let incoming = IncomingManifest::start(&message.bytes, self.limits)?;
                self.state = State::CollectingManifest(Box::new(incoming));
                Ok(Vec::new())
            }
            (MessageType::FileInfoBatch, State::CollectingManifest(incoming)) => {
                incoming.batch(message.num, &message.bytes)?;
                Ok(Vec::new())
            }
            (MessageType::FileInfoEnd, State::CollectingManifest(_)) => {
                let State::CollectingManifest(incoming) = core::mem::replace(&mut self.state, State::Done) else {
                    return Err(SessionError::UnexpectedMessage { kind: kind.to_string(), state: "collecting" });
                };
                let info = incoming.finish(message.num, &message.bytes)?;
                self.state = State::AwaitingDecision;
                Ok(vec![Effect::Emit(Event::Manifest(Box::new(info)))])
            }
            // 旧版单条 `fileinfo`：`start` 之后立刻 `finish`。
            (MessageType::FileInfo, State::AwaitingManifest) => {
                let incoming = IncomingManifest::start(&message.bytes, self.limits)?;
                let info = incoming.finish(0, &[])?;
                self.state = State::AwaitingDecision;
                Ok(vec![Effect::Emit(Event::Manifest(Box::new(info)))])
            }
            (other, _) => Err(SessionError::UnexpectedMessage { kind: other.to_string(), state: self.state.name() }),
        }
    }

    /// 接收方对清单的决策。
    fn on_decision(&mut self, answer: Answer, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        if !matches!(self.state, State::AwaitingDecision) {
            return Err(SessionError::UnexpectedMessage { kind: "decision".to_owned(), state: self.state.name() });
        }
        match answer {
            Answer::Decline => {
                let refusal = Message::new(MessageType::Error).with_message("refusing files");
                let frame = self.encode(&refusal, randomness)?;
                let mut effects = vec![Effect::Send(frame), Effect::Emit(Event::PeerRefused)];
                effects.extend(self.finish(Outcome::Declined));
                Ok(effects)
            }
            // 接受之后进入文件循环，由调用方逐个挑文件喂 `RequestFile`。
            Answer::Accept => {
                self.state = State::Transferring;
                Ok(vec![Effect::Emit(Event::DataStarted)])
            }
        }
    }

    /// 接收方挑好了下一个文件：发 `recipientready`。
    fn on_request_file(
        &mut self,
        index: u64,
        ranges: Vec<i64>,
        randomness: &mut impl Randomness,
    ) -> Result<Vec<Effect>, SessionError> {
        if !(self.config.role == Role::Receiver && matches!(self.state, State::Transferring)) {
            return Err(SessionError::UnexpectedMessage { kind: "request-file".to_owned(), state: self.state.name() });
        }
        self.current_file = index;
        let request = RemoteFileRequest {
            current_file_chunk_ranges: ranges,
            files_to_transfer_current_num: i64::try_from(index).unwrap_or(i64::MAX),
            machine_id: self.config.machine_id.clone(),
            external_ip: self.config.external_ip.clone(),
            reconnect_version: 0,
            features: self.config.features.clone(),
        };
        let message = Message::new(MessageType::RecipientReady).with_bytes(request.to_bytes()?);
        Ok(vec![Effect::Send(self.encode(&message, randomness)?)])
    }

    /// 接收方：当前文件收完，发 `close-sender`。
    fn on_data_finished(&mut self, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        if !(self.config.role == Role::Receiver && matches!(self.state, State::Transferring)) {
            return Err(SessionError::UnexpectedMessage { kind: "data-finished".to_owned(), state: self.state.name() });
        }
        let close = Message::new(MessageType::CloseSender);
        Ok(vec![Effect::Send(self.encode(&close, randomness)?)])
    }

    /// 接收方：所有文件都处理完了，发 `finished`。
    fn on_all_files_done(&mut self, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        if !(self.config.role == Role::Receiver && matches!(self.state, State::Transferring)) {
            return Err(SessionError::UnexpectedMessage {
                kind: "all-files-done".to_owned(),
                state: self.state.name(),
            });
        }
        let finished = Message::new(MessageType::Finished);
        let frame = self.encode(&finished, randomness)?;
        let mut effects = vec![Effect::Send(frame), Effect::Emit(Event::DataStarted)];
        effects.extend(self.finish(Outcome::Completed));
        Ok(effects)
    }

    /// 发送方收到 `recipientready`：把要传的文件交给调用方。
    fn on_recipient_ready(&mut self, message: &Message) -> Result<Vec<Effect>, SessionError> {
        if !(self.config.role == Role::Sender && matches!(self.state, State::Transferring)) {
            return Err(SessionError::UnexpectedMessage {
                kind: "recipientready".to_owned(),
                state: self.state.name(),
            });
        }
        let request = RemoteFileRequest::from_bytes(&message.bytes)?;
        let index = u64::try_from(request.files_to_transfer_current_num).unwrap_or(0);
        self.current_file = index;

        let mut effects = Vec::new();
        if !request.external_ip.is_empty() {
            effects.push(Effect::Emit(Event::PeerExternalIp(request.external_ip)));
        }
        effects.push(Effect::SendFile {
            index,
            ranges: request.current_file_chunk_ranges,
            streams: self.config.data_streams(),
        });
        Ok(effects)
    }

    /// 发送方收到 `close-sender`（接收方说"这个文件收完了"）：回 `close-recipient`。
    fn on_close_sender(&mut self, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        if !(self.config.role == Role::Sender && matches!(self.state, State::Transferring)) {
            return Err(SessionError::UnexpectedMessage { kind: "close-sender".to_owned(), state: self.state.name() });
        }
        let close = Message::new(MessageType::CloseRecipient);
        let frame = self.encode(&close, randomness)?;
        Ok(vec![Effect::Send(frame), Effect::Emit(Event::FileClosed { index: self.current_file })])
    }

    /// 接收方收到 `close-recipient`：这个文件彻底收尾，可以挑下一个了。
    fn on_close_recipient(&mut self) -> Result<Vec<Effect>, SessionError> {
        if !(self.config.role == Role::Receiver && matches!(self.state, State::Transferring)) {
            return Err(SessionError::UnexpectedMessage {
                kind: "close-recipient".to_owned(),
                state: self.state.name(),
            });
        }
        Ok(vec![Effect::Emit(Event::FileClosed { index: self.current_file })])
    }

    /// 收到 `finished`：回一条 `finished` 然后双方都结束。
    fn on_finished(&mut self, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        let reply = Message::new(MessageType::Finished);
        let frame = self.encode(&reply, randomness)?;
        let mut effects = vec![Effect::Send(frame)];
        effects.extend(self.finish(Outcome::Completed));
        Ok(effects)
    }

    // ─────────────────────────── 内部工具 ───────────────────────────

    /// 用当前会话密钥封一条消息。
    fn encode(&self, message: &Message, randomness: &mut impl Randomness) -> Result<Vec<u8>, SessionError> {
        let nonce = croc_protocol::random::nonce(randomness);
        Ok(self.session.seal(message, &nonce)?)
    }

    /// 派生密钥。两端都走这一条，保证转录里的字段顺序一致。
    ///
    /// 做成关联函数（而不是 `&self` 方法）是为了让调用方能在 `&mut self.pake` 还活着
    /// 的时候拿到派生结果——`derive_keys` 只需要房间名，不需要整个会话。
    fn derive_keys(
        room: &str,
        purpose: &str,
        shared: Option<[u8; EPHEMERAL_LEN]>,
        curve: CurveKind,
        initiator: &[u8],
        responder: &[u8],
        salt: &[u8],
    ) -> Result<Keys, SessionError> {
        let shared = shared.ok_or(SessionError::ConfirmationFailed)?;
        Ok(pakekey::derive(&shared, &pakekey::Context { purpose, room, curve, initiator, responder, salt })?)
    }

    /// 把派生出的流量密钥装进会话，并立刻丢掉中间材料。
    fn activate_cipher(&mut self) {
        if let Some(keys) = self.keys.take() {
            self.session = SessionKey::Aead(*keys.encryption_key());
            // `Keys` 的 `Drop` 会清零其余部分。
        }
    }

    /// 发送方的清单三连：`fileinfo-start` + `fileinfo-batch*` + `fileinfo-end`。
    fn send_manifest(&mut self, randomness: &mut impl Randomness) -> Result<Vec<Effect>, SessionError> {
        let payloads = self.manifest.take().ok_or_else(|| SessionError::BadConfig("no manifest to send".to_owned()))?;
        let mut effects = Vec::with_capacity(payloads.batches.len() + 3);
        let start = Message::new(MessageType::FileInfoStart).with_bytes(payloads.header.clone());
        effects.push(Effect::Send(self.encode(&start, randomness)?));
        for (index, batch) in payloads.batches.iter().enumerate() {
            let num = i64::try_from(index).unwrap_or(i64::MAX);
            let message = Message::new(MessageType::FileInfoBatch).with_num(num).with_bytes(batch.clone());
            effects.push(Effect::Send(self.encode(&message, randomness)?));
        }
        let end = Message::new(MessageType::FileInfoEnd).with_num(payloads.batch_count());
        effects.push(Effect::Send(self.encode(&end, randomness)?));
        if let Some(info) = self.sender_info.take() {
            effects.push(Effect::Emit(Event::Manifest(info)));
        }
        Ok(effects)
    }

    /// 结束会话。
    fn finish(&mut self, outcome: Outcome) -> Vec<Effect> {
        self.state = State::Done;
        vec![Effect::Finish(outcome)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use croc_protocol::FileInfo;

    const ROOM: &str = "8f14e45fceea167a5a36dedd4bea2543";
    const PASSPHRASE: &[u8] = b"acid-acorn-acre";
    const MACHINE: &str = "0123456789abcdef0123456789abcdef";

    /// 确定性随机源：从 `start` 起逐字节递增。**仅测试用。**
    struct Counting(u8);

    impl Counting {
        const fn new(start: u8) -> Self {
            Self(start)
        }
    }

    impl Randomness for Counting {
        fn fill(&mut self, buffer: &mut [u8]) {
            for byte in buffer.iter_mut() {
                *byte = self.0;
                self.0 = self.0.wrapping_add(1);
            }
        }
    }

    fn config(role: Role, passphrase: &[u8]) -> SessionConfig<'_> {
        SessionConfig {
            role,
            room: ROOM,
            passphrase,
            curve: CurveKind::Siec,
            features: croc_protocol::declare(croc_protocol::TailcatDeclaration::Unsupported),
            relay_ports: 4,
            machine_id: MACHINE.to_owned(),
            external_ip: String::new(),
        }
    }

    fn manifest() -> SenderInfo {
        SenderInfo {
            files_to_transfer: vec![
                FileInfo { name: "a.bin".to_owned(), size: 1024, ..FileInfo::default() },
                FileInfo { name: "b.bin".to_owned(), size: 0, ..FileInfo::default() },
            ],
            empty_folders_to_transfer: vec![FileInfo {
                name: "empty".to_owned(),
                folder_remote: "empty".to_owned(),
                mode: 0o755 | (1 << 31),
                ..FileInfo::default()
            }],
            total_number_folders: 1,
            machine_id: MACHINE.to_owned(),
            ..SenderInfo::default()
        }
    }

    /// 一次 `handle` 的产出落到哪里去。
    #[derive(Default)]
    struct Sink {
        /// 要发给对端的帧。
        wire: Vec<Vec<u8>>,
        /// 上报的事件。
        events: Vec<Event>,
        /// 结束结论。
        outcome: Option<Outcome>,
        /// 交给引擎的"传这个文件"。
        transfers: Vec<(u64, Vec<i64>, u8)>,
    }

    fn apply(effects: Vec<Effect>, sink: &mut Sink) {
        for effect in effects {
            match effect {
                Effect::Send(frame) => sink.wire.push(frame),
                Effect::SendFile { index, ranges, streams } => sink.transfers.push((index, ranges, streams)),
                Effect::Emit(event) => sink.events.push(event),
                Effect::Finish(result) => sink.outcome = Some(result),
            }
        }
    }

    /// 一次完整会话的确定性重放结果。
    struct Replay {
        sender: Option<Outcome>,
        receiver: Option<Outcome>,
        sender_events: Vec<Event>,
        receiver_events: Vec<Event>,
    }

    /// 把发送方与接收方对着跑一遍：一方的帧原样喂给另一方，
    /// 由这个测试扮演 `croc-engine` 的角色（挑文件、跑数据面）。
    ///
    /// 这正是"协议层是纯状态机"换来的东西——不需要 socket、不需要超时、不需要重试，
    /// 结果完全确定。Go 版做不到这一点（IO 与状态转移缠在同一个 goroutine 里）。
    fn replay(sender_passphrase: &[u8], receiver_passphrase: &[u8], answer: Answer) -> Replay {
        let mut sender =
            Session::new_sender(&config(Role::Sender, sender_passphrase), manifest()).expect("发送方应能构造");
        let mut receiver = Session::new_receiver(&config(Role::Receiver, receiver_passphrase)).expect("接收方应能构造");

        let mut sender_rng = Counting::new(0x10);
        let mut receiver_rng = Counting::new(0x80);

        let mut to_sender: Vec<Vec<u8>> = Vec::new();
        let mut to_receiver: Vec<Vec<u8>> = Vec::new();
        let mut sender_sink = Sink::default();
        let mut receiver_sink = Sink::default();

        apply(sender.handle(InEvent::PeerConnected, &mut sender_rng).expect("配对"), &mut sender_sink);
        to_receiver.append(&mut sender_sink.wire);
        let mut effects = receiver.handle(InEvent::PeerConnected, &mut receiver_rng).expect("配对");
        // 接收方的第一条帧必须是裸帧 `handshake`——少了它 Go 的发送方会一直卡在
        // `senderWaitForHandshake` 里。这条断言是"两向互通"的最小护栏。
        match effects.first() {
            Some(Effect::Send(frame)) => assert_eq!(frame.as_slice(), HANDSHAKE, "接收方必须先发 handshake 裸帧"),
            other => panic!("接收方的第一条效果应是 handshake 帧，实得 {other:?}"),
        }
        effects.remove(0);
        apply(effects, &mut receiver_sink);
        to_sender.append(&mut receiver_sink.wire);
        // 裸帧也喂给发送方：它必须吃掉，而不是当成 JSON 去解压。
        to_sender.insert(0, HANDSHAKE.to_vec());

        // 引擎侧的文件循环状态：还剩几个文件要传。
        let mut pending_files: Vec<u64> = vec![0, 1];
        let mut awaiting_close = false;

        for _round in 0..64 {
            if sender_sink.outcome.is_some() && receiver_sink.outcome.is_some() {
                break;
            }
            let mut progressed = false;

            // 已经结束的一方不再收帧：`finished` 是"回一条就散"，回声会再飘回来一次，
            // 那不是错误，只是对端还没来得及关连接（真实实现里 socket 已经关了）。
            for frame in core::mem::take(&mut to_sender) {
                progressed = true;
                if sender_sink.outcome.is_some() {
                    continue;
                }
                apply(sender.handle(InEvent::Frame(frame), &mut sender_rng).expect("发送方应能处理"), &mut sender_sink);
                to_receiver.append(&mut sender_sink.wire);
            }
            for frame in core::mem::take(&mut to_receiver) {
                progressed = true;
                if receiver_sink.outcome.is_some() {
                    continue;
                }
                apply(
                    receiver.handle(InEvent::Frame(frame), &mut receiver_rng).expect("接收方应能处理"),
                    &mut receiver_sink,
                );
                to_sender.append(&mut receiver_sink.wire);
            }

            // 接收方拿到清单 ⇒ 用户决策。
            if receiver.stage() == Stage::AwaitingDecision && !receiver_sink.events.is_empty() {
                let decided = receiver_sink.events.iter().any(|event| matches!(event, Event::DataStarted));
                if !decided {
                    progressed = true;
                    apply(
                        receiver.handle(InEvent::Decision(answer), &mut receiver_rng).expect("决策"),
                        &mut receiver_sink,
                    );
                    to_sender.append(&mut receiver_sink.wire);
                }
            }

            // 引擎：接收方在文件循环里 ⇒ 挑下一个文件；没有就收尾。
            if receiver.stage() == Stage::Transferring && !awaiting_close {
                progressed = true;
                if let Some(index) = pending_files.first().copied() {
                    // 第 0 个文件只请求前半部分，第 1 个（空文件）请求整个文件——
                    // 正好覆盖"显式区间"与"空列表 = 整个文件"两条路径。
                    let ranges = if index == 0 { vec![crate::chunk::CHUNK_SIZE as i64, 0, 1] } else { Vec::new() };
                    apply(
                        receiver.handle(InEvent::RequestFile { index, ranges }, &mut receiver_rng).expect("请求文件"),
                        &mut receiver_sink,
                    );
                    to_sender.append(&mut receiver_sink.wire);
                    awaiting_close = true;
                } else {
                    apply(receiver.handle(InEvent::AllFilesDone, &mut receiver_rng).expect("收尾"), &mut receiver_sink);
                    to_sender.append(&mut receiver_sink.wire);
                }
            }

            // 引擎：发送方收到 SendFile ⇒ 跑完数据面 ⇒ 接收方发 close-sender。
            if !sender_sink.transfers.is_empty() {
                progressed = true;
                sender_sink.transfers.clear();
                // 数据面是"接收方收完后发 close-sender"驱动的。
                apply(
                    receiver.handle(InEvent::DataFinished, &mut receiver_rng).expect("数据面完成"),
                    &mut receiver_sink,
                );
                to_sender.append(&mut receiver_sink.wire);
            }

            // 引擎：一个文件收尾 ⇒ 从待办里划掉。
            let closed = receiver_sink.events.iter().filter(|event| matches!(event, Event::FileClosed { .. })).count();
            while pending_files.len() > closed {
                pending_files.remove(0);
            }
            if closed > 0 && pending_files.len() == closed {
                awaiting_close = false;
            }
            if sender_sink.events.iter().any(|event| matches!(event, Event::FileClosed { .. })) {
                awaiting_close = false;
            }

            if !progressed {
                break;
            }
        }

        Replay {
            sender: sender_sink.outcome,
            receiver: receiver_sink.outcome,
            sender_events: sender_sink.events,
            receiver_events: receiver_sink.events,
        }
    }

    #[test]
    fn a_full_relay_session_runs_to_completion() {
        let result = replay(PASSPHRASE, PASSPHRASE, Answer::Accept);
        assert_eq!(result.sender, Some(Outcome::Completed), "发送方应正常结束");
        assert_eq!(result.receiver, Some(Outcome::Completed), "接收方应正常结束");
        assert!(result.sender_events.contains(&Event::Secured), "发送方应上报安全通道建立");
        assert!(result.receiver_events.contains(&Event::Secured), "接收方应上报安全通道建立");
    }

    #[test]
    fn the_receiver_receives_the_whole_manifest() {
        let result = replay(PASSPHRASE, PASSPHRASE, Answer::Accept);
        let manifest_event = result
            .receiver_events
            .iter()
            .find_map(|event| match event {
                Event::Manifest(info) => Some(info.clone()),
                _ => None,
            })
            .expect("接收方应收到清单");
        assert_eq!(manifest_event.files_to_transfer.len(), 2);
        assert_eq!(manifest_event.empty_folders_to_transfer.len(), 1);
        assert_eq!(manifest_event.total_number_folders, 1);
    }

    #[test]
    fn the_sender_is_told_exactly_which_chunks_to_send() {
        let mut sender = Session::new_sender(&config(Role::Sender, PASSPHRASE), manifest()).expect("构造");
        let mut rng = Counting::new(0);
        sender.session = SessionKey::Aead([0x5a; croc_protocol::ENCRYPTION_KEY_LEN]);
        sender.state = State::Transferring;

        // 伪造一条 recipientready：第 0 个文件、只请求第 2 块。
        let request = RemoteFileRequest {
            current_file_chunk_ranges: vec![crate::chunk::CHUNK_SIZE as i64, 2 * crate::chunk::CHUNK_SIZE as i64, 1],
            files_to_transfer_current_num: 0,
            ..RemoteFileRequest::default()
        };
        let message = Message::new(MessageType::RecipientReady).with_bytes(request.to_bytes().expect("编码"));
        let nonce = croc_protocol::random::nonce(&mut rng);
        let frame = SessionKey::Aead([0x5a; croc_protocol::ENCRYPTION_KEY_LEN]).seal(&message, &nonce).expect("封包");

        let effects = sender.handle(InEvent::Frame(frame), &mut rng).expect("应能处理");
        let expected = vec![(0u64, vec![crate::chunk::CHUNK_SIZE as i64, 2 * crate::chunk::CHUNK_SIZE as i64, 1], 4u8)];
        assert_eq!(
            effects
                .into_iter()
                .filter_map(|effect| match effect {
                    Effect::SendFile { index, ranges, streams } => Some((index, ranges, streams)),
                    _ => None,
                })
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn the_receiver_opens_with_a_bare_handshake_frame() {
        // Go 的 `senderWaitForHandshake` 在读到 `handshake` 之前不处理任何消息，
        // 所以漏了这一步就是"两个方向都连不上"。
        let mut receiver = Session::new_receiver(&config(Role::Receiver, PASSPHRASE)).expect("构造");
        let mut rng = Counting::new(0);
        let effects = receiver.handle(InEvent::PeerConnected, &mut rng).expect("配对");
        assert_eq!(effects.first(), Some(&Effect::Send(HANDSHAKE.to_vec())));
        assert_eq!(HANDSHAKE, b"handshake", "Go 侧是 handshakeRequest = []byte(handshake)");
        // 第二条才是 `pake`——两者之间没有往返。
        assert!(matches!(effects.get(1), Some(Effect::Send(_))), "handshake 之后应紧跟 pake");
    }

    #[test]
    fn the_sender_swallows_the_bare_handshake_frame() {
        // 发送方收到 `handshake` 必须当无事发生。以前这里会把它当 JSON 去解压，
        // 报 `corrupt deflate stream`，于是 Go→Rust 的方向直接断在第一帧上。
        let mut sender = Session::new_sender(&config(Role::Sender, PASSPHRASE), manifest()).expect("构造");
        let mut rng = Counting::new(0);
        assert_eq!(sender.handle(InEvent::Frame(HANDSHAKE.to_vec()), &mut rng), Ok(Vec::new()));
        assert_eq!(sender.stage(), Stage::Idle, "吃掉 handshake 之后仍在等 pake");

        // 紧接着的 pake 必须还能正常处理。
        let mut receiver = Session::new_receiver(&config(Role::Receiver, PASSPHRASE)).expect("构造");
        let mut receiver_rng = Counting::new(0x80);
        let mut effects = receiver.handle(InEvent::PeerConnected, &mut receiver_rng).expect("配对");
        effects.remove(0);
        let pake = first_frame(effects);
        assert!(sender.handle(InEvent::Frame(pake), &mut rng).is_ok(), "handshake 之后应能正常处理 pake");
    }

    #[test]
    fn a_wrong_passphrase_fails_the_confirmation() {
        let mut sender = Session::new_sender(&config(Role::Sender, PASSPHRASE), manifest()).expect("构造");
        let mut receiver = Session::new_receiver(&config(Role::Receiver, b"wrong-passphrase")).expect("构造");
        let mut sender_rng = Counting::new(0x10);
        let mut receiver_rng = Counting::new(0x80);

        let mut effects = receiver.handle(InEvent::PeerConnected, &mut receiver_rng).expect("配对");
        // 第 0 条是裸帧 `handshake`，第 1 条才是 `pake`。
        assert_eq!(effects.first(), Some(&Effect::Send(HANDSHAKE.to_vec())));
        effects.remove(0);
        let pake = first_frame(effects);
        let reply = first_frame(sender.handle(InEvent::Frame(pake), &mut sender_rng).expect("发送方应能回 pake"));
        let confirm = first_frame(receiver.handle(InEvent::Frame(reply), &mut receiver_rng).expect("接收方应能算密钥"));
        assert_eq!(
            sender.handle(InEvent::Frame(confirm), &mut sender_rng),
            Err(SessionError::ConfirmationFailed),
            "口令不一致应报确认失败"
        );
    }

    #[test]
    fn a_peer_version_mismatch_is_rejected() {
        let mut receiver = Session::new_receiver(&config(Role::Receiver, PASSPHRASE)).expect("构造");
        let mut rng = Counting::new(0);
        let bogus = Message::new(MessageType::Pake)
            .with_version(1)
            .with_bytes(b"whatever".to_vec())
            .with_bytes2(b"siec".to_vec());
        let nonce = croc_protocol::random::nonce(&mut rng);
        let frame = SessionKey::Plain.seal(&bogus, &nonce).expect("应能封包");
        assert_eq!(
            receiver.handle(InEvent::Frame(frame), &mut rng),
            Err(SessionError::VersionMismatch { peer: 1, local: pakekey::PROTOCOL_VERSION })
        );
    }

    #[test]
    fn messages_out_of_order_are_rejected() {
        let mut sender = Session::new_sender(&config(Role::Sender, PASSPHRASE), manifest()).expect("构造");
        let mut rng = Counting::new(0);
        let key = [0x5a; croc_protocol::ENCRYPTION_KEY_LEN];
        let nonce = croc_protocol::random::nonce(&mut rng);

        // 一、明文阶段收到非 PAKE 消息：在**解包那一步**就被拒（Go 的
        // `"unencrypted communication rejected"`）。
        let ready = Message::new(MessageType::RecipientReady).with_bytes(Vec::new());
        let ciphertext = SessionKey::Aead(key).seal(&ready, &nonce).expect("应能封包");
        assert!(
            matches!(sender.handle(InEvent::Frame(ciphertext.clone()), &mut rng), Err(SessionError::Protocol(_))),
            "明文阶段必须拒收加密消息"
        );

        // 二、加密阶段收到"合法但时机不对"的消息：状态机拒它。
        let mut secured = Session::new_sender(&config(Role::Sender, PASSPHRASE), manifest()).expect("构造");
        secured.session = SessionKey::Aead(key);
        assert!(matches!(
            secured.handle(InEvent::Frame(ciphertext), &mut rng),
            Err(SessionError::UnexpectedMessage { .. })
        ));
    }

    #[test]
    fn declining_the_manifest_refuses_the_transfer() {
        let result = replay(PASSPHRASE, PASSPHRASE, Answer::Decline);
        assert_eq!(result.receiver, Some(Outcome::Declined));
        assert!(result.receiver_events.contains(&Event::PeerRefused));
        assert!(matches!(result.sender, Some(Outcome::Failed(_))), "发送方应看到对端拒绝");
    }

    #[test]
    fn cancelling_ends_the_session() {
        let mut receiver = Session::new_receiver(&config(Role::Receiver, PASSPHRASE)).expect("构造");
        let mut rng = Counting::new(0);
        let effects = receiver.handle(InEvent::Cancel, &mut rng).expect("取消应成功");
        assert_eq!(effects, vec![Effect::Finish(Outcome::Cancelled)]);
        assert_eq!(receiver.stage(), Stage::Done);
        assert_eq!(receiver.handle(InEvent::Cancel, &mut rng), Err(SessionError::AlreadyFinished));
    }

    #[test]
    fn configuration_is_validated_up_front() {
        let base = config(Role::Sender, PASSPHRASE);
        assert!(matches!(
            Session::new_sender(&SessionConfig { room: "", ..base.clone() }, manifest()),
            Err(SessionError::BadConfig(_))
        ));
        assert!(matches!(
            Session::new_sender(&SessionConfig { passphrase: b"", ..base.clone() }, manifest()),
            Err(SessionError::BadConfig(_))
        ));
        assert!(matches!(
            Session::new_sender(&SessionConfig { relay_ports: 0, ..base }, manifest()),
            Err(SessionError::BadConfig(_))
        ));
    }

    #[test]
    fn data_streams_are_capped_at_eight() {
        // Go: `min(len(RelayPorts), 8)`。
        let base = config(Role::Sender, PASSPHRASE);
        assert_eq!(SessionConfig { relay_ports: 1, ..base.clone() }.data_streams(), 1);
        assert_eq!(SessionConfig { relay_ports: 8, ..base }.data_streams(), 8);
        assert_eq!(SessionConfig { relay_ports: 12, ..config(Role::Sender, PASSPHRASE) }.data_streams(), 8);
    }

    #[test]
    fn an_oversized_manifest_fails_before_connecting() {
        // 清单在构造时就拆好，所以"太大"在连中继之前就暴露，不必传到一半才失败。
        let huge = SenderInfo {
            files_to_transfer: (0..(croc_protocol::manifest::MAX_MANIFEST_ENTRIES + 1))
                .map(|index| FileInfo { name: format!("f{index}"), size: 1, ..FileInfo::default() })
                .collect(),
            ..SenderInfo::default()
        };
        assert!(matches!(Session::new_sender(&config(Role::Sender, PASSPHRASE), huge), Err(SessionError::Manifest(_))));
    }

    #[test]
    fn the_debug_view_hides_the_passphrase() {
        let session = Session::new_sender(&config(Role::Sender, PASSPHRASE), manifest()).expect("构造");
        let rendered = format!("{session:?}");
        assert!(!rendered.contains("acid-acorn-acre"), "Debug 泄漏了口令");
        assert!(rendered.contains("Sender"));
    }

    #[test]
    fn stage_tracks_the_progress_of_the_session() {
        let mut receiver = Session::new_receiver(&config(Role::Receiver, PASSPHRASE)).expect("构造");
        let mut rng = Counting::new(0);
        assert_eq!(receiver.stage(), Stage::Idle);
        receiver.handle(InEvent::PeerConnected, &mut rng).expect("配对");
        assert_eq!(receiver.stage(), Stage::Securing);
        assert!(!receiver.is_secured());
    }

    /// 从效果表里取出第一条要发出去的帧。
    fn first_frame(effects: Vec<Effect>) -> Vec<u8> {
        effects
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Send(frame) => Some(frame),
                _ => None,
            })
            .expect("应有要发送的帧")
    }
}
