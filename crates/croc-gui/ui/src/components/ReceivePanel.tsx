import { useT } from '../i18n';
import { Download, FolderOpen } from 'lucide-react';
import { chooseFiles } from './FileList';
import { CodeField } from './CodeField';

export function ReceivePanel({
  id,
  active,
  code,
  busy,
  outDir,
  isAndroid,
  overwrite,
  rename,
  onCode,
  onOutDir,
  onOverwrite,
  onRename,
  onStart,
}: {
  /// 窄屏切换条的 `aria-controls` 指向它。
  id: string;
  /// 窄屏下这一块是否被选中：没选中的整块由样式收掉（宽屏两块并排，不受影响）。
  active: boolean;
  code: string;
  busy: boolean;
  outDir: string;
  /// Android 上这格是 SAF 目录 URI，用户不会手打，空着就是"发到公共下载目录"。
  isAndroid: boolean;
  overwrite: boolean;
  rename: boolean;
  onCode: (next: string) => void;
  onOutDir: (next: string) => void;
  onOverwrite: (next: boolean) => void;
  onRename: (next: boolean) => void;
  onStart: () => void;
}) {
  const t = useT();
  const placeholder = isAndroid ? t('receive.outDirPlaceholder') : undefined;

  return (
    <section id={id} className={active ? 'panel mobile-active' : 'panel'} data-tour="receive">
      <div className="panel-heading">
        <span className="step">
          <Download aria-hidden="true" />
        </span>
        <h2>{t('receive.start')}</h2>
        {/* 发起按钮放在标题右边：与发送页同构，页脚不留按钮。 */}
        <div className="panel-heading-action">
          <button type="button" className="primary-button" onClick={onStart} disabled={busy || code.trim() === ''}>
            <Download aria-hidden="true" />
            {busy ? t('receive.starting') : t('receive.start')}
          </button>
        </div>
      </div>

      {/* 与发送页、SSH 页同位置同形：标题下面就是口令码那一格。
          这一格的默认动作是粘贴（码是别人给的），长按切到复制。 */}
      <span className="field-label">{t('receive.code')}</span>
      <CodeField
        value={code}
        action="paste"
        placeholder={t('receive.codePlaceholder')}
        onValue={onCode}
      />

      <span className="field-label">{t('receive.outDir')}</span>
      {/* 挑目录与输入同行：两者说的是同一件事，分两行会让这一格看着像两块。 */}
      <div className="field-row">
        <input
          type="text"
          value={outDir}
          placeholder={placeholder}
          onChange={(event) => onOutDir(event.target.value)}
        />
        <button
          type="button"
          onClick={() => void chooseFiles(false, true).then((picked) => picked[0] && onOutDir(picked[0]))}
        >
          <FolderOpen aria-hidden="true" />
          {t('receive.pickDir')}
        </button>
      </div>

      <div className="divider" />
      <label className="checkbox-label">
        <input type="checkbox" checked={overwrite} onChange={(event) => onOverwrite(event.target.checked)} />
        <span>{t('receive.overwrite')}</span>
      </label>
      <label className="checkbox-label">
        <input type="checkbox" checked={rename} onChange={(event) => onRename(event.target.checked)} />
        <span>{t('receive.rename')}</span>
      </label>
    </section>
  );
}
