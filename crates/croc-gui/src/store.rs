//! 存储模式（G7）：把文件传到暂存服务，换一个取件链接。
//!
//! 与 CLI 的 `croc store` 走同一个 `croc-store` crate，所以链接格式、有效期语法、
//! 下载次数校验都与命令行出来的东西一模一样——换一条实现，用户拿到手的链接就会
//! 在某个细节上对不上。

use std::path::PathBuf;
use std::sync::Arc;

use croc_store::client::{Client, Progress, Reporter, UploadOptions};
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};
use time::format_description::well_known::Rfc3339;

use crate::commands::{ProgressEvent, new_id};
use crate::event;

/// 存储服务的默认来源，与 CLI 的 `--url` 默认值一致。
pub const DEFAULT_ORIGIN: &str = "https://getcroc.com";

/// 上传参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoreArgs {
    /// 要上传的路径。
    pub paths: Vec<String>,
    /// 有效期写法（`90m` / `12h` / `3d` / `2w`）；空则由服务端定。
    #[serde(default)]
    pub expiration: Option<String>,
    /// 允许的下载次数；`0` 表示用默认值。
    #[serde(default)]
    pub downloads: i64,
    /// 服务来源；空则用 [`DEFAULT_ORIGIN`]。
    #[serde(default)]
    pub url: Option<String>,
}

/// 上传结果：界面要显示的就是这条链接。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredResult {
    /// 这次上传的标识（只用来把进度对上号）。
    pub id: String,
    /// 给收件方的浏览器链接。
    pub link: String,
    /// 只有本端持有的撤销令牌。
    pub upload_token: String,
    /// 服务端实际允许的下载次数。
    pub downloads: i64,
    /// 服务端给的过期时间（RFC 3339）。
    pub expires_at: String,
}

/// 上传并拿回链接。命令会等到上传结束（`croc store` 本身就是一次性的 HTTP 往返）。
///
/// # Errors
///
/// 路径为空、有效期写法不合法、服务拒绝或网络中断。
#[tauri::command]
pub async fn store_upload(app: AppHandle, args: StoreArgs) -> Result<StoredResult, String> {
    if args.paths.is_empty() {
        return Err(t!("gui.error.no_paths").to_string());
    }
    let expiration = match args.expiration.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        Some(value) => croc_store::parse_expiration(value).map_err(|error| error.to_string())?,
        None => time::Duration::ZERO,
    };
    let origin = args.url.as_deref().map(str::trim).filter(|value| !value.is_empty()).unwrap_or(DEFAULT_ORIGIN);
    let paths: Vec<PathBuf> = args.paths.into_iter().map(PathBuf::from).collect();
    let id = new_id("store");

    let client = Client::new(None).map_err(|error| error.to_string())?;
    let reporter: Arc<dyn Reporter> = Arc::new(StoreReporter { app: app.clone(), id: id.clone() });
    let result = client
        .upload(origin, &paths, UploadOptions { downloads: args.downloads, expiration }, &reporter)
        .await
        .map_err(|error| error.to_string())?;

    Ok(StoredResult {
        id,
        link: result.share.browser_url().map_err(|error| error.to_string())?,
        upload_token: result.upload_token,
        downloads: result.downloads,
        expires_at: result.expires_at.format(&Rfc3339).unwrap_or_default(),
    })
}

/// 把上传进度接到前端的 `croc://progress` 上。
///
/// 复用传输那一条事件，是为了让界面不必为存储模式另学一种进度格式。
struct StoreReporter {
    app: AppHandle,
    id: String,
}

impl Reporter for StoreReporter {
    fn status(&self, message: &str) {
        let _ = self.app.emit(event::PROGRESS, ProgressEvent::plain(&self.id, "store", message));
    }

    fn progress(&self, progress: Progress) {
        let _ = self.app.emit(
            event::PROGRESS,
            ProgressEvent {
                id: self.id.clone(),
                direction: "store".to_owned(),
                status: String::new(),
                file: Some(progress.file_name),
                transferred: u64::try_from(progress.total_bytes).unwrap_or(0),
                total: u64::try_from(progress.total_size).unwrap_or(0),
            },
        );
    }
}
