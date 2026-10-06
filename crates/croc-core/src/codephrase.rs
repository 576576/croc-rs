//! croc 口令码：生成、解析，以及"码 → 中继房间 + PAKE 口令"的映射。
//!
//! # 三种格式
//!
//! | 格式 | 房间选择子 | PAKE 口令 | 来源 |
//! |------|-----------|-----------|------|
//! | `three-word` | 第 1 个词 | 后 2 个词 | 当前格式，[`generate`] 的产物 |
//! | `four-word` | 前 2 个词 | 后 2 个词 | 历史格式，只为认得出旧码而保留 |
//! | `legacy` | 前 4 个**字节** | 第 6 个**字节**起 | 手输码（`abcd-efgh`） |
//!
//! 房间名不是码本身，而是 `hex(sha256(选择子 + "croc"))`。这样同一个码在任何机器上
//! 都落到同一个房间，而房间名又不直接暴露码。
//!
//! # 那个连字符
//!
//! 词表里有一个词**自身含 `-`**：`yo-yo`（第 1286 行）。于是 `"yo-yo-acorn-acid"` 按
//! 连字符拆是 **4 段**（`["yo", "yo", "acorn", "acid"]`），却要还原成 **3 个词**
//! （`["yo-yo", "acorn", "acid"]`）。Go 用**词表回溯匹配**消解这种歧义——先试第一个词
//! 能有多长，不行就退回来试短的。这里照做，见 [`eff_word_sequence`]。
//!
//! # 与 Go 版的差异
//!
//! 只有一处：Go 的 `Generate` 会把随机源失败作为 `error` 返回，Rust 的
//! [`Randomness`] 是不可失败的（`getrandom` 拿不到随机源时直接 abort，因为继续跑
//! 只会产出不安全的密钥）。所以这里的生成函数不返回 `Result`。

use crate::types::{RoomName, Secret};
use croc_protocol::Randomness;
use sha2::{Digest as _, Sha256};
use std::collections::HashSet;
use std::sync::LazyLock;

/// EFF Short Wordlist #1 的条目数。
pub const EFF_WORD_COUNT: usize = 1296;

/// SSH 邀请码的词数。比普通码多一倍：两个词选房间、四个词留在 PAKE 口令里。
pub const SSH_CODE_WORD_COUNT: usize = 6;

/// 一个码至少要这么多字节，否则旧格式的 `[:4]` / `[5:]` 切分没有意义。
pub const MIN_CODE_LEN: usize = 6;

/// 词与词之间的分隔符。
const SEPARATOR: char = '-';
/// [`SEPARATOR`] 的字符串形态（给 `join` 用）。
const SEPARATOR_STR: &str = "-";
/// 房间名的哈希后缀。改这个值会让所有已有码失效。
const ROOM_HASH_SUFFIX: &str = "croc";
/// SSH 房间名的哈希后缀。和普通码分开，免得两种码撞进同一个房间。
const SSH_ROOM_HASH_SUFFIX: &str = "croc-ssh-v1";
/// 旧格式：房间选择子取前 4 个字节。
const LEGACY_ROOM_SELECTOR_LEN: usize = 4;
/// 旧格式：PAKE 口令从第 6 个字节开始（第 5 个字节是分隔符，被丢掉）。
const LEGACY_PASSPHRASE_START: usize = 5;
/// SSH 码里留给房间的词数。
const SSH_CODE_ROOM_WORD_COUNT: usize = 2;
/// 生成普通码时用的词数。
const CODE_WORD_COUNT: usize = 3;
/// 找一个落在指定中继上的码时，最多试这么多次。
///
/// 命中概率是 `1 / relay_count`，所以只要中继池不是天文数字，这个上限都远远够用；
/// 它的真正作用是**不让一个坏掉的随机源把这里变成死循环**。
const RELAY_SEARCH_LIMIT: u32 = 1 << 16;

/// 编译期已规范化并校验过的词表。
///
/// `build.rs` 负责把 CRLF 折成 LF，并断言行数、大小写与唯一性——词表被改坏会在
/// `cargo build` 就失败，不会拖到运行期。
static WORD_LIST: &str = include_str!(concat!(env!("OUT_DIR"), "/eff-short-wordlist-1.txt"));

/// 词表，按行拆开。长度恒为 [`EFF_WORD_COUNT`]。
fn words() -> &'static [&'static str] {
    static WORDS: LazyLock<Vec<&'static str>> = LazyLock::new(|| WORD_LIST.split('\n').collect());
    &WORDS
}

/// 词表集合，供回溯匹配做 O(1) 查询。
fn word_set() -> &'static HashSet<&'static str> {
    static SET: LazyLock<HashSet<&'static str>> = LazyLock::new(|| words().iter().copied().collect());
    &SET
}

/// 码的切分格式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Format {
    /// 房间取前 4 个字节，PAKE 口令取第 6 个字节起。
    Legacy,
    /// 房间取第 1 个词，PAKE 口令取后 2 个词。
    ThreeWord,
    /// 房间取前 2 个词，PAKE 口令取后 2 个词。历史格式。
    FourWord,
}

impl Format {
    /// 线路/日志里用的名字，与 Go 的 `Format` 常量一致。
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::ThreeWord => "three-word",
            Self::FourWord => "four-word",
        }
    }
}

impl std::fmt::Display for Format {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// 一个普通码解析出来的协议输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Components {
    /// 可以直接发给中继的房间名。
    pub room_name: RoomName,
    /// 参与 PAKE 的口令。
    pub pake_passphrase: Secret,
    /// 码是按哪种格式切开的。
    pub format: Format,
}

/// 一个 SSH 邀请码解析出来的协议输入。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshComponents {
    /// 可以直接发给中继的房间名。
    pub room_name: RoomName,
    /// 参与 PAKE 的口令。
    pub pake_passphrase: Secret,
}

/// 口令码的失败模式。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum CodeError {
    /// 码短于 [`MIN_CODE_LEN`] 个字节。
    #[error("code is too short (must be at least {MIN_CODE_LEN} characters)")]
    TooShort,

    /// 中继池为空。
    #[error("relay count must be positive")]
    InvalidRelayCount,

    /// 指定的中继不在池里。
    #[error("relay index {index} is outside the relay pool of {count}")]
    InvalidRelayIndex {
        /// 请求的下标。
        index: u32,
        /// 池的大小。
        count: u32,
    },

    /// 找不到落在指定中继上的码。
    #[error("no code for relay {index} of {count} was found in {attempts} attempts")]
    RelaySearchExhausted {
        /// 请求的下标。
        index: u32,
        /// 池的大小。
        count: u32,
        /// 已经试过的候选数。
        attempts: u32,
    },

    /// SSH 码不是 [`SSH_CODE_WORD_COUNT`] 个 EFF 词。
    #[error("invalid SSH code: expected {SSH_CODE_WORD_COUNT} EFF words")]
    InvalidSshCode,
}

/// 生成一个三词口令码。
#[must_use]
pub fn generate(randomness: &mut impl Randomness) -> String {
    generate_words(randomness, CODE_WORD_COUNT)
}

/// 生成一个六词 SSH 邀请码。
#[must_use]
pub fn generate_ssh(randomness: &mut impl Randomness) -> String {
    generate_words(randomness, SSH_CODE_WORD_COUNT)
}

/// 生成一个三词码，并保证 [`relay_index`] 恰好等于 `relay_index`。
///
/// 平均要生成 `relay_count` 个候选——所以这个函数只在"用户点名要某个中继"时才用。
///
/// # Errors
///
/// `relay_count` 为 0、`relay_index` 越界，或 [`RELAY_SEARCH_LIMIT`] 次都没撞上时
/// 返回 [`CodeError`]。最后一种只可能发生在随机源坏掉的情况下；Go 那边是无限循环，
/// 这里选择报错而不是挂住。
pub fn generate_for_relay(
    randomness: &mut impl Randomness,
    relay_index: u32,
    relay_count: u32,
) -> Result<String, CodeError> {
    if relay_count == 0 {
        return Err(CodeError::InvalidRelayCount);
    }
    if relay_index >= relay_count {
        return Err(CodeError::InvalidRelayIndex { index: relay_index, count: relay_count });
    }
    for attempt in 0..RELAY_SEARCH_LIMIT {
        let code = generate(randomness);
        if self::relay_index(&code, relay_count)? == relay_index {
            return Ok(code);
        }
        let _ = attempt;
    }
    Err(CodeError::RelaySearchExhausted { index: relay_index, count: relay_count, attempts: RELAY_SEARCH_LIMIT })
}

/// 把一个码确定性地分配到有序中继池中的某一项。
///
/// 用**完整** SHA-256 摘要当大端无符号整数再取模，所以同一份中继列表在任何机器上
/// 都会给出同一个分配。
///
/// # Errors
///
/// `relay_count` 为 0 时返回 [`CodeError::InvalidRelayCount`]。
pub fn relay_index(code: &str, relay_count: u32) -> Result<u32, CodeError> {
    if relay_count == 0 {
        return Err(CodeError::InvalidRelayCount);
    }
    // 逐字节 Horner 取模，等价于 Go 的 `new(big.Int).SetBytes(digest).Mod(count)`：
    // 每读一个字节 `acc ← (acc * 256 + b) mod m`，归纳可知末尾的 `acc` 就是整个大端
    // 整数对 `m` 的余数。因为被模数恒非负，欧几里得取模与 `%` 一致。
    //
    // 累加器恒小于 `relay_count ≤ u32::MAX`，所以 `acc * 256 + 255 < 2^40`，用 `u64`
    // 中转绝不会溢出——这也是为什么这里不需要 `num-bigint`。
    let modulus = u64::from(relay_count);
    let reduced =
        Sha256::digest(code.as_bytes()).iter().fold(0u64, |acc, byte| (acc * 256 + u64::from(*byte)) % modulus);
    // `reduced < modulus ≤ u32::MAX`，这个窄化是恒等的。
    #[allow(clippy::cast_possible_truncation, reason = "reduced < relay_count ≤ u32::MAX")]
    Ok(reduced as u32)
}

/// 解析一个 croc 码。
///
/// # Errors
///
/// 码短于 [`MIN_CODE_LEN`] 个字节时返回 [`CodeError::TooShort`]。
pub fn parse(secret: &str) -> Result<Components, CodeError> {
    if secret.len() < MIN_CODE_LEN {
        return Err(CodeError::TooShort);
    }
    let (selector, passphrase, format) = classify(secret);
    Ok(Components {
        room_name: room_name(&selector, ROOM_HASH_SUFFIX),
        pake_passphrase: Secret::from_bytes(passphrase),
        format,
    })
}

/// 解析一个六词 SSH 邀请码。
///
/// 只认词表里的词，所以即使 `yo-yo` 用了和分隔符一样的连字符，解析也不会有歧义。
///
/// # Errors
///
/// 码不是 [`SSH_CODE_WORD_COUNT`] 个 EFF 词时返回 [`CodeError::InvalidSshCode`]。
pub fn parse_ssh(secret: &str) -> Result<SshComponents, CodeError> {
    let words = eff_word_sequence(secret, SSH_CODE_WORD_COUNT).ok_or(CodeError::InvalidSshCode)?;
    let (selector, passphrase) = split_words(&words, SSH_CODE_ROOM_WORD_COUNT).unwrap_or_default();
    Ok(SshComponents {
        room_name: room_name(&selector, SSH_ROOM_HASH_SUFFIX),
        pake_passphrase: Secret::from_bytes(passphrase),
    })
}

/// 按格式把码切成 `(房间选择子, PAKE 口令, 格式)`，全部是原始字节。
///
/// 判定顺序与 Go 完全一致：三词 EFF → 三词小写 → 四词小写 → 旧格式。
/// 注意**没有**"四词 EFF"这一步——`eff_word_sequence` 只对 3 个词尝试。
fn classify(secret: &str) -> (Vec<u8>, Vec<u8>, Format) {
    let three = eff_word_sequence(secret, CODE_WORD_COUNT).or_else(|| lowercase_words(secret, CODE_WORD_COUNT));
    if let Some((selector, passphrase)) = three.as_deref().and_then(|words| split_words(words, 1)) {
        return (selector, passphrase, Format::ThreeWord);
    }
    let four = lowercase_words(secret, CODE_WORD_COUNT + 1);
    if let Some((selector, passphrase)) = four.as_deref().and_then(|words| split_words(words, 2)) {
        return (selector, passphrase, Format::FourWord);
    }
    // 旧格式按字节切。这里刻意不按字符边界切：Go 就是裸字节切片，`"abcdé-fgh"` 的
    // 口令会从 `é` 的第二个字节开始。
    (
        byte_slice(secret, 0, LEGACY_ROOM_SELECTOR_LEN).to_vec(),
        byte_slice(secret, LEGACY_PASSPHRASE_START, secret.len()).to_vec(),
        Format::Legacy,
    )
}

/// 把词序列切成 `(选择子, PAKE 口令)`：前 `selector_words` 个词进选择子，其余用 `-`
/// 拼成口令。词数不够时返回 `None`。
fn split_words(words: &[String], selector_words: usize) -> Option<(Vec<u8>, Vec<u8>)> {
    let selector = words.get(..selector_words)?;
    let passphrase = words.get(selector_words..)?;
    Some((selector.join(SEPARATOR_STR).into_bytes(), passphrase.join(SEPARATOR_STR).into_bytes()))
}

/// `hex(sha256(选择子 ‖ 后缀))`。
fn room_name(selector: &[u8], suffix: &str) -> RoomName {
    let mut hasher = Sha256::new();
    hasher.update(selector);
    hasher.update(suffix.as_bytes());
    RoomName::new(hex::encode(hasher.finalize()))
}

/// 按字节切 `&str`。越界时截到末尾。
///
/// Go 的切片越界会 panic，但这里的调用点都已经检查过 `secret.len() >= MIN_CODE_LEN`，
/// 所以两端的下标一定落在范围内。
fn byte_slice(value: &str, start: usize, end: usize) -> &[u8] {
    let bytes = value.as_bytes();
    let start = start.min(bytes.len());
    let end = end.clamp(start, bytes.len());
    bytes.get(start..end).unwrap_or_default()
}

/// 生成 `count` 个词拼成的码。
fn generate_words(randomness: &mut impl Randomness, count: usize) -> String {
    let chosen: Vec<&str> = (0..count).map(|_| random_word(randomness)).collect();
    chosen.join(SEPARATOR_STR)
}

/// 从词表里均匀取一个词。
///
/// Go 用 `crypto/rand.Int`（拒绝采样，避免取模偏置），这里同样拒绝采样：先取 8 字节，
/// 落在 `[0, UNIFORM_LIMIT)` 内才收，而 `UNIFORM_LIMIT` 是 [`EFF_WORD_COUNT`] 的整数倍。
/// 8 字节下的拒绝概率约 `1296 / 2^64`，实际不会发生——重试上限只是防止一个病态的
/// [`Randomness`] 实现（比如恒返回 `0xFF`）把这里变成死循环。
fn random_word(randomness: &mut impl Randomness) -> &'static str {
    let mut buffer = [0u8; 8];
    for _ in 0..REJECTION_ATTEMPTS {
        randomness.fill(&mut buffer);
        let value = u64::from_le_bytes(buffer);
        if value < UNIFORM_LIMIT {
            return word_at(value % EFF_WORD_COUNT_U64);
        }
    }
    // 兜底：接受取模偏置。能出码比卡住强，而走到这里说明随机源已经不可用了。
    word_at(u64::from_le_bytes(buffer) % EFF_WORD_COUNT_U64)
}

/// [`random_word`] 的重试上限。
const REJECTION_ATTEMPTS: usize = 64;

/// `2^64` 以内 `EFF_WORD_COUNT` 的最大整数倍：落在 `[0, UNIFORM_LIMIT)` 的值取模后
/// 每个词的概率完全相同。
const UNIFORM_LIMIT: u64 = (u64::MAX / EFF_WORD_COUNT_U64) * EFF_WORD_COUNT_U64;

/// [`EFF_WORD_COUNT`] 的 `u64` 形态。`usize` 转 `u64` 是宽化，`const` 里直接写。
const EFF_WORD_COUNT_U64: u64 = 1296;

/// 按下标取词。下标由 [`random_word`] 保证落在 `[0, EFF_WORD_COUNT)` 内，而词表长度
/// 由 `build.rs` 保证正是 [`EFF_WORD_COUNT`]。
#[expect(clippy::indexing_slicing, reason = "下标由调用方约束在词表长度内")]
fn word_at(index: u64) -> &'static str {
    words()[index as usize]
}

/// 把码切成 `count` 个 EFF 词。
///
/// 因为词表里有 `yo-yo`，切分必须回溯：先试最长的第一个词，不行再退回来试短的。
/// 词段数必须落在 `[count, count * 2]` 内——下界是"每段一个词"，上界是"每个词都被
/// 连字符劈成两段"。
fn eff_word_sequence(secret: &str, count: usize) -> Option<Vec<String>> {
    if count == 0 {
        return None;
    }
    let parts: Vec<&str> = secret.split(SEPARATOR).collect();
    if parts.len() < count || parts.len() > count.saturating_mul(2) {
        return None;
    }
    let mut chosen: Vec<String> = Vec::with_capacity(count);
    backtrack(&parts, &mut chosen, 0, count).then_some(chosen)
}

/// [`eff_word_sequence`] 的回溯核心：从 `parts[start..]` 里再挑出
/// `count - chosen.len()` 个词，恰好用完所有词段时成功。
fn backtrack(parts: &[&str], chosen: &mut Vec<String>, start: usize, count: usize) -> bool {
    if chosen.len() == count {
        return start == parts.len();
    }
    for end in start + 1..=parts.len() {
        let Some(candidate) = parts.get(start..end).map(|slice| slice.join(SEPARATOR_STR)) else {
            continue;
        };
        if !word_set().contains(candidate.as_str()) {
            continue;
        }
        chosen.push(candidate);
        if backtrack(parts, chosen, end, count) {
            return true;
        }
        chosen.pop();
    }
    false
}

/// 把码切成恰好 `count` 段纯小写字母的词。
fn lowercase_words(secret: &str, count: usize) -> Option<Vec<String>> {
    let parts: Vec<&str> = secret.split(SEPARATOR).collect();
    (parts.len() == count && parts.iter().all(|part| is_lowercase_word(part)))
        .then(|| parts.into_iter().map(str::to_owned).collect())
}

/// 非空，且全部是小写 ASCII 字母。
///
/// 与 Go 的 `isLowercaseWord` 等价：Go 按 rune 比较 `'a'..='z'`，这里按字节比较
/// `is_ascii_lowercase`，非 ASCII 字节两边都判否。
fn is_lowercase_word(word: &str) -> bool {
    !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 一个确定性的假随机源：把内部计数器按 `LE` 字节填出去。
    ///
    /// 这不是密码学安全的，只用来让"生成"这一步在测试里可复现。
    #[derive(Default)]
    struct Counter(u64);

    impl Randomness for Counter {
        fn fill(&mut self, buffer: &mut [u8]) {
            for chunk in buffer.chunks_mut(8) {
                self.0 = self.0.wrapping_add(1);
                let bytes = self.0.to_le_bytes();
                chunk.copy_from_slice(&bytes[..chunk.len()]);
            }
        }
    }

    fn random() -> Counter {
        Counter::default()
    }

    #[test]
    fn the_word_list_is_the_eff_short_list() {
        assert_eq!(words().len(), EFF_WORD_COUNT);
        assert_eq!(word_set().len(), EFF_WORD_COUNT, "词表不该有重复词");
        // 这个连字符词是回溯解析存在的唯一理由，钉住它。
        assert!(word_set().contains("yo-yo"));
        assert_eq!(words().get(1285).copied(), Some("yo-yo"), "yo-yo 应在第 1286 行");
    }

    #[test]
    fn generated_codes_have_three_words() {
        let mut randomness = random();
        for _ in 0..32 {
            let code = generate(&mut randomness);
            let components = parse(&code).expect("生成的码应能解析");
            assert_eq!(components.format, Format::ThreeWord);
            // 词段数是 3，除非恰好抽到 `yo-yo`——那个词会让段数变成 4。
            assert_eq!(eff_word_sequence(&code, 3).map(|words| words.len()), Some(3));
            assert!(code.split(SEPARATOR).count() >= 3);
        }
    }

    #[test]
    fn generated_codes_are_distinct() {
        let mut randomness = random();
        let codes: HashSet<String> = (0..64).map(|_| generate(&mut randomness)).collect();
        assert_eq!(codes.len(), 64, "计数器随机源不该产出重复码");
    }

    #[test]
    fn ssh_codes_have_six_words() {
        let mut randomness = random();
        let code = generate_ssh(&mut randomness);
        assert_eq!(code.split(SEPARATOR).count(), SSH_CODE_WORD_COUNT);
        let components = parse_ssh(&code).expect("生成的 SSH 码应能解析");
        assert_eq!(components.room_name.as_str().len(), 64);
    }

    #[test]
    fn parse_uses_the_first_word_for_the_room() {
        let components = parse("acid-acorn-acre").expect("三词码");
        assert_eq!(components.format, Format::ThreeWord);
        assert_eq!(components.pake_passphrase.as_bytes(), b"acorn-acre");
        // 房间名必须是 sha256("acid" + "croc") 的十六进制。
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(b"acid");
            hasher.update(b"croc");
            hex::encode(hasher.finalize())
        };
        assert_eq!(components.room_name.as_str(), expected);
    }

    #[test]
    fn parse_recognises_the_hyphenated_word() {
        // "yo-yo" 用的是和分隔符一样的连字符：三个词的切分只能是 yo-yo / <词> / <词>。
        let components = parse("yo-yo-acorn-acid").expect("含 yo-yo 的三词码");
        assert_eq!(components.format, Format::ThreeWord);
        assert_eq!(components.pake_passphrase.as_bytes(), b"acorn-acid");
    }

    #[test]
    fn parse_falls_back_to_the_four_word_format() {
        // 四个词里只要有一个不在词表里，就落到"四词小写"这一档。
        let components = parse("aaaa-bbbb-cccc-dddd").expect("四词码");
        assert_eq!(components.format, Format::FourWord);
        assert_eq!(components.pake_passphrase.as_bytes(), b"cccc-dddd");
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(b"aaaa-bbbb");
            hasher.update(b"croc");
            hex::encode(hasher.finalize())
        };
        assert_eq!(components.room_name.as_str(), expected);
    }

    #[test]
    fn parse_falls_back_to_the_legacy_split() {
        // "abcd-efgh" 是 9 个字节：房间取 [:4]，口令取 [5:]。
        let components = parse("abcd-efgh").expect("旧格式码");
        assert_eq!(components.format, Format::Legacy);
        assert_eq!(components.pake_passphrase.as_bytes(), b"efgh");
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(b"abcd");
            hasher.update(b"croc");
            hex::encode(hasher.finalize())
        };
        assert_eq!(components.room_name.as_str(), expected);
    }

    #[test]
    fn the_legacy_split_is_byte_based_not_char_based() {
        // "abcdé-fgh"：é 占第 5、6 两个字节，所以口令从 é 的第二个字节开始。
        // 这不是 bug，是 Go 的既有行为——按字符切会让两边算出不同的 PAKE 口令。
        let components = parse("abcdé-fgh").expect("旧格式码");
        assert_eq!(components.format, Format::Legacy);
        assert_eq!(components.pake_passphrase.as_bytes(), &[0xa9, b'-', b'f', b'g', b'h']);
    }

    #[test]
    fn short_codes_are_rejected() {
        assert_eq!(parse("abcde"), Err(CodeError::TooShort));
        assert_eq!(parse(""), Err(CodeError::TooShort));
        assert!(parse("abcdef").is_ok());
    }

    #[test]
    fn parse_ssh_requires_exactly_six_eff_words() {
        assert_eq!(parse_ssh("acid-acorn-acre"), Err(CodeError::InvalidSshCode));
        assert_eq!(parse_ssh("aaaa-bbbb-cccc-dddd-eeee-ffff"), Err(CodeError::InvalidSshCode));
        assert!(parse_ssh("acid-acorn-acre-acts-afar-affix").is_ok());
    }

    #[test]
    fn ssh_rooms_do_not_collide_with_file_transfer_rooms() {
        let ssh = parse_ssh("acid-acorn-acre-acts-afar-affix").expect("SSH 码");
        let plain = parse("acid-acorn-acre").expect("三词码");
        assert_ne!(ssh.room_name, plain.room_name, "后缀不同就不该落进同一个房间");
        // 但房间选择子相同：都是前两个词拼起来。
        let expected = {
            let mut hasher = Sha256::new();
            hasher.update(b"acid-acorn");
            hasher.update(b"croc-ssh-v1");
            hex::encode(hasher.finalize())
        };
        assert_eq!(ssh.room_name.as_str(), expected);
    }

    #[test]
    fn relay_index_is_deterministic_and_in_range() {
        for code in ["acid-acorn-acre", "yo-yo-acorn-acid", "abcd-efgh"] {
            let first = relay_index(code, 7).expect("非零中继数");
            assert!(first < 7);
            assert_eq!(relay_index(code, 7), Ok(first));
        }
    }

    #[test]
    fn relay_index_uses_the_whole_digest() {
        // 用 `num-bigint` 独立算一遍：只取前几个字节的取模会得到不同的结果，
        // 这里把"用完整 32 字节摘要"这件事钉住。
        use num_bigint::BigInt;
        use num_traits::ToPrimitive as _;

        let digest = Sha256::digest(b"acid-acorn-acre");
        let expected = {
            let value = BigInt::from_bytes_be(num_bigint::Sign::Plus, &digest);
            let modulus = BigInt::from(5u32);
            (value % &modulus).to_u32().unwrap_or(0)
        };
        assert_eq!(relay_index("acid-acorn-acre", 5), Ok(expected));
    }

    #[test]
    fn relay_index_rejects_an_empty_pool() {
        assert_eq!(relay_index("acid-acorn-acre", 0), Err(CodeError::InvalidRelayCount));
    }

    #[test]
    fn generate_for_relay_lands_on_the_requested_relay() {
        let mut randomness = random();
        let code = generate_for_relay(&mut randomness, 3, 8).expect("应能找到");
        assert_eq!(relay_index(&code, 8), Ok(3));
    }

    #[test]
    fn generate_for_relay_validates_its_arguments() {
        let mut randomness = random();
        assert_eq!(generate_for_relay(&mut randomness, 0, 0), Err(CodeError::InvalidRelayCount));
        assert_eq!(generate_for_relay(&mut randomness, 8, 8), Err(CodeError::InvalidRelayIndex { index: 8, count: 8 }));
    }

    #[test]
    fn generate_for_relay_gives_up_on_a_broken_random_source() {
        /// 恒返回 `0xFF` 的随机源：候选永远是同一个码。
        struct Stuck;

        impl Randomness for Stuck {
            fn fill(&mut self, buffer: &mut [u8]) {
                buffer.fill(0xff);
            }
        }

        // 恒返回 `0xFF` 的随机源：拒绝采样永远失败，于是每次都落到同一个兜底词上，
        // 候选码恒为同一个。挑一个"它没落上去"的中继，循环就会跑满上限。
        let stuck_code = generate(&mut Stuck);
        let stuck_index = relay_index(&stuck_code, 4).expect("非零中继数");
        let target = (stuck_index + 1) % 4;
        assert_eq!(
            generate_for_relay(&mut Stuck, target, 4),
            Err(CodeError::RelaySearchExhausted { index: target, count: 4, attempts: RELAY_SEARCH_LIMIT })
        );
    }

    #[test]
    fn backtracking_handles_the_hyphenated_word() {
        // `yo-yo` 和分隔符用的是同一个连字符，所以 "yo-yo-acorn-acid" 拆成 4 段却要
        // 还原成 3 个词：只有把前两段合起来才全部命中词表。
        let words = eff_word_sequence("yo-yo-acorn-acid", 3).expect("应能切出三个词");
        assert_eq!(words, vec!["yo-yo", "acorn", "acid"]);
        // 词段数超出 [count, count*2] 时直接放弃。
        assert!(eff_word_sequence("a-b-c-d-e-f-g", 3).is_none());
        assert!(eff_word_sequence("acid", 3).is_none());
    }

    #[test]
    fn lowercase_words_rejects_non_ascii_and_empty_parts() {
        assert!(lowercase_words("aa-bb-cc", 3).is_some());
        assert!(lowercase_words("aa-bb-cc", 4).is_none());
        assert!(lowercase_words("aa--cc", 3).is_none());
        assert!(lowercase_words("aa-Bb-cc", 3).is_none());
        assert!(lowercase_words("aa-bé-cc", 3).is_none());
    }

    #[test]
    fn format_names_match_the_go_constants() {
        assert_eq!(Format::Legacy.as_str(), "legacy");
        assert_eq!(Format::ThreeWord.as_str(), "three-word");
        assert_eq!(Format::FourWord.as_str(), "four-word");
    }
}
