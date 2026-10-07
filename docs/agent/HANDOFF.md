# 交接记录 · 2026-10-07

给下一个接手的人（或下一个模型）。**只写"现在是什么样、怎么复现、哪里是坑"**，
方案与决策见 [`plan/`](plan/README.md)。

---

## 1. 当前状态一句话

**互通矩阵全绿**：`tests/interop/run.sh` 跑出 **通过 16 · 失败 0 · 跳过 2**
（跳过的是 I6「需要外网」与 I12「`--local` 未实现」）。

这是本项目第一次真正跑通这份 harness——在此之前它**一次都没被执行过**，
README 里"只在本地跑"那句话掩盖了这件事。第一次真跑就暴露了 harness 自身三个 bug
（见 §4），所以**别再把"文档写了"当成"验证过了"**。

---

## 2. 这一轮修掉的真 bug

按发现顺序。每一条都写了"症状 → 根因 → 落点"，因为症状都不指向根因。

### 2.1 数据面连接每条文件重建 ⇒ 多文件传输必断

- **症状**：单文件传得好好的；目录（或任何多文件）传到第二个文件时接收方报
  `relay error: relay room full`。
- **根因**：数据端口的房间名是 `{room}-{j}`，中继的一个房间只容得下两端各一条连接。
  我们每条文件都重建 5 条，第二条文件就撞上自己上一条文件**还开着**的连接。
- **Go 怎么做**：PAKE 完成后建一次（`openRelayChannels()`，`croc.go:2796`），
  留到整个 attempt 结束（`c.dataConns`）。
- **落点**：`crates/croc-engine/src/data.rs` 新增 `DataPlane`
  （`Mutex<Option<Vec<Framed<TcpStream>>>>` + `acquire()` / `release()`）。
  连接活到 `send()` / `receive()` 返回才关。
- **顺带**：`receive_file()` 现在把连接还回来（`Result<(Vec<Framed>, Arc<Progress>), _>`），
  与 `send_file()` 一致。
- **为什么可以安全复用**：发送方只发被请求的块（`ChunkQueue` 认领制），一块不多；
  接收方数够就收工，文件之间不会留半条帧。`Framed` 的缓冲是持久的，
  `read_frame()` 取消安全（先解析缓冲、再 `read_buf`），即便中继插了 keepalive 也不串包。

### 2.2 目录映射少了一层 ⇒ 目录传过去"散了一地"

- **症状**：`croc-rs send tree` 到对端后落在 `<out>/a.bin`、`<out>/sub/...`，
  而不是 `<out>/tree/a.bin`。传输本身成功，所以只有比对目录树才看得出来。
- **根因**：Go 的 `folder_remote` 算法是
  `TrimPrefix(Dir(pathName), Dir(absPath) + "/") + "/"`——基准是目录的**父**目录，
  于是 `folder_remote` **天然带上目录自己的名字**（`tree/`、`tree/sub/`，带尾斜杠）。
  我们用目录本身当基准，得到 `.`，接收方就把内容平铺到接收根下。
- **落点**：`crates/croc-engine/src/files.rs`
  - `remote_base()` / `remote_folder_of()`：照抄 Go 的算法；
  - `collect()` 先 `std::path::absolute()` 再走（`. `/`..` 取不出目录名，Go 也是先 `filepath.Abs`）；
  - 单个文件是 `"./"`（Go `croc.go:1178`）；空目录的 `folder_remote` **含它自己**；
  - `folder_count()`：数**去重后的目录路径**、**含根目录**（Go 每 walk 到一个目录就 `++`）。
    它不只是显示——Go 的 `validateSendingTextOffer()` 拿它校验文本传输（必须恰好 0）。
- **验证**：I7 现在**两个方向都跑**（Rust→Rust 与 Rust→Go）。只测一个方向的话，
  两边一起错也看不出来。

### 2.3 自建中继的密码根本没生效

- **症状**：`croc-rs --pass x relay` 起的中继，任何客户端连上都报 `bad password`。
- **根因**：`relay_cmd::run()` 的注释写着"密码从全局 `--pass` 来"，实际却去读
  `CROC_PASS` 环境变量或默认值——**全局 `--pass` 从没传进来**。
- **落点**：`Action::Relay` 现在带一个 `RelayRequest { args, password }`；
  `relay_cmd::run(args, password, cancel)` 把密码当入参。加了回归测试。
- **教训**：注释里写"从 X 来"而代码没接 X，是最难查的一类——它读起来是对的。

### 2.4 回答了**过期**的局域网探测帧 ⇒ 大清单握手报废

这一条最绕，值得单独讲清楚。

- **症状**：几万个文件时，Go 接收方报
  `problem with decoding: decompress message: decompress data: flate: corrupt input before offset 6`。
  几千个文件以下一切正常。
- **排查路径**：
  1. Go 的日志显示"收到一个 1150 字节的帧但解不开"；
  2. 插桩（临时 `eprintln!`）发现发送方 **`files::collect` 一个阶段就花了 17.2 秒**
     （2 万个空文件；对照：Go 版同场景 ≈ 98 秒，所以**不是我们的效率问题**，
     是 Windows + Defender 的文件打开开销）；
  3. 于是发送方 **17 秒后才进房间**，而对端的局域网探测只等
     `localProbeResponseTimeout = 500 ms`（Go `croc.go:374`）；
  4. 对端超时后往下走、发 `handshake` 和 `pake`，**在等 `pake` 的应答**；
     我们此时才进读循环，按顺序读到缓冲里的旧探测帧 `pake1`，回了 `pake2`
     —— 对端拿它当 `pake` 的应答去 DEFLATE 解压，于是 `corrupt input before offset 6`。
- **根因**：**回一个过期的探测答案是致命的**。不是"要不要支持局域网"的问题，
  是时序问题。
- **Go 为什么不踩**：`Send()` 把算哈希放进后台 goroutine、与连中继**并行**
  （`croc.go:1792`），所以它进 `senderWaitForHandshake` 时哈希还没算完，探测总能及时应答。
- **落点**：`crates/croc-core/src/session.rs` 的 `try_local_probe()`
  —— **只吃掉探测帧、不回**。
  - 我们没有内建局域网中继，能回的答案永远是"空地址列表"，也就是 `--no-local` 的意思；
  - 沉默永远安全：Go 的接收方把探测失败**丢掉**
    （`croc.go:2166` 那次 `Send(handshakeRequest)` 会把 `err` 覆盖掉），照常走中继；
  - 代价：对端多等 500 ms，与 Go 自己加 `--no-local` 时一样。
- **将来做本地中继时必须一起改**：让发送方尽早进入读循环（改成 Go 那样并行），
  否则回过去的仍然是过期答案。已写进 `plan/04-cli-plan.md` §1.5 与 `send()` 的注释。
- **⚠️ 未做的性能改进**：`files::collect` 仍然串行在 `relay::connect` 之前
  （`send()` 开头有注释说明为什么保留这个顺序：坏路径要早暴露）。它的代价是
  **大目录时对端要干等十几秒**。想改成 Go 的并行模型，见 §5 待办。

---

## 3. 顺手加的东西

| 项 | 位置 | 说明 |
|---|---|---|
| `--transport auto\|relay\|derp` | `crates/croc-cli/src/cli.rs` | `derp` = 打一条 `note:` 后按 relay，**不影响退出码**（对齐 Go `resolveTransportMode()` 的降级）。`--quiet` 下吞掉提示，与 Go `writeTailcatRelayFallbackWarning()` 一致 |
| `--local` **接受但报错** | 同上 | 隐私开关，**不能悄悄降级成公网中继**。`--local` + 非 auto 的 `--transport` 报 Go 同文错误 `--transport must be auto for local-only transfers` |
| `send --text` 不再要求路径 | 同上 | Go 允许 `croc send --text hello`；`paths` 去掉 clap 的 `required`，改在 `validate_sources()` 里查 |
| 接收端**路径逃逸防护** | `crates/croc-engine/src/files.rs` | `folder_remote` / `name` 是**对端写在清单里的字符串**。`safe_folder()` / `safe_name()` 拒绝对路径与 `..`。⚠️ `PathBuf::join` 遇到绝对路径会**整个替换**前缀——`out_dir.join("/etc/")` 就是 `/etc/` |
| harness「已知未实现」跳过 | `tests/interop/run.sh` | `skip_unimplemented()`，理由会打进结尾的跳过清单。跳过 ≠ 通过，但也不该恒红 |
| `ci-interop` 默认跳外网 | `xtask/src/ci.rs` | 默认设 `CROC_INTEROP_SKIP_NETWORK=1`；显式设空串可跑 I6 |

---

## 4. harness 自身的三个 bug（第一次真跑才暴露）

1. `begin_case()` 只 `mkdir "$CASE_DIR"`，而用例**先造载荷再调 `transfer()`**，
   后者才建 `send/recv` ⇒ `make_file` 直接 `FileNotFoundError`。
2. `[ "$sender_status" -eq 124 ] || sender_status=$?` —— 此时 `$?` 是 `[` 自己的退出码
   （恒为 1），**发送端永远被判失败**。改成先把 `wait` 的退出码收进变量。
3. I14「错误密码」传的"错密码"就是 `$CODE` 本身 ⇒ 等于没错，必然"传成功了"。
   改成给接收端**错的中继口令**，并手写断言（不能用 `transfer()`：
   它的判据是"两边都退 0"，而本用例要的恰恰是"接收端非零退出"）。

另外：Git for Windows 的 `TMPDIR` 是 Windows 形式（`C:\...`），直接喂给 `rm -rf`
会被安全删除策略拒掉（`SAFE_DELETE_INVALID_PATH`），已加 `cygpath -u`。

**教训**：harness 也要有"第一次跑通"的记录，否则它只是**看起来像**门禁。

---

## 5. 待办（按建议顺序）

1. **把互通矩阵挂进 `lint.yml`**（`CROC_INTEROP_SKIP_NETWORK=1`）。
   之前不挂的理由是"矩阵里有一批用例必然红"，**现在不成立了**：
   只剩 I12 会显式 SKIP。这是当前的头号缺口。
2. **`--local`（内建局域网中继）**：需要内嵌 relay + UDP 组播发现
   （Go 的 `broadcastOnLocalNetwork()`，载荷 `"croc" + 端口`）。
   做它**必须同时**把发送方改成"连中继与算哈希并行"，理由见 §2.4。
3. **发送方并行化**：`files::collect` 与 `relay::connect` 并行（Go `croc.go:1792`）。
   纯性能，但也是 §2.4 的前置条件。
4. **`--transport derp` 的 Tailscale 数据面**：⏸ 等上游 tailcat-rs。
5. **`--git` / `--exclude` / `--qrcode` / `--socks5` / `--throttleUpload` 等**：
   `plan/04-cli-plan.md` §1.2/§1.3 标了 ✅ 但**还没实现**——那张表是目标态，
   不是现状。动手前先核一遍。

---

## 6. 怎么复现这一轮的结果

```bash
# 1. 工具链：Rust stable（2024 edition）+ Go（编 Go 版 croc 或直接用现成二进制）
# 2. 编 Rust 版
cargo build --release -p croc-cli

# 3. 跑矩阵（Go 版二进制路径自己换）
CROC_GO_BIN=/d/Desktop/croc.exe CROC_INTEROP_SKIP_NETWORK=1 bash tests/interop/run.sh

# 期望：通过 16 · 失败 0 · 跳过 2
```

失败时用 `CROC_INTEROP_KEEP=1` 保留工作目录翻日志（日志已脱敏）。

**提交前的本地门禁**（质量门禁不跟 push / PR）：

```bash
cargo xtask ci-lint      # fmt / clippy / test / purity / deps —— 必须 5 项全绿
cargo xtask ci-interop   # 矩阵（默认跳外网）
```

---

## 7. 环境备忘（这台机器上踩过的）

- **Windows + Defender 下文件打开很贵**：2 万空文件 `files::collect` ≈ 17 s；
  Go 版同场景 ≈ 98 s。所以**别拿"几万文件"的用例去判断实现效率**，
  先怀疑环境。
- `git push` 需要 `-c http.version=HTTP/1.1` 并重试（代理 `127.0.0.1:6208`
  会间歇性对 `github.com` 返回 502）。
- 仓库是**私有的**：`gh api` 能用，匿名 `api.github.com` 会返回 404。
  核对远端状态用 `gh api repos/576576/croc-rs/commits/main --jq '.sha'`。
- 重编 `croc-rs.exe` 前先 `taskkill //F //IM croc-rs.exe //T`，
  否则 `cargo build` 报 `os error 5`（后台中继还占着文件）。
- 命令行里带 `${...}` 的字符串会被外层预处理器拒掉（`Bad substitution`），
  即便写在带引号的 heredoc 里也一样——避免在命令里拼 shell 变量展开。
