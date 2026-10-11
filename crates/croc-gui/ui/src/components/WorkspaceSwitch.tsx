import { ArrowLeftRight, Clock, Terminal } from 'lucide-react';

export type Page = 'transfer' | 'ssh' | 'history';

/// 三个大页。图标按 croc web 的调子：往来方向、终端、时间。
/// 文字包一层 `workspace-switch-label`：窄屏下样式把未选中的那些收成只剩图标。
export function WorkspaceSwitch({
  value,
  labels,
  onChange,
}: {
  value: Page;
  labels: { transfer: string; ssh: string; history: string };
  onChange: (next: Page) => void;
}) {
  return (
    <div className="workspace-switch three" role="tablist" aria-label={labels.transfer}>
      {/* 文字另给一个 `aria-label`：窄屏上它会被样式藏掉，藏掉之后读屏与悬停仍要有名字。 */}
      <button
        type="button"
        role="tab"
        aria-selected={value === 'transfer'}
        aria-label={labels.transfer}
        title={labels.transfer}
        onClick={() => onChange('transfer')}
      >
        <ArrowLeftRight aria-hidden="true" />
        <span className="workspace-switch-label">{labels.transfer}</span>
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={value === 'ssh'}
        aria-label={labels.ssh}
        title={labels.ssh}
        onClick={() => onChange('ssh')}
      >
        <Terminal aria-hidden="true" />
        <span className="workspace-switch-label">{labels.ssh}</span>
      </button>
      <button
        type="button"
        role="tab"
        aria-selected={value === 'history'}
        aria-label={labels.history}
        title={labels.history}
        onClick={() => onChange('history')}
      >
        <Clock aria-hidden="true" />
        <span className="workspace-switch-label">{labels.history}</span>
      </button>
    </div>
  );
}
