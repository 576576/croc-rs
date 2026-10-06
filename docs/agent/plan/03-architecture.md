# 03 · 架构设计（函数式：纯核心 + 副作用边缘）

> 设计原则（见 [00-overview.md](00-overview.md) §4）：**优雅优先于逐字一致——但只限于实现层。**
> 线路字节是契约，必须逐字节一致；代码结构则按 Rust 惯用法与函数式风格重写，不做 Go 的逐行翻译。

---

## 1. 总分层

```
┌─────────────────────────────────────────────────────────────┐
│  UI 层        croc-cli  (clap + indicatif)                   │
│               croc-gui  (Tauri 2 commands + events)          │
└────────────────────────┬────────────────────────────────────┘
                         │ 只依赖 trait
┌────────────────────────▼────────────────────────────────────┐
│  croc-core     门面层：typestate Client<Connecting|…|Done>   │
│                组装 protocol + engine，暴露 async API         │
└───────┬────────────────────────────────────┬────────────────┘
        │ 纯                                  │ 副作用
┌───────▼──────────────────┐   ┌─────────────▼────────────────┐
│  croc-protocol  (sans-io) │   │  croc-engine  (interpreter)  │
│  ───────────────────────  │   │  ──────────────────────────  │
│  · 帧编解码                │   │  · tokio TcpStream           │
│  · 消息编解码              │   │  · 文件读写                   │
│  · 中继握手状态机          │   │  · 计时器 / 取消              │
│  · peer PAKE 状态机        │   │  · 数据面（relay/local/ts）   │
│  · 传输状态机              │   │  · 事件上报                   │
│  · chunk 规划（纯）        │   │                              │
│  · 覆盖策略（纯）          │   │  只执行 Effect，不做协议判断   │
│  输入 InEvent → Vec<Effect>│   │  Effect → Result<InEvent>    │
└───────┬──────────────────┘   └─────────────┬────────────────┘
        │                                     │
┌───────▼──────────────┐
│  croc-pake (sans-io) │
│  croc-siec (纯数学)   │
└──────────────────────┘

  ⏸ croc-tailscale：PENDING，暂不建 crate，仅保留 TailscalePlane 接口占位
     （见 08-tailscale.md）
```

**关键约束**：`croc-protocol`、`croc-pake`、`croc-siec` 三个 crate **禁止**出现 `tokio::`、`std::fs`、`Instant::now()`。CI 用 `cargo xtask purity` 强制（见 §8）。

---

## 2. 核心模式：sans-io 状态机

协议层不发起 IO，只做状态转移并**描述**下一步要做什么：

```rust
/// 进入状态机的事件（全部是已发生的事实，不是待办）
pub enum InEvent {
    Connected { conn: ConnId, peer: SocketAddr },
    FrameReceived { conn: ConnId, payload: Bytes },
    FrameSent { conn: ConnId },
    ConnectionClosed { conn: ConnId, reason: ClosedReason },
    ChunkRead { file: FileId, pos: u64, bytes: Bytes },
    ChunkWritten { file: FileId, pos: u64 },
    TimerFired(TimerToken),
    UserDecision(Decision),          // 覆盖 / 续传 / 拒绝
    Cancel,
}

/// 状态机希望外部执行的效果
pub enum Effect {
    Connect { conn: ConnId, addr: SocketAddr, via: Route },
    SendFrame { conn: ConnId, payload: Bytes },
    CloseConn { conn: ConnId },
    ReadChunk { file: FileId, range: Range<u64> },
    WriteChunk { file: FileId, pos: u64, bytes: Bytes },
    OpenSink { file: FileId, spec: SinkSpec },   // 路径 or fd（Android）
    Schedule { after: Duration, token: TimerToken },
    Emit(TransferEvent),
    Finish(Result<TransferSummary, ProtocolError>),
}

pub trait Protocol {
    fn handle(&mut self, ev: InEvent) -> Result<Vec<Effect>, ProtocolError>;
}
```

为什么这样更好：
- Go 版的状态机藏在 `for { data, err := conn.Receive(); ... }` 里，一次读循环里混合了 IO、解密、业务判断、进度上报，无法单独测试。
- Rust 版把"收到一帧 → 该做什么"变成纯函数，测试可以 `assert_eq!(m.handle(ev)?, vec![Effect::SendFrame{..}])`。
- 一次完整传输可以在测试里用事件序列**确定性重放**——这对排查"为什么对端说密码错误"价值极高。

> **已落地**：对端会话状态机在 `crates/croc-core/src/session.rs`（[`croc_core::Session`]）。
> 它覆盖 relay 路径的完整消息序列（PAKE → 加密通道 → 清单 → `recipientready` → 数据面
> → `close-sender` / `finished`），并且有一组"两端对着跑"的重放测试
> （`a_full_relay_session_runs_to_completion`）——不需要 socket、不需要超时。
> 数据面搬运、本地中继、tailcat、重连仍归 `croc-engine` 或后续增量，见该模块头部的对照表。

---

## 3. 类型状态（typestate）：让非法状态不可构造

Go 版用 `Step1ChannelSecured … Step5CloseChannels` 五个 bool 加互斥锁表达生命周期，任何组合都可能出现。Rust 版用类型编码：

```rust
pub struct Client<S: Phase> { inner: Inner, _phase: PhantomData<S> }

pub mod phase {
    pub struct Connecting;   // 连中继、等对方
    pub struct Securing;     // PAKE 进行中
    pub struct Offering;     // 清单已交换，等用户决策
    pub struct Transferring; // 数据面已建立
    pub struct Concluding;   // 收尾 / 可重连
    pub struct Done;
}

impl Client<Connecting> {
    pub async fn connect(self) -> Result<Client<Securing>, CrocError>;
}
impl Client<Securing> {
    pub async fn secure_channel(self) -> Result<Client<Offering>, CrocError>;
}
impl Client<Offering> {
    pub async fn accept(self, d: ReceiveDecision) -> Result<Client<Transferring>, CrocError>;
    pub async fn refuse(self) -> Result<Client<Done>, CrocError>;
}
impl Client<Transferring> {
    pub async fn run_to_completion(self) -> Result<Client<Done>, CrocError>;
}
```

好处：
- 编译期禁止"还没 PAKE 就发文件"、"传输中再握手一次"等 bug（Go 版靠运行时 bool 检查）。
- 每个阶段只有该阶段能调的方法，API 自解释。
- 每个阶段实现 `Drop`，保证 socket 与文件句柄一定被清理。

> 内部共享状态仍需要少量 `Arc<Mutex<_>>`（chunk 队列、当前文件句柄），但**限制在 engine 层**，不出现在 protocol 的类型里。

---

## 4. 纯函数模块（无副作用，最容易测试）

| 模块 | 形态 | 说明 |
|------|------|------|
| `chunk` | `fn plan(size: u64, chunk_size: u64) -> impl Iterator<Item = Range<u64>>` | 惰性生成，不分配 Vec |
| `chunk` | `fn missing_ranges(existing: u64, size: u64, cs: u64) -> impl Iterator<Item = Range<u64>>` | 断点续传区间 |
| `chunk` | `fn encode_ranges(&[Range<u64>]) -> Vec<i64>` / `fn decode_ranges(&[i64]) -> Result<Vec<Range<u64>>>` | 与 Go 的三元组 flattened 互转 |
| `policy` | `fn decide(existing: Option<&FileState>, incoming: &FileInfo, pref: Policy) -> Action` | 覆盖/重命名/续传/拒绝，纯决策表 |
| `manifest` | `fn diff(remote: &[FileInfo], local: &Path) -> ManifestDiff` | 纯比较，IO 由调用方先做 |
| `codephrase` | `fn generate(rng: impl Rng) -> Code` / `fn parse(&str) -> Result<Components>` | 纯字符串 + 词表 |
| `crypto` | `fn derive(shared: &[u8], ctx: Context) -> Keys` | 纯 |
| `redact` | `fn redact(msg: &str, secrets: &[&str]) -> Cow<'_, str>` | 纯 |

**风格约定**：
- 优先返回 `impl Iterator`，避免中间 `Vec`；
- 输入用 `&[T]` / `&str`，不用 `&Vec<T>` / `&String`；
- 每个纯模块配 `proptest` 性质测试（例如 `encode ∘ decode = id`、区间并集等于全集）。

---

## 5. 新类型与单位安全

用 newtype 杜绝"把房间名当密码传"这类错误：

```rust
pub struct RoomName(String);
pub struct Secret(String);          // PAKE 口令
pub struct RelayPassword(String);   // 中继密码（weakKey）
pub struct SessionKey(Box<[u8; 32]>);
pub struct ChunkOffset(u64);
pub struct ConnId(u32);
pub struct TransferId(Uuid);
```

- `SessionKey` 用 `zeroize` 落幕，`Debug` 只打印 `<redacted>`。
- `RoomName` / `Secret` 的 `Debug` 同样脱敏，日志里永远看不到明文。
- `EuclidMod`（`croc-siec` 内）封装欧几里得取模，把"必须兼容 Go `big.Int` 语义"这一件事关在一个小类型里，不污染调用方——这正是"优雅优先"的具体落点：**丑的地方要被封装，而不是被注释**。

---

## 6. 数据面抽象（含 Tailscale）

```rust
/// 数据面：负责在 PAKE 之后搬运文件字节。协议层只认这个 trait。
pub trait DataPlane: Send + Sync {
    fn kind(&self) -> PlaneKind;                                  // Relay | Local | Tailscale
    fn open(&self, streams: u8) -> impl Future<Output = Result<Vec<Box<dyn ByteStream>>, PlaneError>> + Send;
    fn fallback(&self) -> Option<PlaneKind>;                      // 失败时该退到哪
}

pub trait ByteStream: AsyncRead + AsyncWrite + Send + Unpin {
    fn label(&self) -> &str;   // 用于日志与进度归属
}
```

> `DataPlane` 需要 `dyn` 对象安全（运行时按平台选择实现），因此用 `async-trait` 或手写 `BoxFuture`；其余 trait 一律用原生 `async fn` in trait。

| 实现 | 平台 | 状态 |
|------|------|------|
| `RelayPlane` | 全部 | v1 必须 |
| `LocalPlane` | 全部（局域网直连 + 本地临时中继） | v1 必须 |
| `TailscalePlane` | 桌面（Linux x64/ARM64、macOS ARM64、Windows x64） | ⏸ **pending**（等上游 tailcat-rs），接口预留但不实现，见 [08-tailscale.md](08-tailscale.md) |

协议层根据 `PlaneKind` 决定在 `Features` 里声明什么，并通过 `transport-select` 消息与对端达成一致——这部分逻辑是纯的，放在 `croc-protocol`。

---

## 7. Engine：副作用解释器

```rust
pub struct Engine {
    conns: HashMap<ConnId, FramedConn>,
    files: HashMap<FileId, Box<dyn FileSink>>,
    timers: TimerWheel,
    cancel: CancellationToken,
    sink: Arc<dyn EventSink>,
    confirm: Arc<dyn ConfirmHandler>,
}

impl Engine {
    pub async fn run(mut self, mut proto: impl Protocol) -> Result<TransferSummary> {
        loop {
            let effects = proto.handle(self.next_event().await?)?;
            for e in effects { self.apply(e).await?; }   // apply 里不做协议判断
            if let Some(r) = proto.take_result() { return r; }
        }
    }
}
```

并发细则：

| 关注点 | 方案 |
|--------|------|
| 运行时 | tokio multi-thread。CLI 用 `#[tokio::main]`；**GUI 复用 Tauri 的 runtime**，不再起一个 |
| 连接 | 每个 `ConnId` 一个读 task；写侧用 `mpsc` 队列串行化（等价于 Go 的 `writeMu`，但不持锁 await） |
| 数据面 | N 条流 → N 个 task，从 chunk 队列 `claim()`；队列 `Arc<Mutex<ChunkQueue>>`，**锁内不做 IO** |
| 文件 IO | 32 KB 粒度的 `read_at`/`write_at`。`std::fs` + `spawn_blocking`（pread/pwrite 语义清晰，避免 `tokio::fs` 内部线程池成为瓶颈）；Android fd 走同一 `FileSink` trait |
| 取消 | 统一 `CancellationToken`；所有 `select!` 必带 `token.cancelled()` 分支；future 必须 cancel-safe |
| 背压 | 有界 `mpsc`（容量 = 2 × 流数）；写盘完成才 ack |
| 进度 | engine 侧节流 `max(10 ms, 0.1%)` 再 `Emit` |

---

## 8. 工具链与编码规范（强制，CI 门禁）

### 8.1 工具链

| 项 | 值 | 说明 |
|----|----|------|
| Edition | **2024**（workspace 统一） | `rust-version` 与 edition 在 `[workspace.package]` 声明，各 crate 继承 |
| 工具链 | **stable**（默认） | 本机实测 `rustc 1.98.1 (2026-09-01)`；CI 用 `dtolnay/rust-toolchain@stable` |
| MSRV | `rust-version = "1.98"` | CI 额外跑一个 MSRV job（`--ignore-rust-version` 关闭），防止依赖悄悄抬高要求 |
| nightly | **仅 Miri 使用** | 只对 `croc-siec`、`croc-pake` 跑 `cargo +nightly miri test`；不参与产物构建。**本仓库一行 `unsafe` 都没有**，所以 Miri 查的是依赖里的 unsafe 与"有人以后伸手去写裸指针"的哨兵，不是我们的门禁——见 §8.5 |
| 产物构建 | 一律 stable | 发布产物的 workflow 不允许出现 nightly |

> tailscale crate 曾要求 MSRV 1.97，但它已标记为 **pending**（见 [08-tailscale.md](08-tailscale.md)），因此不再抬高本项目工具链要求。

### 8.2 `rustfmt.toml`（对齐 Suwayomi-next）

```toml
edition = "2024"
max_width = 120
use_small_heuristics = "Max"
```

### 8.3 Lint（根 `Cargo.toml` `[workspace.lints]` 统一定义）

对齐 Suwayomi-next 的做法：级别集中在 workspace，测试代码在 crate 根部 `#![allow]` 豁免，CI 用 `-D warnings` 兜底。

```toml
[workspace.lints.rust]
unsafe_code = "warn"        # 逐处加 // SAFETY:
rust_2018_idioms = "warn"
missing_docs = "warn"
unexpected_cfgs = { level = "warn", check-cfg = ['cfg(feature, values("tailscale"))'] }

[workspace.lints.clippy]
# --- 不变量三件套：生产代码一律 warn，清零后由 CI 的 -D warnings 兜底 ---
unwrap_used = "warn"
expect_used = "warn"
panic = "warn"
unreachable = "warn"
todo = "warn"               # ⚠ 影响设计：pending 占位不许用 todo!()，见 08-tailscale.md

# --- 异步正确性 ---
await_holding_lock = "warn" # 持锁跨 await 是死锁温床

# --- 控制流函数式化（与本项目 FP 原则一致）---
indexing_slicing = "warn"   # 越界 panic 的根源，改用 .get() / 迭代器 / 切片
manual_let_else = "warn"
map_unwrap_or = "warn"
option_if_let_else = "warn"
needless_continue = "warn"
useless_let_if_seq = "warn"
match_same_arms = "warn"
single_match_else = "warn"
branches_sharing_code = "warn"
if_same_then_else = "warn"

# --- 类别级 ---
correctness = "deny"
suspicious = "warn"
complexity = "warn"
perf = "warn"
```

**豁免**：`croc-siec` / `croc-pake` 内的 `indexing_slicing` 逐处 `#[allow]` 并说明理由（大整数按字节遍历确实需要下标）；`module_name_repetitions` 全局豁免（`croc_` 前缀）。

> **本仓库没有 `unsafe`。** 0 处 `unsafe`、0 处 FFI（没有 `extern "C"` / `#[no_mangle]` /
> `#[link`）、0 处 `transmute` / `from_raw` / `as_ptr`，构建脚本里也没有 `cc` / `bindgen` /
> `pkg-config`。唯一的 C 代码在依赖里（Tauri 在 Linux 上链 GTK / WebKitGTK），不是我们写的。
> `unsafe_code = "warn"` + CI 的 `-D warnings` 让"加一处 unsafe"变成一个必须显式面对的
> 编译错误——**这条纪律才是真正的门禁**，Miri 只是它的补充。

> ⚠ **对设计的反向约束**：因为 `todo = "warn"` + CI `-D warnings`，
> `TailscalePlane` 的 pending 占位**不能写 `todo!()`**，改为返回 `Err(PlaneError::Unavailable)`——
> 见 [08-tailscale.md](08-tailscale.md) §3.1。

### 8.4 规则清单

| 类别 | 规则 |
|------|------|
| 错误 | 库用 `thiserror`（`CrocError`），应用（cli/gui）用 `anyhow`；禁止生产代码 `unwrap()`；`expect()` 仅用于不变量；错误消息小写、无结尾标点 |
| API | Builder 方法 `#[must_use]`；公开 enum 加 `#[non_exhaustive]`；实现 `From` 而非 `Into`；复杂构造用 Builder |
| 异步 | 原生 `async fn` in trait（1.75+），**仅对象安全必需的 `DataPlane` 用 `async-trait`**；禁止跨 `.await` 持 `Mutex`；CPU 密集走 `spawn_blocking` |
| 内存 | 已知容量 `with_capacity`；热路径复用 buffer（`encrypt_in_place`）；大 enum 变体 `Box` 化；帧用 `Bytes` 零拷贝传递 |
| 数值 | 禁止 `as` 窄化 cast，用 `TryFrom`；偏移/长度用 `checked_add` / `saturating_sub` |
| 类型 | ID 用 newtype；状态互斥用 enum；不用字符串表达枚举语义 |
| 命名 | `as_`/`to_`/`into_` 按约定；bool 用 `is_`/`has_`/`can_`；acronym 当普通词（`HttpServer`） |
| 文档 | 所有公开项 `///`；fallible 函数写 `# Errors`；库 crate 不安装 tracing subscriber |
| 观测 | 只用 `tracing`，禁止 `println!`；结构化字段不用字符串插值；**绝不记录 secret/room** |
| 测试 | 单测 `#[cfg(test)] mod tests`；async 用 `#[tokio::test]`；纯函数配 `proptest`；协议层用 golden + 事件序列重放；基准用 `criterion` |
| 项目 | 声明 `rust-version`；依赖在 `[workspace.dependencies]` 统一；`main.rs` 极简；release 开 `lto = "fat"` + `codegen-units = 1` |

### 8.5 架构门禁（`xtask`）

| 检查 | 命令 |
|------|------|
| 纯 crate 不得引入 IO | `cargo xtask purity` — 扫描 `croc-protocol|croc-pake|croc-siec` 中是否出现 `tokio::`、`std::fs`、`Instant::now`、`thread_rng` |
| `unsafe` 纪律 | `[workspace.lints]` 的 `unsafe_code = "warn"` + CI 的 `-D warnings` |
| 交叉编译 | `cargo build --target aarch64-linux-android --workspace` |
| feature 完整性 | `cargo check --all-features` + `unexpected_cfgs` |
| Miri | 对 `croc-siec` / `croc-pake` 跑 `cargo +nightly miri test`（**哨兵，不是门禁**） |

---

## 9. 错误模型

```rust
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CrocError {
    #[error("relay unreachable: {addr}")]
    RelayUnreachable { addr: String, #[source] source: io::Error },
    #[error("bad password")]
    BadPassword,
    #[error("peer refused the transfer")]
    PeerRefused,
    #[error("incompatible protocol version: peer={peer} local={local}")]
    VersionMismatch { peer: u32, local: u32 },
    #[error("transfer interrupted")]
    Interrupted,
    #[error("internal: {0}")]
    Internal(#[from] InternalError),
}

impl CrocError {
    pub fn kind(&self) -> ErrorKind;   // Retryable | Terminal | Internal
}
```

- 所有对外输出（日志、`Display`、GUI 事件）经过 `redact(msg, &[secret, room])`。
- `redact` 是纯函数，单测断言"错误串里不含 secret"。

---

## 10. 依赖选型

| 用途 | crate | 备注 |
|------|-------|------|
| 异步 | `tokio`（rt-multi-thread, net, io-util, time, sync, macros） | |
| 字节缓冲 | `bytes` | 零拷贝帧传递 |
| CLI | `clap`（derive + `clap_complete`） | |
| 序列化 | `serde`、`serde_json`（**`arbitrary_precision` 仅 `croc-pake` 内**）、`serde_bytes` | 见 [02-crypto.md](02-crypto.md) §3.3 |
| 大整数 | `num-bigint`、`num-traits` | 自写 `modinv` |
| 加密 | `aes-gcm`、`pbkdf2`、`sha2`、`hmac`、`hkdf`、`argon2`、`chacha20poly1305`、`subtle`、`zeroize` | 纯 Rust，**禁止 openssl / ring**（Android 交叉编译） |
| 压缩 | `flate2`（`miniz_oxide` 后端） | |
| 哈希 | `xxhash-rust`、`md-5`、`highway`、自研 imohash | |
| 进度条 | `indicatif`（仅 CLI） | |
| 剪贴板 / QR | `arboard`、`qrcodegen`（仅 CLI） | |
| 代理 | `fast-socks5` + 自写 CONNECT | |
| 本地发现 | `socket2`（组播/广播） | |
| zip / gitignore | `zip`、`ignore` | |
| 日志 | `tracing` + `tracing-subscriber` | |
| 取消 | `tokio-util` `CancellationToken` | |
| 错误 | `thiserror`（库）、`anyhow`（应用） | |
| 测试 | `proptest`、`criterion`、`insta`、`mockall` | |
| Tailscale | `tailscale` 0.6 | ⏸ **pending**，未引入依赖；见 [08-tailscale.md](08-tailscale.md) |

**移动端硬约束**：`croc-protocol` / `croc-pake` / `croc-siec` / `croc-core` 必须在 `aarch64-linux-android`、`armv7-linux-androideabi`、`x86_64-linux-android` 上编译通过。

---

## 11. `croc-relay`

- 每端口一个 listener：`:9009` 控制端口，`:9010..9013` 数据端口（默认 `--ports 9009,9010,9011,9012,9013`）。
- `RoomRegistry`: `DashMap<RoomName, Room>`；房间内两条连接用 `tokio::io::copy_bidirectional` 转发。
- 准入限流：`DashMap<IpAddr, SlidingWindow>` + `DashMap<RoomName, SlidingWindow>`。
- 握手有独立 deadline 与 pending 上限，超时即断开不占房间。
- **复用 `croc-protocol` 的服务端握手状态机**（同样是 sans-io），因此 relay 与 client 共享同一套帧/握手实现，不会出现"两端理解不一致"。

---

## 12. 与 GUI 的接口（详见 [05-gui-plan.md](05-gui-plan.md)）

`croc-gui` 是薄适配层：

```
Tauri command ──> croc-core::Client::<Connecting>::new(...).run_to_completion()
                    注入 TauriEventSink / TauriConfirmHandler
Tauri event  <── TransferEvent（节流后 emit 到前端）
```

Rust 侧不保存 UI 状态；只维护 `HashMap<TransferId, CancellationToken>`。状态归前端。
