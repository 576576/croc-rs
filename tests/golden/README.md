# tests/golden —— Go 参考实现的黄金向量

这个目录存放已知答案向量，供 `crates/*/tests/golden.rs` 逐字节比对。向量由
`croc-playground/tools/gen-golden`（Go 程序，与本仓库**平级的独立仓库**）导出 ——
本仓库是纯 Rust 仓库，不收 Go 源码。

**文件缺失是正常状态**：Rust 侧的黄金向量测试在文件缺失时会跳过并打印提示，
所以没有 Go 工具链的机器也能正常跑 `cargo test`。

生成方式（在 `croc-playground/` 下）：

```bash
cd tools/gen-golden
go mod tidy
go run .                # 默认写到 ../../../croc-rs/tests/golden
```

| 文件 | 消费者 |
|---|---|
| `curves.json` | `crates/croc-siec/tests/golden.rs` |
| `pake.json` | `crates/croc-pake/tests/golden.rs` |
| `pakekey.json` | `crates/croc-pakekey/tests/golden.rs` |
| `store.json` | `crates/croc-store/tests/golden.rs` |

生成的文件应当提交进仓库——它们是"Rust 版与 Go 版在某个时间点逐字节一致"的证据，
也是回归测试的基线。重新生成时请一并更新
`croc-playground/tools/gen-golden/README.md` 里记录的 croc 版本与 commit。