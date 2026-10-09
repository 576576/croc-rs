# 项目总览

## 1. 项目是什么

用 Rust 重写的 croc，两个产物：

- **`croc-rs`（CLI）**：与 Go 版 croc v11.5.4 **线路级兼容**的文件传输工具——能与官方 `croc`
  客户端互相收发文件、能连官方中继 `croc.schollz.com:9009`、能自建 Rust 中继（`croc-rs relay`），
  命令行参数与 Go 版对齐，可以当作替代品使用。
- **`crocw-rs`（Tauri 2 GUI）**：复用同一套核心（`croc-core` + `croc-engine`）的跨平台图形界面，
  界面基准是上游的 croc web（getcroc.com）。计划见 [plan/gui.md](plan/gui.md)。

基线是 `schollz/croc` **v11.5.4**（`v11.5.4-4-g62fc9d17`）。

## 2. 为什么不沿用原方案

| 原方案 | 问题 |
|--------|------|
| CGO 交叉编译 Android（`build.sh` 编 `.so`） | 每个 ABI 一套产物，构建脚本脆弱，本地开发几乎无法调试 |
| Go 版 `src/croc/croc.go` 近 4000 行 | 状态机、UI、协议、文件系统耦合，改一处要读全篇 |
| 进度上报靠 `CrocPollProgress` 轮询全局单例 | 不支持并发传输，取消 / 暂停语义弱 |

Rust 化之后是**一套核心 + 两个前端**：Android 由 Tauri 的 Gradle 流程直接把 Rust 编进 `.so`，
不再手工维护 CGO 桥。

## 3. 范围

### 不做

- **`croc ssh`**（共享终端）——依赖 PTY 与 tailcat 的完整能力。
- **`croc store` 的服务端**（Go 的 `croc-web`）——客户端已实现（`croc-rs store` / `send --store` /
  `--revoke`），服务端沿用 Go 的。
- **复刻 Go 版 tailcat 的 CBOR `ConnBlob` wire**（匿名临时节点 + 自签名 DERP 引导 + 一次性客户端
  密钥授权）——原因见 [plan/tailscale.md](plan/tailscale.md)。
- **Web 版**（`getcroc.com` 那种 WASM 接收端）。
- **自研新协议 / 新加密套件**——本项目的立身之本是"兼容"，任何协议改动都是独立提案。

### [PENDING] 暂缓

**Tailscale 数据面**（`--transport derp`）。当前行为是 CLI 层把它翻译成 `relay` 并打一条提示，
**数据面抽象本身尚未实现**——`croc-core` 里没有 `PlaneKind`，`croc-engine::data::DataPlane` 是一个
具体的中继连接持有者而不是可替换的 trait。原因、解除条件与待建的设计见
[plan/tailscale.md](plan/tailscale.md)。

## 4. 设计原则

1. **对外（线路字节）是契约，对内（代码结构）不做逐行翻译。**

   - 对外：房间名、nonce 布局、大整数编码、transcript 哈希都是硬契约，错一个字节就是"密码错误"。
   - 对内：不追求把 Go 逐行搬过来。Go 版的 `croc.go` 是一个 4000 行、几十个 `sync.Mutex` 的巨型
     状态机，那是 Go 的写法。Rust 版把同一套流程写成 sans-io 的纯状态转移（[03](architecture.md) §2）。
   - 因此**不写** `// COMPAT:` 式的翻译腔注释。凡"为了兼容不得不写丑"的地方，用类型封装把它关进
     一个小的纯函数；兼容性知识沉淀在本目录的文档与 golden 测试里。

2. **纯核心，副作用在边缘（functional core, imperative shell）**。协议状态机不碰 socket、文件、时钟；
   它接收事件、返回"下一步做什么"的描述，由外层解释器执行。因此协议层可以 100% 单测、可确定性重放。

3. **核心无 UI 依赖**。`croc-core` 不引用任何 Tauri / 终端 / 进度条代码，只暴露事件与确认回调的
   trait，CLI 与 GUI 各自实现。

4. **可测试性优先**。所有涉及线路字节的模块，都要能在没有网络的情况下用 golden 向量测试
   （Go 版生成的固定输入/输出）。

5. **取消是一等公民**。所有长操作接 `CancellationToken`，对应 Go 版 `c.stop.ctx` 的语义。

6. **错误可分诊**。分"可重试（网络断开）/ 终态（密码错误、对端拒绝）/ 内部 bug"三类，GUI 据此
   决定重试还是报错。

## 5. 仓库结构

```
croc-rs/
├── crates/
│   ├── croc-siec/            底层椭圆曲线算术（SIEC255 等）
│   ├── croc-pake/            schollz/pake v3 的线路兼容移植（SPAKE2）
│   ├── croc-pakekey/         PAKE v2：把会话密钥绑到参与者、会话、用途与转录
│   ├── croc-protocol/        线路层：comm 帧、消息编解码、压缩、AEAD、清单、能力协商、中继握手
│   ├── croc-net/             async IO 原语：Framed、随机源
│   ├── croc-core/            纯领域层：对端会话状态机、chunk、codephrase、policy、redact
│   ├── croc-engine/          副作用解释器：tokio IO、文件、数据面、代理、DNS、限速
│   ├── croc-relay/           中继服务端
│   ├── croc-store/           暂存传输（`croc store`）客户端
│   ├── croc-cli/             可执行程序 `croc-rs`
│   └── croc-gui/             Tauri 2 应用（Rust 侧），产物 `crocw-rs`
├── ui/                       GUI 前端（尚未创建；`tauri.conf.json` 的 `frontendDist` 指向它）
├── docs/agent/               本目录
└── tests/
    ├── interop/              与 Go croc 的端到端互通矩阵
    └── golden/               Go 版生成的协议字节向量
```

分层的关键约束：`croc-protocol` / `croc-pake` / `croc-siec` **不得出现** `tokio::`、`std::fs`、
`std::net`、`std::process`、`std::thread`、`Instant::now`、`SystemTime::now`、`thread_rng`，由
`cargo xtask purity` 强制；`croc-core` 同样按纯层写，但门禁尚未把它纳入。

详见 [architecture.md](architecture.md)。

## 6. 与上游的关系

| 项目 | 关系 | 处理方式 |
|------|------|---------|
| `schollz/croc` v11.5.4 | 协议与行为基线 | **不作为 submodule**；`tests/golden/` 保留由 Go 版生成的向量；互通矩阵需要 Go 版二进制（放在平级的 `croc-playground/`） |
| 同仓库 `web/`（croc web，MIT） | GUI 的**界面**基准 | 照搬设计令牌、`styles.css` 与组件结构到 `crates/croc-gui/ui`，见 [plan/gui.md](plan/gui.md)；MIT 的署名义务记在 [testing.md §5](testing.md) |
