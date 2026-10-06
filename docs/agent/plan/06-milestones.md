# 06 · 路线图与验收

## 1. 阶段总览

```
P0 基建 + 黄金向量 ──┐
                     ├─> P1 线路层 + 数据面抽象 ──> P2 Peer PAKE ──> P3 单文件传输 ★MVP
                     │                                        │
                     └─> (croc-relay 与 P1 同步)               │
                                                              ▼
                                        P4 完整传输 <──────────┘
                                              │
                        ┌─────────────────────┼─────────────────────┐
                        ▼                     ▼                     ▼
                   P5 CLI 完善          P5' GUI 启动      P6 加固（Tailscale ⏸）
                        │                     │                     │
                        └──────────> P7 发布 <─────────────────────┘
                                          │
                                          ▼
                                    P8 Android 与多端收尾
```

⏸ Tailscale 数据面**不在关键路径上**：架构层在 P1 落地，实现层挂起（见 [08-tailscale.md](08-tailscale.md)）。

**强依赖**：P0 → P1 → P2 → P3 → P4。GUI（P5'）最早只能在 P3 之后启动，越晚启动返工越少。

## 2. 里程碑详情

### P0 · 基建与黄金向量
- **交付**：workspace 骨架；`croc-siec`、`croc-pake`、`pakekey` 三个模块 + 全套 golden 测试；`tests/golden/*.json`。
- **验收**：
  - `cargo test` 全绿，golden 向量 100% 匹配；
  - 交叉验证：Rust 生成的 PAKE wire 字节喂给 Go 版能解出**相同 session key**（`tests/interop/pake_cross`）。
- **退出条件**：没有这一条，后面全是猜测。

### P1 · 线路层 + 数据面抽象
- **交付**：帧层、`Comm`、代理、客户端中继握手、`croc-relay`、**`DataPlane` trait 与降级决策**（纯逻辑，此时不引入 tailscale 依赖）。
- **验收**：
  - Rust 客户端连 `croc.schollz.com:9009` 成功拿到 banner；
  - **Go 版客户端能连 Rust relay 完成握手**（双向验证）。
- **退出条件**：两个方向都通。

### P2 · Peer PAKE 与安全通道
- **交付**：消息层、peer PAKE 状态机、feature 协商、脱敏、错误分类。
- **验收**：Rust receiver ↔ Go sender 完成 PAKE 并拿到相同 `EncryptionKey`；反之亦然。

### P3 · 单文件传输（MVP ★）
- **交付**：`croc-rs send file.txt` / `croc-rs <code>` 能跑通。
- **验收**：
  - Rust → Go、Go → Rust 双向传输 1 MB / 100 MB 文件成功，哈希一致；
  - 中断（Ctrl-C）不会产生损坏的输出文件。
- **退出条件**：这是"项目可行"的证明点，也是 GUI 可以开工的信号。

### P4 · 完整传输
- **交付**：多文件/文件夹/空目录、`--zip`、压缩、四种哈希、覆盖策略、断点续传、多端口复用、限速、接收沙箱。
- **验收**：[04-cli-plan.md](04-cli-plan.md) §2 中 P4 全部子项通过；10 万文件清单不 OOM。

### P5 · CLI 完善
- **交付**：进度条、剪贴板、QR、`--text`/`--stdout`、本地发现、`--remember`、信号处理。
- **验收**：与 Go 版日常使用体验无显著差异；同一局域网下能走本地直连。

### P5' · GUI 启动（与 P5 并行）
- **交付**：Tauri 壳 + 发送/接收主流程（桌面）。
- **验收**：桌面三平台完成一次完整收发。

### P6 · 加固（Tailscale 数据面 ⏸ PENDING）
- **交付**：重连、超时治理、零拷贝优化、大清单分块、日志脱敏、次要消息补齐、tailcat 三消息的识别与降级。
- **验收**：kill 中继进程后传输能在 10 次内恢复；单核吞吐 ≥ 500 MB/s；Rust `--transport derp` ↔ Go `--transport derp` 提示 pending 后降级 relay 成功。
- **不做**：`croc-tailscale` crate 与 `tailscale` 依赖——⏸ pending，见 [08-tailscale.md](08-tailscale.md)。

### P7 · 发布
- **交付**：四个 workflow（`release` / `build` / `lint` / `clear`）、`xtask` 三个子命令、安装脚本、README 与差异说明。
- **验收**：推 main 自动出 alpha（含 `croc-rs` + `crocw-rs` 两产物与 checksum）；手动 dispatch 能出 beta/release；lint 门禁拦得住坏提交。

### P8 · Android 与多端收尾
- **交付**：Android APK（arm64 / x64）、前台服务、SAF、分享接入；GUI i18n 全量；历史记录；打包签名。
- **验收**：真机 500 MB 收发；切后台 10 min 存活；系统分享可发。

## 3. 并行建议

| 人员 | 主线 | 可并行 |
|------|------|--------|
| A（核心） | P0 → P1 → P2 → P3 | — |
| B（核心） | P0 完成后接 `croc-relay` + P4 的哈希/压缩 | 与 P1/P2 并行 |
| C（CLI） | P2 后接 CLI 外壳与进度/剪贴板 | 与 P3/P4 并行 |
| D（GUI） | P3 后启动 Tauri 壳 | 依赖 P3 |
| E（移动） | P4 后启动 Android 工程 | 依赖 P4 + G1 |

最小可行配置：2 人（核心 1 人 + GUI 1 人），GUI 人员在 P3 前做前端原型与 i18n/文案迁移准备。

## 4. 每个阶段的"完成定义"（DoD 模板）

- [ ] 该阶段所有子任务的代码已合入 `main`
- [ ] `cargo clippy -- -D warnings` 通过
- [ ] `cargo test` 通过（含新增 golden 向量）
- [ ] 互通测试脚本 `tests/interop/run.sh` 中对应场景通过
- [ ] 文档（本目录）已更新：新增的协议事实、与 Go 版的差异
- [ ] 性能未回退（跑一次基准并记录到 `docs/bench.md`）

## 5. 发布节奏

| 版本 | 内容 | 面向 |
|------|------|------|
| v0.1.0 | P0–P3，仅 CLI，标记 `experimental` | 内部验证 |
| v0.5.0 | + P4，CLI 功能完整 | 早期试用者 |
| v1.0.0 | + P5/P6，CLI 可替换 Go 版 | 公开 |
| v1.1.0 | + `crocw-rs` GUI 桌面版（取代 FlCroc 桌面端） | 公开 |
| v1.2.0 | + Android（取代 FlCroc Android 端） | 公开 |
| ~~v1.3.0~~ | ~~Tailscale 数据面~~ | ⏸ pending，等上游 tailcat-rs |
| v2.0.0 | store / ssh / Tailscale 数据面（若上游已稳定） | 待定 |

> 每个版本的产物均为 `croc-rs` 与 `crocw-rs` 两个，见 [09-build-and-ci.md](09-build-and-ci.md)。
