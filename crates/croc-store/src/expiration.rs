//! 有效期的写法解析（`90m` / `12h` / `3d` / `2w`）。
//!
//! 与 Go 的 `storeapi.ParseExpiration` 同一套语法：正整数加单位，最短一分钟。这层语法是
//! 存储服务协议的一部分，所以只有一份实现——CLI 与 GUI 都从这里取。

use time::Duration;

use crate::Error;

/// 解析 `90m` / `12h` / `3d` / `2w`。
///
/// # Errors
///
/// 不是"正整数 + 单位"，或者不足一分钟。
pub fn parse_expiration(value: &str) -> Result<Duration, Error> {
    let value = value.trim();
    // 单位取最后一个字符，不是最后一个字节。`len()` 给的是字节数，按它切会在多字节
    // 字符内部断开——`split_at` 遇到这种切点直接 panic（`"1天"` 就够触发）。
    let mut chars = value.chars();
    let Some(unit) = chars.next_back() else {
        return Err(Error::Expiration("duration must be a positive whole number followed by m, h, d, or w".to_owned()));
    };
    let digits = chars.as_str();
    let multiplier = match unit {
        'm' => Duration::MINUTE,
        'h' => Duration::HOUR,
        'd' => Duration::days(1),
        'w' => Duration::weeks(1),
        _ => return Err(Error::Expiration("duration unit must be m, h, d, or w".to_owned())),
    };
    let amount =
        digits.parse::<u64>().map_err(|_| Error::Expiration("duration must be a positive whole number".to_owned()))?;
    if amount == 0 {
        return Err(Error::Expiration("duration must be a positive whole number".to_owned()));
    }
    // `Duration::checked_mul` 只收 i32，所以要窄化。窄化失败必须报错：取 `i32::MAX` 顶上
    // 的话，下面的 `checked_mul` 不会溢出（`i32::MAX` 天 ≈ 1.86e14 秒，i64 装得下），
    // 于是超范围的值被静默改成一个用户没写过的数，`ok_or_else` 形同虚设。
    let amount = i32::try_from(amount).map_err(|_| Error::Expiration(format!("duration {value:?} is too large")))?;
    let duration =
        multiplier.checked_mul(amount).ok_or_else(|| Error::Expiration(format!("duration {value:?} is too large")))?;
    if duration < Duration::MINUTE {
        return Err(Error::Expiration("duration must be at least 1m".to_owned()));
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

    #[test]
    fn non_ascii_units_are_rejected_without_panicking() {
        // 按字节取尾字符会在这些输入上 panic（切点落进字符内部）。
        for wrong in ["1天", "30分钟", "1周", "１ｄ"] {
            assert!(parse_expiration(wrong).is_err(), "{wrong:?} 应当被拒");
        }
    }

    #[test]
    fn out_of_range_durations_are_rejected() {
        // 这些曾经被 `unwrap_or(i32::MAX)` 静默截成 i32::MAX 天。
        for wrong in ["99999999999d", "99999999999999w", "5000000000h"] {
            assert!(parse_expiration(wrong).is_err(), "{wrong:?} 应当被拒");
        }
    }
}
