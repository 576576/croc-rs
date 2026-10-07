# 互通测试（Rust ↔ Go）

`run.sh` 跑 I1–I18 互通矩阵，定义见 [07-risks-and-testing.md](../../docs/agent/plan/07-risks-and-testing.md) §2.2。
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
- **未实现的用例显式 SKIP，并打印理由**：`skip_unimplemented <id> <理由>`。
  当前只有 I12（`--local`）走这条路。**跳过不等于通过**，结尾会单列一张跳过清单——
  恒红的门禁会让人学会无视红灯，静默跳过则会让人以为已经做完了，两者都要避免。

## CI

**不进 CI，也不需要进** —— 只在本地跑（`cargo xtask ci-interop`）。

这不是"还没腾出手"，是一个有意的决定，理由见
[09-build-and-ci.md §5.3](../../docs/agent/plan/09-build-and-ci.md)。简版：矩阵要求同一台
机器上同时有 Rust 版、Go 版和它们各自的运行时，每个用例还要各自起中继与两端进程；
挂进 `lint.yml` 会让它变成发布链路上一段又慢又不稳的环节，而它真正的作用是
"改了协议字节之后本地跑一遍"，不是"挡住某个 commit"。

要跑就本地跑：`cargo xtask ci-interop`。
