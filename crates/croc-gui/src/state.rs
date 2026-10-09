//! 传输登记表：Rust 侧唯一保存的状态。
//!
//! 只记"哪些传输还在跑、能不能取消"。文件名、进度、历史全部归前端——Rust 侧保存 UI
//! 状态意味着每个界面改动都要动后端，而 Tauri 的价值恰恰在于不需要。
//!
//! 之所以必须留这一份：取消需要一个句柄。前端拿不到 `CancellationToken`，只能拿
//! 一个字符串 ID，所以这张表就是"ID → 令牌"的映射。

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

/// 一次传输的标识。前端拿它来取消。
pub type TransferId = String;

/// 一次传输当前的状态（给前端列表面板用）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferStatus {
    /// 标识。
    pub id: TransferId,
    /// `"send"` 或 `"receive"`。
    pub direction: String,
    /// 正在处理哪个文件（还没有时为 `None`）。
    pub current_file: Option<String>,
    /// 已经搬完的字节。
    pub transferred: u64,
    /// 总字节（未知时为 0）。
    pub total: u64,
    /// 是否还能取消。
    pub cancellable: bool,
}

/// 传输登记表。
#[derive(Debug, Default)]
pub struct TransferRegistry {
    inner: Mutex<HashMap<TransferId, Entry>>,
}

#[derive(Debug)]
struct Entry {
    token: CancellationToken,
    direction: String,
}

impl TransferRegistry {
    /// 空表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次新传输，回传它的取消令牌。
    ///
    /// 锁中毒时恢复而不是 panic：这张表里只有"ID → 令牌"，没有任何不变量会在
    /// 半路被破坏，所以把中毒的锁拿回来继续用是安全的。一个只用来取消传输的注册表
    /// 不值得让整个 GUI 崩掉。
    #[must_use]
    pub fn register(&self, id: TransferId, direction: &str) -> CancellationToken {
        let token = CancellationToken::new();
        let mut inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.insert(id, Entry { token: token.clone(), direction: direction.to_owned() });
        token
    }

    /// 注销一次传输。
    pub fn unregister(&self, id: &str) {
        self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(id);
    }

    /// 取消一次传输。返回"是否真的取消了"——ID 不存在时返回 `false`，
    /// 前端据此提示"这个传输已经结束了"。
    pub fn cancel(&self, id: &str) -> bool {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner.get(id).is_some_and(|entry| {
            entry.token.cancel();
            true
        })
    }

    /// 当前还在跑的传输。
    #[must_use]
    pub fn list(&self) -> Vec<TransferStatus> {
        let inner = self.inner.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        inner
            .iter()
            .map(|(id, entry)| TransferStatus {
                id: id.clone(),
                direction: entry.direction.clone(),
                current_file: None,
                transferred: 0,
                total: 0,
                cancellable: !entry.token.is_cancelled(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registering_makes_a_transfer_cancellable() {
        let registry = TransferRegistry::new();
        let token = registry.register("t1".to_owned(), "send");
        assert!(!token.is_cancelled());
        assert_eq!(registry.list().len(), 1);
        assert_eq!(registry.list().first().map(|status| status.direction.as_str()), Some("send"));
    }

    #[test]
    fn cancelling_flips_the_token() {
        let registry = TransferRegistry::new();
        let token = registry.register("t1".to_owned(), "receive");
        assert!(registry.cancel("t1"));
        assert!(token.is_cancelled());
        assert_eq!(registry.list().first().map(|status| status.cancellable), Some(false));
    }

    #[test]
    fn cancelling_an_unknown_transfer_reports_false() {
        // 前端据此提示"这个传输已经结束了"，而不是静默什么也不做。
        let registry = TransferRegistry::new();
        assert!(!registry.cancel("nope"));
    }

    #[test]
    fn unregistering_removes_it_from_the_list() {
        let registry = TransferRegistry::new();
        let _ = registry.register("t1".to_owned(), "send");
        registry.unregister("t1");
        assert!(registry.list().is_empty());
        assert!(!registry.cancel("t1"), "注销之后不该还能取消");
    }

    #[test]
    fn cancelling_one_transfer_leaves_the_others_alone() {
        let registry = TransferRegistry::new();
        let first = registry.register("a".to_owned(), "send");
        let second = registry.register("b".to_owned(), "send");
        registry.cancel("a");
        assert!(first.is_cancelled());
        assert!(!second.is_cancelled(), "多任务并发是 GUI 相对 FlCroc 的增量能力，不能互相干扰");
    }
}
