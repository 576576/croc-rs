# 发布与 CI（操作手册）

> 本文档是 `.github/workflows/` 的**决策与操作说明**。设计推导过程见
> [plan/09-build-and-ci.md](plan/09-build-and-ci.md)；本文只讲"怎么用、为什么这么用"。
>
> 对齐 `Suwayomi-next` 的组织方式（同为 Rust workspace 项目），差异清单见 plan/09 §7。

---

## 1. 我该从哪里触发发布

**唯一入口是 `release.yml`。** 其他三个 workflow 都不接受外部触发（`build.yml` 只有
`workflow_call`），所以发布链路上不存在绕过质量门禁的路径。

| 我想做的事 | 怎么做 |
|-----------|--------|
| 出一次日常构建 | 往 `main` 推代码 —— 自动出 alpha |
| 出 beta / release | Actions → Release → Run workflow，选通道 + 平台 + 形态 |
| 只跑质量门禁 | Actions → Lint → Run workflow（或本地 `cargo xtask ci-lint`） |
| 清理堆积的 alpha | Actions → Clear Pre-release → Run workflow（默认 dry run） |
| 每夜长跑 / 基准 | 自动（UTC 18:00），也可手动跑 Nightly |

推 `main` 时**只改了文档不会出包**：`release.yml` 用 `paths-ignore` 忽略 `docs/**` 与
所有 `*.md`。注意写的是忽略列表而**不是**顶层 `paths:` —— 后者是白名单语义，会把所有
代码改动的 push 一起静默挡掉。

---

## 2. 版本号与通道

| 通道 | 触发 | versionName | tag | prerelease |
|------|------|-------------|-----|-----------|
| alpha | 推送 main（自动） | `r{count}` | `r{count}-alpha.{run_id}` | ✅ |
| alpha | 手动 dispatch | `r{count}` | `r{count}-alpha.{run_id}` | ✅ |
| beta | 手动 dispatch | `1.{count/100}.{count%100}` | `v1.y.z-beta.{run_id}` | ❌ |
| release | 手动 dispatch | `1.y.z` | `v1.y.z` | ❌ |

- `count` = `git rev-list --count HEAD`，由 `prep` 算一次。**没有手填版本号的输入框** ——
  版本号只能从提交历史推导，避免"两个 release 抢同一个号"。
- 3 段版本号的末两位**必须补零**（`1.2.05` 而不是 `1.2.5`）：tag 去非数字后要能当数字比
  大小，`1.2.5` → `125` 会小于 `r1205` 被判成旧版本，`1.2.05` → `10205` 才对。
- **通道段只有 beta 非空**（`-beta`）：beta 与 release 共用 `1.y.z` 版本名，不区分就会重名；
  alpha 的 `r{count}` 本身已表明通道，不再加段。
- release 通道重复发布同一个版本号时，`publish` 会先 `gh release delete --cleanup-tag`。

---

## 3. 产物

两个二进制，crate 名不带 `-rs` 后缀，`-rs` 只出现在**二进制名**上：

| 产物 | crate | 平台 |
|------|-------|------|
| **`croc-rs`** | `crates/croc-cli` | 桌面 + 服务器全部平台；Android 不出 |
| **`crocw-rs`** | `crates/croc-gui`（Tauri 2，w = window） | Windows / macOS / Linux / Android |

```
croc-rs-{VER}{通道段}-{TGT}.{ext}
crocw-rs-{VER}{通道段}-{TGT}.{ext}

例：
croc-rs-1.0.00-linux-x64.tar.gz
croc-rs-r1205-alpha.12345-windows-x64.zip
crocw-rs-1.0.00-beta.12345-windows-x64.zip
crocw-rs-1.0.00-android-arm64.apk
```

`TGT` ∈ `windows-x64` / `windows-arm64` / `linux-x64` / `linux-arm64` / `macos-x64` /
`macos-arm64` / `android-arm64` / `android-x64`。

Windows 与 Linux/macOS 的压缩格式不同（`zip` / `tar.gz`），与 Suwayomi-next 一致：
Windows runner 上 `tar` 与 `zip` 的行为差异最容易出坑，各用各的原生格式最省心。

### 3.1 形态开关

| 开关 | 默认 | 含义 |
|------|------|------|
| `pack_cli` | ☑ | 出 `croc-rs` |
| `pack_gui` | ☑ | 出 `crocw-rs`（含 Android APK） |

两者正交。**都关则没有可发布的包** —— `prep` 里预先拦一道，免得每个 target 白跑一遍编译
才报错。同理，勾了 Android 目标却关掉 `pack_gui` 也会被 `prep` 拦下（APK 由 GUI 产出）。

### 3.2 构建目标

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

- **每个 target 都是原生同架构编译**，不引入交叉工具链 —— 核心依赖是纯 Rust 加密栈，
  原生编译最省事，也避免交叉工具链本身成为故障源。
- 默认只勾 Windows x64 + Linux x64，与自动 alpha 的固定矩阵**逐项一致**，所以推送与手动
  两条路径出的包形态相同。
- `macos-13` 已下线，x64 macOS 用 `macos-15-intel`；Windows arm64 用 `windows-11-arm`。
- 判定平台一律用 `runner.os`，不要拿矩阵 runner 标签比字面量（`windows-latest` 之外还有
  `windows-11-arm`）。

---

## 4. 质量门禁

`lint.yml` 的 job 与本地等价命令：

| job | 内容 | 本地等价 |
|-----|------|---------|
| `fmt` | `cargo fmt --all --check` | 同 |
| `clippy` | `cargo clippy --workspace --all-targets -- -D warnings` | 同 |
| `test` | `cargo test --workspace --no-fail-fast` | 同 |
| `arch` | `cargo xtask purity` + `cargo xtask deps` | 同 |
| `msrv` | `cargo +1.98 check --workspace --all-targets` | 同 |
| — | （Miri 与互通**不在** lint 里，见下） | — |

`cargo xtask ci-lint` 一把跑完 `fmt` / `clippy` / `test` / `arch` 四项（MSRV 与 Miri 需要
额外工具链，单独跑）。

**为什么 lint 不跟 PR：** 跟了 PR 就等于允许"PR 红了也照样发" —— 门禁沦为装饰。挂在
`build` 的 `needs` 上则物理上无法绕过。代价是 PR 阶段没有绿灯信号，由本地
`cargo xtask ci-lint` 补。

**Miri 与 Go 互通不在 `lint.yml`，在 `nightly.yml`。** 不是"太慢"，而是它们目前只告警
（`continue-on-error`）——而 `continue-on-error` 的 job **依然会拖住整个可复用工作流**，
`build` 的 `needs: [prep, lint]` 等的是整个 lint，于是看板把发布链路掐断了。
看板待在旁边，等绿了再挪回门禁里。

**nightly 只出现在 `nightly.yml` 的 `miri` job 里。** 产物构建链路（`build.yml`）里不允许出现 nightly。

---

## 5. 常见故障

| 症状 | 原因 | 处置 |
|------|------|------|
| `prep` 报"未选择任何构建目标" | 所有平台开关都没勾 | 至少勾一个 |
| `prep` 报"没有可发布的包" | `pack_cli` 与 `pack_gui` 都关了 | 至少开一个 |
| `prep` 报"勾了 Android 目标但 pack_gui 是关的" | Android 只由 GUI 产出 | 打开 `pack_gui` 或取消 Android |
| `build` 的 job 全部 `skipped` | 调用方传了空矩阵（`[]`） | 正常行为，不是故障 |
| Android job 找不到 APK | Tauri 版本换了输出路径 | 看日志里打印的 `outputs` 目录树，更新 `build.yml` 的 `find` 模式 |
| `publish` 报"没有收到任何发布产物" | 白名单前缀不匹配（产物名改了） | 同步更新 `publish` 里的 `case` 前缀 |
| alpha Release 堆满页面 | 自动 alpha 每次推送都出 | 跑 Clear Pre-release（先 dry run） |
