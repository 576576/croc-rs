import { useState } from 'react';
import { Eye, EyeOff, LogIn, ShieldAlert, SquareTerminal } from 'lucide-react';
import { useT } from '../i18n';

/// 未实现时的去处，写在终端区里。只是文字，不做跳转。
const SSH_URL = 'getcroc.com/#ssh';

/// 共享终端（计划 §G11）。
///
/// 面板照 croc web 的 `SSHPanel` 搭齐：邀请码、加入按钮、状态行、终端区、安全提示。
/// 本版本没有实现，所以加入按钮是禁用的，终端区常显一句说明——把"为什么点不动"写在
/// 界面上，比让它看起来能点更诚实。
export function SshPanel() {
  const t = useT();
  const [code, setCode] = useState('');
  const [showCode, setShowCode] = useState(false);

  return (
    <section className="ssh-workspace" data-tour="ssh" aria-label={t('ssh.title')}>
      <article className="panel ssh-panel">
        <div className="panel-heading ssh-heading">
          <span className="step">
            <SquareTerminal aria-hidden="true" />
          </span>
          <div>
            <h2>{t('ssh.join')}</h2>
            <p>{t('ssh.joinHint')}</p>
          </div>
        </div>

        <form className="ssh-join-form" autoComplete="off" onSubmit={(event) => event.preventDefault()}>
          <div className="ssh-invitation-field">
            <label htmlFor="croc-ssh-invitation">{t('ssh.invitation')}</label>
            <span className="field-row code-field">
              <input
                id="croc-ssh-invitation"
                name="croc-ssh-invitation"
                type={showCode ? 'text' : 'password'}
                autoComplete="off"
                spellCheck={false}
                value={code}
                placeholder={t('ssh.invitationPlaceholder')}
                onChange={(event) => setCode(event.target.value)}
                onPaste={(event) => {
                  event.preventDefault();
                  setCode(event.clipboardData.getData('text'));
                }}
              />
              <button
                type="button"
                aria-label={showCode ? t('ssh.hide') : t('ssh.show')}
                title={showCode ? t('ssh.hide') : t('ssh.show')}
                onClick={() => setShowCode((value) => !value)}
              >
                {showCode ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
              </button>
            </span>
          </div>
          <button
            className="primary-button"
            type="submit"
            disabled
            title={`${t('ssh.note')} ${SSH_URL}`}
          >
            <LogIn aria-hidden="true" /> {t('ssh.joinAction')}
          </button>
        </form>

        <div className="ssh-status-row" aria-live="polite">
          <span className="status-dot" />
          <span>{t('ssh.status')}</span>
        </div>

        <div className="ssh-terminal" aria-label={t('ssh.title')}>
          <p className="note">
            {t('ssh.note')} <span className="link">{SSH_URL}</span>
          </p>
        </div>

        <div className="ssh-actions">
          <p>
            <ShieldAlert aria-hidden="true" /> {t('ssh.safety')}
          </p>
        </div>
      </article>
    </section>
  );
}
