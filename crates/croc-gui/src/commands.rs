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
use croc_engine::{EngineError, EngineEvent, ReceiveRequest, SendRequest, StdoutMode};
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
    /// 整次传输已搬的字节。文件内也在连续更新，不是每收完一个文件才跳一次。
    pub transferred: u64,
    /// 整次传输的总字节（还没拿到清单时为 0）。
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
    ///
    /// 名字过一遍 [`croc_core::policy::display_name`]：文本传输的载荷在线上叫
    /// `croc-stdin-*`（协议要的），直接摆到确认卡上会让用户以为要收一个临时文件。
    pub fn from_manifest(id: &str, info: &SenderInfo) -> Self {
        Self {
            id: id.to_owned(),
            files: info
                .files_to_transfer
                .iter()
                .map(|entry| OfferFile {
                    name: croc_core::policy::display_name(&entry.name, info.sending_text),
                    size: u64::try_from(entry.size).unwrap_or(0),
                })
                .collect(),
            folders: info.total_number_folders,
            total: info.files_to_transfer.iter().filter_map(|entry| u64::try_from(entry.size).ok()).sum(),
        }
    }
}

/// 每个路径占多少字节；目录不报大小（`None`），读不到的也一样。
///
/// 前端只拿到路径、拿不到大小，而列表要照 croc web 那样在文件名右边显示大小。
/// 目录那格留空：递归求和要为一次显示去遍历整棵树，而它的值对用户没有意义。
#[tauri::command]
pub fn stat_paths(paths: Vec<String>) -> Vec<Option<u64>> {
    paths.iter().map(|path| size_of(std::path::Path::new(path))).collect()
}

/// 文件的字节数。目录与其它（符号链接、读不到的）一律 `None`。
fn size_of(path: &std::path::Path) -> Option<u64> {
    let meta = path.symlink_metadata().ok()?;
    if meta.is_file() { Some(meta.len()) } else { None }
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
    // Android 上用户挑的目录是 SAF 的 `content://` URI，引擎走不通（`files::survey`
    // 起手就是 `symlink_metadata`）。先整个拷进应用私有目录，拿真实路径去发，发完删。
    // 桌面端没有 `content://`，这一步是空操作。
    let (staged, unreadable) = stage_saf_directories(&mut paths);
    if paths.is_empty() {
        // 挑过目录但一个都没留下：目录读不到（空目录 / 授权失效），这比"没选东西"
        // 更具体，别报成 no_paths 让用户去反复重点。
        let key = if unreadable { "gui.error.unreadable_dir" } else { "gui.error.no_paths" };
        return Err(t!(key).to_string());
    }
    let curve = parse_curve(args.curve.as_deref())?;
    let (code, components) = resolve_code(args.code.as_deref())?;
    let relay = pick_relay(&code, args.relay)?;
    let id = new_id("send");

    let events = bridge(&app, &id, "send", None);
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
        let result = croc_engine::send(request, token)
            .await
            .map(|summary| t!("gui.summary.send", files = summary.files, bytes = summary.bytes).to_string());
        // 成败都要清：留着就是每一次发送都往缓存区堆一份完整副本。
        cleanup_staged(&staged);
        result
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

    let received: Received = Received::default();
    let events = bridge(&app, &id, "receive", Some(received.clone()));
    // 落盘目录与发布目标是两件事。Android 上用户在界面上挑的目录是 SAF 的
    // `content://` 目录 URI——它不能当文件系统路径用（`create_dir_all` 会撞
    // read-only），所以字节仍然先落到应用私有目录，收完再按 URI 发布过去。
    // 桌面端 `out_dir` 永远是真实路径，`saf_tree` 为 `None`。
    // 空串按"没给"处理：前端在 Android 上没挑目录时就是空串，不能拿它当路径。
    let requested = args.out_dir.filter(|value| !value.trim().is_empty());
    let saf_tree = requested.as_deref().and_then(saf_tree_of);
    // 挑了 SAF 目录时字节仍要先落私有目录（收完再按 URI 发布过去），所以两种情况下
    // 兜底目录都得先问出来。
    // 拿不到私有目录就等于没有落盘位置。硬着头皮用 `.` 会撞上 "Read-only file
    // system"（那边的当前目录是只读的根），而那句话指不到真正的原因。
    let Some(fallback) = default_out_dir() else {
        return Err(t!("gui.error.no_sandbox").to_string());
    };
    let out_dir = PathBuf::from(match &saf_tree {
        Some(_) => fallback,
        None => requested.unwrap_or(fallback),
    });
    let request = ReceiveRequest {
        relay,
        relay_password: args.pass.unwrap_or_else(|| DEFAULT_PASSPHRASE.to_owned()),
        components,
        curve: parse_curve(args.curve.as_deref())?,
        out_dir: out_dir.clone(),
        preferences: Preferences { overwrite: args.overwrite, rename: args.rename },
        no_compress: false,
        // GUI 还没有 `--local` 的开关：默认走中继。
        local: false,
        proxy: croc_engine::proxy::Proxy::default(),
        multicast: croc_engine::local::MULTICAST_GROUP_V4,
        relay6: DEFAULT_RELAY6.to_owned(),
        ip: String::new(),
        // GUI 的标准输出没人看：文本传输若照 CLI 那样"打完删掉"，用户什么都拿不到。
        stdout: StdoutMode::Never,
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
            let mut text = t!(
                "gui.summary.receive",
                received = summary.received,
                bytes = summary.bytes,
                skipped = summary.skipped
            )
            .to_string();
            // Android 上收到的文件先落在应用私有目录里（用户看不见），收完再发布出去：
            // 用户挑过目录就发到那个 SAF 目录，没挑过则发到公共下载目录。桌面端这一步
            // 是空操作，`published` 为 0，不会追加文案。
            let published = publish_received(&out_dir, saf_tree.as_deref(), &received);
            if published > 0 {
                text.push('\n');
                let key = if saf_tree.is_some() { "gui.summary.published_tree" } else { "gui.summary.published" };
                text.push_str(&t!(key, count = published));
            }
            text
        })
    });
    Ok(id)
}

/// 落盘的文件要发布到哪儿：`Some(tree_uri)` 是用户挑的 SAF 目录，`None` 是公共下载目录。
///
/// `content://` 是 SAF 的判据。桌面端不会出现这个 scheme——那边 `out_dir` 是用户手填
/// 或系统文件管理器给的路径。
fn saf_tree_of(out_dir: &str) -> Option<String> {
    let trimmed = out_dir.trim();
    trimmed.starts_with("content://").then(|| trimmed.to_owned())
}

/// 把 `paths` 里的 SAF 目录 URI 就地换成拷进私有目录后的真实路径。
///
/// 返回 `(要清理的暂存目录, 有没有 SAF 目录没拷出来)`。第二个值用来区分"用户根本没选
/// 东西"与"选了但读不到"——两者的提示文案不同。
///
/// 引擎只认文件系统路径，SAF 目录必须先整个拷出来。拷不出来的（目录空、授权失效）
/// 直接从 `paths` 里去掉——留着只会让整次发送在 `survey` 那步报错。
fn stage_saf_directories(paths: &mut Vec<String>) -> (Vec<String>, bool) {
    let mut staged = Vec::new();
    let mut unreadable = false;
    let mut kept = Vec::with_capacity(paths.len());
    for path in paths.drain(..) {
        match saf_tree_of(&path) {
            // 普通路径：桌面端的手填路径、系统文件管理器给的路径，原样留着。
            None => kept.push(path),
            // SAF 目录：换成私有目录里的真实路径，记下等会儿删；拷不出来就丢掉。
            Some(uri) => match crate::android::stage_directory(&uri) {
                Some(local) => {
                    staged.push(local.clone());
                    kept.push(local);
                }
                None => unreadable = true,
            },
        }
    }
    *paths = kept;
    (staged, unreadable)
}

/// 删掉 [`stage_saf_directories`] 拷出来的目录。
fn cleanup_staged(staged: &[String]) {
    for path in staged {
        crate::android::remove_staged(path);
    }
}

/// 把收到的文件发布出去，返回发布成功的个数。
///
/// `saf_tree` 为 `None` 时走公共下载目录（[`crate::android::publish_to_downloads`]），
/// 桌面端两个分支都是空操作。
fn publish_received(out_dir: &std::path::Path, saf_tree: Option<&str>, received: &Received) -> usize {
    let names = std::mem::take(&mut *received.lock().unwrap_or_else(std::sync::PoisonError::into_inner));
    names
        .iter()
        .filter(|name| {
            let local = out_dir.join(name);
            let local = local.to_string_lossy();
            saf_tree.map_or_else(
                || crate::android::publish_to_downloads(&local).is_some(),
                |tree| crate::android::saf_publish(tree, &local, name).is_some(),
            )
        })
        .count()
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

/// 接收过程中收到的文件名（`EngineEvent::FileDone` 记下来的）。
type Received = Arc<std::sync::Mutex<Vec<String>>>;

/// 没指定落盘目录时用哪个：Android 上是应用私有目录，桌面端是当前目录。
///
/// Android 上拿不到私有目录时返回 `None`，由调用方报"没有可落盘的位置"——回落到
/// `.` 会撞上 "Read-only file system"（那边的当前目录是只读的根），那句话指不到
/// 真正的原因。
///
/// 桌面端这一支恒为 `Some`（`Option` 只为和 Android 那一支同签名），clippy 会在非
/// Android 目标上把它挑出来；Android 那一支真会返回 `None`，不能用 `expect`。
#[cfg_attr(not(target_os = "android"), allow(clippy::unnecessary_wraps))]
fn default_out_dir() -> Option<String> {
    #[cfg(target_os = "android")]
    {
        crate::android::sandbox_dir()
    }
    #[cfg(not(target_os = "android"))]
    {
        Some(".".to_owned())
    }
}

/// 收到文件的真实落盘目录（Android 上是应用私有目录，桌面端是当前目录）。
///
/// 界面上「保存到」那格在 Android 存的是 SAF 目录 URI，不能拿来拼文件路径——
/// 另存与分享要的是私有目录里那个真实文件，靠这条命令问出来。
#[tauri::command]
pub fn sandbox_dir() -> String {
    default_out_dir().unwrap_or_default()
}

/// 把引擎事件转成 Tauri event。
///
/// 回调必须是 `Fn`（引擎那边是 `Arc<dyn Fn>`，不要求 `FnMut`），所以累加器不能用
/// 捕获的 `let mut`，只能用 `AtomicU64` / `Mutex`。多花的这几行换来的是：进度上报
/// 不需要引擎配合加锁。
///
/// 报给前端的 `transferred` 是整次传输的累计值，但它是连续的：文件内的进度
/// （`EngineEvent::FileProgress`，100 ms 一次）加上已收工文件的字节数。只按
/// `EngineEvent::Progress`（每收完一个文件报一次）走的话，界面在大文件上会长时间
/// 停在同一个百分比上。
fn bridge(app: &AppHandle, id: &str, direction: &str, received: Option<Received>) -> croc_engine::EventSink {
    use std::sync::atomic::{AtomicU64, Ordering};

    let app = app.clone();
    let id = id.to_owned();
    let direction = direction.to_owned();
    // 已收工文件的累计字节。
    let done = AtomicU64::new(0);
    // 正在搬的这个文件的大小，收工时并进 `done`。
    let current = AtomicU64::new(0);
    let total = AtomicU64::new(0);
    let file: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
    // 接收时把收到的文件名记下来，收完统一发布到下载目录（Android）。
    let received = received.unwrap_or_default();

    Arc::new(move |event: EngineEvent| {
        let mut status = String::new();
        let mut bytes = None;
        match event {
            EngineEvent::Status(message) => status = message,
            EngineEvent::FileStarted { name, size } => {
                current.store(size, Ordering::Relaxed);
                // 总量要等 `Progress` 才准；在那之前用已开工文件的大小累加顶着。
                if total.load(Ordering::Relaxed) == 0 {
                    total.fetch_add(size, Ordering::Relaxed);
                }
                if let Ok(mut guard) = file.lock() {
                    *guard = Some(name.clone());
                }
                status = t!("gui.status.transferring", name = name).to_string();
            }
            EngineEvent::FileProgress { transferred, .. } => {
                bytes = Some(done.load(Ordering::Relaxed).saturating_add(transferred));
            }
            EngineEvent::FileDone { name } => {
                done.fetch_add(current.swap(0, Ordering::Relaxed), Ordering::Relaxed);
                if let Ok(mut guard) = received.lock() {
                    guard.push(name.clone());
                }
                status = t!("gui.status.file_done", name = name).to_string();
            }
            // 每收完一个文件报一次：用引擎的累计值把上面自己算的校准掉。
            EngineEvent::Progress { transferred: now, total: sum } => {
                done.store(now, Ordering::Relaxed);
                total.store(sum, Ordering::Relaxed);
                bytes = Some(now);
            }
            // `EngineEvent` 是 `#[non_exhaustive]`：将来新增的事件默认不上报，
            // 免得一个没设计过的事件把前端弄懵。
            _ => return,
        }
        let payload = ProgressEvent {
            id: id.clone(),
            direction: direction.clone(),
            status,
            file: file.lock().ok().and_then(|guard| guard.clone()),
            transferred: bytes.unwrap_or_else(|| done.load(Ordering::Relaxed)),
            total: total.load(Ordering::Relaxed),
        };
        if !payload.status.is_empty() {
            // Android 上进度还要写进前台服务的通知：界面退到后台就看不见了，通知是那时
            // 唯一的窗口。桌面端这几个函数是空的。文件内的进度不带状态，不会每 100 ms
            // 刷一次通知。
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

    /// 发布文案是按分支取键的（`t!(key, ...)` 用的是变量），键名写错不会编译报错，
    /// 只会在界面上把键名原样显示出来。这里把两条都渲染一遍，键丢了就红。
    #[test]
    fn both_publish_summaries_resolve() {
        for key in ["gui.summary.published", "gui.summary.published_tree"] {
            let text = t!(key, count = 3).to_string();
            assert_ne!(text, key, "{key} 没有对应的文案");
            assert!(text.contains('3'), "{key} 没有代入 count：{text}");
        }
    }

    /// 桌面端的路径没有 `content://`，`stage_saf_directories` 必须原样放行。
    /// （这里跑的是桌面实现：`crate::android::stage_directory` 恒返回 `None`，
    /// 所以带 `content://` 的那些会被丢掉并标成 unreadable——正是"拷不出来就别发"。）
    #[test]
    fn staging_leaves_plain_paths_alone_and_drops_content_uris() {
        let mut paths = vec!["/tmp/a".to_owned(), "C:\\b".to_owned(), "content://tree/c".to_owned()];
        let (staged, unreadable) = stage_saf_directories(&mut paths);
        assert_eq!(paths, vec!["/tmp/a".to_owned(), "C:\\b".to_owned()]);
        assert!(staged.is_empty(), "桌面端没有 SAF，不该有暂存目录");
        assert!(unreadable, "带 content:// 却拷不出来，必须标成读不到");
    }

    /// 全是普通路径时不该被标成"目录读不到"。
    #[test]
    fn staging_plain_paths_reports_nothing_unreadable() {
        let mut paths = vec!["/tmp/a".to_owned()];
        let (staged, unreadable) = stage_saf_directories(&mut paths);
        assert_eq!(staged.len(), 0);
        assert!(!unreadable);
        assert_eq!(paths, vec!["/tmp/a".to_owned()]);
    }
}
