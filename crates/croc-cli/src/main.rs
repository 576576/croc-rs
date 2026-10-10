//! `croc-rs`：croc 的命令行客户端。
//!
//! 三个入口，与 Go 版一致：
//!
//! ```text
//! croc-rs send <文件…>            发送（打印口令码）
//! croc-rs <口令码>                接收
//! croc-rs relay --ports 9009,…    自建中继
//! ```
//!
//! 中继做成子命令而不是独立二进制：发布产物只有 `croc-rs` 与 `crocw-rs` 两个，
//! 多一个二进制就要多一套打包与签名（见 `docs/agent/build-and-ci.md` §3）。
//!
//! 这个文件只做三件事：解析参数、组装 [`croc_engine`] 的请求、把结果翻译成退出码。
//! 传输本身一行都不在这里。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

// 把 `locales/*.yml` 烘成 `t!` 查的那张表。必须在 crate 根：`t!` 展开成
// `crate::_rust_i18n_t!`，而这个名字是本宏生成的。
rust_i18n::i18n!("locales", fallback = "en");

mod cli;
mod clipboard;
mod config;
mod duration;
mod help;
mod locale;
mod notice;
mod qr;
mod relay_cmd;
mod relay_pool;
mod store;
mod term;

use rust_i18n::t;
use std::process::ExitCode;

fn main() -> ExitCode {
    // 定下界面语言要在打出第一句话之前：帮助、报错、提示全走 `t!`。
    locale::install();
    let parsed = match cli::parse() {
        Ok(parsed) => parsed,
        Err(code) => return code,
    };
    cli::init_tracing(parsed.debug);

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{}", t!("cli.error.runtime_start", error = error));
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(cli::run(parsed))
}
