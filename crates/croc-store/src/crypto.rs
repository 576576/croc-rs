//! 密钥派生与 AEAD。
//!
//! 主密钥只有一把（32 字节），各用途的密钥由 HKDF-SHA256 从它派生，`info` 是
//! `"croc-store-v1/<用途>"`。用途有三个：`manifest`（清单）、`data`（分片）、`redeem`
//! （取件能力——能读密文但没有解密密钥）。
//!
//! 每次封装都把 nonce 放在密文前面，所以线上格式是 `nonce ‖ 密文`。

use aes_gcm::aead::{Aead as _, Payload};
use aes_gcm::{Aes256Gcm, KeyInit as _, Nonce};
use hkdf::Hkdf;
use sha2::{Digest as _, Sha256};

use crate::manifest::{self, ChunkRef};
use crate::{Error, KEY_SIZE, MAX_MANIFEST_BYTES, PROTOCOL};

/// AES-GCM 的 nonce 长度。
const NONCE_LEN: usize = 12;

/// AES-GCM 的认证标签长度。
const TAG_LEN: usize = 16;

/// 从主密钥派生一条用途专属的密钥。
fn derive(master: &[u8], label: &str) -> Result<[u8; KEY_SIZE], Error> {
    if master.len() != KEY_SIZE {
        return Err(Error::Format(format!("stored-transfer key must be {KEY_SIZE} bytes")));
    }
    let hkdf = Hkdf::<Sha256>::new(None, master);
    let mut key = [0u8; KEY_SIZE];
    hkdf.expand(format!("{PROTOCOL}/{label}").as_bytes(), &mut key)
        .map_err(|error| Error::Format(format!("derive {label} key: {error}")))?;
    Ok(key)
}

/// 该用途的 AEAD。
fn cipher(master: &[u8], label: &str) -> Result<Aes256Gcm, Error> {
    let key = derive(master, label)?;
    Ok(Aes256Gcm::new(&key.into()))
}

/// 取件能力：它只能解开"取密文"这一步，不是解密密钥。
///
/// 上传时把它的 SHA-256 交给服务端，之后凭它领取清单与分片。
///
/// # Errors
///
/// 主密钥长度不对。
pub fn redeem_capability(master: &[u8]) -> Result<[u8; KEY_SIZE], Error> {
    derive(master, "redeem")
}

/// 服务端存的是这个值，不是能力值本身——数据库泄露也拿不到取件凭证。
#[must_use]
pub fn capability_verifier(capability: &[u8]) -> [u8; 32] {
    Sha256::digest(capability).into()
}

/// 内容的 SHA-256，base64url 无填充。
///
/// 两个地方用它：清单里的文件哈希，以及上传分片时的 `X-Croc-SHA256` 头
/// （服务端据此判断这个分片传完整了没有）。
#[must_use]
pub fn encode_sha256(data: &[u8]) -> String {
    crate::base64url::encode(&Sha256::digest(data))
}

/// 封装。输出 `nonce ‖ 密文`。
fn seal(master: &[u8], label: &str, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, Error> {
    let cipher = cipher(master, label)?;
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce).map_err(|error| Error::Random(error.to_string()))?;
    let mut sealed = cipher
        .encrypt(&Nonce::from(nonce), Payload { msg: plaintext, aad })
        .map_err(|_| Error::Format("stored-transfer seal failed".to_owned()))?;
    let mut output = Vec::with_capacity(NONCE_LEN + sealed.len());
    output.extend_from_slice(&nonce);
    output.append(&mut sealed);
    Ok(output)
}

/// 解封。输入是 `nonce ‖ 密文`。
fn open(master: &[u8], label: &str, ciphertext: &[u8], aad: &[u8]) -> Result<Vec<u8>, Error> {
    let cipher = cipher(master, label)?;
    if ciphertext.len() < NONCE_LEN + TAG_LEN {
        return Err(Error::Format("stored-transfer ciphertext is too short".to_owned()));
    }
    let (nonce, body) = ciphertext.split_at(NONCE_LEN);
    // 长度上面已经查过，转成定长数组只是为了让类型对得上。
    let nonce: [u8; NONCE_LEN] =
        nonce.try_into().map_err(|_| Error::Format("stored-transfer nonce has the wrong length".to_owned()))?;
    cipher.decrypt(&Nonce::from(nonce), Payload { msg: body, aad }).map_err(|_| Error::Authentication)
}

/// 清单的 AAD：协议名、传输 ID、用途，用 NUL 分隔。
fn manifest_aad(id: &str) -> Vec<u8> {
    let mut aad = Vec::with_capacity(PROTOCOL.len() + id.len() + 10);
    aad.extend_from_slice(PROTOCOL.as_bytes());
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(b"\x00manifest");
    aad
}

/// 分片的 AAD：在清单的基础上再拼四个大端 32 位整数（整次传输内的分片下标、文件序号、
/// 文件内的分片序号、明文长度）。
///
/// 明文长度也在这里面，所以把分片截短或补长都会认证失败。
fn chunk_aad(id: &str, reference: ChunkRef) -> Result<Vec<u8>, Error> {
    let mut aad = Vec::with_capacity(PROTOCOL.len() + id.len() + 22);
    aad.extend_from_slice(PROTOCOL.as_bytes());
    aad.push(0);
    aad.extend_from_slice(id.as_bytes());
    aad.extend_from_slice(b"\x00chunk\x00");
    for value in [reference.object_index, reference.file_index, reference.file_chunk, reference.plain_size] {
        let value = u32::try_from(value)
            .map_err(|_| Error::Format("stored-transfer chunk index is out of range".to_owned()))?;
        aad.extend_from_slice(&value.to_be_bytes());
    }
    Ok(aad)
}

/// 校验并加密一份清单。
///
/// # Errors
///
/// 清单不合法（见 [`manifest::decode`]），或主密钥长度不对。
pub fn seal_manifest(master: &[u8], id: &str, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    manifest::decode(plaintext, MAX_MANIFEST_BYTES)?;
    seal(master, "manifest", plaintext, &manifest_aad(id))
}

/// 解密并校验一份清单，返回它的 JSON 字节。
///
/// # Errors
///
/// 认证失败、或解密出来的清单不合法。
pub fn open_manifest(master: &[u8], id: &str, ciphertext: &[u8], max_bytes: i64) -> Result<Vec<u8>, Error> {
    let plaintext = open(master, "manifest", ciphertext, &manifest_aad(id))?;
    manifest::decode(&plaintext, max_bytes)?;
    Ok(plaintext)
}

/// 加密一个分片。
///
/// # Errors
///
/// 分片的位置与明文长度不自洽，或主密钥长度不对。
pub fn seal_chunk(master: &[u8], id: &str, reference: ChunkRef, plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let length = i64::try_from(plaintext.len()).unwrap_or(i64::MAX);
    manifest::validate_chunk_ref(reference, length)?;
    seal(master, "data", plaintext, &chunk_aad(id, reference)?)
}

/// 解密一个分片，并核对解密后的长度。
///
/// # Errors
///
/// 认证失败，或解密出来的长度与 `reference` 说的不一致。
pub fn open_chunk(master: &[u8], id: &str, reference: ChunkRef, ciphertext: &[u8]) -> Result<Vec<u8>, Error> {
    manifest::validate_chunk_ref(reference, reference.plain_size)?;
    let plaintext = open(master, "data", ciphertext, &chunk_aad(id, reference)?)?;
    if i64::try_from(plaintext.len()).ok() != Some(reference.plain_size) {
        return Err(Error::Format("stored-transfer chunk has the wrong plaintext length".to_owned()));
    }
    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> [u8; KEY_SIZE] {
        [7u8; KEY_SIZE]
    }

    #[test]
    fn the_same_label_always_derives_the_same_key() {
        assert_eq!(derive(&key(), "data").expect("派生"), derive(&key(), "data").expect("派生"));
    }

    #[test]
    fn different_labels_derive_different_keys() {
        // 用途之间必须隔离：拿 `redeem` 当 `data` 用应当解不开。
        assert_ne!(derive(&key(), "data").expect("派生"), derive(&key(), "redeem").expect("派生"));
        assert_ne!(derive(&key(), "data").expect("派生"), derive(&key(), "manifest").expect("派生"));
    }

    #[test]
    fn a_sealed_chunk_round_trips() {
        let reference = ChunkRef { object_index: 3, file_index: 1, file_chunk: 2, plain_size: 5 };
        let sealed = seal_chunk(&key(), "abc", reference, b"hello").expect("封装");
        assert_eq!(sealed.len(), NONCE_LEN + 5 + TAG_LEN, "线上格式是 nonce ‖ 密文");
        assert_eq!(open_chunk(&key(), "abc", reference, &sealed).expect("解封"), b"hello");
    }

    #[test]
    fn a_chunk_cannot_be_moved_to_another_transfer() {
        // 分片没有绑传输 ID 的话，服务端就能把 A 的分片喂给 B。
        let reference = ChunkRef { object_index: 0, file_index: 0, file_chunk: 0, plain_size: 5 };
        let sealed = seal_chunk(&key(), "abc", reference, b"hello").expect("封装");
        assert!(matches!(open_chunk(&key(), "xyz", reference, &sealed), Err(Error::Authentication)));
    }

    #[test]
    fn a_chunk_cannot_be_reordered() {
        let first = ChunkRef { object_index: 0, file_index: 0, file_chunk: 0, plain_size: 5 };
        let second = ChunkRef { object_index: 1, file_index: 0, file_chunk: 1, plain_size: 5 };
        let sealed = seal_chunk(&key(), "abc", first, b"hello").expect("封装");
        assert!(matches!(open_chunk(&key(), "abc", second, &sealed), Err(Error::Authentication)));
    }

    #[test]
    fn a_chunk_cannot_be_truncated() {
        let reference = ChunkRef { object_index: 0, file_index: 0, file_chunk: 0, plain_size: 5 };
        let sealed = seal_chunk(&key(), "abc", reference, b"hello").expect("封装");
        let shorter = ChunkRef { plain_size: 4, ..reference };
        assert!(matches!(open_chunk(&key(), "abc", shorter, &sealed), Err(Error::Authentication)));
        let longer = ChunkRef { plain_size: 6, ..reference };
        assert!(matches!(open_chunk(&key(), "abc", longer, &sealed), Err(Error::Authentication)));
    }

    #[test]
    fn a_wrong_key_does_not_open() {
        let reference = ChunkRef { object_index: 0, file_index: 0, file_chunk: 0, plain_size: 5 };
        let sealed = seal_chunk(&key(), "abc", reference, b"hello").expect("封装");
        assert!(matches!(open_chunk(&[9u8; KEY_SIZE], "abc", reference, &sealed), Err(Error::Authentication)));
    }

    #[test]
    fn a_short_ciphertext_is_rejected_before_decrypting() {
        let reference = ChunkRef { object_index: 0, file_index: 0, file_chunk: 0, plain_size: 5 };
        assert!(matches!(open_chunk(&key(), "abc", reference, &[0u8; 8]), Err(Error::Format(_))));
    }

    #[test]
    fn a_key_of_the_wrong_length_is_refused() {
        assert!(matches!(derive(&[0u8; 31], "data"), Err(Error::Format(_))));
    }

    #[test]
    fn the_capability_is_not_the_master_key() {
        let capability = redeem_capability(&key()).expect("派生");
        let verifier = capability_verifier(&capability);
        assert_eq!(verifier.len(), 32);
        assert_ne!(verifier, key(), "服务端存的是 SHA-256，不是能力值本身");
    }

    #[test]
    fn an_out_of_range_chunk_index_is_refused() {
        let reference = ChunkRef { object_index: i64::from(u32::MAX) + 1, file_index: 0, file_chunk: 0, plain_size: 5 };
        assert!(matches!(chunk_aad("abc", reference), Err(Error::Format(_))));
    }
}
