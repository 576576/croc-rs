# 00 · 项目总览

## 1. 我们要做什么

两件互相依赖的事：

1. **croc-rs（CLI）**：用 Rust 从零实现一个与 Go 版 croc v11.5.4 **线路级兼容**的文件传输工具。
   - 能与官方 `croc` 客户端互相收发文件；
   - 能连官方中继 `croc.schollz.com:9009`；
   - 能自建中继（Rust 实现）；
   - CLI 参数与 Go 版对齐，用户可以无痛替换。

2. **crocw-rs（Tauri 2 GUI）**：在同一套 Rust 核心之上做一个跨平台 GUI，**取代 FlCroc**（当前为 Flutter + Go CGO FFI）。
   - 产物名 **`crocw-rs`**（w = window），与 CLI 产物 **`croc-rs`** 并列为 CI 的两个发布产物；
   - 平台：Windows / macOS / Linux / **Android**（iOS 作为 bonus）；
   - 功能对等于 FlCroc：发送、接收、传输历史、设置、i18n、二维码、拖拽、系统分享接入。

## 2. 为什么不用现在的方案

| 现状 | 问题 |
|------|------|
| FlCroc = Flutter UI + Go `go_bridge`（CGO 动态库）+ croc submodule | 三条工具链（Dart / Go / CGO），任何一端升级都要全链路回归 |
| CGO 交叉编译 Android（`build.sh` 编 `.so`） | 每个 ABI 一套产物，构建脚本脆弱，本地开发几乎无法调试 |
| Go 版 croc 单文件近 4000 行（`src/croc/croc.go`） | 状态机、UI、协议、文件系统耦合，改一处要读全篇 |
| 进度上报靠 `CrocPollProgress` 轮询全局单例 | 不支持并发传输，取消/暂停语义弱 |

Rust 化之后：**一套核心（`croc-core`）+ 两个前端（CLI / Tauri）**，Android 由 Tauri 的 Gradle 流程直接把 Rust 编进 `.so`，不再手工维护 CGO 桥。

## 3. 目标 / 非目标

### 目标（In scope）

- [x] 与 croc v11.5.4 客户端**双向互通**（Rust↔Go 收发，两个方向都测）
- [x] 支持 croc 默认中继握手协议（含 `siec` 曲线 PAKE）
- [x] 自建 Rust relay，行为对齐 `src/tcp/tcp.go`
- [x] 核心传输能力：多文件 / 文件夹 / 文本、本地直连发现、中继中转、断点续传、覆盖与重命名策略、压缩、限速、代理（SOCKS5 / HTTP CONNECT）
- [x] 三种数据面统一抽象（Relay / Local / Tailscale）；前两种 v1 实现，**Tailscale 实现标记 ⏸ PENDING**（等待上游 tailcat-rs），但抽象与降级路径 v1 就位，见 [08-tailscale.md](08-tailscale.md)
- [x] CLI 覆盖 Go 版主要 flag（见 [04-cli-plan.md](04-cli-plan.md) 对齐表）
- [x] Tauri 2 GUI 覆盖 FlCroc 全部用户可见功能
- [x] Android APK（arm64 / x64），支持前台服务保活 + SAF 存储

### 非目标（Out of scope，v1 不做）

- ❌ `croc ssh`（共享终端）——依赖 PTY 与 tailcat 的完整能力，放到 v2 评估
- ❌ `croc store`（加密暂存、下载次数限制）——依赖 `storeclient` / `storecrypto` 服务端，v2
- ❌ 复刻 Go 版 tailcat 的 **CBOR ConnBlob wire**（匿名临时节点 + 自签名 DERP 引导 + 一次性客户端密钥授权）——见 [08-tailscale.md](08-tailscale.md)，v1 用官方 `tailscale` crate 走另一条路
- ❌ Web 版（`getcroc.com` 那种 WASM 接收端）——v2
- ❌ 自研新协议 / 新加密套件——本项目的立身之本是"兼容"，任何协议改动都是 v2 之后的独立提案

### 明确的取舍

> **Tailscale 数据面在一开始就纳入架构设计，但不阻塞 v1 交付。** 数据面抽象成 `DataPlane` trait，v1 落地 `Relay` 与 `Local` 两个实现，`Tailscale` 作为第三个实现按平台 feature 开关接入。与 Go 版互通时若对端要求 tailcat，自动降级到 relay（协议层已有安全降级机制，见 [01-protocol.md](01-protocol.md) §5.2）。详见 [08-tailscale.md](08-tailscale.md)。

## 4. 设计原则

1. **优雅优先于逐字一致 —— 但只限于实现层。**
   严格的边界划分：
   - **对外（线路字节）**：是契约，必须逐字节一致，没有商量余地。房间名、nonce 布局、大整数编码、transcript 哈希——错一个字节就是"密码错误"。
   - **对内（代码结构）**：不追求"把 Go 逐行翻译过来"。Go 版的 `croc.go` 是一个 4000 行、几十个 `sync.Mutex` 的巨型状态机，那是 Go 的写法，不是我们要的。Rust 版用**类型状态 + sans-io 纯状态机 + 副作用解释器**重写，让非法状态在编译期就不可能构造出来。
   - 因此**不再要求** `// COMPAT:` 式的翻译腔注释堆砌。凡是"为了兼容而不得不写丑"的地方，应当用**类型与新封装**把它隔离到一个小的纯函数里（例如 `EuclidMod` 新类型），而不是让丑陋扩散到调用方。兼容性知识沉淀在 `01-protocol.md` / `02-crypto.md` 与 golden 测试里，代码保持干净。
2. **纯核心，副作用在边缘（functional core, imperative shell）**。协议状态机不接触 socket、文件、时钟；它接收事件、返回"下一步要做什么"的描述（effect），由外层解释器执行。协议层因此可以 100% 单测、可确定性重放。
3. **核心无 UI 依赖**。`croc-core` 不引用任何 Tauri / 终端 / 进度条代码，只暴露 trait（事件、确认、日志）。CLI 与 GUI 各自实现。
4. **可测试性优先**。所有涉及线路字节的模块，都要能在没有网络的情况下用 golden 向量测试（Go 版生成的固定输入/输出）。
5. **取消是一等公民**。所有长操作接 `CancellationToken`，Go 版 `c.stop.ctx` 的语义在这里对齐。
6. **错误可分诊**。错误分三类：可重试（网络断开）、终态（密码错误、对端拒绝）、内部 bug。GUI 需要据此决定"重试"还是"报错"。
7. **不要一开始就做 GUI**。核心 + CLI 通过互通测试后再开 GUI，否则会在协议未定型的泥潭里同时改两端。

## 5. 仓库结构

```
croc-rs/
├── Cargo.toml                 # workspace
├── crates/
│   ├── croc-siec/             # 纯数学：SIEC255 等曲线（no_std 友好）
│   ├── croc-pake/             # SPAKE2 变体：sans-io 状态机 + wire codec
│   ├── croc-protocol/         # ★ 协议状态机（sans-io，零 IO，可确定性重放）
│   ├── croc-engine/           # 副作用解释器：tokio IO / 文件 / 计时器 / 数据面
│   │                          # （croc-tailscale 暂不建目录：⏸ pending，见 08）
│   ├── croc-core/             # 门面：typestate Client API，组装 protocol + engine
│   ├── croc-relay/            # 中继服务器
│   ├── croc-cli/              # 可执行程序 `croc`
│   └── croc-gui/              # Tauri 2 应用（Rust 侧）
├── app/                       # GUI 前端（Vite + React + TS）
├── docs/
│   └── agent/plan/            # 本目录
└── tests/
    ├── interop/               # 与 Go croc 的端到端互通测试（脚本 + 固件）
    └── golden/                # Go 版生成的协议字节向量
```

分层的理由（也是"优雅优先"的落点）：

- **`croc-protocol` 不依赖 tokio、不依赖文件系统、不依赖时钟**。它只有 `fn handle(event) -> Vec<Effect>`。这意味着：协议可以在单测里用喂字节的方式完整重放一次传输，不需要 mock 任何 IO，也不需要 sleep。这是 Go 版做不到的。
- **`croc-engine` 只做"把 Effect 翻译成真实世界动作"**，本身不含协议判断。
- `croc-siec` / `croc-pake` 独立成 crate：它们是互通成败的关键，必须能脱离网络做纯单元测试，也值得单独发版供他人复用。

详见 [03-architecture.md](03-architecture.md)。

## 6. 与上游的关系

| 项目 | 关系 | 处理方式 |
|------|------|---------|
| `schollz/croc` v11.5.4 | 协议与行为基线 | **不作为 submodule**，但 `tests/golden/` 保留由 Go 版生成的向量；CI 里可选地拉取 Go 版做互通测试 |
| `FlCroc` | 被取代对象 | GUI 需求以 FlCroc 的功能清单为基准（见 [05-gui-plan.md](05-gui-plan.md)），代码不复用；过渡期两者可并存 |

## 7. 成功的定义（DoD）

项目算"做成"需要同时满足：

1. `croc-rs send foo.txt` 生成的三词码，能被官方 `croc v11.5.4` 客户端接收；反之亦然。
2. 1 GB 文件在中继模式下双向传输成功，断点续传（中途 kill 后重连）能续上。
3. 自建 `croc-relay` 能被官方 Go 客户端使用（说明 relay 协议也通了）。
4. Android APK 在真机上能发送/接收 ≥ 500 MB 文件，切后台 10 分钟不被杀。
5. `--transport derp` 提示 pending 后**降级到 relay 并传输成功**（不崩溃、不静默失败，对端无感）。
6. CI 四个 workflow（`release` 唯一入口 / `build` / `lint` / `clear`），两个产物 `croc-rs` 与 `crocw-rs` 在全部目标平台上产出；门禁挂在发布链路上。
7. `tests/interop` 全绿，且 CI 每夜跑一次。

> 产物与 CI 细节见 [09-build-and-ci.md](09-build-and-ci.md)。
