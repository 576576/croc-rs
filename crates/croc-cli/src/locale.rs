//! 界面语言。
//!
//! 只有中文和英文两种。判定顺序是 `LC_ALL` → `LC_MESSAGES` → `LANG` → 系统区域 →
//! 英文；前缀是 `zh` 用中文，其余一律英文。
//!
//! 三项 `LC_*` / `LANG` 都非空时就以它们为准，不再看系统区域：它们是 POSIX 上的权威来源，
//! 系统区域只作兜底。
//!
//! 形如 `zh_CN.UTF-8@euro` 的取值要剥掉 `.编码` 与 `@修饰符` 再比。

use sys_locale::get_locale;

/// 界面语言。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Locale {
    /// 英文。
    En,
    /// 中文。
    Zh,
}

impl Locale {
    /// `rust-i18n` 用的语言标签，与 `locales/` 下的文件名对应。
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh-CN",
        }
    }

    /// 从当前环境判定。
    #[must_use]
    pub fn detect() -> Self {
        Self::resolve(
            std::env::var("LC_ALL").ok().as_deref(),
            std::env::var("LC_MESSAGES").ok().as_deref(),
            std::env::var("LANG").ok().as_deref(),
            get_locale().as_deref(),
        )
    }

    /// 判定逻辑本体。四个入参依次是 `LC_ALL` / `LC_MESSAGES` / `LANG` / 系统区域。
    ///
    /// 抽成纯函数是为了能脱开真实环境测优先级。
    #[must_use]
    pub fn resolve(lc_all: Option<&str>, lc_messages: Option<&str>, lang: Option<&str>, system: Option<&str>) -> Self {
        // `C` / `POSIX` 是"不要本地化"的显式选择，此刻不能再退到系统区域——
        // 它表达的就是"给我英文"，而不是"我没设"。
        for value in [lc_all, lc_messages, lang].into_iter().flatten() {
            if let Some(tag) = normalize(value) {
                return Self::from_tag(&tag);
            }
        }
        system.and_then(normalize).map_or(Self::En, |tag| Self::from_tag(&tag))
    }

    /// 语言标签 → 语言。只认 `zh` 前缀，别的都是英文。
    fn from_tag(tag: &str) -> Self {
        if tag.starts_with("zh") { Self::Zh } else { Self::En }
    }
}

/// 把环境判出来的语言装进 `rust-i18n`。
///
/// 要在打出第一句话之前调一次：`t!` 不传 `locale` 时用的是这里的全局值。
pub fn install() {
    rust_i18n::set_locale(Locale::detect().tag());
}

/// 剥掉编码与修饰符，转小写。空串（以及 `C` / `POSIX`）返回 `Some("c")`，
/// 由调用方决定怎么处理。
fn normalize(value: &str) -> Option<String> {
    let head = value.split(['.', '@']).next()?.trim();
    if head.is_empty() {
        return None;
    }
    let head = head.replace('_', "-").to_ascii_lowercase();
    // `C` / `POSIX` 是有效取值（意思是英文），不能当成"没设"。
    (!head.is_empty()).then_some(head)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_prefixes_win_over_the_system_locale() {
        assert_eq!(Locale::resolve(Some("zh_CN.UTF-8"), None, Some("en_US.UTF-8"), None), Locale::Zh);
        assert_eq!(Locale::resolve(None, Some("zh-TW"), None, Some("en-US")), Locale::Zh);
        assert_eq!(Locale::resolve(None, None, Some("zh"), None), Locale::Zh);
    }

    #[test]
    fn the_first_non_empty_variable_wins() {
        // `LC_ALL` 压得住 `LANG`，与 POSIX 一致。
        assert_eq!(Locale::resolve(Some("en_GB.UTF-8"), None, Some("zh_CN.UTF-8"), None), Locale::En);
        // 空串等于没设。
        assert_eq!(Locale::resolve(Some(""), Some("zh_CN"), None, None), Locale::Zh);
    }

    #[test]
    fn the_system_locale_is_only_a_fallback() {
        assert_eq!(Locale::resolve(None, None, None, Some("zh-CN")), Locale::Zh);
        assert_eq!(Locale::resolve(None, None, None, Some("de-DE")), Locale::En);
        assert_eq!(Locale::resolve(None, None, None, None), Locale::En);
    }

    #[test]
    fn the_c_locale_means_english_and_blocks_the_fallback() {
        // `LANG=C` 是"给我英文"的显式选择，不该再被系统区域（中文）翻回来。
        assert_eq!(Locale::resolve(None, None, Some("C"), Some("zh-CN")), Locale::En);
        assert_eq!(Locale::resolve(None, None, Some("POSIX"), Some("zh-CN")), Locale::En);
    }

    #[test]
    fn encoding_and_modifiers_are_stripped() {
        assert_eq!(Locale::resolve(None, None, Some("zh_CN.UTF-8@euro"), None), Locale::Zh);
        assert_eq!(Locale::resolve(None, None, Some("  en_US  "), None), Locale::En);
    }
}
