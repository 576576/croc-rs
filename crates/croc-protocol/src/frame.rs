//! `comm` 帧格式：`"croc"` 魔数 + u32 小端长度 + 帧体。
//!
//! 对应 Go 的 `src/comm`。那个包里真正属于协议的部分只有两件事：**帧头布局**和
//! **64 MiB 上限**；其余全是 `net.Conn`、代理、超时这些副作用。所以这里只有两个
//! 纯函数，连接管理留给 `croc-engine`。
//!
//! # 中继保活帧
//!
//! 中继在房间里只有一个人时会反复发一个帧体为单字节 `0x01` 的**合法帧**：
//!
//! ```go
//! errSend := roomData.first.Send([]byte{1})
//! ```
//!
//! 它不是特殊帧格式，只是内容恰好是 `[1]`。接收方在等业务帧时要把它跳过，否则
//! PAKE 响应会被保活帧顶掉。

use crate::ProtocolError;

/// 帧头魔数。
pub const MAGIC: [u8; 4] = *b"croc";

/// 帧头长度：4 字节魔数 + 4 字节小端长度。
pub const HEADER_LEN: usize = 8;

/// 单帧上限，与 Go 的 `comm.maxReadMessageSize` 一致。
pub const MAX_MESSAGE_SIZE: usize = 64 * 1024 * 1024;

/// 中继保活帧的帧体。
pub const KEEPALIVE: [u8; 1] = [0x01];

/// 这个帧体是不是中继保活。
pub fn is_keepalive(payload: &[u8]) -> bool {
    payload == KEEPALIVE
}

/// 把帧体封成一个完整的 `comm` 帧。
pub fn encode_frame(payload: &[u8]) -> Result<Vec<u8>, ProtocolError> {
    if payload.len() > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge { actual: payload.len(), limit: MAX_MESSAGE_SIZE });
    }
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&MAGIC);
    // Go 用 `binary.LittleEndian.PutUint32`，长度先截成 u32。上面已经保证不会溢出。
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// 尝试从 `bytes` 的**开头**解出一帧。
///
/// - `Ok(None)`：数据还不够，调用方继续读；
/// - `Ok(Some((payload, consumed)))`：解出一帧，`consumed` 是它占用的字节数，
///   调用方把 `bytes[consumed..]` 留到下一轮。
///
/// 这个签名刻意不持有缓冲区：TCP 分片怎么切都不影响结果，重放同一段字节也得到同样的
/// 结论。缓冲区管理交给 `croc-engine`，测试里可以直接喂任意切片。
pub fn decode_frame(bytes: &[u8]) -> Result<Option<(Vec<u8>, usize)>, ProtocolError> {
    let Some(header) = bytes.get(..HEADER_LEN) else {
        return Ok(None);
    };
    let mut magic = [0u8; 4];
    let Some(magic_source) = header.get(..4) else {
        return Ok(None);
    };
    magic.copy_from_slice(magic_source);
    if magic != MAGIC {
        return Err(ProtocolError::BadMagic(magic));
    }
    let mut length = [0u8; 4];
    let Some(length_source) = header.get(4..HEADER_LEN) else {
        return Ok(None);
    };
    length.copy_from_slice(length_source);
    let length = u32::from_le_bytes(length) as usize;
    if length > MAX_MESSAGE_SIZE {
        return Err(ProtocolError::MessageTooLarge { actual: length, limit: MAX_MESSAGE_SIZE });
    }

    let Some(payload) = bytes.get(HEADER_LEN..HEADER_LEN + length) else {
        return Ok(None);
    };
    Ok(Some((payload.to_vec(), HEADER_LEN + length)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_layout_is_pinned() {
        let frame = encode_frame(b"hi").expect("应能编码");
        assert_eq!(frame, b"croc\x02\x00\x00\x00hi".to_vec());
        assert_eq!(frame.len(), HEADER_LEN + 2);
    }

    #[test]
    fn empty_payload_round_trips() {
        let frame = encode_frame(b"").expect("应能编码");
        assert_eq!(frame, b"croc\x00\x00\x00\x00".to_vec());
        assert_eq!(decode_frame(&frame).expect("应能解码"), Some((Vec::new(), HEADER_LEN)));
    }

    #[test]
    fn decode_round_trips() {
        let payload = vec![0xab; 1000];
        let frame = encode_frame(&payload).expect("应能编码");
        let (decoded, consumed) = decode_frame(&frame).expect("应能解码").expect("应已完整");
        assert_eq!(decoded, payload);
        assert_eq!(consumed, frame.len());
    }

    #[test]
    fn decode_reports_incomplete_input_without_consuming() {
        let frame = encode_frame(b"hello").expect("应能编码");
        for prefix in 0..frame.len() {
            let partial = frame.get(..prefix).expect("前缀应在范围内");
            assert_eq!(decode_frame(partial).expect("不完整不应报错"), None, "前缀长度 {prefix}");
        }
    }

    #[test]
    fn decode_ignores_trailing_bytes_and_reports_consumed() {
        let mut stream = encode_frame(b"first").expect("应能编码");
        stream.extend_from_slice(&encode_frame(b"second").expect("应能编码"));
        let (first, consumed) = decode_frame(&stream).expect("应能解码").expect("应已完整");
        assert_eq!(first, b"first");
        let (second, _) = decode_frame(stream.get(consumed..).expect("剩余字节")).expect("应能解码").expect("应已完整");
        assert_eq!(second, b"second");
    }

    #[test]
    fn bad_magic_is_rejected() {
        let mut frame = encode_frame(b"x").expect("应能编码");
        frame[0] = b'X';
        assert_eq!(decode_frame(&frame), Err(ProtocolError::BadMagic(*b"Xroc")));
    }

    #[test]
    fn oversized_length_field_is_rejected_without_allocating() {
        let mut frame = Vec::new();
        frame.extend_from_slice(&MAGIC);
        frame.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            decode_frame(&frame),
            Err(ProtocolError::MessageTooLarge { actual: u32::MAX as usize, limit: MAX_MESSAGE_SIZE })
        );
    }

    #[test]
    fn oversized_payload_is_rejected_on_encode() {
        let payload = vec![0u8; MAX_MESSAGE_SIZE + 1];
        assert_eq!(
            encode_frame(&payload),
            Err(ProtocolError::MessageTooLarge { actual: MAX_MESSAGE_SIZE + 1, limit: MAX_MESSAGE_SIZE })
        );
    }

    #[test]
    fn keepalive_is_a_plain_one_byte_frame() {
        let frame = encode_frame(&KEEPALIVE).expect("应能编码");
        assert_eq!(frame, b"croc\x01\x00\x00\x00\x01".to_vec());
        let (payload, _) = decode_frame(&frame).expect("应能解码").expect("应已完整");
        assert!(is_keepalive(&payload));
        assert!(!is_keepalive(b""));
        assert!(!is_keepalive(&[0x01, 0x01]));
    }
}
