//! 文件系统那一侧：清单收集、哈希、断点扫描、落盘。
//!
//! 这是"副作用边缘"里最脏的一块，所以它被完整地关在这个模块里——状态机
//! （`croc-core::session`）只看到 [`FileInfo`] 与分片区间，看不到 `PathBuf`。
//!
//! # 目录怎么映射
//!
//! 送一个目录 `photos/`，里面的 `a/b.jpg` 在接收方会落在
//! `<out>/photos/a/b.jpg`。做法是：
//!
//! - `folder_source` = 用户给出的那个路径（读文件时拼绝对路径用）；
//! - `folder_remote` = **目录名 + 相对子目录 + 尾斜杠**（接收方建目录用），
//!   例如 `photos/`、`photos/a/`；单个文件是 `./`；
//! - `name` = 裸文件名。
//!
//! 这与 Go 的 `FileInfo{FolderSource, FolderRemote, Name}` 是同一套约定。**注意
//! `folder_remote` 里含目录自己的名字**——Go 的算法是
//! `TrimPrefix(Dir(pathName), Dir(absPath) + "/") + "/"`，基准取的是目录的*父*目录。
//! 少这一层的话，接收方会把整个目录的内容平铺到 `<out>/` 下，一个 `photos/` 传过去
//! 变成"散了一地"。
//!
//! # 对端给的路径不可信
//!
//! 接收方的 `folder_remote` / `name` 是**对端写在清单里的字符串**。落到本地之前一律
//! 过 [`safe_folder`] / [`safe_name`]：拒绝绝对路径与 `..`，因为
//! `PathBuf::join` 遇到绝对路径会整个替换前缀（`out_dir.join("/etc/")` 就是 `/etc/`）。

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use croc_core::chunk::{self, CHUNK_SIZE, ChunkRanges};
use croc_core::policy::{self, Action, Preferences, Situation};
use croc_protocol::{FileInfo, ModTime, SenderInfo};

use crate::error::EngineError;
use crate::hash;

/// 一个待发送条目在磁盘上的位置，以及它的线路元信息。
#[derive(Debug, Clone)]
pub struct SourceEntry {
    /// 真实路径。
    pub path: PathBuf,
    /// 线路元信息。
    pub info: FileInfo,
}

/// 收集待发送条目，并顺手算好哈希。
///
/// 哈希在这一步就算完（Go 的"急切 xxhash"路径），因为接收方要靠它决定
/// "本地已有同名文件是不是就是这份"。流式边传边算会让接收方拿不到判定依据。
///
/// # Errors
///
/// 路径不存在、读不了、或者目录里出现了不支持的条目（FIFO、设备节点等）。
pub fn collect(paths: &[PathBuf], algorithm: &str) -> Result<(Vec<SourceEntry>, Vec<FileInfo>), EngineError> {
    let mut files = Vec::new();
    let mut empty_folders = Vec::new();

    for path in paths {
        let metadata = fs::symlink_metadata(path).map_err(|error| EngineError::file(path, error))?;
        if metadata.is_dir() {
            // **先补成绝对路径**再往下走。`folder_remote` 的算法要"目录名 + 相对子目录"，
            // 而 `.` / `..` 这类路径取不出目录名；Go 同样是先 `filepath.Abs()`。
            let root = std::path::absolute(path).map_err(|error| EngineError::file(path, error))?;
            collect_directory(&root, &root, &mut files, &mut empty_folders, algorithm)?;
        } else {
            // 单个文件：Go 给的是 `"./"`（见 `croc.go:1178`）。
            files.push(entry_for(path, "./", algorithm)?);
        }
    }

    Ok((files, empty_folders))
}

/// 递归收集一个目录。`root` 与 `directory` 都是绝对路径。
fn collect_directory(
    root: &Path,
    directory: &Path,
    files: &mut Vec<SourceEntry>,
    empty_folders: &mut Vec<FileInfo>,
    algorithm: &str,
) -> Result<(), EngineError> {
    let entries = fs::read_dir(directory).map_err(|error| EngineError::file(directory, error))?;
    let mut names: Vec<PathBuf> = entries.filter_map(Result::ok).map(|entry| entry.path()).collect();
    // 排序：清单顺序决定传输顺序，稳定的顺序让日志与进度条可读，也让重试可复现。
    names.sort();

    let base = remote_base(root);
    for path in names {
        let metadata = fs::symlink_metadata(&path).map_err(|error| EngineError::file(&path, error))?;
        if metadata.is_dir() {
            let before = files.len() + empty_folders.len();
            collect_directory(root, &path, files, empty_folders, algorithm)?;
            if files.len() + empty_folders.len() == before {
                // 空目录（或只有空目录）也要在对端建出来。`folder_remote` 里**含它自己
                // 的名字**，接收方直接 `mkdir -p` 就对了（Go 的 `croc.go:1242`）。
                empty_folders.push(folder_info(&path, &base)?);
            }
            continue;
        }
        // 文件的 `folder_remote` 取**父目录**相对基准的路径——所以直接子文件拿到的是
        // `photos/`，而不是 `photos/a.jpg`。
        let parent = path.parent().unwrap_or(root);
        files.push(entry_for(&path, &remote_folder_of(&base, parent), algorithm)?);
    }
    Ok(())
}

/// `folder_remote` 的基准：**用户给出的那个目录的父目录**。
///
/// 取父目录而不是目录本身，是为了让 `folder_remote` 天然带上目录自己的名字。
fn remote_base(root: &Path) -> PathBuf {
    root.parent().map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// `target` 相对 `base` 的路径，斜杠分隔，**末尾带一个斜杠**（Go 的约定）。
///
/// 只保留 `Normal` 段：`strip_prefix` 失手时（例如用户直接把文件系统根 `/` 当路径送）
/// `relative` 会是绝对路径，照抄过去就是 `/a/`——对端会当"想写到接收目录外面"拒掉。
/// 这里宁可产出空串，让接收方的路径检查去报错，也不产出一个我们明知不该发的字符串。
fn remote_folder_of(base: &Path, target: &Path) -> String {
    let relative = target.strip_prefix(base).unwrap_or(target);
    let text = relative
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(segment) => Some(segment.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/");
    if text.is_empty() { "/".to_owned() } else { format!("{text}/") }
}

/// 构造一个普通文件（或符号链接）的条目。
fn entry_for(path: &Path, folder_remote: &str, algorithm: &str) -> Result<SourceEntry, EngineError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| EngineError::file(path, error))?;
    let name = file_name(path)?;

    if metadata.file_type().is_symlink() {
        let target = fs::read_link(path).map_err(|error| EngineError::file(path, error))?;
        return Ok(SourceEntry {
            path: path.to_path_buf(),
            info: FileInfo {
                name,
                folder_remote: folder_remote.to_owned(),
                folder_source: parent_of(path),
                size: 0,
                symlink: target.to_string_lossy().into_owned(),
                mode: mode_of(&metadata),
                ..FileInfo::default()
            },
        });
    }

    if !metadata.is_file() {
        return Err(EngineError::Config(format!(
            "{} is neither a regular file nor a directory（FIFO / 设备节点暂不支持）",
            path.display()
        )));
    }

    // 走 `hash_file` 而不是"读全文再喂给 `hash_for_algorithm`"：imohash 只采样头/中/尾，
    // 把整个文件读进内存等于把它唯一的优点抹掉，大文件上还多占一份内存。
    let digest = hash::hash_file(algorithm, path)?;

    Ok(SourceEntry {
        path: path.to_path_buf(),
        info: FileInfo {
            name,
            folder_remote: folder_remote.to_owned(),
            folder_source: parent_of(path),
            hash: digest,
            size: i64::try_from(metadata.len()).unwrap_or(i64::MAX),
            mod_time: metadata
                .modified()
                .ok()
                .map_or(ModTime::ZERO, |time| ModTime::new(time::OffsetDateTime::from(time))),
            mode: mode_of(&metadata),
            prepared: true,
            ..FileInfo::default()
        },
    })
}

/// 空目录的元信息（`size == 0`，带目录位）。
///
/// `folder_remote` 用**目录自己**相对基准的路径，也就是**含它自己的名字**——接收方
/// 直接 `mkdir -p` 就够了（Go 的 `croc.go:1242`）。
fn folder_info(path: &Path, base: &Path) -> Result<FileInfo, EngineError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| EngineError::file(path, error))?;
    Ok(FileInfo {
        name: file_name(path)?,
        folder_remote: remote_folder_of(base, path),
        folder_source: parent_of(path),
        size: 0,
        mode: mode_of(&metadata) | croc_protocol::payload::MODE_DIR,
        ..FileInfo::default()
    })
}

/// 把收集结果组装成线路上的清单。
#[must_use]
/// 构造清单。
///
/// `algorithm` 是 `--hash` 给的算法名，**必须原样告诉对端**：接收方按清单里的名字
/// 重算哈希来比对。写死成 `xxhash` 的话，用 `--hash md5` 发送时接收方会拿 md5 的
/// 结果去比 xxhash 的摘要，于是每个文件都判成"内容不同"而整个重传——不报错，只是
/// 白传一遍。
pub fn sender_info(
    files: &[SourceEntry],
    empty_folders: Vec<FileInfo>,
    machine_id: &str,
    algorithm: &str,
) -> SenderInfo {
    SenderInfo {
        total_number_folders: folder_count(files, &empty_folders),
        files_to_transfer: files.iter().map(|entry| entry.info.clone()).collect(),
        empty_folders_to_transfer: empty_folders,
        machine_id: machine_id.to_owned(),
        hash_algorithm: croc_protocol::payload::HashAlgorithm::from(algorithm),
        // **必须声明这一个能力**（Go 的 `croc.go:3112` 也只放它）。
        // 接收方靠它决定"按文件标志压缩"还是退回"一律压缩"的老对端行为——
        // 不声明的话，Go 的接收方会把每个块都当压缩的来解，而我们的清单里
        // 写的是 `c:false`，于是解压失败、写盘越界。
        features: vec![croc_protocol::Feature::PerFileCompression.as_str().to_owned()],
        ..SenderInfo::default()
    }
}

/// 清单里有几个目录。
///
/// 数的是**去重后的目录路径**，而且**包括用户给出的那个根目录**——Go 在 `filepath.Walk`
/// 里每遇到一个目录就 `totalNumberFolders++`（`croc.go:1236`），根目录也算。
/// `tree/sub/deep/c.txt` 一条记录就隐含 `tree`、`tree/sub`、`tree/sub/deep` 三层。
///
/// 这个数在接收方只用于显示，但 Go 的 `validateSendingTextOffer()` 会拿它做校验
/// （文本传输必须恰好 0 个目录），所以不能随手填 0。
fn folder_count(files: &[SourceEntry], empty_folders: &[FileInfo]) -> i64 {
    let mut seen = std::collections::BTreeSet::new();
    let folders = files
        .iter()
        .map(|entry| entry.info.folder_remote.as_str())
        .chain(empty_folders.iter().map(|info| info.folder_remote.as_str()));

    for folder in folders {
        let trimmed = folder.trim_matches('/');
        if trimmed.is_empty() || trimmed == "." {
            continue;
        }
        let mut prefix = String::new();
        for segment in trimmed.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(segment);
            seen.insert(prefix.clone());
        }
    }
    i64::try_from(seen.len()).unwrap_or(i64::MAX)
}

/// 扫一个本地文件，返回"哪些块还是全零"（= 还缺哪些）。
///
/// Go 的断点续传没有状态文件：接收方先把文件 `truncate` 到完整大小，已收到的部分写在
/// 正确偏移上，没收到的部分保持全零。于是"扫一遍零块"就等于"扫一遍缺失块"。
///
/// # Errors
///
/// 打不开或读不了时返回错误。**注意这与 Go 不同**：Go 的 `MissingChunks` 在失败时
/// 返回空列表（= 整个文件），静默退化成"重传一遍"。这里选择报错，因为一个读不了的文件
/// 后续写入也会失败，早报比晚报好。
pub fn scan_missing(path: &Path, size: u64) -> Result<ChunkRanges, EngineError> {
    let mut file = File::open(path).map_err(|error| EngineError::file(path, error))?;
    let mut buffer = vec![0u8; usize::try_from(CHUNK_SIZE).unwrap_or(32 * 1024)];
    let count = chunk::chunk_count(size, CHUNK_SIZE);
    let mut flags = Vec::with_capacity(usize::try_from(count).unwrap_or(0));

    for index in 0..count {
        let offset = index.saturating_mul(CHUNK_SIZE);
        let want = usize::try_from((size - offset).min(CHUNK_SIZE)).unwrap_or(buffer.len());
        let slice = buffer.get_mut(..want).ok_or_else(|| EngineError::file(path, "chunk buffer underflow"))?;
        file.seek(SeekFrom::Start(offset)).map_err(|error| EngineError::file(path, error))?;
        match file.read_exact(slice) {
            Ok(()) => flags.push(chunk::is_all_zero(slice)),
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                // 文件比声明的短：剩下的都算缺。
                flags.push(true);
            }
            Err(error) => return Err(EngineError::file(path, error)),
        }
    }
    Ok(chunk::missing_ranges(flags, CHUNK_SIZE))
}

/// 接收方对一个文件的处理方案。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// 本地内容与发送方一致：不传，只对齐 mtime。
    AlreadyCurrent,
    /// 目标已存在且用户选了重命名：用这个新名字。
    Renamed(String),
    /// 空文件或符号链接：本地直接造出来。
    CreateDirectly,
    /// 用户拒绝了这个文件。
    Declined,
    /// 正常收：`ranges` 是要请求的块（空 = 整个文件）。
    Receive(ChunkRanges),
}

/// 为一个文件做决策。
///
/// 决策本身是纯的（[`croc_core::policy::decide`]），这里只负责把文件系统的事实探好
/// 再喂给它——这正是把策略抽成纯函数换来的东西。
///
/// `algorithm` 是**清单里**的哈希算法（`SenderInfo::hash_algorithm`）：判断"本地这个
/// 同名文件是不是就是要传的那份"必须用发送方算摘要时的同一个算法，否则两边永远对不上。
///
/// # Errors
///
/// 探测目标路径时出错。
pub fn plan(
    info: &FileInfo,
    out_dir: &Path,
    preferences: Preferences,
    sending_text: bool,
    algorithm: &str,
    accepted: bool,
) -> Result<Plan, EngineError> {
    let destination = destination_of(info, out_dir)?;
    let metadata = fs::symlink_metadata(&destination);
    let destination_exists = metadata.is_ok();
    let local_is_regular = metadata.as_ref().is_ok_and(std::fs::Metadata::is_file);
    let same_size = metadata.as_ref().is_ok_and(|m| i64::try_from(m.len()).ok() == Some(info.size));

    // 只有"同名同大小"才值得去算哈希——不同大小必然不同。
    // 走 `hash_file` 而不是读全文：`imohash` 只采样头/中/尾，读全文等于把它的意义抹掉。
    let hashes_equal = destination_exists
        && same_size
        && info.size > 0
        && !info.is_symlink()
        && local_is_regular
        && hash::hash_file(algorithm, &destination).is_ok_and(|digest| digest == info.hash);

    let situation = Situation {
        destination_exists,
        hashes_equal,
        is_text_artifact: policy::is_text_artifact(sending_text, &info.name),
        is_empty_or_symlink: policy::is_empty_or_symlink(info),
        resumable: policy::is_resumable(info, local_is_regular),
    };

    let action = policy::resolve(policy::decide(situation, preferences), answer_of(accepted));
    Ok(match action {
        Action::AlreadyCurrent => Plan::AlreadyCurrent,
        Action::CreateDirectly => Plan::CreateDirectly,
        Action::Rename => Plan::Renamed(rename_target(info, out_dir)?),
        Action::Proceed { resumable } => {
            let ranges = if resumable && destination_exists {
                scan_missing(&destination, u64::try_from(info.size).unwrap_or(0))?
            } else {
                ChunkRanges::whole()
            };
            Plan::Receive(ranges)
        }
        // `Declined` 是用户明确说了不要；`Ask` 已经被 `resolve` 消化掉了，走不到；
        // `Action` 又是 `#[non_exhaustive]`，将来新增的动作也落这里。
        // 三者统一按"不传"处理——**默认保守**，别让一个没考虑过的新动作把文件悄悄覆盖了。
        Action::Declined | Action::Ask { .. } | _ => Plan::Declined,
    })
}

fn answer_of(accepted: bool) -> policy::Answer {
    if accepted { policy::Answer::Accept } else { policy::Answer::Decline }
}

/// 目标路径：`<out>/<folder_remote>/<name>`。
///
/// # Errors
///
/// 对端给的 `folder_remote` 或 `name` 试图写到 `out_dir` 外面时返回错误。
pub fn destination_of(info: &FileInfo, out_dir: &Path) -> Result<PathBuf, EngineError> {
    let folder = safe_folder(&info.folder_remote)?;
    let name = safe_name(&info.name)?;
    Ok(join_checked(out_dir, &folder, &name))
}

/// 空目录的落盘路径。
///
/// **不能**用 [`destination_of`]：那个会把 `name` 再拼一层，而空目录的 `folder_remote`
/// 已经包含了它自己（`photos/empty/`），于是会得到 `out/photos/empty/empty`。
///
/// # Errors
///
/// 同 [`destination_of`]。
pub fn folder_destination_of(info: &FileInfo, out_dir: &Path) -> Result<PathBuf, EngineError> {
    let folder = safe_folder(&info.folder_remote)?;
    // 对端可能给一个空的 `folder_remote`（老对端 / 畸形清单），那就退回用 `name`。
    if folder.as_os_str().is_empty() {
        let name = safe_name(&info.name)?;
        return Ok(out_dir.join(name));
    }
    Ok(join_checked(out_dir, &folder, Path::new("")))
}

/// 把对端给的 `folder_remote` 归一成"只能往 `out_dir` 里面走"的相对路径。
///
/// # 为什么必须归一
///
/// `folder_remote` 是**对端写在清单里的字符串**，对端不受信任。直接
/// `out_dir.join(folder)` 有两条逃逸路径：
///
/// 1. **`..` 段**：`../../etc/` 会写到 `out_dir` 外面；
/// 2. **绝对路径**：`PathBuf::join` 遇到绝对路径会**整个替换**前缀，`/etc/` 直接
///    落到文件系统根（Windows 上 `C:\` 同理）。
///
/// Go 用 `receivefs.Normalize()` 挡这个（`normalizeReceiveFilePath`，`croc.go:881`），
/// 这里等价地挡：拒绝绝对路径与任何 `..` 段，丢掉 `.` 与空段。
///
/// 绝对路径**报错而不是"当成相对路径"**：`"/etc/"` 静默改写成 `"etc/"` 虽然也安全，
/// 但那是把对端说的话改了意思——清单与落盘结果对不上，出问题时没法从日志里看出来。
fn safe_folder(folder_remote: &str) -> Result<PathBuf, EngineError> {
    if starts_with_root(folder_remote) {
        return Err(unsafe_remote_path(folder_remote));
    }
    let mut relative = PathBuf::new();
    for segment in split_remote_path(folder_remote) {
        match segment {
            "" | "." => {}
            ".." => return Err(unsafe_remote_path(folder_remote)),
            // Windows 上 `C:` 这类"驱动器相对"前缀会让 `join` 改掉盘符。
            _ if cfg!(windows) && segment.contains(':') => return Err(unsafe_remote_path(folder_remote)),
            other => relative.push(other),
        }
    }
    Ok(relative)
}

/// 是不是以根/盘符开头（`/etc`、`\etc`、`C:/etc`）。
fn starts_with_root(value: &str) -> bool {
    if value.starts_with('/') || value.starts_with('\\') {
        return true;
    }
    let mut characters = value.chars();
    matches!(
        (characters.next(), characters.next()),
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic()
    )
}

/// 把对端给的 `name` 归一成**单个文件名**。
///
/// Go 的判据是 `cleanName != path.Base(cleanName)` 就拒绝（`croc.go:887`）——也就是
/// "名字里不许有目录成分"。这里同样只认单段。
fn safe_name(name: &str) -> Result<PathBuf, EngineError> {
    if starts_with_root(name) {
        return Err(unsafe_remote_path(name));
    }
    let mut segments = split_remote_path(name).filter(|segment| !segment.is_empty() && *segment != ".");
    let first = segments.next().ok_or_else(|| unsafe_remote_path(name))?;
    if segments.next().is_some() || first == ".." || (cfg!(windows) && first.contains(':')) {
        return Err(unsafe_remote_path(name));
    }
    Ok(PathBuf::from(first))
}

/// 按线路上的分隔符切开。线上一律用 `/`；Windows 上额外把 `\` 也当分隔符——
/// 否则 `..\..\x` 能绕过检查（在 Unix 上 `\` 是合法文件名字符，不能切）。
fn split_remote_path(value: &str) -> impl Iterator<Item = &str> {
    const WINDOWS: &[char] = &['/', '\\'];
    const UNIX: &[char] = &['/'];
    value.split(if cfg!(windows) { WINDOWS } else { UNIX })
}

/// 拼一个保证落在 `out_dir` 里面的路径。
///
/// 分段 `push` 而不是 `join` 整个字符串：`join` 会把绝对路径当"换一个根"处理。
fn join_checked(out_dir: &Path, folder: &Path, name: &Path) -> PathBuf {
    let mut target = out_dir.to_path_buf();
    for component in folder.components().chain(name.components()) {
        if let std::path::Component::Normal(segment) = component {
            target.push(segment);
        }
    }
    target
}

fn unsafe_remote_path(value: &str) -> EngineError {
    EngineError::Config(format!("the peer asked to write outside the receive directory: {value:?}"))
}

/// 找一个没被占用的名字（`--rename`）。
fn rename_target(info: &FileInfo, out_dir: &Path) -> Result<String, EngineError> {
    let folder = destination_of(info, out_dir)?.parent().map_or_else(|| out_dir.to_path_buf(), Path::to_path_buf);
    // `rename_candidates` 只在原名字上加后缀，不会引入目录成分，所以直接拼是安全的。
    for candidate in policy::rename_candidates(&info.name) {
        if !folder.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err(EngineError::Config(format!("cannot find a free name for {}", info.name)))
}

/// 落盘一个文件（或空目录 / 符号链接）。
///
/// 普通文件先 `truncate` 到完整大小再按偏移写——这正是断点续传能工作的前提：
/// 没收到的块保持全零，于是"扫零块"就等于"扫缺失块"。
///
/// # Errors
///
/// 建目录、创建、截断或写失败。
pub fn open_sink(info: &FileInfo, out_dir: &Path, name_override: Option<&str>) -> Result<Sink, EngineError> {
    let mut effective = info.clone();
    if let Some(name) = name_override {
        effective.name = name.to_owned();
    }
    let destination = destination_of(&effective, out_dir)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| EngineError::file(parent, error))?;
    }

    if info.is_symlink() {
        remove_if_exists(&destination)?;
        create_symlink(&info.symlink, &destination)?;
        return Ok(Sink::Done);
    }

    let file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&destination)
        .map_err(|error| EngineError::file(&destination, error))?;
    let size = u64::try_from(info.size).unwrap_or(0);
    file.set_len(size).map_err(|error| EngineError::file(&destination, error))?;

    Ok(Sink::File { file, path: destination, size })
}

/// 一个已打开的落盘目标。
#[derive(Debug)]
pub enum Sink {
    /// 普通文件，按偏移写。
    File {
        /// 文件句柄。
        file: File,
        /// 落盘路径。
        path: PathBuf,
        /// 声明的总大小。
        size: u64,
    },
    /// 已经处理完（符号链接、空目录），不需要数据面。
    Done,
}

impl Sink {
    /// 落盘路径（`Sink::Done` 时为 `None`）。
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        match self {
            Self::File { path, .. } => Some(path),
            Self::Done => None,
        }
    }

    /// 写一块。
    ///
    /// # Errors
    ///
    /// 写失败。**越界写入会被拒绝**——对端可以送任意偏移，写到文件外面去等于让一个
    /// 恶意发送方改写接收方磁盘上的任意位置。
    pub fn write_at(&self, offset: u64, data: &[u8]) -> Result<(), EngineError> {
        let Self::File { file, path, size } = self else {
            return Ok(());
        };
        let end =
            offset.checked_add(data.len() as u64).ok_or_else(|| EngineError::file(path, "chunk offset overflow"))?;
        if end > *size {
            return Err(EngineError::file(
                path,
                format!("chunk at {offset}..{end} exceeds the declared file size {size}"),
            ));
        }
        let handle = file.try_clone().map_err(|error| EngineError::file(path, error))?;
        write_all_at(&handle, data, offset).map_err(|error| EngineError::file(path, error))
    }

    /// 收尾：对齐权限与修改时间。
    ///
    /// # Errors
    ///
    /// 设置权限或时间失败。**失败不算致命**——内容已经写对了，元信息对不上只值得一条
    /// 警告，不该让整次传输算失败。
    pub fn finalize(&self, info: &FileInfo) -> Result<(), EngineError> {
        let Self::File { path, .. } = self else {
            return Ok(());
        };
        set_permissions(path, info.permissions())?;
        if let Some(time) = info.mod_time.get() {
            let system = SystemTime::from(time);
            let _ = filetime_set(path, system);
        }
        Ok(())
    }
}

#[cfg(unix)]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt as _;
    file.write_all_at(data, offset)
}

#[cfg(windows)]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt as _;
    // `seek_write` 不保证一次写完，要自己循环——这一点与 POSIX 的 `write_all_at` 不同。
    let mut written = 0usize;
    while written < data.len() {
        let slice = data.get(written..).unwrap_or_default();
        let count = file.seek_write(slice, offset + written as u64)?;
        if count == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::WriteZero, "short write"));
        }
        written += count;
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn write_all_at(file: &File, data: &[u8], offset: u64) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut handle = file.try_clone()?;
    handle.seek(SeekFrom::Start(offset))?;
    handle.write_all(data)
}

/// 按偏移读一块。
#[cfg(unix)]
pub fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt as _;
    file.read_at(buffer, offset)
}

/// 按偏移读一块。
#[cfg(windows)]
pub fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt as _;
    file.seek_read(buffer, offset)
}

/// 按偏移读一块。
#[cfg(not(any(unix, windows)))]
pub fn read_at(file: &File, buffer: &mut [u8], offset: u64) -> std::io::Result<usize> {
    use std::io::Read as _;
    let mut handle = file.try_clone()?;
    handle.seek(SeekFrom::Start(offset))?;
    handle.read(buffer)
}

fn remove_if_exists(path: &Path) -> Result<(), EngineError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(EngineError::file(path, error)),
    }
}

#[cfg(unix)]
fn create_symlink(target: &str, link: &Path) -> Result<(), EngineError> {
    std::os::unix::fs::symlink(target, link).map_err(|error| EngineError::file(link, error))
}

#[cfg(windows)]
fn create_symlink(target: &str, link: &Path) -> Result<(), EngineError> {
    // Windows 上符号链接要区分文件/目录，且默认需要开发者模式或管理员权限。
    // 失败就退化成写一个内容为目标的普通文件——比整个传输失败好。
    std::os::windows::fs::symlink_file(target, link)
        .or_else(|_| fs::write(link, target))
        .map_err(|error| EngineError::file(link, error))
}

#[cfg(not(any(unix, windows)))]
fn create_symlink(target: &str, link: &Path) -> Result<(), EngineError> {
    fs::write(link, target).map_err(|error| EngineError::file(link, error))
}

// 非 Unix 分支不返回错误，于是这个函数在那些平台上"没必要包 Result"——但它在 Unix 上
// 需要，而两个平台共用同一份调用代码。按平台拆两个签名只会让调用方写 cfg。
#[allow(clippy::unnecessary_wraps, reason = "非 Unix 分支用不到错误，但调用方要统一")]
fn set_permissions(path: &Path, mode: u32) -> Result<(), EngineError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(|error| EngineError::file(path, error))
    }
    #[cfg(not(unix))]
    {
        // Windows 上权限位没有意义（Go 那边也是同样处理）。
        let _ = (path, mode);
        Ok(())
    }
}

/// 尽量对齐修改时间；平台不支持就安静放弃。
fn filetime_set(path: &Path, time: SystemTime) -> std::io::Result<()> {
    // 标准库没有设置 mtime 的 API，而为此拉一个 `filetime` 依赖不值得——
    // 内容已经写对了，时间戳只是锦上添花。这里用 `std::fs::File::set_times`
    // （Rust 1.75+）如果可用的话；不可用就什么都不做。
    let file = OpenOptions::new().write(true).open(path)?;
    file.set_times(std::fs::FileTimes::new().set_modified(time))
}

fn mode_of(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o7777
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        // Windows 没有 Unix 权限位；Go 那边给出的是 0o666 一类的默认值。
        0o644
    }
}

fn file_name(path: &Path) -> Result<String, EngineError> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| EngineError::Config(format!("{} has no file name", path.display())))
}

fn parent_of(path: &Path) -> String {
    path.parent().map(|parent| parent.to_string_lossy().into_owned()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!("croc-engine-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("建临时目录");
        path
    }

    #[test]
    fn a_single_file_collects_with_its_parent_as_the_remote_folder() {
        let dir = temp_dir("single");
        let file = dir.join("payload.bin");
        fs::write(&file, b"hello croc").expect("写");

        let (files, empty) = collect(std::slice::from_ref(&file), "xxhash").expect("收集");
        assert_eq!(files.len(), 1);
        assert!(empty.is_empty());
        let info = &files[0].info;
        assert_eq!(info.name, "payload.bin");
        // Go 给单个文件的就是 `"./"`（`croc.go:1178`），接收方会把它归一成 `"."`。
        assert_eq!(info.folder_remote, "./");
        assert_eq!(info.size, 10);
        assert_eq!(info.hash, hash::xxh64_bytes(b"hello croc"));
        assert!(info.prepared);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn directories_are_walked_and_empty_ones_are_kept() {
        let dir = temp_dir("tree");
        let root = dir.join("tree");
        fs::create_dir_all(root.join("a/b")).expect("建目录");
        fs::create_dir_all(root.join("empty")).expect("建空目录");
        fs::write(root.join("a/b/c.txt"), b"c").expect("写");
        fs::write(root.join("top.txt"), b"t").expect("写");

        let (files, empty) = collect(&[root], "xxhash").expect("收集");
        let names: Vec<&str> = files.iter().map(|entry| entry.info.name.as_str()).collect();
        assert_eq!(names, vec!["c.txt", "top.txt"], "顺序应稳定（按路径排序）");

        // `folder_remote` **含目录自己的名字**（`tree/`）——这是 Go 的约定，
        // 少了这一层接收方会把内容平铺到 `<out>/` 下。
        let folders: Vec<&str> = files.iter().map(|entry| entry.info.folder_remote.as_str()).collect();
        assert_eq!(folders, vec!["tree/a/b/", "tree/"]);
        assert_eq!(empty.len(), 1, "空目录要单独列出来");
        assert_eq!(empty[0].folder_remote, "tree/empty/", "空目录的 folder_remote 含它自己");
        assert!(empty[0].is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sender_info_carries_the_hash_algorithm() {
        let dir = temp_dir("info");
        let file = dir.join("x.bin");
        fs::write(&file, b"x").expect("写");
        let (files, empty) = collect(&[file], "xxhash").expect("收集");
        let info = sender_info(&files, empty, "machine", "xxhash");
        assert_eq!(info.machine_id, "machine");
        assert_eq!(info.hash_algorithm, croc_protocol::payload::HashAlgorithm::Xxhash);
        assert_eq!(info.files_to_transfer.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    /// `--hash` 的名字必须原样进清单，否则接收方按错算法重算、把每个文件都判成
    /// "内容不同"而重传——不报错，只是白传一遍。
    #[test]
    fn the_sender_info_forwards_the_requested_algorithm() {
        for (name, expected) in [
            ("md5", croc_protocol::payload::HashAlgorithm::Md5),
            ("imohash", croc_protocol::payload::HashAlgorithm::Imohash),
            ("highway", croc_protocol::payload::HashAlgorithm::Highway),
        ] {
            let info = sender_info(&[], Vec::new(), "machine", name);
            assert_eq!(info.hash_algorithm, expected, "{name} 应原样进清单");
        }
    }

    #[test]
    fn unsupported_hash_algorithms_are_rejected_early() {
        let dir = temp_dir("hash");
        let file = dir.join("x.bin");
        fs::write(&file, b"x").expect("写");
        // 四种受支持的算法都得能算。
        for name in ["xxhash", "md5", "imohash", "highway"] {
            assert!(collect(std::slice::from_ref(&file), name).is_ok(), "{name} 应当被支持");
        }
        // 不在名单里的名字要**报错**，不能静默退化成 xxhash——那会让对端拿到对不上的摘要。
        assert!(matches!(collect(&[file], "sha256"), Err(EngineError::UnsupportedHash(_))));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn scanning_a_missing_file_finds_every_chunk() {
        let dir = temp_dir("scan");
        let file = dir.join("half.bin");
        let size = CHUNK_SIZE * 3;
        let mut bytes = vec![0u8; usize::try_from(size).unwrap()];
        // 只有第 1 块有数据，第 0、2 块还是零。
        bytes[usize::try_from(CHUNK_SIZE).unwrap()] = 7;
        fs::write(&file, &bytes).expect("写");

        let ranges = scan_missing(&file, size).expect("扫描");
        assert_eq!(ranges.as_wire(), &[CHUNK_SIZE as i64, 0, 1, 2 * CHUNK_SIZE as i64, 1]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fully_populated_file_reports_nothing_missing() {
        let dir = temp_dir("complete");
        let file = dir.join("full.bin");
        fs::write(&file, vec![1u8; 100]).expect("写");
        let ranges = scan_missing(&file, 100).expect("扫描");
        assert!(ranges.is_whole(), "一块都不缺时返回空列表（= 整个文件），这是 Go 的既有行为");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn writing_past_the_declared_size_is_refused() {
        let dir = temp_dir("bounds");
        let info = FileInfo { name: "small.bin".to_owned(), size: 4, ..FileInfo::default() };
        let sink = open_sink(&info, &dir, None).expect("建 sink");
        assert!(sink.write_at(0, b"abcd").is_ok());
        assert!(sink.write_at(2, b"cde").is_err(), "越界写入必须被拒绝");
        assert!(sink.write_at(u64::MAX, b"x").is_err(), "偏移溢出必须被拒绝");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resume_writes_land_at_the_right_offsets() {
        let dir = temp_dir("resume");
        let info = FileInfo { name: "r.bin".to_owned(), size: 8, ..FileInfo::default() };
        let sink = open_sink(&info, &dir, None).expect("建 sink");
        sink.write_at(4, b"efgh").expect("写后半");
        let path = sink.path().expect("路径").to_path_buf();
        drop(sink);

        // 再开一次（模拟续传），补前半。
        let sink = open_sink(&info, &dir, None).expect("重开");
        sink.write_at(0, b"abcd").expect("写前半");
        drop(sink);

        assert_eq!(fs::read(&path).expect("读"), b"abcdefgh");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn identical_content_is_recognised_and_needs_no_transfer() {
        let dir = temp_dir("plan");
        let content = b"same bytes";
        let info = FileInfo {
            name: "x.bin".to_owned(),
            size: i64::try_from(content.len()).unwrap(),
            hash: hash::xxh64_bytes(content),
            ..FileInfo::default()
        };
        fs::write(dir.join("x.bin"), content).expect("预置");

        let plan = plan(&info, &dir, Preferences::default(), false, "xxhash", true).expect("决策");
        assert_eq!(plan, Plan::AlreadyCurrent);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_local_comparison_uses_the_algorithm_from_the_manifest() {
        let dir = temp_dir("plan-algorithm");
        let content = b"same bytes";
        // 清单里带的是 md5 摘要。本地比对要是写死 xxhash，这里就永远认不出"内容相同"，
        // 于是每次传输都把已经存在的文件重传一遍。
        let info = FileInfo {
            name: "x.bin".to_owned(),
            size: i64::try_from(content.len()).unwrap(),
            hash: hash::hash_for_algorithm("md5", content).expect("md5"),
            ..FileInfo::default()
        };
        fs::write(dir.join("x.bin"), content).expect("预置");
        let plan = plan(&info, &dir, Preferences::default(), false, "md5", true).expect("决策");
        assert_eq!(plan, Plan::AlreadyCurrent);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_destination_requests_the_whole_file() {
        let dir = temp_dir("plan-fresh");
        let info = FileInfo { name: "new.bin".to_owned(), size: 100, hash: vec![1], ..FileInfo::default() };
        let plan = plan(&info, &dir, Preferences::default(), false, "xxhash", true).expect("决策");
        assert_eq!(plan, Plan::Receive(ChunkRanges::whole()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_picks_a_free_name() {
        let dir = temp_dir("plan-rename");
        let info = FileInfo { name: "x.bin".to_owned(), size: 100, hash: vec![1], ..FileInfo::default() };
        fs::write(dir.join("x.bin"), b"old").expect("预置");
        fs::write(dir.join("x (1).bin"), b"old").expect("预置");
        let plan =
            plan(&info, &dir, Preferences { overwrite: false, rename: true }, false, "xxhash", true).expect("决策");
        assert_eq!(plan, Plan::Renamed("x (2).bin".to_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn declining_marks_the_file_as_declined() {
        let dir = temp_dir("plan-decline");
        let info = FileInfo { name: "x.bin".to_owned(), size: 100, hash: vec![1], ..FileInfo::default() };
        fs::write(dir.join("x.bin"), b"old").expect("预置");
        let plan = plan(&info, &dir, Preferences::default(), false, "xxhash", false).expect("决策");
        assert_eq!(plan, Plan::Declined);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_files_and_symlinks_bypass_the_data_plane() {
        let dir = temp_dir("plan-empty");
        let empty = FileInfo { name: "e.bin".to_owned(), size: 0, ..FileInfo::default() };
        assert_eq!(
            plan(&empty, &dir, Preferences::default(), false, "xxhash", true).expect("决策"),
            Plan::CreateDirectly
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_folder_paths_do_not_double_the_name() {
        // 空目录的 `folder_remote` 已经含了它自己，再拼一次 `name` 会得到 `out/tree/empty/empty`。
        let folder =
            FileInfo { name: "empty".to_owned(), folder_remote: "tree/empty/".to_owned(), ..FileInfo::default() };
        assert_eq!(folder_destination_of(&folder, Path::new("/out")).expect("路径"), Path::new("/out/tree/empty"));
        let nested =
            FileInfo { name: "deep".to_owned(), folder_remote: "tree/a/deep/".to_owned(), ..FileInfo::default() };
        assert_eq!(folder_destination_of(&nested, Path::new("/out")).expect("路径"), Path::new("/out/tree/a/deep"));
        // 对端给了个空 `folder_remote`（畸形清单）：退回用 `name`，别落到 `out` 本身。
        let bare = FileInfo { name: "top".to_owned(), folder_remote: String::new(), ..FileInfo::default() };
        assert_eq!(folder_destination_of(&bare, Path::new("/out")).expect("路径"), Path::new("/out/top"));
    }

    #[test]
    fn destination_paths_join_the_remote_folder() {
        // Go 的 `folder_remote` 带尾斜杠（`tree/sub/`），`path.Join` 会把它归一掉。
        let info = FileInfo { name: "c.txt".to_owned(), folder_remote: "tree/a/b/".to_owned(), ..FileInfo::default() };
        assert_eq!(destination_of(&info, Path::new("/out")).expect("路径"), Path::new("/out/tree/a/b/c.txt"));
        // 单个文件是 `./`，落在接收根下。
        let root = FileInfo { name: "t.txt".to_owned(), folder_remote: "./".to_owned(), ..FileInfo::default() };
        assert_eq!(destination_of(&root, Path::new("/out")).expect("路径"), Path::new("/out/t.txt"));
    }

    #[test]
    fn the_peer_cannot_write_outside_the_receive_directory() {
        // `folder_remote` 是对端写在清单里的字符串，不受信任。两条逃逸路径都要挡住：
        // `..` 段，以及绝对路径（`join` 遇到绝对路径会整个替换前缀）。
        let escaping = |folder: &str, name: &str| {
            let info = FileInfo { name: name.to_owned(), folder_remote: folder.to_owned(), ..FileInfo::default() };
            destination_of(&info, Path::new("/out")).is_err()
        };
        assert!(escaping("../..", "passwd"), "`..` 段必须被拒");
        assert!(escaping("tree/../../..", "passwd"), "藏在中段的 `..` 也要被拒");
        assert!(escaping("/etc", "passwd"), "绝对路径必须被拒");
        assert!(escaping("tree/", "../../passwd"), "名字里的 `..` 必须被拒");
        assert!(escaping("tree/", "sub/passwd"), "名字里不许有目录成分（Go 的 `path.Base` 判据）");
        // 正常路径不受影响。
        assert!(!escaping("tree/sub/", "ok.txt"));
    }

    #[test]
    fn folder_remote_includes_the_directory_itself() {
        // Go 的算法：`TrimPrefix(Dir(pathName), Dir(absPath) + "/") + "/"`。基准取的是
        // 目录的**父**目录，所以 `folder_remote` 天然带上目录自己的名字——少了这一层，
        // 接收方会把整个目录的内容平铺到 `<out>/` 下。
        let base = Path::new("/send");
        assert_eq!(remote_folder_of(base, Path::new("/send/tree")), "tree/");
        assert_eq!(remote_folder_of(base, Path::new("/send/tree/sub")), "tree/sub/");
        assert_eq!(remote_folder_of(base, Path::new("/send/tree/empty")), "tree/empty/");
    }

    #[test]
    fn the_folder_count_matches_go() {
        // Go 在 `filepath.Walk` 里每遇到一个目录就 `totalNumberFolders++`（根目录也算）。
        let entry = |folder: &str| SourceEntry {
            path: PathBuf::from("/x"),
            info: FileInfo { folder_remote: folder.to_owned(), ..FileInfo::default() },
        };
        let files = vec![entry("tree/"), entry("tree/sub/"), entry("tree/sub/deep/deeper/")];
        let empty = vec![FileInfo { folder_remote: "tree/empty/".to_owned(), ..FileInfo::default() }];
        // tree、tree/sub、tree/sub/deep、tree/sub/deep/deeper、tree/empty
        assert_eq!(folder_count(&files, &empty), 5);

        // 单个文件（`./`）不算目录——文本传输必须恰好 0 个，否则 Go 会拒绝清单。
        assert_eq!(folder_count(&[entry("./")], &[]), 0);
    }
}
