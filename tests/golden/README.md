# tests/golden —— Go 参考实现的黄金向量

这个目录存放由 `tools/gen-golden`（Go 程序）导出的已知答案向量，供
`crates/*/tests/golden.rs` 逐字节比对。

**目录为空是正常状态**：Rust 侧的黄金向量测试在文件缺失时会跳过并打印提示，
所以没有 Go 工具链的机器也能正常跑 `cargo test`。

生成方式：

```bash
cd tools/gen-golden
go mod tidy
go run . -out ../../tests/golden
```

| 文件 | 消费者 |
|---|---|
| `curves.json` | `crates/croc-siec/tests/golden.rs` |
| `pake.json` | `crates/croc-pake/tests/golden.rs` |
| `pakekey.json` | `crates/croc-pakekey/tests/golden.rs` |

生成的文件应当提交进仓库——它们是"Rust 版与 Go 版在某个时间点逐字节一致"的证据，
也是回归测试的基线。重新生成时请一并更新 `tools/gen-golden/README.md` 里记录的
croc 版本与 commit。
