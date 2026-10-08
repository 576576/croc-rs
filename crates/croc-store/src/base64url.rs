//! base64url 无填充。协议里的字节串都用它：传输 ID、主密钥、文件哈希、来源。

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;

/// 编码。
#[must_use]
pub fn encode(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// 解码。
///
/// # Errors
///
/// 不是合法的 base64url 时返回错。
pub fn decode(value: &str) -> Result<Vec<u8>, base64::DecodeError> {
    URL_SAFE_NO_PAD.decode(value)
}
