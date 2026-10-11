import { Clock, FolderOutput, Share2, Trash2 } from 'lucide-react';
import { useT } from '../i18n';
import { formatBytes } from '../format';

/// 一条收工的记录。
export interface HistoryEntry {
  id: string;
  direction: 'send' | 'receive';
  /// 传输显示用的名字：发送是清单里的文件名，接收是最后收完的那个。
  name: string;
  bytes: number;
  /// 收工时刻（`Date.now()`）。
  at: number;
  ok: boolean;
  error: string | null;
}

const STORAGE_KEY = 'croc-gui-history';
/// 只留最近这些条：历史页是给人看的，不是审计日志。
const LIMIT = 200;

export function loadHistory(): HistoryEntry[] {
  try {
    const stored = localStorage.getItem(STORAGE_KEY);
    if (!stored) return [];
    const parsed = JSON.parse(stored);
    return Array.isArray(parsed) ? (parsed as HistoryEntry[]) : [];
  } catch {
    return [];
  }
}

export function saveHistory(entries: HistoryEntry[]): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(entries.slice(0, LIMIT)));
  } catch {
    // 存不下不影响当前会话。
  }
}

export function HistoryPanel({
  entries,
  isAndroid,
  resolvePath,
  onSave,
  onShare,
  onClear,
}: {
  entries: HistoryEntry[];
  /// 另存与分享只在 Android 上给出：桌面端文件本来就摆在用户挑的地方。
  isAndroid: boolean;
  /// 条目里只有文件名，落盘路径由外层拼（它才知道下载目录）。
  resolvePath: (name: string) => string;
  onSave: (path: string) => void;
  onShare: (path: string) => void;
  onClear: () => void;
}) {
  const t = useT();

  return (
    <section className="panel" data-tour="history">
      <div className="panel-heading">
        <span className="step">
          <Clock aria-hidden="true" />
        </span>
        <h2>{t('history.title')}</h2>
        {/* 清空只在有条目时出现：空列表上摆一个没用的按钮只是噪声。 */}
        {entries.length > 0 && (
          <div className="panel-heading-action">
            <button type="button" className="ghost" onClick={onClear}>
              <Trash2 aria-hidden="true" /> {t('history.clear')}
            </button>
          </div>
        )}
      </div>

      {entries.length === 0 ? (
        <p className="note">{t('history.empty')}</p>
      ) : (
        <ul className="queue">
          {entries.map((entry) => (
            <li key={entry.id}>
              <div className="queue-head">
                <span className="queue-name" title={entry.name}>
                  {entry.direction === 'send' ? '↑' : '↓'} {entry.name}
                </span>
                <span className="queue-meta">{formatBytes(entry.bytes)}</span>
              </div>
              <p className={entry.ok ? 'note' : 'note danger'}>
                {entry.ok ? t(entry.direction === 'send' ? 'history.sent' : 'history.received') : t('history.failed')}
                {' · '}
                {new Date(entry.at).toLocaleString()}
              </p>
              {!entry.ok && entry.error && <p className="note danger">{entry.error}</p>}
              {/* 收工之后还想另存或分享：队列取消之后这两个动作只能挂在记录上。 */}
              {isAndroid && entry.ok && entry.direction === 'receive' && (
                <div className="offer-actions">
                  <button type="button" className="ghost" onClick={() => onSave(resolvePath(entry.name))}>
                    <FolderOutput aria-hidden="true" />
                    {t('transfers.save')}
                  </button>
                  <button type="button" className="ghost" onClick={() => onShare(resolvePath(entry.name))}>
                    <Share2 aria-hidden="true" />
                    {t('transfers.share')}
                  </button>
                </div>
              )}
            </li>
          ))}
        </ul>
      )}

    </section>
  );
}
