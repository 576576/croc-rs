import { Clock, Trash2 } from 'lucide-react';
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
  onClear,
}: {
  entries: HistoryEntry[];
  onClear: () => void;
}) {
  const t = useT();

  return (
    <section className="panel" data-tour="history">
      <div className="panel-heading">
        <span className="step">
          <Clock aria-hidden="true" />
        </span>
        <div>
          <h2>{t('history.title')}</h2>
        </div>
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
            </li>
          ))}
        </ul>
      )}

      {entries.length > 0 && (
        <>
          <div className="divider" />
          <button type="button" className="ghost" onClick={onClear}>
            <Trash2 aria-hidden="true" /> {t('history.clear')}
          </button>
        </>
      )}
    </section>
  );
}
