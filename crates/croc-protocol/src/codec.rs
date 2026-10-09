//! 压缩与 AEAD：消息体从 JSON 到线上字节的最后两步。
//!
//! 对应 Go 的 `src/compress` 与 `src/crypt`。
//!
//! # 线格式
//!
//! ```text
//! 加密前：DEFLATE(JSON(Message))
//! 加密后：nonce(12) ‖ ciphertext ‖ tag(16)
//! ```
//!
//! 这个 `nonce ‖ ct ‖ tag` 布局来自 Go 的
//! `aead.Seal(ivBytes, ivBytes, plaintext, nil)`——把 nonce 放在 `dst` 开头，Seal
//! 就把密文和 tag 追加在它后面。RustCrypto 的 `Aead::encrypt` 返回的是 `ct ‖ tag`，
//! 所以这里自己拼上 nonce 前缀。
//!
//! # nonce 为什么是参数
//!
//! Go 在 `crypt.Encrypt` 内部 `rand.Read` 出 nonce。本 crate 是 sans-io 的，随机数
//! 由调用方注入：生产环境给 12 字节真随机，测试里给固定值就能做确定性回归。

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use flate2::Compression;
use flate2::read::DeflateDecoder;
use flate2::write::DeflateEncoder;
use std::io::{Read, Write};

use crate::ProtocolError;

/// AES-256-GCM 的密钥长度。
pub const ENCRYPTION_KEY_LEN: usize = 32;

/// AES-GCM 的 nonce 长度。
pub const AEAD_NONCE_LEN: usize = 12;

/// AES-GCM 的 tag 长度。
pub const AEAD_TAG_LEN: usize = 16;

/// 解压上限，与 Go 的 `message.maxDecompressedMessageSize` 一致。
pub const MAX_DECOMPRESSED_SIZE: usize = 64 * 1024 * 1024;

/// DEFLATE 压缩。
///
/// Go 用 `flate.HuffmanOnly`（只做霍夫曼、不做 LZ77 匹配）来省 CPU。DEFLATE 是自描述
/// 格式，解码端不关心编码端用了什么级别，所以这里用 `Compression::fast()`：Go 能解开
/// 它的产物，它也能解开 Go 的产物。差异只在字节数上。
pub fn compress(data: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    let mut encoder = DeflateEncoder::new(Vec::new(), Compression::fast());
    encoder.write_all(data).map_err(|error| ProtocolError::Compression(error.to_string()))?;
    encoder.finish().map_err(|error| ProtocolError::Compression(error.to_string()))
}

/// DEFLATE 解压，输出被 `limit` 兜住。
///
/// 上限是必须的：压缩炸弹可以把 1 KiB 的输入解成几个 GiB。实现上多读 1 字节，只要
/// 拿到 `limit + 1` 就说明超了——这样"恰好等于上限"和"被截断"不会混淆。
pub fn decompress(data: &[u8], limit: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut decoder = DeflateDecoder::new(data);
    let mut output = Vec::new();
    let probe = u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1);
    let read = decoder
        .by_ref()
        .take(probe)
        .read_to_end(&mut output)
        .map_err(|error| ProtocolError::Compression(error.to_string()))?;
    if read > limit {
        return Err(ProtocolError::DecompressedTooLarge { actual: read, limit });
    }
    Ok(output)
}

/// AES-256-GCM 加密，输出 `nonce ‖ ciphertext ‖ tag`。
pub fn encrypt(plaintext: &[u8], key: &[u8], nonce: &[u8; AEAD_NONCE_LEN]) -> Result<Vec<u8>, ProtocolError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| ProtocolError::InvalidKeyLength { expected: ENCRYPTION_KEY_LEN, actual: key.len() })?;
    let ciphertext =
        cipher.encrypt(&Nonce::from(*nonce), plaintext).map_err(|_| ProtocolError::AuthenticationFailed)?;

    let mut payload = Vec::with_capacity(AEAD_NONCE_LEN + ciphertext.len());
    payload.extend_from_slice(nonce);
    payload.extend_from_slice(&ciphertext);
    Ok(payload)
}

/// AES-256-GCM 解密 `nonce ‖ ciphertext ‖ tag`。
///
/// 校验失败统一报 [`ProtocolError::AuthenticationFailed`]——"口令不对"和"消息被改过"
/// 在密码学上不可区分，把它们分开报反而是 oracle。
pub fn decrypt(payload: &[u8], key: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if payload.len() < AEAD_NONCE_LEN + AEAD_TAG_LEN {
        return Err(ProtocolError::CiphertextTooShort(payload.len()));
    }
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|_| ProtocolError::InvalidKeyLength { expected: ENCRYPTION_KEY_LEN, actual: key.len() })?;
    let (nonce, ciphertext) = payload.split_at(AEAD_NONCE_LEN);
    let nonce = Nonce::try_from(nonce).map_err(|_| ProtocolError::CiphertextTooShort(payload.len()))?;
    cipher.decrypt(&nonce, ciphertext).map_err(|_| ProtocolError::AuthenticationFailed)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; ENCRYPTION_KEY_LEN] = [0x42; ENCRYPTION_KEY_LEN];
    const NONCE: [u8; AEAD_NONCE_LEN] = [0x24; AEAD_NONCE_LEN];

    #[test]
    fn compression_round_trips() {
        let payload = b"{\"t\":\"pake\",\"v\":2}".repeat(20);
        let compressed = compress(&payload).expect("应能压缩");
        assert_eq!(decompress(&compressed, MAX_DECOMPRESSED_SIZE).expect("应能解压"), payload);
    }

    #[test]
    fn compression_round_trips_empty_input() {
        let compressed = compress(b"").expect("应能压缩");
        assert!(decompress(&compressed, MAX_DECOMPRESSED_SIZE).expect("应能解压").is_empty());
    }

    #[test]
    fn decompression_honours_the_limit() {
        let payload = vec![b'a'; 4096];
        let compressed = compress(&payload).expect("应能压缩");
        assert_eq!(decompress(&compressed, 4096).expect("恰好等于上限应通过").len(), 4096);
        assert_eq!(
            decompress(&compressed, 4095),
            Err(ProtocolError::DecompressedTooLarge { actual: 4096, limit: 4095 })
        );
    }

    #[test]
    fn decompression_rejects_garbage() {
        assert!(matches!(decompress(b"not deflate at all", 1024), Err(ProtocolError::Compression(_))));
    }

    #[test]
    fn encryption_layout_is_nonce_then_ciphertext_then_tag() {
        let plaintext = b"hello";
        let payload = encrypt(plaintext, &KEY, &NONCE).expect("应能加密");
        assert_eq!(payload.len(), AEAD_NONCE_LEN + plaintext.len() + AEAD_TAG_LEN);
        assert_eq!(payload.get(..AEAD_NONCE_LEN), Some(NONCE.as_slice()));
    }

    #[test]
    fn encryption_round_trips() {
        let plaintext = b"{\"t\":\"fileinfo\",\"m\":\"hello\"}";
        let payload = encrypt(plaintext, &KEY, &NONCE).expect("应能加密");
        assert_eq!(decrypt(&payload, &KEY).expect("应能解密"), plaintext);
    }

    #[test]
    fn wrong_key_is_rejected() {
        let payload = encrypt(b"secret", &KEY, &NONCE).expect("应能加密");
        let mut wrong = KEY;
        wrong[0] ^= 0xff;
        assert_eq!(decrypt(&payload, &wrong), Err(ProtocolError::AuthenticationFailed));
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let mut payload = encrypt(b"secret", &KEY, &NONCE).expect("应能加密");
        let last = payload.len() - 1;
        if let Some(byte) = payload.get_mut(last) {
            *byte ^= 0x01;
        }
        assert_eq!(decrypt(&payload, &KEY), Err(ProtocolError::AuthenticationFailed));
    }

    #[test]
    fn short_ciphertext_is_rejected_before_any_crypto() {
        assert_eq!(decrypt(&[], &KEY), Err(ProtocolError::CiphertextTooShort(0)));
        assert_eq!(
            decrypt(&[0u8; AEAD_NONCE_LEN + AEAD_TAG_LEN - 1], &KEY),
            Err(ProtocolError::CiphertextTooShort(AEAD_NONCE_LEN + AEAD_TAG_LEN - 1))
        );
    }

    #[test]
    fn wrong_key_length_is_rejected() {
        assert_eq!(
            encrypt(b"x", &[0u8; 16], &NONCE),
            Err(ProtocolError::InvalidKeyLength { expected: ENCRYPTION_KEY_LEN, actual: 16 })
        );
        assert_eq!(
            decrypt(&[0u8; 64], &[0u8; 31]),
            Err(ProtocolError::InvalidKeyLength { expected: ENCRYPTION_KEY_LEN, actual: 31 })
        );
    }

    #[test]
    fn nonce_is_not_reused_implicitly() {
        // 同一个 nonce 两次加密同一明文必须得到同一密文（GCM 是确定性的），
        // 这正是"nonce 必须由调用方保证唯一"的原因，也是本 crate 把它当参数的理由。
        let first = encrypt(b"same", &KEY, &NONCE).expect("应能加密");
        let second = encrypt(b"same", &KEY, &NONCE).expect("应能加密");
        assert_eq!(first, second);

        let other = encrypt(b"same", &KEY, &[0x25; AEAD_NONCE_LEN]).expect("应能加密");
        assert_ne!(first, other);
    }
}
