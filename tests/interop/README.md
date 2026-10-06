# 互通测试（Rust ↔ Go）

`run.sh` 跑 I1–I18 互通矩阵，定义见 [07-risks-and-testing.md](../../docs/agent/plan/07-risks-and-testing.md) §2.2。
这是本项目的**生命线测试** —— 协议字节必须与 Go 版 croc v11.5.4 逐字节兼容。

## 怎么跑

```bash
# 1. 编 Rust 版
cargo build --release -p croc-cli

# 2. 准备 Go 版（任选其一）
export CROC_GO_BIN=/usr/local/bin/croc-go      # 已有二进制
# 或者什么都不设，`cargo xtask ci-interop` 会去 ../croc 现编一份

# 3. 跑
cargo xtask ci-interop
# 或直接：
CROC_GO_BIN=/usr/local/bin/croc-go bash tests/interop/run.sh
```

## 环境变量

| 变量 | 默认 | 说明 |
|------|------|------|
| `CROC_RS_BIN` | `target/release/croc-rs[.exe]` | Rust 版二进制 |
| `CROC_GO_BIN` | —（必需） | Go 版二进制 |
| `CROC_INTEROP_ONLY` | 空（全跑） | 只跑指定用例，逗号分隔，如 `I1,I2,I14` |
| `CROC_INTEROP_SKIP_NETWORK` | 空 | 非空则跳过 I6（官方 relay，需外网） |
| `CROC_INTEROP_TIMEOUT` | `180` | 等发送方退出的超时（秒） |
| `CROC_INTEROP_KEEP` | 空 | 非空则保留临时工作目录，便于翻日志 |

## 约定

- **两端都用固定的 codephrase `acid-acorn-acre` 与固定的 relay 口令**，让失败可复现。
- **日志先脱敏再落盘**：房间号（hex）与口令统一替换成 `[ROOM]` / `[REDACTED]`（风险 R13）。
- **每个用例自起一个 relay**，端口从 31000 起找一段连续 5 个空闲端口（控制口 + 4 个数据口）。
- **崩溃与报错要分清**：`no_crash` 检查日志里有没有 `panic` / `SIGSEGV` 之类，
  "优雅报错"算通过（I14 错误密码就是这种用例），"崩了"一律算失败。

## CI

`lint.yml` 的 `interop` job 跑全矩阵但设了 `CROC_INTEROP_SKIP_NETWORK=1`；
`nightly.yml` 单独跑 I6（官方 relay）。
