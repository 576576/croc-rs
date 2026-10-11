# GUI（Tauri 2，产物 `crocw-rs`）

> 后端复用 `croc-core` + `croc-engine`，与 CLI 同源。目标平台 Windows / macOS / Linux / **Android**。
> **界面基准是 getcroc.com**（上游的浏览器客户端 `croc/web`，基线 `v11.5.4-5-g8dd74239`）。
> 要看 UI 就读 `/d/Github/croc/web/src` 的源码，不要爬站。

G0–G10 已落地。**剩 G11（SSH 共享终端）**：数据面未做，界面上有显式占位。

## 1. 工程布局

```
crates/croc-gui/
  ui/                 Vite + React + TS（pnpm 12，锁文件 pnpm-lock.yaml）
    src/styles.css    从上游 styles.css 按类名裁剪 + 自补（工具栏 / 队列 / 移动端断点）
    src/locales/      en.ts · zh-CN.ts · index.ts（键的基准是 en.ts 的 `MessageKey`）
    src/components/   WorkspaceSwitch（三个大页）· ToolbarActions（右上四枚按钮）
                      Tour（指引）· SendPanel · ReceivePanel · SshPanel · HistoryPanel
                      FileList · SendCode · OfferCard · ProgressBlock
                      TransfersList · SettingsGrid · StatusDot
    dist/             构建产物，不入库
  src/                commands.rs · offers.rs · store.rs · state.rs · android.rs
  locales/*.yml       rust-i18n，与前端 locales/*.ts 的键对齐，由 set_locale 同步切换
  gen/android/        Android 宿主工程，入库（含自写 Kotlin）
```

- **三个大页**：传输 / SSH / 历史。标题栏是原生的（`tauri.conf.json` 不开 `decorations:
  false`），所以前端不再自绘标题栏与窗口按钮。
- **窄屏（≤720px）发送与接收二选一**：切换条照 croc web 的 `mobile-transfer-switch`
  （吸顶、选中态前景背景互换），没选中的那块整块收掉，不再上下堆叠；≤600px 时三个大页
  只留当前那一页的文字，其余收成图标（收起来的靠 `aria-label` 留名字）。
- **传输队列在历史页**：发起发送或接收后直接跳过去。有发送在跑时那一页还带本次口令码——
  跳走了看不到码，这趟就白发了。高级设置留在传输页，是栅格下面单独一块（`.settings-panel`）。
- 能展开的部分一律走 `.disclosure`（二维码、高级设置）：`summary` 自带的三角被
  `list-style: none` 去掉了，箭头得自己画，展开时转 180°。
- **口令码那格三页同构**：紧跟 `panel-heading`，`field-label` 在上、`.field-row.code-field`
  在下（输入框 + 后置一枚按钮）。SSH 页那枚是眼睛（显示/隐藏邀请码），发送与接收页是
  复制/粘贴——一个位置只放得下一枚，所以靠长按（触屏）或 `onContextMenu`（桌面、键盘）
  切换，长按结束后要点掉浏览器补的那次 click，否则切换的同一瞬间又执行一次动作。
  接收页默认是粘贴（码是别人给的），发送页默认是复制。
- **焦点框只跟输入框**：`.field-row:has(input:focus)` 画整组的环，不用 `:focus-within`——
  后者在点后置按钮时也会亮，而那会儿用户并没有在编辑那一格，整行闪一下像点错了。
  后置按钮保留全局的 `:focus-visible`（键盘走到才描边，鼠标点没有任何变化）。
  不认识 `:has()` 的引擎由 `@supports not selector(:has(*))` 退回把环画在输入框自己身上。
- **缩放一律挡掉**：版面按固定版式排的，放大之后按钮会跑出屏幕。触摸那条是
  `html { touch-action: pan-x pan-y }`（只留滚动与点按）+ `index.html` 的
  `user-scalable=no`；桌面那条是 `main.tsx` 里拦带 `ctrlKey` 的 `wheel`（触控板双指滑动
  Chromium 会合成成它）与 Safari 的 `gesture*` 事件。键盘的 Ctrl+加减号拦不到，由 WebView
  自己处理。
- **发送前重置口令码**（默认勾选，随设置存）：上次发的码会回填进那一格，再点发送会把
  它当自定义码交上去，撞回同一个还在跑的房间。勾选时每次发送都用新码；要用自己填的码
  就把勾去掉。勾选框画在发送页（它管的是那格），`Settings.resetCode` 只为跟着存。
- 右上角四枚图标按钮（照 croc web 的 `workspace-toolbar-actions`）：指引 / 语言 /
  仓库 / 深浅色。语言那枚是展开选择（自动 · English · 中文，默认自动），不是点击切换。
- 暂存（`store`）不是独立页，是发送面板里的一种投递方式（直传 / 暂存）。期限与次数那
  两处照 croc web：期限是"数值 + 单位下拉"两件一组（`stored-expiration-control`），
  次数单独一行；拼出来的是后端 `parse_expiration` 认的 `90m` / `12h` / `3d` / `2w`。
- **焦点框画在整组上**：数值格 + 单位下拉、邀请码 + 眼睛按钮，这两处只给输入框画
  outline 会被右边那件压掉一半（croc web 就是这样，SSH 那处不跟它）。做法是
  `:focus-within` 画在容器上，子元素的 `:focus-visible` outline 归零。
- **`clipboard-manager:default` 是空权限集**（插件自己的 `permissions/default.toml` 里
  `permissions = []`，注释写明读写都要显式开）。所以 `capabilities/default.json` 里必须
  逐条列 `allow-write-text` / `allow-read-text`，只写 `clipboard-manager:default` 等于
  复制按钮静默失效——失败被 `.catch` 吞掉，界面上看不出。

- `tauri.conf.json` 的 `frontendDist` 指向 `ui/dist`。`beforeBuildCommand` 是
  `pnpm run build`、**不带 `--prefix ui`**：Tauri 2 把 hook 的 cwd 设成 frontend 目录
  （即 `crates/croc-gui/ui`，见 tauri-cli 的 `helpers::run_hook`，`cwd` 未指定时取
  `dirs.frontend`），不是 crate 根。带上 `--prefix ui` 会从 `ui/` 里再找 `ui/`，
  报 `canonicalizing the --dir argument: ui`。`beforeDevCommand` 同理。
- **编 `crocw-rs` 之前必须先编前端**：`cargo build` 不会替你跑 `beforeBuildCommand`，少了它
  产物能启动但界面全白。CI 的 GUI job 里有 `ui/dist` 非空断言。
- **生产构建必须带 `--features custom-protocol`**：`tauri build`（CLI）会自动加它，
  `cargo build` 不会。少了它 `tauri` 的 `dev` cfg 为真，运行时连的是 `devUrl`
  （http://localhost:1420）而不是编译期嵌入的前端，界面报"localhost 拒绝连接"。
  开发时反过来：`cargo run -p croc-gui` 不带这个 feature，走 dev server。
- 事件名三个：`croc://progress` / `croc://finished` / `croc://offer`。
- ETA 在前端 `src/format.ts::project()` 里算（最近 3 秒的观测点线性外推），**没有引
  `arrival-time`**：速率抖动比拟合精度大一个量级。这是与上游有意为之的一处偏差。
  窗口按时间取而不是按点数取：进度 100 ms 上报一次，固定取 8 个点会把窗口压到
  1 秒以内，速率跟着抖。
- `croc://progress` 的 `transferred` 是整次传输的累计值，但在文件内也连续更新
  （文件内进度 + 已收工文件的字节数），所以大文件上进度条是连续走的，不是每收完
  一个文件跳一格。引擎每收完一个文件还会报一次累计值，用来校准。

## 2. UI 基准的设计契约（色值不得改）

### 2.1 令牌

| 令牌           | 深色      | 浅色      |
| ------------ | ------- | ------- |
| `--canvas`   | `#121411` | `#f2f1ec` |
| `--background` | `#050605` | `#ffffff` |
| `--foreground` | `#ffffff` | `#000000` |
| `--muted`    | `#a4a4a4` | `#555555` |
| `--subtle`   | `#909090` | `#686868` |
| `--soft-border` | `#4b4b4b` | `#aaaaaa` |
| `--panel-tint` | `#0d0f0d` | `#f7f6f2` |
| `--danger`   | `#ffffff` | `#000000` |

字体 `"JetBrains Mono Variable", "JetBrains Mono", ui-monospace, monospace`，
`font-synthesis: none`。**通体等宽，没有第二套字体，没有一个彩色。**

### 2.2 版式

圆角一律 `0`；内容宽度 `min(1080px, calc(100% - 32px))`；最小交互高度 42px（移动端 48px）；
按钮 12px / 字重 750；主描边 `1px solid var(--foreground)`，次要内框用 `--soft-border`；
焦点 `outline: 2px solid var(--foreground); outline-offset: 2px`；
**状态不靠颜色，靠前景背景互换**（选中即 `color: var(--background)` + 反色底）；
字段标签 `.field-label` 10px / 750 / 全大写。

主题：`<html data-theme="dark|light">`，同时写 `style.colorScheme` 与
`<meta name="theme-color">`（`#121411` / `#f2f1ec`）。取值顺序 localStorage →
`prefers-color-scheme` → **兜底 dark**。

### 2.3 组件

| 组件      | 类名                  | 要点                                    |
| ------- | ------------------- | ------------------------------------- |
| 工作区切换   | `.workspace-switch` | `inline-grid`（三页 3 格，投递方式 2 格），选中格反色       |
| 站点头部    | `.site-head`        | 标题 + 进度那一格；`flex`，标题 `flex: none`，进度 `flex: 1` |
| 进度那一格   | `.progress-strip`   | 文件名一行 + `.progress-track` + 一句"百分比 / 字节 / 速率 / 剩余"；没在跑就不渲染 |
| 面板头     | `.panel-heading`    | `auto minmax(0,1fr) auto`：序号格 + 标题 + 右侧动作位；**无副标题**，与下方内容间距 18px |
| 丢弃区     | `.drop-zone`        | `min-height: 112px`，`1px dashed var(--subtle)` |
| 选中清单    | `.selection-summary` | 一行"几个 / 合计多大"，在丢弃区与清单之间              |
| 输入行     | `.field-row`        | 输入框 + 后置一枚按钮同行；`.code-field` 是"按钮只有图标"那一变体 |
| 分享链接    | `.share-link-url`   | 二维码下方的链接，只展示（`user-select: none`）；按钮才是拿走它的入口 |
| 二维码     | `.share-qr`         | `justify-items: center` 居中；`Disclosure` **默认展开** |
| 清单确认卡   | `.offer`            | `1px solid var(--foreground)`，背景 `--panel-tint`；作为 `.offer-strip` 横幅挂在工具栏下 |
| 进度条     | `.progress-track`   | `height: 7px`；只作视觉指示，数字写在旁边那一行          |
| 文件清单    | `.file-list`        | `max-height: 154px`，可滚；目录模式少一列大小       |
| 文本框     | `.text-composer`    | 标题 + `min-height: 132px` 的 `textarea` + 一行字节数 |
| 连接状态点   | `.status-dot`       | 7×7 描边，连上后填前景色                         |
| 传输双栏    | `.transfer-grid`    | `1fr 1fr`                             |
| 高级设置    | `.settings-grid`    | 三列：网关、中继地址、中继密码、记住密码                   |
| 图标      | `lucide-react`      | 线性描边                                  |

上游的营销区（`Blog*` / `.donation-banner` / `.cli-download` / `.home-reviews`）不搬。

**没有传输队列。** 原先历史页上那块"传输列表"（`TransfersList`）已整个删掉，随之删掉
`ProgressBlock`。三个去向：

* 进度与取消 → 站点头部右侧的 `.progress-strip`（只显示在跑的那一条）；
* 清单确认（收不收）→ 工具栏下面的 `.offer-strip` 横幅，因为它挡着传输，要显眼；
* 收工之后的另存 / 分享 → 挂在历史页对应那条记录上（`HistoryPanel` 的 `isAndroid` 分支）。

由此带来的两条：发起发送 / 接收**不再跳页**（进度就在头顶，留在原页还能看码与二维码）；
传输收工时把该条从 `transfers` 里摘掉，失败则把错误提到共用的 `.error-strip`——
不然"失败"只报在一条随即消失的进度条上，等于没报。

**口令码栏三页同构**：SSH 页的邀请码、发送页的自定义口令码、接收页的口令码是同一形
（`.field-row.code-field` = 输入框 + 后置一枚按钮），位置都是紧跟 `.panel-heading`。
SSH 页那枚是眼睛，传输页那枚在复制 / 粘贴之间切换（长按或右键/菜单键）。接收页的
「保存到」也用 `.field-row`（后置按钮带文字），只是没有 `.code-field` 那套方按钮尺寸。

**二维码下面那一行**：链接本体（`getcroc.com/?code=…`，去掉协议头）只展示、不可选中，
旁边一枚「分享链接」按钮才是拿走它的入口——两样都长成可点的样子会分不清。动作按平台
分叉：**桌面复制到剪贴板，Android 起系统分享面板**（`share_text` 命令 → `CrocBridge.shareText`
→ `ACTION_SEND` + `text/plain`；桌面上没有分享面板，复制才是能接着用的那个动作）。

### 2.4 i18n

中英双语，首版就有。前端 `locales/`（一门语言一个 `.ts`，键由 `en.ts` 的 `MessageKey` 约束），
后端 `locales/*.yml`，`set_locale` 两边一起换。
取值顺序沿用 CLI 的判据：localStorage → 浏览器语言（`zh*` 归中文）→ 英文兜底。
代价是文案要走 key，不能照抄上游的字面串。

## 3. 与 CLI 同源的两条硬约束（已修，不可回退）

**文本载荷的三个名字。** `--text` 的内容在线上是一个叫 `croc-stdin-<纳秒时间戳>` 的临时
文件，前缀是**协议要求**的：上游 `validateSendingTextOffer` 明文校验
`file.FolderRemote == "." && strings.HasPrefix(file.Name, "croc-stdin-")`，改了前缀 Go 那边
直接拒收。所以这个名字一个字节也不能动，但**三个面向用户的地方必须换掉**，规则集中在
`croc_core::policy`：

| 场景           | 用什么                            | 结果                       |
| ------------ | ------------------------------ | ------------------------ |
| 发送侧进度与提示     | `policy::display_name(name, …)` | `text`（文本）/ `stdin`（管道） |
| 接收侧确认卡       | 同上（`OfferEvent::from_manifest`） | 同上                       |
| 接收侧落盘名       | `policy::text_destination_name()` | `croc-text-<纳秒>.txt`     |

落盘名的改写点是 `croc_engine::receive` 拿到清单、用户点了"接收"之后（`rename_text_payload`），
与上游 `croc.go:2518` 同一步。改完的清单要放回 `manifest`，否则文件循环与 `FileClosed`
还在用旧名字。判据必须和 `policy::is_text_artifact` 一致（既要 `sending_text` 又要前缀）——
只按前缀改会把用户一个真叫 `croc-stdin-*` 的文件也改掉。

**中继路由。** `pick_relay()`（`commands.rs`）：用户没给中继就按
`codephrase::relay_index(码, 池长)` 折到 `croc_engine::public_relay::RELAYS`，给了就以用户为准，
与 CLI 的 `relay_pool::index_of` 同一判据。此前写死 `croc.schollz.com:9009` 会绕过公共中继池——
**GUI 默认发出的码，CLI 默认去另一端接**，双方停在各自的空房间里，症状是接收端一直卡在认证
而不是报错。

**异步接收确认。** `ReceiveRequest` 有 `confirm`（同步 `Fn(&SenderInfo) -> bool`）与
`confirm_async`（`Arc<dyn Fn(&SenderInfo) -> ConfirmFuture>`）两个槽，取舍规则是
**`confirm` 优先，其次 `confirm_async`，都没有才直接接受**。CLI 只给 `confirm`，所以
`prompt_confirm` 一字节没动，终端 golden 测试照旧全绿。GUI 走 `confirm_async`：清单 emit 成
`croc://offer`，答案由 `respond_to_offer` 投递（`offers.rs`，一次等待一根 `oneshot`）。
取消必须走 `abandon` drop 掉 sender，否则挂在"等用户按按钮"上的传输收不到取消令牌。

## 4. Android

- **包名 `cn.sumitm.crocw`**（`tauri.conf.json` 的 `identifier`），应用名 `Crocw`（`strings.xml`
  的 `app_name` / `main_activity_title`）。Java 包名每段必须以字母
  开头，纯数字的用户名不能直接当包段。
- **整个 `gen/android/` 入库**（里面有自写 Kotlin）。`cargo tauri android init` 对已存在的
  文件是幂等的，重跑不需要手工合并；`.gitignore` 只忽略 Gradle 产物。
  `app/tauri.build.gradle.kts` 与 `tauri.properties` 是 Gradle 首次配置时生成的，不入库。
- 改过的两个文件：`MainActivity.kt`（入口：请求 `POST_NOTIFICATIONS`、启停前台服务）、
  `TransferForegroundService.kt`（前台服务 + `PARTIAL_WAKE_LOCK`，10 min 超时自动放）。
- **Rust→Kotlin 桥**：`CrocBridge.kt`（`@JvmStatic`）+ `src/android.rs`（jni 0.21 +
  ndk-context，仅 Android 编译）。服务按需启停、通知进度、落盘目录（`sandboxDir`）、
  发布到下载目录（`publishToDownloads`）、SAF 另存、系统分享（`shareFile` 走文件 +
  FileProvider，`shareText` 走 `ACTION_SEND` + `text/plain`）、SAF 目录暂存
  （`stageDirectory` / `removeStaged`）。
  桌面端这几个 Rust 函数是 no-op，调用方不写 `cfg` 分叉。返回键由前端接 `tauri://back-button`。
- **JNI 查类必须走 ClassLoader**：`CrocBridge` 不能用 `env.find_class`——`FindClass`
  只认当前这条 Java 调用栈上的 classloader，而命令跑在 Tauri 的 async 线程上（从 native
  附着上去的，栈上没有 Java 帧），它会退到系统 classloader 并抛 `ClassNotFoundException`。
  可靠的一条是先取 ApplicationContext（`ndk_context::android_context().context()`，tao
  在 `android_setup` 里 `new_global_ref` 出来的那个）的 `getClassLoader()`，再 `loadClass`。
- **`CrocBridge` 与 `SafPlugin` 必须写 keep 规则**（`gen/android/app/proguard-rules.pro`）：
  两者都没有 Java 侧调用点，前者靠 Rust 侧按名字 + JNI 描述符反查、后者靠 Tauri 的
  `PluginManager` 反射加载，release 的 R8 全程序优化会把它们当死代码重命名或删掉。
  新增任何靠反射 / JNI 查的 Kotlin 类都要同步加 keep。
- **接收落盘**：先写应用私有目录（`sandboxDir`，Android 上工作目录不可写），收完按
  `EngineEvent::FileDone` 记下的名字逐个发布出去——用户挑过 SAF 目录就发到那个目录
  （`safPublish`），没挑过则发到公共下载目录（`publishToDownloads`，往 `MediaStore.Downloads`
  插条目**不需要任何运行时权限**，只有改动别人创建的文件才要 `MANAGE_EXTERNAL_STORAGE` 或 SAF）。
  代价是一次拷贝（无法直接写到公共目录）；免掉拷贝需要 `croc-engine` 支持 fd 写入，见 §5。
- **挑 SAF 目录**：`crates/croc-saf-plugin`（独立 crate，`init()` 注册为 `saf`）。
  官方 `tauri-plugin-dialog` 在 Android 上走 `Intent.ACTION_GET_CONTENT`，那个 intent
  只出文件、**选不了目录**，授权也不可持久化，所以自己起 `ACTION_OPEN_DOCUMENT_TREE`
  并 `takePersistableUriPermission`。前端调 `plugin:saf|pick_directory`，桌面端恒返回
  `null`（没有 SAF），调用方据此退回系统文件选择器。
  界面上「保存到」那格在 Android 存的是 SAF 目录 URI，**不能当文件系统路径用**；
  真实落盘位置另由 `sandbox_dir` 命令给出（另存与分享要拿它拼路径）。
- **发送目录**：引擎靠 `std::fs` 走目录树（`files::survey` 起手 `symlink_metadata`），
  只认真实路径，所以 SAF 目录要先由 `CrocBridge.stageDirectory` 整个拷到
  `cacheDir/crocw-send/<时间戳>`，`send` 里换成真实路径，传输结束（成败都）
  `removeStaged` 删掉。注意 `chooseFiles(directory=true)` 的**两个调用方要求相反**：
  接收侧要 URI，发送侧要真实路径。
- 清单要 `FOREGROUND_SERVICE`、`FOREGROUND_SERVICE_DATA_SYNC`（Android 14+ 需声明类型）、
  `WAKE_LOCK`、`INTERNET`、`ACCESS_NETWORK_STATE`。**不申请**
  `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`，也不引导用户关电池优化。
- 交叉编译要自己指工具链：`CC_aarch64_linux_android=<NDK>/toolchains/llvm/prebuilt/
  windows-x86_64/bin/aarch64-linux-android30-clang.cmd`、`AR_...=llvm-ar.exe`、
  `CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=` 同上 clang。API 级别取 30（= `minSdk`）。
  Android 实际只构建 lib，验证用 `cargo check --lib --target aarch64-linux-android -p croc-gui`。
  **插件的 `cfg(mobile)` 代码只在这个目标下才被编译**，host 上 pass 不代表 Android 上能编。

移动端其它：触摸目标 48px；前端绝不持有文件字节，只用路径 / URI。
SDK：`minSdk 30` / `targetSdk 36` / `compileSdk 37`（`app/build.gradle.kts`）。
minSdk 30 已经高于 29，所以存储只有 `MediaStore` 一条路，没有 `WRITE_EXTERNAL_STORAGE`
那条 legacy 分支，也不需要申请它。

## 5. 剩余待办

| 项                   | 阻塞点                                                                        |
| ------------------- | -------------------------------------------------------------------------- |
| G11 SSH 共享终端        | 依赖 SSH 数据面（上游 `src/sshshare` + `internal/sshclient`），未实现；SSH 页只有一句去处 |
| 直写公共目录 / SAF 目录（免掉一次拷贝） | 需要 `croc-engine` 的 fs 层抽象出 `FileSink` / `FileSource`（路径 或 raw fd）。现在是"先落私有目录、收完发布出去"，发送目录同理要"先拷出来再发" |
| Android 真机验证        | 只做到交叉编译 + Kotlin 编译通过。`ACTION_OPEN_DOCUMENT_TREE` 的选择流程、`takePersistableUriPermission`、`safPublish` / `stageDirectory` 的读写都只在真机可见；JNI 桥靠类名 + 方法名 + 描述符对上，改签名只会在运行时以 `NoSuchMethodError` 暴露         |
| 通知里的字节级百分比          | 现在推的是 Rust 侧的状态句                                                            |
| 系统托盘 / 更新器 / 传输历史   | P2                                                                          |

## 6. 风险

| 风险                        | 对策                             |
| ------------------------- | ------------------------------ |
| Android 后台被杀              | 前台服务 + WakeLock（已实现，未经真机验证）    |
| 公共目录写入的拷贝开销           | 保持"私有目录 + 收完发布"，不提前引入 fd 通路     |
| 上游 `styles.css` 整体照搬带入营销区 | 按 §2.3 白名单挑类名，不做全文拷贝           |
| Windows 缺 WebView2        | 安装包内嵌 Evergreen Bootstrap      |
| Tauri 2 API 变动            | 锁 minor，升级前冒烟                  |
