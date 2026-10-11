import { useT } from '../i18n';
import { Download, FolderOpen } from 'lucide-react';
import { chooseFiles } from './FileList';

export function ReceivePanel({
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
    <section className="panel" data-tour="receive">
      <div className="panel-heading">
        <span className="step">
          <Download aria-hidden="true" />
        </span>
        <div>
          <h2>{t('receive.start')}</h2>
          <p>{t('receive.intro')}</p>
        </div>
      </div>

      <span className="field-label">{t('receive.code')}</span>
      <input
        type="text"
        value={code}
        placeholder={t('receive.codePlaceholder')}
        onChange={(event) => onCode(event.target.value)}
      />

      <span className="field-label">{t('receive.outDir')}</span>
      <input
        type="text"
        value={outDir}
        placeholder={placeholder}
        onChange={(event) => onOutDir(event.target.value)}
      />
      <div className="toolbar-row">
        <button
          type="button"
          className="ghost"
          onClick={() => void chooseFiles(false, true).then((picked) => picked[0] && onOutDir(picked[0]))}
        >
          <FolderOpen />
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

      <div className="divider" />
      <button type="button" onClick={onStart} disabled={busy || code.trim() === ''}>
        <Download />
        {busy ? t('receive.starting') : t('receive.start')}
      </button>
    </section>
  );
}
