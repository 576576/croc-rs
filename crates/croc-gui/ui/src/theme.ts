export type ThemeName = 'dark' | 'light';

const STORAGE_KEY = 'croc-web-theme';

/// 主题取值顺序与 `index.html` 里那段预挂载脚本一致：localStorage → 系统 → 兜底深色。
/// 两处必须同步，否则第一帧会闪一下白底。
export function resolveTheme(): ThemeName {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (stored === 'dark' || stored === 'light') return stored;
  } catch {
    // 读不到就按系统来。
  }
  return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
}

export function applyTheme(theme: ThemeName): void {
  document.documentElement.setAttribute('data-theme', theme);
  document.documentElement.style.colorScheme = theme;
  const meta = document.querySelector<HTMLMetaElement>('meta[name="theme-color"]');
  if (meta) meta.content = theme === 'dark' ? '#121411' : '#f2f1ec';
  try {
    localStorage.setItem(STORAGE_KEY, theme);
  } catch {
    // 存不下不影响使用。
  }
}
