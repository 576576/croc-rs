import { useT } from '../i18n';
import type { TransferView } from '../types';
import { baseName, formatBytes, formatEta, formatRate, percent, project } from '../format';
import { X } from 'lucide-react';

/// 标题右侧那一格：当前传输的进度条与一句提示。
///
/// 取代原先的传输队列——队列整块占一页，而"正在传什么"是随时要瞟一眼的东西。
/// 同一时刻只显示一条（最先开始的那条），因为横向空位只放得下一行。
export function ProgressStrip({
  transfers,
  onCancel,
}: {
  transfers: TransferView[];
  onCancel: (id: string) => void;
}) {
  const t = useT();
  const active = transfers.find((item) => item.active);
  if (!active) return null;

  const projection = project(active.samples, active.transferred, active.total);
  const name = active.file ? baseName(active.file) : active.id;
  const done = percent(active.transferred, active.total);
  // 百分比、字节与速率都在这一行里，条本身只作视觉指示，不叠数字上去。
  const detail = [
    `${Math.round(done)}%`,
    `${formatBytes(active.transferred)} / ${formatBytes(active.total)}`,
    formatRate(projection.rate),
    formatEta(projection.eta),
  ];

  return (
    <div className="progress-strip">
      <div className="progress-strip-head">
        <span className="progress-strip-name" title={active.file ?? active.id}>
          {active.direction === 'send' ? '↑' : '↓'} {name}
        </span>
        <button
          type="button"
          className="ghost progress-strip-cancel"
          onClick={() => onCancel(active.id)}
          aria-label={t('transfers.cancel')}
          title={t('transfers.cancel')}
        >
          <X aria-hidden="true" />
        </button>
      </div>
      <div className="progress-track">
        <span style={{ width: `${done}%` }} />
      </div>
      <p className="progress-strip-detail">{detail.join(' · ')}</p>
    </div>
  );
}
