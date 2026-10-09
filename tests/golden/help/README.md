# 英文 `-h` 的逐字对照

这里的文件是 Go 版 `croc -h` 的**实际输出**，用来约束"英文帮助与 Go 版逐字一致"这条。

| 文件 | 出处 |
|------|------|
| `top.txt` | `croc -h` |
| `send.txt` … `relay.txt` | `croc <子命令> -h` |
| `generate-fish-completion.txt` | `croc generate-fish-completion -h` |

对照的是 Go 版仓库（`/d/Github/croc`，`src/version/version.go` 里的那个版本，当前 11.5.4）构建出来的
二进制。生成时只做两处替换：

1. 程序名：`croc` → `croc-rs`。
   **只替换程序名**——`croc.schollz.com:9009` / `croc6.schollz.com:9009` 是默认中继地址，两边一致，
   不能动；`CROC_SECRET` 是大写，也不受影响。
   隐藏子命令的 `Name:` 用的是可执行文件名（Go 版里是 `croc11.exe`），这里一并换成 `croc-rs`。
2. `VERSION:` 段里的版本号 → `<VERSION>`。版本号由 `build.rs` 烘进来，随每次提交变，不能写死；
   测试里再替换成实际值。

生成脚本（一次性，不入库）：

```python
import re, subprocess
CROC = "D:/tmp/croc11.exe"          # `go build -o croc11.exe .`
OUT  = "tests/golden/help"
NAME = re.compile(r"(?<![\w-])croc(?![.\w-])")

for topic, args in {"top": [], "send": ["send"], "store": ["store"], "ssh": ["ssh"],
                    "update": ["update"], "relay": ["relay"],
                    "generate-fish-completion": ["generate-fish-completion"]}.items():
    text = subprocess.run([CROC, *args, "-h"], capture_output=True, check=True).stdout.decode("utf-8")
    text = text.replace("croc11.exe", "croc-rs")
    text = NAME.sub("croc-rs", text)
    text = re.sub(r"(VERSION:\n   )[^\n]+", r"\1<VERSION>", text)
    with open(f"{OUT}/{topic}.txt", "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)
```

`NAME` 的两侧边界都不能省：右边挡住 `croc.schollz.com` 与 `croc6.schollz.com`，左边挡住
`getcroc.com`。

文件按 LF 存（`.gitattributes` 的 `text=auto` 会归一化），测试比较前会把 `\r\n` 折成 `\n`，
所以在 Windows 与 Linux 上跑出来的结果相同。环境变量标记按平台变（Windows `[%VAR%]`、其余
`[$VAR]`），测试里显式构造 Windows 那一侧，免得 CI 与本地比出两种结果。
