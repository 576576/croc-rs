import { createContext, useCallback, useContext, useMemo, useState, type ReactNode } from 'react';
import { dictionaries, localeNames, type LocaleName, type MessageKey } from './locales';
import { api } from './bridge';

const STORAGE_KEY = 'croc-gui-locale';

/// `auto` 不是一门语言，而是"还没选过"：落到系统语言。
export type LocalePreference = 'auto' | LocaleName;

/// 判据与 CLI 的 `locale::Locale::detect()` 一致：zh* 归为中文，其余英文。
function systemLocale(): LocaleName {
  return navigator.language.toLowerCase().startsWith('zh') ? 'zh-CN' : 'en';
}

function readPreference(): LocalePreference {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'auto' || (localeNames as readonly string[]).includes(stored ?? '')) {
      return stored as LocalePreference;
    }
  } catch {
    // localStorage 被禁（隐私模式）时按自动处理，不影响功能。
  }
  return 'auto';
}

function resolveLocale(preference: LocalePreference): LocaleName {
  return preference === 'auto' ? systemLocale() : preference;
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
  /// 实际生效的语言。
  locale: LocaleName;
  /// 用户选的那一档，可能是 `auto`。
  preference: LocalePreference;
  setPreference: (next: LocalePreference) => void;
  t: Translator;
}

const LocaleContext = createContext<LocaleValue | null>(null);

export function LocaleProvider({ children }: { children: ReactNode }) {
  const [preference, setPreferenceState] = useState<LocalePreference>(readPreference);
  const locale = resolveLocale(preference);

  const setPreference = useCallback((next: LocalePreference) => {
    setPreferenceState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      // 存不下不影响功能：下次启动回到自动。
    }
    const resolved = resolveLocale(next);
    document.documentElement.lang = resolved;
    // Rust 侧的错误与摘要文案跟同一门语言（见 lib.rs 的 set_locale）。
    void api.setLocale(resolved).catch(() => undefined);
  }, []);

  const value = useMemo<LocaleValue>(
    () => ({ locale, preference, setPreference, t: (key, vars) => translate(locale, key, vars) }),
    [locale, preference, setPreference],
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
