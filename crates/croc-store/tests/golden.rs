#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "测试代码，断言失败就该 panic"
)]

//! 与 Go 参考实现（`schollz/croc` v11 的 `src/storecrypto`）的基准向量比对。
//!
//! 向量由 `croc-playground/tools/gen-golden` 生成到 `tests/golden/store.json`。
//! 文件不存在时跳过，见 `tests/golden/README.md`。
//!
//! 这一层是服务端与客户端共用的加密格式，只要有一处偏差，两边就完全对不上：密钥派生
//! 会得到不同的密钥、AAD 会不同、清单里的时间格式会不同。所以这里逐项比对。

use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use croc_store::manifest::{self, ChunkRef};
use croc_store::share::Share;
use croc_store::{KEY_SIZE, crypto};
use serde_json::Value;

fn load() -> Option<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/store.json");
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).expect("基准向量应是合法 JSON"))
}

fn hex_bytes(value: &Value, key: &str) -> Vec<u8> {
    hex::decode(value[key].as_str().expect("十六进制字段应为字符串")).expect("应是合法十六进制")
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().expect("字段应为字符串").to_owned()
}

/// 向量里的 `Share`。
fn share_of(golden: &Value) -> Share {
    Share { origin: text(golden, "origin"), id: text(golden, "id"), master_key: hex_bytes(golden, "masterKeyHex") }
}

#[test]
fn key_derivation_matches_the_go_reference() {
    let Some(golden) = load() else {
        eprintln!("跳过：tests/golden/store.json 不存在。先运行 croc-playground/tools/gen-golden。");
        return;
    };
    let master = hex_bytes(&golden, "masterKeyHex");

    let capability = crypto::redeem_capability(&master).expect("派生取件能力");
    assert_eq!(hex::encode(capability), text(&golden, "redeemCapabilityHex"), "取件能力不一致");

    let verifier = crypto::capability_verifier(&capability);
    assert_eq!(hex::encode(verifier), text(&golden, "capabilityVerifierHex"), "能力校验值不一致");
}

#[test]
fn share_encoding_matches_the_go_reference() {
    let Some(golden) = load() else {
        eprintln!("跳过：tests/golden/store.json 不存在。");
        return;
    };
    let share = share_of(&golden);

    assert_eq!(share.browser_url().expect("编浏览器链接"), text(&golden, "browserURL"));
    assert_eq!(share.cli_token().expect("编命令行 token"), text(&golden, "cliToken"));

    // 两种编码都要能解回同一个 Share。
    assert_eq!(Share::parse(&text(&golden, "browserURL")).expect("解链接"), share);
    assert_eq!(Share::parse(&text(&golden, "cliToken")).expect("解 token"), share);
}

#[test]
fn manifest_json_matches_the_go_reference_byte_for_byte() {
    let Some(golden) = load() else {
        eprintln!("跳过：tests/golden/store.json 不存在。");
        return;
    };
    let expected = text(&golden, "manifestJSON");

    // Rust 这边解一遍再重新序列化，两侧的字节必须完全一样：字段名、字段顺序、
    // 以及时间戳的写法（Go 用 RFC3339Nano，没有小数部分时不带小数点）都算在内。
    let parsed = manifest::decode(expected.as_bytes(), croc_store::MAX_MANIFEST_BYTES).expect("解清单");
    let reserialized = serde_json::to_string(&parsed).expect("重新序列化");
    assert_eq!(reserialized, expected);
}

#[test]
fn the_manifest_ciphertext_from_go_decrypts() {
    let Some(golden) = load() else {
        eprintln!("跳过：tests/golden/store.json 不存在。");
        return;
    };
    let master = hex_bytes(&golden, "masterKeyHex");
    let id = text(&golden, "id");
    let ciphertext = hex_bytes(&golden, "manifestCipherHex");

    // AAD 里带着传输 ID 与用途，所以这一步同时验证了 HKDF 的 info 串、AAD 的拼法、
    // 以及 nonce 前置的线上格式。
    let plaintext = crypto::open_manifest(&master, &id, &ciphertext, croc_store::MAX_MANIFEST_BYTES).expect("解清单");
    assert_eq!(String::from_utf8(plaintext).expect("应是 UTF-8"), text(&golden, "manifestJSON"));
}

#[test]
fn the_chunk_ciphertexts_from_go_decrypt() {
    let Some(golden) = load() else {
        eprintln!("跳过：tests/golden/store.json 不存在。");
        return;
    };
    let master = hex_bytes(&golden, "masterKeyHex");
    let id = text(&golden, "id");

    let expected_manifest =
        manifest::decode(text(&golden, "manifestJSON").as_bytes(), croc_store::MAX_MANIFEST_BYTES).expect("解清单");
    let references = manifest::chunk_refs(&expected_manifest);
    let chunks = golden["chunks"].as_array().expect("chunks 应为数组");
    assert_eq!(references.len(), chunks.len(), "分片个数应与 Go 的 ChunkRefs 一致");

    for (index, chunk) in chunks.iter().enumerate() {
        let reference = ChunkRef {
            object_index: chunk["objectIndex"].as_i64().expect("objectIndex"),
            file_index: chunk["fileIndex"].as_i64().expect("fileIndex"),
            file_chunk: chunk["fileChunk"].as_i64().expect("fileChunk"),
            plain_size: chunk["plainSize"].as_i64().expect("plainSize"),
        };
        // 位置信息进 AAD，所以分片下标、文件序号、长度任何一项算错，这里都会认证失败。
        assert_eq!(references[index], reference, "第 {index} 个分片的位置不一致");

        let ciphertext = hex::decode(chunk["cipherHex"].as_str().expect("cipherHex")).expect("十六进制");
        let plaintext = crypto::open_chunk(&master, &id, reference, &ciphertext).expect("解分片");
        assert_eq!(hex::encode(&plaintext), chunk["plainHex"].as_str().expect("plainHex"));
    }
}

#[test]
fn the_go_vectors_are_self_consistent() {
    let Some(golden) = load() else {
        eprintln!("跳过：tests/golden/store.json 不存在。");
        return;
    };
    // 生成器自检：主密钥与传输 ID 的长度、密文的线上格式（nonce ‖ 密文）。
    assert_eq!(hex_bytes(&golden, "masterKeyHex").len(), KEY_SIZE);
    assert_eq!(URL_SAFE_NO_PAD.decode(text(&golden, "id")).expect("id 应是 base64url").len(), 16);

    for chunk in golden["chunks"].as_array().expect("chunks") {
        let ciphertext = hex::decode(chunk["cipherHex"].as_str().expect("cipherHex")).expect("十六进制");
        let plain_size = usize::try_from(chunk["plainSize"].as_i64().expect("plainSize")).expect("长度");
        assert_eq!(ciphertext.len(), 12 + plain_size + 16, "线上格式应是 nonce ‖ 密文");
    }
}
