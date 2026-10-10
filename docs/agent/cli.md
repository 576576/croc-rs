# CLI 与 Go 版的对齐与差异

> 交付物 `croc-rs`，目标是在日常使用中可直接替换 Go 版 `croc`。

## 1. 未实现项与不一致项

本节只记录两类条目：**尚未实现**的命令与开关，以及**已实现但行为与 Go 不一致**的开关。
已实现且行为一致的开关不在此列出，其定义以 `croc-rs --help` 为准；差异的完整清单见 §2。

### 1.1 顶层命令

| Go 命令 | `croc-rs` | 说明 |
|---------|-----------|------|
| `croc send` / `croc relay` / `croc store` | ✅ | |
| `croc update` | [PENDING] 只有提示 | 子命令位置留了，`--check` / `--yes` 与别名 `upgrade` 都照 Go 收下，但不做任何事：打一条 `note:` 并以**失败码**退出（见 §2.1 第 3 条） |
| `croc ssh` | [PENDING] 只有提示 | 子命令与参数（`--headless` / `--duration` / `--reconnect-window` / `--no-reconnect` / `--transport` / `--dir`）都照 Go 收下，但不做任何事：打一条 `note:` 并以**失败码**退出。它的 `--transport` 是子命令自己的，与 `send` 的同名开关无关（Go 也是这样，两者取值集不同） |
| `croc generate-fish-completion` | [PENDING] 只有提示 | 同上；并且与 Go 一样在 `--help` 里隐藏 |

> ⚠️ 顶层第一个位置参数是**口令码**。所以任何没被认出来的词（例如 `croc-rs foo`）都会被当成口令码
> 去接收，而不是报"未知命令"。

### 1.2 `send` 子命令参数

无未实现项。

### 1.3 全局参数

无未实现项。

> `--testing` 是 Go 的测试开关。本实现照收（不报"未知参数"），但它是空操作。

### 1.4 `--transport derp` —— [PENDING] 暂缓实现

Tailscale 数据面暂缓，等待上游稳定 tailcat-rs。完整方案见 [plan/tailscale.md](plan/tailscale.md)。

暂缓期间 `--transport` 的取值与行为（所有平台一致）：

| 输入 | 实际行为 |
|------|---------|
| `auto`（默认） | 等价 `relay`。本实现默认不做局域网发现，理由见 §2.2 第 10 条 |
| `relay` | 中继 |
| `derp` | 输出一条 `note:` 提示后按 `relay` 处理；不是错误，不影响退出码 |

提示原文：

```text
$ croc-rs send --transport derp file.txt
note: tailscale data plane is not available yet (pending upstream tailcat-rs); using relay
```

依据：Go 版 `resolveTransportMode()` 在 `tailcattransport.Available()` 为假时降级为 `relay` 并返回
`downgraded=true`（`src/croc/croc.go:160`），`transport_unsupported.go` 在 Android 上返回
`ErrUnsupported`。本实现与该降级等价，对端无法区分。

## 2. 与 Go 版的已知行为差异

以下条目需要在 README 中声明。

### 2.1 [PENDING] 未实现的行为

1. **`--transport derp` 暂缓**（见 §1.4）。当前行为是「提示 + 走 relay」，既不是 Go 版的 Tailcat
   匿名节点协议，也没有采用官方 `tailscale` crate——后者虽已发布，但模型不匹配。
2. **`croc ssh` 只占位**。子命令与参数都收下了，但只打一条 `note:` 并以失败码退出。Go 的 `ssh`
   会开一条端到端加密的协作终端，它依赖 PTY 与 tailcat 数据面，两者都还没有。
3. **`croc update` 与 `croc generate-fish-completion` 只占位**。两个子命令都能被解析（参数也照 Go
   收下），但只打一条 `note:` 并以**失败码**退出。Go 的 `update` 真的会自更新，`generate-fish-completion`
   真的会输出补全脚本——所以这是"未实现"，不是"行为不同"。

### 2.2 已实现，但方式与 Go 不同

1. **`--git` 的匹配基准**。Go 取 `info.Name()`（仅 basename）与 `.gitignore` 比对，因此 `src/*.log`
   这类含路径的规则在 Go 版中不生效；本实现按相对路径匹配。差异仅影响「发送哪些文件」，不涉及协议。
2. **`--hash` 缺少 `imohash-v2`**。该项是 Go 的渐进式哈希协商能力
   （`progressive-file-hash-v1`），需要额外的往返消息，本实现未协商该能力。
3. **`--socks5` / `--connect` 不实现代理认证**。地址中含 `user:pass@` 时直接报错，而不是静默忽略
   ——忽略只会让用户面对「连不上」反复排查。
4. **`--internal-dns` 的持久化条件更严格**。Go 只要 argv 中出现 `--remember` 就写出
   `<config>/internal-dns` 标记（不必同时给出 `--internal-dns`，因此一次 `croc --remember file.txt`
   便会永久启用内置 DNS），且其 argv 扫描遇到 `--internal-dns` 即终止，两个开关的先后顺序会改变
   结果。本实现要求两个开关同时给出。
5. **`--throttleUpload` 参数非法的处理方式**。Go 触发 `panic`（退出码 2 加栈回溯），本实现输出一行
   可读的错误信息并以失败码退出。两者都不静默忽略——未生效的限速比报错更难排查。
6. **剪贴板不预探测 `PATH`**。本实现按优先级依次尝试启动各平台的剪贴板程序，第一个成功的即为命中；
   Go 先做 `LookPath` 判断再启动。另：`--extended-clipboard` 复制的命令中，可执行文件名是 `croc-rs`。
7. **默认不做局域网发现**。Go 默认开启（除非显式给出 `--no-local`）；本实现只在显式给出 `--local`
   时才做组播发现，因此 `--no-local` 在 v1 中是空操作。影响：同一局域网内的传输默认仍经中继，
   需显式 `--local` 才走本地直连。
8. **拒绝命令行口令码时的退出码**。拒绝的条件与 Go 一致（非 Windows、命令行给出码、未启用 classic
   模式、环境变量未给出 `CROC_SECRET`），但 Go 以 `os.Exit(0)` 结束，本实现返回失败码：请求被拒绝
   属于失败，脚本不应据此认为传输已完成。
9. **接收端缺少口令码时的行为**。Go 一律从 stdin 读取一行；本实现仅在 stdin 为终端时询问，管道中
    直接报错，以免脚本静默挂起。
10. **配置文件是 Go 格式的子集**。两个实现共用 `<config>/send.json` 与 `<config>/receive.json`，
    字段名与 Go 的 `croc.Options` 一致，但本实现只写出 Go 也会应用的那部分字段。Go 读取本实现写出
    的文件时，其余字段取零值，与 Go 自身在未设置时写出的值相同。
11. **`croc store` 只做客户端**。Go 的 `croc-web` 既是网站又是暂存服务端；本实现只实现客户端
    （`croc-rs store`、`send --store`、`--revoke`），服务端请用 Go 的。协议名 `croc-store-v1`，
    加密格式与九个 HTTP 端点都兼容：`crates/croc-store` 的基准向量测试比对 HKDF 派生、AAD、清单
    JSON 与 Share 编码，互通由端到端验证（Rust 上传 → Go 下载，反向亦然，两个方向内容都一致）。
    上传成功后打印的过期时间用的是本实现的日期格式，Go 用 RFC1123。
12. **`--no-multi` 只影响发送端**。Go 的接收端没有这个开关，它按中继 banner 里的端口数走；
    本实现照抄这一点。想要单流，只给发送方加 `--no-multi` 即可。

### 2.3 不构成差异，但影响验证

单机上 `Rust 发送 → Go 接收` 的 `--local` 永远发现不到对端：Go 的 `peerdiscovery` 默认
`AllowSelf=false`，会丢弃源 IP 为本机的组播。Go 自身两端同机时同样失败，与本实现无关。互通矩阵的
I12 因此在单机环境跳过，验证该路径需要两台机器。

## 3. 公共中继池

默认中继（`croc.schollz.com:9009`）不是一个地址，而是一个**有序的池**：

| 索引 | 地址 |
|------|------|
| 0 | `1.getcroc.com:9009` |
| 1 | `2.getcroc.com:9009` |
| 2 | `3.getcroc.com:9009` |
| 3 | `4.getcroc.com:9009` |

口令码按 `sha256(码) mod 池长` 确定性地落在其中一台上，所以**码本身就带着"该连哪台"**
这层信息：两端各自算，不需要谁来通知谁。

* **发送端**：没给 `--code` 时先探测一遍池（并发 ping/pong，谁先回用谁），再反着生成一个
  哈希正好落在该索引上的码；给了 `--code` 就按码算索引。
* **接收端**：解析出码之后按同一个公式算出索引，直接连那一台。
* 命中条件（Go 的 `usesPublicRelay`，`cli.go:402`）：`--relay` / `--relay6` 都没在命令行或
  环境里给过、不是 `--local`、没给 `--ip`，且合并出来的地址仍是默认值。
* 上一次选中的那台记在 `<config>/best-relay`（Go 同名文件），连不上时清掉。

漏掉这一层的症状不是报错，而是**静默挂住**：两端各在自己那个房间名正确、也确实空的房间里
等一个永远不来的人，接收端一直停在 `Authenticating code`。

## 4. 界面语言与 `-h`

### 4.1 只做中文与英文

判定顺序固定，先命中先算：

1. `LC_ALL`
2. `LC_MESSAGES`
3. `LANG`
4. 系统区域（Windows 上取系统 UI 语言，其余平台读区域设置；都经 `sys-locale`）
5. 兜底英文

取值先剥掉 `.编码` 与 `@修饰符`（`zh_CN.UTF-8@euro` → `zh_cn`），前缀是 `zh` 用中文，其余一律英文。
`LANG=C` / `LANG=POSIX` 是"给我英文"的显式选择，**不会**再回落到系统区域。

> 注意 `LANG` 压得住系统区域。Git Bash 默认设 `LANG=en_US.UTF-8`，所以在那里跑的
> `croc-rs -h` 是英文；想看中文用 `LC_ALL=zh_CN.UTF-8 croc-rs -h`。

### 4.2 文案在 `locales/` 里

面向用户的句子**不写在 Rust 里**：`crates/croc-cli/locales/en.yml` 与 `zh-CN.yml` 各存一栏，
键一一对应，代码里只写键（`t!("cli.cancelled")`）。

* 文件名就是语言标签（`zh-CN` 对应 `Locale::Zh`），文件里**不带**语言根，顶层直接是
  `help:` / `cli:` / `notice:` / `store:` / `relay:` / `config:`；
* 变量写成 `%{name}`，由 `t!` 的同名实参替换
  （`t!("cli.sent_many", count = 3, size = "4.9 kB")`）；
* 缺键时 `t!` 返回**键本身**，界面上会直接出现 `cli.cancelled` 这种东西。
  `every_locale_translates_the_same_keys` 校验两种语言的键集合完全相同；
* `i18n!("locales", fallback = "en")` 放在 `src/main.rs` 的 crate 根——`t!` 展开成
  `crate::_rust_i18n_t!`，那个名字只能由宏生成；
* proc macro 不能自己声明依赖，所以 `build.rs` 里写了 `rerun-if-changed=locales`：
  没有它，改了 yml 不会重编译，界面会一直用旧表。

加一门语言 = 加一个 `<tag>.yml`（键照抄 `en.yml`）+ 在 `Locale` 里加一个分支并给出标签。

### 4.3 英文 `-h` 与 Go 版逐字一致

英文帮助**不是翻译**，而是 Go 版 `croc -h` 的原样输出，只做两处改动：

* 程序名 `croc` → `croc-rs`（`croc.schollz.com` 这类主机名不动）；
* `VERSION:` 段里的版本号——它随构建变，基准文件里写成 `<VERSION>`。

其余每一个字、每一处对齐、每一处 `(default: …)` 与 `[%VAR%]` 都与 Go 版相同。环境变量标记与 Go
一样按平台变：Windows 上 `[%CROC_RELAY%]`，其余平台 `[$CROC_RELAY]`。

对齐这件事是**手写表 + 自渲染**做的（`crates/croc-cli/src/help.rs`），没有走 clap 的渲染器——后者
的分节与占位符写法（`Usage:` / `Commands:` / `--relay <RELAY>`）与 Go 版对不上。代价是帮助表与
clap 的开关定义可能走散，所以有两条测试约束：

* `tests/golden/help/*.txt` 是与 Go 版输出的逐字对照（生成方式见该目录的 README）；
* `the_help_table_and_the_parser_agree_on_every_flag` 双向核对帮助表与 clap 的开关集。

因为版式要逐字一致，**flag 的归属也必须与 Go 一致**：`--transport` / `--no-local` / `--no-multi`
只挂在 `send` 上，全局多了 `--testing`。接收端因此没有 `--transport` / `--no-multi`——与 Go 相同。

### 4.4 英文的发送提示与 Go 同形

发送端那两行也打到 **stderr**，形状照 Go（`croc.go:1819`）：

```text
\r                                 \rSending 'a.bin' (4.9 kB)
On the other computer, run:
  croc-rs abc-def-ghi

Or open:
  https://getcroc.com/?code=abc-def-ghi
```

三条规则与 Go 一致：

* `--relay` 只在地址**不是默认值**时出现，走公共中继池时一定不出现；
* `--pass` 只在密码不是 `pass123` 时出现；
* `Or open:` 段落另起一行，前面有一个空行。

口令码用黄色高亮（Go 的 `termui.Secret`），开关不上色；文件名用粗体（§4.5 有完整的上色规则）。

`Sending` 那一行在清点完文件、开始算哈希时打（`crates/croc-engine/src/files.rs` 的 `survey`
与 `hash_all` 是分开的，就是为这个）。Go 在算哈希期间还会反复重打中间计数，本实现只打最终
一次——落盘到日志里时中间态只是噪声。

字节数用 Go 的 `utils.ByteCountDecimal`：1024 进位，单位名仍是十进制写法（`4.9 kB`）。

> 已知限制：Windows 上控制台代码页不是 UTF-8（`chcp` 显示 936）时，中文会显示成乱码。
> 这是终端解码的问题，不是输出编码的问题（程序写出的始终是 UTF-8）。Git Bash、Windows Terminal
> 或把代码页切到 65001 都正常。修它需要在启动时改控制台代码页（`unsafe` FFI），而本仓库不含
> `unsafe`，因此不修。

### 4.5 传输期的终端显示与 Go 同形

```text
Sending (->10.0.0.2)
a.bin   42% |████████            | (2.1 kB/4.9 kB, 1.2 MB/s)
```

* **方向行**每趟只打一次：发送是 `Sending (->对端)`，接收是 `Receiving (<-对端)`——措辞与箭头
  取自 Go 的 `formatTransferDirection`。对端地址有两条来源：`externalip` 消息，或者对端支持
  `inline-peer-metadata-v1` 时清单里的 `ExternalIP`（这种组合下根本不走 externalip 交换，
  清单是唯一能拿到它的地方）。
* **进度条** 20 格（Go 的 `progressBarWidth`），方块填充，满格那一帧 cyan 转 green——Go 在
  `completionColorWriter` 里做的是同一件事。百分比是**文件内**的：Go 每个文件重置一条，本实现
  同，对应 `EngineEvent::FileProgress`；整次传输的累计值另有 `EngineEvent::Progress`，每收完
  一个文件报一次（GUI 用的是这一条）。
* **接收侧的文件名行每个文件打一次**（`Receiving 'x' (4.9 kB)`），Go 只在传输开始前打一次，
  且多文件时那一行是 `N files` 加总大小而不是首文件名。多文件时本实现比 Go 多 N-1 行。
* **上报频率** 100 ms，与 Go 的进度条一致。发送侧按已写出的明文块计
  （`data.rs` 的 `UploadProgress`），接收侧按落盘块计（`data.rs` 的 `Progress::add`）。
* **`--stdout` 按文件输出**：每收完一个文件就打一次，与 Go 的 `receiveData` 一致。
  原本是整次接收完毕后统一输出，那条差异已从 §2.2 移除。
* **上色规则**在 `crates/croc-cli/src/term.rs`：口令码黄、文件名粗体，开启条件是 stderr 为
  终端、未设 `NO_COLOR`、`TERM` 非 `dumb`。

> 已知限制：Windows 上没做 Go `colorable` 那一步（用 `SetConsoleMode` 开虚拟终端）。
> Windows Terminal / VS Code 这类终端本身开着 VT，旧的 conhost 会看到转义序列而不是颜色。
> 补它需要 `unsafe` FFI，而本仓库不含 `unsafe`。
