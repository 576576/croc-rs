import { useT } from '../i18n';
import { api, type StoredResult } from '../bridge';
import { CloudUpload, File, FolderOpen, MessageSquareText, Share2, Upload } from 'lucide-react';
import { useEffect, useMemo, useRef, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { chooseFiles, FileList } from './FileList';
import { CodeField } from './CodeField';
import { Disclosure } from './Disclosure';
import { QRCodeSVG } from 'qrcode.react';
import { formatBytes } from '../format';
import { isAndroid } from '../platform';
import type { Settings } from './SettingsGrid';

/// 浏览器端的加入链接。拼法与 CLI 的二维码一致（`crates/croc-cli/src/qr.rs`），
/// 换成别的地址会让"扫码→在手机上打开收件页"这条路断掉。
export function joinUrl(code: string): string {
  return `https://getcroc.com/?code=${encodeURIComponent(code)}`;
}

/// 链接摆到界面上时去掉 `https://`：这一串是给念、给对的，协议头只是噪声。
/// 复制与分享出去的仍是 [`joinUrl`] 那份完整的。
export function joinUrlLabel(code: string): string {
  return joinUrl(code).replace(/^https:\/\//, '');
}

export type ContentMode = 'files' | 'folder' | 'text';
/// 投递方式：直传（口令码）或暂存（上传换一条链接）。
export type DeliveryMode = 'direct' | 'stored';

/// 期限单位，后端 `parse_expiration` 认这四个，拼成 `90m` / `12h` / `3d` / `2w`。
const EXPIRATION_UNITS = ['m', 'h', 'd', 'w'] as const;
type ExpirationUnit = (typeof EXPIRATION_UNITS)[number];
/// 与 CLI 的 `--expiration` 默认值一致。
const DEFAULT_EXPIRATION: { value: number; unit: ExpirationUnit } = { value: 1, unit: 'd' };

/// 文本载荷的上限，croc web 的 `maxTextTransferBytes` 与 Go 的 `validateSendingTextOffer` 都是这个数。
const MAX_TEXT_BYTES = 1024 * 1024;

/// Tauri 的 drop 事件给的是路径数组；Web 的 `File` 对象只有名字，拿不到路径。
/// 这条直流量只走前者，所以桌面端与 Android 行为一致。
export function SendPanel({
  id,
  active,
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
  onResetCode,
  onStart,
}: {
  /// 窄屏切换条的 `aria-controls` 指向它。
  id: string;
  /// 窄屏下这一块是否被选中：没选中的整块由样式收掉（宽屏两块并排，不受影响）。
  active: boolean;
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
  /// 切换"发送前重置口令码"。
  onResetCode: (next: boolean) => void;
  onStart: () => void;
}) {
  const t = useT();
  const zone = useRef<HTMLDivElement>(null);
  const [dragging, setDragging] = useState(false);
  /// 拖放事件给的坐标是物理像素，DOM 量出来的是 CSS 像素：换算要靠窗口的缩放比。
  const scale = useRef(1);
  // 暂存的三个输入自己带着：只有这个分支用得到，放到 App 只会让那边的状态更长。
  const [expirationValue, setExpirationValue] = useState(DEFAULT_EXPIRATION.value);
  const [expirationUnit, setExpirationUnit] = useState<ExpirationUnit>(DEFAULT_EXPIRATION.unit);
  const [downloads, setDownloads] = useState(1);
  const [stored, setStored] = useState<StoredResult | null>(null);
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

  /// 暂存只收文件（croc web 切到暂存时也把内容拨回文件）：留着"目录"会去开目录选择器，
  /// 留着"文本"则那一段没有落地的地方。
  const chooseDelivery = (next: DeliveryMode) => {
    if (next === 'stored') onMode('files');
    onDelivery(next);
  };

  /// 分享接收链接：桌面端复制到剪贴板，Android 起系统分享面板。
  ///
  /// 两边的"分享"含义不同——桌面上没有分享面板，把链接放进剪贴板才是能接着用的动作。
  /// 失败不冒泡：这一格是顺手拿链接的地方，出错打断不了什么，与口令码栏那枚按钮一致。
  const shareReceiveLink = (value: string) => {
    const url = joinUrl(value);
    void (isAndroid ? api.shareText(url) : api.copy(url)).catch(() => undefined);
  };

  const startUpload = async () => {
    setUploading(true);
    try {
      setStored(
        await api.storeUpload({
          paths: files,
          expiration: `${expirationValue}${expirationUnit}`,
          downloads,
        }),
      );
    } finally {
      setUploading(false);
    }
  };

  /// 载荷长度按 UTF-8 算：上限是字节数，不是字符数，中文一个字三字节。
  const textBytes = useMemo(() => new Blob([text]).size, [text]);
  const tooLarge = textBytes > MAX_TEXT_BYTES;
  const somethingToSend = mode === 'text' ? text.trim() !== '' && !tooLarge : files.length > 0;

  return (
    <section id={id} className={active ? 'panel mobile-active' : 'panel'} data-tour="send">
      <div className="panel-heading">
        <span className="step">
          <Upload aria-hidden="true" />
        </span>
        <h2>{t('send.start')}</h2>
        {/* 发起按钮放在标题右边：页脚只留一句状态提示，整块的重心因此提到顶部。 */}
        <div className="panel-heading-action">
          {delivery === 'stored' ? (
            <button
              type="button"
              className="primary-button"
              onClick={() => void startUpload()}
              disabled={uploading || files.length === 0}
            >
              <CloudUpload aria-hidden="true" />
              {uploading ? t('store.starting') : t('store.start')}
            </button>
          ) : (
            <button type="button" className="primary-button" onClick={onStart} disabled={busy || !somethingToSend}>
              <Upload aria-hidden="true" />
              {busy ? t('send.starting') : t('send.start')}
            </button>
          )}
        </div>
      </div>

      {/* 口令码那一格与 SSH 页的邀请码同位置（紧跟标题），框后面那枚按钮长按切换
          复制/粘贴。暂存没有口令码，那一格整个不出现。 */}
      {delivery === 'direct' && (
        <>
          <span className="field-label">{t('send.code')}</span>
          <CodeField
            value={customCode}
            placeholder={t('send.code.customPlaceholder')}
            onValue={onCustomCode}
          />
          {code && (
            <Disclosure label={t('send.qr')} open>
              <div className="share-qr">
                <QRCodeSVG value={joinUrl(code)} size={132} bgColor="transparent" fgColor="currentColor" />
                <p className="share-link-url">{joinUrlLabel(code)}</p>
                <button type="button" className="ghost" onClick={() => shareReceiveLink(code)}>
                  <Share2 aria-hidden="true" />
                  {t('send.shareLink')}
                </button>
              </div>
            </Disclosure>
          )}
          <label className="checkbox-label">
            <input
              type="checkbox"
              checked={settings.resetCode}
              onChange={(event) => onResetCode(event.target.checked)}
            />
            <span>{t('settings.resetCode')}</span>
          </label>
        </>
      )}

      <span className="field-label">{t('send.mode')}</span>
      {/* 两组开关合成一块：第二组去掉上边，与上面那组共用中间那条线（见 styles.css）。
          暂存下没有内容三选，只剩一组，这一格不上 `two`——那会儿第一组的下边就是外框。 */}
      <div className={delivery === 'direct' ? 'mode-stack two' : 'mode-stack'}>
        <div className="workspace-switch two" role="tablist" aria-label={t('send.mode')}>
          <button type="button" role="tab" aria-selected={delivery === 'direct'} onClick={() => chooseDelivery('direct')}>
            <Upload aria-hidden="true" />
            {t('send.mode.direct')}
          </button>
          <button type="button" role="tab" aria-selected={delivery === 'stored'} onClick={() => chooseDelivery('stored')}>
            <CloudUpload aria-hidden="true" />
            {t('send.mode.stored')}
          </button>
        </div>

        {delivery === 'direct' && (
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
        )}
      </div>

      {delivery === 'stored' || mode !== 'text' ? (
        <>
          {/* 拖放只在拖进这块区域里才算：事件是窗口级的，判定在 `useEffect` 里做。 */}
          <div className="drop-zone" ref={zone} data-dragging={dragging} onClick={() => void pick()}>
            <strong>{mode === 'folder' ? t('send.dropzone.folder') : t('send.dropzone.title')}</strong>
            <small>{t('send.dropzone.hint')}</small>
          </div>

          <FileList
            paths={files}
            folder={mode === 'folder'}
            onRemove={(path) => onFiles(files.filter((item) => item !== path))}
          />
        </>
      ) : (
        <div className="text-composer">
          <label className="field-label" htmlFor="send-text">
            {t('send.textLabel')}
          </label>
          <textarea
            id="send-text"
            value={text}
            placeholder={t('send.textPlaceholder')}
            onChange={(event) => onText(event.target.value)}
          />
          <div className="text-composer-meta">
            <span className={tooLarge ? 'error' : undefined} role={tooLarge ? 'alert' : undefined}>
              {formatBytes(textBytes)} / 1 MiB{tooLarge ? t('send.textTooLarge') : ''}
            </span>
          </div>
        </div>
      )}

      {delivery === 'stored' ? (
        <>
          {/* 期限与次数照 croc web：期限是"数值 + 单位"两件一组，次数单独一行。 */}
          <span className="field-label">{t('store.expiration')}</span>
          <div className="stored-expiration-control">
            <input
              type="number"
              min={1}
              step={1}
              value={expirationValue}
              aria-label={t('store.expiration')}
              onChange={(event) => setExpirationValue(Math.max(1, Math.trunc(Number(event.target.value) || 1)))}
            />
            <select
              value={expirationUnit}
              aria-label={t('store.expirationUnit')}
              onChange={(event) => setExpirationUnit(event.target.value as ExpirationUnit)}
            >
              {EXPIRATION_UNITS.map((unit) => (
                <option key={unit} value={unit}>
                  {t(`store.unit.${unit}`)}
                </option>
              ))}
            </select>
          </div>

          <label className="field-label" htmlFor="stored-downloads">
            {t('store.downloads')}
          </label>
          <input
            id="stored-downloads"
            type="number"
            min={1}
            step={1}
            value={downloads}
            onChange={(event) => setDownloads(Math.max(1, Math.trunc(Number(event.target.value) || 1)))}
          />

          {stored && (
            <>
              <div className="divider" />
              <span className="field-label">{t('store.link')}</span>
              <CodeField value={stored.link} copyLabel={t('store.copy')} readOnly />
              <Disclosure label={t('send.qr')} open>
                <div className="share-qr">
                  <QRCodeSVG value={stored.link} size={132} bgColor="transparent" fgColor="currentColor" />
                </div>
              </Disclosure>
              <p className="note">
                {t('store.expires')}: {stored.expiresAt} · {t('store.downloads')}: {stored.downloads}
              </p>
              <p className="note">{t('store.done')}</p>
            </>
          )}
        </>
      ) : (
        settings.noCompress && (
          <p className="note">
            {t('settings.noCompress')} · {t('settings.relay')}: {settings.relay || t('settings.relayPlaceholder')}
          </p>
        )
      )}
    </section>
  );
}
