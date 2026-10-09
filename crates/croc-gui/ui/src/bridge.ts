import { invoke } from '@tauri-apps/api/core';
import { writeText } from '@tauri-apps/plugin-clipboard-manager';
import { openPath, revealItemInDir } from '@tauri-apps/plugin-opener';

/// Rust 侧推给前端的事件名。改这里的同时必须改 `crates/croc-gui/src/lib.rs`。
export const EVENT = {
  progress: 'croc://progress',
  finished: 'croc://finished',
  offer: 'croc://offer',
} as const;

export interface ProgressPayload {
  id: string;
  direction: 'send' | 'receive';
  status: string;
  file: string | null;
  transferred: number;
  total: number;
}

export interface FinishedPayload {
  id: string;
  direction: 'send' | 'receive';
  ok: boolean;
  error: string | null;
  summary: string | null;
}

export interface OfferFile {
  name: string;
  size: number;
}

export interface OfferPayload {
  id: string;
  files: OfferFile[];
  folders: number;
  total: number;
}

export interface SendArgs {
  paths: string[];
  relay?: string;
  pass?: string;
  code?: string;
  curve?: string;
  noCompress: boolean;
  /// 直接发一段文本（G6）：给了它，`paths` 可以不填。
  text?: string;
}

export interface StoreArgs {
  paths: string[];
  /// `90m` / `12h` / `3d` / `2w`；空则由服务端定。
  expiration?: string;
  /// 允许的下载次数；`0` 用默认值。
  downloads: number;
  /// 服务来源；空则用 https://getcroc.com
  url?: string;
}

export interface StoredResult {
  id: string;
  link: string;
  uploadToken: string;
  downloads: number;
  expiresAt: string;
}

export interface ReceiveArgs {
  code: string;
  relay?: string;
  pass?: string;
  outDir?: string;
  curve?: string;
  overwrite: boolean;
  rename: boolean;
}

/// `invoke` 在 WebView 之外（比如 `vite dev` 直接在浏览器里开）会抛错。
/// 包一层是为了让这些异常最终都以同一句话回到界面上，而不是冒出一串 IPC 错误。
async function call<T>(name: string, args?: Record<string, unknown>): Promise<T> {
  try {
    return await invoke<T>(name, args);
  } catch (error) {
    throw new Error(`${name}: ${String(error)}`);
  }
}

export const api = {
  generateCode: (requested?: string) => call<string>('generate_code', { requested }),
  send: (args: SendArgs) => call<string>('send', { args }),
  storeUpload: (args: StoreArgs) => call<StoredResult>('store_upload', { args }),
  receive: (args: ReceiveArgs) => call<string>('receive', { args }),
  cancel: (id: string) => call<void>('cancel_transfer', { id }),
  respondOffer: (id: string, accept: boolean) => call<void>('respond_to_offer', { id, accept }),
  // 这两个只在 Android 上真的起作用：桌面端会收到一句"仅 Android 可用"。
  exportFile: (path: string) => call<void>('export_file', { path }),
  shareFile: (path: string) => call<void>('share_file', { path }),
  setLocale: (locale: string) => call<void>('set_locale', { locale }),
  reveal: (path: string) => revealItemInDir(path).catch(() => openPath(path)),
  copy: (text: string) => writeText(text),
};
