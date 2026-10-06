//! 中继的运行时配置。
//!
//! 默认值逐条对齐 Go 的 `src/tcp/defaults.go`——这些数字是运营参数（有多少人能同时
//! 排队、房间能挂多久），改了会让自建中继的行为和上游不一样，所以写死成常量再暴露成
//! builder，而不是散落在代码里。

use std::time::Duration;

/// 默认的最大并发握手数。
pub const DEFAULT_MAX_PENDING_HANDSHAKES: usize = 64;

/// 默认的握手超时。
pub const DEFAULT_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// 默认的最多等待房间数。
pub const DEFAULT_MAX_ROOMS_OPEN: usize = 128;

/// 默认的房间清理间隔。
pub const DEFAULT_ROOM_CLEANUP_INTERVAL: Duration = Duration::from_secs(10 * 60);

/// 默认的房间存活时间。
pub const DEFAULT_ROOM_TTL: Duration = Duration::from_secs(3 * 60 * 60);

/// 默认的单来源加入次数上限（窗口内）。
pub const DEFAULT_SOURCE_JOIN_LIMIT: usize = 30;

/// 默认的单房间加入次数上限（窗口内）。
pub const DEFAULT_ROOM_JOIN_LIMIT: usize = 30;

/// 默认的限流窗口。
pub const DEFAULT_JOIN_LIMIT_WINDOW: Duration = Duration::from_secs(60);

/// 中继的运行时配置。
///
/// 用 builder 而不是 `pub` 字段，是为了让"改了哪个参数"在调用点一眼可见，也方便以后
/// 加校验（例如"上限必须为正"）。
#[derive(Debug, Clone)]
pub struct RelayConfig {
    banner: String,
    password: String,
    max_pending_handshakes: usize,
    handshake_timeout: Duration,
    max_rooms_open: usize,
    room_cleanup_interval: Duration,
    room_ttl: Duration,
    source_join_limit: usize,
    room_join_limit: usize,
    join_limit_window: Duration,
    keepalive_interval: Duration,
}

impl Default for RelayConfig {
    fn default() -> Self {
        Self {
            banner: String::new(),
            password: String::new(),
            max_pending_handshakes: DEFAULT_MAX_PENDING_HANDSHAKES,
            handshake_timeout: DEFAULT_HANDSHAKE_TIMEOUT,
            max_rooms_open: DEFAULT_MAX_ROOMS_OPEN,
            room_cleanup_interval: DEFAULT_ROOM_CLEANUP_INTERVAL,
            room_ttl: DEFAULT_ROOM_TTL,
            source_join_limit: DEFAULT_SOURCE_JOIN_LIMIT,
            room_join_limit: DEFAULT_ROOM_JOIN_LIMIT,
            join_limit_window: DEFAULT_JOIN_LIMIT_WINDOW,
            keepalive_interval: Duration::from_secs(1),
        }
    }
}

impl RelayConfig {
    /// 默认配置。
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// 空字符串表示使用协议默认 banner（`"ok"`）。
    #[must_use]
    pub fn with_banner(mut self, banner: impl Into<String>) -> Self {
        self.banner = banner.into();
        self
    }

    /// 中继密码。空字符串表示不校验（任何客户端都能进房间）。
    #[must_use]
    pub fn with_password(mut self, password: impl Into<String>) -> Self {
        self.password = password.into();
        self
    }

    /// 同时进行握手的连接数上限；超出的一律直接断开，不占房间。
    #[must_use]
    pub fn with_max_pending_handshakes(mut self, limit: usize) -> Self {
        self.max_pending_handshakes = limit.max(1);
        self
    }

    /// 单条连接的握手超时。
    #[must_use]
    pub fn with_handshake_timeout(mut self, timeout: Duration) -> Self {
        self.handshake_timeout = timeout;
        self
    }

    /// 同时等待配对的房间数上限；超限时淘汰最老的那个。
    #[must_use]
    pub fn with_max_rooms_open(mut self, limit: usize) -> Self {
        self.max_rooms_open = limit.max(1);
        self
    }

    /// 后台清理任务的间隔。
    #[must_use]
    pub fn with_room_cleanup_interval(mut self, interval: Duration) -> Self {
        self.room_cleanup_interval = interval;
        self
    }

    /// 房间在没人配对的情况下最多存活多久。
    #[must_use]
    pub fn with_room_ttl(mut self, ttl: Duration) -> Self {
        self.room_ttl = ttl;
        self
    }

    /// 滑动窗口限流：单来源地址 / 单房间 / 窗口长度。
    #[must_use]
    pub fn with_admission_limits(mut self, source_limit: usize, room_limit: usize, window: Duration) -> Self {
        self.source_join_limit = source_limit.max(1);
        self.room_join_limit = room_limit.max(1);
        self.join_limit_window = window;
        self
    }

    /// 等待配对时给第一条连接发心跳的间隔。
    ///
    /// Go 写死 1 秒。心跳是一条内容为 `0x01` 的合法帧，接收端会跳过它——目的是让
    /// NAT / 中间设备不要把这条空闲连接掐掉。
    #[must_use]
    pub fn with_keepalive_interval(mut self, interval: Duration) -> Self {
        self.keepalive_interval = interval;
        self
    }

    /// 配置的 banner。
    #[must_use]
    pub fn banner(&self) -> &str {
        &self.banner
    }

    /// 配置的中继密码。
    #[must_use]
    pub fn password(&self) -> &str {
        &self.password
    }

    /// 并发握手上限。
    #[must_use]
    pub fn max_pending_handshakes(&self) -> usize {
        self.max_pending_handshakes
    }

    /// 握手超时。
    #[must_use]
    pub fn handshake_timeout(&self) -> Duration {
        self.handshake_timeout
    }

    /// 等待房间数上限。
    #[must_use]
    pub fn max_rooms_open(&self) -> usize {
        self.max_rooms_open
    }

    /// 房间清理间隔。
    #[must_use]
    pub fn room_cleanup_interval(&self) -> Duration {
        self.room_cleanup_interval
    }

    /// 房间存活时间。
    #[must_use]
    pub fn room_ttl(&self) -> Duration {
        self.room_ttl
    }

    /// 单来源限流上限。
    #[must_use]
    pub fn source_join_limit(&self) -> usize {
        self.source_join_limit
    }

    /// 单房间限流上限。
    #[must_use]
    pub fn room_join_limit(&self) -> usize {
        self.room_join_limit
    }

    /// 限流窗口。
    #[must_use]
    pub fn join_limit_window(&self) -> Duration {
        self.join_limit_window
    }

    /// 心跳间隔。
    #[must_use]
    pub fn keepalive_interval(&self) -> Duration {
        self.keepalive_interval
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_go_relay() {
        let config = RelayConfig::default();
        assert_eq!(config.max_pending_handshakes(), 64);
        assert_eq!(config.handshake_timeout(), Duration::from_secs(300));
        assert_eq!(config.max_rooms_open(), 128);
        assert_eq!(config.room_cleanup_interval(), Duration::from_secs(600));
        assert_eq!(config.room_ttl(), Duration::from_secs(10_800));
        assert_eq!(config.source_join_limit(), 30);
        assert_eq!(config.room_join_limit(), 30);
        assert_eq!(config.join_limit_window(), Duration::from_secs(60));
        assert_eq!(config.keepalive_interval(), Duration::from_secs(1));
        assert!(config.banner().is_empty(), "空 banner 表示用协议默认值");
    }

    #[test]
    fn the_builder_chains_and_clamps() {
        let config = RelayConfig::new()
            .with_banner("hello")
            .with_password("secret")
            .with_max_rooms_open(0)
            .with_max_pending_handshakes(0)
            .with_admission_limits(0, 0, Duration::from_secs(5));

        assert_eq!(config.banner(), "hello");
        assert_eq!(config.password(), "secret");
        // 0 会被夹到 1——"上限为零"会让中继完全不可用，那是配置错误而不是意图。
        assert_eq!(config.max_rooms_open(), 1);
        assert_eq!(config.max_pending_handshakes(), 1);
        assert_eq!(config.source_join_limit(), 1);
        assert_eq!(config.room_join_limit(), 1);
    }
}
