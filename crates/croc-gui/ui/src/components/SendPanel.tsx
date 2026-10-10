import { useT } from '../i18n';
import { api, type StoredResult } from '../bridge';
import { Check, CloudUpload, Copy, File, FolderOpen, MessageSquareText, Upload } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { chooseFiles, FileList } from './FileList';
import { SendCode } from './SendCode';
import { QRCodeSVG } from 'qrcode.react';
import type { Settings } from './SettingsGrid';

export type ContentMode = 'files' | 'folder' | 'text';
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
  const zone = useRef<HTMLDivElement>(null);
  const [dragging, setDragging] = useState(false);
  /// 拖放事件给的坐标是物理像素，DOM 量出来的是 CSS 像素：换算要靠窗口的缩放比。
  const scale = useRef(1);
  // 暂存的三个输入自己带着：只有这个分支用得到，放到 App 只会让那边的状态更长。
  const [expiration, setExpiration] = useState('');
  const [downloads, setDownloads] = useState(1);
  const [stored, setStored] = useState<StoredResult | null>(null);
  const [copied, setCopied] = useState(false);
  const [uploading, setUploading] = useState(false);

  // 拖放只在拖到这块区域里才算：事件本身是给整个窗口的，落在别处（比如设置区）
  // 也照收会把用户随手一拖变成一次发送列表的改动。
  useEffect(() => {
    const window = getCurrentWindow();
    let unlisten: (() => void) | undefined;
    void window
      .scaleFactor()
      .then((factor) => {
        scale.current = factor;
      })
      .catch(() => undefined);

    const inside = (x: number, y: number) => {
      const rect = zone.current?.getBoundingClientRect();
      if (!rect) return false;
      return (
        x / scale.current >= rect.left &&
        x / scale.current <= rect.right &&
        y / scale.current >= rect.top &&
        y / scale.current <= rect.bottom
      );
    };

    void window
      .onDragDropEvent((event) => {
        const payload = event.payload;
        if (payload.type === 'leave') {
          setDragging(false);
          return;
        }
        const over = inside(payload.position.x, payload.position.y);
        if (payload.type === 'drop') {
          setDragging(false);
          if (over && payload.paths.length > 0) onFiles(payload.paths);
          return;
        }
        setDragging(over);
      })
      .then((fn) => {
        unlisten = fn;
      })
      .catch(() => undefined);

    return () => unlisten?.();
  }, [onFiles]);

  const pick = async () => {
    const picked = await chooseFiles(mode !== 'folder', mode === 'folder');
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

  const somethingToSend = mode === 'text' ? text.trim() !== '' : files.length > 0;

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
          <div className="workspace-switch three" role="tablist" aria-label={t('send.content')}>
            <button type="button" role="tab" aria-selected={mode === 'files'} onClick={() => onMode('files')}>
              <File aria-hidden="true" />
              {t('send.content.files')}
            </button>
            <button type="button" role="tab" aria-selected={mode === 'folder'} onClick={() => onMode('folder')}>
              <FolderOpen aria-hidden="true" />
              {t('send.content.folder')}
            </button>
            <button type="button" role="tab" aria-selected={mode === 'text'} onClick={() => onMode('text')}>
              <MessageSquareText aria-hidden="true" />
              {t('send.content.text')}
            </button>
          </div>
        </>
      )}

      {delivery === 'stored' || mode !== 'text' ? (
        <>
          {/* 拖放只在拖进这块区域里才算：事件是窗口级的，判定在 `useEffect` 里做。 */}
          <div className="drop-zone" ref={zone} data-dragging={dragging} onClick={() => void pick()}>
            <strong>{mode === 'folder' ? t('send.dropzone.folder') : t('send.dropzone.title')}</strong>
            <small>{t('send.dropzone.hint')}</small>
          </div>

          <FileList paths={files} onRemove={(path) => onFiles(files.filter((item) => item !== path))} />
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
