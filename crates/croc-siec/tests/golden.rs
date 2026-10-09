#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "测试代码，断言失败就该 panic"
)]

//! 与 Go 参考实现的基准向量比对。
//!
//! 向量由 `croc-playground/tools/gen-golden`（Go 程序，直接用 `tscholl2/siec` 与 `crypto/elliptic`）
//! 生成到 `tests/golden/curves.json`。文件不存在时本测试会跳过并打印提示——
//! 这样没有 Go 工具链的机器也能跑 `cargo test`，而一旦生成过向量，比对就是硬断言。
//!
//! 生成方式见 `tests/golden/README.md`。

use std::path::PathBuf;

use croc_siec::params::weierstrass_by_name;
use croc_siec::{AffinePoint, parse_int};
use num_bigint::BigInt;
use serde_json::Value;

fn load(name: &str) -> Option<Value> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden").join(name);
    let text = std::fs::read_to_string(path).ok()?;
    Some(serde_json::from_str(&text).expect("基准向量应是合法 JSON"))
}

fn decimal(case: &Value, key: &str) -> BigInt {
    let literal = case[key].as_str().expect("坐标应以十进制字符串给出");
    parse_int(literal).expect("坐标应能解析成整数")
}

fn bytes(case: &Value, key: &str) -> Vec<u8> {
    hex::decode(case[key].as_str().expect("十六进制字段应为字符串")).expect("应是合法十六进制")
}

#[test]
fn curve_arithmetic_matches_the_go_reference() {
    let Some(golden) = load("curves.json") else {
        eprintln!(
            "跳过：tests/golden/curves.json 不存在。先运行 croc-playground/tools/gen-golden 生成（需要 Go 工具链）。"
        );
        return;
    };
    let cases = golden["cases"].as_array().expect("cases 应为数组");
    assert!(!cases.is_empty(), "基准向量不应为空");

    for case in cases {
        let name = case["curve"].as_str().expect("curve 应为字符串");
        let curve = weierstrass_by_name(name).expect("基准向量里出现了未知曲线");

        let base = AffinePoint::new(decimal(case, "base_x"), decimal(case, "base_y"));
        assert_eq!(base, curve.base_point(), "{name} 的基点与 Go 不一致");
        assert!(curve.is_on_curve(&base), "{name} 的基点不在曲线上");

        let doubled = AffinePoint::new(decimal(case, "doubled_x"), decimal(case, "doubled_y"));
        assert_eq!(curve.double(&base), doubled, "{name} 的 Double 与 Go 不一致");

        let tripled = AffinePoint::new(decimal(case, "tripled_x"), decimal(case, "tripled_y"));
        assert_eq!(curve.add(&doubled, &base), tripled, "{name} 的 Add 与 Go 不一致");

        // 宽标量：NIST 曲线要先走 normalizeScalar，siec 原样进 ScalarMult。
        let scalar = bytes(case, "wide_scalar_hex");
        let effective = if curve.normalize_scalars() { curve.normalize_scalar(&scalar) } else { scalar };
        let expected = AffinePoint::new(decimal(case, "wide_result_x"), decimal(case, "wide_result_y"));
        assert_eq!(curve.scalar_mult(&base, &effective), expected, "{name} 的 ScalarMult 与 Go 不一致");
    }
}
