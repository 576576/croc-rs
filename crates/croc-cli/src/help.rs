//! `-h` / `--help` 的输出。
//!
//! 这里不经过 clap，渲染规则照抄 Go 版自己 fork 的 cli（`internal/cli/help.go`）：
//!
//! * 每一节以 `NAME:` / `USAGE:` 这样的大写标题 + 空行开始；
//! * 两列对齐：左列宽度取该节里最长的一项（按字符数，不是字节），右边空两格后接说明；
//! * 取值开关带 ` value` 占位符，布尔开关不带；
//! * 布尔开关也显示 `(default: false)`，空字符串默认值不显示，字符串默认值加双引号；
//! * 环境变量在行尾标成 `[%VAR%]`（Windows）或 `[$VAR]`（其余平台）。
//!
//! 开关的形状（名字、别名、默认值、环境变量、取不取值）是语言无关的，留在这张表里；
//! 说明文字在 `locales/<语言>.yml`，英文一栏取自 Go 版 `croc -h` 的实际输出，
//! `tests/golden/help/` 下存着对照文件，程序名换成 `croc-rs`，版式与其余每个字都相同。

use rust_i18n::t;

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
    /// 说明的文案键（页内 `flag.` 之后那一段）。
    key: &'static str,
    /// 默认值的最终写法（字符串要连引号一起给，时长给 `5m0s` 这样）。
    default: Option<&'static str>,
    /// 环境变量名。
    env: Option<&'static str>,
    /// 是否取值。布尔开关不带 ` value` 占位符。
    takes_value: bool,
}

/// 布尔开关：不带占位符，但仍显示 `(default: false)`。
macro_rules! boolean {
    ($names:expr, $key:expr) => {
        Flag { names: $names, key: $key, default: Some("false"), env: None, takes_value: false }
    };
}

/// 取值开关。默认值与环境变量按需附加。
macro_rules! valued {
    ($names:expr, $key:expr) => {
        Flag { names: $names, key: $key, default: None, env: None, takes_value: true }
    };
    ($names:expr, $key:expr, default = $default:expr) => {
        Flag { names: $names, key: $key, default: Some($default), env: None, takes_value: true }
    };
    ($names:expr, $key:expr, env = $env:expr) => {
        Flag { names: $names, key: $key, default: None, env: Some($env), takes_value: true }
    };
    ($names:expr, $key:expr, default = $default:expr, env = $env:expr) => {
        Flag { names: $names, key: $key, default: Some($default), env: Some($env), takes_value: true }
    };
}

/// 一页帮助的全部素材。
struct Doc {
    /// `croc-rs` / `croc-rs send`。
    help_name: &'static str,
    /// 这一页自己的文案键（`help.<page>.usage` / `.description` / `.args_usage`）。
    page: &'static str,
    /// 开关说明所在的页（`help.<flags>.flag.<key>`）。顶层页列出的是全局开关，
    /// 所以这里与 `page` 不同。
    flags: &'static str,
    /// 开关列表。
    table: &'static [Flag],
}

/// 从 argv（不含程序名）里认出这次要不要打帮助、打哪一页。
///
/// 规则照 Go 版（urfave/cli）：
/// * `-h` / `--help` 出现在任何位置都算；
/// * 子命令只看第一个词——`croc --help send` 打的是顶层帮助（Go 也是如此）；
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
    pub(crate) fn listed_flags(self) -> Vec<&'static str> {
        self.doc().table.iter().filter_map(|flag| flag.names.first().copied()).collect()
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
    const fn doc(self) -> Doc {
        match self {
            Self::App => APP,
            Self::Send => SEND,
            Self::Store => STORE,
            Self::Ssh => SSH,
            Self::Update => UPDATE,
            Self::Relay => RELAY,
            Self::FishCompletion => FISH,
        }
    }

    /// 顶层页里 `COMMANDS:` 那一节的内容：左列是命令名（语言无关），右列是文案键。
    const fn commands(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::App => COMMANDS,
            _ => &[],
        }
    }
}

/// 取一条文案。`key` 是 yml 里的点分路径。
///
/// `t!` 给的是 `Cow`（不带参数时直接指向编译期烘进来的那份），这里要的是自有的一份：
/// 后面还要往说明那一列上拼 `(default: …)` 与环境变量标记。
fn text(locale: Locale, key: &str) -> String {
    t!(key, locale = locale.tag()).into_owned()
}

/// 这一页自己的一条文案（`help.<page>.<field>`）。
fn page_text(locale: Locale, page: &str, field: &str) -> String {
    text(locale, &format!("help.{page}.{field}"))
}

/// 分节标题：`help.section.<name>`。
fn section(locale: Locale, name: &str) -> String {
    text(locale, &format!("help.section.{name}"))
}

/// 渲染一页帮助。
fn render(topic: Topic, locale: Locale, shell: Shell) -> String {
    let doc = topic.doc();
    let mut out = String::new();
    line(&mut out, &format!("{}:", section(locale, "name")));
    line(&mut out, &format!("   {} - {}", doc.help_name, page_text(locale, doc.page, "usage")));
    line(&mut out, &format!("\n{}:", section(locale, "usage")));
    if topic == Topic::App {
        line(&mut out, &format!("   {}", page_text(locale, doc.page, "usage_text")));
        line(&mut out, &format!("\n{}:\n   {VERSION}", section(locale, "version")));
        let commands = topic.commands();
        if !commands.is_empty() {
            let command_rows = commands
                .iter()
                .map(|(left, key)| ((*left).to_owned(), text(locale, &format!("help.app.command.{key}"))))
                .collect::<Vec<_>>();
            line(&mut out, &format!("\n{}:", section(locale, "commands")));
            rows(&mut out, "   ", &command_rows);
        }
        line(&mut out, &format!("\n{}:", section(locale, "global_options")));
    } else {
        line(
            &mut out,
            &format!(
                "   {} {} {}",
                doc.help_name,
                section(locale, "command_options"),
                page_text(locale, doc.page, "args_usage")
            ),
        );
        let description = page_text(locale, doc.page, "description");
        if !description.is_empty() {
            line(&mut out, &format!("\n{}:\n   {}", section(locale, "description"), description));
        }
        line(&mut out, &format!("\n{}:", section(locale, "options")));
    }
    rows(&mut out, "   ", &flag_rows(&doc, shell, locale));
    out
}

/// 写一行（自带换行）。
fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

/// 写一段两列对齐的内容。左列宽度按字符数算，与 Go 的 `utf8.RuneCountInString` 一致。
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
fn flag_rows(doc: &Doc, shell: Shell, locale: Locale) -> Vec<(String, String)> {
    doc.table
        .iter()
        .map(|flag| {
            let placeholder = if flag.takes_value { " value" } else { "" };
            let left = flag
                .names
                .iter()
                .map(|name| format!("{}{name}{placeholder}", prefix_of(name)))
                .collect::<Vec<_>>()
                .join(", ");
            let mut right = text(locale, &format!("help.{}.flag.{}", doc.flags, flag.key));
            if let Some(default) = flag.default {
                right.push_str(&format!(" ({}: {default})", text(locale, "help.default")));
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
// 开关与页的素材。说明文字见 `locales/`：这里的第二个参数只是键名。
// ---------------------------------------------------------------------------

/// `COMMANDS:` 那一节：左列是命令名（含别名写法，与 Go 逐字相同），右列是文案键。
const COMMANDS: &[(&str, &str)] = &[
    ("send", "send"),
    ("store", "store"),
    ("ssh", "ssh"),
    ("update, upgrade", "update"),
    ("relay", "relay"),
    ("help, h", "help"),
];

const HELP: Flag = boolean!(&["help", "h"], "help");
const VERSION_FLAG: Flag = boolean!(&["version", "v"], "version");

const GLOBAL: &[Flag] = &[
    boolean!(&["internal-dns"], "internal-dns"),
    boolean!(&["classic"], "classic"),
    boolean!(&["remember"], "remember"),
    boolean!(&["debug"], "debug"),
    boolean!(&["yes"], "yes"),
    boolean!(&["stdout"], "stdout"),
    boolean!(&["no-compress"], "no-compress"),
    boolean!(&["ask"], "ask"),
    boolean!(&["local"], "local"),
    boolean!(&["ignore-stdin"], "ignore-stdin"),
    boolean!(&["overwrite"], "overwrite"),
    boolean!(&["rename"], "rename"),
    boolean!(&["testing"], "testing"),
    boolean!(&["quiet"], "quiet"),
    boolean!(&["disable-clipboard"], "disable-clipboard"),
    boolean!(&["extended-clipboard"], "extended-clipboard"),
    valued!(&["revoke"], "revoke"),
    valued!(&["multicast"], "multicast", default = "\"239.255.255.250\""),
    valued!(&["curve"], "curve", default = "\"p256\""),
    valued!(&["ip"], "ip"),
    valued!(&["relay"], "relay", default = "\"croc.schollz.com:9009\"", env = "CROC_RELAY"),
    valued!(&["relay6"], "relay6", default = "\"croc6.schollz.com:9009\"", env = "CROC_RELAY6"),
    valued!(&["out"], "out", default = "\".\""),
    valued!(&["pass"], "pass", default = "\"pass123\"", env = "CROC_PASS"),
    valued!(&["socks5"], "socks5", env = "SOCKS5_PROXY"),
    valued!(&["connect"], "connect", env = "HTTP_PROXY"),
    valued!(&["throttleUpload"], "throttleUpload"),
    HELP,
    VERSION_FLAG,
];

const SEND: Doc = Doc {
    help_name: "croc-rs send",
    page: "send",
    flags: "send",
    table: &[
        boolean!(&["zip"], "zip"),
        valued!(&["code", "c"], "code"),
        valued!(&["transport"], "transport", default = "\"auto\""),
        valued!(&["hash"], "hash", default = "\"xxhash\""),
        valued!(&["text", "t"], "text"),
        boolean!(&["no-local"], "no-local"),
        boolean!(&["no-multi"], "no-multi"),
        boolean!(&["git"], "git"),
        valued!(&["port"], "port", default = "9009"),
        valued!(&["transfers"], "transfers", default = "4"),
        boolean!(&["qrcode", "qr"], "qrcode"),
        valued!(&["exclude"], "exclude"),
        valued!(&["exclude-file"], "exclude-file"),
        valued!(&["socks5"], "socks5", env = "SOCKS5_PROXY"),
        valued!(&["connect"], "connect", env = "HTTP_PROXY"),
        boolean!(&["store"], "store"),
        valued!(&["store-downloads"], "store-downloads", default = "1"),
        valued!(&["store-expiration"], "store-expiration", default = "\"1d\""),
        valued!(&["store-url"], "store-url", default = "\"https://getcroc.com\"", env = "CROC_STORE_URL"),
        HELP,
    ],
};

const STORE: Doc = Doc {
    help_name: "croc-rs store",
    page: "store",
    flags: "store",
    table: &[
        valued!(&["downloads"], "downloads", default = "1"),
        valued!(&["expiration"], "expiration", default = "\"1d\""),
        valued!(&["url"], "url", default = "\"https://getcroc.com\"", env = "CROC_STORE_URL"),
        valued!(&["socks5"], "socks5", env = "SOCKS5_PROXY"),
        valued!(&["connect"], "connect", env = "HTTP_PROXY"),
        boolean!(&["qrcode", "qr"], "qrcode"),
        HELP,
    ],
};

const SSH: Doc = Doc {
    help_name: "croc-rs ssh",
    page: "ssh",
    flags: "ssh",
    table: &[
        boolean!(&["headless"], "headless"),
        valued!(&["duration"], "duration", default = "12h0m0s"),
        valued!(&["reconnect-window"], "reconnect-window", default = "2m0s"),
        boolean!(&["no-reconnect"], "no-reconnect"),
        valued!(&["transport"], "transport", default = "\"auto\""),
        valued!(&["dir"], "dir", default = "\".\""),
        HELP,
    ],
};

const UPDATE: Doc = Doc {
    help_name: "croc-rs update",
    page: "update",
    flags: "update",
    table: &[boolean!(&["check"], "check"), boolean!(&["yes"], "yes"), HELP],
};

const RELAY: Doc = Doc {
    help_name: "croc-rs relay",
    page: "relay",
    flags: "relay",
    table: &[
        valued!(&["host"], "host"),
        valued!(&["ports"], "ports", default = "\"9009,9010,9011,9012,9013\"", env = "CROC_PORTS"),
        valued!(&["port"], "port", default = "9009", env = "CROC_PORT"),
        valued!(&["transfers"], "transfers", default = "5"),
        valued!(&["max-rooms-open"], "max-rooms-open", default = "128", env = "CROC_MAX_ROOMS_OPEN"),
        valued!(
            &["max-pending-handshakes"],
            "max-pending-handshakes",
            default = "64",
            env = "CROC_MAX_PENDING_HANDSHAKES"
        ),
        valued!(&["handshake-timeout"], "handshake-timeout", default = "5m0s", env = "CROC_HANDSHAKE_TIMEOUT"),
        valued!(&["source-join-limit"], "source-join-limit", default = "30", env = "CROC_SOURCE_JOIN_LIMIT"),
        valued!(&["room-join-limit"], "room-join-limit", default = "30", env = "CROC_ROOM_JOIN_LIMIT"),
        valued!(&["join-limit-window"], "join-limit-window", default = "1m0s", env = "CROC_JOIN_LIMIT_WINDOW"),
        HELP,
    ],
};

const FISH: Doc = Doc { help_name: "croc-rs generate-fish-completion", page: "fish", flags: "fish", table: &[HELP] };

const APP: Doc = Doc { help_name: "croc-rs", page: "app", flags: "global", table: GLOBAL };

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    /// 每一页帮助的基准文件。
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
    /// 基准文件是 `croc -h` 的实际输出，只做了两处替换：程序名 → `croc-rs`，
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

    /// 中文页与英文页同形：行数一样，且每一行的开关名与顺序完全一样。
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
    /// 两列之间是两个以上空格（`renderHelpRows` 的 `  ` 分隔符），所以切第一个双空格
    /// 就得到左列。只认 `--` 开头的行：用法示例里那行 `-git to respect your .gitignore`
    /// 也是短横线开头，但它是说明不是开关。
    fn flag_column(line: &str) -> Option<&str> {
        let trimmed = line.trim_start();
        trimmed.starts_with("--").then(|| trimmed.split("  ").next().unwrap_or(trimmed))
    }

    /// 两种语言的文案表必须同键。
    ///
    /// 漏翻一条，界面上出现的就是键名本身（`help.send.flag.zip` 这种），而逐字对照与
    /// 同形测试都挡不住：前者只跑这七页，后者只比形状。
    #[test]
    fn every_locale_translates_the_same_keys() {
        let backend = crate::_rust_i18n_backend();
        let locales = rust_i18n::available_locales!();
        let keys_of = |locale: &str| {
            backend
                .messages_for_locale(locale)
                .unwrap_or_else(|| panic!("没有 {locale} 这张表"))
                .into_iter()
                .map(|(key, _)| key.into_owned())
                .collect::<BTreeSet<_>>()
        };
        let english = keys_of("en");
        assert!(locales.iter().any(|locale| locale == "en"), "locales 里要有 en：{locales:?}");
        assert!(locales.iter().any(|locale| locale == "zh-CN"), "locales 里要有 zh-CN：{locales:?}");
        for locale in locales {
            assert_eq!(keys_of(&locale), english, "{locale} 的键与 en 不一致");
        }
        // 空串是合法的（`fish` 页没有 `DESCRIPTION:` 段），但空键集合说明表根本没加载。
        assert!(!english.is_empty(), "文案表是空的");
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
