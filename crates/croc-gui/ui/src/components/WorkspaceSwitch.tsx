import { ArrowLeftRight, Clock, Terminal } from 'lucide-react';

export type Page = 'transfer' | 'ssh' | 'history';

/// 三个大页。图标按 croc web 的调子：往来方向、终端、时间。
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
      <button type="button" role="tab" aria-selected={value === 'transfer'} onClick={() => onChange('transfer')}>
        <ArrowLeftRight aria-hidden="true" />
        {labels.transfer}
      </button>
      <button type="button" role="tab" aria-selected={value === 'ssh'} onClick={() => onChange('ssh')}>
        <Terminal aria-hidden="true" />
        {labels.ssh}
      </button>
      <button type="button" role="tab" aria-selected={value === 'history'} onClick={() => onChange('history')}>
        <Clock aria-hidden="true" />
        {labels.history}
      </button>
    </div>
  );
}
