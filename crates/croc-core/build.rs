//! 词表在编译期就被规范化并校验，而不是像 Go 那样在包初始化时 panic。
//!
//! 两个理由：
//!
//! 1. `core.autocrlf=true` 的 Windows 检出会把词表变成 CRLF。croc 的码是逐字节
//!    参与哈希的，行尾多一个 `\r` 就会让同一个码算出不同的房间名。在这里统一成 LF，
//!    运行期拿到的就只有唯一形态。
//! 2. 词表被改坏（行数变了、混进大写、出现重复词）应该在 `cargo build` 就报错，
//!    而不是等用户跑到"生成口令码"那一步才崩。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "构建脚本：出错就该让构建失败，而不是把问题推到运行期"
)]

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::PathBuf;

/// EFF Short Wordlist #1 的条目数。
const EXPECTED_WORDS: usize = 1296;
const SOURCE: &str = "assets/eff-short-wordlist-1.txt";
const OUTPUT: &str = "eff-short-wordlist-1.txt";

fn main() {
    println!("cargo:rerun-if-changed={SOURCE}");

    let raw = fs::read_to_string(SOURCE).unwrap_or_else(|error| panic!("读不到词表 {SOURCE}: {error}"));
    // 只归一化完整的 CRLF 序列，其他控制字符仍然视为非法。
    let normalized = raw.replace("\r\n", "\n");
    let body = normalized.strip_suffix('\n').unwrap_or(&normalized);
    let words: Vec<&str> = body.split('\n').collect();

    assert_eq!(words.len(), EXPECTED_WORDS, "{SOURCE} 有 {} 条，应为 {EXPECTED_WORDS} 条", words.len());

    let mut seen = HashSet::with_capacity(words.len());
    for word in &words {
        assert!(is_list_word(word), "{SOURCE} 含非法词 {word:?}（只允许小写字母与词内连字符）");
        assert!(seen.insert(*word), "{SOURCE} 含重复词 {word:?}");
    }

    let destination = PathBuf::from(env::var("OUT_DIR").unwrap()).join(OUTPUT);
    fs::write(&destination, body).unwrap_or_else(|error| panic!("写不出词表 {}: {error}", destination.display()));
}

/// 词表里的一个词：非空，用 `-` 分隔的每一段都是非空的小写 ASCII 字母。
///
/// 允许词内连字符是因为词表里确实有一个 `yo-yo`——它正是解析必须回溯的原因。
fn is_list_word(word: &str) -> bool {
    !word.is_empty()
        && word.split('-').all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_lowercase()))
}
