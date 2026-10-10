import type { OfferFile } from './bridge';

export function formatBytes(value: number): string {
  if (!Number.isFinite(value) || value <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB', 'TB'];
  const exponent = Math.min(Math.floor(Math.log(value) / Math.log(1024)), units.length - 1);
  const scaled = value / 1024 ** exponent;
  return `${scaled.toFixed(scaled >= 10 || exponent === 0 ? 0 : 1)} ${units[exponent]}`;
}

export function formatRate(bytesPerSecond: number): string {
  if (!Number.isFinite(bytesPerSecond) || bytesPerSecond <= 0) return '—';
  return `${formatBytes(bytesPerSecond)}/s`;
}

export function formatEta(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds <= 0) return '—';
  if (seconds < 60) return `${Math.ceil(seconds)}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ${Math.round(seconds % 60)}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

export function percent(transferred: number, total: number): number {
  if (total <= 0) return 0;
  return Math.min(100, (transferred / total) * 100);
}

/// 一次观测：某时刻已搬了多少字节。
export interface Sample {
  at: number;
  transferred: number;
}

/// 瞬时速率取最近两个采样点，ETA 用同窗口的平均速率外推。
///
/// 上游拿 `arrival-time` 做同一件事；这里自己算是不想为一个几十行的拟合再引一个包——
/// 网络速率波动本来就比拟合精度大一个量级。
export interface Projection {
  rate: number;
  eta: number;
}

/// 采样窗口的时长。进度是 100 ms 上报一次，按"最近 N 个点"取窗口会把它压到 1 秒
/// 以内，速率跟着抖——按时间取，慢速传输上窗口自然变长。
const WINDOW_MS = 3000;

export function project(samples: Sample[], transferred: number, total: number): Projection {
  if (samples.length < 2) return { rate: 0, eta: Number.POSITIVE_INFINITY };
  const last = samples[samples.length - 1];
  const recent = samples.filter((sample) => last.at - sample.at <= WINDOW_MS);
  const first = (recent.length >= 2 ? recent : samples.slice(-2))[0];
  const elapsed = (last.at - first.at) / 1000;
  if (elapsed <= 0) return { rate: 0, eta: Number.POSITIVE_INFINITY };
  const rate = (last.transferred - first.transferred) / elapsed;
  if (rate <= 0 || total <= transferred) return { rate, eta: 0 };
  return { rate, eta: (total - transferred) / rate };
}

export function sumSize(files: OfferFile[]): number {
  return files.reduce((total, file) => total + file.size, 0);
}

/// 文件路径在云端Formatted Display 里只需要最后一段，全路径留给 title 属性。
export function baseName(path: string): string {
  const normalized = path.replace(/\\/g, '/');
  return normalized.slice(normalized.lastIndexOf('/') + 1) || path;
}
