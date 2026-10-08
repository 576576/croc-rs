//! croc 的纯领域层。
//!
//! 这一层回答"要做什么"，不回答"怎么做"：没有 `tokio`、没有 `std::fs`、没有时钟、
//! 没有随机源（需要随机数的地方从参数拿一个 [`croc_protocol::Randomness`]）。
//! 读写文件、连网、计时都归 `croc-engine`。
//!
//! | 模块 | 职责 |
//! |------|------|
//! | [`codephrase`] | 口令码的生成与解析：码 → 中继房间名 + PAKE 口令 |
//! | [`chunk`] | 分块规划，以及断点续传用的区间运算 |
//! | [`policy`] | 覆盖 / 续传 / 重命名 / 拒绝的决策表 |
//! | [`session`] | **对端会话状态机**：PAKE → 加密通道 → 清单 → 数据面 → 收尾 |
//! | [`redact`] | 从诊断文本里抹掉秘密 |
//! | [`types`] | 领域新类型（[`RoomName`] / [`Secret`]） |
//!
//! # 为什么"纯"是硬要求
//!
//! 见 `docs/agent/architecture.md` §2：状态机不发起 IO，只做状态转移。这样
//! 一次完整的断点续传决策可以在单元测试里用事件序列确定性重放——排查"为什么对端
//! 说密码错误"这类问题时，这一点比什么都值钱。

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod chunk;
pub mod codephrase;
pub mod policy;
pub mod redact;
pub mod session;
pub mod types;

pub use session::{Effect, Event, InEvent, Outcome, Role, Session, SessionConfig, SessionError, Stage};
pub use types::{RoomName, Secret};
