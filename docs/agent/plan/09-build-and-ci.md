# 09 · 构建、发布与 CI

> **对齐 `D:/Documents/GitHub/Suwayomi-next` 的组织方式**（同为 Rust workspace 项目）。
> 本文档承担 Suwayomi-next 里 `docs/agent/release.md` 的角色：**决策与背景都写在这里，
> workflow 文件里只留必要提示**。

---

## 1. 四个 workflow 与职责边界

| 文件 | 触发 | 角色 |
|------|------|------|
| **`release.yml`** | `push` main + `workflow_dispatch` | **唯一入口**。算版本号、拼矩阵、调 lint、调 build、收产物发布 |
| **`build.yml`** | 仅 `workflow_call` | **可复用构建**。只编译打包 + `upload-artifact`，不含任何版本推导 |
| **`lint.yml`** | `workflow_call` + `workflow_dispatch` | **质量门禁**。fmt / clippy / test / 架构门禁 / MSRV / Miri / 互通 |
| **`clear.yml`** | 仅 `workflow_dispatch` | 预发布清理：每个窗口内只留最新 1 个 alpha Release |

### 1.1 三条铁律（沿用 Suwayomi-next）

1. **质量门禁挂在发布链路上，不单独跟 push/PR。** `build` 的 `needs` 带上 `lint`——门禁不过就直接不进编译。所有入口都从 `release.yml` 进，**没有绕过路径**。唯一例外是纯文档 push（被 `paths-ignore` 跳过，既不发布也不跑门禁）。
2. **`build.yml` 不重新推导任何参数。** 版本号、通道后缀、矩阵 `include` 全在 `prep` 里算一次，`build.yml` 只负责拼。
3. **产物形态用正交开关表达**，默认值与自动 alpha 逐项一致，所以推送与手动两条路径出的包一样，不必按触发方式分叉。

### 1.2 为什么 lint 不跟 PR

跟了 PR 就等于允许"PR 红了也照样发"——门禁沦为装饰。挂在 release 链路上则**物理上无法绕过**。
代价是 PR 阶段没有绿灯信号 → 由本地 `cargo xtask ci-lint` 补（见 §6）。

---

## 2. 通道与版本

| 通道 | 触发 | versionName | tag | prerelease |
|------|------|-------------|-----|-----------|
| alpha | 推送 main（自动） | `r{count}` | `r{count}-alpha.{run_id}` | ✅ |
| alpha | 手动 dispatch | `r{count}` | `r{count}-alpha.{run_id}` | ✅ |
| beta | 手动 dispatch | `1.{n/100}.{n%100 补零两位}` | `v1.y.z-beta.{run_id}` | ❌ |
| release | 手动 dispatch | `1.y.z` | `v1.y.z` | ❌ |

- `count` = 提交数（`git rev-list --count HEAD`），由 `prep` 算一次。
- 3 段版本号的末两位**必须补零**：tag 去非数字后要能当数字比大小，`1.2.5`→125 会小于 `r1205` 被判旧。
- **通道段只有 beta 非空**（`-beta`）：beta 与 release 共用 `1.y.z`，不区分就重名；alpha 的 `r{count}` 本身已表明通道。
- release 同名 tag 已存在时先 `gh release delete --cleanup-tag`。

---

## 3. 两个产物与命名

| 产物（二进制名） | crate | 平台 |
|------|-------|------|
| **`croc-rs`** | `crates/croc-cli`（CLI） | 桌面 + 服务器全部平台；Android 不出 |
| **`crocw-rs`** | `crates/croc-gui`（Tauri 2，w = window） | Windows / macOS / Linux / Android |

> **crate 名不带 `-rs` 后缀**（`croc-cli` / `croc-gui`），`-rs` 只出现在**二进制名**上：

```toml
# crates/croc-cli/Cargo.toml
[[bin]]
name = "croc-rs"
path = "src/main.rs"
```

```
croc-rs-{VER}{通道段}-{TGT}.{ext}
crocw-rs-{VER}{通道段}-{TGT}.{ext}

例：
croc-rs-1.0.0-linux-x64.tar.gz
croc-rs-r1205-alpha.12345-windows-x64.zip
crocw-rs-1.0.0-beta.12345-windows-x64.zip
crocw-rs-1.0.0-android-arm64.apk
```

- `TGT`：`windows-x64` / `windows-arm64` / `linux-x64` / `linux-arm64` / `macos-x64` / `macos-arm64` / `android-arm64` / `android-x64`。
- Windows 上 `croc-rs.exe` 是 console subsystem，`crocw-rs.exe` 由 Tauri 产出（windows subsystem，不弹控制台）。
- **形态开关不进文件名**（我们只有一个形态，不像 Suwayomi 有 `+jre`）。

### 3.1 产物形态开关（dispatch 输入）

| 开关 | 默认 | 含义 |
|------|------|------|
| `pack_cli` | ☑ | 出 `croc-rs` |
| `pack_gui` | ☑ | 出 `crocw-rs`（含 Android APK） |

两者正交；**都关则没有可发布的包**，`prep` 里预先拦一道（别让每个 target 白跑一遍编译）。

---

## 4. 构建目标与 runner（原生同架构，不用 cross）

沿用 Suwayomi-next 的 runner 表：

| 开关 | runner | rust target |
|------|--------|-------------|
| `build_windows_x64` | `windows-latest` | `x86_64-pc-windows-msvc` |
| `build_windows_arm64` | `windows-11-arm` | `aarch64-pc-windows-msvc` |
| `build_linux_x64` | `ubuntu-latest` | `x86_64-unknown-linux-gnu` |
| `build_linux_arm64` | `ubuntu-24.04-arm` | `aarch64-unknown-linux-gnu` |
| `build_macos_x64` | `macos-15-intel` | `x86_64-apple-darwin` |
| `build_macos_arm64` | `macos-15` | `aarch64-apple-darwin` |
| `build_android_arm64` | `ubuntu-latest` | `aarch64-linux-android` |
| `build_android_x64` | `ubuntu-latest` | `x86_64-linux-android` |

- **每个 target 都是原生同架构编译**，不引入交叉工具链（croc-rs 的核心依赖是纯 Rust 加密栈，原生编译最省事）。
- `macos-13` 已下线，x64 macOS 用 `macos-15-intel`；Windows arm64 用 `windows-11-arm`（公共仓库免费）。
- **判定平台一律用 `runner.os`**，不要拿矩阵 runner 标签比字面量（`windows-latest` 之外还有 `windows-11-arm`）。
- 默认只勾 Windows x64 + Linux x64（与 Suwayomi-next 的默认一致），Android arm64 需手动勾。

---

## 5. 各 workflow 骨架

### 5.1 `release.yml`（唯一入口）

```yaml
name: Release
on:
  push:
    branches: [main]
    paths-ignore: ['docs/**', '*.md', '**/*.md']   # 纯文档改动不出包
  workflow_dispatch:
    inputs:
      channel: { type: choice, options: [alpha, beta, release], default: alpha }
      build_windows_x64:    { type: boolean, default: true }
      build_linux_x64:      { type: boolean, default: true }
      build_windows_arm64:  { type: boolean, default: false }
      build_linux_arm64:    { type: boolean, default: false }
      build_macos_arm64:    { type: boolean, default: false }
      build_android_arm64:  { type: boolean, default: false }
      build_android_x64:    { type: boolean, default: false }
      pack_cli:             { type: boolean, default: true }
      pack_gui:             { type: boolean, default: true }

env:
  CARGO_TERM_COLOR: always

jobs:
  prep:                      # 算版本、通道后缀、拼矩阵 include（只算一次）
    outputs: { version_name, channel, channel_suffix, targets, android_targets }

  lint:
    uses: ./.github/workflows/lint.yml

  build:
    needs: [prep, lint]      # ★ 门禁不过就不进编译
    uses: ./.github/workflows/build.yml
    with:
      channel:        ${{ needs.prep.outputs.channel }}
      channel_suffix: ${{ needs.prep.outputs.channel_suffix }}
      version_name:   ${{ needs.prep.outputs.version_name }}
      targets:        ${{ needs.prep.outputs.targets }}
      android_targets:${{ needs.prep.outputs.android_targets }}
      pack_cli:       ${{ ... }}
      pack_gui:       ${{ ... }}

  publish:
    needs: [prep, build]
    steps:
      - uses: actions/download-artifact@v7
        with: { path: dist, merge-multiple: true }
      - run: cd dist && sha256sum * > SHA256SUMS.txt
      - uses: softprops/action-gh-release@v2
        with: { files: dist/*, generate_release_notes: true }
```

> `paths-ignore` 写成忽略列表而**不是**顶层 `paths:`：后者是白名单语义，会把所有代码改动的 push 一起静默挡掉。
> `*.md` 与 `**/*.md` 两条都给：`**/` 能否匹配零级目录在 glob 实现之间有歧义。

### 5.2 `build.yml`（可复用，只构建）

```yaml
name: Build
on:
  workflow_call:
    inputs:
      channel:         { required: true, type: string }
      channel_suffix:  { required: true, type: string }
      version_name:    { required: true, type: string }
      targets:         { required: true, type: string }   # JSON include
      android_targets: { required: true, type: string }
      pack_cli:        { required: false, type: boolean, default: true }
      pack_gui:        { required: false, type: boolean, default: true }

jobs:
  cli:
    if: ${{ inputs.pack_cli }}
    strategy: { fail-fast: false, matrix: { include: ${{ fromJSON(inputs.targets) }} } }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: actions/checkout@v7
      - uses: dtolnay/rust-toolchain@stable
        with: { targets: ${{ matrix.rust_target }} }
      - uses: Swatinem/rust-cache@v2
      - run: cargo build --release -p croc-cli --target ${{ matrix.rust_target }}
      - uses: actions/upload-artifact@v7
        with: { name: croc-rs-${{ inputs.version_name }}${{ inputs.channel_suffix }}-${{ matrix.target }}, path: ... }

  gui:
    if: ${{ inputs.pack_gui }}
    strategy: { fail-fast: false, matrix: { include: ${{ fromJSON(inputs.targets) }} } }
    runs-on: ${{ matrix.os }}
    steps:
      - uses: tauri-apps/tauri-action@v0       # 产出 crocw-rs 安装包
      - uses: actions/upload-artifact@v7

  android:
    if: ${{ inputs.pack_gui }}
    strategy: { matrix: { include: ${{ fromJSON(inputs.android_targets) }} } }
    runs-on: ubuntu-latest
    steps:
      - uses: nttld/setup-ndk@v1
        with: { ndk-version: r26d }
      - run: cargo tauri android build --apk --target ${{ matrix.abi }}
      - uses: actions/upload-artifact@v7
```

### 5.3 `lint.yml`（质量门禁）

```yaml
name: Lint
on:
  workflow_call:
  workflow_dispatch:

env:
  CARGO_TERM_COLOR: always

jobs:
  fmt:      # cargo fmt --all --check
  clippy:   # cargo clippy --workspace --all-targets -- -D warnings
            #   lint 级别由根 Cargo.toml [workspace.lints] 统一定义
  test:     # cargo test --workspace --no-fail-fast
  arch:     # cargo xtask purity   —— 纯 crate 不得引入 IO
            # cargo xtask deps     —— 核心 crate 不得引入 openssl / ring
  msrv:     # dtolnay/rust-toolchain@1.98 + cargo check --workspace
  miri:     # ★ 唯一使用 nightly 的地方
            #   cargo +nightly miri test -p croc-siec -p croc-pake
  interop:  # 起 Rust relay，跑 tests/interop/run.sh（I1–I18）
            #   需要 actions/setup-go + croc v11.5.4
```

- `--no-fail-fast`：默认在第一个失败的测试二进制处就收工，一轮要拿到全部失败。
- **nightly 只出现在 `miri` job**，产物构建链路（`build.yml`）里不允许出现 nightly。

### 5.4 `clear.yml`（预发布清理）

直接沿用 Suwayomi-next：手动 dispatch，输入 `window_hours`（默认 24）、`dry_run`（默认 true）、`cleanup_tag`（默认 true），每个窗口内只留最新的 1 个 alpha Release。

---

## 6. 本地等价命令

```bash
cargo xtask ci-lint      # = lint.yml 全部 job（fmt/clippy/test/purity/deps）
cargo xtask ci-build     # = build.yml 本机 target
cargo xtask ci-interop   # = interop job（需本机有 Go 与 croc 源码）
```

`xtask` 的意义：lint 不跟 PR，所以本地必须能一键跑完等价检查，否则只能靠推 main 才发现红灯。

---

## 7. 与 Suwayomi-next 的差异（有意为之）

| 项 | Suwayomi-next | croc-rs | 原因 |
|----|---------------|---------|------|
| 产物形态开关 | `pack_core` / `pack_jre` / `pack_msi` / `pack_exe` / `pack_oci` | `pack_cli` / `pack_gui` | 没有 JRE / 安装包 / 镜像需求 |
| Windows 工具链 | `msvc` / `gnullvm` / `all` | 仅 msvc | 无 bundled C 依赖，不需要 llvm-mingw |
| 外部资产 | WebUI zip、ext-runtime.jar、JRE | 无 | — |
| Miri | 无 | 有 | 本项目的 `croc-siec` / `croc-pake` 含 unsafe |
| 架构门禁 | 无 | `xtask purity` / `xtask deps` | 本项目对"纯 crate"与"纯 Rust 加密栈"有硬约束 |
| 互通测试 | 无 | `interop` job | 与 Go 版互通是本项目的生命线 |

---

## 8. 落地检查清单

- [x] 根 `Cargo.toml` 配 `[workspace.lints]`（见 [03-architecture.md](03-architecture.md) §8.2）
- [x] 根 `rustfmt.toml`：`edition = "2024"` / `max_width = 120` / `use_small_heuristics = "Max"`
- [x] `.github/workflows/{release,build,lint,clear}.yml` 四份（另有 `nightly.yml`，不属于发布链路）
- [x] `docs/agent/release.md`（操作手册；设计推导在本文档）
- [x] `xtask` 子命令：`purity` / `deps` / `ci-lint` / `ci-build` / `ci-interop`
- [x] `tests/interop/run.sh` + `tests/interop/README.md`（I1–I18）
- [x] `crates/croc-cli`（`croc-rs`）与 `crates/croc-gui`（`crocw-rs`，Tauri 2）落地
- [ ] 打第一个 alpha（推 main 验证自动链路）
- [ ] `tools/gen-golden` 在有 Go 的机器上跑一次，产出 `tests/golden/{curves,pake,pakekey}.json`
