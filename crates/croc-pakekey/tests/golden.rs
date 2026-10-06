#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "测试代码，断言失败就该 panic"
)]

//! 与 Go 参考实现（croc 的 `src/pakekey`）的黄金向量比对。
//!
//! 向量由 `tools/gen-golden` 生成到 `tests/golden/pakekey.json`，覆盖五条曲线 ×
//! 三种用途。参与者身份不在文件里——它由 `purpose`/`room` 推出，正好顺带验证
//! [`croc_pakekey::identities`] 的编码与 Go 一致。**文件不存在时跳过**，见
//! `tools/gen-golden/README.md`。

use std::path::PathBuf;

use croc_pake::{CurveKind, EPHEMERAL_LEN, Identities, Initiator, Responder};
use croc_pakekey::{Context, derive, identities};
use serde_json::Value;

fn load(name: &str) -> Option<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name);
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).expect("黄金向量应是合法 JSON"))
}

fn bytes(case: &Value, key: &str) -> Vec<u8> {
    hex::decode(case[key].as_str().expect("十六进制字段应为字符串")).expect("应是合法十六进制")
}

fn ephemeral(case: &Value, key: &str) -> [u8; EPHEMERAL_LEN] {
    bytes(case, key).try_into().expect("临时标量应为 32 字节")
}

#[test]
fn key_schedule_matches_the_go_reference() {
    let Some(golden) = load("pakekey.json") else {
        eprintln!("跳过：tests/golden/pakekey.json 不存在。先运行 tools/gen-golden 生成（需要 Go 工具链）。");
        return;
    };
    let cases = golden["cases"].as_array().expect("cases 应为数组");
    assert!(!cases.is_empty(), "黄金向量不应为空");

    for case in cases {
        let purpose = case["purpose"].as_str().expect("purpose 应为字符串");
        let room = case["room"].as_str().expect("room 应为字符串");
        let name = case["curve"].as_str().expect("curve 应为字符串");
        let curve = CurveKind::from_name(name).expect("黄金向量里出现了未知曲线");
        let password = bytes(case, "password_hex");
        let label = format!("{name}/{purpose}");

        // 身份由 purpose/room 推出——这一步就在验证 pakekey 的身份编码。
        let (identity_a, identity_b) = identities(purpose, room).expect("应能推出身份");

        let mut alice = Initiator::start(
            &password,
            curve,
            Identities::bound(&identity_a, &identity_b),
            ephemeral(case, "ephemeral_a_hex"),
        )
        .expect("A 初始化");
        let initiator_value = alice.public_bytes();
        assert_eq!(
            hex::encode(initiator_value.as_bytes()),
            case["initiator_hex"].as_str().expect("字段"),
            "{label} 的发起方公开值与 Go 不一致"
        );

        let mut bob =
            Responder::start(&password, curve, Identities::bound(&identity_a, &identity_b)).expect("B 初始化");
        bob.accept_request(initiator_value.as_bytes(), ephemeral(case, "ephemeral_b_hex")).expect("B 处理请求");
        let responder_value = bob.public_bytes();
        assert_eq!(
            hex::encode(responder_value.as_bytes()),
            case["responder_hex"].as_str().expect("字段"),
            "{label} 的响应方公开值与 Go 不一致"
        );

        alice.accept_response(responder_value.as_bytes()).expect("A 处理响应");
        let shared_key = alice.session_key().expect("A 应有会话密钥");
        assert_eq!(
            hex::encode(shared_key),
            case["shared_key_hex"].as_str().expect("字段"),
            "{label} 的会话密钥与 Go 不一致"
        );

        let salt = bytes(case, "salt_hex");
        let context = Context {
            purpose,
            room,
            curve,
            initiator: initiator_value.as_bytes(),
            responder: responder_value.as_bytes(),
            salt: &salt,
        };
        let keys = derive(&shared_key, &context).expect("派生应成功");

        assert_eq!(
            hex::encode(keys.encryption_key()),
            case["encryption_key_hex"].as_str().expect("字段"),
            "{label} 的流量密钥与 Go 不一致"
        );
        assert_eq!(
            hex::encode(keys.confirmation_a()),
            case["confirmation_a_hex"].as_str().expect("字段"),
            "{label} 的 A 确认标签与 Go 不一致"
        );
        assert_eq!(
            hex::encode(keys.confirmation_b()),
            case["confirmation_b_hex"].as_str().expect("字段"),
            "{label} 的 B 确认标签与 Go 不一致"
        );
    }
}
