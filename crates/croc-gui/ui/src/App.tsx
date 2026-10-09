import { useEffect, useState, useCallback } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { TitleBar } from './components/TitleBar';
import { WorkspaceSwitch, type Workspace } from './components/WorkspaceSwitch';
import { SettingsGrid, type Settings } from './components/SettingsGrid';
import { SendPanel, type ContentMode } from './components/SendPanel';
import { ReceivePanel } from './components/ReceivePanel';
import { StoredPanel } from './components/StoredPanel';
import { SshPlaceholder } from './components/SshPlaceholder';
import { TransfersList } from './components/TransfersList';
import {
  api,
  EVENT,
  type FinishedPayload,
  type OfferPayload,
  type ProgressPayload,
  type StoredResult,
} from './bridge';
import { useT } from './i18n';
import { applyTheme, resolveTheme } from './theme';
import type { TransferView } from './types';

const SETTINGS_KEY = 'croc-gui-settings';

/// 另存 / 分享只在 Android 上给：桌面端有系统文件管理器，调过去只会收到一句报错。
const isAndroid = /Android/.test(navigator.userAgent);

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
  const [workspace, setWorkspace] = useState<Workspace>('send');
  const [mode, setMode] = useState<ContentMode>('files');
  const [files, setFiles] = useState<string[]>([]);
  const [text, setText] = useState('');
  const [customCode, setCustomCode] = useState('');
  const [code, setCode] = useState<string | null>(null);
  const [receiveCode, setReceiveCode] = useState('');
  const [outDir, setOutDir] = useState('.');
  const [overwrite, setOverwrite] = useState(false);
  const [rename, setRename] = useState(false);
  const [expiration, setExpiration] = useState('');
  const [downloads, setDownloads] = useState(1);
  const [stored, setStored] = useState<StoredResult | null>(null);
  const [settings, setSettings] = useState<Settings>(loadSettings);
  const [transfers, setTransfers] = useState<TransferView[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    applyTheme(resolveTheme());
  }, []);

  const patchTransfer = useCallback((id: string, patch: Partial<TransferView>) => {
    setTransfers((current) => current.map((item) => (item.id === id ? { ...item, ...patch } : item)));
  }, []);

  useEffect(() => {
    const unlisteners: Array<Promise<UnlistenFn>> = [];

    unlisteners.push(
      listen<ProgressPayload>(EVENT.progress, (event) => {
        const payload = event.payload;
        setTransfers((current) =>
          current.map((item) =>
            item.id === payload.id
              ? {
                  ...item,
                  status: payload.status,
                  file: payload.file,
                  transferred: payload.transferred,
                  total: payload.total,
                  active: true,
                  samples: [...item.samples, { at: Date.now(), transferred: payload.transferred }].slice(-8),
                }
              : item,
          ),
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
        setBusy(false);
      }),
    );

    unlisteners.push(
      listen<OfferPayload>(EVENT.offer, (event) => {
        const offer = event.payload;
        patchTransfer(offer.id, { offer });
      }),
    );

    // 拖文件到窗口：Web 的 File 对象拿不到路径，只有这个事件给真路径。
    unlisteners.push(
      listen<{ paths: string[] }>('tauri://file-drop', (event) => {
        const dropped = event.payload?.paths ?? [];
        if (dropped.length > 0) {
          setFiles((current) => [...current, ...dropped.filter((path) => !current.includes(path))]);
          setMode('files');
          setWorkspace('send');
        }
      }),
    );

    // 返回键（Android）：退到发送页，而不是直接退应用——后者会把正在跑的传输一起带走。
    unlisteners.push(listen('tauri://back-button', () => setWorkspace('send')));

    return () => {
      unlisteners.forEach((promise) => void promise.then((unlisten) => unlisten()));
    };
  }, [patchTransfer]);

  const addTransfer = useCallback((id: string, direction: 'send' | 'receive') => {
    setTransfers((current) => [
      ...current,
      {
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
      },
    ]);
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
        paths: mode === 'files' ? files : [],
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

  const startStore = async () => {
    setError(null);
    setBusy(true);
    try {
      setStored(
        await api.storeUpload({
          paths: files,
          expiration: expiration.trim() === '' ? undefined : expiration.trim(),
          downloads,
        }),
      );
    } catch (cause) {
      setError(String(cause));
    } finally {
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
      <TitleBar />
      <div className="site-shell">
        <div className="site-head">
          <div>
            <h1>{t('app.title')}</h1>
            <p>{t('app.tagline')}</p>
          </div>
          <WorkspaceSwitch
            value={workspace}
            labels={{
              send: t('workspace.send'),
              receive: t('workspace.receive'),
              store: t('workspace.store'),
              ssh: t('workspace.ssh'),
            }}
            onChange={setWorkspace}
          />
        </div>

        <div className="transfer-grid">
          {workspace === 'send' && (
            <SendPanel
              mode={mode}
              files={files}
              text={text}
              busy={busy}
              code={code}
              settings={settings}
              customCode={customCode}
              onMode={setMode}
              onFiles={setFiles}
              onText={setText}
              onCustomCode={setCustomCode}
              onStart={() => void startSend()}
            />
          )}
          {workspace === 'receive' && (
            <ReceivePanel
              code={receiveCode}
              busy={busy}
              outDir={outDir}
              overwrite={overwrite}
              rename={rename}
              onCode={setReceiveCode}
              onOutDir={setOutDir}
              onOverwrite={setOverwrite}
              onRename={setRename}
              onStart={() => void startReceive()}
            />
          )}
          {workspace === 'store' && (
            <StoredPanel
              files={files}
              busy={busy}
              expiration={expiration}
              downloads={downloads}
              result={stored}
              onFiles={setFiles}
              onExpiration={setExpiration}
              onDownloads={setDownloads}
              onStart={() => void startStore()}
            />
          )}
          {workspace === 'ssh' && <SshPlaceholder />}

          <section className="panel">
            <span className="field-label">{t('transfers.title')}</span>
            <TransfersList
              transfers={transfers}
              isAndroid={isAndroid}
              resolvePath={(name) => `${outDir.replace(/[\\/]+$/, '')}/${name}`}
              onCancel={(id) => void api.cancel(id).catch((cause) => setError(String(cause)))}
              onRespond={(id, accept) => void respondOffer(id, accept)}
              onSave={(path) => void api.exportFile(path).catch((cause) => setError(String(cause)))}
              onShare={(path) => void api.shareFile(path).catch((cause) => setError(String(cause)))}
            />
            {error && <p className="note danger">{error}</p>}
            <div className="divider" />
            <SettingsGrid
              value={settings}
              connected={{ relay: transfers.some((item) => item.active), data: transfers.some((item) => item.transferred > 0) }}
              onChange={(next) => {
                setSettings(next);
                saveSettings(next);
              }}
            />
          </section>
        </div>
      </div>
    </div>
  );
}
