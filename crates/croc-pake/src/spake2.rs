//! SPAKE2 状态机：`schollz/pake` v3 的 `Pake` 结构。
//!
//! 协议（Boneh–Shoup《A Graduate Course in Applied Cryptography》Figure 21.15）：
//!
//! ```text
//! A 方（role 0，croc 里的接收方）           B 方（role 1，croc 里的发送方）
//! ────────────────────────────────────────────────────────────────────────
//! α ←$ Z, X = U·pw + α·G          ──X──▶
//!                                          β ←$ Z, Y = V·pw + β·G
//!                                          Z = (X − U·pw)·β = αβ·G
//!                                 ◀──Y──
//! Z = (Y − V·pw)·α = αβ·G
//! k = SHA256(domain ‖ pw ‖ idA ‖ idB ‖ curve ‖ X ‖ Y ‖ Z)   （`Identities::Bound`）
//! k = SHA256(pw ‖ X ‖ Y ‖ Z)                                 （`Identities::Unbound`）
//! ```
//!
//! # 与 Go 版的结构差异
//!
//! Go 用一个 `Pake` 结构 + `Role int` 字段，`Init`/`Update` 靠运行期分支。这里拆成
//! [`Initiator`] 与 [`Responder`] 两个类型——**调错方向在编译期就过不去**，`Update`
//! 里那两个大分支也就不用写运行期 role 判断。
//!
//! 另一处差异是随机数：Go 在 `Init`/`Update` 内部 `rand.Read`。这里把 32 字节临时
//! 标量 `α`/`β` 作为参数传入，让核心保持 sans-io 纯函数——调用方（`croc-engine`）
//! 负责提供随机源，测试里可以喂固定值做确定性回归。

use croc_siec::AffinePoint;
use croc_siec::bigint::go_bytes;
use sha2::{Digest, Sha256};
use zeroize::Zeroize;

use crate::curve::{Curve, CurveKind};
use crate::error::PakeError;
use crate::wire::{self, PublicState};

/// Go `pake` 里 `identityTranscriptDomain` 的值。
///
/// 这个字符串参与会话密钥的转录哈希，改一个字节就会让 Rust 版和 Go 版派生出不同的
/// 密钥，所以它属于线路契约。
const IDENTITY_TRANSCRIPT_DOMAIN: &[u8] = b"github.com/schollz/pake/v3/identity-bound-session-key/v1";

/// 临时标量的长度（Go 里是 `make([]byte, 32)`）。
pub const EPHEMERAL_LEN: usize = 32;

/// 会话密钥的转录模式——对应 Go 里那个 `withIDs` 布尔值。
///
/// `schollz/pake` v3 有**两条**派生路径，哈希输入的结构完全不同：一条带 domain 与
/// u64-LE 长度前缀，另一条什么都不带。这是线路契约，不能统一，所以这里把"走哪条"
/// 提升成一个显式参数，而不是藏在两个构造函数重载里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identities<'a> {
    /// Go 的 `InitCurve`：`sha256(pw ‖ Xᵤ ‖ Xᵥ ‖ Yᵤ ‖ Yᵥ ‖ Zᵤ ‖ Zᵥ)`，**没有长度前缀**。
    ///
    /// croc 用在中继握手：双方共用编译期固定的弱口令 `[1,2,3]`，没有身份可绑。
    Unbound,
    /// Go 的 `InitCurveWithIdentities`：带 domain 与逐字段 u64-LE 长度前缀。
    ///
    /// croc 用在 peer PAKE，把房间名/用途/角色绑进密钥。
    Bound {
        /// 发起方（A，croc 里的接收方）的身份串。
        initiator: &'a [u8],
        /// 响应方（B，croc 里的发送方）的身份串。
        responder: &'a [u8],
    },
}

impl<'a> Identities<'a> {
    /// 绑定双方身份的便捷构造。
    #[must_use]
    pub const fn bound(initiator: &'a [u8], responder: &'a [u8]) -> Self {
        Self::Bound { initiator, responder }
    }

    /// 转成内部表示，顺带做 Go 版的非空校验（只对 `Bound` 有意义）。
    fn to_owned(self) -> Result<Option<BoundIdentities>, PakeError> {
        match self {
            Self::Unbound => Ok(None),
            Self::Bound { initiator, responder } => {
                if initiator.is_empty() || responder.is_empty() {
                    return Err(PakeError::MissingIdentity);
                }
                Ok(Some(BoundIdentities { initiator: initiator.to_vec(), responder: responder.to_vec() }))
            }
        }
    }
}

/// 已校验过的身份对。字段名与 [`Identities::Bound`] 一致，避免两处命名漂移。
#[derive(Debug, Clone)]
struct BoundIdentities {
    initiator: Vec<u8>,
    responder: Vec<u8>,
}

/// 两个角色共用的状态与派生逻辑。
struct Exchange {
    curve: Curve,
    role: u8,
    password: Vec<u8>,
    /// `None` 表示走 Go 的 `withIDs = false` 分支。
    identities: Option<BoundIdentities>,
    /// `V·pw`
    vpw: AffinePoint,
    /// `U·pw`
    upw: AffinePoint,
    /// 本端的临时标量 `α`（A 方）或 `β`（B 方）。
    ephemeral: [u8; EPHEMERAL_LEN],
    x: Option<AffinePoint>,
    y: Option<AffinePoint>,
    z: Option<AffinePoint>,
    key: Option<[u8; EPHEMERAL_LEN]>,
}

impl Exchange {
    fn new(password: &[u8], curve: CurveKind, identities: Identities<'_>, role: u8) -> Result<Self, PakeError> {
        Ok(Self {
            curve: Curve::new(curve),
            role,
            password: password.to_vec(),
            identities: identities.to_owned()?,
            vpw: AffinePoint::identity(),
            upw: AffinePoint::identity(),
            ephemeral: [0u8; EPHEMERAL_LEN],
            x: None,
            y: None,
            z: None,
            key: None,
        })
    }

    fn public_state(&self) -> PublicState {
        PublicState {
            role: self.role,
            u: self.curve.u().clone(),
            v: self.curve.v().clone(),
            x: self.x.clone(),
            y: self.y.clone(),
        }
    }

    fn public_bytes(&self) -> String {
        self.public_state().encode()
    }

    /// Go `(*Pake).deriveSessionKey()`。
    ///
    /// 两个分支的差别只有哈希输入的拼法，见 [`Identities`] 的文档。共同点是都用
    /// `big.Int.Bytes()`（去前导零的大端字节，零值贡献零字节），都必须逐字对上。
    fn derive_session_key(&self) -> Result<[u8; EPHEMERAL_LEN], PakeError> {
        let x = self.x.as_ref().ok_or(PakeError::MissingCoordinate("X"))?;
        let y = self.y.as_ref().ok_or(PakeError::MissingCoordinate("Y"))?;
        let z = self.z.as_ref().ok_or(PakeError::MissingCoordinate("Z"))?;

        let mut hasher = Sha256::new();
        if let Some(BoundIdentities { initiator, responder }) = &self.identities {
            write_field(&mut hasher, IDENTITY_TRANSCRIPT_DOMAIN);
            write_field(&mut hasher, &self.password);
            write_field(&mut hasher, initiator);
            write_field(&mut hasher, responder);
            write_field(&mut hasher, self.curve.name().as_bytes());
            for point in [x, y, z] {
                write_field(&mut hasher, &go_bytes(point.x()));
                write_field(&mut hasher, &go_bytes(point.y()));
            }
        } else {
            hasher.update(&self.password);
            for point in [x, y, z] {
                hasher.update(go_bytes(point.x()));
                hasher.update(go_bytes(point.y()));
            }
        }
        Ok(hasher.finalize().into())
    }

    fn finish(&mut self) -> Result<(), PakeError> {
        if self.key.is_some() {
            return Err(PakeError::AlreadyDerived);
        }
        self.key = Some(self.derive_session_key()?);
        Ok(())
    }
}

impl Drop for Exchange {
    fn drop(&mut self) {
        self.password.zeroize();
        self.ephemeral.zeroize();
        if let Some(key) = self.key.as_mut() {
            key.zeroize();
        }
    }
}

/// Go `writeTranscriptField`：u64 小端长度前缀 + 原文。
fn write_field(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_le_bytes());
    hasher.update(value);
}

/// A 方（role 0）。在 croc 里是**接收方**，也就是 PAKE 的发起方。
pub struct Initiator {
    exchange: Exchange,
}

impl Initiator {
    /// 构造并生成第一段公开值 `X = U·pw + α·G`。
    ///
    /// `ephemeral` 就是 Go 内部 `rand.Read` 出来的 32 字节 `α`，由调用方提供。
    pub fn start(
        password: &[u8],
        curve: CurveKind,
        identities: Identities<'_>,
        ephemeral: [u8; EPHEMERAL_LEN],
    ) -> Result<Self, PakeError> {
        let mut exchange = Exchange::new(password, curve, identities, 0)?;
        exchange.ephemeral = ephemeral;
        exchange.upw = exchange.curve.scalar_mult(exchange.curve.u(), &exchange.password);
        exchange.vpw = exchange.curve.scalar_mult(exchange.curve.v(), &exchange.password);
        let alpha_point = exchange.curve.scalar_mult_base(&exchange.ephemeral);
        exchange.x = Some(exchange.curve.add(&exchange.upw, &alpha_point));
        Ok(Self { exchange })
    }

    /// 要发给 B 方的公开值（Go `(*Pake).Bytes()`）。
    pub fn public_bytes(&self) -> String {
        self.exchange.public_bytes()
    }

    /// 收到 B 方的响应后调用：校验 `Y`、算出 `Z = (Y − V·pw)·α`、派生会话密钥。
    pub fn accept_response(&mut self, peer: &[u8]) -> Result<(), PakeError> {
        if self.exchange.key.is_some() {
            return Err(PakeError::AlreadyDerived);
        }
        let peer = wire::decode(peer)?;
        if peer.role == self.exchange.role {
            return Err(PakeError::RoleConflict);
        }
        let y = peer.y.ok_or(PakeError::MissingCoordinate("Y"))?;
        if !self.exchange.curve.is_on_curve(&y) {
            return Err(PakeError::PeerPointNotOnCurve("Y"));
        }
        self.exchange.y = Some(y.clone());
        let difference = self.exchange.curve.subtract(&y, &self.exchange.vpw);
        self.exchange.z = Some(self.exchange.curve.scalar_mult(&difference, &self.exchange.ephemeral));
        self.exchange.finish()
    }

    /// 派生出的会话密钥；尚未完成握手时为 `None`。
    pub fn session_key(&self) -> Option<[u8; EPHEMERAL_LEN]> {
        self.exchange.key
    }

    /// 本端使用的曲线。
    pub fn curve(&self) -> CurveKind {
        self.exchange.curve.kind()
    }
}

impl core::fmt::Debug for Initiator {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Initiator")
            .field("curve", &self.exchange.curve.name())
            .field("has_session_key", &self.exchange.key.is_some())
            .finish()
    }
}

/// B 方（role 1）。在 croc 里是**发送方**，也就是 PAKE 的响应方。
pub struct Responder {
    exchange: Exchange,
}

impl Responder {
    /// 构造。此时还没有任何公开值——要等收到 A 方的 `X` 才能算出 `Y`。
    pub fn start(password: &[u8], curve: CurveKind, identities: Identities<'_>) -> Result<Self, PakeError> {
        Ok(Self { exchange: Exchange::new(password, curve, identities, 1)? })
    }

    /// 收到 A 方的公开值后调用：校验 `X`、算出 `Y = V·pw + β·G` 与
    /// `Z = (X − U·pw)·β`、派生会话密钥。
    ///
    /// `ephemeral` 就是 Go 在 `Update` 里 `rand.Read` 出来的 32 字节 `β`。
    pub fn accept_request(&mut self, peer: &[u8], ephemeral: [u8; EPHEMERAL_LEN]) -> Result<(), PakeError> {
        if self.exchange.key.is_some() {
            return Err(PakeError::AlreadyDerived);
        }
        let peer = wire::decode(peer)?;
        if peer.role == self.exchange.role {
            return Err(PakeError::RoleConflict);
        }
        let x = peer.x.ok_or(PakeError::MissingCoordinate("X"))?;
        if !self.exchange.curve.is_on_curve(&x) {
            return Err(PakeError::PeerPointNotOnCurve("X"));
        }
        self.exchange.x = Some(x.clone());
        self.exchange.ephemeral = ephemeral;
        self.exchange.upw = self.exchange.curve.scalar_mult(self.exchange.curve.u(), &self.exchange.password);
        self.exchange.vpw = self.exchange.curve.scalar_mult(self.exchange.curve.v(), &self.exchange.password);
        let beta_point = self.exchange.curve.scalar_mult_base(&self.exchange.ephemeral);
        self.exchange.y = Some(self.exchange.curve.add(&self.exchange.vpw, &beta_point));
        let difference = self.exchange.curve.subtract(&x, &self.exchange.upw);
        self.exchange.z = Some(self.exchange.curve.scalar_mult(&difference, &self.exchange.ephemeral));
        self.exchange.finish()
    }

    /// 要回送给 A 方的公开值。**必须在 [`Self::accept_request`] 之后调用**，
    /// 此时才包含 `Y`——Go 版同样是 `Update` 之后才取 `Bytes()`。
    pub fn public_bytes(&self) -> String {
        self.exchange.public_bytes()
    }

    /// 派生出的会话密钥；尚未完成握手时为 `None`。
    pub fn session_key(&self) -> Option<[u8; EPHEMERAL_LEN]> {
        self.exchange.key
    }

    /// 本端使用的曲线。
    pub fn curve(&self) -> CurveKind {
        self.exchange.curve.kind()
    }
}

impl core::fmt::Debug for Responder {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Responder")
            .field("curve", &self.exchange.curve.name())
            .field("has_session_key", &self.exchange.key.is_some())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_A: &[u8] = b"receiver-identity";
    const ID_B: &[u8] = b"sender-identity";

    fn alpha() -> [u8; EPHEMERAL_LEN] {
        [0x11u8; EPHEMERAL_LEN]
    }

    fn beta() -> [u8; EPHEMERAL_LEN] {
        [0x22u8; EPHEMERAL_LEN]
    }

    /// 跑一次完整握手，返回双方派生出的会话密钥。
    fn handshake(curve: CurveKind, password: &[u8]) -> ([u8; EPHEMERAL_LEN], [u8; EPHEMERAL_LEN]) {
        handshake_with(curve, password, Identities::bound(ID_A, ID_B))
    }

    /// 同 [`handshake`]，但可以指定转录模式。
    fn handshake_with(
        curve: CurveKind,
        password: &[u8],
        identities: Identities<'_>,
    ) -> ([u8; EPHEMERAL_LEN], [u8; EPHEMERAL_LEN]) {
        let mut alice = Initiator::start(password, curve, identities, alpha()).expect("A 初始化");
        let initiator_value = alice.public_bytes();

        let mut bob = Responder::start(password, curve, identities).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), beta()).expect("B 处理请求");
        let responder_value = bob.public_bytes();

        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");

        let key_a = alice.session_key().expect("A 应有会话密钥");
        let key_b = bob.session_key().expect("B 应有会话密钥");
        (key_a, key_b)
    }

    #[test]
    fn unbound_handshake_agrees_on_every_curve() {
        // 中继握手走的就是这条路：Go 的 `InitCurve`，没有身份绑定。
        for curve in CurveKind::ALL {
            let (key_a, key_b) = handshake_with(curve, b"\x01\x02\x03", Identities::Unbound);
            assert_eq!(key_a, key_b, "{curve} 上无身份绑定握手两侧密钥不一致");
        }
    }

    #[test]
    fn unbound_and_bound_transcripts_differ() {
        // 两条路径的哈希输入结构不同，同样的口令与临时标量必须派生出不同的密钥。
        // 如果哪天有人"顺手统一"了两条分支，这个测试会立刻炸。
        let (bound_a, _) = handshake(CurveKind::Siec, b"\x01\x02\x03");
        let (unbound_a, _) = handshake_with(CurveKind::Siec, b"\x01\x02\x03", Identities::Unbound);
        assert_ne!(bound_a, unbound_a, "两条转录路径派生出了同一把密钥");
    }

    #[test]
    fn unbound_handshake_does_not_require_identities() {
        // `Identities::Unbound` 不该因为"身份为空"而报错——它根本没有身份。
        assert!(Initiator::start(b"pw", CurveKind::Siec, Identities::Unbound, alpha()).is_ok());
        assert!(Responder::start(b"pw", CurveKind::Siec, Identities::Unbound).is_ok());
    }

    #[test]
    fn unbound_handshake_is_deterministic() {
        let first = handshake_with(CurveKind::Siec, b"weak", Identities::Unbound);
        let second = handshake_with(CurveKind::Siec, b"weak", Identities::Unbound);
        assert_eq!(first, second);
    }

    #[test]
    fn both_parties_agree_on_every_curve() {
        for curve in CurveKind::ALL {
            let (key_a, key_b) = handshake(curve, b"correct horse battery staple");
            assert_eq!(key_a, key_b, "{curve} 上双方派生的密钥不一致");
        }
    }

    #[test]
    fn different_passwords_produce_different_keys() {
        let (key_a, key_b) = handshake(CurveKind::Siec, b"password-one");
        assert_eq!(key_a, key_b);
        let (other_a, _) = handshake(CurveKind::Siec, b"password-two");
        assert_ne!(key_a, other_a, "口令不同却派生出同一把密钥");
    }

    #[test]
    fn identities_are_bound_into_the_session_key() {
        let curve = CurveKind::Siec;
        let mut alice = Initiator::start(b"pw", curve, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        let initiator_value = alice.public_bytes();
        let mut bob = Responder::start(b"pw", curve, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), beta()).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");
        let baseline = alice.session_key().expect("应有密钥");

        // 只换身份，其余全部相同。
        let mut carol =
            Initiator::start(b"pw", curve, Identities::bound(b"other-receiver", ID_B), alpha()).expect("A 初始化");
        carol.accept_response(responder_value.as_bytes()).expect("A 处理响应");
        assert_ne!(baseline, carol.session_key().expect("应有密钥"), "身份未绑定进会话密钥");
    }

    #[test]
    fn wire_values_carry_the_expected_role_and_optional_coordinates() {
        let alice = Initiator::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        let decoded = wire::decode(alice.public_bytes().as_bytes()).expect("应能解回");
        assert_eq!(decoded.role, 0);
        assert!(decoded.x.is_some(), "A 的公开值必须带 X");
        assert!(decoded.y.is_none(), "A 在收到响应之前不应有 Y");

        let bob = Responder::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        let decoded = wire::decode(bob.public_bytes().as_bytes()).expect("应能解回");
        assert_eq!(decoded.role, 1);
        assert!(decoded.x.is_none(), "B 在收到请求之前不应有 X");
        assert!(decoded.y.is_none(), "B 在收到请求之前不应有 Y");
    }

    #[test]
    fn responder_value_contains_both_coordinates() {
        let mut bob = Responder::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        let alice = Initiator::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        bob.accept_request(alice.public_bytes().as_bytes(), beta()).expect("B 处理请求");
        let decoded = wire::decode(bob.public_bytes().as_bytes()).expect("应能解回");
        assert!(decoded.x.is_some() && decoded.y.is_some(), "B 的响应必须同时带 X 与 Y");
    }

    #[test]
    fn same_role_is_rejected() {
        let alice = Initiator::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        let other_alice = alice.public_bytes();
        let mut bob = Responder::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        // 把 A 方的值再喂给 A 方自己：role 相同，必须拒绝。
        let mut alice =
            Initiator::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        assert_eq!(alice.accept_response(other_alice.as_bytes()), Err(PakeError::RoleConflict));
        // 反之，把 B 方的值喂给 B 方自己同样拒绝。
        bob.accept_request(other_alice.as_bytes(), beta()).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        let mut other_bob = Responder::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        assert_eq!(other_bob.accept_request(responder_value.as_bytes(), beta()), Err(PakeError::RoleConflict));
    }

    #[test]
    fn malformed_peer_value_is_rejected() {
        let mut alice =
            Initiator::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        assert_eq!(alice.accept_response(b"not json"), Err(PakeError::MalformedWire));
        // 缺少 Y。
        let without_y = format!(
            "{{\"Role\":1,\"{}\":5,\"{}\":12,\"{}\":7,\"{}\":8}}",
            wire::FIELD_U_X,
            wire::FIELD_U_Y,
            wire::FIELD_V_X,
            wire::FIELD_V_Y
        );
        assert_eq!(alice.accept_response(without_y.as_bytes()), Err(PakeError::MissingCoordinate("Y")));
    }

    #[test]
    fn off_curve_peer_point_is_rejected() {
        let mut alice =
            Initiator::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        // (1, 1) 不在 `y² = x³ + 19` 上。
        let bogus = format!(
            "{{\"Role\":1,\"{}\":5,\"{}\":12,\"{}\":7,\"{}\":8,\"{}\":1,\"{}\":1}}",
            wire::FIELD_U_X,
            wire::FIELD_U_Y,
            wire::FIELD_V_X,
            wire::FIELD_V_Y,
            wire::FIELD_Y_X,
            wire::FIELD_Y_Y
        );
        assert_eq!(alice.accept_response(bogus.as_bytes()), Err(PakeError::PeerPointNotOnCurve("Y")));
    }

    #[test]
    fn double_acceptance_is_rejected() {
        let curve = CurveKind::Siec;
        let mut alice = Initiator::start(b"pw", curve, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        let initiator_value = alice.public_bytes();
        let mut bob = Responder::start(b"pw", curve, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), beta()).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");
        assert_eq!(alice.accept_response(responder_value.as_bytes()), Err(PakeError::AlreadyDerived));
        assert_eq!(bob.accept_request(initiator_value.as_bytes(), beta()), Err(PakeError::AlreadyDerived));
    }

    #[test]
    fn handshake_is_deterministic_for_fixed_ephemerals() {
        let first = handshake(CurveKind::Siec, b"pw");
        let second = handshake(CurveKind::Siec, b"pw");
        assert_eq!(first, second);
    }

    #[test]
    fn different_ephemerals_produce_different_keys() {
        let curve = CurveKind::Siec;
        let mut alice = Initiator::start(b"pw", curve, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        let initiator_value = alice.public_bytes();
        let mut bob = Responder::start(b"pw", curve, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), beta()).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");
        let baseline = alice.session_key().expect("应有密钥");

        let mut alice =
            Initiator::start(b"pw", curve, Identities::bound(ID_A, ID_B), [0x33u8; EPHEMERAL_LEN]).expect("A 初始化");
        let mut bob = Responder::start(b"pw", curve, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        bob.accept_request(alice.public_bytes().as_bytes(), [0x44u8; EPHEMERAL_LEN]).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");
        assert_ne!(baseline, alice.session_key().expect("应有密钥"));
    }

    #[test]
    fn missing_identities_are_rejected() {
        assert_eq!(
            Initiator::start(b"pw", CurveKind::Siec, Identities::bound(b"", ID_B), alpha()).map(|_| ()),
            Err(PakeError::MissingIdentity)
        );
        assert_eq!(
            Responder::start(b"pw", CurveKind::Siec, Identities::bound(ID_A, b"")).map(|_| ()),
            Err(PakeError::MissingIdentity)
        );
    }

    #[test]
    fn debug_output_hides_secrets() {
        let alice = Initiator::start(b"super-secret", CurveKind::Siec, Identities::bound(ID_A, ID_B), alpha())
            .expect("A 初始化");
        let rendered = format!("{alice:?}");
        assert!(!rendered.contains("super-secret"), "Debug 输出泄漏了口令");
        assert!(rendered.contains("siec"));
    }

    #[test]
    fn z_coordinate_is_shared_between_parties() {
        // 直接检查 αβ·G 两侧相等：这是协议正确性的核心，比"密钥相等"更贴近原理。
        let curve = CurveKind::Siec;
        let mut alice = Initiator::start(b"pw", curve, Identities::bound(ID_A, ID_B), alpha()).expect("A 初始化");
        let initiator_value = alice.public_bytes();
        let mut bob = Responder::start(b"pw", curve, Identities::bound(ID_A, ID_B)).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), beta()).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");

        assert_eq!(alice.exchange.z, bob.exchange.z, "αβ·G 两侧不一致");
        let z = alice.exchange.z.as_ref().expect("应有 Z");
        assert!(!z.is_identity(), "Z 不应该是无穷远点");
    }
}
