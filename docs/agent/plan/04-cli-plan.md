# 04 · CLI 版实施计划

> 产物：`croc-rs`（可执行程序）。目标是可直接替换 Go 版 `croc` 的日常使用。

---

## 1. 命令与 flag 对齐表

### 1.1 顶层命令

| Go 命令 | Rust v1 | 说明 |
|---------|---------|------|
| `croc <code>`（默认动作 = 接收） | ✅ | 位置参数即三词码 |
| `croc send [flags] <files...>` | ✅ | |
| `croc relay` | ✅ | 自建中继 |
| `croc --version` / `-v` | ✅ | |
| `croc update` | ⚠️ 仅 `--check` | 自更新放到 v1.1（需要签名基础设施） |
| `croc store` | ❌ v2 | 依赖 store 服务端 |
| `croc ssh` | ❌ v2 | PTY + tailcat |
| `croc generate-fish-completion` | ⚠️ | clap_complete 生成 bash/zsh/fish |

### 1.2 `send` 命令 flag

| Go flag | Rust | 备注 |
|---------|------|------|
| `--code, -c` | ✅ | 自定义三词码 |
| `--zip` | ✅ | 文件夹打包 |
| `--text, -t` | ✅ | 发送文本 |
| `--hash` (xxhash/imohash/md5/highway) | ✅ | 默认 xxhash |
| `--transport` (auto/derp/relay) | ✅ `auto`/`relay` 等价（还没有局域网中继）；`derp` ⏸ pending（提示后按 relay） | 见 §1.4 |
| `--ts-authkey` | ⏸ pending | 解除 pending 后启用，环境变量 `CROC_TS_AUTHKEY` |
| `--no-local` | ✅ | |
| `--no-multi` | ✅ | 禁用多端口复用 |
| `--git` | ✅ | 尊重 .gitignore |
| `--port` / `--transfers` | ✅ | 本地中继/端口数 |
| `--qrcode, -qr` | ✅ | |
| `--exclude` / `--exclude-file` | ✅ | |
| `--socks5` / `--connect` | ✅ | |
| `--store` | ❌ v2 | |

### 1.3 全局 flag

| Go flag | Rust | 备注 |
|---------|------|------|
| `--yes` | ✅ | |
| `--stdout` | ✅ | |
| `--no-compress` | ✅ | |
| `--ask` | ✅ | |
| `--local` | ⏸ v1.1 | 参数已接受，但**报错退出**：还没有内建局域网中继。见 §1.5 |
| `--ignore-stdin` | ✅ | |
| `--overwrite` | ✅ | |
| `--rename` | ✅ | |
| `--quiet` | ✅ | |
| `--debug` | ✅ | |
| `--remember` | ✅ | 配置落盘（对齐 `internal/cli` 的 remembered 设置） |
| `--classic` | ⚠️ | v1 保留参数但仅记录（classic 模式的不安全语义不实现） |
| `--internal-dns` | ⚠️ v1.1 | 内置 DNS stub resolver |
| `--disable-clipboard` / `--extended-clipboard` | ✅ | |
| `--multicast` | ✅ | |
| `--curve` (默认 p256) | ✅ | 支持 p256/p384/p521/siec；`ed25519` 报不支持 |
| `--ip` | ✅ | |
| `--relay` / `--relay6` | ✅ | 环境变量 `CROC_RELAY` / `CROC_RELAY6` |
| `--out` | ✅ | 接收目录 |
| `--pass` | ✅ | 环境变量 `CROC_PASS` |
| `--throttleUpload` | ✅ | 限速，如 `500k` |
| `--revoke` | ❌ v2 | 依赖 store |

### 1.4 `--transport derp` —— ⏸ PENDING

Tailscale 数据面**标记为 pending，等待 Tailscale 团队稳定 tailcat-rs**。完整方案见 [08-tailscale.md](08-tailscale.md)。

pending 期间 CLI 的行为（**所有平台一致**）：

```
$ croc-rs send --transport derp file.txt
note: tailscale data plane is not available yet (pending upstream tailcat-rs); using relay
```

| 输入 | 实际行为 |
|------|---------|
| `--transport auto` | 局域网 → Local；否则 → Relay（**永远不选 Tailscale**） |
| `--transport relay` | Relay |
| `--transport derp` | 打一条 `note:` 后按 Relay 处理，**不是错误、不影响退出码** |

依据：Go 版 `resolveTransportMode()` 在 `tailcattransport.Available() == false` 时降级为 `relay` 并返回 `downgraded=true`（`src/croc/croc.go:160-165`），`transport_unsupported.go` 在 Android 上返回 `ErrUnsupported`。我们的 pending 行为与之等价——**对端看不出区别**。

解除 pending 后按 [08-tailscale.md](08-tailscale.md) §3 执行，届时 CLI 侧新增 `--ts-authkey`。

### 1.5 `--local` —— ⏸ v1.1（参数已接受，但报错退出）

`--local` 在 Go 里的语义是 `OnlyLocal`：**只走局域网，不碰公网中继**。它需要两件事：

1. 发送端起一个**内嵌中继**（`127.0.0.1:<随机端口>`，控制口 + 数据口）；
2. 接收端用 **UDP 组播**（默认 `239.255.255.250`）发现这个端口——载荷是 `"croc" + 端口`
   （Go 版 `broadcastOnLocalNetwork()`，`src/croc/croc.go:1438`）。

两者都还没有。当前的 CLI 行为是**报错退出**：

```
$ croc-rs --local aaa-bbb-ccc
croc-rs: --local（仅局域网）尚未实现：本实现还没有内建局域网中继；请去掉该参数，或改用 --no-local 显式走中继
```

**为什么报错而不是"接受但忽略"**：`--local` 是个隐私开关——用户明确要求不经过公网中继。
把它悄悄降级成公网中继，等于把用户拒绝的事做了，比直接失败更糟。

配套的两条规则：

| 组合 | 行为 |
|------|------|
| `--local --transport auto` | 报错（尚未实现） |
| `--local --transport relay` / `derp` | 报错 `--transport must be auto for local-only transfers`（**与 Go 同文**，`src/cli/cli.go:700`） |

顺带：对端的局域网探测我们**只吃掉、不回**。探测是裸 JSON（`SimpleMessage`），不认出来
就会被当 `Message` 去解压，报 `corrupt deflate stream`；但**回**一个过期的 `pake2` 更糟——
对端只等 500 ms（`localProbeResponseTimeout`），超时后它已经往下走、在等 `pake` 的应答，
这时收到 `pake2` 会拿去解压，握手当场报废。发送方"算完哈希才连中继"，几万文件的哈希在
Windows 上要十几秒，那 500 ms 必然过完。沉默则永远安全：Go 的接收方把探测失败丢掉、
照常走中继路径，代价只是多等 500 ms——与 Go 自己加 `--no-local` 时一样。
见 `crates/croc-core/src/session.rs` 的 `try_local_probe()`。

> 将来做本地中继时**必须同时**让发送方尽早进入读循环（Go 是把算哈希放进后台 goroutine、
> 与连中继并行，`croc.go:1792`），否则回过去的仍然是过期答案。

---

## 2. 分阶段任务分解

### P0 · 基建与黄金向量（先做，做完才能往下）

| # | 任务 | 产出 | 验收 |
|---|------|------|------|
| C0.1 | 初始化 workspace，定 crate 骨架，CI 跑 `cargo fmt/clippy/test` | 空 crate 编译通过 | CI 绿 |
| C0.2 | 写 `tools/gen-golden/*.go`，抓取 Go 版曲线/PAKE/codephrase 向量 | `tests/golden/*.json` | 向量文件入库 |
| C0.3 | 抓一次完整中继握手 + 一次 1 KB 传输的 pcap | `tests/golden/*.bin` | 可用脚本回放 |
| C0.4 | `croc-siec`：SIEC255 + p256/p384/p521 算术 | crate | golden 全绿 |
| C0.5 | `croc-pake`：曲线常量点、Init/Update/SessionKey、wire JSON | crate | golden 全绿（**含 wire 字节级比对**） |
| C0.6 | `croc-core::crypto::pakekey`：frame/Identities/Derive/Confirm | 模块 | golden 全绿 |

> **C0.5 的验收必须是"Rust 生成的 `Bytes()` 喂给 Go 版能解出同样的 session key"**，而不只是自测通过。建一个 `tests/interop/pake_cross/` 用两个小 CLI 互相验证。

### P1 · 线路层

| # | 任务 | 验收 |
|---|------|------|
| C1.1 | `protocol::frame`：magic + u32 LE，读写 deadline，keepalive 过滤 | 单测 + 与 pcap 比对 |
| C1.2 | `transport::conn::Comm`：vectored write、`read_exact`、错误分类 | 单测 |
| C1.3 | `transport::proxy`：SOCKS5 / HTTP CONNECT | 用本地 socks 代理实测 |
| C1.4 | `protocol::handshake`：客户端中继握手（管线化 room 帧） | **连官方 `croc.schollz.com:9009` 成功拿到 banner** ★ |
| C1.5 | `croc-relay`：房间管理、pipe、限流、超时 | **Go 版客户端能连上 Rust 中继并完成握手** |
| C1.6 | `protocol::plane`：`DataPlane` trait、`PlaneKind`、feature 协商与降级决策（纯逻辑） | 单测覆盖"平台 × 对端能力"全组合的选择结果；此时**不引入 tailscale 依赖** |

> C1.4 与 C1.5 是第一个"能证明方向正确"的里程碑。这两个必须同时做——只做客户端无法排除服务端理解错误。

### P2 · Peer PAKE 与安全通道

| # | 任务 | 验收 |
|---|------|------|
| C2.1 | `protocol::message`：JSON/deflate/AEAD 编解码 | 与 Go 版互编互解 |
| C2.2 | `protocol::peer_pake`：pake → pake-confirm 四帧状态机（含 feature 列表） | Rust receiver ↔ Go sender 完成 PAKE |
| C2.3 | 脱敏（`redact`）与错误分类 | 单测：错误信息中不含 secret |

### P3 · 单文件传输（最小可用）

| # | 任务 | 验收 |
|---|------|------|
| C3.1 | `fs::manifest`：单文件 FileInfo + SenderInfo 编解码 | 与 Go 版 JSON 结构比对 |
| C3.2 | `fs::chunk`：区间计算（对齐 `utils.ChunkRangesCount/Bytes`） | 单测 |
| C3.3 | 发送路径：单连接、无压缩、无断点 | **Rust send → Go receive 成功** ★ |
| C3.4 | 接收路径：`recipientready` + 写盘 + 哈希校验 | **Go send → Rust receive 成功** ★ |
| C3.5 | `croc-cli` 雏形：`send` / `<code>` 两个动作 | 手工端到端通过 |

### P4 · 完整传输能力

| # | 任务 | 验收 |
|---|------|------|
| C4.1 | 多文件、文件夹、`--zip`、空目录 | 与 Go 版互传目录树一致 |
| C4.2 | 压缩：逐文件采样判定 + flate | Go 端能解压 Rust 端数据 |
| C4.3 | 哈希：xxhash / imohash / md5 / highway | 与 Go 版对同一文件算出相同值 |
| C4.4 | 覆盖策略：overwrite / rename / resume / refuse | 四种策略手工验证 |
| C4.5 | 断点续传：区间请求 + chunk 队列 | kill 后重传能续上 |
| C4.6 | 多端口复用（最多 5 条数据连接）+ `--no-multi` | 吞吐对比测试 |
| C4.7 | 限速 `--throttleUpload` | 实测速率接近设定值 |
| C4.8 | 接收根路径沙箱（防目录穿越、symlink 校验） | 恶意清单测试：不能写到接收目录外 |

### P5 · CLI 完善

| # | 任务 | 验收 |
|---|------|------|
| C5.1 | 进度条（`indicatif`），多文件总进度 | 观感与 Go 版接近 |
| C5.2 | 剪贴板复制（含 `--extended-clipboard`） | 三平台实测 |
| C5.3 | QR 码输出 | |
| C5.4 | `--text` 发送文本、`--stdout` 输出到标准输出 | 管道测试 |
| C5.5 | 本地发现：组播 + 本地临时中继 + 本地探针握手 | 同一局域网下不走公网中继 |
| C5.6 | `--remember` 配置持久化 | 配置文件路径对齐 Go 版 |
| C5.7 | `--qrcode`、`--ask`、`--yes`、signal 处理（Ctrl-C 优雅退出） | |

### P6 · 加固（Tailscale 数据面 ⏸ PENDING）

| # | 任务 | 验收 |
|---|------|------|
| C6.1 | 重连：`NextReconnectRoom`、候选、退避、上限 10 次 | 中途 kill relay 进程，传输能恢复 |
| C6.2 | 超时与死锁排查：所有 `select!` 带取消分支 | 长跑测试 |
| C6.3 | 零拷贝优化：`encrypt_in_place` / 复用 buffer | 单核 ≥ 500 MB/s |
| C6.4 | 内存上限：大清单分块（`fileinfo-start/batch/end`） | 10 万文件清单不 OOM |
| C6.5 | 日志分级、`--debug`、错误脱敏审查 | |
| C6.6 | 补齐 `fileinfo` 分块、`relay-standby`、`relay-ramp` 等次要消息 | 与 Go 版互操作无 warning |
| C6.7 | `tailcat-offer` / `tailcat-status` / `transport-select` 识别与降级 | Rust `--transport derp` ↔ Go `--transport derp` 降级到 relay 后传输成功 |
| ~~C6.8~~ | ~~`crates/croc-tailscale` 封装 `tailscale` 0.6~~ | ⏸ **pending**，见 [08-tailscale.md](08-tailscale.md) |
| ~~C6.9~~ | ~~数据面择优~~ | ⏸ **pending** |

### P7 · 打包与发布

| # | 任务 |
|---|------|
| C7.1 | 构建矩阵：linux(gnu/musl, x64/arm64)、macos(arm64)、windows(x64/arm64) |
| C7.2 | 产出 **`croc-rs`**（CLI）与 **`crocw-rs`**（GUI / Tauri）两个产物，带 checksum |
| C7.3 | 安装脚本 + Homebrew formula + Scoop / Chocolatey（可选） |
| C7.4 | 文档：README、与 Go 版差异说明、迁移指南 |

> CI 设计（`release` 唯一入口 / `build` / `lint` / `clear`）见 [09-build-and-ci.md](09-build-and-ci.md)。

---

## 3. 性能目标

| 场景 | 目标 |
|------|------|
| 局域网直连（1 Gbps） | ≥ 100 MB/s |
| 中继转发（公网） | 网络带宽的 90% |
| 单核 CPU 占用 @ 500 MB/s | ≤ 60%（含 AES-GCM） |
| 10 万文件清单生成 | ≤ 5 s |
| 冷启动到显示三词码 | ≤ 150 ms |

## 4. 与 Go 版的已知行为差异（需在 README 中声明）

1. `--transport derp` ⏸ **pending**（等待上游 tailcat-rs），当前表现为"提示 + 走 relay"。它既不是 Go 版的 Tailcat 匿名节点协议，也不是官方 `tailscale` crate —— 后者虽已发布但模型不匹配（见 [08-tailscale.md](08-tailscale.md)）。
2. 不支持 `--curve ed25519`（v1）。
3. `croc store` / `croc ssh` 未实现。
4. `--internal-dns` v1.1 提供。
5. 自更新机制与 Go 版不同（v1 只提供 `--update --check`）。
