//! 参数解析、请求组装与退出码。
//!
//! 结构上刻意分成两层：`parse()` 把 argv 变成 [`Parsed`]（纯数据，可测），
//! `run()` 才碰副作用。于是"`--curve` 拼错会被拒绝"这类规则可以脱离网络与磁盘测试。

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::parser::ValueSource;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, Subcommand};
use croc_core::codephrase::{self, Components};
use croc_core::policy::Preferences;
use croc_engine::{EngineEvent, EventSink, ReceiveRequest, SendRequest, silent_sink};
use croc_pake::CurveKind;
use rust_i18n::t;

use crate::config::{Remembered, Side, decode_relay, encode_relay};
use crate::help;
use crate::locale::Locale;
use crate::relay_pool;
use tokio_util::sync::CancellationToken;

/// croc 的默认中继地址。
pub const DEFAULT_RELAY: &str = "croc.schollz.com:9009";

/// IPv6 中继的默认地址。Go 的 `models.DEFAULT_RELAY6`。
pub const DEFAULT_RELAY6: &str = "croc6.schollz.com:9009";

/// `--version` 打出的那一行。
///
/// 版本号由 `build.rs` 烘进来：CI 用 `release.yml` 算出的 `CROC_VERSION_NAME`，本地构建
/// 用提交数推。不是 `Cargo.toml` 里那个包版本——那个与发布通道无关，任何一次构建打出来
/// 都一样，分不清手上的二进制是哪次提交。
///
/// 前面加上 `version ` 是为了形状与 Go 版一致（`croc version 11.5.4`）；clap 打印的是
/// `<name> <version>`，所以这一串正好拼成 `croc-rs version 1.0.50`。
const VERSION_LINE: &str = concat!("version ", env!("CROC_CLI_VERSION"));

/// croc 的默认中继密码。
pub const DEFAULT_PASSPHRASE: &str = "pass123";

// 打给用户看的句子一律走 `t!`，文案在 `locales/` 下（`cli.note.*` / `cli.error.*`）。
// 这里不留文案常量：留一份就会有人只改这一份、忘了另一门语言。

/// 数据面选择。
///
/// `auto` 与 `relay` 目前等价——本实现默认不开局域网直连（等价 Go 的 `--no-local`），
/// 所以 `auto` 的"先试局域网"这一支恒为假；要走局域网得显式给 `--local`。
/// `derp` 尚未实现：提示后按 `relay` 处理，不是错误、不影响退出码。
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    /// 自动：局域网优先，否则走中继。
    Auto,
    /// 强制走中继。
    Relay,
    /// Tailscale DERP：尚未实现，提示后按中继处理。
    Derp,
}

/// 顶层命令行。
#[derive(Debug, Parser)]
#[command(
    name = "croc-rs",
    version = VERSION_LINE,
    // clap 默认给 `-V`，但 Go 版（urfave/cli）的默认版本开关是 `--version, -v`——
    // 习惯 `croc -v` 的人会撞到 "unexpected argument"。这里关掉内置的，自己声明一个，
    // 两个短选项都收。
    disable_version_flag = true,
    // `-h` / `--help` 由 `crate::help` 自己渲染（版式要与 Go 版逐字一致，clap 的渲染器
    // 形状对不上），所以在 clap 这一层把它关掉；`parse()` 会在进 clap 之前先截住它。
    // `help` 子命令同理——它在 Go 版里是 `help, h`，由 `crate::help` 一并接管。
    disable_help_flag = true,
    disable_help_subcommand = true,
    about = "securely transfer files or share a terminal",
    // 允许 `croc-rs <口令码>` 这种"没有子命令"的用法：clap 先试子命令，匹配不上就
    // 落到位置参数上。
    args_conflicts_with_subcommands = false
)]
pub struct Cli {
    /// 子命令。不给就是接收（此时必须给出位置参数里的口令码）。
    #[command(subcommand)]
    command: Option<Command>,

    /// 口令码（接收时直接写在这里）。
    ///
    /// 没给就用环境变量 `CROC_SECRET`（`--extended-clipboard` 复制出来的命令靠的
    /// 就是它）。命令行给的优先。
    #[arg(env = "CROC_SECRET", hide_env_values = true)]
    code: Option<String>,

    /// 打印版本并退出。
    ///
    /// 主短选项是 `-v`（Go 版的形式）；`-V`（clap 的惯例）作为隐藏别名保留，免得已经
    /// 习惯它的人撞车。
    #[arg(short = 'v', long = "version", short_alias = 'V', action = clap::ArgAction::Version)]
    print_version: Option<bool>,

    #[command(flatten)]
    global: Global,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// 发送文件或目录。
    Send(SendArgs),
    /// 启动一个自建中继。
    Relay(crate::relay_cmd::RelayArgs),
    /// 把文件存到暂存服务，拿一个取件链接。
    Store(StoreArgs),
    /// 共享或加入一个协作终端（尚未实现，只打一条提示）。
    Ssh(SshArgs),
    /// 检查并安装新版本（尚未实现，只打一条提示）。
    #[command(alias = "upgrade")]
    Update(UpdateArgs),
    /// 生成 fish 补全并输出到 stdout（尚未实现，只打一条提示）。
    ///
    /// Go 把这个命令标成 hidden，这里照做。
    #[command(hide = true)]
    GenerateFishCompletion,
}

/// `croc-rs ssh` 的参数。
///
/// 位置参数与开关都照 Go 的形式收下，但目前没有功能可做。
/// Go 在这里那个 `--transport` 由全局同名参数承担，所以不重复声明。
#[derive(Debug, clap::Args)]
struct SshArgs {
    /// 六词码。不给就是自己当主机。
    #[arg(value_name = "CODE")]
    code: Option<String>,
    /// 只当主机，不接本地终端。
    #[arg(long)]
    headless: bool,
    /// 主机会话的最长存活时间（Go 风格时长，默认 12h）。
    ///
    /// 默认值写成 `12h0m0s` 而不是 `12h`：这是 Go 的 `time.Duration` 打印自己的样子，
    /// `-h` 要逐字对上。
    #[arg(long, default_value = "12h0m0s")]
    duration: String,
    /// 掉线后客户端重试多久。
    #[arg(long, default_value = "2m0s")]
    reconnect_window: String,
    /// 关掉客户端的自动重连。
    #[arg(long)]
    no_reconnect: bool,
    /// 客户端的数据面（`auto` / `tailcat` / `relay`）。
    ///
    /// 与 Go 一样是 `ssh` 自己的开关，不是全局 `--transport`——后者的取值集也不同。
    #[arg(long, default_value = "auto")]
    transport: String,
    /// 主机 shell 的工作目录。
    #[arg(long, default_value = ".")]
    dir: PathBuf,
}

/// `croc-rs update` 的参数。
///
/// 三个开关都照 Go 的公开形式收下，但目前没有功能可做——收到就只为
/// 挑一句更贴切的提示。
#[derive(Debug, clap::Args)]
struct UpdateArgs {
    /// 只检查有没有新版本，不安装。
    #[arg(long)]
    check: bool,
    /// 安全时不再询问确认。
    #[arg(long)]
    yes: bool,
    /// Go 内部用来登记安装方式的开关。它照 Go 一样隐藏，
    /// 留在命令行里只为让别人脚本里的 `--register-installer` 不至于报"未知参数"。
    #[arg(long, hide = true)]
    register_installer: bool,
}

/// `croc store`：把文件存到暂存服务。
///
/// 与 `send --store` 是同一件事，两处各有一套参数是为了和 Go 的命令行对上。
#[derive(Debug, clap::Args)]
struct StoreArgs {
    /// 要上传的文件。暂存模式只接受普通文件，不递归目录。
    #[arg(value_name = "PATH")]
    paths: Vec<PathBuf>,

    /// 允许的验证下载次数。
    #[arg(long, default_value_t = 1)]
    downloads: i64,

    /// 存储时长（`90m` / `12h` / `3d` / `2w`）。
    #[arg(long, default_value = "1d")]
    expiration: String,

    /// 把 web 接收链接显示成二维码。
    ///
    /// Go 的 `store` 与 `send --store` 都能给，展示的是同一个取件页面的链接。
    #[arg(long, alias = "qr")]
    qrcode: bool,

    /// 暂存服务的来源。
    #[arg(long, default_value = crate::store::DEFAULT_STORE_URL, env = "CROC_STORE_URL")]
    url: String,
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
    ///
    /// 没给就用环境变量 `CROC_SECRET`（与 Go 同名，见 [`merge_send`]）。环境变量优先
    /// ——Go 的 `resolveSendSharedSecret`就是这么定的。
    ///
    /// 这里不把 `CROC_SECRET` 声明成 clap 的 `env`：Go 版的 `--code` 没有环境变量，
    /// 声明了 `-h` 里就会多出一个 `[%CROC_SECRET%]` 标记，与 Go 版对不上。
    #[arg(short = 'c', long)]
    code: Option<String>,

    /// 发送一段文本而不是文件。
    #[arg(short = 't', long)]
    text: Option<String>,

    /// 发送方的数据面：`auto`（默认）/ `relay` / `derp`。
    ///
    /// Go 把它挂在 `send` 上（不是全局），所以接收端没有这个开关。
    ///
    /// `derp` 目前是 pending 状态：打一条 `note:` 后按 `relay` 处理，不影响退出码。
    /// 理由见 `docs/agent/plan/tailscale.md`。
    #[arg(long, value_enum, default_value_t = Transport::Auto)]
    transport: Transport,

    /// 发送时不启用内建中继（与全局的 `--local` 相对）。
    ///
    /// Go 的语义：`--local` 开内建中继，`--no-local` 关掉它。本实现默认就不做局域网
    /// 发现（等价于默认关），所以它目前是空操作。
    #[arg(long)]
    no_local: bool,

    /// 不用多端口复用（只用一条数据流）。
    ///
    /// 与 Go 一样只挂在 `send` 上：接收端不能给，它按中继 banner 里的端口数走。
    #[arg(long)]
    no_multi: bool,

    /// 哈希算法：`xxhash`（默认）/ `md5` / `imohash` / `highway`。
    ///
    /// `imohash` 只采样文件的头、中、尾各 2 MiB，所以大文件上快得多，代价是
    /// "只有采样区不同"的改动可能漏检。
    #[arg(long, default_value = "xxhash")]
    hash: String,

    /// 排除相对路径里包含任一子串的文件（逗号分隔，大小写不敏感）。
    ///
    /// 子串匹配，不是路径匹配：`--exclude node_modules` 会连 `my_node_modules_backup/`
    /// 一起排掉。要精确点掉某个文件用 [`Self::exclude_file`]。
    #[arg(long, value_delimiter = ',')]
    exclude: Vec<String>,

    /// 排除相对路径精确等于任一项的文件（逗号分隔）。
    ///
    /// 相对基准是命令行上给的那个路径：`send --exclude-file a/b.txt photos` 排掉的是
    /// `photos/a/b.txt`。
    #[arg(long, value_delimiter = ',')]
    exclude_file: Vec<String>,

    /// 尊重被传目录下的 `.gitignore`。
    #[arg(long)]
    git: bool,

    /// 目录先打成 zip 再发（zip 里以目录名为顶层，解压后还是一棵目录树）。
    ///
    /// 临时 zip 落在被传目录的父目录下，传输结束后删掉；同名文件已存在会报错。
    #[arg(long)]
    zip: bool,

    /// 发送前把 web 接收链接显示成二维码。
    ///
    /// 链接是 `getcroc.com` 的官方接收页（Go 的 `webReceiveURL`），扫开就能在浏览器里收，
    /// 不需要对方装 croc。
    #[arg(long, alias = "qr")]
    qrcode: bool,

    /// `--local` 下内建中继的基准端口（被占了会往后顺延）。
    ///
    /// 只在 `--local` 下有意义：连外部中继时端口由中继的 banner 说了算，
    /// `--port` 给的数组会被覆盖。
    #[arg(long, default_value = "9009", value_parser = clap::value_parser!(u16).range(1..))]
    port: u16,

    /// `--local` 下内建中继开几个数据端口（也就是数据面并行几条流）。
    ///
    /// 只在 `--local` 下有意义，理由同 [`Self::port`]。
    ///
    /// 上限 8 是数据面的硬上限（`croc_engine::data::MAX_STREAMS`）：再多路也没人听。
    #[arg(long, default_value = "4", value_parser = clap::value_parser!(u16).range(1..=8))]
    transfers: u16,

    /// 把文件存到暂存服务（与 `croc store` 同一套参数）。
    #[arg(long)]
    store: bool,

    /// `--store` 时的下载次数上限。
    ///
    /// Go 的 `--store-downloads` 没有环境变量（`CROC_STORE_DOWNLOADS` 只在 web 服务端
    /// 用），这里也不声明，免得 `-h` 里多一个标记。
    #[arg(long, default_value_t = 1)]
    store_downloads: i64,

    /// `--store` 时的存储时长。
    #[arg(long, default_value = "1d")]
    store_expiration: String,

    /// `--store` 时的服务来源。
    #[arg(long, default_value = crate::store::DEFAULT_STORE_URL, env = "CROC_STORE_URL")]
    store_url: String,
}

#[derive(Debug, clap::Args)]
struct Global {
    /// 中继地址。默认 `croc.schollz.com:9009`。
    ///
    /// 给了非默认值就不再试 `--relay6`，反之亦然：两个都是候选时，Happy Eyeballs
    /// 可能连上用户没指定的那一个，而那边的房间是另一套。
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

    /// 测试用开关。
    ///
    /// Go 拿它让引擎走一条更适合测试的路径。本实现接受它
    /// 只为命令行兼容——脚本里带了这个参数不该报"未知参数"。
    #[arg(long, global = true)]
    testing: bool,

    /// 只走局域网连接（不碰公网中继）。
    ///
    /// 发送方起一个只活在这次传输里的中继，用 UDP 组播把端口喊出去
    /// （`239.255.255.250:9999`，载荷 `croc` + 端口），接收方组播找到它再连。
    /// 见 `croc_engine::local`。
    #[arg(long, global = true)]
    local: bool,

    /// 经 SOCKS5 代理连中继（如 Tor）。
    ///
    /// 地址可以带 `socks5://` 前缀。目标不是局域网地址时才生效，且域名交给代理去解析
    /// ——本地解析会把 DNS 请求漏到网外。与 [`Self::connect`] 同时给出时这个优先。
    #[arg(long, env = "SOCKS5_PROXY", global = true)]
    socks5: Option<String>,

    /// 经 HTTP CONNECT 代理连中继。
    ///
    /// 地址可以带 `http://` 前缀。其余同 [`Self::socks5`]。
    ///
    /// 认 `HTTP_PROXY` 环境变量：Go 版的声明里就有它。
    #[arg(long, env = "HTTP_PROXY", global = true)]
    connect: Option<String>,

    /// 忽略管道进来的 stdin。
    ///
    /// 默认情况下，stdin 是管道/重定向（而不是终端）时，它的内容会被当成一个文件发出去
    /// ——Go 的 `copyStdinContext` 就是这么做的。脚本里用管道喂别的数据时用这个关掉。
    #[arg(long, global = true)]
    ignore_stdin: bool,

    /// 使用自带的 DNS stub resolver，而不是操作系统的解析器。
    ///
    /// 打开后直接问一组公共 DNS（Go 的 `models.publicDNS`），绕过 hosts 文件与
    /// `systemd-resolved` 这类本机配置。与 `--remember` 一起给会被记下来，之后默认开启。
    #[arg(long, global = true)]
    internal_dns: bool,

    /// 切换 classic 模式（切换后直接结束，不传输）。
    ///
    /// classic 模式允许把口令码写在命令行上——那会留在 shell 历史与进程表里，多用户
    /// 机器上别的本地用户能看到并抢先把文件收走。关闭它时，命令行口令码会被拒，改用
    /// `CROC_SECRET` 环境变量（`--classic` 只影响非 Windows 平台，与 Go 一致）。
    #[arg(long, global = true)]
    classic: bool,

    /// 不把口令码复制到剪贴板。
    #[arg(long, global = true)]
    disable_clipboard: bool,

    /// 复制整条接收命令，而不是只复制口令码。
    ///
    /// 命令形如 `CROC_SECRET="<码>" croc-rs --relay <中继>`，对端直接粘进终端就行，
    /// 口令码不会进 shell 历史。
    #[arg(long, global = true)]
    extended_clipboard: bool,

    /// 上行限速，如 `500k` / `1M` / `1048576`（不带单位就是字节/秒）。
    ///
    /// 只对发送方有效。`k` / `m` / `g` 按 1024 进位。
    ///
    /// 主名字是 Go 的 `--throttleUpload`（驼峰，没有横线）；`--throttle-upload` 作为
    /// 别名保留，免得已经习惯那个写法的人撞车。
    #[arg(long = "throttleUpload", alias = "throttle-upload", default_value = "", global = true)]
    throttle_upload: String,

    /// IPv6 中继地址。默认 `croc6.schollz.com:9009`。
    ///
    /// 与 `--relay` 一起按 Happy Eyeballs 试：谁先握手通用谁，所以 IPv6 可用时不必白等
    /// IPv4。两者都连不上才算失败。只给了其中一个（非默认值）时就用那个一个。
    #[arg(long, default_value = DEFAULT_RELAY6, env = "CROC_RELAY6", global = true)]
    relay6: String,

    /// 已知的对方地址（如 `10.0.0.1:9009`、`[::1]:9009`）。
    ///
    /// 给了就只用它：接收方跳过局域网发现直接连过去，也不再试 `--relay`。
    #[arg(long, default_value = "", global = true)]
    ip: String,

    /// 接收方收完之后，把文件内容打到标准输出。
    ///
    /// 文件照常落盘，所以这个开关是
    /// "顺便打一份"，不是"不写盘"。文本传输会自动开它。
    #[arg(long, global = true)]
    stdout: bool,

    /// 把这次用的 `--relay` / `--relay6` 记下来，下次不用再打。
    ///
    /// 存在 `<config>/croc/send.json`（与 Go 同一个位置）。只记这两项——别的开关要么本
    /// 来就有合理默认，要么每次该重新决定。
    #[arg(long, global = true)]
    remember: bool,

    /// 传和收都先问一句。
    ///
    /// 发送方会把"要确认"随清单发给对端，所以对方的 `--yes` 也压不住它；发送前还会
    /// 打印本机 ID，好让对方核对问的是谁。
    #[arg(long, global = true)]
    ask: bool,

    /// `--local` 发现用的组播组地址。
    ///
    /// 端口固定是 9999（Go 用的 `peerdiscovery` 默认值，没有对应开关）。
    #[arg(long, default_value = "239.255.255.250", global = true)]
    multicast: std::net::Ipv4Addr,

    /// 什么都不打印。
    #[arg(long, global = true)]
    quiet: bool,

    /// 用本机保存的撤销凭据删掉一次暂存传输。
    ///
    /// 参数是传输 ID（上传成功时会打印）。凭据存在 `<config>/croc/store-receipts.json`。
    #[arg(long, value_name = "TRANSFER-ID", global = true)]
    revoke: Option<String>,

    /// 打开调试日志（`RUST_LOG` 优先）。
    #[arg(long, global = true)]
    debug: bool,
}

/// 口令码要不要进剪贴板。
///
/// 默认会复制——这是 Go 的行为，也是这个功能的价值所在：口令码要交给对端，
/// 手抄一遍很容易错。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Clipboard {
    /// 只复制口令码。
    Code,
    /// `--extended-clipboard`：复制整条接收命令（`CROC_SECRET=… croc-rs …`）。
    ///
    /// 这样口令码不会进 shell 历史，也不会出现在进程表里。
    Command,
    /// `--disable-clipboard`：不碰剪贴板。
    Off,
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
    ///
    /// `qrcode` / `clipboard` 是纯展示选项，不进 [`SendRequest`]，它们在
    /// [`crate::notice::SendNotice`] 里。
    Send {
        request: Box<SendRequest>,
        /// 口令码是本端自动生成的、且走的是公共中继池。
        ///
        /// 连不上中继时要忘掉池里记住的那台，否则下次还会先试这台。
        generated_public_code: bool,
    },
    /// 接收。
    Receive(Box<ReceiveRequest>),
    /// 自建中继。
    Relay(Box<RelayRequest>),
    /// 存到暂存服务。
    Store(Box<crate::store::StoreRequest>),
    /// 接收一次暂存传输。
    ReceiveStored(Box<crate::store::StoreReceiveRequest>),
    /// 撤销一次暂存传输。
    Revoke(String),
    /// 切换 classic 模式（`--classic`）。
    ///
    /// 它是动作不是开关：Go 把它放在 `Before` 钩子里，处理完就 `os.Exit(0)`，
    /// 后面的命令一概不跑。
    ToggleClassic,
    /// 协作终端（尚未实现）。
    Ssh,
    /// 自更新（尚未实现）。
    Update {
        /// 只检查，不安装。
        check: bool,
    },
    /// 生成 fish 补全（尚未实现）。
    GenerateFishCompletion,
}

/// 自建中继的请求。
///
/// 密码单独拎出来，是因为它来自全局 `--pass`，不属于 `relay` 子命令自己的参数
/// （Go 版也是这样：`--pass` 是全局 flag，`relay` 只认 `--host`/`--ports`）。
pub struct RelayRequest {
    /// 子命令参数。
    pub args: crate::relay_cmd::RelayArgs,
    /// 中继密码（已解析过 `CROC_PASS` 与默认值）。
    pub password: String,
    /// 命令行上给了 `--ports` 吗。决定端口从 `--ports` 来还是从 `--port` 推。
    pub ports_explicit: bool,
}

/// 命令行参数，不含程序名。
///
/// 只给 `crate::help` 认 `-h` / `--help` 用。这里 `to_string_lossy` 是有意的：判帮助
/// 不需要精确的字节，而口令码里出现非 UTF-8 时也不该在写帮助这一步炸掉。
fn argv() -> Vec<String> {
    std::env::args_os().skip(1).map(|word| word.to_string_lossy().into_owned()).collect()
}

/// 解析 argv。
///
/// # Errors
///
/// 参数不合法（曲线名拼错、口令码太短、既没给子命令也没给口令码）时返回
/// [`ExitCode::FAILURE`]，并且已经把原因打到 stderr 上了。
pub fn parse() -> Result<Parsed, ExitCode> {
    // `-h` / `--help` 先自己接住：它要的版式与 clap 的渲染器不是一回事，见 [`crate::help`]。
    // 放在 clap 之前是因为那样连"参数不合法"都不必先过一遍——`croc-rs send -h --typo`
    // 在 Go 版里同样打帮助。
    if let Some(request) = help::request(&argv()) {
        help::print(request, Locale::detect());
        return Err(ExitCode::SUCCESS);
    }
    let matches = Cli::command().get_matches();
    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(error) => {
            let _ = error.print();
            return Err(ExitCode::FAILURE);
        }
    };
    let given = Given::new(&matches);

    // `--classic` 是动作：切完就结束。Go 在 `Before` 里处理完直接 `os.Exit(0)`，哪怕
    // 同一个命令行上还写了别的。
    if cli.global.classic {
        return Ok(Parsed { debug: cli.global.debug, action: Action::ToggleClassic });
    }
    // `--revoke` 也是"做完就完"的动作：Go 把它放在 `Before` 里。
    if let Some(id) = cli.global.revoke.clone() {
        return Ok(Parsed { debug: cli.global.debug, action: Action::Revoke(id) });
    }
    // 取件凭证走环境变量是推荐路径：命令行上的链接会进 shell 历史与进程表。
    if cli.command.is_none()
        && let Ok(token) = std::env::var("CROC_STORE_TOKEN")
        && !token.trim().is_empty()
    {
        let request = stored_request(&cli.global, token.trim().to_owned());
        return Ok(Parsed { debug: cli.global.debug, action: Action::ReceiveStored(Box::new(request)) });
    }
    // 这次读哪一侧的配置：发送读 `send.json`、接收读 `receive.json`（Go 就是两份）。
    // 带了 `--remember` 就不读旧配置：这一趟是要覆盖它（Go 的 `!c.Bool("remember")`）。
    let side = if cli.command.is_none() { Side::Receive } else { Side::Send };
    let remembered = if cli.global.remember { Remembered::default() } else { Remembered::load(side) };

    // `--throttleUpload` 的写错要在这里就报出来：等连上中继再报，用户已经等过一次握手。
    let throttle_upload = match parse_throttle(&cli.global.throttle_upload) {
        Ok(rate) => rate,
        Err(reason) => {
            eprintln!("{reason}");
            return Err(ExitCode::FAILURE);
        }
    };
    // `--remember` 与 `--internal-dns` 一起给就把它记下来：Go 用 <config>/internal-dns
    // 这个空文件记这件事。
    if cli.global.remember && cli.global.internal_dns {
        crate::config::remember_internal_dns(cli.global.quiet);
    }
    let internal_dns = cli.global.internal_dns || crate::config::internal_dns_enabled();
    let clipboard = clipboard_choice(&cli.global, &given, &remembered);

    let events = sink(&cli.global);
    let machine_id = machine_id();
    // classic 模式开着的话，命令行上的口令码是被允许的（Go 的 `classicInsecureMode`）。
    let classic = crate::config::classic_enabled();

    match cli.command {
        // 这两个子命令收下参数、打完提示就结束——它们是位置占位，不是待办。
        Some(Command::Ssh(_args)) => Ok(Parsed { debug: cli.global.debug, action: Action::Ssh }),
        Some(Command::Update(args)) => {
            Ok(Parsed { debug: cli.global.debug, action: Action::Update { check: args.check } })
        }
        Some(Command::GenerateFishCompletion) => {
            Ok(Parsed { debug: cli.global.debug, action: Action::GenerateFishCompletion })
        }
        Some(Command::Store(args)) => {
            let paths = match crate::store::regular_files(&args.paths) {
                Ok(paths) => paths,
                Err(reason) => {
                    eprintln!("croc-rs: {reason}");
                    return Err(ExitCode::FAILURE);
                }
            };
            if let Err(reason) = crate::store::validate_downloads(args.downloads) {
                eprintln!("croc-rs: {reason}");
                return Err(ExitCode::FAILURE);
            }
            let request =
                stored_upload(&cli.global, clipboard, paths, args.downloads, args.expiration, args.url, args.qrcode);
            Ok(Parsed { debug: cli.global.debug, action: Action::Store(Box::new(request)) })
        }
        Some(Command::Relay(args)) => {
            let password = cli.global.pass.clone();
            // `--ports` 给没给决定端口从哪儿来，与 Go 的 `c.IsSet("ports")` 一致。
            let ports_explicit = given.sub_given("ports");
            Ok(Parsed {
                debug: cli.global.debug,
                action: Action::Relay(Box::new(RelayRequest { args, password, ports_explicit })),
            })
        }
        Some(Command::Send(args)) => {
            // `--transport` 是 `send` 自己的开关（Go 也是），可能来自配置文件，所以先合出来
            // 再校验。接收端没有这个开关，所以这段只在发送分支里。
            let transport = effective_transport(&args, &given, &remembered);
            if let Err(reason) = validate_transport(cli.global.local, transport) {
                eprintln!("{reason}");
                return Err(ExitCode::FAILURE);
            }
            // 降级提示在 `--quiet` 下要吞掉——Go 的 `writeTailcatRelayFallbackWarning()`
            // 也是先看 `quiet`。
            if transport == Transport::Derp && !cli.global.quiet {
                eprintln!("{}", t!("cli.note.tailscale_pending"));
            }
            // `--store` 走的是另一条路：文件先加密上传到一个第三方，不经过中继。
            if args.store {
                if let Err(reason) = crate::store::validate_store_sources(
                    args.text.is_some(),
                    args.zip,
                    args.git,
                    &args.exclude,
                    &args.exclude_file,
                    cli.global.ignore_stdin,
                ) {
                    eprintln!("croc-rs: {reason}");
                    return Err(ExitCode::FAILURE);
                }
                let paths = match crate::store::regular_files(&args.paths) {
                    Ok(paths) => paths,
                    Err(reason) => {
                        eprintln!("croc-rs: {reason}");
                        return Err(ExitCode::FAILURE);
                    }
                };
                if let Err(reason) = crate::store::validate_downloads(args.store_downloads) {
                    eprintln!("croc-rs: {reason}");
                    return Err(ExitCode::FAILURE);
                }
                let request = stored_upload(
                    &cli.global,
                    clipboard,
                    paths,
                    args.store_downloads,
                    args.store_expiration,
                    args.store_url,
                    args.qrcode,
                );
                return Ok(Parsed { debug: cli.global.debug, action: Action::Store(Box::new(request)) });
            }
            // 命令行给的优先，`--remember` 存下来的只填空位。
            let options = merge_send(&cli.global, &args, &given, &remembered);
            // 先算出来：下面 `args.paths` 会被移走，之后就借不到整个 `args` 了。
            let save_remembered = cli.global.remember.then(|| remembered_for_send(&cli.global, &args, &given));
            let curve = match parse_curve(&options.curve) {
                Ok(curve) => curve,
                Err(reason) => {
                    eprintln!("{reason}");
                    return Err(ExitCode::FAILURE);
                }
            };
            let mut paths = args.paths;
            if let Err(reason) = validate_hash(&options.hash) {
                eprintln!("{reason}");
                return Err(ExitCode::FAILURE);
            }
            // `--text` 与 stdin 的落点文件名相同，只在这一行的显示上有区别。
            let sending_text = args.text.is_some();
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
            if !cli.global.ignore_stdin {
                // 管道进来的内容也算一个待发文件（Go 的默认行为）。
                match write_stdin_artifact() {
                    Ok(Some(path)) => paths.push(path),
                    Ok(None) => {}
                    Err(error) => {
                        eprintln!("croc-rs: {error}");
                        return Err(ExitCode::FAILURE);
                    }
                }
            }
            if let Err(reason) = validate_sources(&paths, None) {
                eprintln!("{reason}");
                return Err(ExitCode::FAILURE);
            }
            // 非 Windows 上拒命令行口令码，除非开了 classic 模式或给了 `CROC_SECRET`。
            if refuses_command_line_code(cfg!(windows), given.sub_on_cli("code"), classic, env_secret().is_some()) {
                eprintln!("{}", t!("cli.send_code_refused"));
                return Err(ExitCode::FAILURE);
            }
            // 与 Go 一样在开始传输之前写。
            if let Some(remembered) = save_remembered {
                remembered.save(Side::Send, cli.global.quiet);
            }
            // 走公共中继池时，"口令码"和"连哪台中继"是一件事的两半，必须一起定。
            let target = resolve_send_target(&options, &given, &cli.global, internal_dns);
            let (code, components, relay, relay6, public_relay) = match target {
                Ok(resolved) => resolved,
                Err(reason) => {
                    eprintln!("{reason}");
                    return Err(ExitCode::FAILURE);
                }
            };
            let generated_public_code = public_relay && options.code.is_none();
            let request = Box::new(SendRequest {
                relay,
                relay_password: options.password.clone(),
                components,
                code,
                curve,
                paths,
                hash_algorithm: options.hash.clone(),
                exclude: args.exclude,
                exclude_file: args.exclude_file,
                git: options.git,
                zip: args.zip,
                proxy: croc_engine::proxy::Proxy::from_flags(
                    cli.global.socks5.as_deref(),
                    cli.global.connect.as_deref(),
                ),
                multicast: cli.global.multicast,
                relay6,
                ip: cli.global.ip.clone(),
                ask: cli.global.ask,
                sending_text,
                internal_dns,
                throttle_upload,
                no_compress: cli.global.no_compress,
                local: options.local,
                relay_base_port: Some(args.port),
                // 已经由 clap 的 range 限到 1..=8。
                transfers: usize::from(args.transfers),
                no_multi: args.no_multi,
                machine_id,
                events,
            });
            let mut request = request;
            // --quiet 下一句话都不打，连口令码也不打：那是这个开关的字面意思，
            // 也是互通脚本敢直接把输出丢进日志的原因。
            if !cli.global.quiet {
                let notice = crate::notice::SendNotice::new(&request, public_relay, args.qrcode, clipboard);
                request.events = send_sink(Box::new(notice));
            }
            Ok(Parsed { debug: cli.global.debug, action: Action::Send { request, generated_public_code } })
        }
        None => {
            // 接收方的应用集比发送方小：Go 在 `receive()` 里手写的一段。
            let mut options = merge_receive(&cli.global, cli.code.as_deref(), &given, &remembered);
            // 非 Windows 上命令行口令码会被拒；这时环境变量是唯一出路，与 Go 一样先
            // 悄悄换掉它。
            if !cfg!(windows) && given.top_on_cli("code") && !classic {
                if let Some(secret) = env_secret() {
                    options.code = Some(secret);
                } else {
                    eprintln!("{}", t!("cli.receive_code_refused"));
                    return Err(ExitCode::FAILURE);
                }
            }
            // 一个码都没给就在终端上问一句（Go 的 `Enter receive code: `）。管道里不
            // 问：那种场合问下去会一直等，直接报错更清楚。
            let Some(code) = options.code.clone().or_else(prompt_receive_code) else {
                eprintln!("{}", t!("cli.need_code_or_subcommand"));
                return Err(ExitCode::FAILURE);
            };
            if cli.global.remember {
                remembered_for_receive(&cli.global, &code).save(Side::Receive, cli.global.quiet);
            }
            let curve = match parse_curve(&options.curve) {
                Ok(curve) => curve,
                Err(reason) => {
                    eprintln!("{reason}");
                    return Err(ExitCode::FAILURE);
                }
            };
            // 取件链接/令牌不是口令码：它自带解密密钥，走的也是另一条路。
            if croc_store::share::is_stored_value(&code) {
                // 非 Windows 上命令行里的链接会被进程表看见，而它就是密钥本身。
                // classic 模式开着时按用户意愿放行（与命令行口令码同一套判断）。
                if !cfg!(windows) && given.top_on_cli("code") && !classic {
                    eprintln!("{}", t!("cli.note.store_link_refused"));
                    return Err(ExitCode::FAILURE);
                }
                let request = stored_request(&cli.global, code);
                return Ok(Parsed { debug: cli.global.debug, action: Action::ReceiveStored(Box::new(request)) });
            }
            let components = match codephrase::parse(&code) {
                Ok(components) => components,
                Err(error) => {
                    eprintln!("croc-rs: {error}");
                    return Err(ExitCode::FAILURE);
                }
            };
            // 码里带着该连哪台公共中继的索引，所以地址要在解析完码之后才定下来。
            let (relay, relay6) = match resolve_receive_relay(&code, &options, &given, &cli.global) {
                Ok(pair) => pair,
                Err(reason) => {
                    eprintln!("{reason}");
                    return Err(ExitCode::FAILURE);
                }
            };
            // 回调总是注册：对端的 `Ask` 要到清单到达才知道，而它压得住本端的
            // `--yes`（Go 的 `!NoPrompt || Ask || senderInfo.Ask`）。
            let (yes, ask) = (options.yes, cli.global.ask);
            let prompt_machine_id = machine_id.clone();
            let confirm = Some(Arc::new(move |info: &croc_protocol::SenderInfo| {
                prompt_confirm(info, yes, ask, &prompt_machine_id)
            }) as Arc<dyn Fn(&croc_protocol::SenderInfo) -> bool + Send + Sync>);
            Ok(Parsed {
                debug: cli.global.debug,
                action: Action::Receive(Box::new(ReceiveRequest {
                    relay,
                    relay_password: options.password.clone(),
                    components,
                    curve,
                    out_dir: cli.global.out,
                    preferences: Preferences { overwrite: options.overwrite, rename: options.rename },
                    no_compress: cli.global.no_compress,
                    local: options.local,
                    proxy: croc_engine::proxy::Proxy::from_flags(
                        cli.global.socks5.as_deref(),
                        cli.global.connect.as_deref(),
                    ),
                    multicast: cli.global.multicast,
                    relay6,
                    ip: cli.global.ip.clone(),
                    stdout: cli.global.stdout,
                    internal_dns,
                    // `--no-multi` 只挂在 `send` 上（与 Go 一致），接收端不能给，
                    // 它按中继 banner 里的端口数走。
                    no_multi: false,
                    machine_id,
                    confirm,
                    events,
                })),
            })
        }
    }
}

/// 组装一个暂存上传请求。
fn stored_upload(
    global: &Global,
    clipboard: Clipboard,
    paths: Vec<std::path::PathBuf>,
    downloads: i64,
    expiration: String,
    url: String,
    qrcode: bool,
) -> crate::store::StoreRequest {
    crate::store::StoreRequest {
        paths,
        downloads,
        expiration,
        url,
        quiet: global.quiet,
        clipboard,
        qrcode,
        proxy: croc_store::proxy::Proxy::from_flags(global.socks5.as_deref(), global.connect.as_deref()),
    }
}

/// 组装一个暂存接收请求。
fn stored_request(global: &Global, token: String) -> crate::store::StoreReceiveRequest {
    crate::store::StoreReceiveRequest {
        token,
        out: global.out.clone(),
        overwrite: global.overwrite,
        yes: global.yes,
        quiet: global.quiet,
        proxy: croc_store::proxy::Proxy::from_flags(global.socks5.as_deref(), global.connect.as_deref()),
    }
}

/// 跑起来。
pub async fn run(parsed: Parsed) -> ExitCode {
    let cancel = CancellationToken::new();
    spawn_signal_handler(cancel.clone());

    match parsed.action {
        // 人机交互动作，与取消令牌无关。
        Action::ToggleClassic => toggle_classic(),
        // 两个占位命令：说清"没做"，并以失败码退出——脚本不该把"什么都没发生"读成"做完了"。
        Action::Ssh => {
            eprintln!("{}", t!("cli.note.ssh_unimplemented"));
            ExitCode::FAILURE
        }
        Action::Update { check } => {
            let key = if check { "cli.note.update_check_unimplemented" } else { "cli.note.update_unimplemented" };
            eprintln!("{}", t!(key));
            ExitCode::FAILURE
        }
        Action::GenerateFishCompletion => {
            eprintln!("{}", t!("cli.note.fish_completion_unimplemented"));
            ExitCode::FAILURE
        }
        Action::Relay(request) => {
            crate::relay_cmd::run(request.args, &request.password, request.ports_explicit, cancel).await
        }
        Action::Store(request) => crate::store::send(*request).await,
        Action::ReceiveStored(request) => crate::store::receive(*request).await,
        Action::Revoke(id) => crate::store::revoke(&id).await,
        Action::Send { request, generated_public_code } => {
            match croc_engine::send(*request, cancel).await {
                Ok(summary) => {
                    let size = crate::notice::byte_count_decimal(summary.bytes);
                    let key = if summary.files == 1 { "cli.sent_one" } else { "cli.sent_many" };
                    eprintln!("{}", t!(key, size = size, count = summary.files));
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    // 只在"连不上中继"时清缓存：对端拒绝、磁盘满这些跟选哪台中继无关，
                    // 清了只是让下次白白重探一遍。
                    if generated_public_code && error.is_retryable() {
                        crate::config::clear_best_public_relay();
                    }
                    fail(error)
                }
            }
        }
        Action::Receive(request) => match croc_engine::receive(*request, cancel).await {
            Ok(summary) => {
                println!(
                    "{}",
                    t!(
                        "cli.received_summary",
                        received = summary.received,
                        bytes = summary.bytes,
                        skipped = summary.skipped,
                        declined = summary.declined
                    )
                );
                ExitCode::SUCCESS
            }
            Err(error) => fail(error),
        },
    }
}

fn fail(error: croc_engine::EngineError) -> ExitCode {
    if matches!(error, croc_engine::EngineError::Cancelled) {
        eprintln!("{}", t!("cli.cancelled"));
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

/// 发送端的事件出口。
///
/// 口令码不在这里之外的地方打印：Go 是在 `Client.Send()` 内部、清点完文件之后才
/// 打的，所以文件不存在的场合根本不会有码出现。跟着
/// [`EngineEvent::Sending`] 走就能复现这个顺序。
fn send_sink(notice: Box<crate::notice::SendNotice>) -> EventSink {
    use crate::locale::Locale;
    use crate::notice::byte_count_decimal;
    use std::sync::atomic::{AtomicBool, Ordering};

    let locale = Locale::detect();
    // 只打一次：`Sending` 现在只会发一次，但重连等路径将来可能再发。
    let announced = Arc::new(AtomicBool::new(false));
    Arc::new(move |event: EngineEvent| match event {
        EngineEvent::Sending { name, files, folders, bytes } => {
            if announced.swap(true, Ordering::Relaxed) {
                return;
            }
            crate::notice::print_sending(locale, name.as_deref(), files, folders, bytes);
            notice.print(locale);
        }
        EngineEvent::Status(message) => eprintln!("{message}"),
        EngineEvent::FileStarted { name, size } => {
            eprintln!(
                "{}",
                t!("cli.sending_file", locale = locale.tag(), name = name, size = byte_count_decimal(size))
            );
        }
        EngineEvent::Progress { transferred, total } => {
            if let Some(percent) = transferred.saturating_mul(100).checked_div(total) {
                eprint!("\r{percent}%  ");
                if transferred >= total {
                    eprintln!();
                }
            }
        }
        _ => {}
    })
}

/// 进度事件出口。
fn sink(global: &Global) -> EventSink {
    if global.quiet {
        return silent_sink();
    }
    Arc::new(|event: EngineEvent| match event {
        EngineEvent::Status(message) => eprintln!("{message}"),
        EngineEvent::FileStarted { name, size } => eprintln!("{}", t!("cli.receiving_file", name = name, bytes = size)),
        EngineEvent::FileDone { name } => eprintln!("{}", t!("cli.file_done", name = name)),
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
/// `Err` 里是已经可以整行打到 stderr 的字符串。抽成纯函数是为了能脱开
/// `std::env::args()` 测这两条规则。
fn validate_transport(local: bool, transport: Transport) -> Result<(), String> {
    // 这条与 Go 版同文。先查它，免得用户同时遇到两条报错
    // 却只看到第二条。
    if local && transport != Transport::Auto {
        return Err(t!("cli.error.transport_conflict").into_owned());
    }
    Ok(())
}

/// `send` 至少要有个东西可发：一个路径，或者一段 `--text`。
fn validate_sources(paths: &[PathBuf], text: Option<&str>) -> Result<(), String> {
    if paths.is_empty() && text.is_none() {
        return Err(t!("cli.error.send_needs_source").into_owned());
    }
    Ok(())
}

/// `--hash` 的算法名必须提前校验。
///
/// 放在这里而不是等 `files::collect` 报错：几万个文件的哈希要算十几秒，让用户在
/// 那之后才看到"算法名拼错了"是很糟糕的体验。
///
/// 与 Go 的 `receiveHashAlgorithm`同一组名字，少了
/// `imohash-v2`——那是渐进式哈希的协商项，本实现没有谈那条能力。
fn validate_hash(algorithm: &str) -> Result<(), String> {
    if croc_engine::hash::ALGORITHMS.contains(&algorithm) {
        return Ok(());
    }
    Err(t!("cli.error.hash_unsupported", algorithm = algorithm, list = croc_engine::hash::ALGORITHMS.join(" / "))
        .into_owned())
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
    let path = temp_artifact_path();
    std::fs::write(&path, text).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(path)
}

/// 把管道进来的 stdin 落到临时文件。
///
/// 只在 stdin 不是终端时才读——交互式终端下读它会把 `croc-rs send a.txt` 卡在等
/// 用户敲 EOF 上。空的 stdin 直接跳过：发一个零字节文件没有意义（Go 会照发）。
///
/// # Errors
///
/// stdin 读不动，或者临时文件写不下去。
fn write_stdin_artifact() -> Result<Option<PathBuf>, String> {
    use std::io::{IsTerminal as _, Read as _};

    if std::io::stdin().is_terminal() {
        return Ok(None);
    }
    let mut buffer = Vec::new();
    std::io::stdin().read_to_end(&mut buffer).map_err(|error| format!("reading stdin: {error}"))?;
    if buffer.is_empty() {
        return Ok(None);
    }
    let path = temp_artifact_path();
    std::fs::write(&path, &buffer).map_err(|error| format!("{}: {error}", path.display()))?;
    Ok(Some(path))
}

/// 一个 `croc-stdin-<纳秒时间戳>` 的临时文件路径。
///
/// 名字前缀是协议的一部分：接收方靠它识别"这是 `--text` / 管道来的东西"。
fn temp_artifact_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("croc-stdin-{stamp}"))
}

/// 定下口令码与中继地址。
///
/// 走公共中继池时这两者是一件事的两半：码的 SHA-256 对池长取模就是索引，对端拿到
/// 同一个码就能算出同一台中继。所以不能先随机一个码再随便挑一台——那样对端按码算出的
/// 索引指向别的中继，两端各自在正确的房间里等一个永远不来的人。
///
/// 返回 `(码, 码解出的房间与 PAKE 口令, 中继, IPv6 中继, 是不是走的池)`。走池时
/// IPv6 中继置空：池里只按码选一台，再给一个 IPv6 候选会连到别的机器上去。
///
/// # Errors
///
/// 码不合法、池探不到，或者定下索引后生成不出落在它上面的码。
fn resolve_send_target(
    options: &SendOptions,
    given: &Given<'_>,
    global: &Global,
    internal_dns: bool,
) -> Result<(String, Components, String, String, bool), String> {
    let public = relay_pool::applies(
        given.top_given("relay"),
        given.top_given("relay6"),
        options.local,
        &global.ip,
        &options.relay,
        &options.relay6,
    );
    if !public {
        let (code, components) = generate_code(options.code.as_deref()).map_err(|error| format!("croc-rs: {error}"))?;
        return Ok((code, components, options.relay.clone(), options.relay6.clone(), false));
    }
    let proxy = croc_engine::proxy::Proxy::from_flags(global.socks5.as_deref(), global.connect.as_deref());
    // 码已经给定：索引由码决定，没得挑。否则探测一遍挑一台最先应答的。
    let index = match options.code.as_deref() {
        Some(code) => relay_pool::index_of(code)?,
        None => relay_pool::select(&proxy, internal_dns, global.quiet)?,
    };
    let relay = relay_pool::address(index)?.to_owned();
    let code = if let Some(code) = options.code.as_deref() {
        code.to_owned()
    } else {
        // 反着来：先定索引，再找一个哈希正好落在这个索引上的码。
        let count = relay_pool::count();
        let wanted = u32::try_from(index).map_err(|_| t!("cli.error.relay_index_out_of_range", index = index))?;
        codephrase::generate_for_relay(&mut croc_net::OsRandomness, wanted, count)
            .map_err(|error| format!("croc-rs: {error}"))?
    };
    let (code, components) = generate_code(Some(&code)).map_err(|error| format!("croc-rs: {error}"))?;
    Ok((code, components, relay, String::new(), true))
}

/// 接收端按码挑中继。
///
/// 与发送端用同一个 [`relay_pool`]，所以两端算出来的地址必然一致。
///
/// # Errors
///
/// 码不合法或索引越界。
fn resolve_receive_relay(
    code: &str,
    options: &ReceiveOptions,
    given: &Given<'_>,
    global: &Global,
) -> Result<(String, String), String> {
    if !relay_pool::applies(
        given.top_given("relay"),
        given.top_given("relay6"),
        options.local,
        &global.ip,
        &options.relay,
        &options.relay6,
    ) {
        return Ok((options.relay.clone(), options.relay6.clone()));
    }
    let index = relay_pool::index_of(code)?;
    Ok((relay_pool::address(index)?.to_owned(), String::new()))
}

/// 解析 `--throttleUpload`，返回字节/秒。
///
/// 语义与 Go 一致：末位是 `k` / `m` / `g`（大小写都算）就按 1024
/// 进位，否则整个字符串当字节数。长度不足 2 的输入等于没给——Go 的
/// `len(...) > 1` 就是这个意思，所以 `--throttle-upload 5` 在 Go 那边也不限速。
///
/// 与 Go 的差别只在写错时的表现：Go 直接 `panic`（退出码 2 + 一段栈），这里给一行
/// 能读的报错。
fn parse_throttle(value: &str) -> Result<Option<u64>, String> {
    let text = value.trim();
    if text.len() < 2 {
        return Ok(None);
    }
    let (digits, multiplier) = text
        .strip_suffix(['g', 'G'])
        .map(|digits| (digits, 1024_u64 * 1024 * 1024))
        .or_else(|| text.strip_suffix(['m', 'M']).map(|digits| (digits, 1024 * 1024)))
        .or_else(|| text.strip_suffix(['k', 'K']).map(|digits| (digits, 1024)))
        .unwrap_or((text, 1));
    let invalid = || t!("cli.error.throttle_invalid", value = value);
    let number: u64 = digits.trim().parse().map_err(|_| invalid())?;
    let rate = number.checked_mul(multiplier).filter(|rate| *rate > 0).ok_or_else(invalid)?;
    Ok(Some(rate))
}

/// 参数是命令行（或环境变量）给的，还是 clap 填的默认值。
///
/// `--remember` 存下来的值只填空位，判据就是它——对应 Go 的 `c.IsSet(name)`。
/// 环境变量也算「给了」：`CROC_RELAY` 与 `--relay` 是同一个开关的两条路。
///
/// 查询分顶层与子命令两层，调用方按参数的归属选：`--relay` / `--curve` 这些在
/// 顶层，`--code` / `--hash` / `--git` 挂在 `send` 上。`ArgMatches` 只认识自己那一层声明
/// 过的 id，查错层在 debug 下 panic、release 下静默当作「没给」——后者会把用户给的
/// 口令码丢掉、另生成一个，两端于是永远握不上手。
struct Given<'a> {
    top: &'a ArgMatches,
    sub: Option<&'a ArgMatches>,
}

impl<'a> Given<'a> {
    fn new(top: &'a ArgMatches) -> Self {
        Self { top, sub: top.subcommand().map(|(_, matches)| matches) }
    }

    /// 顶层参数给了吗（命令行或环境变量）。
    fn top_given(&self, id: &str) -> bool {
        self.top.value_source(id).is_some_and(|source| source != ValueSource::DefaultValue)
    }

    /// 顶层参数是命令行给的吗。安全相关的判断只看它：环境变量不会进 shell 历史。
    fn top_on_cli(&self, id: &str) -> bool {
        self.top.value_source(id) == Some(ValueSource::CommandLine)
    }

    /// `send` 子命令的参数给了吗。没有子命令时恒为假。
    fn sub_given(&self, id: &str) -> bool {
        self.sub.and_then(|matches| matches.value_source(id)).is_some_and(|source| source != ValueSource::DefaultValue)
    }

    /// `send` 子命令的参数是命令行给的吗。
    fn sub_on_cli(&self, id: &str) -> bool {
        self.sub.and_then(|matches| matches.value_source(id)) == Some(ValueSource::CommandLine)
    }
}

/// 环境变量里的口令码。
fn env_secret() -> Option<String> {
    std::env::var("CROC_SECRET").ok().filter(|secret| !secret.is_empty())
}

/// 「命令行给了就用它；没给就用记住的；记住的是空的就退回默认」。
fn pick(given: bool, remembered: &str, fallback: &str) -> String {
    if given || remembered.is_empty() { fallback.to_owned() } else { remembered.to_owned() }
}

/// 记住的值非空才算数。
fn non_empty(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

/// 发送方这次真正要用的选项。
struct SendOptions {
    relay: String,
    relay6: String,
    password: String,
    curve: String,
    code: Option<String>,
    hash: String,
    git: bool,
    local: bool,
}

/// 发送方的合并：Go 的 `applyRememberedSendOptions`。
fn merge_send(cli: &Global, args: &SendArgs, given: &Given<'_>, remembered: &Remembered) -> SendOptions {
    let mut relay = cli.relay.clone();
    let mut relay6 = cli.relay6.clone();
    // 只试用户指定的那一个：两个都当候选时会连上他没指定的中继，那边的房间是另一套。
    // 接收端没有这一步，这个不对称是刻意的。
    if relay != DEFAULT_RELAY {
        relay6.clear();
    } else if relay6 != DEFAULT_RELAY6 {
        relay.clear();
    }
    if !given.top_given("relay")
        && let Some(address) = decode_relay(&remembered.relay_address)
    {
        relay = address.to_owned();
    }
    if !given.top_given("relay6")
        && let Some(address) = decode_relay(&remembered.relay_address6)
    {
        relay6 = address.to_owned();
    }
    SendOptions {
        relay,
        relay6,
        password: pick(given.top_given("pass"), &remembered.relay_password, &cli.pass),
        curve: pick(given.top_given("curve"), &remembered.curve, &cli.curve),
        hash: pick(given.sub_given("hash"), &remembered.hash_algorithm, &args.hash),
        // 三档，后给的不覆盖先给的：命令行 `--code` → 上次记住的 → `CROC_SECRET`。
        code: env_secret()
            .or_else(|| if given.sub_given("code") { args.code.clone() } else { non_empty(&remembered.shared_secret) }),
        git: if given.sub_given("git") { args.git } else { remembered.git_ignore },
        local: if given.top_given("local") { cli.local } else { remembered.only_local },
    }
}

/// 接收方这次真正要用的选项。
struct ReceiveOptions {
    relay: String,
    relay6: String,
    password: String,
    curve: String,
    code: Option<String>,
    yes: bool,
    overwrite: bool,
    rename: bool,
    local: bool,
}

/// 接收方的合并。Go 在 `receive()` 里手写的一段，应用集比发送方小：
/// 少了 `--hash` / `--git` / `--transport` / `--disable-clipboard` / `--no-local`。
fn merge_receive(cli: &Global, code: Option<&str>, given: &Given<'_>, remembered: &Remembered) -> ReceiveOptions {
    let mut relay = cli.relay.clone();
    let mut relay6 = cli.relay6.clone();
    if !given.top_given("relay")
        && let Some(address) = decode_relay(&remembered.relay_address)
    {
        relay = address.to_owned();
    }
    if !given.top_given("relay6")
        && let Some(address) = decode_relay(&remembered.relay_address6)
    {
        relay6 = address.to_owned();
    }
    ReceiveOptions {
        relay,
        relay6,
        password: pick(given.top_given("pass"), &remembered.relay_password, &cli.pass),
        curve: pick(given.top_given("curve"), &remembered.curve, &cli.curve),
        // 命令行的口令码只可能从位置参数或 `CROC_SECRET` 来，没给就是 `None`。
        code: code.map(ToOwned::to_owned).or_else(|| non_empty(&remembered.shared_secret)),
        // 接收端会记 `--yes`（Go 的 `NoPrompt`），发送端不记。
        yes: if given.top_given("yes") { cli.yes } else { remembered.no_prompt },
        overwrite: if given.top_given("overwrite") { cli.overwrite } else { remembered.overwrite },
        rename: if given.top_given("rename") { cli.rename } else { remembered.rename },
        local: if given.top_given("local") { cli.local } else { remembered.only_local },
    }
}

/// 写发送方的配置。
///
/// 口令码只在命令行确实给了时才写：不然会把一个自动生成的一次性码存下来，下次照用。
fn remembered_for_send(cli: &Global, args: &SendArgs, given: &Given<'_>) -> Remembered {
    Remembered {
        shared_secret: if given.sub_given("code") { args.code.clone().unwrap_or_default() } else { String::new() },
        relay_address: encode_relay(&cli.relay, DEFAULT_RELAY),
        relay_address6: encode_relay(&cli.relay6, DEFAULT_RELAY6),
        relay_password: cli.pass.clone(),
        no_prompt: cli.yes,
        overwrite: cli.overwrite,
        rename: cli.rename,
        curve: cli.curve.clone(),
        only_local: cli.local,
        hash_algorithm: args.hash.clone(),
        git_ignore: args.git,
        disable_clipboard: cli.disable_clipboard,
        disable_local: args.no_local,
        transport: transport_name(args.transport).to_owned(),
    }
}

/// 写接收方的配置。
///
/// 接收端存的口令码是这次实际用的那个（可能来自命令行、环境变量或上次记住的）。
fn remembered_for_receive(cli: &Global, code: &str) -> Remembered {
    Remembered {
        shared_secret: code.to_owned(),
        relay_address: encode_relay(&cli.relay, DEFAULT_RELAY),
        relay_address6: encode_relay(&cli.relay6, DEFAULT_RELAY6),
        relay_password: cli.pass.clone(),
        no_prompt: cli.yes,
        overwrite: cli.overwrite,
        rename: cli.rename,
        curve: cli.curve.clone(),
        only_local: cli.local,
        ..Remembered::default()
    }
}

/// 这次真正要用的 `--transport`。
///
/// 它挂在 `send` 上（与 Go 一致），且可能来自配置文件（只有发送端记它），
/// 所以既不能只看顶层，也不能只看 `args.transport`。
fn effective_transport(args: &SendArgs, given: &Given<'_>, remembered: &Remembered) -> Transport {
    if given.sub_given("transport") {
        return args.transport;
    }
    remembered_transport(&remembered.transport).unwrap_or(args.transport)
}

/// 口令码要不要进剪贴板，进的话放什么。
///
/// `--disable-clipboard` 会跟着 `--remember` 走，`--extended-clipboard` 不会
/// （Go 的记住项里也没有它）。
fn clipboard_choice(cli: &Global, given: &Given<'_>, remembered: &Remembered) -> Clipboard {
    // id 是字段名 `disable_clipboard`，不是它对应的长选项名 `--disable-clipboard`：
    // 写成长选项名在 debug 下 panic、release 下被当成"没给"，于是 `--disable-clipboard`
    // 会被静默忽略。
    let disabled =
        if given.top_given("disable_clipboard") { cli.disable_clipboard } else { remembered.disable_clipboard };
    if disabled {
        Clipboard::Off
    } else if cli.extended_clipboard {
        Clipboard::Command
    } else {
        Clipboard::Code
    }
}

/// `--transport` 写进配置时用的名字。Go 的 `TransportMode` 也是这三个字符串。
const fn transport_name(transport: Transport) -> &'static str {
    match transport {
        Transport::Auto => "auto",
        Transport::Relay => "relay",
        Transport::Derp => "derp",
    }
}

/// 读回配置里的 `--transport`。认不出来就当没记过。
fn remembered_transport(name: &str) -> Option<Transport> {
    <Transport as clap::ValueEnum>::from_str(name, true).ok()
}

/// 解析 `--curve`。拼错时把可用曲线一起列出来。
fn parse_curve(name: &str) -> Result<CurveKind, String> {
    CurveKind::from_name(name).map_err(|error| {
        format!(
            "croc-rs: {error}
可用曲线：{}",
            CurveKind::ALL.map(CurveKind::name).join(", ")
        )
    })
}

/// 命令行上的口令码要不要拒。
///
/// Go 的 `shouldExitForUnixSendCode`：非 Windows、命令行给了码、没开
/// classic 模式、环境变量也没给。Windows 上不拒——命令行在那儿本来就是常态。
#[must_use]
const fn refuses_command_line_code(
    is_windows: bool,
    from_command_line: bool,
    classic: bool,
    has_env_secret: bool,
) -> bool {
    !is_windows && from_command_line && !classic && !has_env_secret
}

/// `--classic` 的提示语。默认答案是否——这个开关削弱安全性，不该被默认确认。
#[must_use]
fn classic_prompt(enabled: bool) -> String {
    let key = if enabled { "cli.classic_prompt_on" } else { "cli.classic_prompt_off" };
    t!(key).into_owned()
}

/// 切换成功之后的说明。
#[must_use]
fn classic_notice(enabled: bool) -> String {
    let key = if enabled { "cli.classic_notice_on" } else { "cli.classic_notice_off" };
    t!(key).into_owned()
}

/// `--classic`：切换 classic 模式。
///
/// 答完无论哪一边都以成功退出（Go 也是 `os.Exit(0)`）：这是一次人机交互，不是一次
/// 传输，用户选择保持原样不算失败。
fn toggle_classic() -> ExitCode {
    use std::io::{BufRead as _, Write as _};

    let enabled = crate::config::classic_enabled();
    eprint!("{}", classic_prompt(enabled));
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return ExitCode::FAILURE;
    }
    if matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        crate::config::set_classic(!enabled, false);
        print!("{}", classic_notice(enabled));
    } else {
        let state = if enabled { t!("cli.classic_state_on") } else { t!("cli.classic_state_off") };
        println!("{}", t!("cli.classic_unchanged", state = state));
    }
    ExitCode::SUCCESS
}

/// 交互式问接收口令码。只在 stdin 是终端时问——脚本里问会一直等下去。
fn prompt_receive_code() -> Option<String> {
    use std::io::{BufRead as _, IsTerminal as _, Write as _};

    if !std::io::stdin().is_terminal() {
        return None;
    }
    eprint!("{}", t!("cli.prompt_receive_code"));
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).ok()?;
    let code = line.trim().to_owned();
    (!code.is_empty()).then_some(code)
}

/// 交互式确认。
fn prompt_confirm(info: &croc_protocol::SenderInfo, yes: bool, ask: bool, local_machine_id: &str) -> bool {
    use std::io::{BufRead as _, Write as _};
    // 谁都没要求问就直接收：本端 `--yes`、也没给 `--ask`、对端也没要求。
    if yes && !ask && !info.ask {
        return true;
    }
    let total: i64 = info.files_to_transfer.iter().map(|entry| entry.size).sum();
    // 双方都要求核对身份时，把自己是谁也打出来——不然两个人对着同一个提示都不知道
    // 问的是哪一台。
    if ask || info.ask {
        eprintln!("{}", t!("cli.machine_id", id = local_machine_id));
    }
    let peer =
        if info.machine_id.is_empty() { t!("cli.unknown_machine").into_owned() } else { info.machine_id.clone() };
    eprint!("{}", t!("cli.confirm_receive", peer = peer, count = info.files_to_transfer.len(), bytes = total));
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
            eprintln!("{}", t!("cli.interrupt"));
            cancel.cancel();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// clap 接受的开关名（主名）。隐藏开关不算——它们本来就不该出现在帮助里。
    fn long_names(args: impl Iterator<Item = clap::Arg>) -> BTreeSet<String> {
        args.filter(|arg| !arg.is_hide_set()).filter_map(|arg| arg.get_long().map(ToOwned::to_owned)).collect()
    }

    /// 帮助表里列出的开关名。
    fn listed(topic: help::Topic) -> BTreeSet<String> {
        topic.listed_flags().into_iter().map(ToOwned::to_owned).collect()
    }

    /// 帮助表与 clap 的开关集必须一致。
    ///
    /// 帮助是手写的一张表（为了与 Go 版版式逐字一致），clap 才是真正解析参数的地方；
    /// 两者一旦不一致，症状要么是"帮助里写了但给了报未知参数"，要么是"能用的开关在帮助
    /// 里查不到"。这条双向核对。
    #[test]
    fn the_help_table_and_the_parser_agree_on_every_flag() {
        let root = Cli::command();
        let global = long_names(root.get_arguments().cloned());

        // 顶层：表里除了 `help`（由 `crate::help` 自己接，clap 这一层没有）之外，
        // 应当与 clap 的开关集完全相同。
        assert_eq!(
            listed(help::Topic::App).difference(&BTreeSet::from(["help".to_owned()])).cloned().collect::<BTreeSet<_>>(),
            global
        );

        for topic in [
            help::Topic::Send,
            help::Topic::Store,
            help::Topic::Ssh,
            help::Topic::Update,
            help::Topic::Relay,
            help::Topic::FishCompletion,
        ] {
            let name = topic.clap_name().expect("子命令都有名字");
            let command = root.find_subcommand(name).unwrap_or_else(|| panic!("clap 里没有 {name}"));
            let own = long_names(command.get_arguments().cloned());
            // `--help` 由 `crate::help` 自己接，clap 这一层没有它；核对时从表里摘掉。
            let table: BTreeSet<String> =
                listed(topic).difference(&BTreeSet::from(["help".to_owned()])).cloned().collect();
            // 表里写的每一个开关都必须真能解析：或者在子命令上，或者是全局的
            //（Go 的 `send` / `store` 也把 `--socks5` / `--connect` 列在自己的 OPTIONS 里）。
            let accepted: BTreeSet<String> = own.union(&global).cloned().collect();
            let unknown: Vec<_> = table.difference(&accepted).collect();
            assert!(unknown.is_empty(), "{name} 的帮助里写了 clap 不认的开关：{unknown:?}");
            // 反过来：子命令自己的开关一个都不能漏（全局的可以不重复列）。
            let missing: Vec<_> = own.difference(&table).filter(|flag| flag.as_str() != "version").collect();
            assert!(missing.is_empty(), "{name} 的开关没写进帮助：{missing:?}");
        }
    }

    /// 每页帮助都要列出开关，且不带重复。
    ///
    /// 开关表只有一份（语言无关），所以这里查的是"表本身没有写漏/写重"。
    #[test]
    fn every_page_lists_its_flags_once() {
        for topic in [
            help::Topic::App,
            help::Topic::Send,
            help::Topic::Store,
            help::Topic::Ssh,
            help::Topic::Update,
            help::Topic::Relay,
            help::Topic::FishCompletion,
        ] {
            let listed = topic.listed_flags();
            assert!(!listed.is_empty(), "{topic:?}: 一页帮助不能没有开关");
            let unique = listed.iter().collect::<BTreeSet<_>>();
            assert_eq!(unique.len(), listed.len(), "{topic:?} 列了重复的开关：{listed:?}");
        }
    }

    /// `--transport` / `--no-local` / `--no-multi` 在 Go 版里只挂在 `send` 上。
    ///
    /// 放到顶层会让 `-h` 的全局区多出三条、与 Go 版对不上，所以这条也一并核对。
    #[test]
    fn the_send_only_flags_stay_on_send() {
        let root = Cli::command();
        ["transport", "no-local", "no-multi"].iter().for_each(|flag| {
            assert!(root.get_arguments().all(|arg| arg.get_long() != Some(flag)), "`--{flag}` 不该出现在顶层");
        });
        let send = root.find_subcommand("send").expect("有 send");
        ["transport", "no-local", "no-multi"].iter().for_each(|flag| {
            assert!(send.get_arguments().any(|arg| arg.get_long() == Some(flag)), "`--{flag}` 应该在 send 上");
        });
    }

    /// `Given` 查过的那些 clap id。
    ///
    /// clap 的 id 是字段名（`disable_clipboard`），不是长选项名
    /// （`--disable-clipboard`）。写错一个字的后果不对称：debug 下 `value_source` 直接
    /// panic，release 下静默返回 `None`、被当成"没给"。这条测试逐条核对它们真的存在。
    const QUERIED_IDS: &[&str] = &[
        "relay",
        "relay6",
        "pass",
        "curve",
        "local",
        "yes",
        "overwrite",
        "rename",
        "disable_clipboard",
        "code",
        "hash",
        "git",
        "transport",
        "ports",
    ];

    #[test]
    fn the_ids_we_query_are_real_clap_ids() {
        let root = Cli::command();
        let known: BTreeSet<String> = std::iter::once(&root)
            .chain(root.get_subcommands())
            .flat_map(clap::Command::get_arguments)
            .map(|arg| arg.get_id().to_string())
            .collect();
        for id in QUERIED_IDS {
            assert!(known.contains(*id), "`{id}` 不是任何 clap 参数的 id；能用的有：{known:?}");
        }
    }

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
        // 文案是我们与用户的契约（`docs/agent/cli.md` §1.4），改它要同步改文档。
        // 断言钉在英文那一栏上：它是与文档对齐的那一版。
        let note = t!("cli.note.tailscale_pending", locale = "en");
        assert!(note.starts_with("note: "), "降级提示必须以 note: 开头");
        assert!(note.contains("tailcat-rs"), "要说清 pending 的对象");
        assert!(note.ends_with("using relay"), "要说清实际走了什么");
    }

    #[test]
    fn local_only_is_accepted_but_still_needs_auto_transport() {
        // `--local` 现在真的能用了：不该再报"未实现"。
        assert_eq!(validate_transport(true, Transport::Auto), Ok(()));
        // 但它仍然与显式的 `--transport` 互斥——Go 版同文。
        assert!(validate_transport(true, Transport::Relay).is_err());
    }

    #[test]
    fn local_conflicts_with_an_explicit_transport_like_go() {
        // Go 版原文：`--transport must be auto for local-only transfers`。
        for transport in [Transport::Relay, Transport::Derp] {
            let error = validate_transport(true, transport).expect_err("应报互斥");
            assert!(error.contains("--transport must be auto for local-only transfers"), "{error}");
        }
    }

    #[test]
    fn send_subcommand_flags_count_as_given() {
        // `--code` / `--hash` / `--git` 的来源记在子命令的 matches 里。只查顶层会把
        // `--code` 读成默认值，发送方于是丢掉用户给的口令码、另生成一个——两端永远
        // 握不上手，表现为双方都卡住等对方。
        let matches = Cli::command().get_matches_from(["croc-rs", "send", "--code", "acid-acorn-acre", "--git", "x"]);
        let given = Given::new(&matches);
        assert!(given.sub_given("code"), "--code 是命令行给的");
        assert!(given.sub_on_cli("code"));
        assert!(given.sub_given("git"), "--git 是命令行给的");
        // 没给的仍然算没给（`--hash` 有默认值）。
        assert!(!given.sub_given("hash"));
        assert!(!given.sub_given("exclude"));
        // 顶层参数照旧。
        let matches = Cli::command().get_matches_from(["croc-rs", "--relay", "10.0.0.1:9100", "code11"]);
        let given = Given::new(&matches);
        assert!(given.top_given("relay"));
        assert!(given.top_on_cli("code"), "接收时口令码是顶层位置参数");
        assert!(!given.sub_given("code"), "没有子命令时子命令那一层恒为未给");
    }

    #[test]
    fn only_the_command_line_is_refused() {
        // Go 的 `shouldExitForUnixSendCode`四个条件全中才拒。
        assert!(refuses_command_line_code(false, true, false, false));
        // Windows 上命令行本来就是常态，拒了等于不能用。
        assert!(!refuses_command_line_code(true, true, false, false));
        // 环境变量给的口令码不会进 shell 历史与进程表。
        assert!(!refuses_command_line_code(false, false, false, false));
        // 环境变量里有码时，Go 是悄悄换掉它而不是报错。
        assert!(!refuses_command_line_code(false, true, false, true));
        // classic 模式就是为这条路留的。
        assert!(!refuses_command_line_code(false, true, true, false));
    }

    #[test]
    fn the_classic_prompt_defaults_to_no() {
        // 提示以 (y/N) 收尾：这是个削弱安全性的开关，回车应当等于放弃。
        // 断言刻意不碰具体措辞——那是 `locales/` 的事，两种语言各写各的。
        for enabled in [true, false] {
            let prompt = classic_prompt(enabled);
            assert!(prompt.ends_with("(y/N) "), "{prompt}");
            assert!(prompt.contains("classic"), "{prompt}");
        }
        // 开着时问的是"要不要关"，关着时问的是"要不要开"，两种语言都得说清方向。
        assert!(classic_notice(true).contains("CROC_SECRET"));
        assert!(classic_notice(false).contains("--code"));
    }

    #[test]
    fn remembered_values_only_fill_the_gaps() {
        // 命令行给了就用命令行的；没给才用记住的；记住的是空的就退回默认。
        assert_eq!(pick(true, "remembered", "cli"), "cli");
        assert_eq!(pick(false, "remembered", "cli"), "remembered");
        assert_eq!(pick(false, "", "cli"), "cli");
        assert_eq!(non_empty(""), None);
        assert_eq!(non_empty("x"), Some("x".to_owned()));
    }

    #[test]
    fn the_transport_name_round_trips() {
        // 配置文件里的名字与 Go 的 `TransportMode` 是同一组字符串。
        for transport in [Transport::Auto, Transport::Relay, Transport::Derp] {
            assert_eq!(remembered_transport(transport_name(transport)), Some(transport));
        }
        assert_eq!(remembered_transport(""), None);
        assert_eq!(remembered_transport("bogus"), None);
    }

    #[test]
    fn throttle_units_follow_go() {
        // Go：`if len(...) > 1`，所以一个字符的输入等于没给。
        assert_eq!(parse_throttle(""), Ok(None));
        assert_eq!(parse_throttle("5"), Ok(None));
        // 不带单位就是字节/秒，不是 kB/s。
        assert_eq!(parse_throttle("500"), Ok(Some(500)));
        assert_eq!(parse_throttle("500k"), Ok(Some(500 * 1024)));
        assert_eq!(parse_throttle("1M"), Ok(Some(1024 * 1024)));
        assert_eq!(parse_throttle("2G"), Ok(Some(2 * 1024 * 1024 * 1024)));
        assert_eq!(parse_throttle(" 1m "), Ok(Some(1024 * 1024)));
    }

    #[test]
    fn bad_throttle_values_are_rejected() {
        // Go 这里直接 panic；我们给一行能读的报错，但同样不静默忽略——
        // 一个没生效的限速会让用户以为带宽被限住了。
        for wrong in ["12x", "abc", "k5", "0k", "1.5m"] {
            assert!(parse_throttle(wrong).is_err(), "{wrong} 应当被拒");
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

    #[test]
    fn only_the_algorithm_names_go_accepts_are_accepted() {
        // Go 的 `HashFile` 是精确 switch，名字对不上就报
        // "unspecified algorithm"——所以我们这里也精确匹配，不做大小写折叠。
        for name in ["xxhash", "md5", "imohash", "highway"] {
            assert_eq!(validate_hash(name), Ok(()), "{name} 应当被接受");
        }
        for wrong in ["sha256", "XXHASH", ""] {
            let error = validate_hash(wrong).expect_err("未知算法名应当被拒绝");
            assert!(error.starts_with("croc-rs: "), "报错要带程序名前缀");
        }
        // 报错得说清是哪个名字不对，否则用户看不出拼错在哪。
        assert!(validate_hash("sha256").expect_err("应报错").contains("sha256"));
    }

    #[test]
    fn port_and_transfers_are_bounded() {
        // `--transfers` 的上限是数据面的硬上限（`croc_engine::data::MAX_STREAMS` = 8）。
        for bad in ["0", "9"] {
            let parsed = Cli::try_parse_from(["croc-rs", "send", "a.bin", "--transfers", bad]);
            assert!(parsed.is_err(), "--transfers {bad} 应当被 clap 拒掉");
        }
        // `--port` 0 不是合法端口。
        let parsed = Cli::try_parse_from(["croc-rs", "send", "a.bin", "--port", "0"]);
        assert!(parsed.is_err(), "--port 0 应当被 clap 拒掉");
        // 默认值要与 Go 的 flag 一致：9009 / 4。
        let parsed = Cli::try_parse_from(["croc-rs", "send", "a.bin"]).expect("默认值应当能解析");
        let Some(Command::Send(args)) = parsed.command else { panic!("应当是 send 子命令") };
        assert_eq!(args.port, 9009);
        assert_eq!(args.transfers, 4);
    }
}
