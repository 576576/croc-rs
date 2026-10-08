# 密码学：与 Go 的逐字节契约

> 描述 `croc-siec` / `croc-pake` / `croc-pakekey` 三个 crate 必须复刻的 Go 语义。
> 任何一个字节的偏差都表现为"密码错误"，而且极难定位——所以这里的每一条都是硬约束，
> 对应的验证在 [testing.md](testing.md) 的 golden 向量里。
> 引用格式 `src/xxx.go:NNN` 指 `schollz/croc` v11.5.4。

## 1. 与 Go 组件的对应

| Go 组件 | Rust 落点 |
|---------|-----------|
| `tscholl2/siec`（SIEC255） | `croc-siec`（自研，无现成实现） |
| `schollz/pake/v3` v3.2.0（SPAKE2 变体） | `croc-pake`（自研，含 wire JSON） |
| `src/pakekey`（PAKE v2） | `croc-pakekey`（HKDF + transcript） |
| `golang.org/x/crypto/pbkdf2` | `pbkdf2` + `sha2` |
| `crypto/aes` + `cipher.NewGCM` | `aes-gcm` |
| `crypto/hmac` / `hkdf` / `sha256` | `hmac` + `hkdf` + `sha2` |
| `math/big` | `num-bigint` |

## 2. SIEC255 曲线（`crates/croc-siec`）

### 2.1 参数（`tscholl2/siec` 的 `initSIEC255`）

```
方程：y² = x³ + 19   （Weierstrass，a = 0, b = 19）
P  = 28948022309329048855892746252183396360603931420023084536990047309120118726721   (255 bit)
N  = 28948022309329048855892746252183396360263649053102146073526672701688283398081
A  = 0
B  = 19
G  = (5, 12)
BitSize = 255
```

`P` 也可写作 `2^255 - 19*...`，直接用上面十进制即可（`BigUint::from_str_radix`）。

### 2.2 必须精确复刻的算术语义

Go 实现用 `math/big`，其语义与 Rust 的差异是**头号坑**：

| Go `big.Int` 语义 | Rust 默认行为 | 对策 |
|------------------|--------------|------|
| `Mod(m)` 返回**欧几里得**模，结果恒 `≥ 0` | `BigInt % m` 是**截断**余数，可为负 | 自己写 `euclid_mod`，负数时 `+m` |
| `Neg(v)` 后 `Mod(v, P)` | 同上 | 同上 |
| `ModInverse(z, P)` | 无内置 | 手写扩展欧几里得（`num-bigint` 无此 API） |
| `Exp(y, 2, P)` 模幂 | `BigUint::modpow` | 用 `BigUint::modpow` |
| `Bytes()`：**大端**、**无前导零**、绝对值 | `BigUint::to_bytes_be()` 语义一致 | ⚠️ **零值返回空切片**（Go `big.Int{}.Bytes()` = `[]byte{}`，参与哈希时是零长度） |
| `Bit(i)` | `bit(i)` | 一致 |
| `BitLen()` | `bits()` | 一致 |
| `Cmp` | `Ord` | 一致 |

**无穷远点用 `(0, 0)` 表示**：`Add` 中若某操作数的 `x.BitLen()==0 && y.BitLen()==0` 则视为无穷远。
所有 Add/Double 都要照做。

### 2.3 点运算（照抄 Go 实现）

```
Add(P1, P2):
  if P1 == (0,0): return P2
  if P2 == (0,0): return P1
  if x1==x2 && y1==y2: return Double(P1)
  z = (x2 - x1) mod P
  if z.BitLen()==0: return (0,0)          # 注意：返回 z 与 lambda，都是 0
  λ = ((y2 - y1) * z^{-1}) mod P
  x3 = (λ² - (x1 + x2)) mod P
  y3 = (λ*(x1 - x3) - y1) mod P
  return (x3, y3)

Double(P1):
  if y1 == 0: return (0,0)
  λ = (3*x1²) * (2*y1)^{-1} mod P
  x3 = (λ² - 2*x1) mod P
  y3 = (λ*(x1 - x3) - y1) mod P
  return (x3, y3)

ScalarMult(B, k []byte):            # k 为大端字节
  (x, y) = (0, 0)
  for b in k:                        # 从高位字节开始
      for bit in 0..8:               # MSB 优先（b & 0x80，然后 b <<= 1）
          (x, y) = Double(x, y)
          if bit set: (x, y) = Add(B, (x, y))   # 注意参数顺序：Add(x1,y1,x2,y2) 其中 x1,y1 = B
  return (x, y)

ScalarBaseMult(k) = ScalarMult(G, k)
```

> ⚠️ `Add` 的参数顺序在 Go 里是 `Add(x1,y1,x2,y2)`。仿射加法本身对称，但 `(0,0)` 的短路分支
> 依赖顺序，照抄顺序最安全。

## 3. `schollz/pake` v3（`crates/croc-pake`）

### 3.1 支持的曲线与常量点

`AvailableCurves() = ["p521", "p256", "p384", "siec", "ed25519"]`。每条曲线预定义了 SPAKE2 的
`U` / `V` 常量点：

| curve | Ux | Uy | Vx | Vy |
|-------|----|----|----|----|
| p256 | 793136080485469241208656611513609866400481671852 | 59748757929350367369315811184980635230185250460108398961713395032485227207304 | 1086685267857089638167386722555472967068468061489 | 9157340230202296554417312816309453883742349874205386245733062928888341584123 |
| p384 | 793136080485469241208656611513609866400481671852 | 7854890799382392388170852325516804266858248936799429260403044177981810983054351714387874260245230531084533936948596 | 1086685267857089638167386722555472967068468061489 | 21898206562669911998235297167979083576432197282633635629145270958059347586763418294901448537278960988843108277491616 |
| p521 | 793136080485469241208656611513609866400481671852 | 4032821203812196944795502391345776760852202059010382256134592838722123385325802540879231526503456158741518531456199762365161310489884151533417829496019094620 | 1086685267857089638167386722555472967068468061489 | 5010916268086655347194655708160715195931018676225831839835602465999566066450501167246678404591906342753230577187831311039273858772817427392089150297708931207 |
| siec | 793136080485469241208656611513609866400481671853 | 18458907634222644275952014841865282643645472623913459400556233196838128612339 | 1086685267857089638167386722555472967068468061489 | 19593504966619549205903364028255899745298716108914514072669075231742699650911 |
| ed25519 | 41821174510521985817056358996007359290163947216650231187782646151092828043509 | 0 | 1456941786990260824647297143563623381366314063537015067473110401627488371271 | 0 |

ed25519 是**特例**：点以 32 字节编码整体放在 `x` 里，`y` 恒为 0；加/减走 `edwards25519::Point`
的编解码；`P = 2^255 - 19`。

`--curve` 由**接收方**决定，发送方照着用。默认 `p256`；中继握手固定 `siec`（与 `--curve` 无关），
所以 `siec` 是必然会被用到的；`ed25519` 只在显式指定时用到，它的点编码与另外四条不同。

### 3.2 协议流程（SPAKE2 变体）

```
Init(pw, role, curve, [idA, idB]):
  if role == 0 (A / receiver):
      Vpw = V * pw            # ScalarMult(V, pw)，pw 为原始字节（大端）
      Upw = U * pw
      α   = 32 字节随机
      Aα  = α * G
      X   = Upw + Aα          # Add
      # X 发给 B
  if role == 1 (B / sender):
      只保存状态，等 Update

Update(otherBytes):
  if self.Role == 1:
      X = other.X
      assert on_curve(X)
      Vpw = V * pw ; Upw = U * pw
      β = 32 字节随机 ; Bα = β * G
      Y = Vpw + Bα
      Z = β * (X - Upw)         # 减法 = Add(X, (Upw.x, -Upw.y mod P))
      K = deriveSessionKey()
  else:
      Y = other.Y
      assert on_curve(Y)
      Z = α * (Y - Vpw)
      K = deriveSessionKey()
```

> `pw` 直接作为 `ScalarMult` 的标量（大端字节），**没有**哈希到标量域的过程。密码长度直接决定
> 循环次数，所以空密码 / 超长密码的行为也要一致。

### 3.3 Wire JSON

`Pake.Bytes()` = `json.Marshal(p.Public())`，结构体字段名是 **Unicode 下标字符**：

```json
{"Role":0,"Uᵤ":123,"Uᵥ":456,"Vᵤ":789,"Vᵥ":12,"Xᵤ":345,"Xᵥ":678,"Yᵤ":null,"Yᵥ":null}
```

- `Uᵤ` = `U` + `U+1D64`（`ᵤ`），`Uᵥ` = `U` + `U+1D65`（`ᵥ`）；V / X / Y 同理。
- 未设置的字段是 `null`（`*big.Int` 为 nil），**不是 0**。
- 大整数以**十进制数字**输出（Go `big.Int.MarshalJSON`），不是字符串，也不是 base64。
- 字段顺序由 Go 结构体决定：`Role, Uᵤ, Uᵥ, Vᵤ, Vᵥ, Xᵤ, Xᵥ, Yᵤ, Yᵥ`。顺序在 JSON 里不影响解析，
  但 golden 比较时按 key 比。

大整数的十进制表示需要 `serde_json` 的 **`arbitrary_precision`** feature：

- 序列化：`Number::from_string_unchecked(n.to_string())`
- 反序列化：`Number::to_string().parse::<BigInt>()`

> `arbitrary_precision` 会影响整个 crate 的数字处理（`f64` 也走字符串路径），所以这套 JSON 逻辑
> 隔离在 `croc-pake` 内，不污染其它 crate 的序列化。

### 3.4 `deriveSessionKey()` —— 两个完全不同的分支

**分支 A（withIDs = false，中继握手用 `InitCurve`）**

```
h = sha256()
h.update(Pw)
h.update(Xᵤ.Bytes()); h.update(Xᵥ.Bytes())
h.update(Yᵤ.Bytes()); h.update(Yᵥ.Bytes())
h.update(Zᵤ.Bytes()); h.update(Zᵥ.Bytes())
K = h.finalize()          # 32 字节
```

**分支 B（withIDs = true，peer PAKE 用 `InitCurveWithIdentities`）**

```
domain = b"github.com/schollz/pake/v3/identity-bound-session-key/v1"
h = sha256()
for field in [domain, Pw, idA, idB, curveName, Xᵤ.Bytes(), Xᵥ.Bytes(),
              Yᵤ.Bytes(), Yᵥ.Bytes(), Zᵤ.Bytes(), Zᵥ.Bytes()]:
    h.update(u64_le(len(field)))
    h.update(field)
K = h.finalize()
```

⚠️ 分支 A **没有长度前缀**，分支 B **有**。两者都用 `big.Int.Bytes()`（去前导零的大端字节），
零值贡献零字节。
⚠️ `Pw` 是**原始密码字节**。

### 3.5 错误对应

| Go 错误 | Rust |
|---------|------|
| `"can't have its own role"` | `PakeError::SameRole` |
| `"X values not on curve"` / `"Y values not on curve"` | `PakeError::PointNotOnCurve` |
| `"session key not generated"` | `PakeError::NoSessionKey` |
| `"no such curve"` | `PakeError::UnknownCurve` |

## 4. `pakekey`：PAKE v2 密钥派生（`crates/croc-pakekey`）

### 4.1 常量

```
ProtocolVersion = 2
SaltSize        = 32
PurposeTransfer   = "peer-transfer"
PurposeLocalProbe = "local-ip-probe"
PurposeSSH        = "peer-ssh"
identityDomain    = "croc/spake2/participant/v2"
transcriptDomain  = "croc/spake2/channel/v2"
```

### 4.2 `frame()` 编码

```rust
fn frame(fields: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in fields {
        out.extend_from_slice(&(f.len() as u64).to_le_bytes());  // u64 小端长度
        out.extend_from_slice(f);
    }
    out
}
```

### 4.3 Identities

```
version = u64_le(2)
idA = frame([identityDomain, version, purpose, room, b"receiver"])
idB = frame([identityDomain, version, purpose, room, b"sender"])
```

### 4.4 Derive

```
transcript = frame([transcriptDomain, version, purpose, room, curve, idA, idB,
                    initiator, responder, salt])
material   = HKDF-SHA256(ikm = sharedKey, salt = salt, info = transcript) 取 96 字节
EncryptionKey = material[0..32]
confirmationKeyA = material[32..64]
confirmationKeyB = material[64..96]
ConfirmationA = HMAC-SHA256(key = confirmationKeyA, msg = transcript)
ConfirmationB = HMAC-SHA256(key = confirmationKeyB, msg = transcript)
```

- `initiator` = A（receiver）的 `Bytes()`，**原始 JSON 字节**；`responder` = B（sender）的。
- `salt` 必须是 32 字节，否则报错（fail-closed）。

### 4.5 Confirm

常量时间比较，且长度必须 == 32。

## 5. AEAD 与 KDF

| 用途 | 算法 | 参数 | 输出格式 |
|------|------|------|---------|
| 中继会话加密 | AES-256-GCM | key = `pbkdf2(strongKey, salt, iter=100, keylen=32, sha256)` | `nonce(12) \|\| ct \|\| tag(16)` |
| Peer 消息加密 | AES-256-GCM | key = PAKE `EncryptionKey` | 同上 |
| 数据块加密 | AES-256-GCM | 同上，**复用 AEAD 实例** | 同上 |

> **性能**：Go 为数据块专门做了 `EncryptAEADTo` / `DecryptAEADInPlace` 的零分配复用。Rust 侧同样
> 复用 `Aes256Gcm` 实例与 `encrypt_in_place_detached` / `decrypt_in_place_detached`，避免每 32 KB
> 分配一次 32 KB。

> **Nonce 安全**：每个块随机 12 字节 nonce。单连接的块数上限要评估随机碰撞风险（2^32 块后生日
> 碰撞概率显著，对应 ~128 TB；实际安全，但长时间大文件传输建议记录并在超阈值时报错）。

## 6. 兼容性知识的存放位置

本模块最容易写成"Go 逐行翻译"。约定：

- **兼容性知识写在本目录的文档与 golden 测试里，不写进代码注释。** 不要满屏
  `// COMPAT: 这里必须和 Go 一样`。
- **把不优雅的部分封装成小类型**，而不是注释：
  - `EuclidMod` / `EucBigInt`：欧几里得取模语义；
  - `GoBigIntJson`：大整数的十进制 JSON 编解码（含 `null` 与 `arbitrary_precision`）；
  - `Transcript`：u64 LE 长度前缀的哈希 transcript 构造。
- 每个封装类型配**独立的 golden 单测**，测试名里注明对应的 Go 行为，
  例如 `euc_mod_matches_go_bigint_semantics`。
- 调用方代码里看不到 `big.Int` 语义细节，只看到 `EuclidMod::add(...)`。

## 7. 安全边界

1. **SIEC255 与 SPAKE2 的实现不是常量时间**。这是为了与 Go 版兼容而做的取舍，与上游风险相同，
   不是新引入的漏洞。`croc-siec` / `croc-pake` 的文档必须明确标注"仅用于与 croc 互通，不作为
   通用密码学库使用"。
2. **`pbkdf2` 迭代次数 100 是上游设计，不要"顺手改成更安全的"**——改了就无法与官方客户端互通。
   要加固只能在中继密码层面（自建中继用更强的密码）解决。
3. 所有随机数用 OS 随机源（对应 Go `crypto/rand`）。
