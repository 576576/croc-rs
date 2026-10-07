//! 参数解析、请求组装与退出码。
//!
//! 结构上刻意分成两层：`parse()` 把 argv 变成 [`Parsed`]（纯数据，可测），
//! `run()` 才碰副作用。于是"`--curve` 拼错会被拒绝"这类规则可以脱离网络与磁盘测试。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use croc_core::codephrase::{self, Components};
use croc_core::policy::Preferences;
use croc_engine::{EngineEvent, EventSink, ReceiveRequest, SendRequest, silent_sink};
use croc_pake::CurveKind;
use tokio_util::sync::CancellationToken;

/// croc 的默认中继地址。
pub const DEFAULT_RELAY: &str = "croc.schollz.com:9009";

/// croc 的默认中继密码。
pub const DEFAULT_PASSPHRASE: &str = "pass123";

/// 顶层命令行。
#[derive(Debug, Parser)]
#[command(
    name = "croc-rs",
    version,
    about = "Send files anywhere, simply and securely.",
    // 允许 `croc-rs <口令码>` 这种"没有子命令"的用法：clap 先试子命令，匹配不上就
    // 落到位置参数上。
    args_conflicts_with_subcommands = false
)]
pub struct Cli {
    /// 子命令。不给就是接收（此时必须给出位置参数里的口令码）。
    #[command(subcommand)]
    command: Option<Command>,

    /// 口令码（接收时直接写在这里）。
    code: Option<String>,

    #[command(flatten)]
    global: Global,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 发送文件或目录。
    Send(SendArgs),
    /// 启动一个自建中继。
    Relay(crate::relay_cmd::RelayArgs),
}

#[derive(Debug, clap::Args)]
struct SendArgs {
    /// 要发送的文件或目录。
    #[arg(required = true, value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// 自定义口令码（至少 6 个字符）。
    #[arg(short = 'c', long)]
    code: Option<String>,

    /// 发送一段文本而不是文件。
    #[arg(short = 't', long)]
    text: Option<String>,

    /// 哈希算法（目前只支持 xxhash）。
    #[arg(long, default_value = "xxhash")]
    hash: String,
}

#[derive(Debug, clap::Args)]
struct Global {
    /// 中继地址。
    #[arg(long, default_value = DEFAULT_RELAY, env = "CROC_RELAY", global = true)]
    relay: String,

    /// 中继密码。
    #[arg(long, default_value = DEFAULT_PASSPHRASE, env = "CROC_PASS", global = true)]
    pass: String,

    /// PAKE 曲线（p521 / p256 / p384 / siec / ed25519）。只有接收方说了算。
    #[arg(long, default_value = "p256", global = true)]
    curve: String,

    /// 落盘目录。
    #[arg(long, default_value = ".", global = true)]
    out: PathBuf,

    /// 不问任何问题，一律接受。
    #[arg(long, global = true)]
    yes: bool,

    /// 目标已存在时直接覆盖（或续传）。
    #[arg(long, global = true)]
    overwrite: bool,

    /// 目标已存在时换一个没被占用的名字。
    #[arg(long, global = true)]
    rename: bool,

    /// 关掉压缩。
    #[arg(long, global = true)]
    no_compress: bool,

    /// 禁用局域网直连（只走中继）。
    ///
    /// 目前是**恒等开关**：本实现还没有本地中继，所以发送方对局域网探测一律回空地址
    /// 列表（等价 Go 的 `--no-local`），接收方也不主动探测。留着它是为了命令行兼容——
    /// 用户脚本里带了这个参数不该报"未知参数"。
    #[arg(long, global = true)]
    no_local: bool,

    /// 什么都不打印。
    #[arg(long, global = true)]
    quiet: bool,

    /// 打开调试日志（`RUST_LOG` 优先）。
    #[arg(long, global = true)]
    debug: bool,
}

/// 解析好的参数。
pub struct Parsed {
    /// 打开调试日志。
    pub debug: bool,
    /// 要执行的动作。
    pub action: Action,
}

/// 要执行的动作。
pub enum Action {
    /// 发送。
    Send(Box<SendRequest>),
    /// 接收。
    Receive(Box<ReceiveRequest>),
    /// 自建中继。
    Relay(crate::relay_cmd::RelayArgs),
}

/// 解析 argv。
///
/// # Errors
///
/// 参数不合法（曲线名拼错、口令码太短、既没给子命令也没给口令码）时返回
/// [`ExitCode::FAILURE`]，并且已经把原因打到 stderr 上了。
pub fn parse() -> Result<Parsed, ExitCode> {
    let cli = Cli::parse();
    let curve = match CurveKind::from_name(&cli.global.curve) {
        Ok(curve) => curve,
        Err(error) => {
            eprintln!("croc-rs: {error}");
            eprintln!("可用曲线：{}", CurveKind::ALL.map(CurveKind::name).join(", "));
            return Err(ExitCode::FAILURE);
        }
    };
    let events = sink(&cli.global);
    let machine_id = machine_id();

    match cli.command {
        Some(Command::Relay(args)) => Ok(Parsed { debug: cli.global.debug, action: Action::Relay(args) }),
        Some(Command::Send(args)) => {
            let mut paths = args.paths;
            if let Some(text) = args.text {
                // `--text`：与 Go 一样落到一个 `croc-stdin-<时间戳>` 临时文件上。
                // 接收方靠这个名字前缀识别"这是文本传输"。
                match write_text_artifact(&text) {
                    Ok(path) => paths.push(path),
                    Err(error) => {
                        eprintln!("croc-rs: {error}");
                        return Err(ExitCode::FAILURE);
                    }
                }
            }
            let (code, components) = match generate_code(args.code.as_deref()) {
                Ok(pair) => pair,
                Err(error) => {
                    eprintln!("croc-rs: {error}");
                    return Err(ExitCode::FAILURE);
                }
            };
            Ok(Parsed {
                debug: cli.global.debug,
                action: Action::Send(Box::new(SendRequest {
                    relay: cli.global.relay,
                    relay_password: cli.global.pass,
                    components,
                    code,
                    curve,
                    paths,
                    hash_algorithm: args.hash,
                    no_compress: cli.global.no_compress,
                    machine_id,
                    events,
                })),
            })
        }
        None => {
            let Some(code) = cli.code else {
                eprintln!("croc-rs: 需要一个口令码，或者一个子命令（send / relay）");
                return Err(ExitCode::FAILURE);
            };
            let components = match codephrase::parse(&code) {
                Ok(components) => components,
                Err(error) => {
                    eprintln!("croc-rs: {error}");
                    return Err(ExitCode::FAILURE);
                }
            };
            let confirm = if cli.global.yes {
                None
            } else {
                Some(Arc::new(prompt_confirm) as Arc<dyn Fn(&croc_protocol::SenderInfo) -> bool + Send + Sync>)
            };
            Ok(Parsed {
                debug: cli.global.debug,
                action: Action::Receive(Box::new(ReceiveRequest {
                    relay: cli.global.relay,
                    relay_password: cli.global.pass,
                    components,
                    curve,
                    out_dir: cli.global.out,
                    preferences: Preferences { overwrite: cli.global.overwrite, rename: cli.global.rename },
                    machine_id,
                    confirm,
                    events,
                })),
            })
        }
    }
}

/// 跑起来。
pub async fn run(parsed: Parsed) -> ExitCode {
    let cancel = CancellationToken::new();
    spawn_signal_handler(cancel.clone());

    match parsed.action {
        Action::Relay(args) => crate::relay_cmd::run(args, cancel).await,
        Action::Send(request) => {
            print_send_instructions(&request);
            match croc_engine::send(*request, cancel).await {
                Ok(summary) => {
                    println!("发送完成：{} 个文件，{} 字节", summary.files, summary.bytes);
                    ExitCode::SUCCESS
                }
                Err(error) => fail(error),
            }
        }
        Action::Receive(request) => match croc_engine::receive(*request, cancel).await {
            Ok(summary) => {
                println!(
                    "接收完成：新收 {} 个（{} 字节），跳过 {} 个，拒绝 {} 个",
                    summary.received, summary.bytes, summary.skipped, summary.declined
                );
                ExitCode::SUCCESS
            }
            Err(error) => fail(error),
        },
    }
}

fn fail(error: croc_engine::EngineError) -> ExitCode {
    if matches!(error, croc_engine::EngineError::Cancelled) {
        eprintln!("croc-rs: 已取消");
    } else {
        eprintln!("croc-rs: {error}");
    }
    ExitCode::FAILURE
}

/// 装 tracing。默认只出 `warn`，`--debug` 或 `RUST_LOG` 才出细节。
pub fn init_tracing(debug: bool) {
    let default = if debug { "croc=debug,croc_engine=debug,croc_relay=debug,info" } else { "warn" };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(default));
    let _ = tracing_subscriber::fmt().with_env_filter(filter).with_target(false).try_init();
}

/// 进度事件出口。
fn sink(global: &Global) -> EventSink {
    if global.quiet {
        return silent_sink();
    }
    Arc::new(|event: EngineEvent| match event {
        EngineEvent::Status(message) => eprintln!("{message}"),
        EngineEvent::FileStarted { name, size } => eprintln!("接收 {name}（{size} 字节）"),
        EngineEvent::FileDone { name } => eprintln!("完成 {name}"),
        EngineEvent::Progress { transferred, total } => {
            if let Some(percent) = transferred.saturating_mul(100).checked_div(total) {
                eprint!("\r{percent}%  ");
                if transferred >= total {
                    eprintln!();
                }
            }
        }
        // `EngineEvent` 是 `#[non_exhaustive]`：将来新增的事件默认不打印，
        // 免得一个没设计过的事件把终端刷花。
        _ => {}
    })
}

/// 生成（或校验）口令码。返回 `(原始码, 协议输入)`。
fn generate_code(requested: Option<&str>) -> Result<(String, Components), codephrase::CodeError> {
    if let Some(code) = requested {
        return Ok((code.to_owned(), codephrase::parse(code)?));
    }
    let code = codephrase::generate(&mut croc_net::OsRandomness);
    let components = codephrase::parse(&code)?;
    Ok((code, components))
}

/// 把 `--text` 的内容落到临时文件。
fn write_text_artifact(text: &str) -> Result<PathBuf, String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut path = std::env::temp_dir();
    path.push(format!("croc-stdin-{stamp}"));
    std::fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

/// 打印"让对方执行什么"。
///
/// 打印的是**口令码**而不是房间名：房间名是 `hex(sha256(码 + "croc"))`，它是线路上的
/// 东西，人对不上。用户之间传递的永远是码。
fn print_send_instructions(request: &SendRequest) {
    println!("口令码：{}", request.code);
    println!("让对方执行：");
    println!("  croc-rs --relay {} {}", request.relay, request.code);
}

/// 交互式确认。
fn prompt_confirm(info: &croc_protocol::SenderInfo) -> bool {
    use std::io::{BufRead as _, Write as _};
    let total: i64 = info.files_to_transfer.iter().map(|entry| entry.size).sum();
    eprint!(
        "接收来自 {} 的 {} 个文件（{} 字节）？[Y/n] ",
        if info.machine_id.is_empty() { "未知机器" } else { &info.machine_id },
        info.files_to_transfer.len(),
        total
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

/// 机器指纹：拿主机名算一个稳定值。
///
/// Go 用的是 `machineid.ID()`（持久化的机器 ID）。本实现只把它当"同一次会话重连"的
/// 标记，所以稳定 + 唯一就够，不必真的去读系统 ID。
fn machine_id() -> String {
    let host =
        std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "unknown".to_owned());
    let digest = croc_engine::hash::xxh64(host.as_bytes());
    format!("{digest:016x}")
}

/// 把 Ctrl-C 接到取消令牌上。
fn spawn_signal_handler(cancel: CancellationToken) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n收到中断，正在收尾…");
            cancel.cancel();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_relay_matches_go() {
        assert_eq!(DEFAULT_RELAY, "croc.schollz.com:9009");
        assert_eq!(DEFAULT_PASSPHRASE, "pass123");
    }

    #[test]
    fn machine_ids_are_stable_and_hex() {
        let id = machine_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|character| character.is_ascii_hexdigit()));
        assert_eq!(id, machine_id(), "同一个进程里必须稳定");
    }

    #[test]
    fn text_artifacts_use_the_prefix_the_policy_layer_expects() {
        let path = write_text_artifact("hello").expect("写临时文件");
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        assert!(croc_core::policy::is_text_artifact(true, &name), "{name} 应被识别为文本传输的临时文件");
        assert_eq!(std::fs::read_to_string(&path).expect("读"), "hello");
        let _ = std::fs::remove_file(&path);
    }
}
