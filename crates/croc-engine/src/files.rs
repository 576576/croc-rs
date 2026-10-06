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
//! - `folder_remote` = 相对该路径的子目录（接收方建目录用），根是 `.`；
//! - `name` = 裸文件名。
//!
//! 这与 Go 的 `FileInfo{FolderSource, FolderRemote, Name}` 是同一套约定。

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
            collect_directory(path, path, &mut files, &mut empty_folders, algorithm)?;
        } else {
            files.push(entry_for(path, ".", algorithm)?);
        }
    }

    Ok((files, empty_folders))
}

/// 递归收集一个目录。
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

    let mut saw_anything = false;
    for path in names {
        let metadata = fs::symlink_metadata(&path).map_err(|error| EngineError::file(&path, error))?;
        if metadata.is_dir() {
            let before = files.len() + empty_folders.len();
            collect_directory(root, &path, files, empty_folders, algorithm)?;
            if files.len() + empty_folders.len() == before {
                // 空目录（或只有空目录）也要在对端建出来。
                empty_folders.push(folder_info(root, &path)?);
            }
            saw_anything = true;
            continue;
        }
        let relative = relative_folder(root, &path);
        files.push(entry_for(&path, &relative, algorithm)?);
        saw_anything = true;
    }
    let _ = saw_anything;
    Ok(())
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

    let bytes = fs::read(path).map_err(|error| EngineError::file(path, error))?;
    let digest = hash::hash_for_algorithm(algorithm, &bytes)
        .ok_or_else(|| EngineError::UnsupportedHash(algorithm.to_owned()))?;

    Ok(SourceEntry {
        path: path.to_path_buf(),
        info: FileInfo {
            name,
            folder_remote: folder_remote.to_owned(),
            folder_source: parent_of(path),
            hash: digest,
            size: i64::try_from(bytes.len()).unwrap_or(i64::MAX),
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
fn folder_info(root: &Path, path: &Path) -> Result<FileInfo, EngineError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| EngineError::file(path, error))?;
    Ok(FileInfo {
        name: file_name(path)?,
        folder_remote: relative_self(root, path),
        folder_source: parent_of(path),
        size: 0,
        mode: mode_of(&metadata) | croc_protocol::payload::MODE_DIR,
        ..FileInfo::default()
    })
}

/// 把收集结果组装成线路上的清单。
#[must_use]
pub fn sender_info(files: &[SourceEntry], empty_folders: Vec<FileInfo>, machine_id: &str) -> SenderInfo {
    let folder_count = files
        .iter()
        .filter_map(|entry| entry.info.folder_remote.split('/').next())
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .count();
    SenderInfo {
        files_to_transfer: files.iter().map(|entry| entry.info.clone()).collect(),
        empty_folders_to_transfer: empty_folders,
        total_number_folders: i64::try_from(folder_count).unwrap_or(0),
        machine_id: machine_id.to_owned(),
        hash_algorithm: croc_protocol::payload::HashAlgorithm::Xxhash,
        ..SenderInfo::default()
    }
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
/// # Errors
///
/// 探测目标路径时出错。
pub fn plan(
    info: &FileInfo,
    out_dir: &Path,
    preferences: Preferences,
    sending_text: bool,
    accepted: bool,
) -> Result<Plan, EngineError> {
    let destination = destination_of(info, out_dir);
    let metadata = fs::symlink_metadata(&destination);
    let destination_exists = metadata.is_ok();
    let local_is_regular = metadata.as_ref().is_ok_and(std::fs::Metadata::is_file);
    let same_size = metadata.as_ref().is_ok_and(|m| i64::try_from(m.len()).ok() == Some(info.size));

    // 只有"同名同大小"才值得去算哈希——不同大小必然不同。
    let hashes_equal = destination_exists
        && same_size
        && info.size > 0
        && !info.is_symlink()
        && local_is_regular
        && fs::read(&destination).is_ok_and(|bytes| hash::xxh64_bytes(&bytes) == info.hash);

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
#[must_use]
pub fn destination_of(info: &FileInfo, out_dir: &Path) -> PathBuf {
    let folder = info.folder_remote.trim_start_matches("./");
    if folder.is_empty() || folder == "." { out_dir.join(&info.name) } else { out_dir.join(folder).join(&info.name) }
}

/// 空目录的落盘路径。
///
/// **不能**用 [`destination_of`]：那个会把 `name` 再拼一层，而空目录的 `folder_remote`
/// 已经包含了它自己（`empty`），于是会得到 `out/empty/empty`。
#[must_use]
pub fn folder_destination_of(info: &FileInfo, out_dir: &Path) -> PathBuf {
    let folder = info.folder_remote.trim_start_matches("./");
    if folder.is_empty() || folder == "." { out_dir.join(&info.name) } else { out_dir.join(folder) }
}

/// 找一个没被占用的名字（`--rename`）。
fn rename_target(info: &FileInfo, out_dir: &Path) -> Result<String, EngineError> {
    let folder = destination_of(info, out_dir).parent().map_or_else(|| out_dir.to_path_buf(), Path::to_path_buf);
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
    let destination = destination_of(&effective, out_dir);
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

/// `path` 自己相对 `root` 的路径（目录用），斜杠分隔，根是 `.`。
fn relative_self(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => ".".to_owned(),
    }
}

/// `path` 的**父目录**相对 `root` 的路径（文件用），斜杠分隔，根目录是 `.`。
fn relative_folder(root: &Path, path: &Path) -> String {
    let parent = path.parent().unwrap_or(root);
    match parent.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
        Err(_) => ".".to_owned(),
    }
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
        assert_eq!(info.folder_remote, ".");
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

        let folders: Vec<&str> = files.iter().map(|entry| entry.info.folder_remote.as_str()).collect();
        assert_eq!(folders, vec!["a/b", "."]);
        assert_eq!(empty.len(), 1, "空目录要单独列出来");
        assert_eq!(empty[0].folder_remote, "empty");
        assert!(empty[0].is_dir());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_sender_info_carries_the_hash_algorithm() {
        let dir = temp_dir("info");
        let file = dir.join("x.bin");
        fs::write(&file, b"x").expect("写");
        let (files, empty) = collect(&[file], "xxhash").expect("收集");
        let info = sender_info(&files, empty, "machine");
        assert_eq!(info.machine_id, "machine");
        assert_eq!(info.hash_algorithm, croc_protocol::payload::HashAlgorithm::Xxhash);
        assert_eq!(info.files_to_transfer.len(), 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unsupported_hash_algorithms_are_rejected_early() {
        let dir = temp_dir("hash");
        let file = dir.join("x.bin");
        fs::write(&file, b"x").expect("写");
        assert!(matches!(collect(&[file], "md5"), Err(EngineError::UnsupportedHash(_))));
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

        let plan = plan(&info, &dir, Preferences::default(), false, true).expect("决策");
        assert_eq!(plan, Plan::AlreadyCurrent);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_destination_requests_the_whole_file() {
        let dir = temp_dir("plan-fresh");
        let info = FileInfo { name: "new.bin".to_owned(), size: 100, hash: vec![1], ..FileInfo::default() };
        let plan = plan(&info, &dir, Preferences::default(), false, true).expect("决策");
        assert_eq!(plan, Plan::Receive(ChunkRanges::whole()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rename_picks_a_free_name() {
        let dir = temp_dir("plan-rename");
        let info = FileInfo { name: "x.bin".to_owned(), size: 100, hash: vec![1], ..FileInfo::default() };
        fs::write(dir.join("x.bin"), b"old").expect("预置");
        fs::write(dir.join("x (1).bin"), b"old").expect("预置");
        let plan = plan(&info, &dir, Preferences { overwrite: false, rename: true }, false, true).expect("决策");
        assert_eq!(plan, Plan::Renamed("x (2).bin".to_owned()));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn declining_marks_the_file_as_declined() {
        let dir = temp_dir("plan-decline");
        let info = FileInfo { name: "x.bin".to_owned(), size: 100, hash: vec![1], ..FileInfo::default() };
        fs::write(dir.join("x.bin"), b"old").expect("预置");
        let plan = plan(&info, &dir, Preferences::default(), false, false).expect("决策");
        assert_eq!(plan, Plan::Declined);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_files_and_symlinks_bypass_the_data_plane() {
        let dir = temp_dir("plan-empty");
        let empty = FileInfo { name: "e.bin".to_owned(), size: 0, ..FileInfo::default() };
        assert_eq!(plan(&empty, &dir, Preferences::default(), false, true).expect("决策"), Plan::CreateDirectly);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_folder_paths_do_not_double_the_name() {
        // 空目录的 `folder_remote` 已经含了它自己，再拼一次 `name` 会得到 `out/empty/empty`。
        let folder = FileInfo { name: "empty".to_owned(), folder_remote: "empty".to_owned(), ..FileInfo::default() };
        assert_eq!(folder_destination_of(&folder, Path::new("/out")), Path::new("/out/empty"));
        let nested = FileInfo { name: "deep".to_owned(), folder_remote: "a/deep".to_owned(), ..FileInfo::default() };
        assert_eq!(folder_destination_of(&nested, Path::new("/out")), Path::new("/out/a/deep"));
        let root = FileInfo { name: "top".to_owned(), folder_remote: ".".to_owned(), ..FileInfo::default() };
        assert_eq!(folder_destination_of(&root, Path::new("/out")), Path::new("/out/top"));
    }

    #[test]
    fn destination_paths_join_the_remote_folder() {
        let info = FileInfo { name: "c.txt".to_owned(), folder_remote: "a/b".to_owned(), ..FileInfo::default() };
        assert_eq!(destination_of(&info, Path::new("/out")), Path::new("/out/a/b/c.txt"));
        let root = FileInfo { name: "t.txt".to_owned(), folder_remote: ".".to_owned(), ..FileInfo::default() };
        assert_eq!(destination_of(&root, Path::new("/out")), Path::new("/out/t.txt"));
    }
}
