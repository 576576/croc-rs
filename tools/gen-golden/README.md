# gen-golden —— 从 Go 参考实现导出黄金向量

这个 Go 程序把 **Go 版 croc / `schollz/pake` 的真实输出** 落成 JSON，供 Rust 侧的
黄金向量测试逐字节比对：

| 产出 | 消费者 | 覆盖内容 |
|---|---|---|
| `curves.json` | `crates/croc-siec/tests/golden.rs` | 四条短 Weierstrass 曲线的 `Double` / `Add` / `ScalarMult` |
| `pake.json` | `crates/croc-pake/tests/golden.rs` | 五条曲线的完整 SPAKE2 握手（公开值 JSON + 会话密钥） |
| `pakekey.json` | `crates/croc-pakekey/tests/golden.rs` | 五条曲线 × 三种用途的密钥编排（HKDF + 双确认标签） |

## 怎么跑

```bash
cd croc-rs/tools/gen-golden
go mod tidy                       # 首次需要，生成 go.sum
go run . -out ../../tests/golden
```

之后 `cargo test --workspace` 就会把这三个文件纳入断言。**文件不存在时 Rust 测试会
跳过并打印提示**，所以没有 Go 工具链的机器也能正常跑 `cargo test`。

## 为什么 `replace` 到本地 croc

`pakekey` 是 croc 仓库里的一个包（`src/pakekey`），Go 模块代理上没有独立版本。
`go.mod` 里的

```
replace github.com/schollz/croc/v11 => ../../croc
```

指向本机的 croc 检出（`D:/Github/croc`）。生成器直接 import 它，而不是照着 Rust 实现
再写一遍 Go 版——自己跟自己比没有任何意义。

如果 croc 检出不在 `../../croc`，改这一行即可。

## 随机数怎么固定

`schollz/pake` 在 `Init`/`Update` 内部调用 `crypto/rand.Read`。`crypto/rand` 的
`Reader` 是包级变量，所以生成器在每次握手前把它换成确定性的 `fillReader`：

| 读取次序 | 消费方 | 值 |
|---|---|---|
| 第 1 次 | A 方 `InitCurveWithIdentities` 的 `α` | `0x11` × 32 |
| 第 2 次 | B 方 `Update` 的 `β` | `0x12` × 32 |

（role 1 的 `Init` 不取随机数，所以次序是稳定的。）

`fillReader` 会记录每次读取的内容，握手结束后 `expectEphemerals` 会核对「恰好两次、
每次 32 字节」。**如果哪天 `crypto/rand.Read` 不再走 `rand.Reader`，生成器会直接
报错退出**，而不是悄悄写出 α/β 与实际不符的假向量。

## 已知缺口

- **ed25519 不在 `curves.json` 里**：`pake.Edwards25519Curve` 没有 `Double` 方法，
  凑不出与其余四条曲线同构的用例。它的适配器在 `croc-siec` 里有单独的性质测试，
  并且 `pake.json` / `pakekey.json` 都覆盖了它。
- 生成器只覆盖**密码学层**。协议层（`comm` 帧、消息编码、压缩、分块）的向量等
  `croc-protocol` 落地后再补。
