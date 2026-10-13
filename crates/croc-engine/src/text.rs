//! `--text` 与管道内容落成的临时文件。
//!
//! 名字前缀 `croc-stdin-` 是协议的一部分：接收方靠它认出"这次传的是文本，不是一个名字
//! 碰巧很怪的文件"（判定在 `croc_core::policy::is_text_artifact`）。所以这个名字只有一处
//! 实现——CLI 与 GUI 都调这里，各写一份迟早会漂。

use std::path::PathBuf;

use croc_core::policy::TEXT_ARTIFACT_PREFIX;

use crate::error::EngineError;

/// 把一段文本落成临时文件，返回它的路径。
///
/// # Errors
///
/// 临时目录写不下去。
pub fn write_text_artifact(text: &str) -> Result<PathBuf, EngineError> {
    let path = artifact_path();
    std::fs::write(&path, text)
        .map_err(|error| EngineError::File { path: path.display().to_string(), reason: error.to_string() })?;
    Ok(path)
}

/// 一个 `<前缀><纳秒时间戳>` 的临时文件路径。
///
/// 时间戳取纳秒，是为了让同一进程内的两次文本发送不撞名——文件名要传到对端，撞名会让
/// 接收方的覆盖判定失去意义。
#[must_use]
pub fn artifact_path() -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("{TEXT_ARTIFACT_PREFIX}{stamp}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_names_carry_the_protocol_prefix() {
        let path = artifact_path();
        let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
        assert!(name.starts_with(TEXT_ARTIFACT_PREFIX), "{name} 少了协议前缀");
    }

    #[test]
    fn writing_round_trips_the_text() {
        let path = write_text_artifact("hello").expect("写临时文件");
        let read = std::fs::read_to_string(&path).expect("读回来");
        assert_eq!(read, "hello");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn two_artifacts_do_not_collide() {
        assert_ne!(artifact_path(), artifact_path());
    }
}
