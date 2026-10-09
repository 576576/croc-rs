//! 接收方对一个文件该做什么——一张纯决策表。
//!
//! 覆盖 / 续传 / 重命名 / 拒绝，这几件事在 Go 里散在 `recipientGetFileReady`、
//! `recipientInitializeFile`、`askReceiveOverwrite` 三个函数里，还夹着 `os.Open` 和
//! `fmt.Fprintf`。这里把它抽成 [`decide`]：输入是调用方先探好的事实，输出是一个
//! [`Action`]。没有 IO，没有提示框，没有全局开关。
//!
//! 抽出来的好处很直接：决策表可以穷举测试。Go 那套要跑起来得先有文件系统、
//! 有终端、有用户输入。
//!
//! # 判定顺序（与 Go 逐条对应）
//!
//! 1. 本地哈希与发送方一致 → [`Action::AlreadyCurrent`]，一个字节都不用传；
//! 2. 目标已存在 + `--rename` → [`Action::Rename`]（文本传输的临时文件除外）；
//! 3. 目标已存在 + 没开 `--overwrite` → [`Action::Ask`]；
//! 4. 空文件或符号链接 → [`Action::CreateDirectly`]，不走数据面；
//! 5. 其余 → [`Action::Proceed`]。
//!
//! 顺序本身是语义的一部分：`--rename` 和 `--overwrite` 同时给出时，Go 选重命名。

use croc_protocol::FileInfo;

/// 发送方为文本传输生成的临时文件前缀（`croc-stdin-`）。
pub const TEXT_ARTIFACT_PREFIX: &str = "croc-stdin-";

/// 目标已存在时，"继续"的两种含义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResumeKind {
    /// 本地基本没有可用的数据，整个重来。
    Overwrite,
    /// 本地已经攒下大部分数据，只补缺失的块。
    Resume,
}

/// 决策所需的事实。
///
/// 全部由调用方先探好——`policy` 自己不做任何 IO。`destination_exists` 要问文件系统，
/// `hashes_equal` 要读文件算哈希，`resumable` 要 `stat`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Situation {
    /// 目标路径上已经有东西。
    pub destination_exists: bool,
    /// 本地已有的同名文件与发送方的哈希一致。
    pub hashes_equal: bool,
    /// 这个条目是发送方为文本传输生成的临时文件（见 [`is_text_artifact`]）。
    pub is_text_artifact: bool,
    /// 发送方说这是空文件或符号链接（见 [`is_empty_or_symlink`]）。
    pub is_empty_or_symlink: bool,
    /// 本地已有的同名文件可以续传（见 [`is_resumable`]）。
    pub resumable: bool,
}

/// 用户偏好，对应 `--overwrite` 与 `--rename`。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Preferences {
    /// `--overwrite`：不问，直接覆盖或续传。
    pub overwrite: bool,
    /// `--rename`：目标已存在时换一个没被占用的名字。
    pub rename: bool,
}

/// 接收方对一个文件该做什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// 本地内容与发送方完全一致：不传数据，只把 mtime 对齐过去。
    AlreadyCurrent,
    /// 换一个没被占用的名字（见 [`rename_candidates`]），然后按"目标不存在"处理。
    Rename,
    /// 目标已存在且用户没关掉提示：问一句。`resumable` 决定提示语是 "Resume" 还是
    /// "Overwrite"，以及要不要去扫本地文件算缺失块。
    Ask {
        /// 本地那个文件能不能续传。
        resumable: bool,
    },
    /// 直接覆盖（或续传），不问。
    Proceed {
        /// 本地那个文件能不能续传。
        resumable: bool,
    },
    /// 空文件或符号链接：本地直接建出来就完事，不走数据面。
    CreateDirectly,
    /// 用户在上一次询问里选了"不要"，跳过这个文件。
    Declined,
}

/// 用户对 [`Action::Ask`] 的回答。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// 覆盖 / 续传。
    Accept,
    /// 跳过这个文件。
    Decline,
}

/// 纯决策表。
#[must_use]
pub fn decide(situation: Situation, preferences: Preferences) -> Action {
    if situation.hashes_equal {
        return Action::AlreadyCurrent;
    }
    // 文本传输的临时文件名是接收方生成的（`croc-stdin-*`），不可能撞上用户自己
    // 的文件，所以重命名和询问都跳过。
    let collides = situation.destination_exists && !situation.is_text_artifact;
    if collides && preferences.rename {
        return Action::Rename;
    }
    if collides && !preferences.overwrite {
        return Action::Ask { resumable: situation.resumable };
    }
    if situation.is_empty_or_symlink {
        return Action::CreateDirectly;
    }
    Action::Proceed { resumable: situation.resumable }
}

/// 把 [`Action::Ask`] 的回答并回决策表。
///
/// 对非 `Ask` 的输入原样返回——调用方不必先判断自己手里拿的是不是 `Ask`。
#[must_use]
pub fn resolve(action: Action, answer: Answer) -> Action {
    match (action, answer) {
        (Action::Ask { resumable }, Answer::Accept) => Action::Proceed { resumable },
        (Action::Ask { .. }, Answer::Decline) => Action::Declined,
        (other, _) => other,
    }
}

/// 这个条目是不是发送方为文本传输生成的临时文件。
///
/// Go 要求同时满足"这次是文本传输"和"文件名带 `croc-stdin-` 前缀"。
#[must_use]
pub fn is_text_artifact(sending_text: bool, name: &str) -> bool {
    sending_text && name.starts_with(TEXT_ARTIFACT_PREFIX)
}

/// 这个条目是不是"不用走数据面"的：空文件或符号链接。
#[must_use]
pub fn is_empty_or_symlink(info: &FileInfo) -> bool {
    info.size == 0 || info.is_symlink()
}

/// 本地已有的同名文件能不能续传。
///
/// Go 的条件：发送方给的 `Size > 0`、不是符号链接、且本地那个是普通文件。
#[must_use]
pub fn is_resumable(info: &FileInfo, local_is_regular_file: bool) -> bool {
    info.size > 0 && !info.is_symlink() && local_is_regular_file
}

/// 本地已有数据的百分比，用于提示语里的 `(72.3%)`。
///
/// `total_bytes` 为 0 时返回 100——没什么可传的。
#[must_use]
pub fn completion_percent(missing_bytes: u64, total_bytes: u64) -> f64 {
    if total_bytes == 0 {
        return 100.0;
    }
    // u64 → f64 会丢精度，但这里只是给人看的一位小数百分比，够用。
    #[allow(clippy::cast_precision_loss, reason = "仅用于展示的百分比，精度损失无意义")]
    {
        let missing = missing_bytes.min(total_bytes) as f64;
        let total = total_bytes as f64;
        100.0 - missing / total * 100.0
    }
}

/// 判定为"续传"还是"整个重来"。
///
/// Go 的判据是 `100 - missing/total*100 < 99`，两边化简后等价于
/// `missing * 100 > total`（本地已有超过 1%）。这里用整数比较，避免浮点误差。
#[must_use]
pub fn resume_kind(missing_bytes: u64, total_bytes: u64) -> ResumeKind {
    if total_bytes > 0 && missing_bytes.saturating_mul(100) > total_bytes {
        ResumeKind::Resume
    } else {
        ResumeKind::Overwrite
    }
}

/// `--rename` 时的候选文件名：原名，然后是 `原名 (1)`、`原名 (2)`……
///
/// 扩展名保持不动，序号插在扩展名之前。惰性生成，调用方挨个试 `exists()`。
///
/// # 照抄的怪癖
///
/// Go 的 `path.Ext` 对 `.bashrc` 这种"整名就是扩展名"的文件返回整个名字，于是
/// 第二个候选会变成 `" (1).bashrc"`。这里照抄：它影响的是用户看到的文件名，改掉
/// 会让两个实现的行为对不上，而这种对不上没有任何好处。
pub fn rename_candidates(name: &str) -> impl Iterator<Item = String> + '_ {
    let (base, extension) = split_extension(name);
    std::iter::once(name.to_owned()).chain((1u32..).map(move |index| format!("{base} ({index}){extension}")))
}

/// 按 Go `path.Ext` 的规则切 `(主干, 扩展名)`：扩展名从最后一个 `.` 开始且含点，
/// 没有点则为空串。
fn split_extension(name: &str) -> (&str, &str) {
    // `FileInfo.Name` 是裸文件名，正常不含路径分隔符；真含了也只在最后一段里找点。
    let start = name.rfind(['/', '\\']).map_or(0, |index| index + 1);
    let tail = name.get(start..).unwrap_or_default();
    tail.rfind('.').map_or((name, ""), |dot| {
        let split = start + dot;
        (name.get(..split).unwrap_or(name), name.get(split..).unwrap_or_default())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn situation() -> Situation {
        Situation::default()
    }

    #[test]
    fn identical_content_needs_no_transfer() {
        let action = decide(
            Situation { hashes_equal: true, destination_exists: true, ..situation() },
            Preferences { overwrite: false, rename: true },
        );
        assert_eq!(action, Action::AlreadyCurrent, "哈希一致时连 --rename 都不该触发");
    }

    #[test]
    fn a_missing_destination_just_proceeds() {
        assert_eq!(decide(situation(), Preferences::default()), Action::Proceed { resumable: false });
    }

    #[test]
    fn rename_wins_over_overwrite() {
        // Go 先判 rename，所以两个开关同时给出时选重命名。
        let action = decide(
            Situation { destination_exists: true, ..situation() },
            Preferences { overwrite: true, rename: true },
        );
        assert_eq!(action, Action::Rename);
    }

    #[test]
    fn an_existing_destination_asks_unless_overwrite_is_set() {
        let existing = Situation { destination_exists: true, resumable: true, ..situation() };
        assert_eq!(decide(existing, Preferences::default()), Action::Ask { resumable: true });
        assert_eq!(
            decide(existing, Preferences { overwrite: true, rename: false }),
            Action::Proceed { resumable: true }
        );
    }

    #[test]
    fn text_artifacts_are_never_prompted_for() {
        // 名字是接收方生成的，撞不上用户文件，所以既不改名也不问。
        let artifact = Situation { destination_exists: true, is_text_artifact: true, ..situation() };
        assert_eq!(decide(artifact, Preferences::default()), Action::Proceed { resumable: false });
        assert_eq!(
            decide(artifact, Preferences { overwrite: false, rename: true }),
            Action::Proceed { resumable: false }
        );
    }

    #[test]
    fn empty_files_and_symlinks_bypass_the_data_plane() {
        let empty = Situation { is_empty_or_symlink: true, ..situation() };
        assert_eq!(decide(empty, Preferences::default()), Action::CreateDirectly);
        // 但"目标已存在 + 要问"仍然优先：用户得先同意覆盖。
        let existing = Situation { destination_exists: true, ..empty };
        assert_eq!(decide(existing, Preferences::default()), Action::Ask { resumable: false });
    }

    #[test]
    fn declining_an_ask_skips_the_file() {
        let asked = Action::Ask { resumable: true };
        assert_eq!(resolve(asked, Answer::Accept), Action::Proceed { resumable: true });
        assert_eq!(resolve(asked, Answer::Decline), Action::Declined);
    }

    #[test]
    fn resolving_a_non_ask_action_changes_nothing() {
        for action in [Action::AlreadyCurrent, Action::Rename, Action::CreateDirectly, Action::Declined] {
            assert_eq!(resolve(action, Answer::Accept), action);
            assert_eq!(resolve(action, Answer::Decline), action);
        }
    }

    #[test]
    fn text_artifact_detection_needs_both_halves() {
        assert!(is_text_artifact(true, "croc-stdin-1234"));
        assert!(!is_text_artifact(false, "croc-stdin-1234"), "不是文本传输就不算");
        assert!(!is_text_artifact(true, "report.txt"), "没有前缀就不算");
    }

    #[test]
    fn resumability_matches_the_go_conditions() {
        let normal = FileInfo { size: 100, ..FileInfo::default() };
        assert!(is_resumable(&normal, true));
        assert!(!is_resumable(&normal, false), "本地那个不是普通文件");
        assert!(!is_resumable(&FileInfo { size: 0, ..FileInfo::default() }, true), "空文件");
        let link = FileInfo { size: 100, symlink: "../target".into(), ..FileInfo::default() };
        assert!(!is_resumable(&link, true), "符号链接");
    }

    #[test]
    fn empty_or_symlink_covers_both_cases() {
        assert!(is_empty_or_symlink(&FileInfo::default()));
        assert!(is_empty_or_symlink(&FileInfo { symlink: "x".into(), size: 5, ..FileInfo::default() }));
        assert!(!is_empty_or_symlink(&FileInfo { size: 5, ..FileInfo::default() }));
    }

    #[test]
    fn resume_needs_more_than_one_percent_of_local_data() {
        // Go 的判据是"已有比例 > 1%"。
        assert_eq!(resume_kind(0, 1000), ResumeKind::Overwrite, "一个字节都没有");
        assert_eq!(resume_kind(10, 1000), ResumeKind::Overwrite, "刚好 1%");
        assert_eq!(resume_kind(11, 1000), ResumeKind::Resume);
        assert_eq!(resume_kind(1000, 1000), ResumeKind::Resume, "全都缺时 missing == total，> 1%");
        assert_eq!(resume_kind(0, 0), ResumeKind::Overwrite, "空文件不该除零");
        assert_eq!(resume_kind(1, 1), ResumeKind::Resume);
        assert_eq!(resume_kind(1, 99), ResumeKind::Resume, "1/99 ≈ 1.01%");
        assert_eq!(resume_kind(1, 100), ResumeKind::Overwrite, "1/100 恰好 1%");
    }

    #[test]
    fn completion_percent_matches_the_prompt_text() {
        assert!((completion_percent(0, 1000) - 100.0).abs() < f64::EPSILON);
        assert!((completion_percent(1000, 1000) - 0.0).abs() < f64::EPSILON);
        assert!((completion_percent(277, 1000) - 72.3).abs() < 1e-9);
        assert!((completion_percent(0, 0) - 100.0).abs() < f64::EPSILON);
        // 缺失量超过文件大小时夹到 0，不会算出负数百分比。
        assert!((completion_percent(5000, 1000) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rename_candidates_insert_before_the_extension() {
        let candidates: Vec<String> = rename_candidates("report.tar.gz").take(4).collect();
        assert_eq!(candidates, vec!["report.tar.gz", "report.tar (1).gz", "report.tar (2).gz", "report.tar (3).gz"]);
    }

    #[test]
    fn rename_candidates_handle_a_name_without_an_extension() {
        let candidates: Vec<String> = rename_candidates("README").take(3).collect();
        assert_eq!(candidates, vec!["README", "README (1)", "README (2)"]);
    }

    #[test]
    fn rename_candidates_reproduce_go_ext_on_dotfiles() {
        // `path.Ext(".bashrc") == ".bashrc"`，于是主干是空串。
        let candidates: Vec<String> = rename_candidates(".bashrc").take(2).collect();
        assert_eq!(candidates, vec![".bashrc", " (1).bashrc"]);
    }

    #[test]
    fn rename_candidates_only_look_at_the_last_path_segment() {
        assert_eq!(split_extension("dir.d/file"), ("dir.d/file", ""));
        assert_eq!(split_extension("a/b.txt"), ("a/b", ".txt"));
    }
}
