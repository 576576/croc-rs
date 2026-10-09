//! Go 风格的时长解析。
//!
//! `croc relay --handshake-timeout 5m` 这类取值在 Go 版里是 `time.Duration`，接受
//! `5m` / `300s` / `1h30m` 这样的写法。本实现不能只收秒数——那样同一个命令行在两边
//! 行为不同，而且 `(default: 5m0s)` 这个显示值也对不上。
//!
//! 单位表与 Go 的 `time.ParseDuration` 一致：`ns` / `us` / `µs` / `μs` / `ms` / `s` /
//! `m` / `h`。不带单位的纯 `0` 也接受（Go 允许）。

use std::time::Duration;

/// 解析一个 Go 风格的时长字符串。
///
/// # Errors
///
/// 格式不对（缺单位、单位不认识、数字部分为空、溢出）时返回可以整行打给用户的原因。
pub fn parse(text: &str) -> Result<Duration, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("时长为空".to_owned());
    }
    if trimmed == "0" {
        return Ok(Duration::ZERO);
    }
    let (digits, negative) = trimmed.strip_prefix('-').map_or((trimmed, false), |rest| (rest, true));
    let mut total = Duration::ZERO;
    let mut rest = digits;
    let invalid = || format!("{text:?} 不是合法时长（例：5m、300s、1h30m）");
    while !rest.is_empty() {
        let number_end = rest.find(|c: char| !c.is_ascii_digit() && c != '.').unwrap_or(rest.len());
        let (number, tail) = rest.split_at(number_end);
        if number.is_empty() || number == "." {
            return Err(invalid());
        }
        let value: f64 = number.parse().map_err(|_| invalid())?;
        let unit_end = tail.find(|c: char| c.is_ascii_digit() || c == '.').unwrap_or(tail.len());
        let (unit, remaining) = tail.split_at(unit_end);
        let scale = unit_scale(unit).ok_or_else(|| format!("{text:?} 里的单位 {unit:?} 不认识"))?;
        // 用 `try_from_secs_f64` 而不是 `from_secs_f64`：后者对超范围的值直接 panic，
        // 而这里的输入完全来自命令行。
        let piece = Duration::try_from_secs_f64(value * scale).map_err(|_| invalid())?;
        total = total.checked_add(piece).ok_or_else(invalid)?;
        rest = remaining;
    }
    if negative {
        return Err(format!("{text:?} 是负数；时长必须为正"));
    }
    Ok(total)
}

/// 单位 → 秒数。
fn unit_scale(unit: &str) -> Option<f64> {
    Some(match unit {
        "ns" => 1e-9,
        "us" | "µs" | "μs" => 1e-6,
        "ms" => 1e-3,
        "s" => 1.0,
        "m" => 60.0,
        "h" => 3600.0,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_units_go_accepts_are_accepted() {
        assert_eq!(parse("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(parse("300s"), Ok(Duration::from_secs(300)));
        assert_eq!(parse("1h30m"), Ok(Duration::from_secs(5400)));
        assert_eq!(parse("12h"), Ok(Duration::from_secs(43_200)));
        assert_eq!(parse("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse("0"), Ok(Duration::ZERO));
    }

    #[test]
    fn the_forms_the_flags_default_to_round_trip() {
        // 帮助里显示的默认值就是这几个字符串。
        assert_eq!(parse("5m0s"), Ok(Duration::from_secs(300)));
        assert_eq!(parse("1m0s"), Ok(Duration::from_secs(60)));
        assert_eq!(parse("12h0m0s"), Ok(Duration::from_secs(43_200)));
        assert_eq!(parse("2m0s"), Ok(Duration::from_secs(120)));
    }

    #[test]
    fn garbage_is_rejected_with_a_readable_reason() {
        assert!(parse("").is_err());
        assert!(parse("5").is_err(), "缺单位");
        assert!(parse("5x").is_err(), "单位不认识");
        assert!(parse("-5m").is_err(), "负数");
    }
}
