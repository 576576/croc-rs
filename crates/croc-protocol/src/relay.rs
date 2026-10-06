//! 中继（relay）握手：客户端与服务端共用的 sans-io 状态机。
//!
//! 对应 Go 的 `src/tcp`：客户端侧是 `HandshakeTCPServerCapability`，服务端侧是
//! `server.clientHandshake` + `server.clientCommunication`。
//!
//! # 为什么值得单独抽出来
//!
//! 这是 croc 里唯一"两端都跑我们自己的代码"的协议——中继服务端也是本项目实现的。
//! 把两端都做成纯状态机之后，**一次完整握手可以在单元测试里跑完**：把一方的 `send`
//! 原样喂给另一方，不碰 socket、不碰时钟。Go 版做不到这一点（IO 与状态转移缠在同一个
//! goroutine 里），这正是它 `tcp_test.go` 有近千行的原因。
//!
//! # 线格式
//!
//! 所有帧都是 [`crate::frame`] 的 comm 帧，**帧体是裸字节**，不是 `message.Message`
//! 的 JSON。只有第一帧会被中继"偷看"一次，用来识别 SSH 共享会话（Go 的
//! `observedRoomProtocol`）。
//!
//! ```text
//! 客户端                                    中继
//! ─────────────────────────────────────────────────────────────────────────
//! A = pake.InitCurve(weakKey, 0, "siec")  ──A.Bytes()──▶
//!                                          B = InitCurve(weakKey, 1, "siec")
//!                                          B.Update(A, β) ⇒ K
//!                                         ◀──B.Bytes()──
//! A.Update(B) ⇒ K
//! salt ←$ 8 字节                          ──salt──▶
//! key = PBKDF2(K, salt, 100, sha256)
//!                                         key = PBKDF2(K, salt, 100, sha256)
//!                                         ──Enc(password)──▶
//!                                         ──Enc(room)──▶     （流水线，不等 banner）
//!                                         ◀──Enc("ok|||<对端地址>")──
//!                                         ◀──Enc("ok")──     （房间准入）
//! ── 之后双向裸转发，中继不再解析 ──
//! ```
//!
//! # 明文探活
//!
//! 客户端可以发 `"ping"` 代替 PAKE 载荷，中继回 `"pong"` 后立即断开。这条路径不走
//! 加密、不占房间，纯粹用来测延迟（`croc --ping`）。
//!
//! # 与 Go 版的差异
//!
//! - **随机数注入**：`α`、`β`、盐、AEAD nonce 全部经 [`Randomness`] 由调用方提供，
//!   状态机本身是确定的。
//! - **不实现"快速准入"**：Go 的升级版中继会签发一个绑定源地址的 HMAC 令牌，让客户端
//!   在数据端口跳过 PAKE。这是纯优化，不影响线格式——本实现不签发，也不会主动请求。
//!   万一收到对端签发的 `capability`，会原样带在 [`ClientSession`] 里以便日后接上。
//! - **不做 banner 前的 5 ms "抢读"**：那是为了在 banner 之前拿到流水线过来的房间名。
//!   房间帧本来就已经在缓冲区里，按正常顺序读它不会多花一次往返。

use croc_pake::{CurveKind, EPHEMERAL_LEN, Identities, Initiator, PakeError, Responder};
use sha2::Sha256;
use zeroize::Zeroize;

use crate::ProtocolError;
use crate::codec::{self, AEAD_NONCE_LEN};
use crate::random::{self, Randomness};

/// 中继握手用的固定弱口令。
///
/// 它不是用来保护内容的——两端都硬编码 `[1,2,3]`，任何人都能算出这把 SPAKE2 密钥。
/// 它只负责把 `strongKey` 藏起来，让旁路观察者无法伪造 `Enc(password)`。真正的鉴权是
/// 后面那条中继密码。**不要"顺手"把它改强**：改了就不兼容，而且没有意义。
pub const WEAK_KEY: [u8; 3] = [1, 2, 3];

/// 中继握手固定使用的曲线（Go 里写死 `"siec"`）。
pub const RELAY_CURVE: CurveKind = CurveKind::Siec;

/// PBKDF2 的盐长度（Go 里 `make([]byte, 8)`）。
pub const SALT_LEN: usize = 8;

/// PBKDF2 的迭代次数，与 Go 的 `pbkdf2.Key(..., 100, ...)` 一致。
pub const PBKDF2_ITERATIONS: u32 = 100;

/// 中继会话密钥长度。
pub const RELAY_KEY_LEN: usize = 32;

/// 探活请求。
pub const PING: &[u8] = b"ping";

/// 探活响应。
pub const PONG: &[u8] = b"pong";

/// 中继在探活时使用的哨兵房间名。
pub const PING_ROOM: &str = "pinglkasjdlfjsaldjf";

/// banner 与对端地址之间的分隔符。
pub const SEPARATOR: &str = "|||";

/// 没有配置 banner 时中继回送的默认值。
pub const DEFAULT_BANNER: &str = "ok";

/// 房间准入通过。
pub const ADMISSION_OK: &str = "ok";

/// 房间准入被限流。
pub const ADMISSION_RATE_LIMITED: &str = "rate limited";

/// 房间已满。
pub const ADMISSION_ROOM_FULL: &str = "room full";

/// 中继密码错误时服务端回送的明文内容（加密后发出）。
pub const BAD_PASSWORD: &str = "bad password";

/// 中继握手的失败模式。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RelayError {
    /// 中继密码不匹配。
    #[error("bad password")]
    BadPassword,

    /// 房间已经被另一对连接占满。
    #[error("relay room full")]
    RoomFull,

    /// 中继对该来源地址或该房间限流。
    #[error("relay admission rate limited")]
    RateLimited,

    /// 中继回送的内容既不是 banner 也不是已知的拒绝原因。
    #[error("invalid relay response")]
    InvalidResponse,

    /// 状态机在某个状态下收到了不该出现的帧。
    #[error("unexpected frame while {0}")]
    UnexpectedFrame(&'static str),

    /// 收到 `ping` 以外的帧，但对方给的不是合法的 SPAKE2 载荷。
    #[error(transparent)]
    Pake(#[from] PakeError),

    /// AEAD 层失败（口令不对或帧被篡改）。
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// SPAKE2 还没派生出会话密钥就继续往下走了。
    #[error("relay handshake has no session key yet")]
    MissingStrongKey,

    /// 握手已经结束，又收到了帧。
    #[error("relay handshake is already complete")]
    AlreadyComplete,
}

/// 由 SPAKE2 会话密钥与盐派生出中继流量密钥。
///
/// Go：`pbkdf2.Key(strongKey, salt, 100, 32, sha256.New)`。迭代次数只有 100，因为
/// 输入 `strongKey` 已经是 256 位高熵值——PBKDF2 在这里只是当 KDF 用，不是抗暴力破解。
///
/// # Errors
///
/// `strong_key` 为空时返回 [`RelayError::MissingStrongKey`]。
pub fn derive_key(strong_key: &[u8], salt: &[u8]) -> Result<[u8; RELAY_KEY_LEN], RelayError> {
    if strong_key.is_empty() {
        return Err(RelayError::MissingStrongKey);
    }
    let mut key = [0u8; RELAY_KEY_LEN];
    pbkdf2::pbkdf2_hmac::<Sha256>(strong_key, salt, PBKDF2_ITERATIONS, &mut key);
    Ok(key)
}

/// 中继会话密钥。`Debug` 与 `Drop` 都不放过它。
#[derive(Clone, PartialEq, Eq)]
pub struct RelayKey([u8; RELAY_KEY_LEN]);

impl RelayKey {
    /// 从派生结果构造。
    #[must_use]
    pub const fn new(bytes: [u8; RELAY_KEY_LEN]) -> Self {
        Self(bytes)
    }

    /// 底层字节。
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; RELAY_KEY_LEN] {
        &self.0
    }

    /// 加密一帧。
    fn seal(&self, plaintext: &[u8], nonce: &[u8; AEAD_NONCE_LEN]) -> Result<Vec<u8>, RelayError> {
        Ok(codec::encrypt(plaintext, &self.0, nonce)?)
    }

    /// 解密一帧。
    fn open(&self, payload: &[u8]) -> Result<Vec<u8>, RelayError> {
        Ok(codec::decrypt(payload, &self.0)?)
    }
}

impl core::fmt::Debug for RelayKey {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter.write_str("RelayKey(<redacted>)")
    }
}

impl Drop for RelayKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// 房间准入的判定结果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// 放行。
    Allowed,
    /// 来源地址或房间触发了限流。
    RateLimited,
    /// 房间里已经有一对连接了。
    RoomFull,
}

impl Admission {
    /// 回送给客户端的明文。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Allowed => ADMISSION_OK,
            Self::RateLimited => ADMISSION_RATE_LIMITED,
            Self::RoomFull => ADMISSION_ROOM_FULL,
        }
    }
}

// ───────────────────────────────── 客户端 ─────────────────────────────────

/// 客户端握手成功后拿到的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientSession {
    /// 中继 banner。
    pub banner: String,
    /// 中继看到的本端地址（`host:port`）。
    pub ip_address: String,
    /// 升级版中继签发的不透明准入令牌。本实现不使用，但保留以便日后接上。
    pub capability: Option<String>,
    /// 中继会话密钥。裸转发用不到它，但重连与排障时有用。
    pub key: RelayKey,
}

/// 一次 [`ClientHandshake::step`] 的产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStep {
    /// 紧接着要发出去的帧。
    ///
    /// 通常为空——只有"收到 `B` 的那一步"会一次发三帧（盐、加密口令、加密房间名）。
    pub send: Vec<Vec<u8>>,
    /// 握手完成时的会话信息；`None` 表示还要继续等下一帧。
    pub session: Option<ClientSession>,
}

impl ClientStep {
    fn idle() -> Self {
        Self { send: Vec::new(), session: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientState {
    /// 还没发 `A`。
    Fresh,
    /// 已发 `A`，等 `B`。
    AwaitingResponse,
    /// 已发盐 + 口令 + 房间名，等 banner。
    AwaitingBanner,
    /// 已拿到 banner，等房间准入结果。
    AwaitingAdmission,
    /// 完成。
    Done,
}

/// 中继握手的客户端状态机。
pub struct ClientHandshake {
    exchange: Initiator,
    salt: [u8; SALT_LEN],
    password: Vec<u8>,
    room: Vec<u8>,
    /// banner 阶段解析出来、等准入阶段才凑齐的会话信息。
    pending: Option<ClientSession>,
    state: ClientState,
}

impl ClientHandshake {
    /// 构造。`password` 是**中继密码**（`--relay-password`），不是房间口令。
    ///
    /// 这里会立刻用掉 32 + 8 字节随机数（SPAKE2 的 `α` 与中继盐）。
    ///
    /// # Errors
    ///
    /// SPAKE2 初始化失败时返回 [`RelayError::Pake`]。
    pub fn new(password: &[u8], room: &str, randomness: &mut impl Randomness) -> Result<Self, RelayError> {
        let ephemeral: [u8; EPHEMERAL_LEN] = random::array(randomness);
        let exchange = Initiator::start(&WEAK_KEY, RELAY_CURVE, Identities::Unbound, ephemeral)?;
        Ok(Self {
            exchange,
            salt: random::array(randomness),
            password: password.to_vec(),
            room: room.as_bytes().to_vec(),
            pending: None,
            state: ClientState::Fresh,
        })
    }

    /// 握手的第一步：要发出去的 PAKE 发起方载荷。
    ///
    /// # Errors
    ///
    /// 重复调用返回 [`RelayError::UnexpectedFrame`]。
    pub fn start(&mut self) -> Result<Vec<u8>, RelayError> {
        if self.state != ClientState::Fresh {
            return Err(RelayError::UnexpectedFrame("starting the relay handshake"));
        }
        self.state = ClientState::AwaitingResponse;
        Ok(self.exchange.public_bytes().into_bytes())
    }

    /// 处理一帧。
    ///
    /// # Errors
    ///
    /// 口令错误、被限流、房间已满、响应格式不对，或者状态不对，都会返回 [`RelayError`]。
    pub fn step(&mut self, frame: &[u8], randomness: &mut impl Randomness) -> Result<ClientStep, RelayError> {
        match self.state {
            ClientState::Fresh => Err(RelayError::UnexpectedFrame("before sending the PAKE payload")),
            ClientState::AwaitingResponse => self.accept_responder(frame, randomness),
            ClientState::AwaitingBanner => self.accept_banner(frame),
            ClientState::AwaitingAdmission => self.accept_admission(frame),
            ClientState::Done => Err(RelayError::AlreadyComplete),
        }
    }

    /// 收到 `B`：算出会话密钥，一次性发出盐、加密口令、加密房间名。
    ///
    /// Go 把房间名和口令一起流水线发出，不等 banner——两者用的是同一把密钥，中继在
    /// 口令校验通过后就能读到已经缓冲好的房间帧。这样少一次往返。
    fn accept_responder(&mut self, frame: &[u8], randomness: &mut impl Randomness) -> Result<ClientStep, RelayError> {
        self.exchange.accept_response(frame)?;
        let key = self.session_key()?;

        let password = key.seal(&self.password, &random::nonce(randomness))?;
        let room = key.seal(&self.room, &random::nonce(randomness))?;
        self.state = ClientState::AwaitingBanner;

        Ok(ClientStep { send: vec![self.salt.to_vec(), password, room], session: None })
    }

    /// 收到 banner：`"<banner>|||<对端地址>[|||<capability>]"`。
    fn accept_banner(&mut self, frame: &[u8]) -> Result<ClientStep, RelayError> {
        let key = self.session_key()?;
        let data = key.open(frame)?;
        let text = String::from_utf8_lossy(&data).into_owned();

        // Go 先看有没有分隔符；没有的话要么是 "bad password"，要么是格式错误。
        let parts = text.split(SEPARATOR).collect::<Vec<_>>();
        let (Some(banner), Some(ip_address)) = (parts.first(), parts.get(1)) else {
            return if text.trim() == BAD_PASSWORD {
                Err(RelayError::BadPassword)
            } else {
                Err(RelayError::InvalidResponse)
            };
        };

        self.pending = Some(ClientSession {
            banner: (*banner).to_owned(),
            ip_address: (*ip_address).to_owned(),
            capability: parts.get(2).map(|value| (*value).to_owned()),
            key,
        });
        self.state = ClientState::AwaitingAdmission;
        Ok(ClientStep::idle())
    }

    /// 收到房间准入结果。
    fn accept_admission(&mut self, frame: &[u8]) -> Result<ClientStep, RelayError> {
        let key = self.session_key()?;
        let data = key.open(frame)?;
        match data.as_slice() {
            ADMISSION_OK_BYTES => {
                let session = self.pending.take().ok_or(RelayError::InvalidResponse)?;
                self.state = ClientState::Done;
                Ok(ClientStep { send: Vec::new(), session: Some(session) })
            }
            ADMISSION_RATE_LIMITED_BYTES => Err(RelayError::RateLimited),
            ADMISSION_ROOM_FULL_BYTES => Err(RelayError::RoomFull),
            _ => Err(RelayError::InvalidResponse),
        }
    }

    /// 从 SPAKE2 会话密钥重新派生出中继密钥。
    ///
    /// 重算而不是存字段，是为了让 `ClientSession` 的构造点只有一处——密钥不会在多个
    /// 地方各存一份、各自被忘记擦除。
    fn session_key(&self) -> Result<RelayKey, RelayError> {
        let strong_key = self.exchange.session_key().ok_or(RelayError::MissingStrongKey)?;
        Ok(RelayKey::new(derive_key(&strong_key, &self.salt)?))
    }
}

impl core::fmt::Debug for ClientHandshake {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ClientHandshake")
            .field("state", &self.state)
            .field("room", &"<redacted>")
            .field("password", &"<redacted>")
            .finish()
    }
}

impl Drop for ClientHandshake {
    fn drop(&mut self) {
        self.password.zeroize();
        self.room.zeroize();
    }
}

const ADMISSION_OK_BYTES: &[u8] = ADMISSION_OK.as_bytes();
const ADMISSION_RATE_LIMITED_BYTES: &[u8] = ADMISSION_RATE_LIMITED.as_bytes();
const ADMISSION_ROOM_FULL_BYTES: &[u8] = ADMISSION_ROOM_FULL.as_bytes();

// ───────────────────────────────── 服务端 ─────────────────────────────────

/// 服务端握手成功后拿到的东西。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSession {
    /// 客户端请求加入的房间名。
    pub room: String,
    /// 中继会话密钥，用来加密房间准入结果。
    pub key: RelayKey,
}

impl ServerSession {
    /// 加密一条房间准入结果，返回要回送的帧。
    ///
    /// # Errors
    ///
    /// AEAD 失败时返回 [`RelayError::Protocol`]。
    pub fn admit(&self, outcome: Admission, randomness: &mut impl Randomness) -> Result<Vec<u8>, RelayError> {
        self.key.seal(outcome.as_str().as_bytes(), &random::nonce(randomness))
    }
}

/// 一次 [`ServerHandshake::step`] 的产出。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerStep {
    /// 先发这些帧。
    pub send: Vec<Vec<u8>>,
    /// 然后进入这个终态；`None` 表示继续等下一帧。
    pub outcome: Option<ServerOutcome>,
}

impl ServerStep {
    fn idle() -> Self {
        Self { send: Vec::new(), outcome: None }
    }

    fn send_only(frame: Vec<u8>) -> Self {
        Self { send: vec![frame], outcome: None }
    }
}

/// 服务端握手的终态。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerOutcome {
    /// 握手完成，可以进入房间准入。
    Accepted(ServerSession),
    /// 对方只是探活：发完 `pong` 就可以断开。
    Ping,
    /// 拒绝。**注意 [`ServerStep::send`] 里的帧要照发**——口令错误时中继会回一条加密的
    /// `"bad password"`，让客户端能区分"口令错"和"网络坏"。
    Rejected(RelayError),
}

#[derive(Debug)]
enum ServerState {
    /// 等第一帧（PAKE 载荷或 `ping`）。
    AwaitingInitiator,
    /// 已回 `B`，等盐。
    AwaitingSalt { responder: Box<Responder> },
    /// 已有密钥，等加密口令。
    AwaitingPassword { key: RelayKey },
    /// 已回 banner，等加密房间名。
    AwaitingRoom { key: RelayKey },
    /// 完成。
    Done,
}

/// 中继握手的服务端状态机。
pub struct ServerHandshake {
    password: String,
    banner: String,
    remote: String,
    state: ServerState,
}

impl ServerHandshake {
    /// 构造。`banner` 为空时回送 [`DEFAULT_BANNER`]；`remote` 是中继看到的对端地址。
    #[must_use]
    pub fn new(password: impl Into<String>, banner: impl Into<String>, remote: impl Into<String>) -> Self {
        let banner = banner.into();
        Self {
            password: password.into(),
            banner: if banner.is_empty() { DEFAULT_BANNER.to_owned() } else { banner },
            remote: remote.into(),
            state: ServerState::AwaitingInitiator,
        }
    }

    /// 处理一帧。
    ///
    /// # Errors
    ///
    /// 只有在状态机自身不可用时才返回 `Err`；**对端的错误（口令错、帧损坏）走
    /// [`ServerOutcome::Rejected`]**，因为那需要先把拒绝帧发出去。
    pub fn step(&mut self, frame: &[u8], randomness: &mut impl Randomness) -> Result<ServerStep, RelayError> {
        match core::mem::replace(&mut self.state, ServerState::Done) {
            ServerState::AwaitingInitiator => self.accept_initiator(frame, randomness),
            ServerState::AwaitingSalt { responder } => self.accept_salt(frame, *responder),
            ServerState::AwaitingPassword { key } => self.accept_password(frame, key, randomness),
            ServerState::AwaitingRoom { key } => Ok(self.accept_room(frame, key)),
            ServerState::Done => Err(RelayError::AlreadyComplete),
        }
    }

    /// 第一帧：`ping` 走探活，其余当作 PAKE 载荷。
    fn accept_initiator(&mut self, frame: &[u8], randomness: &mut impl Randomness) -> Result<ServerStep, RelayError> {
        if frame == PING {
            self.state = ServerState::Done;
            return Ok(ServerStep { send: vec![PONG.to_vec()], outcome: Some(ServerOutcome::Ping) });
        }

        // `InitCurve(weakKey, 1, "siec")` 本身不抽随机数，`Update` 才抽 `β`。
        let mut responder = Responder::start(&WEAK_KEY, RELAY_CURVE, Identities::Unbound)?;
        match responder.accept_request(frame, random::array(randomness)) {
            Ok(()) => {}
            Err(error) => {
                // 载荷不合法：没有可回送的内容，直接拒绝。
                self.state = ServerState::Done;
                return Ok(ServerStep { send: Vec::new(), outcome: Some(ServerOutcome::Rejected(error.into())) });
            }
        }
        let response = responder.public_bytes().into_bytes();
        self.state = ServerState::AwaitingSalt { responder: Box::new(responder) };
        Ok(ServerStep::send_only(response))
    }

    /// 盐：派生中继密钥。
    fn accept_salt(&mut self, frame: &[u8], responder: Responder) -> Result<ServerStep, RelayError> {
        let strong_key = responder.session_key().ok_or(RelayError::MissingStrongKey)?;
        let key = RelayKey::new(derive_key(&strong_key, frame)?);
        self.state = ServerState::AwaitingPassword { key };
        Ok(ServerStep::idle())
    }

    /// 加密口令：比对成功后回 banner。
    fn accept_password(
        &mut self,
        frame: &[u8],
        key: RelayKey,
        randomness: &mut impl Randomness,
    ) -> Result<ServerStep, RelayError> {
        let received = match key.open(frame) {
            Ok(plaintext) => plaintext,
            Err(error) => {
                // 连口令帧都解不开——密钥对不上，对方大概率不是 croc 客户端。
                self.state = ServerState::Done;
                return Ok(ServerStep { send: Vec::new(), outcome: Some(ServerOutcome::Rejected(error)) });
            }
        };

        if !password_matches(&received, &self.password) {
            let rejection = key.seal(BAD_PASSWORD.as_bytes(), &random::nonce(randomness))?;
            self.state = ServerState::Done;
            return Ok(ServerStep {
                send: vec![rejection],
                outcome: Some(ServerOutcome::Rejected(RelayError::BadPassword)),
            });
        }

        let response = format!("{}{SEPARATOR}{}", self.banner, self.remote);
        let banner = key.seal(response.as_bytes(), &random::nonce(randomness))?;
        self.state = ServerState::AwaitingRoom { key };
        Ok(ServerStep::send_only(banner))
    }

    /// 加密房间名：握手完成。
    fn accept_room(&mut self, frame: &[u8], key: RelayKey) -> ServerStep {
        let room = key.open(frame).map(|plaintext| String::from_utf8_lossy(&plaintext).into_owned());
        match room {
            Ok(room) => {
                self.state = ServerState::Done;
                ServerStep { send: Vec::new(), outcome: Some(ServerOutcome::Accepted(ServerSession { room, key })) }
            }
            Err(error) => {
                self.state = ServerState::Done;
                ServerStep { send: Vec::new(), outcome: Some(ServerOutcome::Rejected(error)) }
            }
        }
    }
}

impl core::fmt::Debug for ServerHandshake {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("ServerHandshake")
            .field("state", &self.state)
            .field("banner", &self.banner)
            .field("remote", &self.remote)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl Drop for ServerHandshake {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}

/// Go：`strings.TrimSpace(recv) != strings.TrimSpace(configured)`。
///
/// 两侧都 trim，所以 `echo "secret" | croc ...` 这种带换行的用法不会意外失败。
fn password_matches(received: &[u8], expected: &str) -> bool {
    String::from_utf8_lossy(received).trim() == expected.trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// 确定性随机源：从 `start` 起逐字节递增。**仅测试用。**
    struct Counting(u8);

    impl Counting {
        fn new(start: u8) -> Self {
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

    const PASSWORD: &str = "relay-secret";
    const ROOM: &str = "1234-adjective-animal";
    const BANNER: &str = "welcome to croc";
    const REMOTE: &str = "203.0.113.7:40404";

    /// 在进程内跑完一次握手，把一方的 `send` 原样喂给另一方。
    ///
    /// 这正是"两端都是纯状态机"换来的东西：不需要 socket、不需要超时、不需要重试，
    /// 结果完全确定。
    fn run_handshake(
        password: &str,
        room: &str,
        banner: &str,
        admission: Admission,
    ) -> Result<(ClientSession, ServerSession), RelayError> {
        let mut client_randomness = Counting::new(0x10);
        let mut server_randomness = Counting::new(0x80);

        let mut client = ClientHandshake::new(password.as_bytes(), room, &mut client_randomness)?;
        let mut server = ServerHandshake::new(password, banner, REMOTE);

        let mut to_server = VecDeque::new();
        let mut to_client = VecDeque::new();
        to_server.push_back(client.start()?);

        let mut client_session = None;
        let mut server_session = None;
        let mut rounds = 0usize;

        while client_session.is_none() {
            rounds += 1;
            assert!(rounds < 16, "握手没能在 16 轮内收敛，状态机可能卡住了");

            while let Some(frame) = to_server.pop_front() {
                let step = server.step(&frame, &mut server_randomness)?;
                to_client.extend(step.send);
                match step.outcome {
                    None => {}
                    Some(ServerOutcome::Accepted(session)) => {
                        to_client.push_back(session.admit(admission, &mut server_randomness)?);
                        server_session = Some(session);
                    }
                    Some(ServerOutcome::Ping) => panic!("测试路径不该走到探活"),
                    Some(ServerOutcome::Rejected(error)) => return Err(error),
                }
            }

            while let Some(frame) = to_client.pop_front() {
                let step = client.step(&frame, &mut client_randomness)?;
                to_server.extend(step.send);
                if step.session.is_some() {
                    client_session = step.session;
                }
            }
        }

        let client_session = client_session.expect("客户端应有会话");
        let server_session = server_session.expect("服务端应有会话");
        Ok((client_session, server_session))
    }

    #[test]
    fn handshake_completes_and_both_sides_agree_on_the_room() {
        let (client, server) = run_handshake(PASSWORD, ROOM, BANNER, Admission::Allowed).expect("握手应成功");
        assert_eq!(server.room, ROOM);
        assert_eq!(client.banner, BANNER);
        assert_eq!(client.ip_address, REMOTE);
        assert_eq!(client.capability, None);
        assert_eq!(client.key, server.key, "两端派生的中继密钥应一致");
    }

    #[test]
    fn default_banner_is_used_when_none_is_configured() {
        let (client, _) = run_handshake(PASSWORD, ROOM, "", Admission::Allowed).expect("握手应成功");
        assert_eq!(client.banner, DEFAULT_BANNER);
    }

    #[test]
    fn the_session_key_is_not_the_raw_spake2_key() {
        // 中继密钥是 PBKDF2(strongKey, salt) 的结果，必须与 SPAKE2 输出不同。
        let strong_key = [0x5au8; 32];
        let salt = [0x11u8; SALT_LEN];
        let derived = derive_key(&strong_key, &salt).expect("应能派生");
        assert_ne!(derived.as_slice(), strong_key.as_slice());
        assert_eq!(derived.len(), RELAY_KEY_LEN);
    }

    #[test]
    fn derivation_is_deterministic_and_salt_dependent() {
        let strong_key = [0x5au8; 32];
        assert_eq!(
            derive_key(&strong_key, &[0x11; SALT_LEN]).expect("应能派生"),
            derive_key(&strong_key, &[0x11; SALT_LEN]).expect("应能派生")
        );
        assert_ne!(
            derive_key(&strong_key, &[0x11; SALT_LEN]).expect("应能派生"),
            derive_key(&strong_key, &[0x12; SALT_LEN]).expect("应能派生")
        );
    }

    #[test]
    fn empty_strong_key_is_rejected() {
        assert_eq!(derive_key(&[], &[0u8; SALT_LEN]), Err(RelayError::MissingStrongKey));
    }

    #[test]
    fn a_wrong_relay_password_is_reported_as_bad_password() {
        let mut client_randomness = Counting::new(0x10);
        let mut server_randomness = Counting::new(0x80);
        let mut client = ClientHandshake::new(b"wrong", ROOM, &mut client_randomness).expect("构造");
        let mut server = ServerHandshake::new(PASSWORD, BANNER, REMOTE);

        let first = client.start().expect("start");
        let response = server.step(&first, &mut server_randomness).expect("step").send;
        let step = client.step(&response[0], &mut client_randomness).expect("step");
        let mut to_server = VecDeque::from(step.send);

        // 服务端先读盐、再读口令；口令那一步会产出加密的 "bad password" 与拒绝终态。
        let salt = to_server.pop_front().expect("盐");
        assert!(server.step(&salt, &mut server_randomness).expect("step").send.is_empty());

        let password_frame = to_server.pop_front().expect("口令");
        let step = server.step(&password_frame, &mut server_randomness).expect("step");
        assert_eq!(step.send.len(), 1, "应回送一条加密的拒绝原因");
        assert_eq!(step.outcome, Some(ServerOutcome::Rejected(RelayError::BadPassword)));
    }

    #[test]
    fn the_client_surfaces_the_relayed_bad_password() {
        // 手工把服务端的拒绝帧喂给客户端，验证客户端也报 BadPassword 而不是 InvalidResponse。
        let mut client_randomness = Counting::new(0x10);
        let mut server_randomness = Counting::new(0x80);
        let mut client = ClientHandshake::new(b"wrong", ROOM, &mut client_randomness).expect("构造");
        let mut server = ServerHandshake::new(PASSWORD, BANNER, REMOTE);

        let request = client.start().expect("start");
        let response = server.step(&request, &mut server_randomness).expect("step").send;
        let step = client.step(&response[0], &mut client_randomness).expect("step");
        let mut to_server = VecDeque::from(step.send);

        let salt = to_server.pop_front().expect("盐");
        assert!(server.step(&salt, &mut server_randomness).expect("step").send.is_empty());
        let password_frame = to_server.pop_front().expect("口令");
        let rejection = server.step(&password_frame, &mut server_randomness).expect("step").send;

        assert_eq!(client.step(&rejection[0], &mut client_randomness), Err(RelayError::BadPassword));
    }

    #[test]
    fn rate_limiting_and_full_rooms_are_distinguished() {
        // 这两种结果都发生在握手成功之后，靠准入帧区分。
        assert_eq!(
            run_handshake(PASSWORD, ROOM, BANNER, Admission::RateLimited).map(|_| ()),
            Err(RelayError::RateLimited)
        );
        assert_eq!(run_handshake(PASSWORD, ROOM, BANNER, Admission::RoomFull).map(|_| ()), Err(RelayError::RoomFull));
    }

    #[test]
    fn ping_is_answered_with_pong_and_never_creates_a_room() {
        let mut server = ServerHandshake::new(PASSWORD, BANNER, REMOTE);
        let mut randomness = Counting::new(0);
        let step = server.step(PING, &mut randomness).expect("应能处理探活");
        assert_eq!(step.send, vec![PONG.to_vec()]);
        assert_eq!(step.outcome, Some(ServerOutcome::Ping));

        // 探活之后状态机就结束了。
        assert_eq!(server.step(PING, &mut randomness), Err(RelayError::AlreadyComplete));
    }

    #[test]
    fn the_password_is_compared_after_trimming_both_sides() {
        assert!(password_matches(b"  relay-secret\n", PASSWORD));
        assert!(password_matches(PASSWORD.as_bytes(), " relay-secret "));
        assert!(!password_matches(b"relay-secret-x", PASSWORD));
        assert!(!password_matches(b"", PASSWORD));
    }

    #[test]
    fn a_garbage_first_frame_is_rejected_without_a_response() {
        let mut server = ServerHandshake::new(PASSWORD, BANNER, REMOTE);
        let mut randomness = Counting::new(0);
        let step = server.step(b"this is not a pake payload", &mut randomness).expect("应能处理");
        assert!(step.send.is_empty(), "非法载荷不该得到任何回送");
        assert!(matches!(step.outcome, Some(ServerOutcome::Rejected(_))));
    }

    #[test]
    fn the_client_refuses_to_step_before_start() {
        let mut randomness = Counting::new(0);
        let mut client = ClientHandshake::new(PASSWORD.as_bytes(), ROOM, &mut randomness).expect("构造");
        assert_eq!(
            client.step(b"anything", &mut randomness),
            Err(RelayError::UnexpectedFrame("before sending the PAKE payload"))
        );
        // `start` 只能调一次。
        client.start().expect("start");
        assert_eq!(client.start(), Err(RelayError::UnexpectedFrame("starting the relay handshake")));
    }

    #[test]
    fn the_client_pipelines_three_frames_after_the_responder_value() {
        // 盐、加密口令、加密房间名必须一次发出去，这是少一次往返的关键。
        let mut randomness = Counting::new(0x10);
        let mut client = ClientHandshake::new(PASSWORD.as_bytes(), ROOM, &mut randomness).expect("构造");
        let mut server_randomness = Counting::new(0x80);
        let mut server = ServerHandshake::new(PASSWORD, BANNER, REMOTE);

        let request = client.start().expect("start");
        let response = server.step(&request, &mut server_randomness).expect("step").send;
        let step = client.step(&response[0], &mut randomness).expect("step");

        assert_eq!(step.send.len(), 3, "应一次发出盐 + 口令 + 房间名");
        assert_eq!(step.send[0].len(), SALT_LEN, "第一帧应是裸盐");
        assert!(step.session.is_none());
    }

    #[test]
    fn debug_output_hides_the_password_and_room() {
        let mut randomness = Counting::new(0);
        let client = ClientHandshake::new(PASSWORD.as_bytes(), ROOM, &mut randomness).expect("构造");
        let rendered = format!("{client:?}");
        assert!(!rendered.contains(PASSWORD), "Debug 泄漏了中继密码");
        assert!(!rendered.contains(ROOM), "Debug 泄漏了房间名");

        let server = ServerHandshake::new(PASSWORD, BANNER, REMOTE);
        let rendered = format!("{server:?}");
        assert!(!rendered.contains(PASSWORD), "Debug 泄漏了中继密码");
        assert!(rendered.contains(BANNER), "banner 不是秘密，应该能看到");

        let key = RelayKey::new([0x5a; RELAY_KEY_LEN]);
        assert_eq!(format!("{key:?}"), "RelayKey(<redacted>)");
    }

    #[test]
    fn different_randomness_produces_different_wire_bytes() {
        // 同一个口令、同一个房间，随机数不同 ⇒ 线上字节必须不同（否则 α 被复用了）。
        let mut first_randomness = Counting::new(0x10);
        let mut second_randomness = Counting::new(0x20);
        let mut first = ClientHandshake::new(PASSWORD.as_bytes(), ROOM, &mut first_randomness).expect("构造");
        let mut second = ClientHandshake::new(PASSWORD.as_bytes(), ROOM, &mut second_randomness).expect("构造");
        assert_ne!(first.start().expect("start"), second.start().expect("start"));
    }

    #[test]
    fn the_handshake_is_deterministic_for_fixed_randomness() {
        // 确定性重放：同样的随机数 ⇒ 同样的会话密钥。出错时能精确复现。
        let first = run_handshake(PASSWORD, ROOM, BANNER, Admission::Allowed).expect("应成功");
        let second = run_handshake(PASSWORD, ROOM, BANNER, Admission::Allowed).expect("应成功");
        assert_eq!(first.0.key, second.0.key);
    }

    #[test]
    fn admission_strings_match_the_go_constants() {
        assert_eq!(Admission::Allowed.as_str(), "ok");
        assert_eq!(Admission::RateLimited.as_str(), "rate limited");
        assert_eq!(Admission::RoomFull.as_str(), "room full");
        assert_eq!(PING, b"ping");
        assert_eq!(PONG, b"pong");
        assert_eq!(SEPARATOR, "|||");
        assert_eq!(WEAK_KEY, [1, 2, 3]);
        assert_eq!(PING_ROOM, "pinglkasjdlfjsaldjf");
        assert_eq!(RELAY_CURVE, CurveKind::Siec);
        assert_eq!(PBKDF2_ITERATIONS, 100);
        assert_eq!(SALT_LEN, 8);
    }
}
