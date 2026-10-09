//! Tauri 命令：前端能调的全部入口。
//!
//! 每个命令都是"解析参数 → 组装 `croc-engine` 请求 → 跑 → 把结果折成可序列化的错误"。
//! 这里没有一行协议代码，所以 GUI 与 CLI 的行为不可能漂移。

// `#[tauri::command]` 展开出的胶水代码里有一个 `unreachable!()`，而 clippy 把它的 span
// 算到我们写的函数签名上——逐函数的 `#[allow]` 不管用（属性宏会重新生成整个条目）。
// 所以在这里整模块豁免一次：本文件里没有任何我们自己写的 `unreachable!()`。
#![allow(clippy::unreachable, reason = "tauri 宏展开产物的 span 落在我们的签名上")]

use std::path::PathBuf;
use std::sync::Arc;

use croc_core::codephrase;
use croc_core::policy::Preferences;
use croc_engine::{EngineError, EngineEvent, ReceiveRequest, SendRequest};
use croc_pake::CurveKind;
use croc_protocol::SenderInfo;
use rust_i18n::t;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};
use tokio_util::sync::CancellationToken;

use crate::event;
use crate::offers::OfferRegistry;
use crate::state::{TransferId, TransferRegistry};

/// `--relay6` 的默认值：与 CLI 同一个 IPv6 中继，和 `--relay` 一起做 Happy Eyeballs。
const DEFAULT_RELAY6: &str = "croc6.schollz.com:9009";
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
    /// 直接发一段文本（`croc send --text` 的等价物）。给了它，`paths` 可以不填。
    #[serde(default)]
    pub text: Option<String>,
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

impl ProgressEvent {
    /// 只有一句状态、没有字节数的进展（存储模式的状态播报用这个）。
    pub fn plain(id: &str, direction: &str, status: &str) -> Self {
        Self {
            id: id.to_owned(),
            direction: direction.to_owned(),
            status: status.to_owned(),
            file: None,
            transferred: 0,
            total: 0,
        }
    }
}

/// 推给前端的清单确认请求。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferEvent {
    /// 哪次传输。
    pub id: TransferId,
    /// 清单里的文件。
    pub files: Vec<OfferFile>,
    /// 还要建的空目录数。
    pub folders: i64,
    /// 总量（字节）。
    pub total: u64,
}

/// 清单里的一个文件，界面只需要名字与大小。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OfferFile {
    /// 文件名（不含目录）。
    pub name: String,
    /// 字节数。
    pub size: u64,
}

impl OfferEvent {
    /// 把发送方的清单折成界面要的形状。
    pub fn from_manifest(id: &str, info: &SenderInfo) -> Self {
        Self {
            id: id.to_owned(),
            files: info
                .files_to_transfer
                .iter()
                .map(|entry| OfferFile { name: entry.name.clone(), size: u64::try_from(entry.size).unwrap_or(0) })
                .collect(),
            folders: info.total_number_folders,
            total: info.files_to_transfer.iter().filter_map(|entry| u64::try_from(entry.size).ok()).sum(),
        }
    }
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
/// 路径为空、曲线名不认识、口令码不合法时返回错误。传输本身的失败走
/// [`event::FINISHED`] 事件，不从命令返回——命令返回时传输才刚开始。
#[tauri::command]
pub async fn send(
    app: AppHandle,
    registry: State<'_, Arc<TransferRegistry>>,
    args: SendArgs,
) -> Result<TransferId, String> {
    let text = args.text.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let mut paths = args.paths;
    if let Some(text) = text {
        // `--text` 的等价物：文本先落成 `croc-stdin-*` 临时文件，再走正常发送——
        // 接收方靠这个名字前缀认出"这次是文本"（croc_engine::text）。
        let artifact = croc_engine::text::write_text_artifact(text)?;
        paths.push(artifact.to_string_lossy().into_owned());
    }
    if paths.is_empty() {
        return Err(t!("gui.error.no_paths").to_string());
    }
    let curve = parse_curve(args.curve.as_deref())?;
    let (code, components) = resolve_code(args.code.as_deref())?;
    let relay = pick_relay(&code, args.relay)?;
    let id = new_id("send");

    let events = bridge(&app, &id, "send");
    let request = SendRequest {
        relay,
        relay_password: args.pass.unwrap_or_else(|| DEFAULT_PASSPHRASE.to_owned()),
        components,
        code,
        curve,
        paths: paths.into_iter().map(PathBuf::from).collect(),
        // GUI 还没有排除项与 .gitignore 的输入口，先给空。
        exclude: Vec::new(),
        exclude_file: Vec::new(),
        git: false,
        zip: false,
        // GUI 还没有代理设置，先直连。
        proxy: croc_engine::proxy::Proxy::default(),
        multicast: croc_engine::local::MULTICAST_GROUP_V4,
        relay6: DEFAULT_RELAY6.to_owned(),
        ip: String::new(),
        ask: false,
        sending_text: text.is_some(),
        // GUI 还没有 DNS 与限速的输入口。
        internal_dns: false,
        throttle_upload: None,
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
            .map(|summary| t!("gui.summary.send", files = summary.files, bytes = summary.bytes).to_string())
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
    offers: State<'_, Arc<OfferRegistry>>,
    args: ReceiveArgs,
) -> Result<TransferId, String> {
    let code = args.code.clone();
    let components = codephrase::parse(&code).map_err(|error| error.to_string())?;
    let relay = pick_relay(&code, args.relay)?;
    let id = new_id("recv");

    // GUI 里"收不收"由清单确认卡决定：清单推给前端，答案由 `respond_to_offer` 投递。
    // 一次接收只会走到清单确认这一步一次，所以 `wait` 的那一端放在 `Option` 里等着被
    // 取走就够了——真被调用第二次时，取不到它就按"拒绝"处理。
    let answer = Arc::new(std::sync::Mutex::new(Some(offers.wait(&id))));
    let offer_app = app.clone();
    let offer_id = id.clone();
    let confirm_async = Some(Arc::new(move |info: &SenderInfo| {
        let _ = offer_app.emit(event::OFFER, OfferEvent::from_manifest(&offer_id, info));
        let mut slot = answer.lock().ok();
        let pending = slot.as_mut().and_then(|slot| slot.take());
        Box::pin(async move {
            match pending {
                Some(answer) => answer.await.unwrap_or(false),
                None => false,
            }
        }) as croc_engine::ConfirmFuture
    }) as croc_engine::AsyncConfirmSink);

    let events = bridge(&app, &id, "receive");
    let request = ReceiveRequest {
        relay,
        relay_password: args.pass.unwrap_or_else(|| DEFAULT_PASSPHRASE.to_owned()),
        components,
        curve: parse_curve(args.curve.as_deref())?,
        out_dir: PathBuf::from(args.out_dir.unwrap_or_else(|| ".".to_owned())),
        preferences: Preferences { overwrite: args.overwrite, rename: args.rename },
        no_compress: false,
        // GUI 还没有 `--local` 的开关：默认走中继。
        local: false,
        proxy: croc_engine::proxy::Proxy::default(),
        multicast: croc_engine::local::MULTICAST_GROUP_V4,
        relay6: DEFAULT_RELAY6.to_owned(),
        ip: String::new(),
        stdout: false,
        internal_dns: false,
        no_multi: false,
        machine_id: machine_id(),
        confirm: None,
        confirm_async,
        events,
    };

    let cancel = registry.register(id.clone(), "receive");
    let offers_to_clear = offers.inner().clone();
    let id_to_clear = id.clone();
    spawn(app, registry.inner().clone(), id.clone(), "receive", cancel, move |token| async move {
        let result = croc_engine::receive(request, token).await;
        // 无论成败都要把等待中的决策端收掉，否则取消后的一次多余的"接受"会留在表里。
        offers_to_clear.abandon(&id_to_clear);
        result.map(|summary| {
            t!("gui.summary.receive", received = summary.received, bytes = summary.bytes, skipped = summary.skipped)
                .to_string()
        })
    });
    Ok(id)
}

/// 回答一次清单确认。
///
/// # Errors
///
/// 这次传输没有在等决策时返回错误——前端据此提示用户，而不是静默什么都不做。
#[tauri::command]
pub fn respond_to_offer(offers: State<'_, Arc<OfferRegistry>>, id: String, accept: bool) -> Result<(), String> {
    if offers.respond(&id, accept) { Ok(()) } else { Err(t!("gui.error.no_offer").to_string()) }
}

/// 让用户挑一个位置另存（Android 的 SAF）。
///
/// # Errors
///
/// 非 Android 平台返回错误：桌面端有系统文件管理器，不需要这一层。
#[tauri::command]
pub fn export_file(path: String) -> Result<(), String> {
    crate::android::export_file(&path);
    if cfg!(target_os = "android") { Ok(()) } else { Err(t!("gui.error.mobile_only").to_string()) }
}

/// 把文件交给别的应用（系统分享）。
///
/// # Errors
///
/// 非 Android 平台返回错误。
#[tauri::command]
pub fn share_file(path: String) -> Result<(), String> {
    crate::android::share_file(&path);
    if cfg!(target_os = "android") { Ok(()) } else { Err(t!("gui.error.mobile_only").to_string()) }
}

/// 切换界面语言。前端每次切语言都调一次，好让后端的错误与摘要跟着变。
#[tauri::command]
pub fn set_locale(locale: String) {
    rust_i18n::set_locale(&locale);
}

/// 取消一次传输。
///
/// # Errors
///
/// 传输已经结束时返回错误——前端据此提示用户，而不是静默什么都不做。
#[tauri::command]
pub fn cancel_transfer(
    registry: State<'_, Arc<TransferRegistry>>,
    offers: State<'_, Arc<OfferRegistry>>,
    id: String,
) -> Result<(), String> {
    // 取消要连清单确认一起放开：还挂在"等用户按按钮"上的传输收不到取消令牌。
    offers.abandon(&id);
    if registry.cancel(&id) { Ok(()) } else { Err(t!("gui.error.transfer_finished", id = id).to_string()) }
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
                snapshot(t!("gui.status.transferring", name = name).to_string(), Some(name))
            }
            EngineEvent::Progress { transferred: now, total: sum } => {
                transferred.store(now, Ordering::Relaxed);
                total.store(sum, Ordering::Relaxed);
                snapshot(String::new(), file.lock().ok().and_then(|guard| guard.clone()))
            }
            EngineEvent::FileDone { name } => snapshot(t!("gui.status.file_done", name = name).to_string(), Some(name)),
            // `EngineEvent` 是 `#[non_exhaustive]`：将来新增的事件默认不上报，
            // 免得一个没设计过的事件把前端弄懵。
            _ => return,
        };
        if !payload.status.is_empty() {
            // Android 上进度还要写进前台服务的通知：界面退到后台就看不见了，通知是那时
            // 唯一的窗口。桌面端这几个函数是空的。
            crate::android::notify_progress("crocw-rs", &payload.status);
        }
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
        // Android 上要挂前台服务：界面退到后台之后，没有它系统会直接把传输掐掉。
        crate::android::start_service();
        let result = run(cancel).await;
        registry.unregister(&id);
        // 队列空了才收：并发传输时不能因为其中一个结束就把服务关掉。
        if registry.list().is_empty() {
            crate::android::stop_service();
        }
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

/// 挑一台中继。
///
/// 用户在设置里给了就用给的；没给则按码落到公共中继池的某一台上——与 CLI 的
/// `relay_pool::index_of` 是同一条判据（`sha256(码) mod 池长`）。少了这一步，GUI 发出的
/// 码会让默认配置的 CLI 去另一台机器上等，两边各守一个确实存在、但也是真的空的房间，
/// 症状是接收端一直卡在认证而不是报错。
///
/// # Errors
///
/// 码落不到池上时返回错误（池为空或索引窄化失败）。
fn pick_relay(code: &str, requested: Option<String>) -> Result<String, String> {
    if let Some(relay) = requested.as_deref().map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(relay.to_owned());
    }
    let pool = croc_engine::public_relay::RELAYS;
    let Ok(count) = u32::try_from(pool.len()) else {
        return Err(t!("gui.error.relay_pool").to_string());
    };
    let index = codephrase::relay_index(code, count).map_err(|_| t!("gui.error.relay_pool").to_string())?;
    let Ok(index) = usize::try_from(index) else { return Err(t!("gui.error.relay_pool").to_string()) };
    pool.get(index).copied().map(str::to_owned).ok_or_else(|| t!("gui.error.relay_pool").to_string())
}

fn parse_curve(name: Option<&str>) -> Result<CurveKind, String> {
    CurveKind::from_name(name.unwrap_or("p256")).map_err(|error| error.to_string())
}

fn resolve_code(requested: Option<&str>) -> Result<(String, codephrase::Components), String> {
    let code = requested.map_or_else(|| codephrase::generate(&mut croc_net::OsRandomness), ToOwned::to_owned);
    let components = codephrase::parse(&code).map_err(|error| error.to_string())?;
    Ok((code, components))
}

pub(crate) fn new_id(prefix: &str) -> TransferId {
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
