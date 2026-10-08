# 互通测试（Rust ↔ Go）

`run.sh` 跑 I1–I21 互通矩阵，定义见 [07-risks-and-testing.md](../../docs/agent/plan/07-risks-and-testing.md) §2.2。
这是本项目的**生命线测试** —— 协议字节必须与 Go 版 croc v11.5.4 逐字节兼容。

## 怎么跑

```bash
# 1. 编 Rust 版
cargo build --release -p croc-cli

# 2. 指一份 Go 版 croc（默认路径是平级的 ../croc-playground/croc.exe）
cargo xtask ci-interop

# 或直接跑脚本：
CROC_GO_BIN=../croc-playground/croc.exe bash tests/interop/run.sh
```

`croc-playground/` 是与本仓库**平级的独立仓库**，里面的 `croc.exe` 是 Go 版、
`croc-rs.exe` 是 Rust 版的投放位——放好二进制就能手工对跑。`ci-interop` 在
`CROC_GO_BIN` 没给时会去找 `../croc-playground/croc.exe`，再退回现编 `../croc`。

## 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `CROC_RS_BIN` | `target/release/croc-rs[.exe]` | Rust 版二进制 |
| `CROC_GO_BIN` | `../croc-playground/croc.exe`，否则现编 `../croc` | Go 版二进制 |
| `CROC_INTEROP_ONLY` | 空（全跑） | 只跑指定用例，逗号分隔，如 `I1,I2,I14` |
| `CROC_INTEROP_SKIP_NETWORK` | 空 | 非空则跳过 I6（官方 relay，需外网） |
| `CROC_INTEROP_TIMEOUT` | `180` | 等发送方退出的超时（秒） |
| `CROC_INTEROP_KEEP` | 空 | 非空则保留临时工作目录，便于翻日志 |
| `CROC_INTEROP_FORCE_UNIMPLEMENTED` | 空 | 非空则连"依赖尚未实现的能力"的用例也照跑（预期红） |

## 约定

- **两端都用固定的 codephrase `acid-acorn-acre` 与固定的 relay 口令**，让失败可复现。
- **日志先脱敏再落盘**：房间号（hex）与口令统一替换成 `[ROOM]` / `[REDACTED]`（风险 R13）。
- **每个用例自起一个 relay**，端口从 31000 起找一段连续 5 个空闲端口（控制口 + 4 个数据口）。
- **崩溃与报错要分清**：`no_crash` 检查日志里有没有 `panic` / `SIGSEGV` 之类，
  "优雅报错"算通过（I14 错误密码就是这种用例），"崩了"一律算失败。
- **跳过的用例显式 SKIP，并打印理由**：`skip_unimplemented <id> <理由>`。
  当前是 I12（`--local`：Go 的 `peerdiscovery` 默认 `AllowSelf=false`，会丢掉源 IP 是本机
  的广播，得两台机器才能验）与 I6（`CROC_INTEROP_SKIP_NETWORK` 非空时）。
  **跳过不等于通过**，结尾会单列一张跳过清单——恒红的门禁会让人学会无视红灯，
  静默跳过则会让人以为已经做完了，两者都要避免。

## harness 自身踩过的坑

`run.sh` 也是代码，它第一次真跑之前同样没人验证过。以下都是**实际跑出来的**，
改这个脚本时别再踩一遍：

1. `begin_case()` 原本只 `mkdir "$CASE_DIR"`，而用例是**先造载荷再调 `transfer()`**
   （后者才建 `send/recv`）⇒ `make_file` 直接 `FileNotFoundError`。
2. `[ "$sender_status" -eq 124 ] || sender_status=$?` —— 此时 `$?` 是 `[` 自己的退出码
   （恒为 1），**发送端永远被判失败**。要先把 `wait` 的退出码收进变量。
3. I14「错误密码」传的"错密码"就是 `$CODE` 本身 ⇒ 等于没错，必然"传成功了"。
   改成给接收端**错的中继口令**，并手写断言（不能用 `transfer()`：它的判据是
   "两边都退 0"，而本用例要的恰恰是"接收端非零退出"）。
4. Git for Windows 的 `TMPDIR` 是 Windows 形式（`C:\...`），直接喂给 `rm -rf`
   会被安全删除策略拒掉（`SAFE_DELETE_INVALID_PATH`），已加 `cygpath -u`。
5. 退出时的 `rm -rf "$WORK"` 仍可能对 I13 的部分文件报 `Permission denied`：
   发送方刚退出，Windows 还没释放句柄。**这是收尾竞态，不是用例失败**——
   看到满屏 `Permission denied` 而最后一行是 `matrix rc=0`，就说明矩阵是绿的。
6. **Rust 代码起 bash 不能用 `Command::new("bash")`**：Windows 上按 `PATH` 命中的是
   `C:\Windows\System32\bash.exe`（**WSL 的 bash**），它把 `D:\repo\run.sh` 的反斜杠
   当转义符，报 `No such file or directory`（看着像脚本不见了）。
   `xtask` 里由 `resolve_bash()` 显式找 Git for Windows 的 `bash.exe`，
   路径经 `to_shell_path()` 转成正斜杠；可用 `CROC_INTEROP_BASH` 覆盖。
7. **每个用例末尾必须收掉自己起的中继**：`start_relay` 只把 PID 记在 `$RELAY_PID`，
   写下一个用例时就被覆盖——漏写一次 `stop_relay`，那份中继就再也没人收。症状很隐蔽：
   矩阵自己照样打完"通过 N · 失败 0"，但残留进程一直挂在进程表里（**把它接在 `| tail`
   后面时，管道也永远等不到 EOF，看起来像整个矩阵卡死**）。现在 `cleanup` 会兜底收掉
   本次跑过的全部中继 PID（`RELAY_PIDS`），但这条规约仍然要遵守。

**教训**：harness 也要有"第一次跑通"的记录，否则它只是**看起来像**门禁。

## CI

**不进 CI，也不需要进** —— 只在本地跑（`cargo xtask ci-interop`）。

这不是"还没腾出手"，是一个有意的决定，理由见
[09-build-and-ci.md §5.3](../../docs/agent/plan/09-build-and-ci.md)。简版：矩阵要求同一台
机器上同时有 Rust 版、Go 版和它们各自的运行时，每个用例还要各自起中继与两端进程；
挂进 `lint.yml` 会让它变成发布链路上一段又慢又不稳的环节，而它真正的作用是
"改了协议字节之后本地跑一遍"，不是"挡住某个 commit"。

要跑就本地跑：`cargo xtask ci-interop`。
