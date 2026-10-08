# [PENDING] GUI 计划（Tauri 2，产物 `crocw-rs`）

> 目标平台：Windows / macOS / Linux / **Android**（iOS 列为 stretch）。
> 后端复用 `croc-core` + `croc-engine`，与 CLI 同源——这是相对 FlCroc（Flutter + Go CGO）的最大优势。
> 产物名 **`crocw-rs`**（w = window），与 CLI 的 `croc-rs` 并列为 CI 的两个发布产物，见
> [../build-and-ci.md](../build-and-ci.md)。

## 0. [PENDING] 现状

`crates/croc-gui` 已有 Tauri 后端骨架（约 600 行）：

- `commands.rs`：`generate_code` / `send` / `receive` / `cancel_transfer` / `list_transfers`；
- `state.rs`：`TransferRegistry`（传输 ID → 取消句柄与状态）；
- `tauri.conf.json`：窗口、bundle 与图标配置齐了，`frontendDist` 指向 **`ui/`**。

**`ui/` 目录还不存在**，前端一行没写。所以下面第 2、3 节的界面与前端选型都还是待做的事；
第 4 节（Android）全部未开始。

## 1. 为什么是 Tauri 2

| 维度 | Tauri 2 | Slint | Dioxus | egui |
|------|---------|-------|--------|------|
| Android 成熟度 | ✅ 自 2.0 起正式支持，Gradle 集成 | ⚠️ JNI bridge，案例少 | ⚠️ 实验性 | ❌ 需自接 android-activity |
| 桌面成熟度 | ✅ | ✅ | ✅ | ✅ |
| 与 Rust 核心共享 | ✅ 同一进程直接调用 | ✅ | ✅ | ✅ |
| 前端人力/生态 | ✅ Web 生态最大 | ❌ 需学 .slint | ⚠️ Rust 写 UI | ⚠️ 需自绘组件 |
| 包体 | WebView 复用系统（Win 需装 WebView2） | 最小 | 小 | 小 |
| i18n / 富组件 | ✅ 现成库 | 一般 | 少 | 少 |

代价是 Windows 依赖 WebView2（Win11 自带，Win10 需分发 Evergreen Bootstrap），以及 Android 上的
存储适配工作量——后者无论选哪个框架都要做。

## 2. 功能对等表（对齐 FlCroc）

FlCroc 现状（从 `lib/` 与 `pubspec.yaml` 反推）：

| FlCroc 能力 | FlCroc 实现 | Tauri 版方案 | 优先级 |
|-------------|-------------|-------------|--------|
| 发送文件/文件夹 | `views/send` + `file_picker` | Tauri dialog plugin（桌面）/ 系统 SAF 选择器（Android） | P0 |
| 拖拽发送 | `widgets/file_drop_target` + `desktop_drop` | HTML5 drop（桌面）；Android 不支持 | P0（桌面） |
| 生成/展示三词码 | `core.generateCodePhrase` | `core_generate_code` command（已有） | P0 |
| 二维码展示 | `qr_flutter` | 前端 `qrcode` npm 包 | P0 |
| 二维码扫描 | `widgets/native_qr_scanner` | 前端 `zxing-wasm` / `html5-qrcode`（**免原生**，跨平台一致） | P1 |
| 接收（输入码 → 确认清单 → 下载） | `views/receive` | 同构，事件驱动 | P0 |
| 传输进度 / 速度 / 剩余时间 | `TransferProgress` 流 | `TransferEvent::Progress` → Tauri event | P0 |
| 取消传输 | `cancelTransfer` | `core_cancel_transfer`（已有） | P0 |
| 传输历史 | `views/history` + `drift`(SQLite) | `tauri-plugin-sql`（SQLite） | P1 |
| 设置页 | `views/settings`（relay、目录、主题、语言） | `tauri-plugin-store` 持久化 | P0 |
| 传输模式（auto / relay / tailscale） | FlCroc 无（Go 版才有） | 设置页 + `core_set_transport`；**tailscale 选项置灰并注明"等待上游 tailcat-rs"** | P1 |
| Tailscale authkey 配置 | 无 | ⏸ pending，解除后再加（设置页输入 + 授权 URL 跳转） | ⏸ |
| 主题（Material 3、自定义配色、深浅色） | `manager/theme_manager` | Tailwind + CSS 变量，跟随系统 + 手动切换 | P1 |
| 多语言 i18n | `l10n` + `assets/bundles`（zh / zh-Hant / en / ja / fr） | `i18next`，**直接复用现有 JSON 文案** | P1 |
| 桌面自定义标题栏 | `widgets/window_title_bar` + `window_manager` | Tauri window API + 自绘标题栏 | P1 |
| 打开文件 / 在文件夹中显示 | `open_filex` | `tauri-plugin-opener` | P1 |
| Toast 提示 | `fluttertoast` | 前端 toast 组件 | P2 |
| Android 系统分享接收 | `share_receiver` | 自定义 Kotlin 插件处理 `ACTION_SEND` | P1（Android） |
| 下载目录（Android） | `downloadsfolder` | SAF / MediaStore 插件 | P0（Android） |
| 网络状态监听 | `connectivity_plus` | 前端 `navigator.onLine` + 自定义插件 | P2 |
| 确认对话框（覆盖/续传/重命名） | `widgets/dialog` | 前端弹窗 + 确认回调 | P0 |
| 关于 / 致谢页 | `views/settings/acknowledgments` | 静态页 | P2 |

**新增（FlCroc 没有，值得做）**：

- 多任务并发（FlCroc 的 Go bridge 是全局单例，只能一个传输）；
- 传输完成后的校验结果显示；
- 内置中继地址健康检查（ping 延迟）；
- 桌面端"最近发送"快捷区；
- 当前数据面指示（局域网直连 / 中继）与手动切换；Tailscale 项保留占位但置灰。

## 3. 架构

```
┌──────────────────────────────────────────────┐
│ ui/  (Vite + React + TS + Tailwind)          │
│  ├── pages/  Send · Receive · History · Settings │
│  ├── store/  Zustand（传输列表、配置、主题）   │
│  ├── i18n/   i18next（复用 FlCroc 文案）      │
│  └── bridge/ invoke() / listen() 封装         │
└─────────────── Tauri IPC ────────────────────┘
┌──────────────────────────────────────────────┐
│ crates/croc-gui/  (Rust)                     │
│  ├── commands.rs   # #[tauri::command]        │
│  ├── sink.rs       # 进度 → app.emit()        │
│  ├── confirm.rs    # 确认 → 前端弹窗          │
│  ├── android/      # Kotlin：SAF / FGS / Share│
│  └── state.rs      # TransferRegistry        │
└─────────────── 直接函数调用 ─────────────────┘
┌──────────────────────────────────────────────┐
│ croc-core / croc-engine  （与 CLI 完全共享） │
└──────────────────────────────────────────────┘
```

### 3.1 Tauri Commands

```
core_get_version() / core_generate_code() / core_send_files(req) / core_receive_files(req)
core_cancel_transfer(id) / core_list_transfers() / core_shutdown()
```

- command 立即返回 `TransferId`，实际传输在后台 tokio task 跑（**不要** await 在 command 里，否则
  Tauri 的 WebView 会卡）。
- 进度通过 `app.emit("transfer://event", payload)` 推送。
- 需要用户决策时（覆盖/续传/同意接收），Rust 侧 `emit("transfer://confirm", req)` 并 `await` 一个
  oneshot，前端回 `core_resolve_confirm(id, action)`。

### 3.2 事件载荷

```ts
type TransferEvent =
  | { t: 'waiting',   transferId: string, code: string, relay: string }
  | { t: 'secured',   transferId: string }
  | { t: 'offer',     transferId: string, files: FileView[], totalBytes: number }
  | { t: 'start',     transferId: string, fileIndex: number, name: string, size: number, resumeBytes: number }
  | { t: 'progress',  transferId: string, fileIndex: number, done: number, total: number, bps: number }
  | { t: 'overall',   transferId: string, filesDone: number, filesTotal: number, bytesDone: number, bytesTotal: number }
  | { t: 'fileDone',  transferId: string, fileIndex: number, path: string }
  | { t: 'reconnect', transferId: string, attempt: number, reason: string }
  | { t: 'finished',  transferId: string, summary: Summary }
  | { t: 'failed',    transferId: string, code: string, message: string };
```

进度事件在 Rust 侧节流后再 emit。

### 3.3 前端技术选型

| 项 | 选择 | 理由 |
|----|------|------|
| 框架 | React + TypeScript | 生态最大，维护成本低 |
| 构建 | Vite | Tauri 官方模板；产物进 `ui/` |
| 样式 | Tailwind CSS + CSS 变量主题 | 深浅色切换、自定义配色 |
| 状态 | Zustand | 比 Redux 轻，适合中等规模 |
| i18n | i18next + JSON | 可**直接复用 FlCroc 的 `assets/bundles/*.json`** |
| 二维码 | `qrcode`（生成）+ `zxing-wasm`（扫描） | 纯 WASM，Android 上不用写原生相机代码 |
| 数据库 | `tauri-plugin-sql`（SQLite） | 历史记录 |
| 配置 | `tauri-plugin-store` | 设置项 |
| 文件/URL 打开 | `tauri-plugin-opener` | |
| 对话框 | `tauri-plugin-dialog` + 自绘弹窗 | |

## 4. Android 适配（移动端难点，单列）

### 4.1 构建

```bash
cargo tauri android init     # 生成 android/ 工程（Gradle + Kotlin）
cargo tauri android build --apk --target aarch64    # arm64
cargo tauri android build --apk --target x86_64     # x64
```

- NDK 需支持 `aarch64-linux-android` / `armv7-linux-androideabi` / `x86_64-linux-android`。
- `croc-core` 这条链路必须无 OpenSSL 依赖（用 RustCrypto 纯 Rust 后端），否则交叉编译会痛不欲生。
- CI：GitHub Actions `ubuntu-latest` + `ndk-r26`，产出 arm64/x64 APK。

### 4.2 存储：SAF / MediaStore（**必须写 Kotlin 插件**）

Android 10+ 分区存储下，应用不能直接用绝对路径访问公共目录。方案：

```
用户在系统文件选择器里选目录
        │  ACTION_OPEN_DOCUMENT_TREE
        ▼
Kotlin: Intent 结果 Uri → context.contentResolver.takePersistableUriPermission()
        │  通过 DocumentFile / ContentResolver 创建目标文件
        ▼  ParcelFileDescriptor.detachFd() 拿到 int fd
Rust:   File::from_raw_fd(fd)  ← 通过 JNI 把 fd 传给核心
```

这要求 `croc-engine` 的 fs 层抽象出"路径 或 raw fd"两种来源：

```rust
pub trait FileSink {
    fn write_at(&mut self, pos: u64, data: &[u8]) -> io::Result<()>;
    fn finish(self: Box<Self>) -> io::Result<()>;
}
pub trait FileSource {
    fn read_at(&self, pos: u64, buf: &mut [u8]) -> io::Result<usize>;
    fn len(&self) -> u64;
}
```

⚠️ **这个抽象目前不存在**（`croc-engine` 只有按路径读写），是 Android 阶段之前必须补的前置。

> v1 可先走简化方案：接收文件一律写入**应用私有目录**（`getExternalFilesDir(Download)`），完成后再
> 用 SAF 让用户"另存到"公共目录。这样传输路径全在私有目录，绕开 SAF 写入 fd 的复杂度，代价是多
> 一次拷贝。**推荐 v1 走这条**——那就不必先做上面的抽象。

### 4.3 前台服务与后台传输

Android 会在后台几分钟后杀掉进程。方案（Kotlin 插件）：

```kotlin
class TransferForegroundService : Service() {
    override fun onCreate() {
        startForeground(NOTIF_ID, buildNotification())   // 必须 5 s 内调用
        acquireWakeLock(PARTIAL_WAKE_LOCK)
    }
}
```

- `AndroidManifest.xml`：`FOREGROUND_SERVICE`、`FOREGROUND_SERVICE_DATA_SYNC`（Android 14+ 需声明
  类型）、`WAKE_LOCK`、`INTERNET`、`ACCESS_NETWORK_STATE`。
- Rust 侧在发送/接收开始时通过 JNI 调 `startForegroundService()`，结束时 `stopService()`。
- 通知显示实时进度（`NotificationCompat.Builder` + `setProgress`），进度由 Rust 侧回调推送。
- 必须引导用户关闭电池优化（`ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`），否则 Doze 下仍会断。

### 4.4 系统分享接入

`AndroidManifest.xml` 给 Activity 加：

```xml
<intent-filter>
  <action android:name="android.intent.action.SEND" />
  <action android:name="android.intent.action.SEND_MULTIPLE" />
  <category android:name="android.intent.category.DEFAULT" />
  <data android:mimeType="*/*" />
</intent-filter>
```

Kotlin 侧取出 `EXTRA_STREAM` 的 `Uri` 列表 → `ContentResolver.openFileDescriptor` → fd 传给 Rust 作为
发送源（同样需要 `FileSource` 的 fd 实现）。

### 4.5 其它移动端注意事项

| 项 | 处理 |
|----|------|
| WebView 版本 | Android 7+ 用系统 WebView（需 `minSdk 24`）；低端机上首屏可能慢，加 splash |
| 返回键 | 监听 `tauri://back-button` 或前端 history |
| 权限 | 仅 `INTERNET` + 通知；**不要**申请 `READ_EXTERNAL_STORAGE`（Android 13+ 已细分） |
| 大文件 OOM | 前端绝不持有文件字节；只用路径/URI |
| 断点 | Android 上进程被杀后状态丢失，需在启动时扫描未完成的 `.croc-partial` 文件提示续传 |

## 5. 桌面端适配

| 能力 | 方案 |
|------|------|
| 无边框窗口 + 自绘标题栏 | `tauri.conf.json` 设 `decorations: false`，前端画标题栏，调用 `getCurrentWindow().minimize()/toggleMaximize()/close()` |
| 文件拖拽 | 监听 `tauri://file-drop` 事件（比 HTML5 drop 更可靠，能拿到真实路径） |
| 剪贴板 | 前端 `navigator.clipboard`（Tauri 已授予权限） |
| 系统托盘 | `tauri-plugin-tray`（可选：最小化到托盘，传输中常驻） |
| 安装包 | Windows：NSIS/WiX；macOS：dmg；Linux：AppImage/deb/rpm（Tauri 自带） |
| 更新器 | `tauri-plugin-updater`（需签名；晚于 v1） |

## 6. 里程碑

| # | 任务 | 依赖 | 验收 |
|---|------|------|------|
| G0 | `croc-gui` 壳工程跑起来，能 `invoke("core_get_version")` | — | 空壳可运行 |
| G1 | 发送流程端到端（桌面）：选文件 → 出码 → 进度 → 完成 | CLI 传输链路 | 桌面三平台通过 |
| G2 | 接收流程端到端（桌面）：输码 → 清单确认 → 下载 | G1 | 含覆盖/重命名/续传三种决策 UI |
| G3 | 设置页 + 持久化 + i18n 骨架 | G1 | 切换语言/主题生效 |
| G4 | 历史记录（SQLite）+ 打开文件/显示位置 | G2 | |
| G5 | Android 工程跑通：`tauri android build` 出 APK，能发送 | G1 | 真机可发送 100 MB |
| G6 | Android 存储/前台服务/分享接入 | G5 | 后台 10 min 不被杀；系统分享可发 |
| G7 | 多任务并发 + 取消 + 错误提示完善 | G2 | 同时 3 个传输互不干扰 |
| G8 | 打包签名 + CI 矩阵（Win/Linux/macOS/Android） | G7 | 与 FlCroc 现有产物对齐 |
| G9 | i18n 全量文案迁移（zh / zh-Hant / en / ja / fr） | G3 | 复用 FlCroc JSON |
| G10 | 传输模式 UI（auto / relay），tailscale 选项置灰注明 pending | G3 | 三平台可切换并生效；pending 项不误导用户 |

## 7. 从 FlCroc 迁移

1. **文案**：`FlCroc/assets/bundles/*.json` 与 `lib/l10n` 直接转成 i18next 的 `locales/*.json`，
   key 名基本可复用。
2. **图标**：`assets/images/icon.png` 直接给 Tauri 生成各平台图标（`tauri icon`）。
3. **设置项**：FlCroc 的 `lib/common/prefs.dart` → `tauri-plugin-store` 的同名 key，尽量保持键名
   一致（方便老用户升级时不用重配）。
4. **历史表**：`drift` 的表结构导出为 SQLite 建表 SQL，直接用在 `tauri-plugin-sql`。
5. **过渡期**：两者可并存，包名不同；数据不做自动迁移，但设置可导出/导入 JSON。

## 8. 风险（GUI 专项）

| 风险 | 影响 | 对策 |
|------|------|------|
| Android WebView 版本差异导致 UI 异常 | 中 | 用保守的 CSS；`minSdk 24`；禁用前沿 CSS 特性 |
| Android 后台被杀 | 高 | 前台服务 + WakeLock + 引导关闭电池优化；传输状态持久化以便恢复 |
| SAF 存储复杂度超预期 | 高 | v1 先走"私有目录 + 完成后另存"，fd 通路留到 v1.1 |
| Windows WebView2 缺失 | 中 | 安装包内嵌 Evergreen Bootstrap，失败时给明确提示 |
| Tauri 2 API 变动 | 中 | 锁定 minor 版本，升级前跑冒烟 |
| 大文件传输时 WebView 与 Rust 争抢 CPU | 低 | 进度节流 + 数据面走独立 tokio worker |
