# [PENDING] Tailscale / Tailcat 数据面

> ## 状态：**[PENDING]** —— 等上游提供可用的 tailcat Rust 实现
>
> 本文的调研结论长期有效，但**不进入排期**。在 upstream 给出可用的实现之前：
>
> - **不引入 `tailscale` crate 依赖**（连 optional 也不加，避免污染 MSRV 与 CI）；
> - `--transport derp` 接受，行为 = 打一条 `pending` 提示后按 relay 处理；
> - 数据面抽象**尚未实现**（见 §3），等真要接 tailscale 时再建。
>
> 解除条件：upstream 发布稳定的 tailcat Rust 实现 → 按 §7 落地。

## 1. 为什么挂着

- **实现层**强行推进会引入 pre-1.0、MSRV 更高、无兼容保证、且模型不匹配的依赖，收益不抵成本。
- **架构层**（数据面抽象 + 降级协商）在真正要接第二、第三个数据面之前做，只是凭空多一层间接。

## 2. 调研结论

### 2.1 croc 的 `--transport derp` 到底是什么

不是"直接用 DERP 协议"，而是 **Tailcat**：

| 事实 | 出处 |
|------|------|
| croc vendored 了 `tailscale/tailcat` 的网络与 wire 层（commit `c04c5af`，2026-08-27，BSD-3-Clause） | `internal/tailcat/UPSTREAM.md` |
| 传输时启动一个**内嵌的、匿名的 Tailscale 节点**（不需要 tailnet 账号 / auth key） | `internal/tailcat/tailcat.go` |
| 节点身份是临时生成的 node key；**offer 是 CBOR 编码的 `ConnBlob`**，含 `ServerPublic`（节点公钥）与 DERP region 描述 | `internal/tailcat/wire.go` |
| offer 大小上限 4 KB，经 `tailcat-offer` 消息传给对端；对端用 `tailcat-status` 回报，再用 `transport-select` 定选 | `src/croc/tailcat_negotiation.go` |
| 数据面是 tailnet 上的 1–16 条 TCP 流（`FirstTCPPort = 40000`） | `src/tailcattransport/config.go` |
| 授权绑定：用 croc 的 PAKE session key 派生一次性客户端密钥 | `internal/tailcat/UPSTREAM.md` |
| Go 版仅 `linux/windows/darwin/freebsd/openbsd` 编译（build tag `!croc_no_tailcat && …`），**Android 不支持** | `src/tailcattransport/bundle_supported.go:1` |

### 2.2 Tailscale 官方 Rust crate

| 项 | 结论 |
|----|------|
| crate | **`tailscale`**（crates.io） |
| 仓库 | `github.com/tailscale/tailscale-rs`（2026-04 公开发布，WIP） |
| 模型 | **加入一个已存在的 tailnet**：`Device::new(&Config, Option<auth_key>)` |
| 关键 API | `tcp_listen` / `tcp_connect` / `udp_bind` / `ipv4_addr` / `is_authorized` / `shutdown` / `peer_by_name` |
| 传输能力 | 公有 DERP 中继 ✅；直连 NAT 穿透"在多数情况下"（WIP） |
| **平台** | Linux x86_64/ARM64、macOS ARM64、Windows x86_64。❌ **Android / iOS / BSD 明确不支持** |
| 稳定性 | pre-1.0，**无向后兼容保证**，可能 break |

### 2.3 关键判断：两者不是同一个东西

| 维度 | croc/Tailcat（Go） | `tailscale` crate（Rust） |
|------|-------------------|--------------------------|
| 网络身份 | 临时匿名节点，无需账号 | 需要 auth key 加入 tailnet |
| 引导方式 | 自包含 CBOR offer（含 DERP region + 公钥） | 控制面 + DERP map |
| 授权 | PAKE 派生的一次性客户端密钥 | tailnet ACL |
| 互操作前提 | 需复刻 CBOR wire + DERP 客户端 + Noise/WireGuard 握手 + disco | 需要双方在同一 tailnet |

**结论**：用 `tailscale` crate **无法直接复刻** croc 的 Tailcat wire 协议。要 100% 与 Go 版在 tailcat
模式互通，等于要在 Rust 里重写 Tailcat 的匿名节点栈（DERP 客户端 + Noise 握手 + disco + CBOR
blob），工作量以"人月"计，且 `tailscale` crate 当前不暴露这些原语。

## 3. 现状

到今天为止实际落地的只有**两件事**：

1. `--transport` 收下 `auto` / `relay` / `derp` 三个值；`derp` 在 CLI 层被翻译成 `relay` 并打一条
   提示（文案见 [../cli.md](../cli.md) §1.4）。
2. `croc-protocol::features` 里的能力声明与 Go 的 `pakeFeatures()` 逐项一致——这决定了与 Go 互通时
   走哪条路（见 §5）。

**没有落地的**：

- 没有 `PlaneKind`、没有可替换的数据面 trait。`croc-engine::data::DataPlane` 是一个**具体类型**
  （中继连接的持有者），不提供 `kind()` / `fallback()` 之类的接口。
- 没有 `transport-select` / `tailcat-offer` / `tailcat-status` 的处理逻辑。
- 没有 `croc-tailscale` crate。

## 4. 待建的设计

真要接第二个数据面时，抽象大概长这样（**尚未实现**，写在这里是为了留下设计意图）：

```rust
/// 数据面：负责在 PAKE 之后搬运文件字节。协议层只认这个 trait。
pub trait DataPlane: Send + Sync {
    fn kind(&self) -> PlaneKind;                       // Relay | Local | Tailscale
    fn open(&self, streams: u8) -> PlaneFuture;        // 建立 N 条流
    fn fallback(&self) -> Option<PlaneKind>;           // 失败时该退到哪
}

pub struct TailscalePlane;                             // 占位

impl DataPlane for TailscalePlane {
    fn kind(&self) -> PlaneKind { PlaneKind::Tailscale }

    fn open(&self, _streams: u8) -> PlaneFuture {
        // 不用 todo!()：`clippy::todo` 是 warn，CI 开了 -D warnings。
        Box::pin(async { Err(PlaneError::Unavailable("tailscale data plane is pending")) })
    }

    fn fallback(&self) -> Option<PlaneKind> { Some(PlaneKind::Relay) }
}
```

注意 `DataPlane` 需要 `dyn` 对象安全（运行时按平台选择实现），所以这里得用 `BoxFuture` 或
`async-trait`；其它 trait 一律用原生 `async fn` in trait。

`--transport derp` 若已在 CLI 层被翻译成 `relay`，正常情况下根本不会构造 `TailscalePlane`；即使构造
了，也只是拿到 `Unavailable` 并走 `fallback()`，不会 panic。

## 5. 与 Go 版互通时的行为（**不会破坏兼容性**）

决定权在**能力声明**，不在数据面本身。Go 的本端列表由 `pakeFeatures()` 组装
（`src/croc/tailcat_negotiation.go:212`）：五项基础能力恒发，tailcat 相关的两项按本端支持情况追加。

本实现声明五项基础能力（其中包含 `implicit-tailcat-ready-v1`）、**不声明**
`experimental-tailcat-v1`。对端（包括官方 Go 客户端）据此认定本端不支持 tailcat，走 relay 数据面。
这与 croc 自身在 Android / `croc_no_tailcat` 构建下的行为完全一致
（`src/tailcattransport/transport_unsupported.go`），是协议内的既定降级路径，不是错误。

⚠️ 容易读反的一条：`implicit-tailcat-ready-v1` **不是**"本端支持 tailcat"，它是"对端可以不等显式
的就绪信号就开传"。详见 [../protocol.md](../protocol.md) §5.2。

**降级必须静默且可靠**：这是唯一的兼容性红线。

## 6. 风险

| ID | 风险 | 影响 | 对策 |
|----|------|------|------|
| T1 | `tailscale` crate pre-1.0，API 可能 break | 无（已挂起） | 不引入依赖；解除后锁 minor 并把调用收在单一 crate 内 |
| T2 | 抬高 MSRV | 无（已挂起） | 不引入依赖 |
| T3 | NAT 直连 WIP，吞吐可能差 | 中（解除后） | 解除后：`auto` 模式并行探测 relay 与 tailscale，先就绪者胜（Go 的 `staged-transport-v1` 同思路） |
| T4 | 上游平台面窄（无 Android/iOS） | 中（解除后） | 与 Go 版 build tag 行为一致，UI 明确提示 |
| T5 | 误以为"用了 tailscale 就能和 Go 版 tailcat 互通" | **高** | 本文即为澄清；互通用例钉死的是"降级到 relay 后成功"，不是 tailcat 直通 |
| T6 | 授权交互（auth key 从哪来） | 中（解除后） | GUI 设置页 authkey 输入 + 授权 URL；CLI `--ts-authkey` / `CROC_TS_AUTHKEY` |
| T7 | 有人误开 `TailscalePlane` | 低 | CLI 层就把 `derp` 翻译成 `Relay`；即使构造也只返回 `Unavailable` 并走 `fallback()` |

## 7. 解除 pending 后要做的

1. 建 `crates/croc-tailscale`，把 `tailscale` 依赖关在这一层里；实现 §4 的 `TailscalePlane`。
2. 按平台 feature 开关接入 `croc-engine` 的数据面选择。
3. `--transport` 的 `auto` 分支加上并行探测（relay 与 tailscale 谁先就绪用谁）。
4. CLI 加上 `--ts-authkey`（环境变量 `CROC_TS_AUTHKEY`）；GUI 设置页加 authkey 输入与授权 URL 跳转。
5. 补互通与单元测试：

| 场景 | 期望 |
|------|------|
| Rust `--transport derp` ↔ Go `--transport derp`（桌面） | 提示 pending，**降级 relay 后传输成功** |
| Rust `--transport derp` ↔ Rust（任一平台） | 同上 |
| Android 上 `--transport derp` | 同上，不崩溃 |
| 单元：数据面选择 | 在 pending 下恒返回 `Relay`，永不返回 `Tailscale` |
| 解除后新增：同 tailnet 直通、中途断开回退续传 | 一致 |

## 8. 平台能力表（GUI 设置页用）

| 平台 | Relay | Local | Tailscale |
|------|-------|-------|-----------|
| Windows / Linux / macOS | ✅ | ✅ | ⏸ pending |
| Android / iOS | ✅ | ✅ | ⏸ pending（上游亦不支持） |
| 其它 | ✅ | ✅ | ⏸ pending |

GUI 里该选项显示为灰色并注明"等待上游 tailcat-rs"。
