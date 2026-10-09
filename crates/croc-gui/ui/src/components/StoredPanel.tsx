import { useT } from '../i18n';
import { api, type StoredResult } from '../bridge';
import { FileArchive, CloudUpload, Copy, Check } from 'lucide-react';
import { useState } from 'react';
import { chooseFiles, FileList } from './FileList';
import { QRCodeSVG } from 'qrcode.react';

/// 存储模式（G7）：上传到暂存服务，拿一条链接代替口令码。
/// 有效期与下载次数的语法跟 CLI 的 `croc store` 完全一样（`croc_store::parse_expiration`）。
export function StoredPanel({
  files,
  busy,
  expiration,
  downloads,
  result,
  onFiles,
  onExpiration,
  onDownloads,
  onStart,
}: {
  files: string[];
  busy: boolean;
  expiration: string;
  downloads: number;
  result: StoredResult | null;
  onFiles: (next: string[]) => void;
  onExpiration: (next: string) => void;
  onDownloads: (next: number) => void;
  onStart: () => void;
}) {
  const t = useT();
  const [copied, setCopied] = useState(false);

  const copy = () => {
    if (!result) return;
    void api
      .copy(result.link)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => undefined);
  };

  return (
    <section className="panel">
      <p className="note">{t('store.intro')}</p>

      <span className="field-label">{t('store.files')}</span>
      <FileList paths={files} onRemove={(path) => onFiles(files.filter((item) => item !== path))} />
      <div className="toolbar-row">
        <button
          type="button"
          className="ghost"
          onClick={() => void chooseFiles(true, false).then((picked) => picked.length > 0 && onFiles(picked))}
        >
          <FileArchive />
          {t('send.pick')}
        </button>
      </div>

      <div className="settings-grid">
        <label>
          <span>{t('store.expiration')}</span>
          <input
            type="text"
            value={expiration}
            placeholder={t('store.expirationPlaceholder')}
            onChange={(event) => onExpiration(event.target.value)}
          />
        </label>
        <label>
          <span>{t('store.downloads')}</span>
          <input
            type="text"
            inputMode="numeric"
            value={String(downloads)}
            onChange={(event) => onDownloads(Number(event.target.value.replace(/[^0-9]/g, '')))}
          />
        </label>
      </div>

      <div className="divider" />
      <button type="button" onClick={onStart} disabled={busy || files.length === 0}>
        <CloudUpload />
        {busy ? t('store.starting') : t('store.start')}
      </button>

      {result && (
        <>
          <span className="field-label">{t('store.link')}</span>
          <div className="send-code">
            <strong title={result.link}>{result.link}</strong>
            <span />
            <button type="button" className="ghost" onClick={copy} aria-label={t('store.copied')}>
              {copied ? <Check /> : <Copy />}
            </button>
          </div>
          <details>
            <summary className="field-label">{t('send.qr')}</summary>
            <div className="share-qr">
              <QRCodeSVG value={result.link} size={132} bgColor="transparent" fgColor="currentColor" />
            </div>
          </details>
          <p className="note">
            {t('store.expires')}: {result.expiresAt} · {t('store.downloads')}: {result.downloads}
          </p>
          <p className="note">{t('store.done')}</p>
        </>
      )}
    </section>
  );
}
