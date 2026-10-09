//! 清单：这次传输有哪些文件、每个多大、内容哈希、以及分片映射。
//!
//! 清单是整个协议里唯一带元数据的东西，所以它先加密再上传——服务端看不到文件名、
//! 大小、时间与哈希。接收方只有在拿到主密钥之后才能解开它。
//!
//! 文件名必须是单层的（不含目录成分），这样接收方在落盘时不需要构造目录树。

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use unicode_general_category::{GeneralCategory, get_general_category};

use crate::{CHUNK_SIZE, Error, MAX_FILES, VERSION};

/// 一份清单。字段名是单字母，因为整份清单要加密上传，能省则省。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// 格式版本。
    #[serde(rename = "v")]
    pub version: u32,
    /// 分片大小（明文）。
    #[serde(rename = "cs")]
    pub chunk_size: i64,
    /// 文件列表，顺序即分片在整次传输里的顺序。
    #[serde(rename = "f")]
    pub files: Vec<ManifestFile>,
}

/// 清单里的一个文件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    /// 文件名。单层，不含 `/` 或 `\`。
    #[serde(rename = "n")]
    pub name: String,
    /// 字节数。
    #[serde(rename = "s")]
    pub size: i64,
    /// 修改时间（UTC）。
    ///
    /// 用 RFC3339 编解码，与 Go 的 `time.Time` 一致：Go 那边不带哈希的清单字段全部原样
    /// 进 JSON，时间戳的写法（没有小数秒时连小数点都不写）必须一模一样。
    #[serde(rename = "m", with = "time::serde::rfc3339")]
    pub modified: OffsetDateTime,
    /// 内容的 SHA-256，base64url 无填充。
    #[serde(rename = "h")]
    pub sha256: String,
    /// 第一个分片在整次传输里的下标。
    #[serde(rename = "fc")]
    pub first_chunk: i64,
    /// 分片个数。
    #[serde(rename = "cc")]
    pub chunk_count: i64,
}

/// 一个分片的位置。它同时是解这个分片所需的 AAD。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkRef {
    /// 在整次传输里的分片下标。
    pub object_index: i64,
    /// 第几个文件（清单里的下标）。
    pub file_index: i64,
    /// 文件内的第几个分片。
    pub file_chunk: i64,
    /// 明文长度。最后一个分片通常小于 [`CHUNK_SIZE`]。
    pub plain_size: i64,
}

/// 校验一段 JSON 是不是合法清单，是则返回它。
///
/// 与 Go 的 `decodeManifest` 一样：拒绝未知字段，拒绝尾随数据——清单是加密的，
/// 多出来的字段只可能来自版本不匹配或人为构造，静默忽略会让两端对同一份数据有不同理解。
///
/// # Errors
///
/// JSON 不合法、字段不认识、清单自相矛盾（版本/分片大小/名字/哈希/分片映射）。
pub fn decode(plaintext: &[u8], max_bytes: i64) -> Result<Manifest, Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(plaintext);
    let manifest = Manifest::deserialize(&mut deserializer)?;
    // 有尾随数据说明这不是一份完整清单。
    deserializer.end()?;
    validate_manifest(&manifest, max_bytes)?;
    Ok(manifest)
}

/// 按同样的规则校验一份已经解析好的清单。
///
/// 接收方在创建任何文件之前用它把关：一份畸形清单不该让磁盘上留下半个文件。
///
/// # Errors
///
/// 同 [`decode`]，只是少了 JSON 那一层。
pub fn validate_manifest(manifest: &Manifest, max_bytes: i64) -> Result<(), Error> {
    if manifest.version != VERSION {
        return Err(Error::Format(format!("unsupported stored-transfer version {}", manifest.version)));
    }
    if manifest.chunk_size != CHUNK_SIZE {
        return Err(Error::Format(format!("unsupported stored-transfer chunk size {}", manifest.chunk_size)));
    }
    if manifest.files.is_empty() || manifest.files.len() > MAX_FILES {
        return Err(Error::Format(format!("stored transfer must contain between 1 and {MAX_FILES} files")));
    }
    if max_bytes <= 0 {
        return Err(Error::Format("stored-transfer byte limit must be positive".to_owned()));
    }

    let mut keys = Vec::with_capacity(manifest.files.len());
    for (index, entry) in manifest.files.iter().enumerate() {
        if !is_safe_single_name(&entry.name) {
            return Err(Error::Format(format!("stored-transfer file {index} has an unsafe name")));
        }
        let key = collision_key(&entry.name);
        if keys.contains(&key) {
            return Err(Error::Format(format!("stored-transfer manifest has duplicate names: {:?}", entry.name)));
        }
        keys.push(key);
    }

    let mut total = 0i64;
    let mut next_chunk = 0i64;
    for entry in &manifest.files {
        // `entry.size > max_bytes - total` 同时挡住了负数与总量超限，且保证下面的加法不溢出。
        if entry.size < 0 || entry.size > max_bytes - total {
            return Err(Error::Format("stored transfer exceeds the configured byte limit".to_owned()));
        }
        total += entry.size;
        let hash = crate::base64url::decode(&entry.sha256)
            .map_err(|_| Error::Format(format!("stored-transfer file {:?} has an invalid SHA-256 hash", entry.name)))?;
        if hash.len() != 32 {
            return Err(Error::Format(format!("stored-transfer file {:?} has an invalid SHA-256 hash", entry.name)));
        }
        let expected = (entry.size + CHUNK_SIZE - 1) / CHUNK_SIZE;
        if entry.first_chunk != next_chunk || entry.chunk_count != expected {
            return Err(Error::Format(format!("stored-transfer file {:?} has an invalid chunk map", entry.name)));
        }
        next_chunk += expected;
    }
    Ok(())
}

/// 校验一个分片的位置与它的明文长度。
///
/// # Errors
///
/// 下标为负、超出 32 位、或长度不在 `1..=CHUNK_SIZE` 且与给的长度不一致。
pub fn validate_chunk_ref(reference: ChunkRef, plaintext_length: i64) -> Result<(), Error> {
    if reference.object_index < 0 || reference.file_index < 0 || reference.file_chunk < 0 {
        return Err(Error::Format("stored-transfer chunk index cannot be negative".to_owned()));
    }
    let limit = i64::from(u32::MAX);
    if reference.object_index > limit || reference.file_index > limit || reference.file_chunk > limit {
        return Err(Error::Format("stored-transfer chunk index cannot be larger than 2^32-1".to_owned()));
    }
    if reference.plain_size < 1 || reference.plain_size > CHUNK_SIZE || plaintext_length != reference.plain_size {
        return Err(Error::Format("stored-transfer chunk has an invalid plaintext length".to_owned()));
    }
    Ok(())
}

/// 按清单顺序列出每个分片的位置。
///
/// 上传与下载都按这个顺序走，`object_index` 同时也是服务端那边的分片编号。
#[must_use]
pub fn chunk_refs(manifest: &Manifest) -> Vec<ChunkRef> {
    let mut references = Vec::new();
    for (file_index, entry) in manifest.files.iter().enumerate() {
        let file_index = i64::try_from(file_index).unwrap_or(i64::MAX);
        let mut remaining = entry.size;
        for file_chunk in 0..entry.chunk_count {
            let plain_size = remaining.min(CHUNK_SIZE);
            references.push(ChunkRef {
                object_index: entry.first_chunk + file_chunk,
                file_index,
                file_chunk,
                plain_size,
            });
            remaining -= plain_size;
        }
    }
    references
}

/// 文件名是否可以安全地直接落盘。
///
/// 规则抄自 Go 的 `receivefs.Normalize` + `path.Base` 检查，只是这里只需要判定、不需要
/// 归一：能被接受的名字一定不含目录成分，构造不出越界的路径。
///
/// 接收方的文件系统不一定跑在创建它的那台机器上，所以这些检查要照顾所有支持的平台：
/// 反斜杠在 Unix 上只是普通字符，在 Windows 上却是分隔符；Windows 还会静默去掉名字末尾的
/// 点与空格，并把 `CON` 这类名字当设备而不是文件。
fn is_safe_single_name(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    // 反斜杠会被当成目录分隔符，含它的名字就不再是单层。
    if name.contains('\\') || name.contains('/') || name.contains(':') {
        return false;
    }
    // `//host/share` 与 `C:` 这类绝对路径。
    if name.starts_with('/') || has_windows_volume(name) {
        return false;
    }
    validate_component(name) && !forbidden_component(name)
}

/// 是不是以盘符开头（`C:...`）。
fn has_windows_volume(name: &str) -> bool {
    if name.starts_with("//") {
        return true;
    }
    let mut characters = name.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

/// 一个名字在所有支持的平台上都能当普通文件名用吗。
fn validate_component(component: &str) -> bool {
    if !component.chars().all(is_printable) {
        return false;
    }
    // 结尾的点与空格会被 Windows 静默去掉，于是 `a.` 与 `a` 会撞在一起。
    if component.ends_with('.') || component.ends_with(' ') {
        return false;
    }
    let stem = component.split('.').next().unwrap_or(component);
    !is_windows_device_name(stem)
}

/// 字符属于 Go 的 `unicode.IsGraphic && unicode.IsPrint`，也就是字母、记号、数字、标点、
/// 符号，外加 ASCII 空格。
///
/// 挡的是控制字符（终端转义）、格式字符（零宽字符）与私用区字符——它们在文件名里要么
/// 制造歧义，要么在别处被当成指令。
fn is_printable(character: char) -> bool {
    if character == ' ' {
        return true;
    }
    matches!(
        get_general_category(character),
        GeneralCategory::UppercaseLetter
            | GeneralCategory::LowercaseLetter
            | GeneralCategory::TitlecaseLetter
            | GeneralCategory::ModifierLetter
            | GeneralCategory::OtherLetter
            | GeneralCategory::NonspacingMark
            | GeneralCategory::SpacingMark
            | GeneralCategory::EnclosingMark
            | GeneralCategory::DecimalNumber
            | GeneralCategory::LetterNumber
            | GeneralCategory::OtherNumber
            | GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::MathSymbol
            | GeneralCategory::CurrencySymbol
            | GeneralCategory::ModifierSymbol
            | GeneralCategory::OtherSymbol
    )
}

/// Windows 的保留设备名。
///
/// `COM¹` 那种上标写法也要挡：Windows 同样会把它当设备。
fn is_windows_device_name(stem: &str) -> bool {
    let upper = stem.to_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CLOCK$" | "CONIN$" | "CONOUT$") {
        return true;
    }
    let Some(rest) = upper.strip_prefix("COM").or_else(|| upper.strip_prefix("LPT")) else {
        return false;
    };
    rest.len() == 1 && rest.starts_with(|character: char| ('1'..='9').contains(&character))
        || matches!(rest, "\u{b9}" | "\u{b2}" | "\u{b3}")
}

/// 名字里不该由接收方创建的那几个目录。
///
/// 大小写与归一化都不能绕过：`CollisionKey` 先把名字折叠成可移植比较形式，所以 `.SSH`
/// 与 `.ssh` 是一回事。
fn forbidden_component(component: &str) -> bool {
    matches!(collision_key(component).as_str(), ".ssh" | ".git" | ".gnupg")
}

/// 跨文件系统比较用的名字形式：先 NFC 归一，再做 Unicode 大小写折叠。
///
/// 大小写不敏感的文件系统（Windows / macOS 默认）与归一化不敏感的文件系统会把两个不同的
/// 名字指向同一个文件，所以重复检测必须用这个形式——否则 `a.txt` 与 `A.txt` 会互相覆盖。
fn collision_key(name: &str) -> String {
    use unicode_normalization::UnicodeNormalization as _;
    caseless::default_case_fold_str(&name.nfc().collect::<String>())
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::OffsetDateTime;

    fn entry(name: &str, size: i64, first_chunk: i64, chunk_count: i64) -> ManifestFile {
        ManifestFile {
            name: name.to_owned(),
            size,
            modified: OffsetDateTime::UNIX_EPOCH,
            sha256: crate::base64url::encode(&[0u8; 32]),
            first_chunk,
            chunk_count,
        }
    }

    fn manifest(files: Vec<ManifestFile>) -> Manifest {
        Manifest { version: VERSION, chunk_size: CHUNK_SIZE, files }
    }

    #[test]
    fn a_well_formed_manifest_passes() {
        let value = manifest(vec![entry("a.txt", 10, 0, 1), entry("b.txt", 10, 1, 1)]);
        assert!(validate_manifest(&value, 1 << 40).is_ok());
    }

    #[test]
    fn a_manifest_json_round_trips() {
        let value = manifest(vec![entry("a.txt", 10, 0, 1)]);
        let encoded = serde_json::to_vec(&value).expect("编码");
        assert_eq!(decode(&encoded, 1 << 40).expect("解码"), value);
    }

    #[test]
    fn unknown_fields_are_refused() {
        // 多一个字段说明对面按别的版本在写；静默忽略会让两端对同一份清单有不同理解。
        let text = br#"{"v":1,"cs":4194304,"f":[],"extra":1}"#;
        assert!(matches!(decode(text, 1 << 40), Err(Error::Json(_))));
    }

    #[test]
    fn trailing_data_is_refused() {
        let text = br#"{"v":1,"cs":4194304,"f":[]}{"v":1}"#;
        assert!(matches!(decode(text, 1 << 40), Err(Error::Json(_))));
    }

    #[test]
    fn an_empty_file_list_is_refused() {
        assert!(matches!(validate_manifest(&manifest(Vec::new()), 1 << 40), Err(Error::Format(_))));
    }

    #[test]
    fn the_wrong_version_or_chunk_size_is_refused() {
        let mut wrong_version = manifest(vec![entry("a", 1, 0, 1)]);
        wrong_version.version = 2;
        assert!(matches!(validate_manifest(&wrong_version, 1 << 40), Err(Error::Format(_))));

        let mut wrong_chunk = manifest(vec![entry("a", 1, 0, 1)]);
        wrong_chunk.chunk_size = CHUNK_SIZE * 2;
        assert!(matches!(validate_manifest(&wrong_chunk, 1 << 40), Err(Error::Format(_))));
    }

    #[test]
    fn a_chunk_map_that_does_not_add_up_is_refused() {
        // 第二个文件的分片从 0 开始（与第一个重叠），而不是接着上一个。
        let overlapping = manifest(vec![entry("a", 10, 0, 1), entry("b", 10, 0, 1)]);
        assert!(matches!(validate_manifest(&overlapping, 1 << 40), Err(Error::Format(_))));

        // 分片个数与大小对不上。
        let wrong_count = manifest(vec![entry("a", CHUNK_SIZE + 1, 0, 1)]);
        assert!(matches!(validate_manifest(&wrong_count, 1 << 40), Err(Error::Format(_))));
    }

    #[test]
    fn a_total_size_over_the_limit_is_refused() {
        let value = manifest(vec![entry("a", 100, 0, 1)]);
        assert!(matches!(validate_manifest(&value, 99), Err(Error::Format(_))));
        assert!(validate_manifest(&value, 100).is_ok());
    }

    #[test]
    fn unsafe_names_are_refused() {
        for name in [
            "",
            ".",
            "..",
            "a/b",
            "a\\b",
            "/etc/passwd",
            "C:evil",
            "//host/share",
            "a\u{0}b",
            "a\u{7f}b",
            "trailing.",
            "trailing ",
            "CON",
            "com1",
            "LPT9",
            "COM\u{b9}",
            ".ssh",
            ".GIT",
            "a:b",
        ] {
            let value = manifest(vec![entry(name, 1, 0, 1)]);
            assert!(matches!(validate_manifest(&value, 1 << 40), Err(Error::Format(_))), "{name:?} 应当被拒");
        }
    }

    #[test]
    fn ordinary_names_are_accepted() {
        for name in ["a.txt", "报告 2026.pdf", "a b", ".gitignore", "naïve.txt", "COM10", "コピー.txt"] {
            let value = manifest(vec![entry(name, 1, 0, 1)]);
            assert!(validate_manifest(&value, 1 << 40).is_ok(), "{name:?} 应当通过");
        }
    }

    #[test]
    fn names_that_collide_on_a_case_insensitive_filesystem_are_refused() {
        // Windows / macOS 上 `a.txt` 与 `A.txt` 是同一个文件，放过去就是互相覆盖。
        let value = manifest(vec![entry("a.txt", 1, 0, 1), entry("A.txt", 1, 1, 1)]);
        assert!(matches!(validate_manifest(&value, 1 << 40), Err(Error::Format(_))));
    }

    #[test]
    fn names_that_collide_after_normalization_are_refused() {
        // `é` 的组合式与预组合式在归一化敏感的文件系统上是同一个名字。
        let value = manifest(vec![entry("e\u{301}.txt", 1, 0, 1), entry("\u{e9}.txt", 1, 1, 1)]);
        assert!(matches!(validate_manifest(&value, 1 << 40), Err(Error::Format(_))));
    }

    #[test]
    fn chunk_refs_follow_the_manifest_order() {
        let value = manifest(vec![entry("a", CHUNK_SIZE + 5, 0, 2), entry("b", 3, 2, 1)]);
        let references = chunk_refs(&value);
        assert_eq!(references.len(), 3);
        assert_eq!(references[0], ChunkRef { object_index: 0, file_index: 0, file_chunk: 0, plain_size: CHUNK_SIZE });
        assert_eq!(references[1], ChunkRef { object_index: 1, file_index: 0, file_chunk: 1, plain_size: 5 });
        assert_eq!(references[2], ChunkRef { object_index: 2, file_index: 1, file_chunk: 0, plain_size: 3 });
    }

    #[test]
    fn chunk_index_bounds_are_enforced() {
        let ok = ChunkRef { object_index: i64::from(u32::MAX), file_index: 0, file_chunk: 0, plain_size: 1 };
        assert!(validate_chunk_ref(ok, 1).is_ok());

        let negative = ChunkRef { object_index: -1, ..ok };
        assert!(matches!(validate_chunk_ref(negative, 1), Err(Error::Format(_))));

        let too_big = ChunkRef { object_index: i64::from(u32::MAX) + 1, ..ok };
        assert!(matches!(validate_chunk_ref(too_big, 1), Err(Error::Format(_))));

        let zero_size = ChunkRef { plain_size: 0, ..ok };
        assert!(matches!(validate_chunk_ref(zero_size, 0), Err(Error::Format(_))));

        let over_chunk = ChunkRef { plain_size: CHUNK_SIZE + 1, ..ok };
        assert!(matches!(validate_chunk_ref(over_chunk, CHUNK_SIZE + 1), Err(Error::Format(_))));

        let mismatched = ChunkRef { plain_size: 5, ..ok };
        assert!(matches!(validate_chunk_ref(mismatched, 4), Err(Error::Format(_))));
    }

    #[test]
    fn the_protocol_name_is_the_one_go_uses() {
        assert_eq!(crate::PROTOCOL, "croc-store-v1");
    }
}
