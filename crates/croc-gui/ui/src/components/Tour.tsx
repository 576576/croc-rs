import { useEffect } from 'react';
import { useT } from '../i18n';
import type { MessageKey } from '../locales';
import type { Page } from './WorkspaceSwitch';

export interface TourStep {
  /// 这一步在哪个大页上。
  page: Page;
  /// 高亮谁：元素的 `data-tour` 值。
  target: string;
  title: MessageKey;
  body: MessageKey;
}

/// 五步走：发送、接收、进度、历史、共享终端。
export const TOUR_STEPS: TourStep[] = [
  { page: 'transfer', target: 'send', title: 'tour.send.title', body: 'tour.send.body' },
  { page: 'transfer', target: 'receive', title: 'tour.receive.title', body: 'tour.receive.body' },
  // 进度那一格在站点头部，三页都在，所以这一步放哪一页都一样。
  { page: 'transfer', target: 'progress', title: 'tour.progress.title', body: 'tour.progress.body' },
  { page: 'history', target: 'history', title: 'tour.history.title', body: 'tour.history.body' },
  { page: 'ssh', target: 'ssh', title: 'tour.ssh.title', body: 'tour.ssh.body' },
];

/// 指引本身：切到对应页面、给目标元素描一圈边框，说明写在底部的气泡里。
/// 关掉它的按钮就是右上角那枚问号（`ToolbarActions` 的 toggle）。
export function Tour({
  index,
  onIndex,
  onClose,
}: {
  index: number;
  onIndex: (next: number) => void;
  onClose: () => void;
}) {
  const t = useT();
  const step = TOUR_STEPS[index];
  const last = index === TOUR_STEPS.length - 1;

  useEffect(() => {
    const element = document.querySelector(`[data-tour="${step.target}"]`);
    element?.classList.add('tour-target');
    element?.scrollIntoView({ block: 'nearest' });
    return () => element?.classList.remove('tour-target');
  }, [step.target]);

  if (!step) return null;

  return (
    <div className="tour-bubble" role="dialog" aria-label={t('nav.howToUse')}>
      <span className="tour-step">{t('tour.step', { current: index + 1, total: TOUR_STEPS.length })}</span>
      <strong>{t(step.title)}</strong>
      <p>{t(step.body)}</p>
      <div className="tour-actions">
        <button type="button" className="ghost" disabled={index === 0} onClick={() => onIndex(index - 1)}>
          {t('tour.back')}
        </button>
        {last ? (
          <button type="button" onClick={onClose}>
            {t('tour.done')}
          </button>
        ) : (
          <button type="button" onClick={() => onIndex(index + 1)}>
            {t('tour.next')}
          </button>
        )}
      </div>
    </div>
  );
}
