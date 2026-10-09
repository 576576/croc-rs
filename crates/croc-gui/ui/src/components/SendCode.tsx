import { useT } from '../i18n';
import { QRCodeSVG } from 'qrcode.react';
import { Copy, Check, ExternalLink } from 'lucide-react';
import { useCallback, useState } from 'react';
import { api } from '../bridge';
import { openUrl } from '@tauri-apps/plugin-opener';

/// 浏览器端的加入链接。拼法与 CLI 的二维码一致（`crates/croc-cli/src/qr.rs`），
/// 换成别的地址会让"扫码→在手机上打开收件页"这条路断掉。
export function joinUrl(code: string): string {
  return `https://getcroc.com/?code=${encodeURIComponent(code)}`;
}

export function SendCode({ code }: { code: string | null }) {
  const t = useT();
  const [copied, setCopied] = useState(false);

  const onCopy = useCallback(() => {
    if (!code) return;
    void api
      .copy(code)
      .then(() => {
        setCopied(true);
        window.setTimeout(() => setCopied(false), 1200);
      })
      .catch(() => undefined);
  }, [code]);

  if (!code) return null;

  return (
    <>
      <span className="field-label">{t('send.code')}</span>
      <div className="send-code">
        <strong>{code}</strong>
        <span />
        <button type="button" className="ghost" onClick={onCopy} aria-label={t('send.copied')}>
          {copied ? <Check /> : <Copy />}
        </button>
      </div>
      <details>
        <summary className="field-label">{t('send.qr')}</summary>
        <div className="share-qr">
          <QRCodeSVG value={joinUrl(code)} size={132} bgColor="transparent" fgColor="currentColor" />
          <button type="button" className="ghost" onClick={() => void openUrl(joinUrl(code))}>
            <ExternalLink />
            {t('send.openInBrowser')}
          </button>
        </div>
      </details>
    </>
  );
}
