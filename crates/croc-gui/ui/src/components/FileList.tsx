import { useT } from '../i18n';
import { baseName } from '../format';
import { Trash2 } from 'lucide-react';

export function FileList({ paths, onRemove }: { paths: string[]; onRemove: (path: string) => void }) {
  const t = useT();

  if (paths.length === 0) {
    return <p className="note">{t('send.files.empty')}</p>;
  }

  return (
    <ul className="file-list">
      {paths.map((path) => (
        <li key={path} title={path}>
          <Trash2 role="presentation" />
          <span>{baseName(path)}</span>
          <span>{path}</span>
          <button type="button" className="ghost" onClick={() => onRemove(path)} aria-label={t('transfers.cancel')}>
            <Trash2 />
          </button>
        </li>
      ))}
    </ul>
  );
}

/// Tauri 的文件选择器给的是完整路径；`File` 对象给不到路径，
/// 所以桌面版只认 {@link api} 那条路与 drop 事件里的路径数组。
export async function chooseFiles(multiple: boolean, directory: boolean): Promise<string[]> {
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({ multiple, directory });
  if (!picked) return [];
  return Array.isArray(picked) ? picked : [picked];
}
