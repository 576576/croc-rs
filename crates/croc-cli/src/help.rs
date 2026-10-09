//! `-h` / `--help` 的输出。
//!
//! 这里**不经过 clap**，渲染规则照抄 Go 版自己 fork 的 cli（`internal/cli/help.go`）：
//!
//! * 每一节以 `NAME:` / `USAGE:` 这样的大写标题 + 空行开始；
//! * 两列对齐：左列宽度取该节里最长的一项（按**字符**数，不是字节），右边空两格后接说明；
//! * 取值开关带 ` value` 占位符，布尔开关不带；
//! * 布尔开关也显示 `(default: false)`，空字符串默认值不显示，字符串默认值加双引号；
//! * 环境变量在行尾标成 `[%VAR%]`（Windows）或 `[$VAR]`（其余平台）。
//!
//! 英文文案取自 Go 版 `croc -h` 的实际输出，`tests/golden/help/` 下有对照文件；
//! 程序名换成 `croc-rs`，版式与其余每一个字都相同。

use crate::locale::Locale;

/// 想看的帮助主题。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Topic {
    /// 顶层。
    App,
    /// `send`。
    Send,
    /// `store`。
    Store,
    /// `ssh`。
    Ssh,
    /// `update`。
    Update,
    /// `relay`。
    Relay,
    /// `generate-fish-completion`（在 `--help` 里隐藏）。
    FishCompletion,
}

/// 版本号（由 `build.rs` 烘进来）。
const VERSION: &str = env!("CROC_CLI_VERSION");

/// 环境变量标记的写法。Go 版按 `runtime.GOOS` 选，这里照抄。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    /// `[%VAR%]`。
    Windows,
    /// `[$VAR]`。
    Posix,
}

impl Shell {
    /// 本机平台。
    const fn host() -> Self {
        if cfg!(windows) { Self::Windows } else { Self::Posix }
    }
}

/// 一条开关。
struct Flag {
    /// 名字，不带横线。第一个是主名，其余是别名。
    names: &'static [&'static str],
    /// 说明。
    usage: &'static str,
    /// 默认值的**最终写法**（字符串要连引号一起给，时长给 `5m0s` 这样）。
    default: Option<&'static str>,
    /// 环境变量名。
    env: Option<&'static str>,
    /// 是否取值。布尔开关不带 ` value` 占位符。
    takes_value: bool,
}

/// 布尔开关：不带占位符，但照样显示 `(default: false)`。
macro_rules! boolean {
    ($names:expr, $usage:expr) => {
        Flag { names: $names, usage: $usage, default: Some("false"), env: None, takes_value: false }
    };
}

/// 取值开关。默认值与环境变量按需附加。
macro_rules! valued {
    ($names:expr, $usage:expr) => {
        Flag { names: $names, usage: $usage, default: None, env: None, takes_value: true }
    };
    ($names:expr, $usage:expr, default = $default:expr) => {
        Flag { names: $names, usage: $usage, default: Some($default), env: None, takes_value: true }
    };
    ($names:expr, $usage:expr, env = $env:expr) => {
        Flag { names: $names, usage: $usage, default: None, env: Some($env), takes_value: true }
    };
    ($names:expr, $usage:expr, default = $default:expr, env = $env:expr) => {
        Flag { names: $names, usage: $usage, default: Some($default), env: Some($env), takes_value: true }
    };
}

/// 一页帮助的全部素材。
struct Doc {
    /// `croc-rs` / `croc-rs send`。
    help_name: &'static str,
    /// 短说明，跟在 `NAME:` 那行的 ` - ` 之后。
    usage: &'static str,
    /// `DESCRIPTION:` 段。空则不输出该段（Go 的 `generate-fish-completion` 就是这样）。
    description: &'static str,
    /// `USAGE:` 行尾的参数说明。
    args_usage: &'static str,
    /// 开关列表。
    flags: &'static [Flag],
}

/// 从 argv（**不含**程序名）里认出这次要不要打帮助、打哪一页。
///
/// 规则照 Go 版（urfave/cli）：
/// * `-h` / `--help` 出现在**任何**位置都算；
/// * 子命令只看**第一个**词——`croc --help send` 打的是顶层帮助（Go 也是如此）；
/// * `help` / `h` 命令后面跟的子命令名决定打哪一页；
/// * 裸 `--` 之后全是位置参数，不再是开关（否则 `send -- -h` 会把一个叫 `-h` 的文件
///   当成帮助请求）。
#[must_use]
pub fn request(args: &[String]) -> Option<Topic> {
    let flags_end = args.iter().position(|arg| arg == "--").unwrap_or(args.len());
    let head = args.get(..flags_end).unwrap_or(args);
    let wants_help = head.iter().any(|arg| arg == "-h" || arg == "--help");
    let first = head.first().map(String::as_str);
    let help_command = matches!(first, Some("help" | "h"));
    if !wants_help && !help_command {
        return None;
    }
    let named = if help_command {
        head.get(1).and_then(|word| Topic::from_command(word))
    } else {
        first.and_then(Topic::from_command)
    };
    Some(named.unwrap_or(Topic::App))
}

/// 打一页帮助到 stdout。
pub fn print(topic: Topic, locale: Locale) {
    print!("{}", render(topic, locale, Shell::host()));
}

impl Topic {
    /// 对应的子命令名。顶层没有。
    ///
    /// 只有测试用（把帮助表与 clap 的开关集对上）。
    #[cfg(test)]
    pub(crate) const fn clap_name(self) -> Option<&'static str> {
        match self {
            Self::App => None,
            Self::Send => Some("send"),
            Self::Store => Some("store"),
            Self::Ssh => Some("ssh"),
            Self::Update => Some("update"),
            Self::Relay => Some("relay"),
            Self::FishCompletion => Some("generate-fish-completion"),
        }
    }

    /// 这一页上列出的开关主名。
    ///
    /// 只有测试用。
    #[cfg(test)]
    pub(crate) fn listed_flags(self, locale: Locale) -> Vec<&'static str> {
        self.doc(locale).flags.iter().filter_map(|flag| flag.names.first().copied()).collect()
    }

    /// 命令名 → 主题。不是命令名就返回 `None`。
    fn from_command(word: &str) -> Option<Self> {
        match word {
            "send" => Some(Self::Send),
            "store" => Some(Self::Store),
            "ssh" => Some(Self::Ssh),
            "update" | "upgrade" => Some(Self::Update),
            "relay" => Some(Self::Relay),
            "generate-fish-completion" => Some(Self::FishCompletion),
            _ => None,
        }
    }

    /// 这一页的素材。
    const fn doc(self, locale: Locale) -> Doc {
        match (self, locale) {
            (Self::App, Locale::En) => APP_EN,
            (Self::App, Locale::Zh) => APP_ZH,
            (Self::Send, Locale::En) => SEND_EN,
            (Self::Send, Locale::Zh) => SEND_ZH,
            (Self::Store, Locale::En) => STORE_EN,
            (Self::Store, Locale::Zh) => STORE_ZH,
            (Self::Ssh, Locale::En) => SSH_EN,
            (Self::Ssh, Locale::Zh) => SSH_ZH,
            (Self::Update, Locale::En) => UPDATE_EN,
            (Self::Update, Locale::Zh) => UPDATE_ZH,
            (Self::Relay, Locale::En) => RELAY_EN,
            (Self::Relay, Locale::Zh) => RELAY_ZH,
            (Self::FishCompletion, Locale::En) => FISH_EN,
            (Self::FishCompletion, Locale::Zh) => FISH_ZH,
        }
    }

    /// 顶层页里 `COMMANDS:` 那一节的内容。
    const fn commands(self, locale: Locale) -> &'static [(&'static str, &'static str)] {
        match (self, locale) {
            (Self::App, Locale::Zh) => COMMANDS_ZH,
            (Self::App, Locale::En) => COMMANDS_EN,
            _ => &[],
        }
    }
}

/// 渲染一页帮助。
fn render(topic: Topic, locale: Locale, shell: Shell) -> String {
    let doc = topic.doc(locale);
    let mut out = String::new();
    line(&mut out, &format!("{}:", label("NAME", locale)));
    line(&mut out, &format!("   {} - {}", doc.help_name, doc.usage));
    line(&mut out, &format!("\n{}:", label("USAGE", locale)));
    if topic == Topic::App {
        let usage_text = match locale {
            Locale::En => USAGE_TEXT_EN,
            Locale::Zh => USAGE_TEXT_ZH,
        };
        line(&mut out, &format!("   {usage_text}"));
        line(&mut out, &format!("\n{}:\n   {VERSION}", label("VERSION", locale)));
        let commands = topic.commands(locale);
        if !commands.is_empty() {
            let command_rows = commands.iter().map(|(left, right)| ((*left).to_owned(), (*right).to_owned()));
            line(&mut out, &format!("\n{}:", label("COMMANDS", locale)));
            rows(&mut out, "   ", &command_rows.collect::<Vec<_>>());
        }
        line(&mut out, &format!("\n{}:", label("GLOBAL OPTIONS", locale)));
    } else {
        line(&mut out, &format!("   {} {} {}", doc.help_name, label("COMMAND OPTIONS", locale), doc.args_usage));
        if !doc.description.is_empty() {
            line(&mut out, &format!("\n{}:\n   {}", label("DESCRIPTION", locale), doc.description));
        }
        line(&mut out, &format!("\n{}:", label("OPTIONS", locale)));
    }
    rows(&mut out, "   ", &flag_rows(doc.flags, shell, locale));
    out
}

/// 分节标题。英文与 Go 版一致，中文用对应的词。
fn label(section: &'static str, locale: Locale) -> &'static str {
    match (section, locale) {
        ("NAME", Locale::Zh) => "名称",
        ("USAGE", Locale::Zh) => "用法",
        ("VERSION", Locale::Zh) => "版本",
        ("COMMANDS", Locale::Zh) => "命令",
        ("GLOBAL OPTIONS", Locale::Zh) => "全局选项",
        ("DESCRIPTION", Locale::Zh) => "说明",
        ("OPTIONS", Locale::Zh) => "选项",
        ("COMMAND OPTIONS", Locale::Zh) => "[子命令选项]",
        ("COMMAND OPTIONS", Locale::En) => "[command options]",
        (other, _) => other,
    }
}

/// 写一行（自带换行）。
fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

/// 写一段两列对齐的内容。左列宽度按**字符**数算，与 Go 的 `utf8.RuneCountInString` 一致。
fn rows(out: &mut String, prefix: &str, rows: &[(String, String)]) {
    let width = rows.iter().map(|(left, _)| left.chars().count()).max().unwrap_or(0);
    for (left, right) in rows {
        out.push_str(prefix);
        out.push_str(left);
        for _ in left.chars().count()..width {
            out.push(' ');
        }
        if !right.is_empty() {
            out.push_str("  ");
            out.push_str(right);
        }
        out.push('\n');
    }
}

/// 开关列表 → 两列内容。
fn flag_rows(flags: &[Flag], shell: Shell, locale: Locale) -> Vec<(String, String)> {
    flags
        .iter()
        .map(|flag| {
            let placeholder = if flag.takes_value { " value" } else { "" };
            let left = flag
                .names
                .iter()
                .map(|name| format!("{}{name}{placeholder}", prefix_of(name)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut right = flag.usage.to_owned();
            if let Some(default) = flag.default {
                let label = match locale {
                    Locale::En => "default",
                    Locale::Zh => "默认",
                };
                right.push_str(&format!(" ({label}: {default})"));
            }
            if let Some(name) = flag.env {
                right.push(' ');
                right.push_str(&match shell {
                    Shell::Windows => format!("[%{name}%]"),
                    Shell::Posix => format!("[${name}]"),
                });
            }
            (left, right)
        })
        .collect()
}

/// 一个名字该带几根横线：单字母用 `-`，其余用 `--`。
fn prefix_of(name: &str) -> &'static str {
    if name.chars().count() == 1 { "-" } else { "--" }
}

// ---------------------------------------------------------------------------
// 英文文案。逐字取自 Go 版 `croc -h`，只把程序名换成 `croc-rs`。
// ---------------------------------------------------------------------------

const USAGE_TEXT_EN: &str = "\
croc-rs [GLOBAL OPTIONS] [COMMAND] [COMMAND OPTIONS] [filename(s) or folder]

   USAGE EXAMPLES:
   Send a file:
      croc-rs send file.txt

      -git to respect your .gitignore
   Send multiple files:
      croc-rs send file1.txt file2.txt file3.txt
    or
      croc-rs send *.jpg

   Send everything in a folder:
      croc-rs send example-folder-name

   Send a file with a custom code:
      croc-rs send --code secret-code file.txt

   Receive a file using code:
      croc-rs secret-code

   Store files for later download:
      croc-rs store --downloads 3 --expiration 3d file.txt

   Share a terminal:
      croc-rs ssh

   Join a shared terminal:
      CROC_SECRET=six-word-invitation croc-rs ssh";

const COMMANDS_EN: &[(&str, &str)] = &[
    ("send", "send file(s), or folder (see options with croc-rs send -h)"),
    ("store", "upload encrypted files for later download"),
    ("ssh", "share or join a secure, collaborative terminal"),
    ("update, upgrade", "check for and safely install a newer croc-rs release"),
    ("relay", "start your own relay (optional)"),
    ("help, h", "Shows a list of commands or help for one command"),
];

const HELP_EN: Flag = boolean!(&["help", "h"], "show help");
const VERSION_EN: Flag = boolean!(&["version", "v"], "print the version");

const GLOBAL_EN: &[Flag] = &[
    boolean!(&["internal-dns"], "use a built-in DNS stub resolver rather than the host operating system"),
    boolean!(
        &["classic"],
        "toggle between the classic mode (insecure due to local attack vector) and new mode (secure)"
    ),
    boolean!(&["remember"], "save these settings to reuse next time"),
    boolean!(&["debug"], "toggle debug mode"),
    boolean!(&["yes"], "automatically agree to all prompts"),
    boolean!(&["stdout"], "redirect file to stdout"),
    boolean!(&["no-compress"], "disable compression"),
    boolean!(&["ask"], "make sure sender and recipient are prompted"),
    boolean!(&["local"], "force to use only local connections"),
    boolean!(&["ignore-stdin"], "ignore piped stdin"),
    boolean!(&["overwrite"], "do not prompt to overwrite or resume"),
    boolean!(&["rename"], "receive files that already exist under a new name instead of prompting"),
    boolean!(&["testing"], "flag for testing purposes"),
    boolean!(&["quiet"], "disable all output"),
    boolean!(&["disable-clipboard"], "disable copy to clipboard"),
    boolean!(&["extended-clipboard"], "copy full command with secret as env variable to clipboard"),
    valued!(&["revoke"], "revoke a stored transfer using its local sender receipt"),
    valued!(&["multicast"], "multicast address to use for local discovery", default = "\"239.255.255.250\""),
    valued!(&["curve"], "choose an encryption curve (p521, p256, p384, siec, ed25519)", default = "\"p256\""),
    valued!(&["ip"], "set sender ip if known e.g. 10.0.0.1:9009, [::1]:9009"),
    valued!(&["relay"], "address of the relay", default = "\"croc.schollz.com:9009\"", env = "CROC_RELAY"),
    valued!(&["relay6"], "ipv6 address of the relay", default = "\"croc6.schollz.com:9009\"", env = "CROC_RELAY6"),
    valued!(&["out"], "specify an output folder to receive the file", default = "\".\""),
    valued!(&["pass"], "password for the relay", default = "\"pass123\"", env = "CROC_PASS"),
    valued!(&["socks5"], "SOCKS5 proxy address (relay DNS is resolved by the proxy)", env = "SOCKS5_PROXY"),
    valued!(&["connect"], "add a http proxy", env = "HTTP_PROXY"),
    valued!(&["throttleUpload"], "throttle the upload speed e.g. 500k"),
    HELP_EN,
    VERSION_EN,
];

const SEND_EN: Doc = Doc {
    help_name: "croc-rs send",
    usage: "send file(s), or folder (see options with croc-rs send -h)",
    description: "send file(s), or folder",
    args_usage: "[filename(s) or folder]",
    flags: &[
        boolean!(&["zip"], "zip folder before sending"),
        valued!(&["code", "c"], "codephrase used to connect to relay (at least 6 characters)"),
        valued!(&["transport"], "sender file data transport (auto, derp, relay)", default = "\"auto\""),
        valued!(&["hash"], "hash algorithm (xxhash, imohash, md5, highway)", default = "\"xxhash\""),
        valued!(&["text", "t"], "send some text"),
        boolean!(&["no-local"], "disable local relay when sending"),
        boolean!(&["no-multi"], "disable multiplexing"),
        boolean!(&["git"], "enable .gitignore respect / don't send ignored files"),
        valued!(&["port"], "base port for the relay", default = "9009"),
        valued!(&["transfers"], "number of ports to use for transfers", default = "4"),
        boolean!(&["qrcode", "qr"], "show the web receive URL as a qrcode"),
        valued!(&["exclude"], "exclude files if they contain any of the comma separated strings"),
        valued!(&["exclude-file"], "exclude files matching any of the comma separated relative paths exactly"),
        valued!(&["socks5"], "SOCKS5 proxy address (relay DNS is resolved by the proxy)", env = "SOCKS5_PROXY"),
        valued!(&["connect"], "add a http proxy", env = "HTTP_PROXY"),
        boolean!(&["store"], "upload encrypted files for a finite lifetime or a limited number of verified downloads"),
        valued!(&["store-downloads"], "number of verified downloads allowed in stored mode", default = "1"),
        valued!(
            &["store-expiration"],
            "stored lifetime after upload (for example 90m, 12h, 3d, or 2w)",
            default = "\"1d\""
        ),
        valued!(
            &["store-url"],
            "stored-transfer service origin",
            default = "\"https://getcroc.com\"",
            env = "CROC_STORE_URL"
        ),
        HELP_EN,
    ],
};

const STORE_EN: Doc = Doc {
    help_name: "croc-rs store",
    usage: "upload encrypted files for later download",
    description: "store regular files with a download limit and expiration",
    args_usage: "[filename(s)]",
    flags: &[
        valued!(&["downloads"], "number of verified downloads allowed in stored mode", default = "1"),
        valued!(&["expiration"], "stored lifetime after upload (for example 90m, 12h, 3d, or 2w)", default = "\"1d\""),
        valued!(
            &["url"],
            "stored-transfer service origin",
            default = "\"https://getcroc.com\"",
            env = "CROC_STORE_URL"
        ),
        valued!(&["socks5"], "SOCKS5 proxy address (relay DNS is resolved by the proxy)", env = "SOCKS5_PROXY"),
        valued!(&["connect"], "add a http proxy", env = "HTTP_PROXY"),
        boolean!(&["qrcode", "qr"], "show the web receive URL as a qrcode"),
        HELP_EN,
    ],
};

const SSH_EN: Doc = Doc {
    help_name: "croc-rs ssh",
    usage: "share or join a secure, collaborative terminal",
    description: "start without a code to host; provide a six-word code to join",
    args_usage: "[six-word-code]",
    flags: &[
        boolean!(&["headless"], "host without attaching the local terminal"),
        valued!(&["duration"], "maximum hosted session lifetime", default = "12h0m0s"),
        valued!(&["reconnect-window"], "how long a guest retries after a connection loss", default = "2m0s"),
        boolean!(&["no-reconnect"], "disable automatic guest reconnection"),
        valued!(&["transport"], "guest transport (auto, tailcat, relay)", default = "\"auto\""),
        valued!(&["dir"], "working directory for a hosted shell", default = "\".\""),
        HELP_EN,
    ],
};

const UPDATE_EN: Doc = Doc {
    help_name: "croc-rs update",
    usage: "check for and safely install a newer croc-rs release",
    description: "self-update official standalone installs; show package-manager guidance for other installs",
    args_usage: "[arguments...]",
    flags: &[
        boolean!(&["check"], "check for a newer release without installing it"),
        boolean!(&["yes"], "install without prompting when self-update is safe"),
        HELP_EN,
    ],
};

const RELAY_EN: Doc = Doc {
    help_name: "croc-rs relay",
    usage: "start your own relay (optional)",
    description: "start relay",
    args_usage: "[arguments...]",
    flags: &[
        valued!(&["host"], "host of the relay"),
        valued!(&["ports"], "ports of the relay", default = "\"9009,9010,9011,9012,9013\"", env = "CROC_PORTS"),
        valued!(&["port"], "base port for the relay", default = "9009", env = "CROC_PORT"),
        valued!(&["transfers"], "number of ports to use for relay", default = "5"),
        valued!(
            &["max-rooms-open"],
            "maximum waiting rooms per relay port",
            default = "128",
            env = "CROC_MAX_ROOMS_OPEN"
        ),
        valued!(
            &["max-pending-handshakes"],
            "maximum incomplete handshakes per relay port",
            default = "64",
            env = "CROC_MAX_PENDING_HANDSHAKES"
        ),
        valued!(
            &["handshake-timeout"],
            "maximum time for an initial relay handshake",
            default = "5m0s",
            env = "CROC_HANDSHAKE_TIMEOUT"
        ),
        valued!(
            &["source-join-limit"],
            "maximum room joins per source IP in the admission window",
            default = "30",
            env = "CROC_SOURCE_JOIN_LIMIT"
        ),
        valued!(
            &["room-join-limit"],
            "maximum joins per room in the admission window",
            default = "30",
            env = "CROC_ROOM_JOIN_LIMIT"
        ),
        valued!(
            &["join-limit-window"],
            "sliding window for relay admission limits",
            default = "1m0s",
            env = "CROC_JOIN_LIMIT_WINDOW"
        ),
        HELP_EN,
    ],
};

const FISH_EN: Doc = Doc {
    help_name: "croc-rs generate-fish-completion",
    usage: "generate fish completion and output to stdout",
    description: "",
    args_usage: "[arguments...]",
    flags: &[HELP_EN],
};

const APP_EN: Doc = Doc {
    help_name: "croc-rs",
    usage: "securely transfer files or share a terminal",
    description: "",
    args_usage: "",
    flags: GLOBAL_EN,
};

// ---------------------------------------------------------------------------
// 中文文案。版式与英文版相同，逐条对应。
// ---------------------------------------------------------------------------

const USAGE_TEXT_ZH: &str = "\
croc-rs [全局选项] [子命令] [子命令选项] [文件名或目录]

   用法示例：
   发送文件：
      croc-rs send file.txt

      -git 遵循 .gitignore
   发送多个文件：
      croc-rs send file1.txt file2.txt file3.txt
    或
      croc-rs send *.jpg

   发送整个目录：
      croc-rs send example-folder-name

   指定口令码发送：
      croc-rs send --code secret-code file.txt

   用口令码接收：
      croc-rs secret-code

   暂存后取件：
      croc-rs store --downloads 3 --expiration 3d file.txt

   共享终端：
      croc-rs ssh

   加入共享终端：
      CROC_SECRET=six-word-invitation croc-rs ssh";

const COMMANDS_ZH: &[(&str, &str)] = &[
    ("send", "发送文件或目录（见 croc-rs send -h）"),
    ("store", "加密上传文件，供以后下载"),
    ("ssh", "共享或加入安全协作终端"),
    ("update, upgrade", "检查并安全安装新版本"),
    ("relay", "自建中继（可选）"),
    ("help, h", "列出命令，或查看子命令帮助"),
];

const HELP_ZH: Flag = boolean!(&["help", "h"], "显示帮助");
const VERSION_ZH: Flag = boolean!(&["version", "v"], "打印版本");

const GLOBAL_ZH: &[Flag] = &[
    boolean!(&["internal-dns"], "使用内置 DNS stub resolver，而非操作系统解析器"),
    boolean!(&["classic"], "切换 classic 模式（不安全，存在本地攻击面）与新模式（安全）"),
    boolean!(&["remember"], "记住这些设置供下次使用"),
    boolean!(&["debug"], "切换调试模式"),
    boolean!(&["yes"], "所有询问自动同意"),
    boolean!(&["stdout"], "将文件内容输出到标准输出"),
    boolean!(&["no-compress"], "禁用压缩"),
    boolean!(&["ask"], "发送与接收两端均需确认"),
    boolean!(&["local"], "仅使用局域网连接"),
    boolean!(&["ignore-stdin"], "忽略管道输入的 stdin"),
    boolean!(&["overwrite"], "覆盖或续传时不询问"),
    boolean!(&["rename"], "同名文件以新名字接收，不询问"),
    boolean!(&["testing"], "测试用开关"),
    boolean!(&["quiet"], "不输出任何内容"),
    boolean!(&["disable-clipboard"], "不复制口令码到剪贴板"),
    boolean!(&["extended-clipboard"], "复制整条命令，口令码以环境变量传递"),
    valued!(&["revoke"], "用本机保存的发送方凭据撤销暂存传输"),
    valued!(&["multicast"], "局域网发现使用的组播地址", default = "\"239.255.255.250\""),
    valued!(&["curve"], "选择加密曲线（p521, p256, p384, siec, ed25519）", default = "\"p256\""),
    valued!(&["ip"], "已知的对方地址，如 10.0.0.1:9009、[::1]:9009"),
    valued!(&["relay"], "中继地址", default = "\"croc.schollz.com:9009\"", env = "CROC_RELAY"),
    valued!(&["relay6"], "中继的 IPv6 地址", default = "\"croc6.schollz.com:9009\"", env = "CROC_RELAY6"),
    valued!(&["out"], "接收文件的落盘目录", default = "\".\""),
    valued!(&["pass"], "中继密码", default = "\"pass123\"", env = "CROC_PASS"),
    valued!(&["socks5"], "SOCKS5 代理地址（中继 DNS 由代理解析）", env = "SOCKS5_PROXY"),
    valued!(&["connect"], "HTTP CONNECT 代理地址", env = "HTTP_PROXY"),
    valued!(&["throttleUpload"], "上行限速，如 500k"),
    HELP_ZH,
    VERSION_ZH,
];

const SEND_ZH: Doc = Doc {
    help_name: "croc-rs send",
    usage: "发送文件或目录（见 croc-rs send -h）",
    description: "发送文件或目录",
    args_usage: "[文件名或目录]",
    flags: &[
        boolean!(&["zip"], "发送前将目录打包为 zip"),
        valued!(&["code", "c"], "连接中继的口令码（至少 6 个字符）"),
        valued!(&["transport"], "发送方的数据面（auto, derp, relay）", default = "\"auto\""),
        valued!(&["hash"], "哈希算法（xxhash, imohash, md5, highway）", default = "\"xxhash\""),
        valued!(&["text", "t"], "发送文本"),
        boolean!(&["no-local"], "发送时禁用本地中继"),
        boolean!(&["no-multi"], "禁用多端口复用"),
        boolean!(&["git"], "遵循 .gitignore，不发送被忽略的文件"),
        valued!(&["port"], "中继的基准端口", default = "9009"),
        valued!(&["transfers"], "传输使用的端口数", default = "4"),
        boolean!(&["qrcode", "qr"], "将 web 接收链接显示为二维码"),
        valued!(&["exclude"], "相对路径包含任一子串（逗号分隔）的文件不发送"),
        valued!(&["exclude-file"], "相对路径精确等于任一项（逗号分隔）的文件不发送"),
        valued!(&["socks5"], "SOCKS5 代理地址（中继 DNS 由代理解析）", env = "SOCKS5_PROXY"),
        valued!(&["connect"], "HTTP CONNECT 代理地址", env = "HTTP_PROXY"),
        boolean!(&["store"], "加密上传文件，可限制有效期或下载次数"),
        valued!(&["store-downloads"], "暂存模式下允许的验证下载次数", default = "1"),
        valued!(&["store-expiration"], "上传后的存储时长（如 90m、12h、3d、2w）", default = "\"1d\""),
        valued!(&["store-url"], "暂存服务地址", default = "\"https://getcroc.com\"", env = "CROC_STORE_URL"),
        HELP_ZH,
    ],
};

const STORE_ZH: Doc = Doc {
    help_name: "croc-rs store",
    usage: "加密上传文件，供以后下载",
    description: "暂存普通文件，可限制下载次数与有效期",
    args_usage: "[文件名]",
    flags: &[
        valued!(&["downloads"], "暂存模式下允许的验证下载次数", default = "1"),
        valued!(&["expiration"], "上传后的存储时长（如 90m、12h、3d、2w）", default = "\"1d\""),
        valued!(&["url"], "暂存服务地址", default = "\"https://getcroc.com\"", env = "CROC_STORE_URL"),
        valued!(&["socks5"], "SOCKS5 代理地址（中继 DNS 由代理解析）", env = "SOCKS5_PROXY"),
        valued!(&["connect"], "HTTP CONNECT 代理地址", env = "HTTP_PROXY"),
        boolean!(&["qrcode", "qr"], "将 web 接收链接显示为二维码"),
        HELP_ZH,
    ],
};

const SSH_ZH: Doc = Doc {
    help_name: "croc-rs ssh",
    usage: "共享或加入安全协作终端",
    description: "不给口令码即作为主机；给出六词码即加入",
    args_usage: "[六词码]",
    flags: &[
        boolean!(&["headless"], "仅作为主机，不接入本地终端"),
        valued!(&["duration"], "主机会话的最长存活时长", default = "12h0m0s"),
        valued!(&["reconnect-window"], "客户端掉线后的重试时长", default = "2m0s"),
        boolean!(&["no-reconnect"], "禁用客户端自动重连"),
        valued!(&["transport"], "客户端的数据面（auto, tailcat, relay）", default = "\"auto\""),
        valued!(&["dir"], "主机 shell 的工作目录", default = "\".\""),
        HELP_ZH,
    ],
};

const UPDATE_ZH: Doc = Doc {
    help_name: "croc-rs update",
    usage: "检查并安全安装新版本",
    description: "自更新官方独立安装；其他安装方式给出包管理器指引",
    args_usage: "[参数…]",
    flags: &[boolean!(&["check"], "仅检查是否有新版本，不安装"), boolean!(&["yes"], "自更新安全时不再询问"), HELP_ZH],
};

const RELAY_ZH: Doc = Doc {
    help_name: "croc-rs relay",
    usage: "自建中继（可选）",
    description: "启动中继",
    args_usage: "[参数…]",
    flags: &[
        valued!(&["host"], "中继的监听地址"),
        valued!(&["ports"], "中继的端口列表", default = "\"9009,9010,9011,9012,9013\"", env = "CROC_PORTS"),
        valued!(&["port"], "中继的基准端口", default = "9009", env = "CROC_PORT"),
        valued!(&["transfers"], "中继使用的端口数", default = "5"),
        valued!(&["max-rooms-open"], "每个中继端口的最大等待房间数", default = "128", env = "CROC_MAX_ROOMS_OPEN"),
        valued!(
            &["max-pending-handshakes"],
            "每个中继端口的最大未完成握手数",
            default = "64",
            env = "CROC_MAX_PENDING_HANDSHAKES"
        ),
        valued!(&["handshake-timeout"], "单次初始握手的超时时间", default = "5m0s", env = "CROC_HANDSHAKE_TIMEOUT"),
        valued!(
            &["source-join-limit"],
            "准入窗口内单个来源 IP 的最大加入次数",
            default = "30",
            env = "CROC_SOURCE_JOIN_LIMIT"
        ),
        valued!(&["room-join-limit"], "准入窗口内单个房间的最大加入次数", default = "30", env = "CROC_ROOM_JOIN_LIMIT"),
        valued!(&["join-limit-window"], "准入限流的滑动窗口长度", default = "1m0s", env = "CROC_JOIN_LIMIT_WINDOW"),
        HELP_ZH,
    ],
};

const FISH_ZH: Doc = Doc {
    help_name: "croc-rs generate-fish-completion",
    usage: "生成 fish 补全并输出到 stdout",
    description: "",
    args_usage: "[参数…]",
    flags: &[HELP_ZH],
};

const APP_ZH: Doc =
    Doc {
        help_name: "croc-rs", usage: "安全传输文件或共享终端", description: "", args_usage: "", flags: GLOBAL_ZH
    };

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// 每一页帮助的黄金文件。
    const PAGES: &[(Topic, &str)] = &[
        (Topic::App, "top"),
        (Topic::Send, "send"),
        (Topic::Store, "store"),
        (Topic::Ssh, "ssh"),
        (Topic::Update, "update"),
        (Topic::Relay, "relay"),
        (Topic::FishCompletion, "generate-fish-completion"),
    ];

    /// 英文帮助与 Go 版逐字对照。
    ///
    /// 黄金文件是 `croc -h` 的实际输出，只做了两处替换：程序名 → `croc-rs`，
    /// 版本号 → `<VERSION>`（它随构建变，不能写死）。生成方式见
    /// `tests/golden/help/README`。
    ///
    /// 这里显式用 [`Shell::Windows`]：环境变量标记在 Windows 上是 `[%VAR%]`、别处是
    /// `[$VAR]`，写死一侧才能让三种平台上的 CI 都跑出同一个结果。
    #[test]
    fn the_english_help_matches_the_go_builds_byte_for_byte() {
        for (topic, page) in PAGES {
            let path =
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden/help").join(format!("{page}.txt"));
            let expected = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            // 仓库里存的是 LF，Windows 上检出可能是 CRLF。
            let expected = expected.replace("\r\n", "\n").replace("<VERSION>", VERSION);
            assert_eq!(render(*topic, Locale::En, Shell::Windows), expected, "{page}");
        }
    }

    /// 中文页与英文页**同形**：行数一样，且每一行的开关名与顺序完全一样。
    ///
    /// 这条挡的是"翻中文时漏掉一节、或把某个开关写到了别的位置"——那种错误在两种语言
    /// 之间肉眼很难对上。
    #[test]
    fn the_chinese_pages_have_the_same_shape_as_the_english_ones() {
        for (topic, page) in PAGES {
            let english = render(*topic, Locale::En, Shell::Windows);
            let chinese = render(*topic, Locale::Zh, Shell::Windows);
            assert_eq!(english.lines().count(), chinese.lines().count(), "{page}: 行数不同");
            let columns = |text: &str| {
                text.lines().filter_map(|line| flag_column(line).map(ToOwned::to_owned)).collect::<Vec<_>>()
            };
            assert_eq!(columns(&english), columns(&chinese), "{page}: 开关列不同");
        }
    }

    /// 一行里开关名那一列；不是开关行就返回 `None`。
    ///
    /// 两列之间是**两个以上**空格（`renderHelpRows` 的 `  ` 分隔符），所以切第一个双空格
    /// 就得到左列。只认 `--` 开头的行：用法示例里那行 `-git to respect your .gitignore`
    /// 也是短横线开头，但它是说明不是开关。
    fn flag_column(line: &str) -> Option<&str> {
        let trimmed = line.trim_start();
        trimmed.starts_with("--").then(|| trimmed.split("  ").next().unwrap_or(trimmed))
    }

    #[test]
    fn every_command_word_maps_to_its_topic() {
        assert_eq!(Topic::from_command("send"), Some(Topic::Send));
        assert_eq!(Topic::from_command("upgrade"), Some(Topic::Update));
        assert_eq!(Topic::from_command("nope"), None);
    }

    #[test]
    fn help_is_recognised_anywhere_on_the_line() {
        let argv = |words: &[&str]| words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
        assert_eq!(request(&argv(&["-h"])), Some(Topic::App));
        assert_eq!(request(&argv(&["send", "-h"])), Some(Topic::Send));
        assert_eq!(request(&argv(&["send", "file.txt", "--help"])), Some(Topic::Send));
        assert_eq!(request(&argv(&["help", "send"])), Some(Topic::Send));
        assert_eq!(request(&argv(&["h"])), Some(Topic::App));
        // 与 Go 版一致：子命令只看第一个词，所以这一句打的是顶层帮助。
        assert_eq!(request(&argv(&["--help", "send"])), Some(Topic::App));
        assert_eq!(request(&argv(&["send", "file.txt"])), None);
        assert_eq!(request(&argv(&["secret-code"])), None);
        // 裸 `--` 之后是位置参数：`send -- -h` 发的就是一个叫 `-h` 的文件。
        assert_eq!(request(&argv(&["send", "--", "-h"])), None);
        assert_eq!(request(&argv(&["send", "--help", "--", "-h"])), Some(Topic::Send));
    }

    #[test]
    fn the_two_columns_are_aligned_by_character_count() {
        let mut out = String::new();
        rows(&mut out, "   ", &[("a".to_owned(), "1".to_owned()), ("bbb".to_owned(), "2".to_owned())]);
        assert_eq!(out, "   a    1\n   bbb  2\n");
    }

    #[test]
    fn the_env_marker_follows_the_platform() {
        let out = render(Topic::Relay, Locale::En, Shell::Windows);
        assert!(out.contains("[%CROC_PORTS%]"), "{out}");
        let out = render(Topic::Relay, Locale::En, Shell::Posix);
        assert!(out.contains("[$CROC_PORTS]"), "{out}");
    }
}
