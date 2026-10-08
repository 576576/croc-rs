# 构建、发布与 CI

> 本文是 `.github/workflows/` 的**决策与设计说明**；"怎么用"在 [release.md](release.md)。
> 组织方式对齐 Suwayomi-next（同为 Rust workspace 项目），差异见 §7。

## 1. 四个 workflow 与职责边界

| 文件 | 触发 | 角色 |
|------|------|------|
| **`release.yml`** | `push` main + `workflow_dispatch` | **唯一入口**。算版本号、拼矩阵、调 lint、调 build、收产物发布 |
| **`build.yml`** | 仅 `workflow_call` | **可复用构建**。只编译打包 + `upload-artifact`，不含任何版本推导 |
| **`lint.yml`** | `workflow_call` + `workflow_dispatch` | **质量门禁**。fmt / clippy / test / 架构门禁 / MSRV |
| **`clear.yml`** | 仅 `workflow_dispatch` | 预发布清理：每个窗口内只留最新 1 个 alpha Release |

### 1.1 三条铁律

1. **质量门禁挂在发布链路上，不单独跟 push/PR。** `build` 的 `needs` 带上 `lint`——门禁不过就直接
   不进编译。所有入口都从 `release.yml` 进，**没有绕过路径**。唯一例外是纯文档 push（被
   `paths-ignore` 跳过，既不发布也不跑门禁）。
2. **`build.yml` 不重新推导任何参数。** 版本号、通道后缀、矩阵 `include` 全在 `prep` 里算一次，
   `build.yml` 只负责拼。
3. **产物形态用正交开关表达**，默认值与自动 alpha 逐项一致，所以推送与手动两条路径出的包一样，
   不必按触发方式分叉。

### 1.2 为什么 lint 不跟 PR

跟了 PR 就等于允许"PR 红了也照样发"——门禁沦为装饰。挂在 release 链路上则**物理上无法绕过**。
代价是 PR 阶段没有绿灯信号，由本地 `cargo xtask ci-lint` 补（见 §6）。

## 2. 通道与版本

| 通道 | 触发 | versionName | tag | prerelease |
|------|------|-------------|-----|-----------|
| alpha | 推送 main（自动） | `r{count}` | `r{count}-alpha.{run_id}` | ✅ |
| alpha | 手动 dispatch | `r{count}` | `r{count}-alpha.{run_id}` | ✅ |
| beta | 手动 dispatch | `1.{n/100}.{n%100 补零两位}` | `v1.y.z-beta.{run_id}` | ❌ |
| release | 手动 dispatch | `1.y.z` | `v1.y.z` | ❌ |

- `count` = 提交数（`git rev-list --count HEAD`），由 `prep` 算一次。**没有手填版本号的输入框**——
  版本号只能从提交历史推导，避免"两个 release 抢同一个号"。
- 3 段版本号的末两位**必须补零**：tag 去非数字后要能当数字比大小，`1.2.5`→125 会小于 `r1205`
  被判旧。
- **通道段只有 beta 非空**（`-beta`）：beta 与 release 共用 `1.y.z`，不区分就重名；alpha 的
  `r{count}` 本身已表明通道。
- release 同名 tag 已存在时先 `gh release delete --cleanup-tag`。

## 3. 两个产物与命名

| 产物（二进制名） | crate | 平台 |
|------|-------|------|
| **`croc-rs`** | `crates/croc-cli`（CLI） | 桌面 + 服务器全部平台；Android 不出 |
| **`crocw-rs`** | `crates/croc-gui`（Tauri 2，w = window） | Windows / macOS / Linux / Android |

> **crate 名不带 `-rs` 后缀**（`croc-cli` / `croc-gui`），`-rs` 只出现在**二进制名**上。

```
croc-rs-{VER}{通道段}-{TGT}.{ext}      ← 桌面：包里同时装 croc-rs 与 crocw-rs
crocw-rs-{VER}{通道段}-{TGT}.apk       ← Android：只有 GUI，本来就是单独一个文件

例：
croc-rs-1.0.00-linux-x64.tar.gz        （内含 croc-rs、crocw-rs、LICENSE、README.md）
croc-rs-r1205-alpha.12345-windows-x64.zip
crocw-rs-1.0.00-android-arm64.apk
```

- `TGT`：`windows-x64` / `windows-arm64` / `linux-x64` / `linux-arm64` / `macos-x64` /
  `macos-arm64` / `android-arm64` / `android-x64`。
- Windows 上 `croc-rs.exe` 是 console subsystem，`crocw-rs.exe` 由 Tauri 产出（windows subsystem，
  不弹控制台）。
- Windows 与 Linux/macOS 的压缩格式不同（`zip` / `tar.gz`）：Windows runner 上 `tar` 与 `zip` 的
  行为差异最容易出坑，各用各的原生格式最省心。

### 3.0 为什么两个二进制共用同一个包

`croc-rs` 与 `crocw-rs` **共用同一套协议后端**（`croc-core` + `croc-engine`），所以它们的版本必须
一致。分开出包就给了"CLI 是 r17、GUI 是 r19"这种组合存在的机会，而那正是互传时最难查的一类问题
（"为什么我这边连不上你那边"）。

一个 target 一个归档，名字用 CLI 的（它是主入口），里面两个二进制都在。唯一例外是 Android：它只有
GUI，出的是 APK，本来就是单独一个文件。

### 3.1 产物形态开关（dispatch 输入）

| 开关 | 默认 | 含义 |
|------|------|------|
| `pack_cli` | ☑ | 出 `croc-rs` |
| `pack_gui` | ☑ | 出 `crocw-rs`（含 Android APK） |

两者正交；**都关则没有可发布的包**，`prep` 里预先拦一道（别让每个 target 白跑一遍编译）。
同理，勾了 Android 目标却关掉 `pack_gui` 也会被 `prep` 拦下——APK 由 GUI 产出。

## 4. 构建目标与 runner（原生同架构，不用 cross）

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

- **每个 target 都是原生同架构编译**，不引入交叉工具链。
- `macos-13` 已下线，x64 macOS 用 `macos-15-intel`；Windows arm64 用 `windows-11-arm`。
- **判定平台一律用 `runner.os`**，不要拿矩阵 runner 标签比字面量（`windows-latest` 之外还有
  `windows-11-arm`）。
- 默认只勾 Windows x64 + Linux x64，与自动 alpha 的固定矩阵一致，两条路径出的包形态相同。

## 5. 各 workflow 的形状与决策

> 具体的 job 步骤、input 默认值与 `run:` 命令以 `.github/workflows/*.yml` 为**唯一来源**，
> 本文只记形状与那些不看代码就想不出来的决定。

### 5.1 `release.yml`（唯一入口）

四个 job；`build` 显式依赖前两个：

| job | 内容 |
|-----|------|
| `prep` | 算一次版本号、通道后缀、矩阵 `include`，作为 output 往上传 |
| `lint` | 调 `lint.yml` |
| `build` | **`needs: [prep, lint]`** —— 门禁不过就不进编译 |
| `publish` | 收产物、生成 `SHA256SUMS.txt`、发 Release |

两个容易写错的地方：

- `paths-ignore` 写成**忽略列表**，而不是顶层 `paths:`。后者是白名单语义，会把所有代码改动的
  push 一起静默挡掉。
- `*.md` 与 `**/*.md` 两条都给：`**/` 能否匹配零级目录在 glob 实现之间有歧义。

### 5.2 `build.yml`（可复用，只构建）

三个 job 都只做"编译 + `upload-artifact`"，不含任何版本推导：

| job | 门控 | 内容 |
|-----|------|------|
| `cli` | `inputs.pack_cli` | 按 `inputs.targets` 矩阵 `cargo build --release -p croc-cli` |
| `gui` | `inputs.pack_gui` | 按同一矩阵走 `tauri-action` |
| `android` | `inputs.pack_gui` | 按 `inputs.android_targets` 矩阵，先 `setup-ndk` 再 `cargo tauri android build --apk` |

全部 `fail-fast: false`——一轮要拿到所有平台的失败，而不是第一个就收工。

### 5.3 `lint.yml`（质量门禁）

```yaml
jobs:
  fmt:      # cargo fmt --all --check
  clippy:   # cargo clippy --workspace --all-targets -- -D warnings
  test:     # cargo test --workspace --no-fail-fast
  arch:     # cargo xtask purity   —— 纯 crate 不得引入 IO
  msrv:     # cargo +1.98 check --workspace --all-targets
```

**互通矩阵不进 CI，只在本地跑**（`cargo xtask ci-interop`）。这是一个**有意的决定**，理由不是它的
通过率，而是它的形态：

| 维度 | 矩阵的情况 | 对 CI 的含义 |
|---|---|---|
| 依赖 | 同一台机器上要有 Rust 版 + Go 版 croc（Go 版在平级的 `croc-playground/`）+ Go 工具链 | runner 上得多编一份 Go 出来，还要拉外部依赖 |
| 时长 | 每个用例各自起中继、拉起两端进程、传文件，是**分钟级** | `build` 的 `needs` 要等它，发布链路整体变慢 |
| 稳定性 | 端口探测、进程时序、Windows 句柄释放都有竞态（见 `tests/interop/README.md` 的「harness 自身踩过的坑」） | 偶发红灯，且失败时不在代码而在环境 |
| 用途 | "改了协议字节之后本地跑一遍" | 门禁的职责是挡住坏提交，不是提供验证手段 |

**这个决定可以推翻**——如果哪天矩阵稳定到可以在 Linux runner 上复现（比如把端口分配改成固定端口池、
把进程等待改成带超时的轮询），再讨论挂回 `lint.yml`。

> 顺带记一个坑：`continue-on-error` 的 job **依然会拖住整个可复用工作流**，而 `build` 的
> `needs: [prep, lint]` 等的是**整个 lint**。第一次推 CI 时 fmt / clippy / test / arch / MSRV 全绿，
> 但两个"只告警"的 job 还在跑，`build` 一直等不到——看板串在链路上就会把链路掐断。

- `--no-fail-fast`：默认在第一个失败的测试二进制处就收工，一轮要拿到全部失败。
- **全仓只用 stable**，产物构建链路里更不允许出现 nightly。
- 跑 `--workspace` 的 job（clippy / test / msrv）都要装 GTK / WebKitGTK：`croc-gui` 依赖 tauri，
  而 tauri 在 Linux 上要链接 glib / GTK / WebKitGTK。缺了会停在 `glib-sys` 的 build script 上，
  报错只有一句 "failed to run custom build command"。

### 5.4 `clear.yml`（预发布清理）

手动 dispatch，输入 `window_hours`（默认 24）、`dry_run`（默认 true）、`cleanup_tag`（默认 true），
每个窗口内只留最新的 1 个 alpha Release。

## 6. 本地等价命令

```bash
cargo xtask ci-lint      # = lint.yml 全部 job（fmt / clippy / test / purity）
cargo xtask ci-build     # = build.yml 本机 target
cargo xtask ci-interop   # = 互通矩阵（只在本地跑）
```

`xtask` 的意义：lint 不跟 PR，所以本地必须能一键跑完等价检查，否则只能靠推 main 才发现红灯。

## 7. 与 Suwayomi-next 的差异（有意为之）

| 项 | Suwayomi-next | croc-rs | 原因 |
|----|---------------|---------|------|
| 产物形态开关 | `pack_core` / `pack_jre` / `pack_msi` / `pack_exe` / `pack_oci` | `pack_cli` / `pack_gui` | 没有 JRE / 安装包 / 镜像需求 |
| Windows 工具链 | `msvc` / `gnullvm` / `all` | 仅 msvc | 无需 llvm-mingw |
| 外部资产 | WebUI zip、ext-runtime.jar、JRE | 无 | — |
| Miri | 无 | 无 | 本仓库 0 处 `unsafe`、0 处 FFI，安全 Rust 产不出 UB——见 [architecture.md](architecture.md) §7.2 |
| 架构门禁 | 无 | `xtask purity` | 本项目的纯 crate 必须保持 sans-io |
| 互通测试 | 无 | 无（**有意只在本地跑**，见 §5.3） | 与 Go 版互通是生命线，但它要同机双实现、分钟级、有环境竞态，不适合当门禁 |

## 8. 本机踩过的坑（Windows 开发机）

这些都是**实际踩过并确认过根因**的，不是猜测。换机器不一定复现，但同类错误会换皮出现。

- **`bash` 有两个**：`which bash` 给的是 Git Bash 的 `/bin/bash`，但 `C:\Windows\System32\bash.exe`
  （WSL）也在 `PATH` 里且优先级更高。**Rust 子进程按 `PATH` 找会选到 WSL 那个**，于是 Windows 路径
  里的反斜杠被当转义符。凡是 Rust 代码要起 bash，都得显式找 Git for Windows 的 `bash.exe`，别写
  `Command::new("bash")`。
- **Windows + Defender 下文件打开很贵**：2 万空文件 `files::collect` ≈ 17 s，Go 版同场景 ≈ 98 s。
  所以**别拿"几万文件"的用例去判断实现效率**，先怀疑环境。
- **重编 `croc-rs.exe` 前先 `taskkill //F //IM croc-rs.exe //T`**，否则 `cargo build` 报
  `os error 5`（后台中继还占着文件）。
- **`git push` 要重试**：代理 `127.0.0.1:7890` 会间歇性对 `github.com` 失败。用
  `git -c http.version=HTTP/1.1 push`（`-c` 必须放在 `push` **之前**），必要时
  `env -u HTTPS_PROXY -u HTTP_PROXY ...` 走直连。两条路都试一遍再下结论。
- ⚠️ 写重试循环时别用 `if git push ... | tail -3; then`——**管道退出码是 `tail` 的**，恒为 0，于是
  "推失败"会被判成成功。用 `if out=$(git push ... 2>&1); then`，或者 `set -o pipefail`。
- 仓库是**私有的**：`gh api` 能用，匿名 `api.github.com` 会返回 404。
- 全 prerelease 的仓库 `/releases/latest` **返回 404**（它只给正式版），要用
  `gh api repos/.../releases --jq '.[0]'`。资产名里的版本号也不等于 tag（tag 是
  `r26-alpha.<run id>`，资产名是 `croc-rs-r26-...`），按 tag 拼下载 URL 同样 404。
- 命令行里带 `${...}` 的字符串会被外层预处理器拒掉（`Bad substitution`），即便写在带引号的 heredoc
  里也一样——避免在命令里拼 shell 变量展开。
- **带 CJK 的多行文本用 Python 脚本改，别塞进 heredoc**：shell 传参时的编码转换会吃掉字符，症状是
  文件里出现 U+FFFD 替换字符。改完扫一遍确认没有：
  `grep -rl $'\ufffd' --include='*.md' --include='*.rs' .`。
- **`Edit` 工具改 CRLF 文件会因精确匹配失败**：先读文件确认实际行尾，或直接用 Python 按字节处理
  （`read_bytes().decode('utf-8')` → 替换 → `write_bytes`）。
