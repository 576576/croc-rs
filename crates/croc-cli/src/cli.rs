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

/// Tailscale 数据面尚未就绪时打给用户看的提示。
///
/// Go 版的原文是 `Tailcat is unavailable in this build; using the croc relay instead.`
/// （`src/cli/cli.go:573`）。我们用自己的一句话，因为我们的降级理由是"上游还没稳"，
/// 而不是"这个构建没编进去"——见 `docs/agent/plan/08-tailscale.md` §2。
pub const TAILSCALE_PENDING_NOTE: &str =
    "note: tailscale data plane is not available yet (pending upstream tailcat-rs); using relay";

/// `--transport` 与 `--local` 冲突时的报错（Go 版同文，`src/cli/cli.go:700`）。
///
/// `--local` 与 `--no-local` 的互斥交给 clap（`conflicts_with`），不在这里重复一句。
pub const TRANSPORT_CONFLICT: &str = "croc-rs: --transport must be auto for local-only transfers";

/// 数据面选择。
///
/// `auto` 与 `relay` 目前等价——本实现还没有局域网中继，`auto` 的"先试局域网"这一支
/// 恒为假。`derp` 是 ⏸ pending：提示后按 `relay` 处理，**不是错误、不影响退出码**
/// （对齐 Go 版 `resolveTransportMode()` 在 `tailcatAvailable == false` 时的降级）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    /// 自动：局域网优先，否则走中继。
    Auto,
    /// 强制走中继。
    Relay,
    /// Tailscale DERP —— ⏸ pending，提示后按中继处理。
    Derp,
}

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
    ///
    /// 不设 `required`：`send --text` 不需要路径（Go 版同样允许
    /// `croc send --text hello`）。"至少得有个东西可发"这条规则放在
    /// [`validate_sources`] 里查，才能给出比 clap 默认文案更清楚的提示。
    #[arg(value_name = "PATH")]
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

    /// 数据面：`auto`（默认）/ `relay` / `derp`。
    ///
    /// `derp` 目前是 **pending** 状态：打一条 `note:` 后按 `relay` 处理，不影响退出码。
    /// 理由见 `docs/agent/plan/08-tailscale.md`。
    #[arg(long, value_enum, default_value_t = Transport::Auto, global = true)]
    transport: Transport,

    /// 只走局域网连接（不碰公网中继）。
    ///
    /// 发送方起一个**只活在这次传输里**的中继，用 UDP 组播把端口喊出去
    /// （`239.255.255.250:9999`，载荷 `croc` + 端口），接收方组播找到它再连。
    /// 见 `croc_engine::local`。
    #[arg(long, global = true, conflicts_with = "no_local")]
    local: bool,

    /// 禁用局域网直连（只走中继）。
    ///
    /// 发送方不起内建中继、也不广播。留着它是为了命令行兼容——用户脚本里带了这个
    /// 参数不该报"未知参数"。
    #[arg(long, global = true, conflicts_with = "local")]
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
    Relay(Box<RelayRequest>),
}

/// 自建中继的请求。
///
/// 密码单独拎出来，是因为它来自**全局** `--pass`，不属于 `relay` 子命令自己的参数
/// （Go 版也是这样：`--pass` 是全局 flag，`relay` 只认 `--host`/`--ports`）。
pub struct RelayRequest {
    /// 子命令参数。
    pub args: crate::relay_cmd::RelayArgs,
    /// 中继密码（已解析过 `CROC_PASS` 与默认值）。
    pub password: String,
}

/// 解析 argv。
///
/// # Errors
///
/// 参数不合法（曲线名拼错、口令码太短、既没给子命令也没给口令码）时返回
/// [`ExitCode::FAILURE`]，并且已经把原因打到 stderr 上了。
pub fn parse() -> Result<Parsed, ExitCode> {
    let cli = Cli::parse();

    if let Err(reason) = validate_transport(cli.global.local, cli.global.transport) {
        eprintln!("{reason}");
        return Err(ExitCode::FAILURE);
    }
    // 降级提示在 `--quiet` 下要吞掉——Go 的 `writeTailcatRelayFallbackWarning()`
    // 也是先看 `quiet`（`src/cli/cli.go:581`）。
    if cli.global.transport == Transport::Derp && !cli.global.quiet {
        eprintln!("{TAILSCALE_PENDING_NOTE}");
    }

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
        Some(Command::Relay(args)) => {
            let password = cli.global.pass.clone();
            Ok(Parsed { debug: cli.global.debug, action: Action::Relay(Box::new(RelayRequest { args, password })) })
        }
        Some(Command::Send(args)) => {
            let mut paths = args.paths;
            if let Err(reason) = validate_sources(&paths, args.text.as_deref()) {
                eprintln!("{reason}");
                return Err(ExitCode::FAILURE);
            }
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
                    local: cli.global.local,
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
                    no_compress: cli.global.no_compress,
                    local: cli.global.local,
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
        Action::Relay(request) => crate::relay_cmd::run(request.args, &request.password, cancel).await,
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

/// 校验数据面相关的参数组合。
///
/// `Err` 里是**已经可以整行打到 stderr** 的字符串。抽成纯函数是为了能脱开
/// `std::env::args()` 测这两条规则。
fn validate_transport(local: bool, transport: Transport) -> Result<(), &'static str> {
    // 这条与 Go 版同文（`src/cli/cli.go:700`）。先查它，免得用户同时踩两个坑
    // 却只看到第二个。
    if local && transport != Transport::Auto {
        return Err(TRANSPORT_CONFLICT);
    }
    Ok(())
}

/// `send` 至少要有个东西可发：一个路径，或者一段 `--text`。
fn validate_sources(paths: &[PathBuf], text: Option<&str>) -> Result<(), &'static str> {
    if paths.is_empty() && text.is_none() {
        return Err("croc-rs: send 需要至少一个路径，或者一段 --text 文本");
    }
    Ok(())
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
    // `--local` 时中继就是本端，喊出去的端口每次都不同，对端是靠组播找过来的——
    // 打印一个 `--relay` 只会让人拿一个下次就失效的地址。
    if request.local {
        println!("  croc-rs --local {}", request.code);
    } else {
        println!("  croc-rs --relay {} {}", request.relay, request.code);
    }
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

    #[test]
    fn derp_degrades_to_relay_without_failing() {
        // pending 期间 `derp` 不是错误：解析照常通过，退出码不受影响。
        assert_eq!(validate_transport(false, Transport::Derp), Ok(()));
        assert_eq!(validate_transport(false, Transport::Auto), Ok(()));
        assert_eq!(validate_transport(false, Transport::Relay), Ok(()));
    }

    #[test]
    fn the_pending_note_names_the_reason() {
        // 文案是我们与用户的契约（`docs/agent/plan/04-cli-plan.md` §1.4），改它要同步改文档。
        assert!(TAILSCALE_PENDING_NOTE.starts_with("note: "), "降级提示必须以 note: 开头");
        assert!(TAILSCALE_PENDING_NOTE.contains("tailcat-rs"), "要说清 pending 的对象");
        assert!(TAILSCALE_PENDING_NOTE.ends_with("using relay"), "要说清实际走了什么");
    }

    #[test]
    fn local_only_is_accepted_but_still_needs_auto_transport() {
        // `--local` 现在真的能用了：不该再报"未实现"。
        assert_eq!(validate_transport(true, Transport::Auto), Ok(()));
        // 但它仍然与显式的 `--transport` 互斥——Go 版同文（src/cli/cli.go:700）。
        assert!(validate_transport(true, Transport::Relay).is_err());
    }

    #[test]
    fn local_conflicts_with_an_explicit_transport_like_go() {
        // Go 版原文：`--transport must be auto for local-only transfers`（src/cli/cli.go:700）。
        for transport in [Transport::Relay, Transport::Derp] {
            let error = validate_transport(true, transport).expect_err("应报互斥");
            assert!(error.contains("--transport must be auto for local-only transfers"), "{error}");
        }
    }

    #[test]
    fn send_accepts_text_without_a_path_but_needs_something() {
        // Go 允许 `croc send --text hello`；我们的 `paths` 因此不能是 clap 的必填项。
        assert_eq!(validate_sources(&[], Some("hello")), Ok(()));
        assert_eq!(validate_sources(&[PathBuf::from("a.bin")], None), Ok(()));
        assert_eq!(validate_sources(&[PathBuf::from("a.bin")], Some("hello")), Ok(()));
        let error = validate_sources(&[], None).expect_err("两样都没有应报错");
        assert!(error.starts_with("croc-rs: "), "报错要带程序名前缀");
    }
}
