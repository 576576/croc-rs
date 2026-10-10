# wasm 目标调研与计划

调研时间：2026-10-10。所有结论都来自本机实测（不是读文档推测），未实测的部分都标了
"待验证"。

## 1. 结论

| 路线 | 目标 | 现状 | 建议 |
|---|---|---|---|
| A. CI 门禁 | `wasm32-unknown-unknown` | 纯核心只差 1 行 Cargo.toml | **先做**，半天 |
| B. 浏览器 wasm 模块 | `wasm32-unknown-unknown` | 24 个函数里 18 个有现成实现 | 做完 A 再说 |
| C. WASI 运行时 | `wasm32-wasip1/2` | 上游未就位（见 §4） | 等上游，不要现在投入 |

`--target` 本身不写进任何 crate：它是 CI 与本地命令的参数，代码里不需要 `cfg`。

## 2. 实测矩阵

`cargo check -p <crate> --target <target>`，wasip1 那列带 `RUSTFLAGS=--cfg tokio_unstable`。

| crate | wasm32-unknown-unknown | wasm32-wasip1 | 卡点 |
|---|---|---|---|
| croc-siec / croc-pake / croc-pakekey | 通过（零改动） | 通过 | 无 |
| croc-protocol | **失败** | 通过 | 浏览器目标上 `getrandom` 报 `compile_error!` |
| croc-core | 同 protocol | 通过 | 同上（它依赖 protocol） |
| croc-net | **失败** | **通过**（需 tokio_unstable） | 浏览器目标上 mio 无后端 |
| croc-relay | 失败 | **失败** | `TcpListener::bind` 在 wasi 下不存在 |
| croc-store | 失败 | **失败** | `socket2` 不支持该 target |
| croc-engine | 失败 | **失败** | `zstd-sys` 的 C build script 编不过 |
| croc-cli / croc-gui | 失败 | 失败 | 经 engine / tauri 传递 |

要点：

* **croc-core 是纯的**（依赖只有 protocol / pake / pakekey / sha2 / hex / thiserror /
  zeroize），`codephrase::relay_index` 就在它里面 —— 浏览器要的"码 → 中继"逻辑不用额外搬。
* wasi 下 `croc-net` 能编，但**连不上**：tokio 在 wasi 下没有 `bind` / `connect`，
  只能 `FromRawFd` 接宿主预先打开的 fd（`tokio-1.53.2/src/net/tcp/listener.rs:292`
  一带全是 `#[cfg(target_os = "wasi")]` 的裁剪）。

## 3. 卡点一：`getrandom`（只影响浏览器目标）

失败信息来自 `getrandom-0.4.3/src/backends.rs`：`wasm32-unknown-unknown` 默认不支持。

唯一入口（`cargo tree -p croc-protocol -e features -i getrandom`）：

```
aes-gcm 默认 feature "getrandom"
  → aead/getrandom → crypto-common/getrandom → dep:getrandom
```

全仓没有一处用 `generate_nonce` / `OsRng` / `rand_core`（`croc-protocol::encrypt` 收的是
显式 nonce），所以把这层去掉是安全的：

```toml
# 根 Cargo.toml
aes-gcm = { version = "0.11", default-features = false, features = ["aes", "alloc"] }
```

**待验证**：改完要跑一遍 `cargo check --workspace --all-targets` + `cargo test --workspace`
+ `cargo xtask ci-lint`，确认原生侧没用到被裁掉的 API。

注意这条路的形状：是**删掉**一个依赖特性，不是往纯 crate 里加 `getrandom`。后者会被
`cargo xtask purity` 拦（`FORBIDDEN_DEPS` 里有 `getrandom`），前者不会——所以
**方案 A 不用动 xtask**。

## 4. 卡点二：wasi 的 socket 栈还没就位

实测（tokio 1.53.2 / mio 1.2.4）：

* mio 1.2.4 有 `sys/wasip1` 后端，`target_os = "wasi"` 且非 p1 时走 unix 后端 —— 说明
  mio 侧已经落地；
* tokio 侧 **只在 `--cfg tokio_unstable` 下**才允许在 wasm 上开 `net`，且文档明说
  "WASI 目前不支持从 WASM 内部新建 socket"（`tokio-1.53.2/src/lib.rs:450` 一带）；
* 上层还差两块：`socket2`（`croc-store` / `croc-engine`）不支持 wasi target，
  `zstd-sys` 的 C 构建需要 WASI SDK。

也就是说：wasi 这条线的"待合并"不只是一个 PR，是 tokio unstable 转正 + socket2 +
C 依赖三件事。**现在投入会返工**，故列为路线 C，只记不办。

触发重新评估的条件：tokio 去掉 `net` 的 `tokio_unstable` 门禁，且 socket2 支持 wasi。

## 5. 方案 A：给 CI 加浏览器目标门禁

1. 根 `Cargo.toml`：`aes-gcm` 关默认 feature（§3 那一行）。
2. `.github/workflows/lint.yml` 加一个 job（与 `arch` / `msrv` 并列）：
   `rustup target add wasm32-unknown-unknown` →
   `cargo check -p croc-siec -p croc-pake -p croc-pakekey -p croc-protocol -p croc-core --target wasm32-unknown-unknown`。
3. `xtask` 加一个 `ci-wasm`（本地等价命令），并写进 `cargo xtask help`。
4. 文档：`docs/agent/architecture.md` 里补一句"纯核心必须能编到 wasm32-unknown-unknown"。

收益：谁往核心里塞了 IO / 时钟 / OS 随机源，这条 job 立刻红，比 `purity` 的关键字扫描
更硬（它是真编译）。

## 6. 方案 B：浏览器 wasm 模块（替换 croc web 的 Go wasm）

Go 侧暴露 24 个函数（`../croc/web/wasm/main.go` 的 `b.expose` 列表）：

| 类别 | Go 函数 | Rust 侧 | 状态 |
|---|---|---|---|
| PAKE | pakeInit / pakeInitWithIdentities / pakeUpdate / deriveKey / derivePeerKeys / confirmPeerKey | croc-pake + croc-pakekey | 现成 |
| 编解码 | encrypt / decrypt / compress / decompress / cipherInit / cipherRelease / encodeChunk / decodeChunk | croc-protocol（`codec.rs`）+ flate2 | 现成 |
| 哈希 | hashInit / hashUpdate / hashFinal / sha256×3 | croc-protocol 或 sha2 直出 | 现成 |
| 码 | codeComponents / sshCodeComponents / relayIndex | croc-core `codephrase.rs` | 现成 |
| 暂存 | storeGenerateKey / storeRedeemCapability / storeSealManifest / storeOpenManifest / storeSealChunk / storeOpenChunk | croc-store（**缠着 croc-net + tokio + reqwest**） | 要拆 |

差距只有"暂存"那一块。可行的拆法：`croc-store` 里已经不碰 IO 的
`crypto.rs` / `manifest.rs` / `share.rs` / `base64url.rs` / `expiration.rs` 抽成
`croc-store-core`，`client.rs` / `http.rs` / `proxy.rs` 留在原 crate。

工作量之外还有两件非技术的事：

* 新 crate `croc-wasm`（wasm-bindgen）+ JS 侧替换 `web/src/wasm/client.ts`，要能过
  croc web 自己的 e2e；
* 上游不一定愿意换掉 Go wasm —— 这条线更适合作为我们自己的 fork 验证。

## 7. 验证清单（方案 A 落地时照跑）

```console
cargo check -p croc-siec -p croc-pake -p croc-pakekey -p croc-protocol -p croc-core \
  --target wasm32-unknown-unknown
cargo check --workspace --all-targets      # 原生侧没被裁坏
cargo test --workspace --no-fail-fast
cargo xtask ci-lint                        # 含 purity：不该有新违规
```

## 8. 不做的事

* 不在 wasm 上编 `croc-relay` / `croc-engine` / `croc-store` / `croc-cli` / `croc-gui`
  —— 卡在 socket2、zstd-sys、tauri，且不解决"wasi 不能新建 socket"。
* 不为浏览器目标写 `wasm-bindgen`：那是方案 B 的事，等 A 落地再开。
* 不动 `xtask purity`：`getrandom` 是被**删掉**的，不是被加进纯 crate 的。

## 附：本机环境

已装 `wasm32-unknown-unknown` 与 `wasm32-wasip1`（`wasm32-wasip2` 那次下载 TLS 握手 EOF，
需要时用 `env -u HTTPS_PROXY ... rustup target add wasm32-wasip2` 重试）。
