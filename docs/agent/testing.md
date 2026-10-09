# 测试与合规

## 1. 测试分层

| 层 | 范围 | 需要 Go 环境 | 运行频率 |
|----|------|--------------|---------|
| L0 纯函数测试 | `croc-protocol` / `croc-pake` / `croc-siec` / `croc-core`：性质测试 + 事件序列重放 | 否 | 每次提交 |
| L1 单元测试 | 曲线算术、帧编解码、分片区间、codephrase、哈希 | 否 | 每次提交 |
| L2 Golden 测试 | `tests/golden/`（协议向量 + 帮助输出） | 否（向量已入库） | 每次提交 |
| L3 交叉验证 | Rust ↔ Go 交换 PAKE / 消息字节 | **是** | 随互通矩阵 |
| L4 集成测试 | 本机起 Rust relay + Rust sender/receiver 互传 | 否 | 每次提交 |
| L5 互通测试 | Rust ↔ Go 客户端互传（含 Go relay） | **是** | 本地 `cargo xtask ci-interop` |
| L6 真实环境 | 官方 relay、跨 NAT、弱网、kill 恢复 | 部分 | 手动 |
| L7 端到端 UI | Tauri 桌面 / Android 真机 | 否 | 发布前 |

事件序列重放是 L0 里最有价值的一环：`croc_core::Session` 是 sans-io 的，所以一次完整握手与清单
交换可以在测试里靠喂 `InEvent` 走完，**不需要 socket，也不需要 sleep**。

## 2. 互通矩阵（`tests/interop/run.sh`）

`cargo xtask ci-interop` 跑下面的矩阵。它**不在 CI 里**，理由见
[build-and-ci.md](build-and-ci.md) §5.3。

| # | 发送方 | 接收方 | 中继 | 场景 | 期望 |
|---|--------|--------|------|------|------|
| I1 | Rust | Rust | Rust relay | 1 MB 文件 | 哈希一致 |
| I2 | Rust | Go | Rust relay | 1 MB | 一致 |
| I3 | Go | Rust | Rust relay | 1 MB | 一致 |
| I4 | Rust | Go | Go relay | 100 MB | 一致 |
| I5 | Go | Rust | Go relay | 100 MB | 一致 |
| I6 | Rust | Go | 官方 relay | 1 MB（需网络） | 一致 |
| I7 | Rust | Rust **与** Go | Rust relay | 文件夹 + 空目录 + symlink | 两个方向的树结构都一致 |
| I8 | Rust | Go | Rust relay | 文本（`--text`） | 内容一致 |
| I9 | Rust | Go | Rust relay | 断点续传（传 50% 时 kill，重传） | 最终一致 |
| I10 | Rust | Go | Rust relay | 同名文件 → 覆盖 / 重命名 / 拒绝 | 三种策略行为一致 |
| I11 | Rust | Go | Rust relay | 压缩开启/关闭 | 一致 |
| I12 | Rust | Go | 局域网本地直连 | 不走中继 | ⏸ 单机上 Go 侧发现不到（`AllowSelf=false` 会丢掉源 IP 是本机的广播，见 [cli.md](cli.md) §2.3），harness 里显式 SKIP |
| I13 | Rust | Go | Rust relay | 10 万文件清单 | 不 OOM，一致 |
| I14 | Rust | Go | Rust relay | 错误的中继口令 | 接收端非零退出、无文件落地、双方不崩 |
| I15 | Rust | Go | Rust relay | 传输中对端 Ctrl-C | 优雅退出，无残留 .part 文件 |
| I16 | Rust `--transport derp` | Go `--transport derp` | Go relay | 桌面 | 提示 pending 后**降级 relay 传输成功** |
| I17 | Rust `--transport derp` | Rust `--transport derp` | Rust relay | 桌面 | 同上 |
| I18 | Rust `--transport derp` | Go | — | Android | 同上，不崩溃 |
| I19 | Rust | Rust | 局域网本地直连 | 不走中继 | 内容一致（不经 Go 的组播发现，单机能跑） |
| I20 | Rust `--hash md5` | Go | Rust relay | 接收方本地已有同内容文件 | 发送方报 **0 字节**——Go 拿自己算的 md5 认定"不需要传"，即摘要跨实现一致 |
| I21 | Rust | Go `--curve ed25519` | Rust relay | — | 内容一致。曲线由**接收方**决定，`--curve` 因此给在 Go 一侧；ed25519 的点编码与另外四条不同（点整体塞在 `x` 坐标里、`y` 恒为 0） |

harness 自身的要求：**每个用例必须显式收尾**（停中继、清理进程）。漏掉一处，残留进程会持有继承的
文件描述符，让矩阵看起来"卡死"。做法与踩过的坑记在 `tests/interop/README.md`。

## 3. Golden 向量

向量由 **Go 版实际生成**，统一放在 `tests/golden/`：

| 文件 | 内容 | 消费者 |
|------|------|--------|
| `curves.json` | 点加 / 倍乘 / 标量乘 | `crates/croc-siec/tests/golden.rs` |
| `pake.json` | 各曲线的 `InitCurve` → `Bytes()`、`Update` 后的 `SessionKey()` | `crates/croc-pake/tests/golden.rs` |
| `pakekey.json` | `(room, secret, curve, 两端的 wire, salt) → (encKey, confA, confB)` | `crates/croc-pakekey/tests/golden.rs` |
| `store.json` | `croc-store-v1` 的加密格式：HKDF 派生、分片 AAD、清单 JSON、Share 的两种编码 | `crates/croc-store/tests/golden.rs` |
| `help/*.txt` | 各页 `-h` 的英文输出，逐字取自 Go 版（只有程序名换成 `croc-rs`、版本号留成 `<VERSION>`） | `crates/croc-cli/src/help.rs` 的测试 |

维护约定：

- 生成器是 `croc-playground/tools/gen-golden`（Go 程序，**不在本仓库**，一次性，需要 Go 环境）。
  `help/` 那一组不一样：它们是 `croc -h` 的输出直接抄下来的，生成脚本见
  [tests/golden/help/README.md](../../tests/golden/help/README.md)。
- 向量文件**只增不改**。Go 版行为变了就新增文件，并在本文记录。
- 每次改动密码学相关代码，先跑 L2，再跑 L5。
- 涉及密文的向量在**固定随机源**下产出，所以可以重算并直接比对；只要密钥派生、AAD、JSON 字段名
  或时间格式有一处偏差，比对就会红。

## 4. [PENDING] 尚未建立

- **模糊测试**（`cargo fuzz`）：帧解码器、消息解码器、清单解码（恶意路径 / 超长名 / 超深目录）
  三个目标都还没有。
- **性能基准**（`criterion`，结果落 `docs/bench.md`）：还没有。

## 5. 合规与许可

**本项目以 `LGPL-3.0-or-later` 发布**。选弱化许可证是为了让**库**能被闭源程序链接使用，而 CLI 整体
分发仍受 copyleft 约束；`-or-later` 让下游可以采用后续版本的 LGPL，不必等本项目改声明。

| 项 | 说明 | 状态 |
|----|------|------|
| 本项目 | **LGPL-3.0-or-later**（弱化 copyleft，允许链接） | ✅ 已确定 |
| 上游 croc web（`croc/web`，MIT © Schollz） | GUI 以它的 UI 为基准，会照搬 `styles.css` 与组件结构。MIT 的保留条件要求在分发物里带上版权声明与许可文本，落点是 `THIRD_PARTY_NOTICES.md` 与 `ui/src/styles.css` 的头部注释 | ⏸ 随 GUI 里程碑 G0 一并补齐 |
| 上游 croc（MIT, © Schollz） | 独立实现而非 fork；协议与算法是事实标准，但**搬运的词表、常量点等素材需保留署名** | ⏸ 搁置到打磨阶段统一处理 |
| `tscholl2/siec`（曲线参数） | 需在 `croc-siec` README 注明参数来源 | ⏸ 同上 |
| `schollz/pake` v3（算法移植） | 需在文档中说明来源 | ⏸ 同上 |
| EFF 词表 | 需随文件附 `wordlists/LICENSE.txt` | ⏸ 同上 |
| Android 发布 | 上架商店需隐私政策（说明端到端加密、不上传数据） | 发布前 |

> 打磨阶段的统一动作：建 `THIRD_PARTY_NOTICES.md`，逐项补齐 MIT / BSD / CC 署名与来源链接。
> 不必阻塞开发。
