import { Minus, Square, X, Languages, Moon, Sun } from 'lucide-react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { applyTheme, resolveTheme, type ThemeName } from '../theme';
import { useLocale, useT } from '../i18n';
import { useState } from 'react';

/// 手机上不存在最小化窗口，所以只出现语言与主题两枚按钮。
const isDesktop = !/Android|iPhone|iPad|iPod/.test(navigator.userAgent);

export function TitleBar() {
  const t = useT();
  const { locale, setLocale } = useLocale();
  const [theme, setTheme] = useState<ThemeName>(resolveTheme());

  const toggleTheme = () => {
    const next: ThemeName = theme === 'dark' ? 'light' : 'dark';
    setTheme(next);
    applyTheme(next);
  };

  return (
    <header className="title-bar" data-tauri-drag-region>
      <span className="title" data-tauri-drag-region>
        {t('app.title')}
      </span>

      {isDesktop ? (
        <div className="controls">
          <div className="mobile-controls">
            <button type="button" className="ghost" onClick={() => setLocale(locale === 'zh-CN' ? 'en' : 'zh-CN')}>
              <Languages />
              {locale === 'zh-CN' ? 'EN' : '中文'}
            </button>
            <button type="button" className="ghost" onClick={toggleTheme}>
              {theme === 'dark' ? <Sun /> : <Moon />}
              {theme === 'dark' ? t('app.theme.light') : t('app.theme.dark')}
            </button>
          </div>
          <button type="button" className="window-control" onClick={() => void getCurrentWindow().minimize()}>
            <Minus />
          </button>
          <button type="button" className="window-control" onClick={() => void getCurrentWindow().toggleMaximize()}>
            <Square />
          </button>
          <button type="button" className="window-control" onClick={() => void getCurrentWindow().close()}>
            <X />
          </button>
        </div>
      ) : (
        <div className="mobile-controls">
          <button type="button" className="ghost" onClick={() => setLocale(locale === 'zh-CN' ? 'en' : 'zh-CN')}>
            <Languages />
            {locale === 'zh-CN' ? 'EN' : '中文'}
          </button>
          <button type="button" className="ghost" onClick={toggleTheme}>
            {theme === 'dark' ? <Sun /> : <Moon />}
          </button>
        </div>
      )}
    </header>
  );
}
