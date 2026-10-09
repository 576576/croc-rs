//! 有效期的写法解析（`90m` / `12h` / `3d` / `2w`）。
//!
//! 与 Go 的 `storeapi.ParseExpiration` 同一套语法：正整数加单位，最短一分钟。这层语法是
//! 存储服务协议的一部分，所以只有一份实现——CLI 与 GUI 都从这里取。

use time::Duration;

/// 解析 `90m` / `12h` / `3d` / `2w`。
///
/// # Errors
///
/// 不是"正整数 + 单位"，或者不足一分钟。
pub fn parse_expiration(value: &str) -> Result<Duration, String> {
    let value = value.trim();
    let (digits, unit) = value.split_at(value.len().saturating_sub(1));
    if value.len() < 2 {
        return Err("duration must be a positive whole number followed by m, h, d, or w".to_owned());
    }
    let multiplier = match unit {
        "m" => Duration::MINUTE,
        "h" => Duration::HOUR,
        "d" => Duration::days(1),
        "w" => Duration::weeks(1),
        _ => return Err("duration unit must be m, h, d, or w".to_owned()),
    };
    let amount = digits.parse::<u64>().map_err(|_| "duration must be a positive whole number".to_owned())?;
    if amount == 0 {
        return Err("duration must be a positive whole number".to_owned());
    }
    let seconds = i64::try_from(amount).map_err(|_| format!("duration {value:?} is too large"))?;
    let duration = multiplier
        .checked_mul(seconds.try_into().unwrap_or(i32::MAX))
        .ok_or_else(|| format!("duration {value:?} is too large"))?;
    if duration < Duration::MINUTE {
        return Err("duration must be at least 1m".to_owned());
    }
    Ok(duration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unit_is_accepted() {
        assert_eq!(parse_expiration("1m").expect("1m"), Duration::MINUTE);
        assert_eq!(parse_expiration("12h").expect("12h"), Duration::hours(12));
        assert_eq!(parse_expiration("3d").expect("3d"), Duration::days(3));
        assert_eq!(parse_expiration("2w").expect("2w"), Duration::weeks(2));
    }

    #[test]
    fn surrounding_spaces_are_trimmed() {
        assert_eq!(parse_expiration(" 1d ").expect("带空格"), Duration::days(1));
    }

    #[test]
    fn malformed_durations_are_rejected() {
        for wrong in ["", "m", "0m", "-1h", "1x", "12", "1.5h"] {
            assert!(parse_expiration(wrong).is_err(), "{wrong:?} 应当被拒");
        }
    }
}
