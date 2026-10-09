# [IN PROGRESS] GUI 计划（Tauri 2，产物 `crocw-rs`）

> 进度：G0–G10 均已落地（前端工程、中继路由修复、清单确认、设置持久化、进度与 ETA、
> 二维码与复制、CI 前端构建、文本直传、存储模式、Android 宿主工程与 Rust→Kotlin 桥）。
> **G11（SSH 共享终端）未实现**：数据面还没有，界面上留了显式占位（第四格工作区）。

> 目标平台：Windows / macOS / Linux / **Android**（iOS 列为 stretch）。
> 后端复用 `croc-core` + `croc-engine`，与 CLI 同源。产物名 **`crocw-rs`**（w = window），与 CLI
> 的 `croc-rs` 并列，见 [../build-and-ci.md](../build-and-ci.md)。
>
> **UI 基准是 getcroc.com** —— 即上游的浏览器客户端 `croc/web`。界面以它的设计契约为准。

## 0. 现状

### 0.1 Rust 侧（`crates/croc-gui/src`，665 行）

| 文件 | 行数 | 内容 |
|------|------|------|
| `commands.rs` | 405 | `generate_code` / `send` / `receive` / `cancel_transfer` / `list_transfers` |
| `state.rs` | 148 | `TransferRegistry`：传输 ID → `CancellationToken` |
| `lib.rs` | 40 | 事件名常量、`init_tracing` |
| `main.rs` | 35 | 入口 |

事件桥在 `commands.rs` 的 `bridge()`：把 `EngineEvent` 折成 `ProgressEvent` 后 `emit("croc://progress")`，
结束时 `emit("croc://finished")`。

G2 之后新增两处：`offers.rs`（清单确认的一次性决策通道，带三个测试）与 `locales/*.yml`
（rust-i18n，键名与前端对齐，由 `set_locale` 切换）。

### 0.2 前端（`crates/croc-gui/ui`，已换成 Vite + React + TS）

| 路径 | 内容 |
|------|------|
| `index.html` | 单页挂载点；`<html data-theme>` 在 React 之前定好，避免首帧闪白 |
| `src/main.tsx` | `LocaleProvider` + `App` + `styles.css` |
| `src/App.tsx` | 工作区状态、三个事件的监听、发送 / 接收命令的调用 |
| `src/locales.ts` | `en` / `zh-CN` 两套文案 |
| `src/components/*` | `TitleBar` · `WorkspaceSwitch` · `SendPanel` · `ReceivePanel` · `FileList` · `SendCode` · `OfferCard` · `ProgressBlock` · `SettingsGrid` · `TransfersList` · `StatusDot` |
| `src/styles.css` | §1.1 的令牌 + §1.4 的组件规则（从上游 `styles.css` 按类名抽出）+ 少量自补 |

包管理器是 **pnpm 12**（`packageManager` 字段钉住版本，锁文件是 `pnpm-lock.yaml`）。
`tauri.conf.json` 的 `frontendDist` 指向 `ui/dist`，`beforeBuildCommand` 是
`pnpm --prefix ui run build`。

### 0.3 权限

`capabilities/default.json` 除 `core:default` 外，给了自绘标题栏（`core:window:allow-start-dragging`
/ `allow-minimize` / `allow-toggle-maximize` / `allow-close`）、事件监听（`core:event:allow-listen`）
与三个插件（`dialog` / `clipboard-manager` / `opener`）的默认权限集。

### 0.4 阻断级缺陷（G1，已修）

第一条已经在 `pick_relay`（`commands.rs`）里按上面那条判据修掉：用户没给中继就
`codephrase::relay_index(码, 池长)` 折到 `croc_engine::public_relay::RELAYS`，给了就以用户的为准。
第二、三条分别在 G1（文件选择器 + `tauri://file-drop`）与 G2（清单确认，见 §4.1）里补上。

| 缺陷 | 事实 | 后果 |
|------|------|------|
| **GUI 绕过公共中继池** | `commands.rs` 把中继定成 `croc.schollz.com:9009` 直接传进 `SendRequest` / `ReceiveRequest`。CLI 在 `--relay` 未被显式给出时按 `sha256(码) mod 4` 落到 `1..4.getcroc.com`（`crates/croc-cli/src/relay_pool.rs`）；`croc-engine` 只导出这张表（`public_relay.rs`），自己不选路 | GUI 默认发出的码，CLI 默认去另一端接，双方停在各自的空房间里。**症状是接收端一直卡在认证，不是报错**。Go v11 的默认也同样已是 `1..4` 池 |
| **手填路径** | 没有文件选择器、没有拖拽 | 桌面端可用性不成立 |
| **接收无清单确认** | `ReceiveRequest.confirm` 传 `None`，且 `ConfirmSink` 是同步 `Fn(&SenderInfo) -> bool` | 做不了清单确认卡（见 §4 的 P0 项） |

第一条尤其致命：它让"GUI 与 CLI 同源"在**默认配置**下不成立。修法是 GUI 走与 CLI 同一条判据——
用户没自定义中继时，用 `codephrase::relay_index(码, 4)` 把码折到 `croc_engine::public_relay::RELAYS`
的成员上；用户在设置里改了中继就以用户给的为准（这正是 CLI 里 `relay_pool::applies()` 的语义）。

## 1. UI 基准：getcroc.com（croc web）

getcroc.com 不是介绍页——它是上游的浏览器端 croc 客户端，界面本体。基线 `v11.5.4-5-g8dd74239`。

| 上游路径 | 作用 | 我们用不用 |
|----------|------|-----------|
| `src/styles.css`（4487 行） | 设计令牌与全部组件样式 | 用（裁剪后） |
| `src/App.tsx`（2389 行） | 工作区、发送 / 接收、进度、设置 | 用其结构与文案 |
| `src/stored-ui.tsx` | 存储模式（加密链接 + 有效期）的 UI | 用（`croc-store` 已就绪） |
| `src/share-qr.tsx` | 二维码分享 | 用 |
| `src/SSHPanel.tsx` | SSH 模式 + xterm.js，懒加载 | pending（依赖 SSH 数据面） |
| `src/protocol/*`、`src/wasm/*` | 协议与 Go→WASM 密码学 | 不用，Rust 侧已有 |
| `Blog*`、`.donation-banner`、`.cli-download`、`.home-reviews` | 官网营销区 | 不用 |

### 1.1 设计令牌（照抄，色值不得改）

| 令牌 | 深色 | 浅色 |
|------|------|------|
| `--canvas` | `#121411` | `#f2f1ec` |
| `--background` | `#050605` | `#ffffff` |
| `--foreground` | `#ffffff` | `#000000` |
| `--muted` | `#a4a4a4` | `#555555` |
| `--subtle` | `#909090` | `#686868` |
| `--soft-border` | `#4b4b4b` | `#aaaaaa` |
| `--panel-tint` | `#0d0f0d` | `#f7f6f2` |
| `--danger` | `#ffffff` | `#000000` |

字体：`"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace`
（npm `@fontsource-variable/jetbrains-mono`），`font-synthesis: none`、
`text-rendering: geometricPrecision`。**通体等宽，没有第二套字体，没有一个彩色。**

### 1.2 版式规则

| 规则 | 取值 |
|------|------|
| 圆角 | `0`（`button / input / textarea / details / .panel`） |
| 内容宽度 | `.site-shell { width: min(1080px, calc(100% - 32px)); padding: 52px 0 24px; }` |
| 最小交互高度 | 42–43px（移动端 48px） |
| 按钮字号 / 字重 | 12px / **750** |
| 主描边 | `1px solid var(--foreground)`；次要内框用 `--soft-border` |
| 焦点 | `outline: 2px solid var(--foreground); outline-offset: 2px`（`:focus-visible`） |
| 状态表示 | 不靠颜色，靠**前景背景互换**：选中即 `color: var(--background); background: var(--foreground)` |
| 字段标签 | `.field-label`：10px / 750 / `letter-spacing: .12em` / 全大写 |

### 1.3 主题切换

`<html data-theme="dark|light">`，同时写 `style.colorScheme` 与 `<meta name="theme-color">`
（`#121411` / `#f2f1ec`）。取值顺序：`localStorage["croc-web-theme"]` → 否则
`matchMedia("(prefers-color-scheme: light)")` 为真取 light，**兜底是 dark**。
localStorage 在 Tauri 的 WebView 里照常可用，所以这段可以整体搬过来；是否换成 Tauri 的
系统主题 API 在 G3 连同"设置项持久化方式"一起定。

### 1.4 组件清单

| 组件 | 类名 | 要点 |
|------|------|------|
| 工作区切换 | `.workspace-switch` | 两格 `inline-grid`，选中格反色 |
| 丢弃区 | `.drop-zone` | `min-height: 112px`，`1px dashed var(--subtle)` |
| 口令码块 | `.send-code` | `grid-template-columns: auto minmax(0,1fr) 42px`：标签 + 码值 + 复制按钮 |
| 二维码 | `.share-qr` | 可折叠，默认收起 |
| 清单确认卡 | `.offer` | `1px solid var(--foreground)`，背景 `--panel-tint` |
| 进度条 | `.progress-track` | `height: 7px`，`1px solid var(--foreground)` |
| 速率 / ETA | `.progress-metrics` | 两格；ETA 由前端 `project()` 自算（见下） |
| 文件清单 | `.file-list` | `max-height: 154px`，可滚 |
| 连接状态点 | `.status-dot` | 7×7 描边；连上后 `background: var(--foreground)` |
| 传输双栏 | `.transfer-grid` | `1fr 1fr`，共享上 / 左边框 |
| 高级设置 | `.settings-grid` | 三列；网关、中继地址、中继密码、记住密码 |
| 图标 | `lucide-react` | 线性描边，与整体一致 |

### 1.5 运行期配置

上游 `public/config.js` 暴露 `window.__CROC_RUNTIME_CONFIG__`：

```json
{"gatewayURL":"/ws","relayAddresses":["1.getcroc.com:9009","2.getcroc.com:9009","3.getcroc.com:9009","4.getcroc.com:9009"],"relayPassword":"pass123","store":{"enabled":true,"maxTransferBytes":1073741824,"maxFiles":100,"maxDownloads":1000,"expiresSeconds":86400,"maxExpiresSeconds":0}}
```

桌面端不需要这层间接配置：中继地址与密码直接作为设置项，默认值取
`croc_engine::public_relay::RELAYS`（配合 §0.4 的码→索引路由）。

## 2. 与现行 `ui/` 的差距

| 项 | 现行 `ui/` | 目标 |
|----|-----------|------|
| 技术栈 | 原生三文件，无构建 | Vite + React + TS（与上游同栈） |
| 强调色 | `--accent: #2d7d9a` | 无强调色，单色体系 |
| 圆角 | 6px | 0 |
| 字体 | 系统 sans-serif | JetBrains Mono Variable |
| 主题 | 只跟系统 | `data-theme` + 手动切换 + 持久化 |
| 导航 | 三个标签页 | 工作区切换 + 发送 / 接收双栏 + 可折叠高级设置 |
| 选文件 | 文本框手填路径 | `.drop-zone` + 文件选择器 |
| 口令码 | 一行文本 | 码块 + 复制 + 二维码 + 浏览器链接 |
| 接收 | 直接下载 + 覆盖/改名勾选 | `.offer` 清单确认卡 + 决策按钮 |
| 进度 | 百分比条 | 总量 + 单文件 + 速率 + ETA |
| 文本直传 / 存储模式 / SSH | 无 | 见 §4 |

## 3. 架构

```
crates/croc-gui/
  ui/                     Vite + React + TS 项目根
    package.json  vite.config.ts  tsconfig.json
    index.html
    src/
      main.tsx
      App.tsx
      styles.css          从上游 styles.css 裁剪；§1.1 令牌表的值不得改
      components/         workspace-switch · drop-zone · send-code · share-qr
                          offer · progress-block · file-list · settings-grid
      lib/tauri.ts        invoke / listen 封装（现 main.js 那层）
    dist/                 构建产物，不入库
  src/                    Rust 侧（现有四个文件，随 §4 增补）
  tauri.conf.json         frontendDist 指向 ui/dist，加 beforeBuildCommand
```

- **`beforeBuildCommand` 要在 `ui/` 下跑包管理器**。Tauri 2 不会自动切工作目录：命令是在
  `tauri.conf.json` 所在的 crate 根执行的。实测 `pnpm --prefix ui run build` 可行，CI 与
  本地都用它（本地想省事就 `cd crates/croc-gui/ui && pnpm run build`，两者等价）。
- 事件名与载荷形状保持现状（`croc://progress` / `croc://finished`），避免 Rust 与前端同时改。
  新增的一个是 `croc://offer`（清单确认，见 §4.1）。
- ETA 在前端 `src/format.ts::project()` 里算：拿最近 8 个观测点做线性外推。**没有引
  `arrival-time`**——这里的速率抖动本来就比拟合精度大一个量级，为一个几十行的窗口平均
  再拉一个依赖不划算。这是与上游有意为之的一处偏差。
- `withGlobalTauri` 暂保留；新代码通过 `@tauri-apps/api` 导入，逐步不依赖全局对象。

### 3.1 Rust 侧要补的依赖

| crate | 用途 | 优先级 |
|-------|------|--------|
| `tauri-plugin-dialog` | 选择要发的文件、选择落盘目录 | P0 |
| `tauri-plugin-clipboard-manager` | 复制口令码与分享链接（`navigator.clipboard` 在 WebView 里不稳） | P0 |
| `tauri-plugin-opener` | 打开文件 / 在文件管理器中显示 | P1 |
| `tauri-plugin-store` | 设置持久化。是否引入取决于 G3 的决定——上游用 localStorage，直接照搬成本更低 | P1 |

`__CROC_RUNTIME_CONFIG__` 那套外置配置**不引入**。

### 3.2 桌面拖拽

HTML5 的 `File` 对象拿不到路径，必须听 Tauri 的 `tauri://file-drop` 事件拿真实路径。
G1 实测它在 `core:default` 里是否已覆盖，不够就补 `core:window` / `core:event` 权限。

## 4. 功能对等表（以 croc-web 为基准）

| croc-web 能力 | 上游实现 | Tauri 版方案 | 优先级 |
|---------------|----------|-------------|--------|
| 选文件 / 拖拽 | `input[type=file]` + drop zone | `.drop-zone` + `tauri-plugin-dialog` + `tauri://file-drop` | P0 |
| 生成并展示三词码 | 前端生成 | `generate_code`（已有）+ `.send-code` 块 | P0 |
| 复制码 / 浏览器链接 | Clipboard API | `tauri-plugin-clipboard-manager` | P0 |
| 二维码 | `qrcode.react` | 同（`crates/croc-cli/src/qr.rs` 已有 `https://getcroc.com/?code=` 的拼法，文案对齐它） | P0 |
| 接收前的清单确认 | `.offer` 卡 | **需先在 croc-engine 支持异步确认**（见下） | P0 |
| 传输进度 / 速率 / ETA | `arrival-time` | 复用：`Progress` 事件 → 前端算 | P0 |
| 取消传输 | 上层 abort | `cancel_transfer`（已有） | P0 |
| 发送内容二选一（文件 / 文本） | `SendContent` | ✅ G6：`SendArgs.text` → `croc_engine::text::write_text_artifact`，与 CLI 的 `--text` 同一个临时文件规则 | P1 |
| 存储模式（加密链接 + 有效期） | `stored-ui.tsx` | ✅ G7：`store_upload` 命令（`croc-store`），前端 `StoredPanel` 给链接 / 二维码 / 到期时间 | P1 |
| 连接状态指示 | `.status-dot` | 中继已连 / 数据面已建两个状态点 | P1 |
| 高级设置 | `.settings-grid` | 网关、中继地址、中继密码、记住密码 | P1 |
| SSH 模式 | `SSHPanel` + xterm.js（懒加载，独立 `croc-ssh.wasm`） | ⏸ G11：依赖本仓库尚未实现的 SSH 数据面。**界面上有显式占位**（第四格工作区 + `SshPlaceholder` 面板），不藏起来 | pending |
| 多任务并发 | 无（浏览器里一次一个） | 我们本来就有（每个传输独立 ID 与 token），优于上游，保留 | — |
| 传输历史 | 无 | 不在 croc-web 清单内，暂缓 | P2 |
| i18n | 无（英文） | ✅ 中英双语，见下 | 已定 |

### 4.1 两个待定点（已定，2026-10-09）

**异步接收确认 —— 加了 `AsyncConfirmSink`。** 原先只有同步的
`ConfirmSink = Arc<dyn Fn(&SenderInfo) -> bool>`。现在 `ReceiveRequest` 多一个
`confirm_async: Option<AsyncConfirmSink>`（`Arc<dyn Fn(&SenderInfo) -> ConfirmFuture>`）：

* `croc-engine` 的取舍规则写在 `receive.rs` 的决策分支上：**`confirm` 优先，其次
  `confirm_async`，都没有才直接接受**。CLI 只给 `confirm`，所以它的 `prompt_confirm`
  一字节没动，golden 测试照旧全绿。
* GUI 走 `confirm_async`：清单 emit 成 `croc://offer`，答案由 `respond_to_offer` 投递
  （`crates/croc-gui/src/offers.rs`，一次等待一根 `oneshot`）。
* 取消要把等待一起放开：`OfferRegistry::abandon` drop 掉 sender，等在它上面的 future
  得到 `Err` → 视为拒绝。不这么做的话，还挂在"等用户按按钮"上的传输收不到取消令牌。

**界面语言 —— 中英双语，首版就有。** 前端自带 `ui/src/locales.ts`（`en` / `zh-CN`，
可能动时才插值），后端另有一套 `crates/croc-gui/locales/*.yml`（rust-i18n，键的分组与
前端对齐），由 `set_locale` 命令同步——用户点右上角的 `EN / 中文` 时两边一起换。
取值顺序沿用 CLI 的判据：`localStorage` → 浏览器语言（`zh*` 归中文）→ 英文兜底。
代价是界面文案要走 key，不能照抄上游的字面串；换来的是 CLI 已有的语种 GUI 也有。

## 5. 桌面端

| 能力 | 方案 |
|------|------|
| 无边框窗口 + 自绘标题栏 | `decorations: false`，前端画标题栏，调 `getCurrentWindow().minimize()/toggleMaximize()/close()`。标题栏要用 §1.2 的规则，不引 custom scrollbar 之类的装饰 |
| 拖拽 | `tauri://file-drop`（见 §3.2） |
| 剪贴板 | `tauri-plugin-clipboard-manager` |
| 系统托盘 | `tauri-plugin-tray`（P2，传输中常驻） |
| 安装包 | Tauri 自带；Windows 需处理 WebView2 Evergreen Bootstrap |
| 更新器 | `tauri-plugin-updater`（需签名，晚于 v1） |

**CI 影响**：`build.yml` 现在刻意避开了 `cargo tauri build`，注释给的理由是"我们没有前端构建，
`ui/` 是静态页"（`build.yml:92`）。换成 Vite 后这条理由不再成立——GUI job 必须先装 Node 并跑前端
构建，否则产物会是一个指向不存在的 `ui/dist` 的二进制。是否改回 `cargo tauri build`、或手工补
`actions/setup-node` + npm build，在 G8 连同 [../build-and-ci.md](../build-and-ci.md) 一起改。

**`.gitignore`**：加 `node_modules/`、`crates/croc-gui/ui/dist/`。

## 6. Android 适配

按 §4 的功能表走同一个 UI；本节只列移动端特有的部分。

### 6.0 工程归属与包名

* **包名 `cn.sumitm.croc_rs`**（`tauri.conf.json` 的 `identifier`）。Android 的 Java 包名
  每段必须以字母开头，所以纯数字的用户名不能直接当包段用。
* **整个 `gen/android/` 入库**。理由：里面有我们自己写的 Kotlin（`TransferForegroundService.kt`
  与改过的 `MainActivity.kt`）。实测 `cargo tauri android init` **不会覆盖已存在的文件**，
  重跑 init 不需要手工合并；因此 `.gitignore` 只忽略 Gradle 产物（`app/build/`、`.gradle/`）。
* `app/tauri.build.gradle.kts` 与 `tauri.properties` **不入库**：它们是 Gradle 首次配置
  时生成的（模板自带的 `app/.gitignore` 已经忽略），内容是 Tauri 的 Android 依赖表。

我们加的两个文件：

| 文件 | 作用 |
|------|------|
| `MainActivity.kt` | Activity 入口：请求通知权限、启停前台服务、SAF 另存的 Activity Result |
| `TransferForegroundService.kt` | 前台服务 + `PARTIAL_WAKE_LOCK` + 通知渠道 |
| `CrocBridge.kt` | Rust 调过来的 `@JvmStatic` 入口（见 §6.5） |

### 6.1 构建

```bash
cargo tauri android init                              # 生成 android/ 工程
cargo tauri android build --apk --target aarch64      # arm64
cargo tauri android build --apk --target x86_64       # x64
```

NDK 需覆盖 `aarch64-linux-android` / `armv7-linux-androideabi` / `x86_64-linux-android`。

### 6.2 存储：SAF / MediaStore（必须写 Kotlin 插件）

```
用户在系统文件选择器里选目录 → ACTION_OPEN_DOCUMENT_TREE
  → Kotlin：takePersistableUriPermission()，用 DocumentFile 建目标文件
  → ParcelFileDescriptor.detachFd() 得到 int fd
  → Rust：File::from_raw_fd(fd)，经 JNI 交给传输层
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

这个抽象目前不存在（`croc-engine` 只按路径读写），是 Android 阶段之前必须补的前置。
**v1 简化且推荐**：接收一律写入应用私有目录（`getExternalFilesDir(Download)`），完成后让用户用
SAF 另存到公共目录。多一次拷贝，但绕开 fd 通路。

### 6.3 前台服务与后台传输

```kotlin
class TransferForegroundService : Service() {
  override fun onCreate() {
    super.onCreate()
    createChannel()
    startForeground(NOTIF_ID, buildNotification())   // 必须 5 s 内调用
    acquireWakeLock(PARTIAL_WAKE_LOCK)
  }
}
```

已实现，见 `gen/android/app/src/main/java/cn/sumitm/croc_rs/TransferForegroundService.kt`
（通知渠道 + `PARTIAL_WAKE_LOCK`，超时 10 min 自动释放）。
`MainActivity.onCreate` 启它、`onDestroy` 停它，Android 13+ 会先请求 `POST_NOTIFICATIONS`。

通知里的进度目前只有静态文案：**Rust 侧要把实时进度送到 Kotlin，得写 Tauri 插件或 JNI 桥**，
这条路见 §6.5。

清单需 `FOREGROUND_SERVICE`、`FOREGROUND_SERVICE_DATA_SYNC`（Android 14+ 要声明类型）、
`WAKE_LOCK`、`INTERNET`、`ACCESS_NETWORK_STATE`。**不申请**
`REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`，也不去引导用户关电池优化：前台服务已经把应用抬到
"用户可见正在运行"的那一档，多一次系统弹窗换不到与之匹配的传输可靠性。

### 6.4 其它移动端事项

| 项 | 处理 |
|----|------|
| WebView 版本 | `minSdk 24`；低端机首屏慢，加 splash。CSS 用保守子集（§1.2 的规则本身就是保守的） |
| 触摸目标 | 按钮提到 48px（上游移动端断点同此值） |
| 返回键 | 监听 `tauri://back-button` 或前端 history |
| 权限 | 仅 `INTERNET` + 通知；**不要**申请 `READ_EXTERNAL_STORAGE` |
| 大文件 OOM | 前端绝不持有文件字节，只用路径 / URI |
| 断点 | 进程被杀后状态丢失，启动时扫描未完成的 `.croc-partial` 提示续传 |

### 6.5 Rust → Kotlin 的桥（G10，已实现）

`crates/croc-gui/src/android.rs`（仅 Android 编译）+ `gen/android/.../CrocBridge.kt`：

| Rust | Kotlin | 作用 |
|------|--------|------|
| `start_service` / `stop_service` | `CrocBridge.startService` / `stopService` | 前台服务按需启停（队列空了才停） |
| `notify_progress` | `CrocBridge.notifyProgress` | 把 Rust 侧的状态句写进通知 |
| `export_file` | `CrocBridge.exportFile` | SAF 另存（`ACTION_CREATE_DOCUMENT`，结果在 `MainActivity` 里拷贝） |
| `share_file` | `CrocBridge.shareFile` | 系统分享（`FileProvider` + `ACTION_SEND`） |

两边靠**类名 + 方法名 + JNI 描述符**对上，改签名只会以 `NoSuchMethodError` 在运行时暴露，
所以改一边必须改另一边。桌面端这几个 Rust 函数是空的（no-op），调用方不写 `cfg` 分叉。

返回键由前端处理：`tauri://back-button` → 回到发送页，而不是直接退应用。

**还没做 / 未验证**：

| 项 | 状态 |
|----|------|
| 真机验证 | ❌ 只做到 `cargo check --target aarch64-linux-android` 通过，没在设备上跑过 |
| SAF 直接收文件（不落私有目录） | ❌ 仍要 §6.2 的 `FileSink` / `FileSource` 抽象；现在是"先落私有目录、再另存" |
| 通知里的字节级进度 | ⚠️ 现在推的是 Rust 侧的状态句，不是百分比 |

## 7. 里程碑

| # | 任务 | 依赖 | 验收 |
|---|------|------|------|
| G0 | Vite + React + TS 工程落地；注入 §1.1 令牌与 §1.2 规则，`crocw-rs` 能起来 | — | 窗口出得来，深/浅色切换生效，视觉与 getcroc.com 对齐到肉眼无差 |
| G1 | **修 §0.4 的中继池缺陷** + 文件选择器 / 拖拽 + 发送端到端 | — | GUI 默认发、CLI 默认收能通 |
| G2 | 异步接收确认 + `.offer` 清单卡 | 见 §4.1 | 含覆盖 / 重命名 / 续传三种决策 |
| G3 | 设置持久化 + 主题切换 + 连接状态点 | G1 | 重开后设置生效 |
| G4 | 进度升级：总量 + 单文件 + 速率 + ETA | G1 | 与 CLI 的百分比一致 |
| G5 | 口令码块的复制 / 二维码 / 浏览器链接 | G1 | 扫码能在手机浏览器打开 getcroc 收件页 |
| G6 | 文本直传 | G2 | 与 `croc send --text` 互通 |
| G7 | 存储模式（链接 + 有效期） | G1 | 到期自动失效 |
| G8 | CI：Node + 前端构建；同步 [../build-and-ci.md](../build-and-ci.md) | G0 | matrix 四平台产物齐全 |
| G9 | Android APK 出包并能发送 | G1, G8 | 真机可发 100 MB |
| G10 | Android 存储 / 前台服务 / 系统分享 | G9 | 后台 10 min 不被杀 |
| G11 | SSH 模式 | SSH 数据面 | pending，待前置解锁 |

## 8. 风险

| 风险 | 影响 | 对策 |
|------|------|------|
| 中继池缺陷没修就开始堆 UI | 高，默认场景直接不通 | 排在 G1 第一位，先于任何界面工作 |
| `beforeBuildCommand` 工作目录踩坑 | 中，CI 静默产出坏包 | G0 实测；CI 里加"产物 ls ui/dist 非空"的断言 |
| 上游 `styles.css` 4487 行整体照搬会带入营销区样式 | 中 | 按 §1.4 白名单挑类名，不做全文拷贝 |
| 异步确认改法与 CLI 的 golden 冲突 | 中 | 先做 G2 的方案评审，CLI 测试必须全绿 |
| Android WebView 版本差异 | 中 | `minSdk 24`；禁用前沿 CSS |
| Android 后台被杀 | 高 | 前台服务 + WakeLock；按另有 IO standards 详见 §6.5 |
| SAF 复杂度超预期 | 高 | v1 走"私有目录 + 完成后另存" |
| Windows 缺 WebView2 | 中 | 安装包内嵌 Evergreen Bootstrap |
| Tauri 2 API 变动 | 中 | 锁 minor，升级前冒烟 |
