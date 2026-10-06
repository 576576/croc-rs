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
| I7 | Rust | Rust | Rust relay | 文件夹 + 空目录 + symlink | 树结构一致 |
| I8 | Rust | Go | Rust relay | 文本（`--text`） | 内容一致 |
| I9 | Rust | Go | Rust relay | 断点续传（传 50% 时 kill，重传） | 最终一致 |
| I10 | Rust | Go | Rust relay | 同名文件 → 覆盖 / 重命名 / 拒绝 | 三种策略行为一致 |
| I11 | Rust | Go | Rust relay | 压缩开启/关闭 | 一致 |
| I12 | Rust | Go | 局域网本地直连 | 不走中继 | 一致 |
| I13 | Rust | Go | Rust relay | 10 万文件清单 | 不 OOM，一致 |
| I14 | Rust | Go | Rust relay | 错误密码 | 双方都报"密码错误"，不崩溃 |
| I15 | Rust | Go | Rust relay | 传输中对端 Ctrl-C | 优雅退出，无残留 .part 文件 |
| I16 | Rust `--transport derp` | Go `--transport derp` | Go relay | 桌面 | ⏸ pending：提示后**降级 relay 传输成功** |
| I17 | Rust `--transport derp` | Rust `--transport derp` | Rust relay | 桌面 | 同上（pending 期间不区分对端） |
| I18 | Rust `--transport derp` | Go | — | Android | 同上，不崩溃 |

### 2.3 Golden 向量的维护

- 向量由 `tools/gen-golden/*.go` 生成（一次性，需 Go 环境）。
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

互通测试是 `lint.yml` 里的一个 job（跑 I1–I5、I7–I15、I16–I18）：

- `actions/setup-go`，`go install github.com/schollz/croc/v11@v11.5.4`（或 `go build` 源码）；
- 起一个 Rust relay（本地端口），跑矩阵；
- 失败时保留双方日志（**注意先脱敏**）。

**互通矩阵目前只在本地跑**（`cargo xtask ci-interop`）。它没有进 `lint.yml`，因为矩阵里的
I12（`--local`）、I16–I18（`--transport derp`）、I9（断点续传）依赖尚未实现或尚未实测的
能力，跑起来必然红——一个恒红的门禁比没有门禁更糟。**矩阵全绿之后把它加回 `lint.yml`**，
在那之前它是本项目的头号缺口（见 [09](09-build-and-ci.md) §8 的检查清单）。

### 3.1 本地必须能跑完等价检查

因为 lint 不跟 PR，`xtask` 是唯一能在推代码前发现红灯的手段：

```bash
cargo xtask ci-lint      # = lint.yml 全部 job
cargo xtask ci-build     # = build.yml 本机 target
cargo xtask ci-interop   # = interop job（需本机 Go + croc 源码）
```

---

## 4. 合规与许可

**本项目以 GPL-3 初始，且 FlCroc 同为本人项目** —— 因此复用 FlCroc 的 i18n 文案与图标**不存在授权障碍**，按 GPL-3 一致处理即可。

| 项 | 说明 | 状态 |
|----|------|------|
| 本项目 | GPL-3（与 FlCroc 一致） | ✅ 已确定 |
| FlCroc 文案 / 图标复用 | 同为本人项目，GPL-3 下直接复用 | ✅ 无阻碍 |
| 上游 croc（MIT, © Schollz） | 独立实现而非 fork；协议与算法是事实标准，但**搬运的词表、常量点等素材需保留署名** | ⏸ **搁置到实现完成后打磨阶段统一处理**（届时需补 `THIRD_PARTY_NOTICES`，并注明协议来源与 MIT 声明） |
| `tscholl2/siec`（曲线参数） | 需在 `croc-siec` README 注明参数来源 | ⏸ 同上 |
| `schollz/pake` v3（算法移植） | 需在文档中说明来源 | ⏸ 同上 |
| EFF 词表 | 随文件附 `wordlists/LICENSE.txt` 一并搬运 | ⏸ 同上 |
| `tailscale/tailcat` wire（若解除 pending 后复刻） | BSD-3-Clause，需注明上游 commit `c04c5af` | ⏸ 解除 pending 时再说 |
| `tailscale` crate | 需确认其许可证后再引入（当前为 optional feature） | ⏸ 引入前确认 |
| Android 发布 | 上架商店需隐私政策（说明端到端加密、不上传数据） | 发布前 |

> 打磨阶段的统一动作：建 `THIRD_PARTY_NOTICES.md`，逐项补齐 MIT/BSD/CC 署名与来源链接。不必阻塞开发。
