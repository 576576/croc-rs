//! Android 侧的绑定：把 `pick_directory` 转发给 Kotlin 的 `SafPlugin`。
//!
//! 类名要与 `android/src/main/java/cn/sumitm/crocw/saf/SafPlugin.kt` 的包名 + 类名
//! 对上（`register_android_plugin` 的第二个参数是类名，包名走第一个参数）；
//! 命令名要与 Kotlin 侧 `@Command` 的方法名逐字一致。两边对不上只会在真机上
//! 抛 `ClassNotFoundException` / `NoSuchMethodError`，编译期看不出来。

use tauri::{
    Runtime,
    plugin::{PluginApi, PluginHandle},
};

use crate::{PickDirectoryResponse, Result};

/// Kotlin 包名（`SafPlugin.kt` 的 `package` 声明）。
const PLUGIN_IDENTIFIER: &str = "cn.sumitm.crocw.saf";

pub fn init<R: Runtime, C: serde::de::DeserializeOwned>(
    _app: &tauri::AppHandle<R>,
    api: PluginApi<R, C>,
) -> Result<Saf<R>> {
    let handle = api.register_android_plugin(PLUGIN_IDENTIFIER, "SafPlugin")?;
    Ok(Saf(handle))
}

/// SAF 目录选择器。
pub struct Saf<R: Runtime>(PluginHandle<R>);

impl<R: Runtime> Clone for Saf<R> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<R: Runtime> Saf<R> {
    /// 挑一个 SAF 目录，返回目录 URI；用户取消返回 `None`。
    ///
    /// 用 `_async` 变体：`ACTION_OPEN_DOCUMENT_TREE` 要等用户选完才回来，同步那条
    /// 会一直占着线程。
    pub(crate) async fn pick_directory(&self) -> Result<Option<String>> {
        let response: PickDirectoryResponse = self.0.run_mobile_plugin_async("pickDirectory", ()).await?;
        Ok(response.uri)
    }
}
