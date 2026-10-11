//! 桌面端的空实现。
//!
//! 桌面端没有 Storage Access Framework：用户在系统文件管理器里挑目录，前端直接拿
//! 真实路径。这里恒返回 `None`，调用方据此退回自己的默认目录。

use std::marker::PhantomData;

use tauri::{Runtime, plugin::PluginApi};

use crate::Result;

/// SAF 选择器（桌面端）。
///
/// `PhantomData<fn() -> R>` 而不是 `PhantomData<R>`：后者会把 `R` 拖进 auto trait
/// 推导，而 `app.manage()` 要求 `Send + Sync`——本结构不持有 `R`，不该受它约束。
pub struct Saf<R: Runtime>(PhantomData<fn() -> R>);

impl<R: Runtime> Clone for Saf<R> {
    fn clone(&self) -> Self {
        Self(PhantomData)
    }
}

impl<R: Runtime> Saf<R> {
    /// 桌面端没有 SAF：恒返回 `None`。
    pub async fn pick_directory(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

/// 桌面端的建插件入口。
///
/// 返回 `Result` 是为了与 `mobile::init` 同签名——`lib.rs` 的 `setup` 里用一个
/// `platform::init(app, api)?` 同时调两处。桌面端不可能失败，错误分支恒不触发。
#[allow(clippy::unnecessary_wraps, reason = "与 mobile::init 同签名，lib.rs 才能用同一个 platform::init 调两处")]
pub fn init<R: Runtime, C: serde::de::DeserializeOwned>(
    _app: &tauri::AppHandle<R>,
    _api: PluginApi<R, C>,
) -> Result<Saf<R>> {
    Ok(Saf(PhantomData))
}
