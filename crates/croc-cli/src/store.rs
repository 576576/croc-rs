//! `croc store`：把文件存到暂存服务，拿一个取件链接。
//!
//! 与普通传输的区别是不需要两端同时在线：发送方传完就走，收件方什么时候取都行，
//! 服务端只保管密文（它看不到文件名与内容）。代价是要信任一个第三方来保管与计数。
//!
//! 撤销凭据（上传令牌）存在本机的 `store-receipts.json` 里，`--revoke <id>` 用它。

use rust_i18n::t;
use std::path::PathBuf;
use std::sync::Arc;

use croc_store::client::{Client, Progress, Reporter, UploadOptions};
use croc_store::share::Share;

use crate::cli::Clipboard;

/// 默认的暂存服务。
pub const DEFAULT_STORE_URL: &str = "https://getcroc.com";

/// `croc store` 与 `send --store` 的请求。
pub struct StoreRequest {
    /// 要上传的文件。
    pub paths: Vec<PathBuf>,
    /// 允许的验证下载次数。
    pub downloads: i64,
    /// 存储时长的原始写法（`12h` 之类）。
    pub expiration: String,
    /// 服务来源。
    pub url: String,
    /// 不打印进度。
    pub quiet: bool,
    /// 剪贴板行为。
    pub clipboard: Clipboard,
    /// 是否显示二维码。
    pub qrcode: bool,
    /// 代理。
    pub proxy: Option<croc_store::proxy::Proxy>,
}

/// 接收一次暂存传输的请求。
pub struct StoreReceiveRequest {
    /// 取件链接或 token。
    pub token: String,
    /// 落盘目录。
    pub out: PathBuf,
    /// 目标已存在时直接覆盖。
    pub overwrite: bool,
    /// 全部默认答"是"，不再逐项确认。
    pub yes: bool,
    /// 不打印进度。
    pub quiet: bool,
    /// 代理。
    pub proxy: Option<croc_store::proxy::Proxy>,
}

/// 解析 `90m` / `12h` / `3d` / `2w`。
///
/// 与 Go 的 `storeapi.ParseExpiration` 同一套语法：必须是正整数加单位，最短一分钟。
///
/// # Errors
///
/// 写法不合法时给出人能读的原因。
pub fn parse_expiration(value: &str) -> Result<time::Duration, String> {
    let value = value.trim();
    let (digits, unit) = value.split_at(value.len().saturating_sub(1));
    if value.len() < 2 {
        return Err("duration must be a positive whole number followed by m, h, d, or w".to_owned());
    }
    let multiplier = match unit {
        "m" => time::Duration::MINUTE,
        "h" => time::Duration::HOUR,
        "d" => time::Duration::days(1),
        "w" => time::Duration::weeks(1),
        _ => return Err("duration unit must be m, h, d, or w".to_owned()),
    };
    let amount = digits.parse::<u64>().map_err(|_| "duration must be a positive whole number".to_owned())?;
    if amount == 0 {
        return Err("duration must be a positive whole number".to_owned());
    }
    let seconds = i64::try_from(amount).map_err(|_| format!("duration {value:?} is too large"))?;
    let duration = multiplier
        .checked_mul(seconds.try_into().unwrap_or(i32::MAX))
        .ok_or_else(|| format!("duration {value:?} is too large"))?;
    if duration < time::Duration::MINUTE {
        return Err("duration must be at least 1m".to_owned());
    }
    Ok(duration)
}

/// `--downloads` 的校验。
///
/// # Errors
///
/// 小于 1。
pub fn validate_downloads(downloads: i64) -> Result<(), String> {
    if downloads < 1 {
        return Err("--downloads must be positive".to_owned());
    }
    Ok(())
}

/// 暂存模式只接受普通文件：目录、设备文件、符号链接都拒掉。
///
/// # Errors
///
/// 路径读不了，或者不是普通文件。
pub fn regular_files(paths: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    for path in paths {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if !metadata.is_file() {
            return Err(format!("stored mode supports regular files only: {}", path.display()));
        }
    }
    Ok(paths.to_vec())
}

/// 上传前的检查：暂存模式不读 stdin，也不接受那些只对普通传输有意义的开关。
///
/// # Errors
///
/// 给了一个暂存模式不支持的开关，或者 stdin 是管道。
pub fn validate_store_sources(
    text: bool,
    zip: bool,
    git: bool,
    exclude: &[String],
    exclude_file: &[String],
    ignore_stdin: bool,
) -> Result<(), String> {
    if text || zip || git || !exclude.is_empty() || !exclude_file.is_empty() {
        return Err("stored mode supports regular file arguments only".to_owned());
    }
    if !ignore_stdin && stdin_is_pipe() {
        return Err("stored mode does not accept stdin; pass regular file paths".to_owned());
    }
    Ok(())
}

/// stdin 是管道（而不是终端）。
fn stdin_is_pipe() -> bool {
    use std::io::IsTerminal as _;
    !std::io::stdin().is_terminal()
}

/// 一行人能读的状态与百分比进度。
struct Printer {
    quiet: bool,
    last: std::sync::Mutex<String>,
}

impl Printer {
    fn new(quiet: bool) -> Self {
        Self { quiet, last: std::sync::Mutex::new(String::new()) }
    }
}

impl Reporter for Printer {
    fn status(&self, message: &str) {
        if self.quiet {
            return;
        }
        let Ok(mut last) = self.last.lock() else { return };
        if *last == message {
            return;
        }
        *last = message.to_owned();
        eprintln!("{message}");
    }

    fn progress(&self, progress: Progress) {
        if self.quiet || progress.total_size <= 0 {
            return;
        }
        let percent = 100.0 * progress.total_bytes as f64 / progress.total_size as f64;
        eprint!("\r  {percent:5.1}%  {}  ", human_bytes(progress.total_bytes));
    }
}

/// 十进制的人类可读大小（与 Go 的 `utils.ByteCountDecimal` 同一个量纲）。
fn human_bytes(bytes: i64) -> String {
    let value = bytes.max(0) as f64;
    for (unit, scale) in [("TB", 1e12), ("GB", 1e9), ("MB", 1e6), ("kB", 1e3)] {
        if value >= scale {
            return format!("{:.1} {unit}", value / scale);
        }
    }
    format!("{bytes} B")
}

/// 一条撤销凭据。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Receipt {
    /// 传输 ID。
    pub id: String,
    /// 服务来源。
    pub origin: String,
    /// 上传令牌。
    #[serde(rename = "uploadToken")]
    pub upload_token: String,
    /// 过期时间。
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

fn receipts_path() -> Option<PathBuf> {
    crate::config::config_dir().map(|dir| dir.join("store-receipts.json"))
}

/// 读出还没过期的凭据。
///
/// 过期的直接丢掉：留着它们只会让 `--revoke` 报"没有凭据"时更难解释。
fn read_receipts() -> Vec<Receipt> {
    let Some(path) = receipts_path() else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    let receipts: Vec<Receipt> = serde_json::from_slice(&bytes).unwrap_or_default();
    let now = time::OffsetDateTime::now_utc();
    receipts.into_iter().filter(|receipt| expiry(&receipt.expires_at).is_none_or(|at| at > now)).collect()
}

fn write_receipts(receipts: &[Receipt]) -> Result<(), std::io::Error> {
    let Some(path) = receipts_path() else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(receipts).unwrap_or_default();
    std::fs::write(&path, bytes)?;
    // 上传令牌是凭据，别让同机的其他用户读到。
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn expiry(value: &str) -> Option<time::OffsetDateTime> {
    time::OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339).ok()
}

fn save_receipt(receipt: Receipt) -> Result<(), std::io::Error> {
    let mut receipts = read_receipts();
    receipts.retain(|existing| existing.id != receipt.id);
    receipts.push(receipt);
    write_receipts(&receipts)
}

fn remove_receipt(id: &str) -> Result<(), std::io::Error> {
    let mut receipts = read_receipts();
    receipts.retain(|receipt| receipt.id != id);
    write_receipts(&receipts)
}

/// 上传。返回进程退出码。
pub async fn send(request: StoreRequest) -> std::process::ExitCode {
    if request.paths.is_empty() {
        eprintln!("{}", t!("store.error.need_path"));
        return std::process::ExitCode::FAILURE;
    }
    let expiration = match parse_expiration(&request.expiration) {
        Ok(value) => value,
        Err(reason) => {
            eprintln!("{}", t!("store.error.expiration", reason = reason));
            return std::process::ExitCode::FAILURE;
        }
    };
    let url = request.url.trim().to_owned();
    let reporter: Arc<dyn Reporter> = Arc::new(Printer::new(request.quiet));
    let client = match Client::new(request.proxy.clone()) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let options = UploadOptions { downloads: request.downloads, expiration };
    let result = match client.upload(&url, &request.paths, options, &reporter).await {
        Ok(result) => result,
        Err(error) => {
            eprintln!("\ncroc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if !request.quiet {
        eprintln!();
    }

    let (browser_url, token) = match (result.share.browser_url(), result.share.cli_token()) {
        (Ok(browser_url), Ok(token)) => (browser_url, token),
        (Err(error), _) | (_, Err(error)) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    if let Err(error) = save_receipt(Receipt {
        id: result.share.id.clone(),
        origin: result.share.origin.clone(),
        upload_token: result.upload_token.clone(),
        expires_at: result.expires_at.format(&time::format_description::well_known::Rfc3339).unwrap_or_default(),
    }) {
        eprintln!("{}", t!("store.error.receipt_failed", error = error));
        return std::process::ExitCode::FAILURE;
    }

    let downloads = match result.downloads {
        1 => t!("store.downloads_one").into_owned(),
        count => t!("store.downloads_many", count = count).into_owned(),
    };
    println!("{}", t!("store.summary", expires = result.expires_at, downloads = downloads));
    println!();
    println!("{}", t!("store.browser_link"));
    println!("    {browser_url}");
    println!();
    println!("{}", t!("store.cli_recipient"));
    println!("    {}", t!("store.cli_recipient_hint"));
    println!("    {token}");
    println!();
    println!("{}", t!("store.revoke_hint"));
    println!("    croc-rs --revoke {}", result.share.id);

    if request.clipboard != Clipboard::Off {
        let _ = crate::clipboard::copy(&browser_url);
    }
    if request.qrcode
        && let Ok(rendered) = crate::qr::render(&browser_url)
    {
        println!();
        print!("{rendered}");
    }
    std::process::ExitCode::SUCCESS
}

/// 下载。返回进程退出码。
pub async fn receive(request: StoreReceiveRequest) -> std::process::ExitCode {
    let share = match Share::parse(&request.token) {
        Ok(share) => share,
        Err(error) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let client = match Client::new(request.proxy.clone()) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    let (manifest, expires) = match client.inspect(&share).await {
        Ok(value) => value,
        Err(error) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };

    let total: i64 = manifest.files.iter().map(|file| file.size).sum();
    eprintln!("Encrypted stored transfer:");
    for file in &manifest.files {
        eprintln!("  {}  {}", file.name, human_bytes(file.size));
    }
    if let Some(expires) = expires {
        eprintln!("  (expires {expires})");
    }
    eprintln!("Total: {}", human_bytes(total));

    for file in &manifest.files {
        let destination = request.out.join(&file.name);
        if destination.exists() && !request.overwrite {
            eprintln!("croc-rs: destination already exists (use --overwrite): {}", destination.display());
            return std::process::ExitCode::FAILURE;
        }
    }
    if !request.yes && !confirm("Receive these files? (Y/n) ") {
        eprintln!("croc-rs: stored transfer refused");
        return std::process::ExitCode::FAILURE;
    }

    let reporter: Arc<dyn Reporter> = Arc::new(Printer::new(request.quiet));
    match client.receive(&share, &manifest, &request.out, &reporter).await {
        Ok(()) => {
            if !request.quiet {
                eprintln!();
            }
            std::process::ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("\ncroc-rs: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// 撤销。返回进程退出码。
pub async fn revoke(id: &str) -> std::process::ExitCode {
    let id = id.trim();
    if id.is_empty() {
        eprintln!("croc-rs: usage: croc-rs --revoke <transfer-id>");
        return std::process::ExitCode::FAILURE;
    }
    let Some(receipt) = read_receipts().into_iter().find(|receipt| receipt.id == id) else {
        eprintln!("croc-rs: no unexpired local revoke receipt for {id}");
        return std::process::ExitCode::FAILURE;
    };
    // 撤销只需要来源与 ID，不需要主密钥——但 `browser_url()` 会校验长度，所以给一个占位密钥。
    let share = Share { origin: receipt.origin, id: receipt.id, master_key: vec![0u8; croc_store::KEY_SIZE] };
    let client = match Client::new(None) {
        Ok(client) => client,
        Err(error) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match client.revoke(&share, &receipt.upload_token).await {
        // 已经没了也算成功：用户要的是"它不在了"。
        Ok(()) => {}
        Err(croc_store::Error::Service { status, .. }) if status == 404 || status == 410 => {}
        Err(error) => {
            eprintln!("croc-rs: {error}");
            return std::process::ExitCode::FAILURE;
        }
    }
    if let Err(error) = remove_receipt(id) {
        eprintln!("croc-rs: 更新撤销凭据失败：{error}");
        return std::process::ExitCode::FAILURE;
    }
    println!("Stored transfer {id} revoked.");
    std::process::ExitCode::SUCCESS
}

/// 在终端上问一句是/否。
fn confirm(prompt: &str) -> bool {
    use std::io::Write as _;
    print!("{prompt}");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "" | "y" | "yes")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_duration_syntax_matches_go() {
        assert_eq!(parse_expiration("1m").expect("1m"), time::Duration::MINUTE);
        assert_eq!(parse_expiration("12h").expect("12h"), time::Duration::hours(12));
        assert_eq!(parse_expiration("3d").expect("3d"), time::Duration::days(3));
        assert_eq!(parse_expiration("2w").expect("2w"), time::Duration::weeks(2));
        assert_eq!(parse_expiration(" 1d ").expect("带空格"), time::Duration::days(1));
    }

    #[test]
    fn bad_durations_are_rejected() {
        for wrong in ["", "1", "0m", "d", "1x", "-1d", "1.5h"] {
            assert!(parse_expiration(wrong).is_err(), "{wrong:?} 应当被拒");
        }
    }

    #[test]
    fn human_bytes_is_readable() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1500), "1.5 kB");
        assert_eq!(human_bytes(1_500_000), "1.5 MB");
    }

    #[test]
    fn store_sources_reject_the_unsupported_switches() {
        assert!(validate_store_sources(true, false, false, &[], &[], true).is_err());
        assert!(validate_store_sources(false, true, false, &[], &[], true).is_err());
        assert!(validate_store_sources(false, false, true, &[], &[], true).is_err());
        assert!(validate_store_sources(false, false, false, &["x".to_owned()], &[], true).is_err());
        assert!(validate_store_sources(false, false, false, &[], &["x".to_owned()], true).is_err());
        // 都给空、且忽略 stdin 时通过。
        assert!(validate_store_sources(false, false, false, &[], &[], true).is_ok());
    }

    #[test]
    fn downloads_must_be_positive() {
        assert!(validate_downloads(1).is_ok());
        assert!(validate_downloads(0).is_err());
        assert!(validate_downloads(-1).is_err());
    }
}
