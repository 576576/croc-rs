//! croc 的 `pakekey` 包：把 SPAKE2 会话密钥绑到参与者、会话、用途与确切转录上。
//!
//! 这是 croc 对 `schollz/pake` 的**加固层**。`pake` 只给你一个 32 字节会话密钥；
//! 这个 crate 负责把「谁跟谁、在哪个房间、做什么、用了哪条曲线、交换了哪两串字节」
//! 全部混进密钥派生，于是：
//!
//! - 同一口令在两个不同房间之间**不能**互相确认（交叉接线攻击）；
//! - 换用途（传输 / 本地探测 / SSH）会换出完全不同的密钥；
//! - 双方的确认标签是**角色专属**的，A 的标签 B 验不过。
//!
//! # 密钥编排
//!
//! ```text
//! transcript = frame(domain, version, purpose, room, curve, idA, idB, initiator, responder, salt)
//! material   = HKDF-SHA256(ikm = spake2_session_key, salt = salt, info = transcript, len = 96)
//! ┌ encryption_key   = material[0..32]                            ← 消息层 AES-256-GCM 的密钥
//! ├ confirmation_keyA = material[32..64]  → HMAC-SHA256(·, transcript)
//! └ confirmation_keyB = material[64..96]  → HMAC-SHA256(·, transcript)
//! ```
//!
//! `frame()` 是「u64 小端长度前缀 + 原文」的拼接。长度前缀是必须的：没有它，
//! `("ab", "c")` 和 `("a", "bc")` 会拼出同一串字节。
//!
//! # 与 Go 版的差异
//!
//! 只有一处：Go 的 `Context.Curve` 是字符串，这里用 [`CurveKind`]，于是「曲线名拼错」
//! 在编译期就不可能发生。线路字节不受影响——`CurveKind::name()` 就是那个字符串。
//!
//! 这个 crate 同样没有 `tokio` / `std::fs` / `Instant::now`，随机数由调用方注入。

#![allow(
    clippy::similar_names,
    reason = "`idA`/`idB`、`confirmationA`/`confirmationB` 是 croc 线路契约里的名字，改名只会让对照 Go 源码变难"
)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

use croc_pake::{CurveKind, EPHEMERAL_LEN, Identities, Initiator, PakeError, Responder};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::Zeroize;

/// 失败模式。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PakeKeyError {
    /// `purpose` 不在 croc 认可的三种用途里。
    #[error("unsupported PAKE purpose {0:?}")]
    UnsupportedPurpose(String),

    /// 房间名不能为空。
    #[error("PAKE room is required")]
    MissingRoom,

    /// 没有 SPAKE2 会话密钥就无从派生。
    #[error("PAKE session key is required")]
    MissingSessionKey,

    /// 双方的公开值都必须参与转录。
    #[error("both PAKE wire values are required")]
    MissingWireValue,

    /// 盐长度必须恰好是 [`SALT_SIZE`]。
    #[error("PAKE salt must be {expected} bytes, got {actual}")]
    InvalidSaltLength {
        /// 期望长度。
        expected: usize,
        /// 实际长度。
        actual: usize,
    },

    /// HKDF 或 HMAC 的内部失败。参数都已在前面校验过，走到这里说明有实现错误。
    #[error("PAKE key schedule failed")]
    KeySchedule,

    /// 底下的 SPAKE2 层报错。
    #[error(transparent)]
    Pake(#[from] PakeError),
}

/// 对端 PAKE 协议的版本号。Go 侧是 `pakekey.ProtocolVersion`，不匹配即拒绝。
pub const PROTOCOL_VERSION: u64 = 2;

/// 对端密钥编排要求的盐长度。
pub const SALT_SIZE: usize = 32;

/// 文件传输的用途标识。
pub const PURPOSE_TRANSFER: &str = "peer-transfer";
/// 局域网 IP 探测的用途标识。
pub const PURPOSE_LOCAL_PROBE: &str = "local-ip-probe";
/// SSH 分享的用途标识。
pub const PURPOSE_SSH: &str = "peer-ssh";

/// 参与者身份绑定的域分隔串。
const IDENTITY_DOMAIN: &[u8] = b"croc/spake2/participant/v2";

/// 通道转录的域分隔串。
const TRANSCRIPT_DOMAIN: &[u8] = b"croc/spake2/channel/v2";

/// 流量密钥长度（AES-256-GCM）。
const ENCRYPTION_KEY_LEN: usize = 32;

/// 确认标签长度（HMAC-SHA256）。
const CONFIRMATION_LEN: usize = 32;

/// HKDF 一次扩出 96 字节，正好切成三份 32 字节。
const KEY_MATERIAL_LEN: usize = 96;

/// 派生密钥所需的会话上下文。
///
/// 每个字段都会被写进转录，改任何一项都会换出完全不同的密钥与确认标签。
#[derive(Debug, Clone, Copy)]
pub struct Context<'a> {
    /// 用途，取值见 [`PURPOSE_TRANSFER`] 等常量。
    pub purpose: &'a str,
    /// 房间名（croc 的 `--room`）。
    pub room: &'a str,
    /// 本次握手用的曲线。
    pub curve: CurveKind,
    /// 发起方（A 方，接收方）的 SPAKE2 公开值。
    pub initiator: &'a [u8],
    /// 响应方（B 方，发送方）的 SPAKE2 公开值。
    pub responder: &'a [u8],
    /// 密钥编排的盐，长度必须为 [`SALT_SIZE`]。
    pub salt: &'a [u8],
}

/// 派生出的通道密钥材料。
///
/// 析构时清零；`Debug` 只打印长度，不打印内容。
pub struct Keys {
    encryption_key: [u8; ENCRYPTION_KEY_LEN],
    confirmation_a: [u8; CONFIRMATION_LEN],
    confirmation_b: [u8; CONFIRMATION_LEN],
}

impl Keys {
    /// 消息层流量密钥（AES-256-GCM）。
    pub fn encryption_key(&self) -> &[u8; ENCRYPTION_KEY_LEN] {
        &self.encryption_key
    }

    /// A 方（接收方）的确认标签。
    pub fn confirmation_a(&self) -> &[u8; CONFIRMATION_LEN] {
        &self.confirmation_a
    }

    /// B 方（发送方）的确认标签。
    pub fn confirmation_b(&self) -> &[u8; CONFIRMATION_LEN] {
        &self.confirmation_b
    }
}

impl Drop for Keys {
    fn drop(&mut self) {
        self.encryption_key.zeroize();
        self.confirmation_a.zeroize();
        self.confirmation_b.zeroize();
    }
}

impl core::fmt::Debug for Keys {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("Keys")
            .field("encryption_key", &"<32 bytes>")
            .field("confirmation_a", &"<32 bytes>")
            .field("confirmation_b", &"<32 bytes>")
            .finish()
    }
}

/// 把若干字段拼成无歧义的长度前缀编码。
///
/// Go 的 `frame(fields ...[]byte)`：每段先写 u64 小端长度，再写原文。
pub fn frame(fields: &[&[u8]]) -> Vec<u8> {
    let capacity = fields.iter().map(|field| 8 + field.len()).sum();
    let mut encoded = Vec::with_capacity(capacity);
    for field in fields {
        encoded.extend_from_slice(&(field.len() as u64).to_le_bytes());
        encoded.extend_from_slice(field);
    }
    encoded
}

/// 生成一对隐式参与者身份。
///
/// **A 方永远是接收方，B 方永远是发送方**——这个顺序是 croc 的协议约定，不是调用方
/// 可以挑的。返回 `(id_a, id_b)`。
pub fn identities(purpose: &str, room: &str) -> Result<(Vec<u8>, Vec<u8>), PakeKeyError> {
    if !matches!(purpose, PURPOSE_TRANSFER | PURPOSE_LOCAL_PROBE | PURPOSE_SSH) {
        return Err(PakeKeyError::UnsupportedPurpose(purpose.to_owned()));
    }
    if room.is_empty() {
        return Err(PakeKeyError::MissingRoom);
    }
    let version = PROTOCOL_VERSION.to_le_bytes();
    let role_a = frame(&[IDENTITY_DOMAIN, &version, purpose.as_bytes(), room.as_bytes(), b"receiver"]);
    let role_b = frame(&[IDENTITY_DOMAIN, &version, purpose.as_bytes(), room.as_bytes(), b"sender"]);
    Ok((role_a, role_b))
}

/// 构造 A 方（croc 里的接收方）的 PAKE 实例，身份已按 `purpose`/`room` 绑定。
pub fn initiator(
    password: &[u8],
    curve: CurveKind,
    purpose: &str,
    room: &str,
    ephemeral: [u8; EPHEMERAL_LEN],
) -> Result<Initiator, PakeKeyError> {
    let (id_a, id_b) = identities(purpose, room)?;
    Initiator::start(password, curve, Identities::bound(&id_a, &id_b), ephemeral).map_err(PakeKeyError::from)
}

/// 构造 B 方（croc 里的发送方）的 PAKE 实例，身份已按 `purpose`/`room` 绑定。
pub fn responder(password: &[u8], curve: CurveKind, purpose: &str, room: &str) -> Result<Responder, PakeKeyError> {
    let (id_a, id_b) = identities(purpose, room)?;
    Responder::start(password, curve, Identities::bound(&id_a, &id_b)).map_err(PakeKeyError::from)
}

/// 把 SPAKE2 会话密钥扩展成流量密钥与双向确认标签。
pub fn derive(shared_key: &[u8], context: &Context<'_>) -> Result<Keys, PakeKeyError> {
    if shared_key.is_empty() {
        return Err(PakeKeyError::MissingSessionKey);
    }
    if context.initiator.is_empty() || context.responder.is_empty() {
        return Err(PakeKeyError::MissingWireValue);
    }
    if context.salt.len() != SALT_SIZE {
        return Err(PakeKeyError::InvalidSaltLength { expected: SALT_SIZE, actual: context.salt.len() });
    }
    let (id_a, id_b) = identities(context.purpose, context.room)?;
    let transcript = transcript(context, &id_a, &id_b);

    let hkdf = Hkdf::<Sha256>::new(Some(context.salt), shared_key);
    let mut material = [0u8; KEY_MATERIAL_LEN];
    hkdf.expand(&transcript, &mut material).map_err(|_| PakeKeyError::KeySchedule)?;

    let keys = {
        let (encryption_key, rest) = material.split_at(ENCRYPTION_KEY_LEN);
        let (confirmation_key_a, confirmation_key_b) = rest.split_at(CONFIRMATION_LEN);
        Keys {
            encryption_key: encryption_key.try_into().map_err(|_| PakeKeyError::KeySchedule)?,
            confirmation_a: confirmation_tag(confirmation_key_a, &transcript)?,
            confirmation_b: confirmation_tag(confirmation_key_b, &transcript)?,
        }
    };
    material.zeroize();
    Ok(keys)
}

/// 常数时间校验一个角色专属的确认标签。
///
/// 长度不等于 32 直接返回 `false`——Go 的 `Confirm` 同样先查 `len(expected) ==
/// sha256.Size`，再走 `hmac.Equal`。
pub fn confirm(expected: &[u8], received: &[u8]) -> bool {
    expected.len() == CONFIRMATION_LEN && bool::from(expected.ct_eq(received))
}

fn transcript(context: &Context<'_>, id_a: &[u8], id_b: &[u8]) -> Vec<u8> {
    let version = PROTOCOL_VERSION.to_le_bytes();
    frame(&[
        TRANSCRIPT_DOMAIN,
        &version,
        context.purpose.as_bytes(),
        context.room.as_bytes(),
        context.curve.name().as_bytes(),
        id_a,
        id_b,
        context.initiator,
        context.responder,
        context.salt,
    ])
}

fn confirmation_tag(key: &[u8], transcript: &[u8]) -> Result<[u8; CONFIRMATION_LEN], PakeKeyError> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).map_err(|_| PakeKeyError::KeySchedule)?;
    mac.update(transcript);
    Ok(mac.finalize().into_bytes().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PASSWORD: &[u8] = b"shared password";
    const ROOM: &str = "room-one";

    /// 跑一次完整握手，返回双方各自的派生密钥。
    fn exchange(curve: CurveKind, purpose: &str, room: &str) -> (Keys, Keys) {
        let mut alice = initiator(PASSWORD, curve, purpose, room, [0x11u8; EPHEMERAL_LEN]).expect("A 初始化");
        let initiator_value = alice.public_bytes();
        let mut bob = responder(PASSWORD, curve, purpose, room).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), [0x22u8; EPHEMERAL_LEN]).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");

        let salt = [0x42u8; SALT_SIZE];
        let context = Context {
            purpose,
            room,
            curve,
            initiator: initiator_value.as_bytes(),
            responder: responder_value.as_bytes(),
            salt: &salt,
        };
        let keys_a = derive(&alice.session_key().expect("A 有会话密钥"), &context).expect("A 派生");
        let keys_b = derive(&bob.session_key().expect("B 有会话密钥"), &context).expect("B 派生");
        (keys_a, keys_b)
    }

    #[test]
    fn both_peers_derive_identical_material_on_every_curve_and_purpose() {
        for curve in CurveKind::ALL {
            for purpose in [PURPOSE_TRANSFER, PURPOSE_LOCAL_PROBE, PURPOSE_SSH] {
                let (keys_a, keys_b) = exchange(curve, purpose, ROOM);
                assert_eq!(keys_a.encryption_key(), keys_b.encryption_key(), "{curve}/{purpose} 的流量密钥不一致");
                assert_eq!(keys_a.confirmation_a(), keys_b.confirmation_a(), "{curve}/{purpose} 的 A 标签不一致");
                assert_eq!(keys_a.confirmation_b(), keys_b.confirmation_b(), "{curve}/{purpose} 的 B 标签不一致");
            }
        }
    }

    #[test]
    fn cross_wired_rooms_cannot_confirm() {
        let (keys_one, _) = exchange(CurveKind::P256, PURPOSE_TRANSFER, "room-one");
        let (keys_two, _) = exchange(CurveKind::P256, PURPOSE_TRANSFER, "room-two");
        assert!(!confirm(keys_one.confirmation_a(), keys_two.confirmation_a()));
        assert!(!confirm(keys_one.confirmation_b(), keys_two.confirmation_b()));
        assert_ne!(keys_one.encryption_key(), keys_two.encryption_key());
    }

    #[test]
    fn changing_any_transcript_field_invalidates_the_confirmation() {
        let salt = [1u8; SALT_SIZE];
        let base = Context {
            purpose: PURPOSE_TRANSFER,
            room: ROOM,
            curve: CurveKind::P256,
            initiator: b"initiator",
            responder: b"responder",
            salt: &salt,
        };
        let want = derive(b"a fixed shared session key", &base).expect("基线派生");

        let other_salt = [2u8; SALT_SIZE];
        let variants = [
            ("room", Context { room: "room-two", ..base }),
            ("purpose", Context { purpose: PURPOSE_LOCAL_PROBE, ..base }),
            ("curve", Context { curve: CurveKind::P384, ..base }),
            ("initiator", Context { initiator: b"other", ..base }),
            ("responder", Context { responder: b"other", ..base }),
            ("salt", Context { salt: &other_salt, ..base }),
        ];
        for (name, changed) in variants {
            let got = derive(b"a fixed shared session key", &changed).expect("变体派生");
            assert!(
                !confirm(want.confirmation_a(), got.confirmation_a())
                    && !confirm(want.confirmation_b(), got.confirmation_b()),
                "{name} 改变后确认标签仍然有效"
            );
        }
    }

    #[test]
    fn role_specific_tags_differ() {
        let (keys, _) = exchange(CurveKind::Siec, PURPOSE_TRANSFER, ROOM);
        assert_ne!(keys.confirmation_a(), keys.confirmation_b(), "A/B 确认标签不应相同");
        assert!(!confirm(keys.confirmation_a(), keys.confirmation_b()));
    }

    #[test]
    fn derive_validates_its_inputs() {
        let salt = [0u8; SALT_SIZE];
        let valid = Context {
            purpose: PURPOSE_TRANSFER,
            room: "room",
            curve: CurveKind::P256,
            initiator: b"a",
            responder: b"b",
            salt: &salt,
        };
        assert_eq!(derive(&[], &valid).unwrap_err(), PakeKeyError::MissingSessionKey);
        assert_eq!(derive(b"key", &Context { initiator: b"", ..valid }).unwrap_err(), PakeKeyError::MissingWireValue);
        assert_eq!(derive(b"key", &Context { responder: b"", ..valid }).unwrap_err(), PakeKeyError::MissingWireValue);
        assert_eq!(
            derive(b"key", &Context { salt: &salt[..SALT_SIZE - 1], ..valid }).unwrap_err(),
            PakeKeyError::InvalidSaltLength { expected: SALT_SIZE, actual: SALT_SIZE - 1 }
        );
        assert_eq!(
            derive(b"key", &Context { purpose: "nope", ..valid }).unwrap_err(),
            PakeKeyError::UnsupportedPurpose("nope".to_owned())
        );
    }

    #[test]
    fn confirm_rejects_wrong_sizes_and_values() {
        assert!(!confirm(&[0u8; 32], &[0u8; 31]));
        assert!(!confirm(&[0u8; 32], &[0u8; 33]));
        assert!(!confirm(&[], &[]));
        assert!(!confirm(&[1u8; 32], &[2u8; 32]));
        assert!(confirm(&[1u8; 32], &[1u8; 32]));
    }

    #[test]
    fn identities_are_role_separated_and_validated() {
        let (id_a, id_b) = identities(PURPOSE_TRANSFER, ROOM).expect("应能构造身份");
        assert_ne!(id_a, id_b, "A/B 身份不应相同");
        assert!(id_a.ends_with(b"receiver"));
        assert!(id_b.ends_with(b"sender"));
        assert_eq!(identities("nope", ROOM), Err(PakeKeyError::UnsupportedPurpose("nope".to_owned())));
        assert_eq!(identities(PURPOSE_TRANSFER, ""), Err(PakeKeyError::MissingRoom));
    }

    #[test]
    fn identity_framing_is_unambiguous() {
        // 没有长度前缀时 `("ab", "c")` 与 `("a", "bc")` 会拼出同一串字节。
        assert_ne!(frame(&[b"ab", b"c"]), frame(&[b"a", b"bc"]));
    }

    #[test]
    fn frame_layout_is_pinned() {
        assert_eq!(
            frame(&[b"ab", b"c"]),
            b"\x02\x00\x00\x00\x00\x00\x00\x00ab\x01\x00\x00\x00\x00\x00\x00\x00c".to_vec()
        );
        assert!(frame(&[]).is_empty());
    }

    #[test]
    fn identity_layout_is_pinned() {
        let (id_a, id_b) = identities(PURPOSE_TRANSFER, ROOM).expect("应能构造身份");
        // 每段 = 8 字节长度前缀 + 内容：
        // domain(8+26) + version(8+8) + purpose(8+13) + room(8+8) + role(8+8|8+6)
        assert_eq!(id_a.len(), 34 + 16 + 21 + 16 + 16);
        assert_eq!(id_b.len(), 34 + 16 + 21 + 16 + 14);
        assert_eq!(
            id_a.split_at(34 + 16 + 21 + 16).0,
            id_b.split_at(34 + 16 + 21 + 16).0,
            "除了角色段，两条身份的前缀必须一致"
        );
        assert_eq!(id_a.split_at(34 + 16 + 21 + 16).1, b"\x08\x00\x00\x00\x00\x00\x00\x00receiver".as_slice());
        assert_eq!(id_b.split_at(34 + 16 + 21 + 16).1, b"\x06\x00\x00\x00\x00\x00\x00\x00sender".as_slice());
        assert!(id_a.starts_with(&[26, 0, 0, 0, 0, 0, 0, 0]));
    }

    #[test]
    fn derivation_is_deterministic() {
        let (first, _) = exchange(CurveKind::Siec, PURPOSE_TRANSFER, ROOM);
        let (second, _) = exchange(CurveKind::Siec, PURPOSE_TRANSFER, ROOM);
        assert_eq!(first.encryption_key(), second.encryption_key());
        assert_eq!(first.confirmation_a(), second.confirmation_a());
    }

    #[test]
    fn debug_output_hides_key_material() {
        let (keys, _) = exchange(CurveKind::Siec, PURPOSE_TRANSFER, ROOM);
        let rendered = format!("{keys:?}");
        assert!(rendered.contains("<32 bytes>"));
        assert!(!rendered.contains(&format!("{:?}", keys.encryption_key())));
    }
}
