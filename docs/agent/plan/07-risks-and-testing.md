# 07 · 风险登记册与测试策略

## 1. 风险登记册

| ID | 风险 | 概率 | 影响 | 对策 | 触发信号 |
|----|------|------|------|------|---------|
| **R1** | **SIEC255 / pake v3 移植出现字节级偏差** | 高 | 致命（表现为"密码错误"） | P0 阶段先做 golden 向量；交叉验证程序（Rust 输出喂 Go）；把 `big.Int` 语义差异列成 checklist 逐条比对 | Rust↔Go 握手失败但双方都无报错 |
| **R2** | Go `big.Int.Bytes()` 的前导零/零值语义搞错 | 中 | 致命 | 单测覆盖 0、1、2^8、2^255-1 等边界；transcript 用 golden | session key 不一致 |
| **R3** | JSON 中 Unicode 字段名 / `null` / 大整数处理错误 | 中 | 致命 | 用 `arbitrary_precision`；写"Go 版输出 → Rust 解析"的 round-trip 测试 | 解析失败或静默丢字段 |
| **R4** | 中继握手的管线化顺序理解错误 | 中 | 高 | 抓包对照；Rust relay 与 Go relay 交叉互连 | 连官方 relay 超时 |
| **R5** | 断点续传的 chunk 区间语义复杂（三元组 flattened） | 中 | 中 | 与 Go 的 `utils.ChunkRanges*` 做 golden；大文件 kill 恢复测试 | 续传后文件损坏 |
| **R6** | 多端口数据连接的 room 命名 / 连接时序 | 中 | 中 | 对齐 `{room}-{j}`；抓包验证 | 只有一条连接生效 |
| **R7** | Android 后台被杀 / SAF 复杂度 | 高 | 中 | v1 走私有目录 + 前台服务；fd 通路延后 | 真机测试中断 |
| **R8** | 上游 croc 协议演进（v11.x 仍在加 feature） | 中 | 中 | 锁定基线 v11.5.4，README 声明兼容版本；CI 每夜跑互通测试，失败即告警 | 官方客户端升级后互通失败 |
| **R9** | 性能不达标（每块分配导致 GC/内存抖动） | 中 | 中 | 复用 buffer + `encrypt_in_place`；基准测试进 CI | 吞吐 < 100 MB/s |
| **R10** | 目录穿越 / symlink 等安全问题 | 低 | 高 | 接收根沙箱（对齐 `receivefs.Root`）+ 恶意清单模糊测试 | — |
| **R11** | Windows 上文件路径/权限/稀疏文件行为差异 | 中 | 中 | Windows 专项测试矩阵 | 接收文件损坏 |
| **R12** | Tauri 2 API 变动 | 中 | 低 | 锁定 minor；升级前跑冒烟 | 编译失败 |
| **R13** | 秘密泄露到日志/错误（room、secret） | 中 | 中 | 统一 `redact()` 出口；测试断言错误串中不含 secret | — |
| **R14** | 交叉编译 Android 时依赖引入 C 库 | 中 | 中 | `croc-core` 只用纯 Rust 加密栈；CI 加 `cargo build --target aarch64-linux-android` 门禁 | 链接失败 |
| **R15** | 误以为 `tailscale` crate 能直接复刻 Go 版 Tailcat wire，导致 v1 被拖住 | 高 | 高 | 调研已澄清（[08-tailscale.md](08-tailscale.md) §1.3）：模型不同。**整块标记 PENDING**，不引入依赖；仅保留架构层抽象 | 有人提议"先实现 tailcat 再发版" |
| **R16** | ~~`tailscale` crate pre-1.0 / MSRV 1.97~~ | 中 | 中 | **pending 后风险归零**：不引入依赖，本项目保持 Rust 2024 + stable 1.98 | — |
| **R17** | pending 期间误走 `TailscalePlane` | 低 | 低 | CLI 层即把 `derp` 翻译成 `Relay`；占位实现返回 `PlaneError::Unavailable` + `fallback() → Relay`，**无 panic 路径**；单测覆盖"选择器恒返回 Relay" | 出现 `Unavailable` 错误日志 |

**R1–R4 是"项目生死线"，必须在 P0/P1 阶段用交叉验证彻底排除。**

---

## 2. 测试策略

### 2.1 分层

| 层 | 范围 | 是否需要 Go 环境 | 运行频率 |
|----|------|------------------|---------|
| L0 纯函数测试 | `croc-protocol` / `croc-pake` / `croc-siec`：性质测试 + 事件序列重放 | 否 | 每次提交 |
| L1 单元测试 | 曲线算术、帧编解码、chunk 区间、codephrase、哈希 | 否 | 每次提交 |
| L2 Golden 测试 | `tests/golden/*.json` 向量比对 | 否（向量已入库） | 每次提交 |
| L3 交叉验证 | Rust 小程序 ↔ Go 小程序 交换 PAKE/消息字节 | **是** | 每次提交（可选，CI 装 Go） |
| L4 集成测试 | 本机起 Rust relay + Rust sender/receiver 互传 | 否 | 每次提交 |
| L5 互通测试 | Rust ↔ Go 客户端互传（含 Go relay） | **是** | 每次提交 + 每夜 |
| L6 真实环境 | 官方 relay、跨 NAT、弱网、kill 恢复 | 部分 | 每夜 / 手动 |
| L7 端到端 UI | Tauri 桌面/Android 真机 | 否 | 发布前 |

### 2.2 互通测试矩阵（`tests/interop/run.sh`）

| # | 发送方 | 接收方 | 中继 | 场景 | 期望 |
|---|--------|--------|------|------|------|
| I1 | Rust | Rust | Rust relay | 1 MB 文件 | 哈希一致 |
| I2 | Rust | Go | Rust relay | 1 MB | 一致 |
| I3 | Go | Rust | Rust relay | 1 MB | 一致 |
| I4 | Rust | Go | Go relay | 100 MB | 一致 |
| I5 | Go | Rust | Go relay | 100 MB | 一致 |
| I6 | Rust | Go | 官方 relay | 1 MB（需网络，标 `#[ignore]`） | 一致 |
| I7 | Rust | Rust **与** Go | Rust relay | 文件夹 + 空目录 + symlink | 两个方向的树结构都一致 |
| I8 | Rust | Go | Rust relay | 文本（`--text`） | 内容一致 |
| I9 | Rust | Go | Rust relay | 断点续传（传 50% 时 kill，重传） | 最终一致 |
| I10 | Rust | Go | Rust relay | 同名文件 → 覆盖 / 重命名 / 拒绝 | 三种策略行为一致 |
| I11 | Rust | Go | Rust relay | 压缩开启/关闭 | 一致 |
| I12 | Rust | Go | 局域网本地直连 | 不走中继 | ⏸ 未实现：`--local` 报错退出（见 [04](04-cli-plan.md) §1.5），harness 里显式 SKIP |
| I13 | Rust | Go | Rust relay | 10 万文件清单 | 不 OOM，一致 |
| I14 | Rust | Go | Rust relay | 错误的中继口令 | 接收端非零退出、无文件落地、双方不崩 |
| I15 | Rust | Go | Rust relay | 传输中对端 Ctrl-C | 优雅退出，无残留 .part 文件 |
| I16 | Rust `--transport derp` | Go `--transport derp` | Go relay | 桌面 | ⏸ pending：提示后**降级 relay 传输成功** |
| I17 | Rust `--transport derp` | Rust `--transport derp` | Rust relay | 桌面 | 同上（pending 期间不区分对端） |
| I18 | Rust `--transport derp` | Go | — | Android | 同上，不崩溃 |

### 2.3 Golden 向量的维护

- 向量由 `croc-playground/tools/gen-golden` 生成（Go 程序，**不在本仓库**；一次性，需 Go 环境）。
- 向量文件**只增不改**；若 Go 版行为变化，新增文件（`v2.json`）而不是覆盖，并在本文记录。
- 每次修改密码学相关代码，必须先跑 L2 再跑 L3。

### 2.4 模糊测试

- `cargo fuzz` 目标 1：帧解码器（随机字节流，不能 panic、不能 OOM）。
- 目标 2：消息解码器（随机 JSON + deflate）。
- 目标 3：文件清单解码（恶意路径 `../`、绝对链接、超长名、超深目录）。

### 2.5 性能基准（`docs/bench.md`）

| 场景 | 指标 |
|------|------|
| 纯加密（32 KB 块，AES-GCM） | MB/s per core |
| 局域网直连吞吐 | MB/s |
| 中继转发吞吐 | MB/s |
| 清单生成（10 万文件） | ms |
| 握手到安全通道就绪 | ms（含 SIEC255 标量乘） |

每次合入 `main` 后跑一次并记录，回退超过 15% 则阻断。

---

## 3. CI 设计

**四个 workflow**，对齐 Suwayomi-next 的组织方式（release 为唯一入口、lint 挂在发布链路上），
完整 yaml 见 [09-build-and-ci.md](09-build-and-ci.md)。要点：

| workflow | 触发 | 内容 |
|----------|------|------|
| `release.yml` | `push` main（paths-ignore 文档）+ `workflow_dispatch` | **唯一入口**：prep → lint → build → publish |
| `build.yml` | 仅 `workflow_call` | 全矩阵编译，产出 `croc-rs` 与 `crocw-rs`；不推导任何参数 |
| `lint.yml` | `workflow_call` + `workflow_dispatch`（**不跟 push/PR**） | fmt、clippy、test、`xtask purity`/`deps`、MSRV |
| `clear.yml` | 仅 `workflow_dispatch` | 预发布清理：每窗口只留最新 1 个 alpha |

> **质量门禁不跟 PR**：挂在 `build` 的 `needs` 上，门禁不过就不进编译——所有入口都从 `release.yml` 进，没有绕过路径。
> 代价是 PR 阶段没有绿灯，由本地 `cargo xtask ci-lint` 补。

工具链：**全仓只用 stable（Rust 2024，MSRV 1.98）。**

**互通矩阵只在本地跑**（`cargo xtask ci-interop`），**不进 `lint.yml`**。

这是有意的决定，理由是矩阵的形态而不是它的通过率：

- 它要求同一台机器上同时有 Rust 版、Go 版 croc 和两套运行时（Go 版在平级的
  `croc-playground/` 里）；
- 每个用例各自起一个中继、拉起两端进程、传文件，是**分钟级**的端到端作业；
- 它的用途是"改了协议字节之后本地跑一遍"，不是"挡住某个 commit"。挂进发布链路
  只会把一段慢且依赖外部产物的环节塞进 `build` 的 `needs`。

早先"不挂"的理由是"矩阵里有一批用例依赖尚未实现的能力，跑起来必然红"——那个理由
确实已经不成立（I9 与 I16–I18 均已通过，I12 由 `skip_unimplemented()` 显式 SKIP 并
打印理由）。**但结论没变，理由换了**：不是"挂了就恒红"，而是"挂了也不划算"。

> 踩过的坑：这份 harness 在真正跑通之前一直是**没被执行过**的（README 里那句"只在本地跑"
> 掩盖了它一次都没跑过的事实）。第一次真跑就暴露了三个 bug：`begin_case` 没建 `send/recv`
> 目录（`make_file` 直接 FileNotFound）、`[ ... ] || sender_status=$?` 把 `$?` 取成了 `[`
> 自己的退出码（发送端恒判失败）、以及中继密码根本没从 `--pass` 传进 `relay_cmd`。
> **教训：harness 也要有"第一次跑通"的记录，否则它只是看起来像门禁。**

> 踩过的坑：这份 harness 在真正跑通之前一直是**没被执行过**的（README 里那句"只在本地跑"
> 掩盖了它一次都没跑过的事实）。第一次真跑就暴露了三个 bug：`begin_case` 没建 `send/recv`
> 目录（`make_file` 直接 FileNotFound）、`[ ... ] || sender_status=$?` 把 `$?` 取成了 `[`
> 自己的退出码（发送端恒判失败）、以及中继密码根本没从 `--pass` 传进 `relay_cmd`。
> **教训：harness 也要有"第一次跑通"的记录，否则它只是看起来像门禁。**

### 3.1 本地必须能跑完等价检查

因为 lint 不跟 PR，`xtask` 是唯一能在推代码前发现红灯的手段：

```bash
cargo xtask ci-lint      # = lint.yml 全部 job
cargo xtask ci-build     # = build.yml 本机 target
cargo xtask ci-interop   # = 互通矩阵（只在本地跑，不在 CI 里）
```

---

## 4. 合规与许可

**本项目以 LGPL-3.0 发布**（`LGPL-3.0-only`）。选弱化许可证是为了让**库**能被
闭源程序链接使用，而 CLI 整体分发仍然受 copyleft 约束。FlCroc 与本项目同为本人作品，
因此复用其 i18n 文案与图标**不存在授权障碍**。

> 最初定的是 GPL-3（对齐 FlCroc），后改为 LGPL-3.0。改动只涉及 `LICENSE`、
> `Cargo.toml` 的 `license` 字段与文档措辞，不涉及代码。

| 项 | 说明 | 状态 |
|----|------|------|
| 本项目 | **LGPL-3.0-only**（弱化 copyleft，允许链接） | ✅ 已确定 |
| FlCroc 文案 / 图标复用 | 同为本人项目 | ✅ 无阻碍 |
| 上游 croc（MIT, © Schollz） | 独立实现而非 fork；协议与算法是事实标准，但**搬运的词表、常量点等素材需保留署名** | ⏸ **搁置到实现完成后打磨阶段统一处理**（届时需补 `THIRD_PARTY_NOTICES`，并注明协议来源与 MIT 声明） |
| `tscholl2/siec`（曲线参数） | 需在 `croc-siec` README 注明参数来源 | ⏸ 同上 |
| `schollz/pake` v3（算法移植） | 需在文档中说明来源 | ⏸ 同上 |
| EFF 词表 | 随文件附 `wordlists/LICENSE.txt` 一并搬运 | ⏸ 同上 |
| `tailscale/tailcat` wire（若解除 pending 后复刻） | BSD-3-Clause，需注明上游 commit `c04c5af` | ⏸ 解除 pending 时再说 |
| `tailscale` crate | 需确认其许可证后再引入（当前为 optional feature） | ⏸ 引入前确认 |
| Android 发布 | 上架商店需隐私政策（说明端到端加密、不上传数据） | 发布前 |

> 打磨阶段的统一动作：建 `THIRD_PARTY_NOTICES.md`，逐项补齐 MIT/BSD/CC 署名与来源链接。不必阻塞开发。
