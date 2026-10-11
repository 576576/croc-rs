import { useT } from '../i18n';
import { Disclosure } from './Disclosure';
import { StatusDot } from './StatusDot';

export interface Settings {
  relay: string;
  pass: string;
  rememberPass: boolean;
  curve: string;
  noCompress: boolean;
  /// 发送前是否重新生成口令码。勾着就用新码，不勾则用口令码那格里填的。
  /// 勾选框画在发送页（它管的是那一格），所以它在这里只是为了跟着设置一起存。
  resetCode: boolean;
}

export function SettingsGrid({
  value,
  connected,
  onChange,
}: {
  value: Settings;
  connected: { relay: boolean; data: boolean };
  onChange: (next: Settings) => void;
}) {
  const t = useT();
  const set = <K extends keyof Settings>(key: K, next: Settings[K]) => onChange({ ...value, [key]: next });

  return (
    <Disclosure label={t('settings.title')}>
      <div className="settings-grid">
        <label>
          <span>{t('settings.relay')}</span>
          <input
            type="text"
            value={value.relay}
            placeholder={t('settings.relayPlaceholder')}
            onChange={(event) => set('relay', event.target.value)}
          />
        </label>
        <label>
          <span>{t('settings.pass')}</span>
          <input
            type={value.rememberPass ? 'text' : 'password'}
            value={value.pass}
            onChange={(event) => set('pass', event.target.value)}
          />
        </label>
        <label>
          <span>{t('settings.curve')}</span>
          <select value={value.curve} onChange={(event) => set('curve', event.target.value)}>
            <option value="p256">p256</option>
            <option value="p384">p384</option>
            <option value="p521">p521</option>
            <option value="siec">siec</option>
            <option value="ed25519">ed25519</option>
          </select>
        </label>
        <label className="checkbox-label">
          <input type="checkbox" checked={value.rememberPass} onChange={(event) => set('rememberPass', event.target.checked)} />
          <span>{t('settings.rememberPass')}</span>
        </label>
        <label className="checkbox-label">
          <input type="checkbox" checked={value.noCompress} onChange={(event) => set('noCompress', event.target.checked)} />
          <span>{t('settings.noCompress')}</span>
        </label>
        <div className="checkbox-label">
          <StatusDot connected={connected.relay} />
          <span>{t('settings.statusRelay')}</span>
          <StatusDot connected={connected.data} />
          <span>{t('settings.statusData')}</span>
        </div>
      </div>
    </Disclosure>
  );
}
