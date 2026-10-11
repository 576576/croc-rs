import { useEffect, useRef, useState, useCallback } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { WorkspaceSwitch, type Page } from './components/WorkspaceSwitch';
import { ToolbarActions } from './components/ToolbarActions';
import { Tour, TOUR_STEPS } from './components/Tour';
import { SettingsGrid, type Settings } from './components/SettingsGrid';
import { SendPanel, type ContentMode, type DeliveryMode } from './components/SendPanel';
import { ReceivePanel } from './components/ReceivePanel';
import { SshPanel } from './components/SshPanel';
import { HistoryPanel, loadHistory, saveHistory, type HistoryEntry } from './components/HistoryPanel';
import { TransfersList } from './components/TransfersList';
import {
  api,
  EVENT,
  type FinishedPayload,
  type OfferPayload,
  type ProgressPayload,
} from './bridge';
import { useT } from './i18n';
import { isAndroid } from './platform';
import { applyTheme, resolveTheme } from './theme';
import type { TransferView } from './types';

const SETTINGS_KEY = 'croc-gui-settings';

const DEFAULT_SETTINGS: Settings = {
  relay: '',
  pass: 'pass123',
  rememberPass: false,
  curve: 'p256',
  noCompress: false,
};

function loadSettings(): Settings {
  try {
    const stored = localStorage.getItem(SETTINGS_KEY);
    if (!stored) return DEFAULT_SETTINGS;
    const parsed = JSON.parse(stored) as Partial<Settings>;
    return {
      ...DEFAULT_SETTINGS,
      ...parsed,
      // 密码只在用户勾了"记住"时才落盘：没勾就每次都从默认值起步。
      pass: parsed.rememberPass ? parsed.pass ?? DEFAULT_SETTINGS.pass : DEFAULT_SETTINGS.pass,
    };
  } catch {
    return DEFAULT_SETTINGS;
  }
}

function saveSettings(value: Settings): void {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(value.rememberPass ? value : { ...value, pass: '' }));
  } catch {
    // 存不下不影响当前会话。
  }
}

export function App() {
  const t = useT();
  const [page, setPage] = useState<Page>('transfer');
  const [tourOpen, setTourOpen] = useState(false);
  const [tourStep, setTourStep] = useState(0);
  const [delivery, setDelivery] = useState<DeliveryMode>('direct');
  const [mode, setMode] = useState<ContentMode>('files');
  const [files, setFiles] = useState<string[]>([]);
  const [text, setText] = useState('');
  const [customCode, setCustomCode] = useState('');
  const [code, setCode] = useState<string | null>(null);
  const [receiveCode, setReceiveCode] = useState('');
  // Android 上这格存的是 SAF 目录 URI（用户挑过才有）；空表示没挑，收到后发到公共
  // 下载目录。桌面端存的是真实路径，初值 `.` 即当前目录。
  const [outDir, setOutDir] = useState(isAndroid ? '' : '.');
  // 收到文件的真实落盘目录（Android 上是应用私有目录）。另存与分享要它拼路径——
  // `outDir` 在 Android 上可能是 `content://`，拼不出文件系统路径。
  const [sandbox, setSandbox] = useState('.');
  const [overwrite, setOverwrite] = useState(false);
  const [rename, setRename] = useState(false);
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [transfers, setTransfers] = useState<TransferView[]>([]);
  const [history, setHistory] = useState<HistoryEntry[]>(loadHistory);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /// 每条传输最近的快照：收工时要从它身上取名字与字节数写进历史。
  const latest = useRef<Record<string, TransferView>>({});

  useEffect(() => {
    applyTheme(resolveTheme());
  }, []);

  // 收到文件的真实落盘目录：Android 上是应用私有目录。拿不到就退回 `.`——
  // 桌面端本来就是当前目录，Android 上没这个值也只是另存/分享按下会失败。
  useEffect(() => {
    void api
      .sandboxDir()
      .then(setSandbox)
      .catch(() => undefined);
  }, []);

  // 指引走到哪一步就切到那一页：步骤里有跨页的（历史、共享终端）。
  useEffect(() => {
    if (tourOpen) setPage(TOUR_STEPS[tourStep].page);
  }, [tourOpen, tourStep]);

  const patchTransfer = useCallback((id: string, patch: Partial<TransferView>) => {
    setTransfers((current) => current.map((item) => (item.id === id ? { ...item, ...patch } : item)));
  }, []);

  const recordHistory = useCallback((entry: HistoryEntry) => {
    setHistory((current) => {
      const next = [entry, ...current].slice(0, 200);
      saveHistory(next);
      return next;
    });
  }, []);

  useEffect(() => {
    const unlisteners: Array<Promise<UnlistenFn>> = [];

    unlisteners.push(
      listen<ProgressPayload>(EVENT.progress, (event) => {
        const payload = event.payload;
        setTransfers((current) =>
          current.map((item) => {
            if (item.id !== payload.id) return item;
            const next: TransferView = {
              ...item,
              status: payload.status,
              file: payload.file,
              transferred: payload.transferred,
              total: payload.total,
              active: true,
              samples: [...item.samples, { at: Date.now(), transferred: payload.transferred }].slice(-40),
            };
            latest.current[payload.id] = next;
            return next;
          }),
        );
      }),
    );

    unlisteners.push(
      listen<FinishedPayload>(EVENT.finished, (event) => {
        const payload = event.payload;
        patchTransfer(payload.id, {
          active: false,
          ok: payload.ok,
          error: payload.error,
          summary: payload.summary,
        });
        const snapshot = latest.current[payload.id];
        recordHistory({
          id: payload.id,
          direction: payload.direction,
          name: snapshot?.file ?? payload.id,
          bytes: snapshot?.total ?? 0,
          at: Date.now(),
          ok: payload.ok,
          error: payload.error,
        });
        setBusy(false);
      }),
    );

    unlisteners.push(
      listen<OfferPayload>(EVENT.offer, (event) => {
        const offer = event.payload;
        patchTransfer(offer.id, { offer });
      }),
    );

    // 返回键（Android）：退到传输页，而不是直接退应用——后者会把正在跑的传输一起带走。
    unlisteners.push(listen('tauri://back-button', () => setPage('transfer')));

    return () => {
      unlisteners.forEach((promise) => void promise.then((unlisten) => unlisten()));
    };
  }, [patchTransfer, recordHistory]);

  const addTransfer = useCallback((id: string, direction: 'send' | 'receive') => {
    const entry: TransferView = {
      id,
      direction,
      status: '',
      file: null,
      transferred: 0,
      total: 0,
      active: true,
      ok: null,
      error: null,
      summary: null,
      samples: [],
      offer: null,
    };
    latest.current[id] = entry;
    setTransfers((current) => [...current, entry]);
  }, []);

  const relayArgs = () => ({
    relay: settings.relay.trim() === '' ? undefined : settings.relay.trim(),
    pass: settings.pass.trim() === '' ? undefined : settings.pass.trim(),
  });

  const startSend = async () => {
    setError(null);
    setBusy(true);
    try {
      // 先把码生成好再点发送：码是本次会话的唯一凭据，用户应该先看一眼。
      const resolved = await api.generateCode(customCode.trim() === '' ? undefined : customCode.trim());
      setCode(resolved);
      const id = await api.send({
        paths: mode === 'text' ? [] : files,
        text: mode === 'text' ? text : undefined,
        ...relayArgs(),
        code: resolved,
        curve: settings.curve,
        noCompress: settings.noCompress,
      });
      addTransfer(id, 'send');
    } catch (cause) {
      setError(String(cause));
      setBusy(false);
    }
  };

  const startReceive = async () => {
    setError(null);
    setBusy(true);
    try {
      const id = await api.receive({
        code: receiveCode.trim(),
        ...relayArgs(),
        outDir,
        curve: settings.curve,
        overwrite,
        rename,
      });
      addTransfer(id, 'receive');
      patchTransfer(id, { offer: null });
    } catch (cause) {
      setError(String(cause));
      setBusy(false);
    }
  };

  const respondOffer = async (id: string, accept: boolean) => {
    try {
      await api.respondOffer(id, accept);
      patchTransfer(id, { offer: null });
    } catch (cause) {
      setError(String(cause));
    }
  };

  return (
    <div className="app-frame">
      <div className="site-shell">
        <div className="site-head">
          <div>
            <h1>{t('app.title')}</h1>
          </div>
        </div>

        <div className="workspace-toolbar">
          <WorkspaceSwitch
            value={page}
            labels={{
              transfer: t('workspace.transfer'),
              ssh: t('workspace.ssh'),
              history: t('workspace.history'),
            }}
            onChange={setPage}
          />
          <ToolbarActions
            tourActive={tourOpen}
            onToggleTour={() => {
              setTourStep(0);
              setTourOpen((open) => !open);
            }}
          />
        </div>

        {page === 'transfer' && (
          <div className="transfer-grid">
            <SendPanel
              delivery={delivery}
              mode={mode}
              files={files}
              text={text}
              busy={busy}
              code={code}
              settings={settings}
              customCode={customCode}
              onDelivery={setDelivery}
              onMode={setMode}
              onFiles={setFiles}
              onText={setText}
              onCustomCode={setCustomCode}
              onStart={() => void startSend()}
            />
            <ReceivePanel
              code={receiveCode}
              busy={busy}
              outDir={outDir}
              isAndroid={isAndroid}
              overwrite={overwrite}
              rename={rename}
              onCode={setReceiveCode}
              onOutDir={setOutDir}
              onOverwrite={setOverwrite}
              onRename={setRename}
              onStart={() => void startReceive()}
            />
          </div>
        )}

        {page === 'ssh' && <SshPanel />}

        {page === 'history' && (
          <HistoryPanel
            entries={history}
            onClear={() => {
              setHistory([]);
              saveHistory([]);
            }}
          />
        )}

        {page === 'transfer' && (
          <section className="panel" data-tour="queue">
            <span className="field-label">{t('transfers.title')}</span>
            <TransfersList
              transfers={transfers}
              isAndroid={isAndroid}
              resolvePath={(name) => `${sandbox.replace(/[\\/]+$/, '')}/${name}`}
              onCancel={(id) => void api.cancel(id).catch((cause) => setError(String(cause)))}
              onRespond={(id, accept) => void respondOffer(id, accept)}
              onSave={(path) => void api.exportFile(path).catch((cause) => setError(String(cause)))}
              onShare={(path) => void api.shareFile(path).catch((cause) => setError(String(cause)))}
            />
            {error && <p className="note danger">{error}</p>}
            <div className="divider" />
            <SettingsGrid
              value={settings}
              connected={{
                relay: transfers.some((item) => item.active),
                data: transfers.some((item) => item.transferred > 0),
              }}
              onChange={(next) => {
                setSettings(next);
                saveSettings(next);
              }}
            />
          </section>
        )}

        {tourOpen && <Tour index={tourStep} onIndex={setTourStep} onClose={() => setTourOpen(false)} />}
      </div>
    </div>
  );
}
