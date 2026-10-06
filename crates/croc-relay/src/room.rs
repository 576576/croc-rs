//! 房间注册表：谁在等、谁配上了、谁该被清掉。
//!
//! 一个"房间"就是一对连接。第一条连接进来自建房间，第二条进来就把两条对接起来。
//!
//! # 为什么把 `oneshot` 放进注册表
//!
//! Go 版里两条连接的 goroutine 直接共享 `*comm.Comm`，配对后由**第二条**的 goroutine
//! 同时持有两个 socket 做转发。Rust 里没法把 socket 的所有权"借"给另一个 task，所以
//! 这里反过来：第二条连接把自己的连接**送**给第一条（通过 [`oneshot`]），由第一条的
//! task 完成对接。
//!
//! 这个反转顺带解决了一个顺序问题。Go 的注释里写着：必须先给第二条连接回"准入通过"，
//! 再开始转发——否则第二条的客户端可能把对方发来的第一帧当成中继的响应。在我们这里，
//! 第二条的准入帧由它自己的 task 发出（且 [`Framed::write_frame`] 会 flush），之后才
//! 把连接送出去，所以顺序是结构上保证的，不靠"两个 goroutine 跑得够快"。

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use croc_net::Framed;
use tokio::net::TcpStream;
use tokio::sync::oneshot;

/// 加入房间的结果。
pub enum RoomJoin {
    /// 这个连接是房间里的第一位。拿着接收端等第二位到来。
    First {
        /// 第二位连接（连同它已经缓冲的字节）会从这里出来。发送端被丢弃说明房间被
        /// 清理或淘汰了——此时应当直接断开第一位。
        receiver: oneshot::Receiver<Framed<TcpStream>>,
        /// 如果因为到达房间数上限而淘汰了一个老房间，这里是它的名字（仅用于日志）。
        evicted: Option<String>,
    },
    /// 房间里已经有人在等。把接收端交给对方，并把自己的连接送过去。
    Second(oneshot::Sender<Framed<TcpStream>>),
    /// 房间已经有一对连接了。
    Full,
}

#[derive(Debug)]
struct RoomEntry {
    opened: Instant,
    /// 第一位连接交出来的"收件口"。`None` 表示已经被第二位取走。
    waiter: Option<oneshot::Sender<Framed<TcpStream>>>,
    paired: bool,
}

/// 房间注册表。
#[derive(Debug)]
pub struct RoomRegistry {
    rooms: Mutex<HashMap<String, RoomEntry>>,
    max_waiting: usize,
}

impl RoomRegistry {
    /// 创建一个最多同时挂 `max_waiting` 个等待房间的注册表。
    #[must_use]
    pub fn new(max_waiting: usize) -> Self {
        Self { rooms: Mutex::new(HashMap::new()), max_waiting: max_waiting.max(1) }
    }

    /// 尝试加入房间。
    pub fn join(&self, room: &str, now: Instant) -> RoomJoin {
        let mut rooms = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(entry) = rooms.get_mut(room) {
            if entry.paired {
                return RoomJoin::Full;
            }
            return match entry.waiter.take() {
                Some(waiter) => {
                    entry.paired = true;
                    RoomJoin::Second(waiter)
                }
                None => RoomJoin::Full,
            };
        }

        let waiting = rooms.values().filter(|entry| !entry.paired).count();
        let evicted = (waiting >= self.max_waiting).then(|| oldest_waiting(&rooms)).flatten().inspect(|oldest| {
            rooms.remove(oldest);
        });

        let (sender, receiver) = oneshot::channel();
        rooms.insert(room.to_owned(), RoomEntry { opened: now, waiter: Some(sender), paired: false });
        RoomJoin::First { receiver, evicted }
    }

    /// 把一个房间从表里去掉。丢掉里面的 `waiter` 会让还在等的第一位连接收到 `Err`。
    pub fn remove(&self, room: &str) {
        let mut rooms = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);
        rooms.remove(room);
    }

    /// 清掉超过 `ttl` 还没配对的房间，返回被清掉的名字。
    pub fn expire(&self, now: Instant, ttl: Duration) -> Vec<String> {
        let mut rooms = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);
        let expired = rooms
            .iter()
            .filter(|(_, entry)| !entry.paired && now.duration_since(entry.opened) > ttl)
            .map(|(room, _)| room.clone())
            .collect::<Vec<_>>();
        for room in &expired {
            rooms.remove(room);
        }
        expired
    }

    /// 清空所有房间（关闭中继时用）。
    pub fn clear(&self) {
        let mut rooms = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);
        rooms.clear();
    }

    /// 当前还在等配对的房间数。
    #[must_use]
    pub fn waiting(&self) -> usize {
        let rooms = self.rooms.lock().unwrap_or_else(PoisonError::into_inner);
        rooms.values().filter(|entry| !entry.paired).count()
    }
}

fn oldest_waiting(rooms: &HashMap<String, RoomEntry>) -> Option<String> {
    rooms.iter().filter(|(_, entry)| !entry.paired).min_by_key(|(_, entry)| entry.opened).map(|(room, _)| room.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_joiner_waits_and_the_second_pairs() {
        let registry = RoomRegistry::new(8);
        let now = Instant::now();

        let RoomJoin::First { receiver, evicted } = registry.join("room", now) else {
            panic!("第一位应该是 First");
        };
        assert_eq!(evicted, None);
        assert_eq!(registry.waiting(), 1);

        let RoomJoin::Second(_waiter) = registry.join("room", now) else {
            panic!("第二位应该是 Second");
        };
        assert_eq!(registry.waiting(), 0, "配对后的房间不再算等待中");

        // 第三位被拒。
        assert!(matches!(registry.join("room", now), RoomJoin::Full));
        drop(receiver);
    }

    #[test]
    fn different_rooms_do_not_interfere() {
        let registry = RoomRegistry::new(8);
        let now = Instant::now();
        assert!(matches!(registry.join("a", now), RoomJoin::First { .. }));
        assert!(matches!(registry.join("b", now), RoomJoin::First { .. }));
        assert_eq!(registry.waiting(), 2);
        assert!(matches!(registry.join("a", now), RoomJoin::Second(_)));
        assert_eq!(registry.waiting(), 1);
    }

    #[tokio::test]
    async fn the_waiting_peer_is_notified_when_the_room_is_removed() {
        let registry = RoomRegistry::new(8);
        let RoomJoin::First { receiver, .. } = registry.join("room", Instant::now()) else {
            panic!("应该是 First");
        };
        registry.remove("room");
        assert!(receiver.await.is_err(), "房间被删掉时等待端应收到 Err");
    }

    #[test]
    fn eviction_removes_the_oldest_waiting_room() {
        let registry = RoomRegistry::new(2);
        let start = Instant::now();
        assert!(matches!(registry.join("oldest", start), RoomJoin::First { .. }));
        assert!(matches!(registry.join("newer", start + Duration::from_secs(1)), RoomJoin::First { .. }));

        // 第三个房间会挤掉最老的。
        let RoomJoin::First { evicted, .. } = registry.join("newest", start + Duration::from_secs(2)) else {
            panic!("应该是 First");
        };
        assert_eq!(evicted.as_deref(), Some("oldest"));
        assert_eq!(registry.waiting(), 2);
    }

    #[test]
    fn paired_rooms_do_not_count_towards_the_waiting_limit() {
        let registry = RoomRegistry::new(1);
        let now = Instant::now();
        assert!(matches!(registry.join("a", now), RoomJoin::First { .. }));
        assert!(matches!(registry.join("a", now), RoomJoin::Second(_)));

        // "a" 已经配对了，所以它不该占等待名额——"b" 应该能正常建房间。
        let RoomJoin::First { evicted, .. } = registry.join("b", now) else {
            panic!("应该是 First");
        };
        assert_eq!(evicted, None, "配对中的房间不该被算作等待名额");
    }

    #[test]
    fn expiry_only_touches_unpaired_rooms() {
        let registry = RoomRegistry::new(8);
        let start = Instant::now();
        assert!(matches!(registry.join("waiting", start), RoomJoin::First { .. }));
        assert!(matches!(registry.join("paired", start), RoomJoin::First { .. }));
        assert!(matches!(registry.join("paired", start), RoomJoin::Second(_)));

        let later = start + Duration::from_secs(3600);
        assert_eq!(registry.expire(later, Duration::from_secs(60)), vec!["waiting".to_owned()]);
        // 配对中的房间留着——它正在传数据，删掉会打断传输。
        assert_eq!(registry.waiting(), 0);
    }

    #[test]
    fn clear_drops_everything() {
        let registry = RoomRegistry::new(8);
        let now = Instant::now();
        assert!(matches!(registry.join("a", now), RoomJoin::First { .. }));
        registry.clear();
        assert_eq!(registry.waiting(), 0);
    }
}
