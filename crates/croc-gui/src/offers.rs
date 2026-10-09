//! 清单确认：`croc-engine` 拿到清单时问用户一句。
//!
//! CLI 那边的答案来自终端，同步就有；GUI 的答案要等界面上的按钮，而在一个 tokio worker
//! 上阻塞等待会让整个 runtime 停摆，所以 GUI 走 `AsyncConfirmSink`：清单 emit 给前端，
//! 答案由 [`OfferRegistry::respond`] 投递。

use std::collections::HashMap;
use std::sync::Mutex;

use tokio::sync::oneshot;

/// 正在等用户按压按钮的那些传输。
#[derive(Default)]
pub struct OfferRegistry {
    /// 传输 ID → 决策的发送端。
    pending: Mutex<HashMap<String, oneshot::Sender<bool>>>,
}

impl OfferRegistry {
    /// 空登记表。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 登记一次等待，返回决策的接收端。
    ///
    /// 同一次传输重复调用会以最新的一次为准：旧的 receiver 会被 drop 掉，等在它上面的
    /// future 立刻得到"不接受"。
    pub fn wait(&self, id: &str) -> oneshot::Receiver<bool> {
        let (answer, rx) = oneshot::channel();
        if let Ok(mut guard) = self.pending.lock() {
            guard.insert(id.to_owned(), answer);
        }
        rx
    }

    /// 投递一次决策。返回值表示这次决策真的有人在等——前端拿它区分"点迟了"。
    pub fn respond(&self, id: &str, accept: bool) -> bool {
        let Ok(mut guard) = self.pending.lock() else { return false };
        guard.remove(id).is_some_and(|answer| answer.send(accept).is_ok())
    }

    /// 放弃一次等待（取消、错误收尾）。
    ///
    /// drop 掉发送端会让等着的一端得到 `Err`，也就是"不接受"——不这么做的话，传输会永久
    /// 挂在清单确认那一步上，取消也不生效。
    pub fn abandon(&self, id: &str) {
        if let Ok(mut guard) = self.pending.lock() {
            guard.remove(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_decision_reaches_the_waiting_transfer() {
        let offers = OfferRegistry::new();
        let answer = offers.wait("recv-1");
        assert!(offers.respond("recv-1", true), "登记过就必须能投递");
        assert!(answer.await.unwrap_or(false));
    }

    #[tokio::test]
    async fn abandoning_declines_instead_of_hanging() {
        let offers = OfferRegistry::new();
        let answer = offers.wait("recv-2");
        offers.abandon("recv-2");
        assert!(!answer.await.unwrap_or(false), "取消后必须落到拒绝，不能挂住");
    }

    #[test]
    fn answering_twice_reports_the_second_was_too_late() {
        let offers = OfferRegistry::new();
        let _answer = offers.wait("recv-3");
        assert!(offers.respond("recv-3", true));
        assert!(!offers.respond("recv-3", true), "同一次等待只能被投一次");
    }
}
