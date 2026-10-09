//! `--qrcode`：把 web 接收链接渲染成终端里的二维码。
//!
//! Go 用 `skip2/go-qrcode` 的 `ToSmallString`。这里只拿它的编码器，
//! 渲染是自己拼的半角块字符——省掉一整套图形依赖。

use qrcode::{Color, EcLevel, QrCode};

/// 二维码四周的静默区（模块数）。
///
/// 留白不够扫描器就找不到定位角。这里左右各留 2 个模块，上下各留 1 行——上下是
/// 两行像素合成一行，所以 1 行等于 2 个模块。
const QUIET: isize = 2;

/// web 接收页的链接，对应 Go 的 `webReceiveURL`。
#[must_use]
pub fn web_receive_url(code: &str) -> String {
    format!("https://getcroc.com/?code={}", percent_encode(code))
}

/// 把链接渲染成终端可打印的二维码。
///
/// 每两行像素合成一行，用 `▀` `▄` `█` 与空格表示。黑模块画成亮块——深色终端下
/// 这才呈现出"白底黑块"，跟 `qrencode -t UTF8` 一样。
///
/// # Errors
///
/// 链接长到二维码装不下时，返回一句给用户看的说明。
pub fn render(url: &str) -> Result<String, String> {
    let code = QrCode::with_error_correction_level(url.as_bytes(), EcLevel::M)
        .map_err(|error| format!("croc-rs: 生成二维码失败：{error}"))?;
    let width = isize::try_from(code.width()).unwrap_or(isize::MAX);
    let modules = code.to_colors();
    let dark = |x: isize, y: isize| {
        (0..width).contains(&x)
            && (0..width).contains(&y)
            && modules
                .get(usize::try_from(y * width + x).unwrap_or(usize::MAX))
                .is_some_and(|color| *color == Color::Dark)
    };

    let low = -QUIET;
    let high = width + QUIET;
    let mut out = String::new();
    let mut top = low;
    while top < high {
        for x in low..high {
            out.push(match (dark(x, top), dark(x, top + 1)) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        out.push('\n');
        top += 2;
    }
    Ok(out)
}

/// 最小的百分号编码，等价于 Go 的 `url.QueryEscape` 里我们够用的那部分。
///
/// 口令码本身是 `[a-z0-9-]`，转义后原样；这个函数是为用户用 `--code` 塞进别的字符时
/// 兜底的——不编码就会拼出一个坏 URL。
fn percent_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(byte));
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_web_url_matches_go() {
        // Go：`"https://getcroc.com/?code=" + url.QueryEscape(code)`。
        assert_eq!(web_receive_url("acid-acorn-acre"), "https://getcroc.com/?code=acid-acorn-acre");
        assert_eq!(web_receive_url("a b&c"), "https://getcroc.com/?code=a%20b%26c");
    }

    #[test]
    fn the_rendered_code_is_square_and_has_a_quiet_zone() {
        let text = render(&web_receive_url("acid-acorn-acre")).expect("应能渲染");
        let lines: Vec<&str> = text.lines().collect();
        assert!(lines.len() > 8, "画得太小多半是编码错了：{} 行", lines.len());
        // 左右静默区：每行首尾都是空格（白模块）。
        for line in &lines {
            assert!(line.starts_with(' '), "缺少左静默区：{line:?}");
            assert!(line.ends_with(' '), "缺少右静默区：{line:?}");
        }
        let width = lines[0].chars().count();
        assert!(lines.iter().all(|line| line.chars().count() == width), "每行应当一样宽");
    }

    #[test]
    fn an_over_long_url_is_reported_instead_of_panicking() {
        let error = render(&"x".repeat(5000)).expect_err("装不下应当报错");
        assert!(error.starts_with("croc-rs: "), "报错要带程序名前缀：{error}");
    }
}
