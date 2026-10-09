//! comm 帧的异步读写端。
//!
//! 把任意 `AsyncRead + AsyncWrite`（`TcpStream`、TLS 流、内存里的 `DuplexStream`、
//! 将来 Tailscale 的 `ByteStream`）包成一个"读一整条帧 / 写一整条帧"的接口。
//!
//! # 为什么是 `read_frame` 而不是 `read`
//!
//! Go 版的 `comm.Comm` 把"攒够 8 字节头 → 按长度攒够帧体"这段逻辑和读循环缠在一起，
//! 于是每条用到它的地方都要自己处理"这次只读到半条帧"。这里把它收进一个类型：调用方
//! 拿到的永远是完整的一帧，或者 `None`（对端干净关闭）。
//!
//! 内部用 [`bytes::BytesMut`] 而不是 `Vec<u8>`，是因为 `BytesMut::advance` 只是移动
//! 游标、不搬运内存——连续处理一串帧时不会反复 memmove。

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::CrocIoError;

/// 默认读缓冲大小。一条帧最大 64 MiB，所以缓冲会按需增长；这个初值只是省掉前几次扩容。
const DEFAULT_CAPACITY: usize = 16 * 1024;

/// 把字节流包成 comm 帧的读写端。
#[derive(Debug)]
pub struct Framed<S> {
    inner: S,
    buffer: BytesMut,
}

impl<S> Framed<S> {
    /// 用一个空缓冲包装。
    pub fn new(inner: S) -> Self {
        Self { inner, buffer: BytesMut::with_capacity(DEFAULT_CAPACITY) }
    }

    /// 指定初始缓冲容量。已知对端会立刻发大帧时（例如数据端口）可以调大。
    pub fn with_capacity(inner: S, capacity: usize) -> Self {
        Self { inner, buffer: BytesMut::with_capacity(capacity) }
    }

    /// 底层流的只读引用。
    pub fn get_ref(&self) -> &S {
        &self.inner
    }

    /// 底层流的可变引用。不要绕过 `Framed` 直接读——会让内部缓冲与实际流脱节。
    pub fn get_mut(&mut self) -> &mut S {
        &mut self.inner
    }

    /// 拆回底层流。缓冲里若还有半条帧会一并丢弃，所以只该在确定没有未消费数据时调用。
    pub fn into_inner(self) -> S {
        self.inner
    }

    /// 已经缓冲、尚未构成完整帧的字节数。
    ///
    /// 中继在配对成功之后要把两条连接"对接"起来：那时双方可能已经把第一条帧发出来了。
    /// 这个数字用来判断"还有没有没转发的数据"。
    pub fn buffered(&self) -> usize {
        self.buffer.len()
    }

    /// 取走缓冲里剩余的字节，并把缓冲清空。
    ///
    /// 中继把两条连接对接时用：握手是按帧读的，但一次底层读可能顺手把下一帧的开头也
    /// 拉进缓冲。这些字节属于"应用数据"方向，必须在开始裸转发之前先送过去，否则会被
    /// 静默丢掉。
    pub fn take_buffered(&mut self) -> Vec<u8> {
        self.buffer.split().to_vec()
    }
}

impl<S: AsyncRead + Unpin> Framed<S> {
    /// 读一整条帧的帧体。
    ///
    /// 返回 `Ok(None)` 表示对端干净地关闭了连接（正好停在帧边界上）。如果对方在
    /// 帧中间断开，返回 [`CrocIoError::TruncatedFrame`]——这两种情况在协议上完全不同，
    /// 混在一起会让"对端正常退出"和"有人在截断流量"看起来一样。
    ///
    /// # Errors
    ///
    /// - 底层读失败；
    /// - 帧头 magic 不对、或声明长度超过 64 MiB（[`CrocIoError::Protocol`]）；
    /// - 帧中途断线（[`CrocIoError::TruncatedFrame`]）。
    pub async fn read_frame(&mut self) -> Result<Option<Vec<u8>>, CrocIoError> {
        loop {
            if let Some((payload, consumed)) = croc_protocol::decode_frame(&self.buffer)? {
                self.buffer.advance(consumed);
                return Ok(Some(payload));
            }

            let read = self.inner.read_buf(&mut self.buffer).await?;
            if read == 0 {
                return if self.buffer.is_empty() {
                    Ok(None)
                } else {
                    Err(CrocIoError::TruncatedFrame { buffered: self.buffer.len() })
                };
            }
        }
    }
}

impl<S: AsyncWrite + Unpin> Framed<S> {
    /// 写一整条帧，并立刻 flush。
    ///
    /// 中继的握手只有几个帧，flush 的开销可以忽略；而把"写完就走"做成语义，能避免
    /// 那种"帧还在写缓冲里，程序却已经开始等对方回复"的死锁。
    ///
    /// # Errors
    ///
    /// 帧体超过 64 MiB，或底层写失败。
    pub async fn write_frame(&mut self, payload: &[u8]) -> Result<(), CrocIoError> {
        let bytes = croc_protocol::encode_frame(payload)?;
        self.inner.write_all(&bytes).await?;
        self.inner.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::duplex;

    #[tokio::test]
    async fn frames_round_trip() {
        let (client, server) = duplex(64 * 1024);
        let mut writer = Framed::new(client);
        let mut reader = Framed::new(server);

        writer.write_frame(b"first").await.expect("应能写");
        writer.write_frame(b"").await.expect("应能写空帧");
        writer.write_frame(b"third").await.expect("应能写");

        assert_eq!(reader.read_frame().await.expect("应能读"), Some(b"first".to_vec()));
        assert_eq!(reader.read_frame().await.expect("应能读"), Some(Vec::new()));
        assert_eq!(reader.read_frame().await.expect("应能读"), Some(b"third".to_vec()));
    }

    #[tokio::test]
    async fn a_clean_close_reports_none() {
        let (client, server) = duplex(1024);
        drop(client);
        let mut reader = Framed::new(server);
        assert_eq!(reader.read_frame().await.expect("干净关闭不该报错"), None);
    }

    #[tokio::test]
    async fn a_close_in_the_middle_of_a_frame_is_reported() {
        let (mut client, server) = duplex(1024);
        // 只写头，不写体，然后关掉。
        client.write_all(b"croc\x10\x00\x00\x00").await.expect("应能写");
        drop(client);

        let mut reader = Framed::new(server);
        assert!(matches!(reader.read_frame().await, Err(CrocIoError::TruncatedFrame { buffered: 8 })));
    }

    #[tokio::test]
    async fn a_bad_magic_is_rejected() {
        let (mut client, server) = duplex(1024);
        client.write_all(b"not!\x00\x00\x00\x00").await.expect("应能写");
        client.flush().await.expect("应能 flush");

        let mut reader = Framed::new(server);
        assert!(matches!(reader.read_frame().await, Err(CrocIoError::Protocol(_))));
    }

    #[tokio::test]
    async fn large_frames_survive_fragmentation() {
        // duplex 的缓冲有限，一条 256 KiB 的帧必然被拆成多次读——正好验证缓冲累积。
        let (client, server) = duplex(4096);
        let payload = vec![0xa5u8; 256 * 1024];

        let writer_task = {
            let payload = payload.clone();
            tokio::spawn(async move {
                let mut writer = Framed::new(client);
                writer.write_frame(&payload).await.expect("应能写");
            })
        };

        let mut reader = Framed::new(server);
        assert_eq!(reader.read_frame().await.expect("应能读"), Some(payload));
        writer_task.await.expect("写任务应结束");
    }

    #[tokio::test]
    async fn buffered_bytes_are_visible_before_a_full_frame_arrives() {
        let (mut client, server) = duplex(1024);
        client.write_all(b"croc\x04\x00\x00\x00").await.expect("应能写");
        client.flush().await.expect("应能 flush");

        let mut reader = Framed::new(server);
        // 只给了头，还没给体：这里会阻塞，所以用超时来观察"缓冲里确实攒了 8 字节"。
        let peek = tokio::time::timeout(std::time::Duration::from_millis(50), reader.read_frame()).await;
        assert!(peek.is_err(), "帧不完整时不该返回");
        assert_eq!(reader.buffered(), 8);
    }
}
