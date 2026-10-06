# croc-rs

[croc](https://github.com/schollz/croc) 的 Rust 重写版 —— 在任意两台电脑之间端到端加密地传文件，
配一个可自建的中继。

> **协议基线：Go 版 croc `v11.5.4`。** 线路字节是硬契约，目标是与官方客户端**双向互通**。
> 实现层按 Rust 惯用法重写，不做 Go 的逐行翻译（见 [设计原则](docs/agent/plan/00-overview.md)）。

---

## 两个产物

| 产物 | crate | 平台 |
|------|-------|------|
| **`croc-rs`** | [`crates/croc-cli`](crates/croc-cli) | 桌面 + 服务器全部平台 |
| **`crocw-rs`** | [`crates/croc-gui`](crates/croc-gui)（Tauri 2，w = window） | Windows / macOS / Linux / Android |

## 怎么用

```bash
# 发送（打印一个口令码）
croc-rs send ./photos

# 接收
croc-rs acid-acorn-acre

# 自建中继
croc-rs relay --ports 9009,9010,9011,9012,9013
```

常用开关与 Go 版一致：`--relay` / `--pass` / `--curve` / `--out` / `--overwrite` / `--rename` /
`--yes` / `--no-compress` / `--code` / `--text`。

---

## 架构：纯核心 + 副作用边缘

```
croc-cli / croc-gui        UI 层，只依赖 trait
        │
croc-engine                副作用解释器：tokio socket、文件 IO、数据面
        │
croc-core                  纯领域层：会话状态机、口令码、分片规划、覆盖策略
        │
croc-protocol              sans-io 线路层：帧、消息、中继握手、清单
        │
croc-pakekey / croc-pake / croc-siec    纯数学与密钥协商
```

**硬约束**：`croc-protocol` / `croc-pake` / `croc-siec` **禁止**出现 `tokio::`、`std::fs`、
`Instant::now`、OS 随机源。随机数由调用方注入，于是**一次完整的握手与清单交换可以在单元测试里
确定性重放**——不需要 socket、不需要超时。CI 用 `cargo xtask purity` 强制这条线。

**本仓库没有 `unsafe`**：0 处 `unsafe`、0 处 FFI、0 处 `transmute` / `from_raw` / `as_ptr`，
构建脚本里也没有 `cc` / `bindgen`。`[workspace.lints]` 的 `unsafe_code = "warn"` 配上 CI 的
`-D warnings`，让"加一处 `unsafe`"变成一个必须显式面对的编译错误。

Miri 仍然在跑（`nightly.yml`，唯一用 nightly 的地方），但它查的是**依赖里的** unsafe
（主要是 `curve25519-dalek` 的 serial 后端）与"以后有人伸手写裸指针"的哨兵——**它是看板不是门禁**。

---

## 开发

```bash
cargo xtask ci-lint      # = lint.yml 的 fmt / clippy / test / 架构门禁
cargo xtask ci-build     # = build.yml 的本机 target
cargo xtask ci-interop   # = 与 Go 版的互通矩阵（需要 Go 与 croc 源码）

cargo xtask purity       # 纯 crate 里不许有 IO
cargo xtask deps         # 核心 crate 不许引入 openssl / ring
```

质量门禁**不跟 push / PR**，而是挂在发布链路上（`build` 的 `needs` 带上 `lint`），
所以发布链路上没有绕过路径。代价是 PR 阶段没有绿灯信号——由上面这三条本地命令补。
详见 [docs/agent/release.md](docs/agent/release.md)。

### 协议改动的规矩

**先改文档再改代码。** 协议理解沉淀在 [`docs/agent/plan/`](docs/agent/plan)（`01-protocol.md` /
`02-crypto.md`），改完才动实现。兼容性知识靠 golden 向量与互通测试钉住，不靠代码里的
`// COMPAT:` 注释。

---

## 许可

GPL-3.0-only。协议与算法来自 MIT 许可的 Go 版 croc（© Schollz）与 `tscholl2/siec`；
词表与常量点的署名在实现完成后统一补 `THIRD_PARTY_NOTICES.md`。
