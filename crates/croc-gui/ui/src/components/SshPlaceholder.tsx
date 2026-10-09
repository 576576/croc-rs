import { useT } from '../i18n';
import { Terminal } from 'lucide-react';

/// SSH 共享终端的占位（计划 §G11）。
///
/// 刻意做成一块看得见的面板而不是把它藏掉：清单上有这一项，缺的是数据面（`croc ssh` 的
/// Rust 实现），把"没做"写进界面比让它凭空消失好。
export function SshPlaceholder() {
  const t = useT();
  return (
    <section className="panel">
      <span className="field-label">{t('ssh.title')}</span>
      <div className="placeholder">
        <Terminal />
        <p className="note">{t('ssh.pending')}</p>
        <p className="queue-meta">{t('ssh.docs')}</p>
      </div>
      <div className="divider" />
      <button type="button" disabled>
        {t('common.notImplemented')}
      </button>
    </section>
  );
}
