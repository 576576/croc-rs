import { useT } from '../i18n';
import { FileArchive, FolderOpen, Upload } from 'lucide-react';
import { useState } from 'react';
import { chooseFiles, FileList } from './FileList';
import { SendCode } from './SendCode';
import type { Settings } from './SettingsGrid';

export type ContentMode = 'files' | 'text';

/// Tauri 的 drop 事件给的是路径数组；Web 的 `File` 对象只有名字，拿不到路径。
/// 这条直流量只走前者，所以桌面端与 Android 行为一致。
export function SendPanel({
  mode,
  files,
  text,
  busy,
  code,
  settings,
  customCode,
  onMode,
  onFiles,
  onText,
  onCustomCode,
  onStart,
}: {
  mode: ContentMode;
  files: string[];
  text: string;
  busy: boolean;
  code: string | null;
  settings: Settings;
  customCode: string;
  onMode: (next: ContentMode) => void;
  onFiles: (next: string[]) => void;
  onText: (next: string) => void;
  onCustomCode: (next: string) => void;
  onStart: () => void;
}) {
  const t = useT();
  const [dragging, setDragging] = useState(false);

  const pick = async (directory: boolean) => {
    const picked = await chooseFiles(true, directory);
    if (picked.length > 0) onFiles(picked);
  };

  const somethingToSend = mode === 'files' ? files.length > 0 : text.trim() !== '';

  return (
    <section className="panel">
      <span className="field-label">{t('send.content')}</span>
      <div className="workspace-switch four">
        <button type="button" aria-selected={mode === 'files'} onClick={() => onMode('files')}>
          <FileArchive />
          {t('send.content.files')}
        </button>
        <button type="button" aria-selected={mode === 'text'} onClick={() => onMode('text')}>
          <Upload />
          {t('send.content.text')}
        </button>
      </div>

      {mode === 'files' ? (
        <>
          <div
            className="drop-zone"
            data-dragging={dragging}
            onClick={() => void pick(false)}
            onDragOver={(event) => {
              event.preventDefault();
              setDragging(true);
            }}
            onDragLeave={() => setDragging(false)}
            onDrop={(event) => {
              event.preventDefault();
              setDragging(false);
              const dropped = Array.from(event.dataTransfer.files, (file) => file.name);
              if (dropped.length > 0) onFiles(dropped);
            }}
          >
            <strong>{t('send.dropzone.title')}</strong>
            <small>{t('send.dropzone.hint')}</small>
            <input type="file" multiple readOnly />
          </div>

          <FileList paths={files} onRemove={(path) => onFiles(files.filter((item) => item !== path))} />

          <div className="toolbar-row">
            <button type="button" className="ghost" onClick={() => void pick(false)}>
              <FileArchive />
              {t('send.pick')}
            </button>
            <button type="button" className="ghost" onClick={() => void pick(true)}>
              <FolderOpen />
              {t('send.pickDir')}
            </button>
          </div>
        </>
      ) : (
        <textarea
          rows={6}
          value={text}
          placeholder={t('send.textPlaceholder')}
          onChange={(event) => onText(event.target.value)}
        />
      )}

      <span className="field-label">{t('send.code.custom')}</span>
      <input
        type="text"
        value={customCode}
        placeholder={t('send.code.customPlaceholder')}
        onChange={(event) => onCustomCode(event.target.value)}
      />

      <SendCode code={code} />

      <div className="divider" />
      <button type="button" onClick={onStart} disabled={busy || !somethingToSend}>
        <Upload />
        {busy ? t('send.starting') : t('send.start')}
      </button>
      {settings.noCompress && (
        <p className="note">
          {t('settings.noCompress')} · {t('settings.relay')}: {settings.relay || t('settings.relayPlaceholder')}
        </p>
      )}
    </section>
  );
}
