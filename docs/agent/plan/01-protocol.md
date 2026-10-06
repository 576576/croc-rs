# 01 · 协议兼容基线（croc v11.5.4）

> 本文是 croc-rs 的**唯一真相来源**。所有实现细节以 Go 源码 + 本文描述为准，任何与实测不符之处以实测为准并回来修订本文。
> 引用格式 `src/xxx.go:NNN` 指 `schollz/croc` v11.5.4。

---

## 1. 术语与角色

| 术语 | 含义 |
|------|------|
| **relay / 中继** | 控制面服务器。默认 `croc.schollz.com:9009`（v4）/ `croc6.schollz.com:9009`（v6）。同时可承担数据面转发。 |
| **room / 房间** | 中继上的 rendezvous 标识。三词码的第一个词经哈希后得到，格式见 §3。 |
| **sender / 发送方** | PAKE 角色 **B（role=1）**，在 `pakekey` 中恒为 responder。 |
| **receiver / 接收方** | PAKE 角色 **A（role=0）**，恒为 initiator，负责先发 PAKE 消息。 |
| **code / 三词码** | 用户可见的口令，形如 `apples-bicycle-cactus`。 |
| **weakKey** | 中继密码（默认 `pass123`），用于客户端↔中继的 PAKE。 |
| **secret / PAKE passphrase** | 三词码的后两个词，用于 peer↔peer 的 PAKE。 |

⚠️ **角色恒定**：croc 的 `pakekey.Identities()` 明确写死 "Party A is always the receiver and party B is the sender"（`src/pakekey/pakekey.go:49-50`），且参与密钥派生。Rust 实现绝不能按"谁先连上"来分配角色。

---

## 2. 线路帧格式（`src/comm/comm.go`）

所有 TCP 连接（客户端↔中继、客户端↔客户端）统一使用：

```
+---------------+---------------------+------------------+
| "croc" (4 B)  | length (u32, LE)    | body (length B)  |
+---------------+---------------------+------------------+
```

| 字段 | 值 |
|------|-----|
| MAGIC | ASCII `croc`（`0x63 0x72 0x6f 0x63`），**不是** 0 结尾 |
| length | **小端** `u32`，body 字节数 |
| 单帧上限 | `maxReadMessageSize = 64 MiB`；超过即 `ErrMessageTooLarge` 并断开 |
| keepalive | body 为单字节 `0x01` 的帧，接收方必须忽略并继续读（`src/croc/croc.go:2295`） |

实现要点：
- 写侧用 `write_vectored`（`net.Buffers`）一次性写 header+body，避免 Nagle 造成的小包；Rust 侧建议 `write_all_vectored` 或直接拼一个 buffer（`tokio::io::AsyncWriteExt::write_all_vectored` 仍是 nightly，稳妥做法是预分配 `4+4+len` 的 buffer）。
- 读侧先 `read_exact(8)`，校验 magic，再 `read_exact(body)`。**必须先清零旧 deadline 再设新 deadline**（Go 版在 `readWithDeadlineInto` 里有注释说明这是修过的 bug）。
- 默认读写 deadline 3 小时（`comm.New`）；body 读取 deadline 10 分钟（`messageBodyReadTimeout`），用于容纳巨大的断点续传控制消息。

---

## 3. 三词码（codephrase，`src/codephrase/codephrase.go`）

- 词表：**EFF Short Wordlist #1**，1296 个词，`wordlists/eff-short-wordlist-1.txt`（需原样搬运，注意 Go 版会把 CRLF 归一为 LF）。
- 默认格式 `FormatThreeWord`：`word1-word2-word3`
  - `word1` → 房间：不是直接用单词，而是 `sha256(word1 + "croc")` 后取一定长度（见 `roomHashSuffix`）作为 room name。
  - `word2 + word3` → PAKE passphrase（拼接方式见源码，`Parse()` 会返回 `PAKEPassphrase`）。
- 兼容旧格式：`legacy`（首 4 字节为 room）、`four-word`（前两词为 room）。Rust 端**必须实现解析**（被动兼容），生成只用 three-word。
- 最短 6 字符限制（`ErrCodeTooShort`）。

> 实现动作：把 `eff-short-wordlist-1.txt` 复制到 `crates/croc-core/src/codephrase/wordlist.txt`，并用 Go 版生成 20 组 `code → (room, passphrase)` golden 向量放进 `tests/golden/codephrase.json`。

---

## 4. 客户端 ↔ 中继握手（`src/tcp/tcp.go`）

### 4.1 控制面握手的完整时序

```
C:  A.Bytes()                       # PAKE(siec, role=0) 的公开值，JSON
S:  B.Bytes()                       # PAKE(siec, role=1) 的公开值
    [双方各自得到 strongKey = PAKE session key]
C:  salt (8 字节明文帧)              # crypt.New(strongKey, nil) 生成的随机 salt
C:  Encrypt(password)                # 用 pbkdf2(strongKey, salt, iter=100, keylen=32, sha256)
C:  Encrypt(room)                    # 管线化：紧跟 password 之后发送，不等 banner
S:  Encrypt(banner + "|||" + remoteAddr [+ "|||" + capability])
S:  Encrypt("ok")                    # 或 "rate limited" / 其它拒绝原因
```

失败分支：密码错误时服务端回 `Encrypt("bad password")`；客户端据此报 `bad password`。

关键细节：
1. 曲线**固定 `siec`**（SIEC255），与 `--curve` 无关。`src/tcp/tcp.go:406`（服务端）与 `:915`（客户端）。
2. `weakKey` = 中继密码，默认 `pass123`。
3. 会话密钥派生：`crypt.New(strongKey, salt)` = `pbkdf2.Key(strongKey, salt, 100, 32, sha256.New)`，迭代次数是 **100**（非常低，但必须一致）。
4. 加密是 AES-256-GCM，nonce 12 字节随机前置，输出 = `nonce || ciphertext || tag`（`src/crypt/crypt.go:56-78`）。
5. banner 形如 `9009,9010,9011,9012,9013`，是中继开放的数据端口列表；客户端用它建立多条数据连接做多路复用。
6. `capability` 只在升级过的中继（fast admission）出现，可为空。

> ⚠️ **管线化陷阱**：客户端在收到 banner **之前**就发送了 room（`src/tcp/tcp.go:964-979`）。服务端实现必须容忍"密码帧之后紧接着还有一帧已缓冲"，否则会错序。这是 v11 的行为，Rust 两端都要照做。

### 4.2 Ping

`PingServer`：客户端直接发 ASCII `"ping"`，服务端回 `"pong"`，不经过 PAKE（服务端在 `clientHandshake` 最开头判断 `bytes.Equal(Abytes, []byte("ping"))`）。用于中继健康检查/测延迟。

### 4.3 房间与数据中继

- 控制连接加入 room = 三词码导出的 room name。
- 数据连接加入 room = `{room}-{j}`（`src/croc/croc.go:2839`），`j` 从 1 开始，每个数据端口一条连接。
- 中继把同房间的两条连接 `pipe` 起来（`src/tcp/tcp.go:643`），即纯字节转发，不理解内容。
- 房间超时清理：`deleteOldRooms()`；握手有独立 timeout 与 pending 上限。

### 4.4 服务端必须实现的其它行为

| 行为 | 说明 |
|------|------|
| 准入限流 | 每个源 IP、每个 room 在滑动窗口内的 join 次数上限；超限回 `rate limited` |
| 最大房间数 | `max-rooms-open` |
| 未完成握手上限 | `max-pending-handshakes` |
| 握手超时 | `handshake-timeout` |
| 首帧协议观察 | `firstFrameObserver` 用于统计房间用途（可选，v1 可跳过） |

---

## 5. Peer ↔ Peer PAKE（`src/pakekey` + `src/croc/croc.go`）

### 5.1 时序

```
R(receiver, role=0/A):  Init(secret, 0, curve, "peer-transfer", room)
R → S:  Message{ t:"pake", v:2, b:<Pake public JSON>, b2:<curve 名>, f:[features] }
S(sender,  role=1/B):  Init(secret, 1, curve, "peer-transfer", room)
S:        Update(initiator) → Y
S:        SessionKey()
S:        salt = 32 字节随机
S:        Derive(sharedKey, {purpose, room, curve, initiator, responder, salt}) → Keys
S → R:  Message{ t:"pake", v:2, b:<Pake public JSON>, b2:<salt 32B>, f:[features] }   # 注意：未加密
R:        Update(responder) → SessionKey → Derive(同一 salt) → Keys
R → S:  Message{ t:"pake-confirm", v:2, b:ConfirmationA }
S:        校验 ConfirmationA；
S → R:  Message{ t:"pake-confirm", v:2, b:ConfirmationB }
R:        校验 ConfirmationB
双方:      c.Key = Keys.EncryptionKey (32B)，后续所有消息与数据用 AES-256-GCM 加密
```

要点：
1. **前两帧 `pake` 是明文发送的**（`message.Send(..., nil, ...)`，key 传 nil，`src/croc/croc.go:2703`、`:2730`）。
2. `Version` 必须等于 `pakekey.ProtocolVersion = 2`，否则 `incompatiblePakeVersionError`。
3. salt 长度必须 32（`SaltSize`）。
4. 确认标签：receiver 发 `ConfirmationA`，sender 发 `ConfirmationB`，各 32 字节 HMAC-SHA256。
5. `pake-confirm` 也是明文发送（此时 `c.Key` 还没设置）。
6. 从 `pake-confirm` 之后开始，所有 `Message` 用 `c.Key` 加密；`activateSecureChannel` 之后才建立数据连接。

### 5.2 Feature 协商

`Message.Features []string` 与 `SenderInfo.Features` / `RemoteFileRequest.Features` 承载能力协商。v11.5.4 已知：

| feature | 含义 | v1 是否实现 |
|---------|------|------------|
| `per-file-compression-v1` | 逐文件压缩标记 | ✅ |
| `inline-peer-metadata-v1` | 内联 peer 元数据 | ✅ |
| `progressive-file-hash-v1` | 渐进式哈希 | ✅（至少能识别） |
| `chunked-file-info-v1` | 分块文件清单 | ✅ |
| `staged-transport-v1` | 分阶段数据面建立 | ✅ |
| `implicit-tailcat-ready-v1` | Tailcat/DERP 就绪（匿名 Tailscale 节点数据面） | ❌ v1 不声明 |

**降级策略**：Rust 端不声明 tailcat 相关 feature，Go 端会自动走 relay 数据面——这与 croc 自身在 Android 或 `croc_no_tailcat` 构建下的行为完全一致（`src/tailcattransport/transport_unsupported.go`），是协议内的既定降级路径，不是错误。

### 5.3 数据面协商消息（v1 需识别，不必实现）

Go 版在 PAKE 之后可能发：

```
sender  → receiver: tailcat-offer        # CBOR ConnBlob（≤ 4 KB），含临时节点公钥与 DERP region
receiver → sender:  tailcat-status       # 建立进度 / 失败原因
sender  → receiver: transport-select     # 最终选定数据面（relay 或 tailcat）
```

Rust 端 v1 的行为：
1. **不主动发送** `tailcat-offer`（因为不声明 `implicit-tailcat-ready-v1`，Go 端根本不会走到这条路径）。
2. **若收到** `tailcat-offer`（例如对端是强行开启的 Go 构建）：回复一个 `tailcat-status` 表示不支持，或直接不响应并等待 `transport-select`；随后按 relay 继续。具体选哪种以实测为准——**先在 `tests/interop` 里跑一遍再定**。
3. **必须正确处理** `transport-select`：若对端选中 relay，按 relay 建立数据面。

> 完整策略与平台矩阵见 [08-tailscale.md](08-tailscale.md)。

---

## 6. 消息层（`src/message/message.go`）

```jsonc
{
  "t":  "fileinfo",   // Type
  "v":  2,            // Version
  "m":  "错误文本",    // Message
  "b":  "<base64>",   // Bytes
  "b2": "<base64>",   // Bytes2
  "n":  3,            // Num
  "f":  ["feature"]   // Features
}
```

编码流程：`json.Marshal` → `compress`（flate **HuffmanOnly**）→ 若 key 非空则 `AES-256-GCM` 加密。
解码流程反之，解压有 64 MiB 上限。

### 6.1 Rust 侧必须踩准的 Go JSON 语义

| 项 | Go 行为 | Rust 对策 |
|----|---------|-----------|
| `[]byte` | base64 **标准编码**（带 `+`/`/`，尾部 `=` 补齐） | `#[serde(with = "serde_bytes")]`；`serde_json` 对 `Vec<u8>` 默认是数组，**必须用 serde_bytes** |
| 省略空值 | 所有字段带 `omitempty` | serde 用 `#[serde(skip_serializing_if = "Option::is_none")]` + `default` |
| 数字 | `int` 直接输出 | `Num` 用 `Option<i64>` |
| 未知字段 | 忽略 | serde 默认忽略（**不要**加 `deny_unknown_fields`） |
| 时间 | `time.Time` RFC3339 纳秒 | `FileInfo.ModTime` 用 `chrono::DateTime<Utc>`，`serde` 默认即 RFC3339；注意精度对齐 |

### 6.2 消息类型全集

```
pake, pake-confirm, transport-select, tailcat-offer, tailcat-status, externalip,
finished, error, close-recipient, close-sender, recipientready,
fileinfo, fileinfo-start, fileinfo-batch, fileinfo-end,
file-prepared, exact-hash-request, exact-hash-result,
relay-standby, relay-ramp, ssh-authorize, ssh-offer
```

v1 需要实现的：`pake`、`pake-confirm`、`externalip`、`fileinfo`（+ `fileinfo-start/batch/end`）、`recipientready`、`finished`、`error`、`close-recipient`、`close-sender`、`file-prepared`、`exact-hash-request`、`exact-hash-result`、`relay-standby`、`relay-ramp`。

v1 需要**识别但不实现**的：`tailcat-offer`、`tailcat-status`、`transport-select`（收到后走 relay 降级，见 §5.3）。
其余未识别类型收到时应**忽略而非断开**（前向兼容）。

---

## 7. 文件传输状态机

### 7.1 五步生命周期（`Client.Step1..Step5`）

| 步骤 | 标志 | 含义 |
|------|------|------|
| 1 | `ChannelSecured` | PAKE 完成，`c.Key` 就绪 |
| 2 | `FileInfoTransferred` | 文件清单已交换 |
| 3 | `RecipientRequestFile` | 接收方已确认（同意/拒绝/选择） |
| 4 | `FileTransferred` | 数据传完 |
| 5 | `CloseChannels` | 关闭 |

### 7.2 主流程

```
[sender]
  1. 收集文件 → GetFilesInfo → FileInfo[]（name/folderRemote/folderSource/hash/size/modTime/...）
  2. 连中继（控制面），等待接收方
  3. 收到 pake → 回 pake(+salt) → 收 pake-confirm → 回 pake-confirm → Step1
  4. 发送 Message{t:"fileinfo", b:<SenderInfo JSON>}  → Step2
     （大清单用 fileinfo-start / fileinfo-batch * N / fileinfo-end 分块）
  5. 等 recipientready：{b:<RemoteFileRequest JSON>}  → Step3
  6. 打开 N 条数据连接（relay 多端口 或 本地直连），按 chunk 发送 → Step4
  7. finished / close-sender → Step5

[receiver]
  1. 连中继，主动发 pake
  2. pake-confirm 交换 → Step1
  3. 收 fileinfo → 展示清单 → 询问用户（overwrite / rename / resume / refuse）
  4. 发送 recipientready + RemoteFileRequest{CurrentFileChunkRanges, MachineID, ExternalIP, ReconnectVersion, Features}
  5. 收数据块，写入文件 → Step4
  6. 校验哈希 → finished
```

### 7.3 数据块格式

- 块大小：`chunkSize = TCP_BUFFER_SIZE/2 = 32768` 字节。
- 明文 payload：`u64 LE 文件内偏移 || 数据`。
- 若该文件启用压缩：先对整个 `8+n` 的 payload 做 flate 压缩（HuffmanOnly），再加密。
- 加密：AES-256-GCM，输出 `nonce(12) || ct || tag(16)`，作为**一整个 comm 帧**发出。
- 接收侧解压上限 `maxDecompressedChunkSize = 32768 + 8 = 32776`；解出后长度必须 `>= 9 && <= 32776`，否则报错断开。
- 偏移必须落在文件大小范围内（`chunkFits`），防止越界写。

> 压缩字节**不要求**与 Go 版逐字节一致——只要产出合法的 deflate 流，Go 的 `flate` 就能解。因此 Rust 侧可用 `flate2` 任意 level。这是本项目少数几个"兼容性不要求字节一致"的地方，但**必须在集成测试中验证 Go 端能解压 Rust 端产出的数据**。

### 7.4 断点续传（resume）

- 接收方发现目标文件已存在且大小 < 远端大小时，把**缺失的 chunk 区间**放进 `RemoteFileRequest.CurrentFileChunkRanges`（`[chunkSize, startIndex, count]` 三元组 flattened）。
- 发送方用 `requestedChunkQueue` 按区间发块（支持多 worker 并发 claim）。
- `ReconnectVersion = 1`：断线后用 `NextReconnectRoom` 重新加入中继的新房间，`reconnectRelayCandidates()` 依次尝试，退避 `reconnectBackoff(attempt)`，最多 `maxReconnectAttempts = 10`。
- 只在传输**已启动**后才重试；握手阶段的失败是终态错误。

### 7.5 哈希与校验

`--hash` 支持 `xxhash`（默认）、`imohash`、`imohash-v2`、`md5`、`highway`。用于判断"文件是否已存在/是否需要续传"，以及传输后校验。
- xxhash = XXH64（`cespare/xxhash/v2`）→ Rust 用 `xxhash-rust` 的 `xxh64`。
- imohash = 采样哈希（前 16KB + 后 16KB + size），croc 用 `imohash.NewCustom(16*16*8*1024, 128*1024)`，需自行移植（约 60 行）。
- highwayhash 用固定 key，需 `highway` crate 或移植。
- md5 常规。

---

## 8. 本地直连与发现（不走中继）

1. **本地中继**：发送方在本机开一个临时中继（`setupLocalRelay`，端口从 `RelayPorts[0]` 起找空闲端口）。
2. **组播发现**：`peerdiscovery` 在 `--multicast`（默认 `239.255.255.250`）上广播，接收方发现后直连。
3. **本地探针**：连上同一中继的 peer 会先做一次 `purpose = "local-ip-probe"` 的 PAKE（`senderWaitForHandshake`），之后用该密钥加密交换本机 IP 列表（`ips?` 请求 / IP 列表响应 / `handshake` 结束标记）。
   - 注意：这个 PAKE 的 `Purpose` 是 `local-ip-probe`，`Room` 用 **room name** 而不是 secret，参与身份与 transcript。
   - 收到无法识别的帧时，Go 版会 `gracefully refusing using the public relay`——Rust 端要有同样的宽容处理。

---

## 9. 代理支持

- `--socks5`：SOCKS5 代理（Rust: `fast-socks5` 或 `tokio-socks`）。**DNS 由代理侧解析**，本地 IP 直连不走代理（`utils.IsLocalIP`）。
- `--connect`（HTTP_PROXY）：HTTP `CONNECT` 隧道。
- `--internal-dns`：内置 DNS stub resolver，向 Cloudflare/Google/Quad9 等公共 DNS 并发查询。v1 可后置。

---

## 10. 兼容性红线清单（实现时逐条自检）

- [ ] 帧 magic 与 u32 LE 长度
- [ ] `0x01` keepalive 帧在所有读取循环中被跳过
- [ ] 中继 PAKE 曲线 = `siec`，`weakKey` = 中继密码
- [ ] pbkdf2 迭代数 = **100**，keylen = 32，hash = SHA-256
- [ ] 客户端在 banner 前管线发送 room 帧
- [ ] peer PAKE `Version = 2`，salt = 32 字节
- [ ] PAKE 前两帧与 confirm 两帧均**明文**
- [ ] A = receiver、B = sender（身份绑定写死）
- [ ] 消息体压缩用 deflate、上限 64 MiB
- [ ] 数据块 chunk = 32768，payload = u64 LE offset + data
- [ ] 数据连接 room = `{room}-{j}`
- [ ] 未识别的消息 type 忽略不断开
