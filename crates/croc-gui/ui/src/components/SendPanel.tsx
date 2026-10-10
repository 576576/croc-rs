import { useT } from '../i18n';
import { api, type StoredResult } from '../bridge';
import { Check, CloudUpload, Copy, FileArchive, FolderOpen, Upload } from 'lucide-react';
import { useState } from 'react';
import { chooseFiles, FileList } from './FileList';
import { SendCode } from './SendCode';
import { QRCodeSVG } from 'qrcode.react';
import type { Settings } from './SettingsGrid';

export type ContentMode = 'files' | 'text';
/// 投递方式：直传（口令码）或暂存（上传换一条链接）。
export type DeliveryMode = 'direct' | 'stored';

/// Tauri 的 drop 事件给的是路径数组；Web 的 `File` 对象只有名字，拿不到路径。
/// 这条直流量只走前者，所以桌面端与 Android 行为一致。
export function SendPanel({
  delivery,
  mode,
  files,
  text,
  busy,
  code,
  settings,
  customCode,
  onDelivery,
  onMode,
  onFiles,
  onText,
  onCustomCode,
  onStart,
}: {
  delivery: DeliveryMode;
  mode: ContentMode;
  files: string[];
  text: string;
  busy: boolean;
  code: string | null;
  settings: Settings;
  customCode: string;
  onDelivery: (next: DeliveryMode) => void;
  onMode: (next: ContentMode) => void;
  onFiles: (next: string[]) => void;
  onText: (next: string) => void;
  onCustomCode: (next: string) => void;
  onStart: () => void;
}) {
  const t = useT();
  const [dragging, setDragging] = useState(false);
  // 暂存的三个输入自己带着：只有这个分支用得到，放到 App 只会让那边的状态更长。
  const [expiration, setExpiration] = useState('');
  const [downloads, setDownloads] = useState(1);
  const [stored, setStored] = useState<StoredResult | null>(null);
  const [copied, setCopied] = useState(false);
  const [uploading, setUploading] = useState(false);

  const pick = async (directory: boolean) => {
    const picked = await chooseFiles(true, directory);
    if (picked.length > 0) onFiles(picked);
  };

  const startUpload = async () => {
    setUploading(true);
    try {
      setStored(
        await api.storeUpload({
          paths: files,
          expiration: expiration.trim() === '' ? undefined : expiration.trim(),
          downloads,
        }),
      );
    } finally {
      setUploading(false);
    }
  };

  const copyLink = () => {
    if (!stored) return;
    void api
      .copy(stored.link)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => undefined);
  };

  const somethingToSend = mode === 'files' ? files.length > 0 : text.trim() !== '';

  return (
    <section className="panel" data-tour="send">
      <div className="panel-heading">
        <span className="step">
          <Upload aria-hidden="true" />
        </span>
        <div>
          <h2>{t('send.start')}</h2>
          <p>{delivery === 'stored' ? t('store.intro') : t('send.intro')}</p>
        </div>
      </div>

      <span className="field-label">{t('send.mode')}</span>
      <div className="workspace-switch two" role="tablist" aria-label={t('send.mode')}>
        <button type="button" role="tab" aria-selected={delivery === 'direct'} onClick={() => onDelivery('direct')}>
          <Upload aria-hidden="true" />
          {t('send.mode.direct')}
        </button>
        <button type="button" role="tab" aria-selected={delivery === 'stored'} onClick={() => onDelivery('stored')}>
          <CloudUpload aria-hidden="true" />
          {t('send.mode.stored')}
        </button>
      </div>

      {delivery === 'direct' && (
        <>
          <span className="field-label">{t('send.content')}</span>
          <div className="workspace-switch two" role="tablist" aria-label={t('send.content')}>
            <button type="button" role="tab" aria-selected={mode === 'files'} onClick={() => onMode('files')}>
              <FileArchive aria-hidden="true" />
              {t('send.content.files')}
            </button>
            <button type="button" role="tab" aria-selected={mode === 'text'} onClick={() => onMode('text')}>
              <Upload aria-hidden="true" />
              {t('send.content.text')}
            </button>
          </div>
        </>
      )}

      {delivery === 'stored' || mode === 'files' ? (
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
              <FileArchive aria-hidden="true" />
              {t('send.pick')}
            </button>
            <button type="button" className="ghost" onClick={() => void pick(true)}>
              <FolderOpen aria-hidden="true" />
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

      {delivery === 'stored' ? (
        <>
          <div className="settings-grid">
            <label>
              <span>{t('store.expiration')}</span>
              <input
                type="text"
                value={expiration}
                placeholder={t('store.expirationPlaceholder')}
                onChange={(event) => setExpiration(event.target.value)}
              />
            </label>
            <label>
              <span>{t('store.downloads')}</span>
              <input
                type="text"
                inputMode="numeric"
                value={String(downloads)}
                onChange={(event) => setDownloads(Number(event.target.value.replace(/[^0-9]/g, '')))}
              />
            </label>
          </div>

          <div className="divider" />
          <button type="button" onClick={() => void startUpload()} disabled={uploading || files.length === 0}>
            <CloudUpload aria-hidden="true" />
            {uploading ? t('store.starting') : t('store.start')}
          </button>

          {stored && (
            <>
              <span className="field-label">{t('store.link')}</span>
              <div className="send-code">
                <strong title={stored.link}>{stored.link}</strong>
                <span />
                <button type="button" className="ghost" onClick={copyLink} aria-label={t('store.copied')}>
                  {copied ? <Check aria-hidden="true" /> : <Copy aria-hidden="true" />}
                </button>
              </div>
              <details>
                <summary className="field-label">{t('send.qr')}</summary>
                <div className="share-qr">
                  <QRCodeSVG value={stored.link} size={132} bgColor="transparent" fgColor="currentColor" />
                </div>
              </details>
              <p className="note">
                {t('store.expires')}: {stored.expiresAt} · {t('store.downloads')}: {stored.downloads}
              </p>
              <p className="note">{t('store.done')}</p>
            </>
          )}
        </>
      ) : (
        <>
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
            <Upload aria-hidden="true" />
            {busy ? t('send.starting') : t('send.start')}
          </button>
          {settings.noCompress && (
            <p className="note">
              {t('settings.noCompress')} · {t('settings.relay')}: {settings.relay || t('settings.relayPlaceholder')}
            </p>
          )}
        </>
      )}
    </section>
  );
}
