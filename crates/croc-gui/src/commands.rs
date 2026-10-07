//! Tauri 命令：前端能调的全部入口。
//!
//! 每个命令都是"解析参数 → 组装 `croc-engine` 请求 → 跑 → 把结果折成可序列化的错误"。
//! **这里没有一行协议代码**，所以 GUI 与 CLI 的行为不可能漂移。

// `#[tauri::command]` 展开出的胶水代码里有一个 `unreachable!()`，而 clippy 把它的 span
// 算到**我们写的函数签名**上——逐函数的 `#[allow]` 不管用（属性宏会重新生成整个条目）。
// 所以在这里整模块豁免一次：本文件里没有任何我们自己写的 `unreachable!()`。
#![allow(clippy::unreachable, reason = "tauri 宏展开产物的 span 落在我们的签名上")]

use std::path::PathBuf;
use std::sync::Arc;

use croc_core::codephrase;
use croc_core::policy::Preferences;
use croc_engine::{EngineError, EngineEvent, ReceiveRequest, SendRequest};
use croc_pake::CurveKind;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::event;
use crate::state::{TransferId, TransferRegistry};

/// 默认中继地址。
const DEFAULT_RELAY: &str = "croc.schollz.com:9009";
/// 默认中继密码。
const DEFAULT_PASSPHRASE: &str = "pass123";

/// 推给前端的进展事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressEvent {
    /// 哪次传输。
    pub id: TransferId,
    /// `"send"` 或 `"receive"`。
    pub direction: String,
    /// 一句人类可读的状态。
    pub status: String,
    /// 正在处理哪个文件。
    pub file: Option<String>,
    /// 已搬字节。
    pub transferred: u64,
    /// 总字节（未知为 0）。
    pub total: u64,
}

/// 推给前端的结束事件。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinishedEvent {
    /// 哪次传输。
    pub id: TransferId,
    /// `"send"` 或 `"receive"`。
    pub direction: String,
    /// 成功了没有。
    pub ok: bool,
    /// 失败原因（成功时为 `None`）。
    pub error: Option<String>,
    /// 结果摘要，直接给界面显示。
    pub summary: Option<String>,
}

/// 发送参数。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendArgs {
    /// 要发送的路径。
    pub paths: Vec<String>,
    /// 中继地址。
    #[serde(default)]
    pub relay: Option<String>,
    /// 中继密码。
    #[serde(default)]
    pub pass: Option<String>,
    /// 自定义口令码（不给就生成）。
    #[serde(default)]
    pub code: Option<String>,
    /// 曲线名。
    #[serde(default)]
    pub curve: Option<String>,
    /// 关掉压缩。
    #[serde(default)]
    pub no_compress: bool,
}

/// 接收参数。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveArgs {
    /// 口令码。
    pub code: String,
    /// 中继地址。
    #[serde(default)]
    pub relay: Option<String>,
    /// 中继密码。
    #[serde(default)]
    pub pass: Option<String>,
    /// 落盘目录。
    #[serde(default)]
    pub out_dir: Option<String>,
    /// 曲线名。
    #[serde(default)]
    pub curve: Option<String>,
    /// 直接覆盖同名文件。
    #[serde(default)]
    pub overwrite: bool,
    /// 同名时自动改名。
    #[serde(default)]
    pub rename: bool,
}

/// 生成（或校验）一个口令码。
///
/// 前端用它做"先给用户看一眼码，再点发送"的两步交互——口令码是这次会话的唯一凭据，
/// 应该让用户有机会抄下来。
///
/// # Errors
///
/// 用户给的自定义码不合法时返回错误。
#[tauri::command]
pub fn generate_code(requested: Option<String>) -> Result<String, String> {
    match requested {
        Some(code) => {
            codephrase::parse(&code).map_err(|error| error.to_string())?;
            Ok(code)
        }
        None => Ok(codephrase::generate(&mut croc_net::OsRandomness)),
    }
}

/// 开一次发送。命令立刻返回，传输在后台跑。
///
/// # Errors
///
/// 路径为空、曲线名不认识、口令码不合法时返回错误。**传输本身的失败走
/// [`event::FINISHED`] 事件**，不从命令返回——命令返回时传输才刚开始。
#[tauri::command]
pub async fn send(
    app: AppHandle,
    registry: State<'_, Arc<TransferRegistry>>,
    args: SendArgs,
) -> Result<TransferId, String> {
    if args.paths.is_empty() {
        return Err("至少选一个文件".to_owned());
    }
    let curve = parse_curve(args.curve.as_deref())?;
    let (code, components) = resolve_code(args.code.as_deref())?;
    let id = new_id("send");

    let events = bridge(&app, &id, "send");
    let request = SendRequest {
        relay: args.relay.unwrap_or_else(|| DEFAULT_RELAY.to_owned()),
        relay_password: args.pass.unwrap_or_else(|| DEFAULT_PASSPHRASE.to_owned()),
        components,
        code,
        curve,
        paths: args.paths.into_iter().map(PathBuf::from).collect(),
        hash_algorithm: "xxhash".to_owned(),
        no_compress: args.no_compress,
        // GUI 还没有 `--local` 的开关：默认走中继。下面三个只影响 `--local` 下的内建
        // 中继，所以这里的取值不会被用到。
        local: false,
        relay_base_port: None,
        transfers: croc_engine::local::LOCAL_DATA_PORTS,
        no_multi: false,
        machine_id: machine_id(),
        events,
    };

    let cancel = registry.register(id.clone(), "send");
    spawn(app, registry.inner().clone(), id.clone(), "send", cancel, move |token| async move {
        croc_engine::send(request, token)
            .await
            .map(|summary| format!("已发送 {} 个文件（{} 字节）", summary.files, summary.bytes))
    });
    Ok(id)
}

/// 开一次接收。命令立刻返回，传输在后台跑。
///
/// # Errors
///
/// 口令码不合法时返回错误。
#[tauri::command]
pub async fn receive(
    app: AppHandle,
    registry: State<'_, Arc<TransferRegistry>>,
    args: ReceiveArgs,
) -> Result<TransferId, String> {
    let code = args.code.clone();
    let components = codephrase::parse(&code).map_err(|error| error.to_string())?;
    let id = new_id("recv");

    let events = bridge(&app, &id, "receive");
    let request = ReceiveRequest {
        relay: args.relay.unwrap_or_else(|| DEFAULT_RELAY.to_owned()),
        relay_password: args.pass.unwrap_or_else(|| DEFAULT_PASSPHRASE.to_owned()),
        components,
        curve: parse_curve(args.curve.as_deref())?,
        out_dir: PathBuf::from(args.out_dir.unwrap_or_else(|| ".".to_owned())),
        preferences: Preferences { overwrite: args.overwrite, rename: args.rename },
        no_compress: false,
        // GUI 还没有 `--local` 的开关：默认走中继。
        local: false,
        no_multi: false,
        machine_id: machine_id(),
        // GUI 里"收不收"由前端的确认弹窗决定，这里给一个恒真的回调；
        // 真正的交互在清单到达之前就完成了（用户是主动点"接收"的）。
        confirm: None,
        events,
    };

    let cancel = registry.register(id.clone(), "receive");
    spawn(app, registry.inner().clone(), id.clone(), "receive", cancel, move |token| async move {
        croc_engine::receive(request, token)
            .await
            .map(|summary| format!("新收 {} 个（{} 字节），跳过 {}", summary.received, summary.bytes, summary.skipped))
    });
    Ok(id)
}

/// 取消一次传输。
///
/// # Errors
///
/// 传输已经结束时返回错误——前端据此提示用户，而不是静默什么都不做。
#[tauri::command]
pub fn cancel_transfer(registry: State<'_, Arc<TransferRegistry>>, id: String) -> Result<(), String> {
    if registry.cancel(&id) { Ok(()) } else { Err(format!("{id} 已经结束了")) }
}

/// 列出还在跑的传输。
///
/// # Errors
///
/// 本命令不会失败；`Result` 是为了与其它命令保持同一签名。
#[tauri::command]
pub fn list_transfers(registry: State<'_, Arc<TransferRegistry>>) -> Result<Vec<crate::state::TransferStatus>, String> {
    Ok(registry.list())
}

// ───────────────────────────── 内部工具 ─────────────────────────────

/// 把引擎事件转成 Tauri event。
///
/// 回调必须是 `Fn`（引擎那边是 `Arc<dyn Fn>`，不要求 `FnMut`），所以累加器不能用
/// 捕获的 `let mut`，只能用 `AtomicU64` / `Mutex`。多花的这几行换来的是：进度上报
/// 不需要引擎配合加锁。
fn bridge(app: &AppHandle, id: &str, direction: &str) -> croc_engine::EventSink {
    use std::sync::atomic::{AtomicU64, Ordering};

    let app = app.clone();
    let id = id.to_owned();
    let direction = direction.to_owned();
    let transferred = AtomicU64::new(0);
    let total = AtomicU64::new(0);
    let file: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    Arc::new(move |event: EngineEvent| {
        let snapshot = |status: String, current: Option<String>| ProgressEvent {
            id: id.clone(),
            direction: direction.clone(),
            status,
            file: current,
            transferred: transferred.load(Ordering::Relaxed),
            total: total.load(Ordering::Relaxed),
        };

        let payload = match event {
            EngineEvent::Status(status) => snapshot(status, file.lock().ok().and_then(|guard| guard.clone())),
            EngineEvent::FileStarted { name, size } => {
                if let Ok(mut guard) = file.lock() {
                    *guard = Some(name.clone());
                }
                total.fetch_add(size, Ordering::Relaxed);
                snapshot(format!("正在传 {name}"), Some(name))
            }
            EngineEvent::Progress { transferred: now, total: sum } => {
                transferred.store(now, Ordering::Relaxed);
                total.store(sum, Ordering::Relaxed);
                snapshot(String::new(), file.lock().ok().and_then(|guard| guard.clone()))
            }
            EngineEvent::FileDone { name } => snapshot(format!("完成 {name}"), Some(name)),
            // `EngineEvent` 是 `#[non_exhaustive]`：将来新增的事件默认不上报，
            // 免得一个没设计过的事件把前端弄懵。
            _ => return,
        };
        let _ = app.emit(event::PROGRESS, payload);
    })
}

/// 在后台跑一次传输，结束时发 [`event::FINISHED`] 并注销。
fn spawn<F, Fut>(
    app: AppHandle,
    registry: Arc<TransferRegistry>,
    id: TransferId,
    direction: &'static str,
    cancel: CancellationToken,
    run: F,
) where
    F: FnOnce(CancellationToken) -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<String, EngineError>> + Send + 'static,
{
    tauri::async_runtime::spawn(async move {
        let result = run(cancel).await;
        registry.unregister(&id);
        let payload = match result {
            Ok(summary) => {
                FinishedEvent { id, direction: direction.to_owned(), ok: true, error: None, summary: Some(summary) }
            }
            Err(error) => FinishedEvent {
                id,
                direction: direction.to_owned(),
                ok: false,
                // 引擎的错误已经过 `redact`，不会带口令或房间名。
                error: Some(error.to_string()),
                summary: None,
            },
        };
        let _ = app.emit(event::FINISHED, payload);
    });
}

fn parse_curve(name: Option<&str>) -> Result<CurveKind, String> {
    CurveKind::from_name(name.unwrap_or("p256")).map_err(|error| error.to_string())
}

fn resolve_code(requested: Option<&str>) -> Result<(String, codephrase::Components), String> {
    let code = requested.map_or_else(|| codephrase::generate(&mut croc_net::OsRandomness), ToOwned::to_owned);
    let components = codephrase::parse(&code).map_err(|error| error.to_string())?;
    Ok((code, components))
}

fn new_id(prefix: &str) -> TransferId {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{prefix}-{stamp}")
}

/// 机器指纹，与 CLI 用同一套算法。
fn machine_id() -> String {
    let host =
        std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "unknown".to_owned());
    format!("{:016x}", croc_engine::hash::xxh64(host.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn curves_default_to_p256_like_the_cli() {
        assert_eq!(parse_curve(None).expect("默认"), CurveKind::P256);
        assert_eq!(parse_curve(Some("siec")).expect("siec"), CurveKind::Siec);
        assert!(parse_curve(Some("nope")).is_err());
    }

    #[test]
    fn codes_are_generated_and_validated() {
        let (code, _) = resolve_code(None).expect("生成");
        assert!(resolve_code(Some(&code)).is_ok(), "生成的码必须能被自己解析");
        assert!(resolve_code(Some("short")).is_err());
    }

    #[test]
    fn transfer_ids_are_unique_and_prefixed() {
        let first = new_id("send");
        let second = new_id("send");
        assert_ne!(first, second, "两次传输不能撞 ID，否则取消会误伤");
        assert!(first.starts_with("send-"));
    }

    #[test]
    fn the_machine_id_matches_the_cli_shape() {
        let id = machine_id();
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|character| character.is_ascii_hexdigit()));
    }
}
