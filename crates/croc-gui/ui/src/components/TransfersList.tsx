import { useT } from '../i18n';
import type { TransferView } from '../types';
import { baseName, percent, project } from '../format';
import { ProgressBlock } from './ProgressBlock';
import { OfferCard } from './OfferCard';
import { X, Share2, FolderOutput } from 'lucide-react';

export function TransfersList({
  transfers,
  isAndroid,
  resolvePath,
  onCancel,
  onRespond,
  onSave,
  onShare,
}: {
  transfers: TransferView[];
  /// 另存与分享只在 Android 上给出桌面端会失败。
  isAndroid: boolean;
  /// 界面只有文件名，落盘路径由外层拼（它才知道下载目录）。
  resolvePath: (name: string) => string;
  onCancel: (id: string) => void;
  onRespond: (id: string, accept: boolean) => void;
  onSave: (path: string) => void;
  onShare: (path: string) => void;
}) {
  const t = useT();

  if (transfers.length === 0) {
    return <p className="note">{t('transfers.empty')}</p>;
  }

  return (
    <ul className="queue">
      {transfers.map((transfer) => {
        const projection = project(transfer.samples, transfer.transferred, transfer.total);
        const title = transfer.file ? baseName(transfer.file) : transfer.id;
        const finishedFile = !transfer.active && transfer.ok === true && transfer.file ? transfer.file : null;
        return (
          <li key={transfer.id}>
            <div className="queue-head">
              <span className="queue-name" title={transfer.file ?? transfer.id}>
                {transfer.direction === 'send' ? '↑' : '↓'} {title}
              </span>
              <span className="queue-meta">{Math.round(percent(transfer.transferred, transfer.total))}%</span>
              {transfer.active && (
                <button type="button" className="ghost" onClick={() => onCancel(transfer.id)} aria-label={t('transfers.cancel')}>
                  <X />
                </button>
              )}
            </div>
            {transfer.offer && <OfferCard offer={transfer.offer} onRespond={(accept) => onRespond(transfer.id, accept)} />}
            <ProgressBlock
              transferred={transfer.transferred}
              total={transfer.total}
              rate={projection.rate}
              eta={projection.eta}
              active={transfer.active}
            />
            <p className={transfer.ok === false ? 'note danger' : 'note'}>
              {transfer.summary ?? transfer.error ?? transfer.status}
            </p>
            {isAndroid && finishedFile && (
              <div className="offer-actions">
                <button type="button" className="ghost" onClick={() => onSave(resolvePath(finishedFile))}>
                  <FolderOutput />
                  {t('transfers.save')}
                </button>
                <button type="button" className="ghost" onClick={() => onShare(resolvePath(finishedFile))}>
                  <Share2 />
                  {t('transfers.share')}
                </button>
              </div>
            )}
          </li>
        );
      })}
    </ul>
  );
}
