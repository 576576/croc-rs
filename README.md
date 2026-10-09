# croc-rs

[croc](https://github.com/schollz/croc) 的 Rust 实现 —— 在任意两台电脑之间端到端加密地传文件。

[croc]: https://github.com/schollz/croc

---

## 这是什么

`croc-rs` 让你在两台电脑之间**简单地、安全地**传文件与目录，不需要架本地服务器，也不需要做
端口映射：

- **任意两台电脑**都能传（p2p，中继兜底）
- **端到端加密** —— 用 PAKE（密码认证密钥协商）协商出会话密钥，只有收发双方知道
- **跨平台** —— Windows / Linux / macOS
- **多文件与整个目录**，断点续传
- **自建中继**，也可以直连公共中继
- **口令码代替 IP** —— 双方交换一句三词口令就能连上，不用交换地址

## 两个产物

| 产物 | crate | 说明 |
|------|-------|------|
| **`croc-rs`** | [`crates/croc-cli`](crates/croc-cli) | CLI，跨平台 |
| **`crocw-rs`** | [`crates/croc-gui`](crates/croc-gui) | 图形界面，跨平台 |

GUI 与 CLI 用的是同一套协议与引擎，只是入口不同——所以下面的互通矩阵结论对两者同时成立。

## 安装

压缩包自带两个二进制，解压即用：

| OS | 资产 |
|---|---|
| Windows | `croc-rs-<版本>-windows-x64.zip` |
| Linux | `croc-rs-<版本>-linux-x64.tar.gz` |

包里含 `croc-rs`（CLI）、`crocw-rs`（GUI）、`LICENSE` 与 `README.md`。

## 手动构建

```bash
git clone https://github.com/576576/croc-rs
cd croc-rs
cargo build --release -p croc-cli
# 产物在 target/release/croc-rs（Windows 是 .exe）
```

需要 Rust stable（edition 2024，MSRV 1.98+）。

## 用法

发送一个文件或目录：

```bash
$ croc-rs send ./photos
口令码：acid-acorn-acre
让对方执行：
  croc-rs --relay croc.schollz.com:9009 acid-acorn-acre
```

在另一台电脑上接收：

```bash
$ croc-rs acid-acorn-acre
接收来自 XXX 的 3 个文件（1048576 字节）？[Y/n]
```

多个文件可以一次列出来：

```bash
croc-rs send file1 file2 folder1
```

也可以用管道：

```bash
cat [文件名] | croc-rs send      # 发送 stdin
croc-rs --yes [口令码] > out    # 接收进 stdout
```

## 选项

### 自定义口令码

```bash
croc-rs send --code [口令码] [文件]
```

口令码至少 6 个字符。不给就自动生成一句三词口令。

### 发送文本

```bash
croc-rs send --text "hello world"
```

### 自动同意

```bash
croc-rs --yes [口令码]
```

### 覆盖 / 改名

目标已存在时，默认会问你要不要覆盖。也可以直接定：

```bash
croc-rs --yes --overwrite [口令码]   # 直接覆盖（也用于续传）
croc-rs --yes --rename [口令码]      # 换一个没被占用的名字，如 video (1).mkv
```

### 关掉压缩

```bash
croc-rs --no-compress send [文件]
```

### 换加密曲线

```bash
croc-rs --curve p521 [口令码]
```

可选：`p521` / `p384` / `p256`（默认）/ `siec` / `ed25519`。**只有接收方说了算** ——
发送方跟随接收方的曲线。

### 指定落盘目录

```bash
croc-rs --out ./downloads [口令码]
```

### 安静模式

```bash
croc-rs --quiet send [文件]
```

### 调试日志

```bash
croc-rs --debug send [文件]        # 等价于 RUST_LOG=croc=debug,croc_engine=debug,croc_relay=debug
```

### 自建中继

```bash
croc-rs relay
```

默认用 TCP 端口 9009–9013（一个控制端口 + 四个数据端口）。换端口用 `--ports`（至少 2 个）：

```bash
croc-rs relay --ports 1111,1112,1113
```

给自己的中继加口令（`--pass` 是全局开关，**必须在 `relay` 之前或用环境变量给**）：

```bash
croc-rs --pass 你的口令 relay
```

用它发文件：

```bash
croc-rs --relay "myrelay.example.com:9009" --pass 你的口令 send [文件]
```

`--relay` 与 `--pass` 也可以走环境变量 `CROC_RELAY` / `CROC_PASS` / `CROC_PORTS`。

## 界面语言

界面按 `LC_ALL` → `LC_MESSAGES` → `LANG` → 系统区域依次判定 i18n。

`crates/croc-cli/locales/` 下每种语言一个 yml 文件，缺的键回退英文。

```bash
LC_ALL=zh_CN.UTF-8 croc-rs -h    # 强制中文
LC_ALL=en_US.UTF-8 croc-rs -h    # Force using English
```

英文的帮助输出与 Go 版 `croc -h` **逐字相同**（只有程序名和版本号不同），所以从 `croc` 过来的人
看到的是同一份说明。`-h` / `--help` 等价，`help` / `h` 子命令也认：

```bash
croc-rs help send                # 等价于 croc-rs send -h
```

## 与 Go 版的差距

哪些 flag 和子命令已经实现、哪些还没有，逐项列在
[docs/agent/cli.md](docs/agent/cli.md)。

## 开发

```bash
cargo xtask ci-lint      # = lint.yml 的 fmt / clippy / test / 架构门禁
cargo xtask ci-build     # = build.yml 的本机 target
cargo xtask ci-interop   # = 与 Go 版的互通矩阵（需要 Go 版 croc）

cargo xtask purity       # 纯 crate 里不许有 IO
```

**互通矩阵不在 CI 里，只在本地跑** —— 它要同一台机器上同时有 Rust 版与 Go 版，每个用例还
各自起中继与两端进程。它真正的用途是"改了协议字节之后本地跑一遍"。理由与取舍见
[docs/agent/build-and-ci.md](docs/agent/build-and-ci.md) §5.3。

质量门禁**不跟 push / PR**，而是挂在发布链路上（`build` 的 `needs` 带上 `lint`），
所以发布链路上没有绕过路径。代价是 PR 阶段没有绿灯信号——由上面这几条本地命令补。

### 架构：纯核心 + 副作用边缘

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

**本仓库是纯 Rust 仓库，不含任何 Go 源码。** 需要 Go 的辅助工具（基准向量生成器）与
互通用的 Go 版二进制都放在平级的 `croc-playground/` 里。

### 协议改动的规矩

**先改文档再改代码。** 协议理解沉淀在 [`docs/agent/`](docs/agent)（`protocol.md` / `crypto.md`），改完才动实现。兼容性知识靠基准向量与互通测试保证。

## 致谢

- [schollz/croc](https://github.com/schollz/croc) —— 协议与行为基线（MIT，© Zack Schollz）。
  本项目是独立实现，不是 fork。
- [@tscholl2](https://github.com/tscholl2) —— SIEC255 曲线参数
  （[encryption gists](https://gist.github.com/tscholl2/dc7dc15dc132ea70a98e8542fefffa28)）。
- [@warner](https://github.com/warner) —— 触发 PAKE 这个想法的
  [magic-wormhole](https://github.com/magic-wormhole/magic-wormhole)。
- [EFF 词表](https://www.eff.org/dice) —— 三词口令码的词源。

完整的第三方署名清单待打磨阶段补齐。

## 许可

**LGPL-3.0-or-later**

完整的第三方署名清单在打磨阶段统一补齐，见 [testing.md §5](docs/agent/testing.md)。