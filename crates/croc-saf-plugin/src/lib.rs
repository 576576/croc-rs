//! 让用户在 Android 上挑一个 Storage Access Framework 目录。
//!
//! 为什么需要这个插件：Tauri 官方的 `tauri-plugin-dialog` 在 Android 上用
//! `Intent.ACTION_GET_CONTENT`，它选不了目录（那个 intent 只出文件），拿到的授权也
//! 不可持久化（`takePersistableUriPermission` 只认 `ACTION_OPEN_DOCUMENT` /
//! `ACTION_OPEN_DOCUMENT_TREE`），应用重启后写入会失败。
//!
//! 桌面端没有这套东西，[`Saf::pick_directory`] 恒返回 `None`——调用方不写 `cfg`
//! 分叉，判据是"有没有选到目录"。

use tauri::{
    Runtime,
    plugin::{Builder, TauriPlugin},
};

#[cfg(desktop)]
mod desktop;
#[cfg(mobile)]
mod mobile;

#[cfg(desktop)]
use desktop as platform;
#[cfg(mobile)]
use mobile as platform;

pub use platform::Saf;

/// 插件错误。
///
/// 桌面端没有与移动端通信这一步，所以只在移动端会真的出现。
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// 与移动端插件通信失败。
    #[cfg(mobile)]
    #[error("mobile plugin error: {0}")]
    Mobile(#[from] tauri::plugin::mobile::PluginInvokeError),
}

// Tauri 命令的错误类型必须能序列化。`thiserror` 只管 `Display`，序列化要自己写：
// 序列化成 `Display` 的那一句话，前端拿到的就是它。
impl serde::Serialize for Error {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

/// 插件 `Result`。
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Kotlin 侧 `pickDirectoryResult` 回传的形状。
///
/// key 要与 `SafPlugin.kt` 里 `JSObject.put("uri", ...)` 对上；`uri` 为 `null`
/// 表示用户取消。
#[cfg(mobile)]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct PickDirectoryResponse {
    pub uri: Option<String>,
}

/// 挑一个 SAF 目录，返回目录 URI（`content://.../tree/...`）；用户取消返回 `None`。
///
/// # Errors
///
/// 与移动端插件通信失败时返回错误。用户取消不算错误。
#[tauri::command]
async fn pick_directory<R: Runtime>(app: tauri::AppHandle<R>) -> Result<Option<String>> {
    use tauri::Manager as _;

    let saf = app.state::<Saf<R>>();
    saf.pick_directory().await
}

/// 建插件。
pub fn init<R: Runtime>() -> TauriPlugin<R> {
    Builder::new("saf")
        .invoke_handler(tauri::generate_handler![pick_directory])
        .setup(|app, api| {
            use tauri::Manager as _;

            let saf = platform::init(app, api)?;
            app.manage(saf);
            Ok(())
        })
        .build()
}
