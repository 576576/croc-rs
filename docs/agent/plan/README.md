# 待完成的计划

这里只放**还没做完**的事。已完成内容里的知识（协议契约、密码学语义、架构约定、测试与 CI 的
决策）已经搬到上一层作为长期参考文档了，不要再往这里加。

| 计划 | 状态 | 说明 |
|------|------|------|
| [gui.md](gui.md) | [PENDING] | Tauri 2 GUI（产物 `crocw-rs`）。后端命令齐全，前端有一套无构建的原生三文件；UI 基准改为 getcroc.com，待换成 Vite + React + TS |
| [tailscale.md](tailscale.md) | [PENDING] | Tailscale / Tailcat 数据面。等上游 tailcat-rs；数据面抽象本身尚未实现 |

> 标记约定：`[PENDING]` = **尚未实现**。参考文档里描述未实现能力的段落也用它标注，
> 免得把"文档里写了"读成"已经做了"。

长期参考文档（`docs/agent/` 上一层）：

| 文档 | 内容 |
|------|------|
| [../overview.md](../overview.md) | 项目定位、范围、设计原则、仓库结构 |
| [../protocol.md](../protocol.md) | 线路层兼容基线（帧、握手、PAKE、消息、传输状态机） |
| [../crypto.md](../crypto.md) | 密码学与 Go 的逐字节契约 |
| [../architecture.md](../architecture.md) | 分层、sans-io 状态机、编码规范、错误模型 |
| [../cli.md](../cli.md) | CLI 未实现项与与 Go 的行为差异 |
| [../testing.md](../testing.md) | 测试分层、互通矩阵、golden 向量、合规 |
| [../build-and-ci.md](../build-and-ci.md) | 四个 workflow 的决策与设计；本机踩过的坑 |
| [../release.md](../release.md) | 发布操作手册 |
