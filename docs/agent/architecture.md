# 架构

> 设计原则见 [overview.md](overview.md) §4：**线路字节是契约，代码结构不做 Go 的逐行翻译**。

## 1. 分层

```
┌─────────────────────────────────────────────────────────────┐
│  UI 层        croc-cli  (clap + indicatif)                   │
│               croc-gui  (Tauri 2 commands + events)          │
└────────────────────────┬────────────────────────────────────┘
                         │ 只依赖 trait（事件回调 / 确认回调）
┌────────────────────────▼────────────────────────────────────┐
│  croc-engine   副作用解释器：tokio IO、文件、数据面、代理、    │
│                DNS、限速。把 Effect 翻译成真实动作。          │
└────────────────────────┬────────────────────────────────────┘
                         │ 纯
┌────────────────────────▼────────────────────────────────────┐
│  croc-core     纯领域层：Session 状态机、chunk、codephrase、  │
│                policy、redact、types                         │
│                InEvent → Vec<Effect>                         │
├─────────────────────────────────────────────────────────────┤
│  croc-protocol 线路层：comm 帧、消息编解码、压缩、AEAD、      │
│                清单、能力协商、中继握手状态机                 │
├─────────────────────────────────────────────────────────────┤
│  croc-pakekey / croc-pake / croc-siec                        │
└─────────────────────────────────────────────────────────────┘

  croc-net        Framed / 随机源（副作用边缘的第一层）
  croc-relay      中继服务端（复用 croc-protocol 的握手状态机）
  croc-store      暂存传输客户端（HTTP + 加密）
```

**关键约束**：`croc-protocol` / `croc-pake` / `croc-siec` 三个 crate **不得出现** `tokio::`、
`std::fs`、`std::net`、`std::process`、`std::thread`、`Instant::now`、`SystemTime::now`、
`thread_rng`，由 `cargo xtask purity` 强制。`croc-core` 同样按纯层写（依赖只有这三个 crate 加
`sha2` / `hex` / `zeroize` / `thiserror` / `num-*`），但门禁目前没把它纳入。

## 2. 核心模式：sans-io 状态机

对端会话的全部状态转移在 `croc-core::session`（[`croc_core::Session`]）。它对应 Go
`src/croc/croc.go` 里的 `Send` / `Receive` / `processMessage` / `updateState` 那一大坨——Go 把状态
藏在 `for { data, err := conn.Receive(); … }` 的读循环里，一次迭代里混着解密、业务判断、进度上报
与文件 IO，于是"为什么对端说密码错误"只能靠加日志猜。

这里把它写成 `Session::handle`：输入一个**已经发生的事实**，输出一串**希望外部执行的效果**。

```rust
pub enum Stage { Idle, Securing, AwaitingDecision, Transferring, Concluding, Done }

pub enum InEvent {
    PeerConnected,
    Frame(Vec<u8>),                  // 已剥掉帧头，尚未解密
    Decision(Answer),                // 用户对清单的决策（只有接收方）
    RequestFile { index: u64, ranges: Vec<i64> },
    // …
}

pub enum Effect {
    Send(Vec<u8>),                   // 把这一帧发到控制连接
    SendFile { index: u64, ranges: Vec<i64>, streams: u8 },
    Emit(Event),                     // 上报给 UI
    Finish(Result<Outcome, SessionError>),
    // …
}
```

不碰 socket、不碰文件、不看时钟，随机数由调用方注入。于是整条握手与清单交换可以在单元测试里用
事件序列**确定性重放**（`a_full_relay_session_runs_to_completion`），不需要 socket，也不需要 sleep。

覆盖范围是 **relay 传输路径**的完整消息序列。不在里面、归 `croc-engine` 或另属状态机的部分
（模块头部有对照表）：中继控制端口握手属于 `croc_protocol::relay`；数据面上的分块搬运、本地中继、
局域网发现需要 socket 与文件 IO，属于 `croc-engine`。

## 3. 纯函数模块

| 模块 | 形态 | 说明 |
|------|------|------|
| `croc-core::chunk` | `plan` / `missing_ranges` / `ChunkRanges` | 分片规划；区间用 Go 的**扁平三元组**形态表示，`[]` 表示"整个文件" |
| `croc-core::policy` | `decide(existing, incoming, pref) -> Action` | 覆盖 / 重命名 / 续传 / 拒绝的纯决策表 |
| `croc-core::codephrase` | `generate(rng)` / `parse(&str)` | 纯字符串 + 词表 |
| `croc-core::redact` | `redact(msg, secrets) -> Cow<str>` | 脱敏 |
| `croc-protocol::manifest` | 清单编解码与校验 | 含文件名安全校验（见下） |
| `croc-protocol::frame` / `message` / `codec` | 编解码 | 帧、JSON、压缩、AEAD |
| `croc-pakekey` | `derive(shared, ctx) -> Keys` | 纯 |

**风格约定**：优先返回 `impl Iterator` 避免中间 `Vec`；输入用 `&[T]` / `&str` 而不是
`&Vec<T>` / `&String`；能配性质测试的配 `proptest`（例如 `encode ∘ decode = id`）。

### 3.1 文件名的安全校验

接收方的每一条路径都要过这一关，规则对齐 Go 的 `receivefs`：

- **NFC 归一**后再比较（macOS 上 `a.txt` 与归一前后不同形的同名会互相覆盖）；
- **Unicode 大小写折叠**做重复检测（Windows / macOS 文件系统大小写不敏感，`a.txt` 与 `A.txt`
  会覆盖同一个文件）；
- 字符类别白名单（等价 Go 的 `IsGraphic && IsPrint`）；
- Windows 保留设备名（`CON` / `NUL` / `COM1`…）与结尾的点 / 空格；
- 绝对路径、`..`、超深目录、超长名一律拒绝。

## 4. 新类型与单位安全

用 newtype 杜绝"把房间名当密码传"这类错误：

```rust
pub struct RoomName(String);        // croc-core::types
pub struct Secret(Box<[u8]>);       // croc-core::types
pub struct SessionKey(…);           // croc-protocol
```

`RoomName` / `Secret` / 会话密钥的 `Debug` 一律脱敏——日志里永远看不到明文。

`EuclidMod`（`croc-siec` 内）封装欧几里得取模，把"必须兼容 Go `big.Int` 语义"这件事关在一个小类型
里，不污染调用方——这是"优雅优先"的具体落点：**丑的地方要被封装，而不是被注释**。

## 5. 数据面

数据面是**中继的数据端口**：房间名 `{room}-{j}`，`j` 从 1 开始。`croc-engine::data::DataPlane`
持有这批连接。

- **一次传输只建一次，全程复用**。中继的一个房间只容得下两端各一条连接，每条文件重建的话第二条
  文件会撞上自己上一条还开着的连接，中继回 `room full`——表现是"单文件传得好好的，目录一传就断
  在第二个文件上"。Go 同样如此（`openRelayChannels()` 一次、留到 attempt 结束）。
- **连接在结构体 `drop` 时关**，也就是整个传输结束、`finished` 已交换之后。中途关连接会让中继把
  还没读走的数据连同连接一起丢掉。
- 发送方只发被请求的块（认领制），接收方数够就收工，所以文件之间不会留下半条帧。
- `--no-multi` 把数据面砍成一条流（只连第 0 个数据端口）。

局域网直连（本地临时中继 + 组播发现）在 `croc-engine::local`，走的是同一套数据面代码。

> [PENDING] 曾计划把数据面抽象成可按平台替换的 trait（`Relay` / `Local` / `Tailscale`），**没有实现**。
> 现状是一个具体类型；tailscale 那一支的待建设计见 [plan/tailscale.md](plan/tailscale.md)。

局域网直连（本地临时中继 + 组播发现）原本也在这张图里，实际落在 `croc-engine::local`，走的是同一套
数据面代码。

## 6. 并发

| 关注点 | 方案 |
|--------|------|
| 运行时 | tokio multi-thread。CLI 用 `#[tokio::main]`；GUI 复用 Tauri 的 runtime |
| 数据面 | N 条流 → N 个 task，从分片队列认领；队列私有，不跨 `await` 持锁 |
| 文件 IO | 32 KB 粒度的读写 |
| 取消 | 统一 `CancellationToken`；所有 `select!` 必带 `token.cancelled()` 分支 |
| 进度 | 节流后再上报，避免每条分片都惊动 UI |

## 7. 工具链与编码规范

### 7.1 工具链

| 项 | 值 |
|----|----|
| Edition | **2024**（workspace 统一，在 `[workspace.package]` 声明，各 crate 继承） |
| 工具链 | **stable**，全仓不用 nightly |
| MSRV | 声明在 `rust-version`；CI 有一个 MSRV job 防止依赖悄悄抬高要求 |

### 7.2 格式与 lint

- `rustfmt.toml`：`edition` / `max_width` / `use_small_heuristics`。
- lint 级别集中在根 `Cargo.toml` 的 `[workspace.lints]`，**不在这里重复罗列**——清单文件是唯一来源。
  做法是对齐 Suwayomi-next：生产代码一律 `warn`，测试代码在 crate 根部 `#![allow]` 豁免，CI 用
  `-D warnings` 兜底。
- `todo = "warn"` + CI `-D warnings` 有一条**反向约束**：占位实现不能写 `todo!()`，要返回一个错误值。

> 本仓库没有 `unsafe`，也没有 FFI。依赖图里带"FFI 味道"的 `windows-sys` / `libc` 只是平台 API 的
> 绑定声明，不含 C 源码；真正链系统 C 库的只有 `croc-gui`（Linux 上要链 GTK / WebKitGTK，WebView
> 绕不开，且是**链接**不是编译）。因此不需要 Miri——安全 Rust 产不出 UB。

### 7.3 规则清单

| 类别 | 规则 |
|------|------|
| 错误 | 库用 `thiserror`，应用（cli/gui）用 `anyhow`；禁止生产代码 `unwrap()`；`expect()` 仅用于不变量；错误消息小写、无结尾标点 |
| API | Builder 方法 `#[must_use]`；公开 enum 加 `#[non_exhaustive]`；实现 `From` 而非 `Into` |
| 异步 | 原生 `async fn` in trait；禁止跨 `.await` 持 `Mutex`；CPU 密集走 `spawn_blocking` |
| 内存 | 已知容量 `with_capacity`；热路径复用 buffer（`encrypt_in_place`）；帧用 `Bytes` 零拷贝传递 |
| 数值 | 禁止 `as` 窄化 cast，用 `TryFrom`；偏移/长度用 `checked_add` / `saturating_sub` |
| 类型 | ID 用 newtype；状态互斥用 enum；不用字符串表达枚举语义 |
| 命名 | `as_`/`to_`/`into_` 按约定；bool 用 `is_`/`has_`/`can_`；acronym 当普通词（`HttpServer`） |
| 文档 | 所有公开项 `///`；fallible 函数写 `# Errors`；库 crate 不安装 tracing subscriber |
| 观测 | 只用 `tracing`，禁止 `println!`；结构化字段不用字符串插值；**绝不记录 secret/room** |
| 测试 | 单测 `#[cfg(test)] mod tests`；async 用 `#[tokio::test]`；协议层用 golden + 事件序列重放 |
| 依赖 | 声明 `rust-version`；依赖在 `[workspace.dependencies]` 统一；`main.rs` 极简；release 开 `lto = "fat"` + `codegen-units = 1` |

### 7.4 架构门禁

```bash
cargo xtask purity    # croc-siec / croc-pake / croc-protocol 不得引入 IO、时钟、网络、进程与
                      # OS 随机源（tokio:: / std::fs / std::net / std::process / std::thread /
                      # Instant::now / SystemTime::now / thread_rng），依赖里也不许出现 tokio
```

`unsafe` 的纪律由 `[workspace.lints]` 的 `unsafe_code = "warn"` + CI 的 `-D warnings` 兜底。

## 8. 错误模型

错误按 crate 分层，都用 `thiserror`：

- `croc-pake::PakeError`、`croc-pakekey::PakeKeyError`、`croc-protocol::*Error`：**协议违规**
  （点不在曲线上、确认标签不符、帧超限、清单非法）。
- `croc-core::SessionError`：状态机层面的非法转移。
- `croc-engine::EngineError`：**副作用失败**（连不上、读写失败、数据面建不起来）。

这个分工是刻意的：消息顺序错了和磁盘写不进去是两类问题，调用方要能分开处置。

所有对外输出（日志、`Display`、GUI 事件）经过 `redact`。`redact` 是纯函数，单测断言"错误串里不含
secret"。

## 9. 依赖选型原则

**具体清单与版本以 `Cargo.toml` 的 `[workspace.dependencies]` 为准，本文不重复罗列。** 选型时守的
几条：

- **加密用 RustCrypto 实现**（`aes-gcm` / `pbkdf2` / `sha2` / `hmac` / `hkdf` / `subtle` / `zeroize`），
  加上自研的 `croc-siec` / `croc-pake`。
- **需要出 Android 的那条链路（`croc-gui` 及其依赖）不引入 C 加密后端**；`croc-store` 的 HTTP 栈
  用 rustls 系列，根证书取静态列表而不是系统信任库（精简容器里可能不存在）。
- 压缩用 `flate2`（`miniz_oxide` 后端）；`croc-pake` 的 JSON 单独用 `arbitrary_precision`，不扩散到
  其它 crate。
- 移动端硬约束：`croc-protocol` / `croc-pake` / `croc-siec` / `croc-core` 必须在
  `aarch64-linux-android` / `armv7-linux-androideabi` / `x86_64-linux-android` 上编译通过。

## 10. `croc-relay`

- 每端口一个 listener：`:9009` 控制端口，`:9010..9013` 数据端口（默认
  `--ports 9009,9010,9011,9012,9013`）。
- `RoomRegistry` 用一个映射表 + 互斥锁维护房间；房间内两条连接建立后做双向字节转发。
- 握手有独立的 waiting 上限，超时即断开，不占房间。
- **复用 `croc-protocol` 的服务端握手状态机**（同样是 sans-io），所以 relay 与 client 共享同一套
  帧 / 握手实现，不会出现"两端理解不一致"。

## 11. 与 GUI 的接口

`croc-gui` 是薄适配层：Tauri command 启动传输，注入 Tauri 的事件回调与确认回调，`TransferEvent`
节流后 `emit` 到前端。

Rust 侧不保存 UI 状态，只维护"传输 ID → 取消句柄"。状态归前端。
