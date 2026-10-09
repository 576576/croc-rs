import { useT } from '../i18n';
import type { OfferPayload } from '../bridge';
import { formatBytes } from '../format';

/// 清单确认卡：握手完成、清单到手之后才出现，收不收由用户在这一栏里定。
export function OfferCard({ offer, onRespond }: { offer: OfferPayload; onRespond: (accept: boolean) => void }) {
  const t = useT();
  return (
    <div className="offer">
      <strong>{t('offer.title')}</strong>
      <p>
        {t('offer.files', { count: offer.files.length })} · {t('offer.size')} {formatBytes(offer.total)} ·{' '}
        {t('offer.folders')} {offer.folders}
      </p>
      <ul className="file-list">
        {offer.files.map((file) => (
          <li key={file.name} title={file.name}>
            <span aria-hidden="true">·</span>
            <span>{file.name}</span>
            <span>{formatBytes(file.size)}</span>
          </li>
        ))}
      </ul>
      <div className="offer-actions">
        <button type="button" onClick={() => onRespond(true)}>
          {t('offer.accept')}
        </button>
        <button type="button" className="ghost" onClick={() => onRespond(false)}>
          {t('offer.decline')}
        </button>
      </div>
    </div>
  );
}
