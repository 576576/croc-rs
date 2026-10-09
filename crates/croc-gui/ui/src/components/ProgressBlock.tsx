import { useT } from '../i18n';
import { formatEta, formatRate, percent } from '../format';

export function ProgressBlock({
  transferred,
  total,
  rate,
  eta,
  active,
}: {
  transferred: number;
  total: number;
  rate: number;
  eta: number;
  active: boolean;
}) {
  const t = useT();
  return (
    <div>
      <div className="progress-track">
        <span style={{ width: `${percent(transferred, total)}%` }} />
      </div>
      <div className="progress-metrics">
        <div>
          <span>{t('progress.rate')}</span>
          <strong>{active ? formatRate(rate) : '—'}</strong>
        </div>
        <div>
          <span>{t('progress.eta')}</span>
          <strong>{active ? formatEta(eta) : '—'}</strong>
        </div>
      </div>
    </div>
  );
}
