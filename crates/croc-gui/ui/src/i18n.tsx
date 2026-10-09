import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';
import { dictionaries, localeNames, type LocaleName, type MessageKey } from './locales';
import { api } from './bridge';

const STORAGE_KEY = 'croc-gui-locale';

/// 语言 Deutsch 取值顺序：用户选过就用选过的，否则看浏览器语言，兜底英文。
/// 与 CLI 的 `locale::Locale::detect()` 判据一致（zh* 归为中文，其余英文）。
export function detectLocale(): LocaleName {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored && (localeNames as readonly string[]).includes(stored)) {
      return stored as LocaleName;
    }
  } catch {
    // localStorage 被禁（隐私模式）时退回下一档，不影响使用。
  }
  return navigator.language.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en';
}

type Translator = (key: MessageKey, vars?: Record<string, string | number>) => string;

function translate(locale: LocaleName, key: MessageKey, vars?: Record<string, string | number>): string {
  const template = dictionaries[locale][key] ?? dictionaries.en[key] ?? key;
  if (!vars) return template;
  // 只用 `{name}` 占位，不引 i18n 运行时库：这里的插值只有计数与文件名两种。
  return template.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const value = vars[name];
    return value === undefined ? whole : String(value);
  });
}

interface LocaleValue {
  locale: LocaleName;
  setLocale: (locale: LocaleName) => void;
  t: Translator;
}

const LocaleContext = createContext<LocaleValue | null>(null);

export function LocaleProvider({ children }: { children: ReactNode }) {
  const [locale, setLocaleState] = useState<LocaleName>(detectLocale);

  const setLocale = useCallback((next: LocaleName) => {
    setLocaleState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // 存不下就算了：下次启动退回浏览器语言，不会丢功能。
    }
    document.documentElement.lang = next;
    // Rust 侧的错误与摘要文案跟同一门语言（见 lib.rs 的 set_locale）。
    void api.setLocale(next).catch(() => undefined);
  }, []);

  const value = useMemo<LocaleValue>(
    () => ({ locale, setLocale, t: (key, vars) => translate(locale, key, vars) }),
    [locale, setLocale],
  );

  return <LocaleContext.Provider value={value}>{children}</LocaleContext.Provider>;
}

export function useLocale(): LocaleValue {
  const value = useContext(LocaleContext);
  if (!value) throw new Error('useLocale 必须在 LocaleProvider 里用');
  return value;
}

export function useT(): Translator {
  return useLocale().t;
}
