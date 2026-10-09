import { Upload, Download, CloudUpload, Terminal } from 'lucide-react';

export type Workspace = 'send' | 'receive' | 'store' | 'ssh';

export function WorkspaceSwitch({
  value,
  labels,
  onChange,
}: {
  value: Workspace;
  labels: { send: string; receive: string; store: string; ssh: string };
  onChange: (next: Workspace) => void;
}) {
  return (
    <div className="workspace-switch four" role="tablist" aria-label={labels.send}>
      <button type="button" role="tab" aria-selected={value === 'send'} onClick={() => onChange('send')}>
        <Upload />
        {labels.send}
      </button>
      <button type="button" role="tab" aria-selected={value === 'receive'} onClick={() => onChange('receive')}>
        <Download />
        {labels.receive}
      </button>
      <button type="button" role="tab" aria-selected={value === 'store'} onClick={() => onChange('store')}>
        <CloudUpload />
        {labels.store}
      </button>
      {/* SSH 显式占位：功能依赖还没做出来的数据面，先让人看见它在清单上（计划 §G11）。 */}
      <button
        type="button"
        role="tab"
        aria-selected={value === 'ssh'}
        disabled={value === 'ssh' && false}
        title={labels.ssh}
        onClick={() => onChange('ssh')}
      >
        <Terminal />
        {labels.ssh}
      </button>
    </div>
  );
}
