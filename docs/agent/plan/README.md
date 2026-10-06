# croc-rs 实施计划（文档索引）

> 目标：用 Rust 重写 croc（协议 100% 兼容 Go 版 croc v11.5.4），并另建一个 Tauri 2 GUI 版取代 FlCroc（Windows / macOS / Linux / Android）。
>
> 基线：`schollz/croc` v11.5.4（`v11.5.4-4-g62fc9d17`）。

## 文档导航

| 文档 | 内容 | 读者 |
|------|------|------|
| [00-overview.md](00-overview.md) | 项目定位、目标/非目标、设计原则、仓库与 crate 布局、与 croc / FlCroc 的关系 | 所有人（先看这篇） |
| [01-protocol.md](01-protocol.md) | croc v11 协议兼容基线：帧格式、relay 握手、PAKE 流程、消息类型、文件传输状态机、断点续传 | 核心开发者（必读） |
| [02-crypto.md](02-crypto.md) | 密码学移植方案：SIEC255 曲线、schollz/pake v3 wire 格式、PAKE v2 密钥派生、AES-GCM / Argon2 | 核心开发者（必读，最高风险） |
| [03-architecture.md](03-architecture.md) | Rust workspace 划分、模块职责、依赖选型、并发模型、错误与取消 | 核心开发者 |
| [04-cli-plan.md](04-cli-plan.md) | CLI 版（`croc-rs` 可执行程序）实施计划：命令/flag 对齐表、分阶段任务分解 | 核心开发者 |
| [05-gui-plan.md](05-gui-plan.md) | Tauri 2 GUI 版计划：取代 FlCroc 的功能对等表、command/event API、Android 适配 | GUI / 移动端开发者 |
| [06-milestones.md](06-milestones.md) | 路线图：P0–P8 里程碑、每阶段交付物与验收标准、发布与打包 | 所有人 |
| [07-risks-and-testing.md](07-risks-and-testing.md) | 风险登记册、互操作性测试矩阵、CI 与合规 | 所有人 |
| [08-tailscale.md](08-tailscale.md) | ⏸ **PENDING** — Tailscale / Tailcat 数据面调研与架构预留（等上游 tailcat-rs） | 想改传输层的人必读 |
| [09-build-and-ci.md](09-build-and-ci.md) | CI 四 workflow（release / build / lint / clear），两个产物 `croc-rs` 与 `crocw-rs`，通道与版本规则 | 发版与 CI 维护 |

## 一句话摘要

- **croc-rs（CLI）**：把 Go 版 croc 的 `src/` 逐个翻译为 Rust crate，线路层与 Go 版逐字节兼容，可与官方 `croc` 客户端互通、可连官方中继、可自建 Rust 中继。
- **croc-gui（Tauri 2）**：复用同一套 `croc-core`，前端 Web 技术栈重建 FlCroc 的 UI（发送 / 接收 / 历史 / 设置），Android 侧重点解决 SAF 存储、前台服务与后台传输。

## 已确认的关键决策

| 决策项 | 结论 | 依据 |
|--------|------|------|
| GUI 技术栈 | **Tauri 2**（Rust 核心 + Web 前端） | Rust 生态中唯一对桌面 + Android 都有正式支持的方案；后端逻辑与 CLI 共享核心 |
| 互通范围 | **全量兼容**（客户端 + 自建 Rust relay） | 见 [01-protocol.md](01-protocol.md) |
| 协议版本 | 对齐 croc v11.5.4：PAKE `ProtocolVersion = 2`、`ReconnectVersion = 1` | `src/pakekey`、`src/croc/croc.go` |
| 默认曲线 | peer PAKE 默认 `p256`；**relay 握手固定 `siec`（SIEC255）** | `src/tcp/tcp.go:406`、`src/cli/cli.go:210` |
| **实现风格** | **优雅优先于逐字一致**——仅实现层；线路字节仍是硬契约 | 见 [00-overview.md](00-overview.md) §4 |
| **架构范式** | **函数式：纯核心（sans-io）+ 副作用边缘 + 类型状态** | 见 [03-architecture.md](03-architecture.md) |
| **Tailscale** | ⏸ **PENDING**：架构抽象 + 降级协商 v1 就位（P1，零依赖）；实现挂起等上游 tailcat-rs。`--transport derp` = 提示后走 relay | 见 [08-tailscale.md](08-tailscale.md) |
| 工具链 | **Rust 2024 + stable**（MSRV 1.98）；nightly **仅用于 Miri** | 见 [03-architecture.md](03-architecture.md) §8.1 |
| 产物 | **`croc-rs`**（CLI）+ **`crocw-rs`**（GUI，w = window） | 见 [09-build-and-ci.md](09-build-and-ci.md) |
| CI | 四 workflow：`release`（唯一入口）/ `build` / `lint` / `clear`；**lint 不跟 PR，挂在发布链路上** | 对齐 Suwayomi-next，见 [09-build-and-ci.md](09-build-and-ci.md) |
| 异步运行时 | Tokio | 生态最全（socks5、组播、Tauri 兼容） |
| 许可证 | **GPL-3**（与 FlCroc 一致，同为本人项目，无授权障碍）；上游 MIT 署名搁置到打磨阶段 | 见 [07-risks-and-testing.md](07-risks-and-testing.md) §4 |
| 代码仓库 | 单仓库多 crate（workspace），GUI 在 `crates/croc-gui` + `app/` | 见 [03-architecture.md](03-architecture.md) |

## 阅读顺序建议

1. `00-overview.md` —— 建立整体认知（含设计原则）
2. `01-protocol.md` + `02-crypto.md` —— 动手前必须吃透（这两篇决定了能不能互通）
3. `03-architecture.md` —— 开始搭骨架（函数式分层）
4. `08-tailscale.md` —— 传输层的人必读（澄清 Tailcat ≠ tailscale crate）
5. `04-cli-plan.md` / `05-gui-plan.md` —— 按各自方向执行
6. `09-build-and-ci.md` —— 发版与 CI 维护
7. `06-milestones.md` / `07-risks-and-testing.md` —— 排期与验收

## 维护约定

- 本目录文档是**实施计划**，不是设计稿；每完成一个里程碑，回到 `06-milestones.md` 勾选并补充实测结论。
- 协议细节若与 Go 版实测不符，**以实测为准并立即修订本文档**，同时在 `07-risks-and-testing.md` 记录差异。
- 涉及密码学的任何改动必须先在 `02-crypto.md` 更新，再改代码。
