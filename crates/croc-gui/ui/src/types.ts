import type { OfferPayload } from './bridge';
import type { Sample } from './format';

/// 一次传输在界面上的全部状态。
///
/// Rust 侧只给"发生了什么"（progress / finished / offer），速率与 ETA 由这里的采样算，
/// 界面状态与后端状态刻意分成两份：后端不知道一次传输在界面上被折叠还是被点了取消。
export interface TransferView {
  id: string;
  direction: 'send' | 'receive';
  /// 一句人类可读的状态（来自 Rust 侧的事件）。
  status: string;
  file: string | null;
  transferred: number;
  total: number;
  /// 正在跑（`false` 表示已结束或已被取消）。
  active: boolean;
  ok: boolean | null;
  error: string | null;
  summary: string | null;
  samples: Sample[];
  /// 非 `null` 表示这张清单还在等用户按按钮。
  offer: OfferPayload | null;
}
