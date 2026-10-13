# croc-rs Rust 代码审查报告

日期：2026-10-13
依据：rust-skills（265 条规则 / 26 类，按 CRITICAL > HIGH > MEDIUM > LOW 分级）
范围：`crates/*/src`，12 个 crate，84 个文件，26 437 行

## 0. 方法

三层：静态模式扫描 → 逐条读上下文排除误报 → 对存疑项写探针在**真实 crate 代码**上验证。
第三条是这份报告与普通 grep 输出的区别：下面两条 CRITICAL/HIGH 都是跑出来的，不是看出来的。

已有的门禁（审查时全绿，修复后仍全绿）：

| 门禁 | 审查时 | 修复后 |
| --- | --- | --- |
| `cargo fmt --check` | 通过 | 通过 |
| `cargo clippy --workspace --all-targets` | 通过 | 通过 |
| `cargo clippy -p croc-gui --target aarch64-linux-android` | 通过 | 通过 |
| `cargo test --workspace` | 540 个测试全绿 | 542 个全绿（+2 个用例） |

## 1. 结论

代码底子很好，扫描出的多数"可疑模式"经确认都是**已经论证过的刻意选择**（详见 §3）。
真实缺陷 7 条，其中 1 条可 panic、1 条静默接受非法输入。**§2.1–§2.6 已修，§2.7 经评估保持原状**
（执行记录见 §4）：

| # | 级别 | 位置 | 一句话 | 状态 |
| --- | --- | --- | --- | --- |
| 1 | **CRITICAL** | `croc-store/src/expiration.rs:15` | 非 ASCII 输入（`1天`）触发 `str::split_at` 的 char 边界 panic | 已修 |
| 2 | **HIGH** | `croc-store/src/expiration.rs:32` | 超范围输入被 `unwrap_or(i32::MAX)` 静默饱和，不报错 | 已修 |
| 3 | MEDIUM | `croc-store/src/expiration.rs:13`、`croc-engine/src/text.rs:16` | 库层返回 `Result<_, String>`，与全仓 thiserror 约定不一致 | 已修 |
| 4 | MEDIUM | `croc-store/src/client.rs` 等 | async fn 内直接调用阻塞 `std::fs` | 已修（只改值两处） |
| 5 | LOW | `croc-store/src/client.rs` 9 处 | `try_from(..).unwrap_or(MAX)` 家族，掩盖"不可达"这件事 | 已修 |
| 6 | LOW | `croc-gui/src/android.rs:186,197` | `unsafe` 块缺 `// SAFETY:` 前缀 | 已修 |
| 7 | LOW | `Cargo.toml:164` | `lto = "thin"` 可评估换成 `"fat"` | 保持 thin |

## 2. 逐条详述

### 2.1 CRITICAL —— `parse_expiration` 对非 ASCII 输入 panic

`crates/croc-store/src/expiration.rs:15`

```rust
pub fn parse_expiration(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let (digits, unit) = value.split_at(value.len().saturating_sub(1));   // ← 这里
```

`str::len()` 是**字节**长度，`split_at` 又要求切点落在 char 边界上。输入以多字节字符结尾时切点落进字符内部，直接 panic。

规则：`err-result-over-panic`、`anti-panic-expected`。也与本仓自己的门禁冲突——`Cargo.toml` 把
`unwrap_used` / `panic` 设为 `warn` 且 CI 跑 `-D warnings`，这条是从 lint 网眼里漏过去的。

验证（临时探针插在 `expiration.rs` 的 `tests` 里，跑完已删）：

```
$ cargo test -p croc-store --lib expiration -- --nocapture
thread 'expiration::tests::tmp_probe_non_ascii_unit' panicked at .../core/src/str/mod.rs:862:21:
end byte index 3 is not a char boundary; it is inside '天' (bytes 1..4 of string)
probe "1天" -> PANIC
thread '...' panicked at .../core/src/str/mod.rs:862:21:
end byte index 7 is not a char boundary; it is inside '钟' (bytes 5..8 of string)
probe "30分钟" -> PANIC
```

用户可达路径（两处都是用户输入）：

| 入口 | 调用点 |
| --- | --- |
| `croc-rs store --expire 1天` | `crates/croc-cli/src/store.rs:235` |
| GUI 的对应输入 | `crates/croc-gui/src/store.rs:66` |

现有测试只覆盖了 ASCII 与带空格的 ASCII（`croc-cli/src/store.rs:428` 有 `" 1d "`），中文单位没测到。

改法：在切分前按**字符**取尾字符，让非 ASCII 单位正常落到"单位不认识"那个分支：

```rust
let mut chars = value.chars();
let Some(unit) = chars.next_back() else {
    return Err("duration must be a positive whole number followed by m, h, d, or w".to_owned());
};
let digits = chars.as_str();
```

`chars.next_back()` 取走最后一个字符，`as_str()` 返回剩下的部分，两者都在 char 边界上。
`"1天"` 于是走 `_ => Err("duration unit must be m, h, d, or w")`——既不再 panic，语义也对。

### 2.2 HIGH —— 超范围输入被静默饱和

`crates/croc-store/src/expiration.rs:32`

```rust
let duration = multiplier
    .checked_mul(seconds.try_into().unwrap_or(i32::MAX))   // ← 窄化失败时取 i32::MAX
    .ok_or_else(|| format!("duration {value:?} is too large"))?;
```

`i64 → i32` 失败时取 `i32::MAX`，而 `Duration::checked_mul(i32::MAX)` **不会**溢出（`time::Duration`
内部是 i64 秒，`i32::MAX` 天 ≈ 1.86e14 秒，远在 i64 范围内），所以下面的 `ok_or_else` 永远不触发。
用户写超范围的值会被静默改成一个他没写过的巨大值。

验证（同上，探针已删）：

```
probe     99999999999d -> Ok(2147483647 天)      ← 正好是 i32::MAX
probe  99999999999999w -> Ok(15032385529 天)
probe      5000000000h -> Ok(89478485 天)
```

规则：`num-overflow-explicit`、`err-no-unwrap-prod`。

改法：窄化失败就报错，不要 `unwrap_or`：

```rust
let amount = i32::try_from(seconds).map_err(|_| format!("duration {value:?} is too large"))?;
let duration = multiplier
    .checked_mul(amount)
    .ok_or_else(|| format!("duration {value:?} is too large"))?;
```

### 2.3 MEDIUM —— 库层用 `Result<_, String>`

| 位置 | 签名 |
| --- | --- |
| `croc-store/src/expiration.rs:13` | `pub fn parse_expiration(value: &str) -> Result<Duration, String>` |
| `croc-engine/src/text.rs:16` | `pub fn write_text_artifact(text: &str) -> Result<PathBuf, String>` |

全仓 12 个 crate 里，除 `croc-cli`（应用层）与 `croc-gui`（Tauri 命令必须返回 `String`）外的 10 个
都用 thiserror（`croc-store/src/lib.rs:52` 的 `Error`、`croc-engine/src/error.rs` 的 `EngineError`）。
上面两处是例外：调用方拿到 `String` 之后没法按错误种类分叉，只能整串往下透传。

规则：`err-thiserror-lib`、`err-custom-type`。

改法：两处都把 `String` 换成各自 crate 的既有 `Error`（`croc-store` 加一个
`Error::Format(..)` 或新的 `Error::Duration`；`croc-engine` 用 `EngineError::File`，
它的 `{path}: {reason}` 格式正好对上现在手工拼的那句）。`croc-gui` 那侧不用动——
Tauri 命令返回 `String` 是框架要求。

### 2.4 MEDIUM —— async fn 里直接调阻塞 `std::fs`

| 行 | 调用 | 所在函数 |
| --- | --- | --- |
| 207 | `std::fs::create_dir_all(output)` | `async fn receive` |
| 232 | `std::fs::remove_file(session.state_file())` | `async fn receive` |
| 673 | `std::fs::symlink_metadata(path)` | `async fn prepare`（**循环内**） |
| 745 / 767 | `std::fs::read` / `write`（状态文件） | `fn load` / `fn save` |
| 798 / 806 | `std::fs::File::open` / `rename` | `async fn install_verified` |

规则：`async-tokio-fs`、`async-spawn-blocking`。

判断要分开看，不要一律当问题：

- **单次、微小**的（232、745、767、798、806）：`rename`、`remove_file`、状态文件读写都是
  几十微秒级，tokio 官方给出的界是"单次 < 10–100 μs 且不与长时间计算相邻"，这些都在界内。
  改成 `tokio::fs` 反而多一次 `spawn_blocking` 的线程往返。
- **值得改的两处**：
  - `prepare` 的 673 在**每个输入文件**上做一次 `symlink_metadata`。目录里几百上千个文件时，
    累积阻塞是毫秒到秒级，而且它在 `hash_file(..).await` 之前——会独占一个 worker。
  - `receive` 的 207 `create_dir_all` 在多级新建时可能几十毫秒。

已改：`prepare` 拆成两段——`survey()` 是同步函数，整个装进一次 `tokio::task::spawn_blocking`；
哈希循环留在 async 里（`hash_file` 本身就是 `tokio::fs` + `await`）。`receive` 的 `create_dir_all`
换成 `tokio::fs`。其余单次微小的调用保持 `std::fs`，并在 `install_verified` 里写明了判据。

### 2.5 LOW —— `unwrap_or(MAX)` 家族（9 处，已修）

`croc-store/src/client.rs` 里一共 **9 处**。初查只认出 4 处，`usize::MAX` 与 `unwrap_or(0)`
那几处是逐条读上下文时才补齐的：

```
270:  i64::try_from(prepared.manifest_json.len()).unwrap_or(i64::MAX)
344:  usize::try_from(file.chunk_count).unwrap_or(usize::MAX)
488:  u64::try_from(file.size).unwrap_or(u64::MAX)
490:  i64::try_from(index).unwrap_or(i64::MAX)
509:  u64::try_from(reference.file_chunk * CHUNK_SIZE).unwrap_or(u64::MAX)
560:  usize::try_from(file.chunk_count).unwrap_or(usize::MAX)
561:  usize::try_from(file.first_chunk).unwrap_or(usize::MAX)
570:  u64::try_from(i64::try_from(sequence).unwrap_or(i64::MAX) * CHUNK_SIZE).unwrap_or(u64::MAX)
571:  usize::try_from(reference.plain_size).unwrap_or(0)
```

这九处**目前都不可达**：`manifest::validate_manifest`（`croc-store/src/manifest.rs:123`）用
`entry.size < 0 || entry.size > max_bytes - total` 同时挡住了负数与总量超限，`validate_chunk_ref:146`
管住了分片下标范围。所以不是 bug。

但写法本身掩盖了"不可达"这件事，掩盖的代价就是 §2.2 那种真漏网。已按 `num-overflow-explicit`
收进一个 `narrow()` 辅助函数，失败就报 `Error::Format`：

```rust
fn narrow<T, U>(value: T, what: &str) -> Result<U, Error>
where
    U: TryFrom<T>,
{
    U::try_from(value).map_err(|_| Error::Format(format!("{what} is out of range")))
}
```

顺带修掉的：

- 570 行 `... * CHUNK_SIZE`：先窄化再乘，`i64::MAX * CHUNK_SIZE` 那条溢出路径没了；
- `size + CHUNK_SIZE - 1`（原 689 行，算 `chunk_count`）同样会溢出。**有符号整数的
  `i64::div_ceil` 至今未稳定**（本机 `rustc 1.99.0` 上仍是 `#[unstable(feature = "int_roundings",
  issue = "88581")]`；稳定的只有无符号整数那份，`#[stable(since = "1.73.0")]`），所以摊开写成
  `size / CHUNK_SIZE + i64::from(size % CHUNK_SIZE != 0)`；
- `total_bytes += size` 改成 `checked_add`。

`expect` 用不了——本仓把 `expect_used` 设为 `warn` 且 CI 跑 `-D warnings`。

### 2.6 LOW —— `unsafe` 块缺 `// SAFETY:` 前缀

`croc-gui/src/android.rs:187,198`：

```rust
#[expect(unsafe_code, reason = "ndk-context 只提供裸指针")]
let Ok(vm) = (unsafe { JavaVM::from_raw(context.vm().cast()) }) else { ... };
```

两处都有 `reason` 说明（讲清了"不接管生命周期"），内容上够。缺的只是 `unsafe-safety-comment`
要求的 `// SAFETY:` 前缀——那个前缀能被 grep、被 CI 检查，散在 `reason` 里的不行。
全仓只有这两处 `unsafe`，都已加上 `// SAFETY:` 前缀（原来的生命周期说明保留）。

### 2.7 LOW —— `lto = "thin"`（经评估保持原状）

`Cargo.toml:164` 现在 `lto = "thin"`。`fat` 通常再快 5–15%，代价是链接时间明显变长。
croc 的瓶颈在网络与磁盘（`data.rs` 的加解密、`flate2` 的压缩才是 CPU 大头），
收益大概率落在噪声里。

这条**没有改**：按 `perf-profile-first`，没有 profiler 数据就不该动构建配置——改了也无从判断
是变快还是变慢。要动就先有个能覆盖 `data.rs` 加解密或 `hash.rs` 的基准。

## 3. 已验证、确认**不是**问题的方面

扫描出的这些模式都逐条看过上下文，结论是刻意选择，不应改：

| 模式 | 出现处 | 为什么不是问题 |
| --- | --- | --- |
| `unwrap` / `expect` | 全仓 607 处 | 生产代码 **0 处**（除 2 个 `build.rs`）；129 处在 `tests/` 集成测试、476 处在 `#[cfg(test)]` 模块。`wire.rs` 那 10 处是自定义方法 `self.expect(b'[')`，不是 `Option::expect` |
| `words()[index as usize]` | `croc-core/src/codephrase.rs:358` | 带 `#[expect(clippy::indexing_slicing, reason=..)]` + 下标约束说明 |
| `reduced as u32` | `croc-core/src/codephrase.rs:234` | 带 `#[allow(cast_possible_truncation, reason=..)]` + 数学论证（`reduced < modulus ≤ u32::MAX`） |
| `payload.len() as u32` | `croc-protocol/src/frame.rs:45` | 上一行有 `MAX_MESSAGE_SIZE` 前置检查，且有专门测试 `oversized_payload_is_rejected_on_encode` |
| `u32::from_le_bytes(..) as usize` | `croc-protocol/src/frame.rs:75` | 64 位上是宽化；且先过 `MAX_MESSAGE_SIZE` 检查再分配 |
| `&String` / `&Vec<T>` 参数 | 全仓 | **0 处** |
| `TODO` / `FIXME` | 全仓 | **0 处** |
| 文档覆盖 | 774 个 pub 项 | 92%（56 个"缺失"中 30+ 是 `pub mod x;` 声明，属误报） |
| `clone()` 密度 | 最高 12.8 处/千行 | 无热点，不构成 `anti-clone-excessive` |
| `Mutex` / `RwLock` | 36 处 | 配套 `await_holding_lock` lint 已开，无持锁跨 await |
| `format!` | 204 处 | 集中在状态通报（`reporter.status`），不在数据面热路径 |
| serde 设计 | — | `skip_serializing_if` 36、`serde(default)` 15、`rename_all` 13、`#[non_exhaustive]` 18，设计充分 |
| 令牌桶 | `croc-engine/src/throttle.rs` | 锁中毒用 `PoisonError::into_inner` 继续算（不静默关限速），注释解释了"令牌可为负"的成因，有测试 |

## 4. 本轮执行记录

按"能 panic 的先修"排的：

| 顺序 | 内容 | 涉及文件 |
| --- | --- | --- |
| 1 | §2.1 + §2.2：切分改按字符；窄化失败改显式报错；补 2 个测试 | `croc-store/src/expiration.rs` |
| 2 | §2.3：新增 `Error::Expiration`；`write_text_artifact` 改用 `EngineError::File` | `croc-store/src/lib.rs`、`croc-engine/src/text.rs`、`croc-cli/src/store.rs`、`croc-gui/src/{store,commands}.rs` |
| 3 | §2.4：拆出 `survey()` 走 `spawn_blocking`；`create_dir_all` 换 `tokio::fs` | `croc-store/src/client.rs` |
| 4 | §2.5：9 处饱和写法收进 `narrow()`；乘法与总量改 `checked_*` | `croc-store/src/client.rs` |
| 5 | §2.6：加 `// SAFETY:` 前缀 | `croc-gui/src/android.rs` |
| — | §2.7：不改 | — |

错误消息保持与 Go 逐字一致：`Error::Expiration` 用 `#[error("{0}")]` 原样透出，不加
`Format` 那层 `stored transfer format: ` 前缀——加了会破坏与 `storeapi.ParseExpiration` 的对齐。
