# 08 · Tailscale / Tailcat 数据面规划（⏸ PENDING）

> ## 状态：**PENDING — 等待 Tailscale 团队稳定 tailcat-rs**
>
> 本文的调研结论与架构预留**长期有效**，但**不进入 v1 实现排期**。
> 在 upstream 提供可用的 tailcat Rust 实现之前：
> - **不引入 `tailscale` crate 依赖**（连 optional 也不加，避免污染 MSRV 与 CI）；
> - `DataPlane` 抽象、`PlaneKind`、feature 协商与降级路径**照常实现**（纯逻辑，零依赖）；
> - `TailscalePlane` 保留为**未实现的接口预留**，代码里只有一个返回 `PlaneError::Unavailable` 的占位实现（**不用 `todo!()`**，因为 `clippy::todo = "warn"` + CI `-D warnings`）；
> - `--transport derp` 参数接受，行为 = 打一条 `pending` 提示后按 relay 处理。
>
> 解除 pending 的条件：upstream 发布稳定的 tailcat Rust 实现 → 回到本文 §3 的落地表执行。

---

## 0. 为什么是 pending

- 用户要求"一开始统一考虑，不拖到 v2"——已满足：**架构层（数据面抽象 + 降级协商）在 P1 就落地**，不依赖任何外部 crate。
- 但**实现层**强行推进会引入 pre-1.0、MSRV 1.97、无兼容保证、且模型不匹配的依赖，收益不抵成本。
- 因此：架构就位，实现挂起，等 upstream。

---

## 1. 调研结论

### 1.1 croc 的 `--transport derp` 到底是什么

不是"直接用 DERP 协议"，而是 **Tailcat**：

| 事实 | 出处 |
|------|------|
| croc vendored 了 `tailscale/tailcat` 的网络与 wire 层（commit `c04c5af`，2026-08-27，BSD-3-Clause） | `internal/tailcat/UPSTREAM.md` |
| 传输时启动一个**内嵌的、匿名的 Tailscale 节点**（不需要 tailnet 账号 / auth key） | `internal/tailcat/tailcat.go` |
| 节点身份是临时生成的 node key；**offer 是 CBOR 编码的 `ConnBlob`**，含 `ServerPublic`（节点公钥）与 DERP region 描述 | `internal/tailcat/wire.go` |
| offer 大小上限 4 KB，经 `tailcat-offer` 消息传给对端；对端用 `tailcat-status` 回报，再用 `transport-select` 定选 | `src/croc/tailcat_negotiation.go` |
| 数据面是 tailnet 上的 1–16 条 TCP 流（`FirstTCPPort = 40000`） | `src/tailcattransport/config.go` |
| 授权绑定：用 croc 的 PAKE session key 派生一次性客户端密钥（`croc_authorization.go`） | `internal/tailcat/UPSTREAM.md` |
| Go 版仅 `linux/windows/darwin/freebsd/openbsd` 编译（build tag `!croc_no_tailcat && …`），**Android 不支持** | `src/tailcattransport/bundle_supported.go:1` |

### 1.2 Tailscale 官方 Rust crate

| 项 | 结论 |
|----|------|
| crate | **`tailscale`**（crates.io，当前 0.6） |
| 仓库 | `github.com/tailscale/tailscale-rs`（2026-04 公开发布，WIP） |
| 模型 | **加入一个已存在的 tailnet**：`Device::new(&Config, Option<auth_key>)` |
| 关键 API | `tcp_listen(addr) -> netstack::TcpListener`、`tcp_connect(addr) -> netstack::TcpStream`、`udp_bind`、`ipv4_addr`/`ipv6_addr`、`is_authorized`、`shutdown(timeout)`、`peer_by_name` |
| 传输能力 | 公有 DERP 中继 ✅；直连 NAT 穿透"在多数情况下"（WIP） |
| **平台** | Linux x86_64/ARM64、macOS ARM64、Windows x86_64。❌ **Android / iOS / BSD 明确不支持** |
| MSRV / edition | 1.97 / Rust 2024（滚动 MSRV） |
| 稳定性 | pre-1.0，**无向后兼容保证**，可能 break |

### 1.3 关键判断：两者不是同一个东西

| 维度 | croc/Tailcat（Go） | `tailscale` crate（Rust） |
|------|-------------------|--------------------------|
| 网络身份 | 临时匿名节点，无需账号 | 需要 auth key 加入 tailnet |
| 引导方式 | 自包含 CBOR offer（含 DERP region + 公钥） | 控制面 + DERP map |
| 授权 | PAKE 派生的一次性客户端密钥 | tailnet ACL |
| 互操作前提 | 需复刻 CBOR wire + DERP 客户端 + Noise/WireGuard 握手 + disco | 需要双方在同一 tailnet |

**结论**：用 `tailscale` crate **无法直接复刻** croc 的 Tailcat wire 协议。要 100% 与 Go 版在 tailcat 模式互通，等于要在 Rust 里重写 Tailcat 的匿名节点栈（DERP 客户端 + Noise 握手 + disco + CBOR blob），工作量以"人月"计，且 `tailscale` crate 当前不暴露这些原语。

---

## 2. 因此采用的策略（三数据面 + 优雅降级）

```
                  ┌──────────────────────────────────────────┐
   --transport    │  auto（默认）                             │
                  │    ├─ 同一局域网 → Local（直连/本地中继）   │
                  │    ├─ 同 tailnet 且双方支持 → Tailscale   │
                  │    └─ 否则 → Relay                        │
                  ├──────────────────────────────────────────┤
                  │  relay  → 强制 Relay                      │
                  │  derp   → ⏸ pending：提示后按 Relay 处理   │
                  └──────────────────────────────────────────┘
```

**pending 期间的实际行为**（全部平台一致，含桌面）：

```
$ croc-rs send --transport derp file.txt
note: tailscale data plane is not available yet (pending upstream tailcat-rs); using relay
```

### 2.1 与 Go 版互通时的行为（**不会破坏兼容性**）

- Rust 侧**默认不声明** `implicit-tailcat-ready-v1` feature → Go 侧走 relay。这与 croc 自身在 Android / `croc_no_tailcat` 构建下的行为完全一致（`transport_unsupported.go`）。
- 若未来实现了 Tailcat wire，则开启该 feature，用 `tailcat-offer` / `tailcat-status` / `transport-select` 三消息协商；失败时 `selectRelayAfterTailcatFailure` 回退（Go 版已有此路径）。
- **降级必须静默且可靠**：这是唯一的兼容性红线。

### 2.2 解除 pending 后的做法（**现在不实现**）

双方都在同一 tailnet 时走 `TailscalePlane`：
- 用 tailcat-rs（或 `tailscale::Device`）在 tailnet IP 上监听，把地址通过**自定义 feature 标记**（如 `croc-tailscale-v1`）放进 `Features` / 消息 `Bytes`；
- 对端不识别该 feature 则回退 relay —— 与 Go 版互通时永远走 relay；
- 好处：NAT 穿透强于自建中继，且不需要公网服务器。

---

## 3. 分阶段落地（pending 版）

| 阶段 | 内容 | 状态 |
|------|------|------|
| **P1（v1 必须）** | `DataPlane` trait、`PlaneKind`、feature 协商、`transport-select` 处理、降级路径 | ✅ **照常做**，纯逻辑，落在 `croc-protocol`，零外部依赖 |
| ~~P6~~ | ~~`crates/croc-tailscale` 封装 `tailscale` 0.6~~ | ⏸ **挂起** |
| ~~v1.3~~ | ~~发布含 Tailscale 数据面的版本~~ | ⏸ **挂起** |
| **解除后** | 按 §3.1 引入依赖、实现 `TailscalePlane`、补 I16–I18 测试 | 待 upstream |

### 3.1 pending 期间的代码形态

**不建 `croc-tailscale` 目录**，只在 `croc-core` 里留一个占位：

**注意**：`[workspace.lints]` 里 `clippy::todo = "warn"`（对齐 Suwayomi-next），CI 又是 `-D warnings`，
所以占位**不能写 `todo!()`**，改为返回一个错误值：

```rust
// crates/croc-core/src/plane/tailscale.rs
//! ⏸ PENDING — 等待 upstream tailcat-rs 稳定。设计见 docs/agent/plan/08-tailscale.md

pub struct TailscalePlane;

impl DataPlane for TailscalePlane {
    fn kind(&self) -> PlaneKind { PlaneKind::Tailscale }

    fn open(&self, _streams: u8) -> PlaneFuture {
        // 不用 todo!()：clippy::todo 是 warn，CI 开了 -D warnings。
        Box::pin(async { Err(PlaneError::Unavailable("tailscale data plane is pending")) })
    }

    fn fallback(&self) -> Option<PlaneKind> { Some(PlaneKind::Relay) }
}
```

`--transport derp` 在 CLI 层就被翻译成 `Relay` 并打一条 `note:`，**正常情况下根本不会构造 `TailscalePlane`**；
即使构造了，也只是拿到 `Unavailable` 错误并走 `fallback()` → `Relay`，不会 panic。
`xtask purity` / `cargo check --all-features` 都不会碰到 tailscale 依赖。

### 3.2 平台能力表（GUI 设置页，pending 期间）

| 平台 | Relay | Local | Tailscale |
|------|-------|-------|-----------|
| Windows / Linux / macOS | ✅ | ✅ | ⏸ pending |
| Android / iOS | ✅ | ✅ | ⏸ pending（上游亦不支持） |
| 其它 | ✅ | ✅ | ⏸ pending |

GUI 里该选项显示为灰色并注明"等待上游 tailcat-rs"。

---

## 4. 风险与对策

| ID | 风险 | 影响 | 对策 |
|----|------|------|------|
| T1 | `tailscale` crate pre-1.0，API 可能 break | ~~中~~ → 无 | **pending：不引入依赖**，风险归零；解除后锁 minor 并把调用收在单一 crate 内 |
| T2 | MSRV 1.97 抬高工具链要求 | ~~中~~ → 无 | **pending：不引入依赖**；本项目保持 Rust 2024 + stable 1.98 |
| T3 | NAT 直连 WIP，吞吐可能差 | 中（解除后） | 解除后：`auto` 模式并行探测 relay 与 tailscale，500 ms 内先就绪者胜（Go 版 `staged-transport-v1` 同思路） |
| T4 | 上游平台面窄（无 Android/iOS） | 中（解除后） | 与 Go 版 build tag 行为一致，UI 明确提示 |
| T5 | 误以为"用了 tailscale 就能和 Go 版 tailcat 互通" | **高** | 本文即为澄清；CI 用例钉死"降级到 relay 后成功"，而非 tailcat 直通 |
| T6 | 授权交互（auth key 从哪来） | 中（解除后） | GUI 设置页 authkey 输入 + 授权 URL；CLI `--ts-authkey` / `CROC_TS_AUTHKEY` |
| T7 | pending 期间有人误开 `TailscalePlane` | 低 | CLI 层就把 `derp` 翻译成 `Relay`；即使构造也只返回 `Unavailable` 并走 `fallback() → Relay`，不会 panic；单测覆盖"选择器恒返回 `Relay`" |

---

## 5. 测试（pending 期间）

| 场景 | 期望 |
|------|------|
| Rust `--transport derp` ↔ Go `--transport derp`（桌面） | 提示 pending，**降级 relay 后传输成功** |
| Rust `--transport derp` ↔ Rust（任一平台） | 同上 |
| Android 上 `--transport derp` | 同上，不崩溃 |
| 单元：选择数据面 | `select_plane(...)` 在 pending 下恒返回 `Relay`，永不返回 `Tailscale` |

解除 pending 后再补：同 tailnet 直通、中途断开回退续传。

---

## 6. 一句话总结

> **架构层（数据面抽象 + feature 协商 + 降级）在 P1 就落地，零依赖、不影响 v1 交付；
> 实现层标记为 PENDING，等待 Tailscale 团队稳定 tailcat-rs 后再启用。**
> pending 期间 `--transport derp` 表现为"提示 + 走 relay"，与 Go 版在 Android / `croc_no_tailcat` 下的行为一致。
