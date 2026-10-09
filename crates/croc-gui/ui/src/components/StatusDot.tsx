export function StatusDot({ connected }: { connected: boolean }) {
  return <span className={connected ? 'status-dot connected' : 'status-dot'} aria-hidden="true" />;
}
