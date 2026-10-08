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
| `croc ssh` | [PENDING] 只有提示 | 子命令与参数（`--headless` / `--duration` / `--reconnect-window` / `--no-reconnect` / `--dir`，以及全局的 `--transport`）都照 Go 收下，但不做任何事：打一条 `note:` 并以**失败码**退出 |
| `croc generate-fish-completion` | [PENDING] 只有提示 | 同上；并且与 Go 一样在 `--help` 里隐藏 |

> ⚠️ 顶层第一个位置参数是**口令码**。所以任何没被认出来的词（例如 `croc-rs foo`）都会被当成口令码
> 去接收，而不是报"未知命令"。

### 1.2 `send` 子命令参数

| Go flag | `croc-rs` | 说明 |
|---------|-----------|------|
| `--ts-authkey` | [PENDING] 暂缓 | 见 §1.4 |

### 1.3 全局参数

无未实现项。

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
2. **`--no-multi` 的作用域**。Go 只把它挂在 `send` 子命令上，接收方无法给出；本实现做成全局参数，
   否则它对接收方无从生效。
3. **`--hash` 缺少 `imohash-v2`**。该项是 Go 的渐进式哈希协商能力
   （`progressive-file-hash-v1`），需要额外的往返消息，本实现未协商该能力。
4. **`--socks5` / `--connect` 不实现代理认证**。地址中含 `user:pass@` 时直接报错，而不是静默忽略
   ——忽略只会让用户面对「连不上」反复排查。
5. **`--stdout` 的输出时机**。本实现在全部文件接收完毕后统一输出；Go 每收完一个文件即输出一次。
   内容与顺序一致，仅时机不同。
6. **`--internal-dns` 的持久化条件更严格**。Go 只要 argv 中出现 `--remember` 就写出
   `<config>/internal-dns` 标记（不必同时给出 `--internal-dns`，因此一次 `croc --remember file.txt`
   便会永久启用内置 DNS），且其 argv 扫描遇到 `--internal-dns` 即终止，两个开关的先后顺序会改变
   结果。本实现要求两个开关同时给出。
7. **`CROC_SECRET` 仅作兜底**。命令行的位置参数优先于该环境变量；Go 的 `resolveSendSharedSecret`
   （`cli.go:395`）则是环境变量优先——原因是 Go 的 `send` 默认不允许在命令行上给出码。本实现两条
   路径都保留，因此以更明确的那条为准。
8. **`--throttleUpload` 参数非法的处理方式**。Go 触发 `panic`（退出码 2 加栈回溯），本实现输出一行
   可读的错误信息并以失败码退出。两者都不静默忽略——未生效的限速比报错更难排查。
9. **剪贴板不预探测 `PATH`**。本实现按优先级依次尝试启动各平台的剪贴板程序，第一个成功的即为命中；
   Go 先做 `LookPath` 判断再启动。另：`--extended-clipboard` 复制的命令中，可执行文件名是 `croc-rs`。
10. **默认不做局域网发现**。Go 默认开启（除非显式给出 `--no-local`）；本实现只在显式给出 `--local`
    时才做组播发现，因此 `--no-local` 在 v1 中是空操作。影响：同一局域网内的传输默认仍经中继，
    需显式 `--local` 才走本地直连。
11. **拒绝命令行口令码时的退出码**。拒绝的条件与 Go 一致（非 Windows、命令行给出码、未启用 classic
    模式、环境变量未给出 `CROC_SECRET`），但 Go 以 `os.Exit(0)` 结束，本实现返回失败码：请求被拒绝
    属于失败，脚本不应据此认为传输已完成。
12. **接收端缺少口令码时的行为**。Go 一律从 stdin 读取一行；本实现仅在 stdin 为终端时询问，管道中
    直接报错，以免脚本静默挂起。
13. **配置文件是 Go 格式的子集**。两个实现共用 `<config>/send.json` 与 `<config>/receive.json`，
    字段名与 Go 的 `croc.Options` 一致，但本实现只写出 Go 也会应用的那部分字段。Go 读取本实现写出
    的文件时，其余字段取零值，与 Go 自身在未设置时写出的值相同。
14. **`croc store` 只做客户端**。Go 的 `croc-web` 既是网站又是暂存服务端；本实现只实现客户端
    （`croc-rs store`、`send --store`、`--revoke`），服务端请用 Go 的。协议名 `croc-store-v1`，
    加密格式与九个 HTTP 端点都兼容：`crates/croc-store` 的黄金向量测试比对 HKDF 派生、AAD、清单
    JSON 与 Share 编码，互通由端到端验证（Rust 上传 → Go 下载，反向亦然，两个方向内容都一致）。
    上传成功后打印的过期时间用的是本实现的日期格式，Go 用 RFC1123。

### 2.3 不构成差异，但影响验证

单机上 `Rust 发送 → Go 接收` 的 `--local` 永远发现不到对端：Go 的 `peerdiscovery` 默认
`AllowSelf=false`，会丢弃源 IP 为本机的组播。Go 自身两端同机时同样失败，与本实现无关。互通矩阵的
I12 因此在单机环境跳过，验证该路径需要两台机器。
