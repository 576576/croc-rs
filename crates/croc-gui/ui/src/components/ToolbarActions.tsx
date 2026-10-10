import { useEffect, useRef, useState } from 'react';
import { CircleQuestionMark, Languages, Moon, Sun } from 'lucide-react';
import { REPOSITORY_URL, api } from '../bridge';
import { useLocale, useT, type LocalePreference } from '../i18n';
import { applyTheme, resolveTheme, type ThemeName } from '../theme';

/// GitHub 的标记：lucide 1.53 已经不带品牌图标，这里内联官方那条路径。
function GithubMark() {
  return (
    <svg viewBox="0 0 24 24" width="18" height="18" fill="currentColor" aria-hidden="true">
      <path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12" />
    </svg>
  );
}

const OPTIONS: LocalePreference[] = ['auto', 'en', 'zh-CN'];

/// 右上角那四枚按钮。顺序与 croc web 一致：指引 / 语言 / 仓库 / 深浅色。
export function ToolbarActions({
  tourActive,
  onToggleTour,
}: {
  tourActive: boolean;
  onToggleTour: () => void;
}) {
  const t = useT();
  const { preference, setPreference } = useLocale();
  const [theme, setTheme] = useState<ThemeName>(resolveTheme);
  const [menuOpen, setMenuOpen] = useState(false);
  const picker = useRef<HTMLDivElement>(null);

  // 菜单在别处点一下就收起来：不监听的话它会一直浮在页面上挡住内容。
  useEffect(() => {
    if (!menuOpen) return;
    const close = (event: MouseEvent) => {
      if (!picker.current?.contains(event.target as Node)) setMenuOpen(false);
    };
    document.addEventListener('mousedown', close);
    return () => document.removeEventListener('mousedown', close);
  }, [menuOpen]);

  const toggleTheme = () => {
    const next: ThemeName = theme === 'dark' ? 'light' : 'dark';
    setTheme(next);
    applyTheme(next);
  };

  const label = (option: LocalePreference) =>
    option === 'auto' ? t('language.auto') : option === 'en' ? 'English' : '中文';

  return (
    <div className="workspace-toolbar-actions">
      <button
        type="button"
        className={`icon-button${tourActive ? ' active' : ''}`}
        aria-pressed={tourActive}
        aria-label={t('nav.howToUse')}
        title={t('nav.howToUse')}
        onClick={onToggleTour}
      >
        <CircleQuestionMark aria-hidden="true" />
      </button>

      <div className="language-picker" ref={picker}>
        <button
          type="button"
          className="icon-button"
          aria-label={t('nav.language')}
          title={t('nav.language')}
          aria-expanded={menuOpen}
          aria-haspopup="menu"
          onClick={() => setMenuOpen((open) => !open)}
        >
          <Languages aria-hidden="true" />
        </button>
        {menuOpen && (
          <div className="language-menu" role="menu">
            {OPTIONS.map((option) => (
              <button
                key={option}
                type="button"
                role="menuitemradio"
                aria-checked={preference === option}
                onClick={() => {
                  setPreference(option);
                  setMenuOpen(false);
                }}
              >
                {label(option)}
              </button>
            ))}
          </div>
        )}
      </div>

      <button
        type="button"
        className="icon-button"
        aria-label={t('nav.github')}
        title={t('nav.github')}
        onClick={() => void api.openUrl(REPOSITORY_URL)}
      >
        <GithubMark />
      </button>

      <button
        type="button"
        className="icon-button"
        aria-label={t('nav.theme')}
        title={t('nav.theme')}
        onClick={toggleTheme}
      >
        {theme === 'dark' ? <Sun aria-hidden="true" /> : <Moon aria-hidden="true" />}
      </button>
    </div>
  );
}
