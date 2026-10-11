import { useEffect, useState } from 'react';
import { useT } from '../i18n';
import { api } from '../bridge';
import { isAndroid } from '../platform';
import { baseName, formatBytes } from '../format';
import { File, FolderOpen, X } from 'lucide-react';

/// 选中清单，照 croc web 的 `selection-summary` 与 `file-list`：先一行"几个、多大"，
/// 再一行一个文件（图标 / 名字 / 大小 / 移除）。空清单只剩那一行计数，不另给提示语。
///
/// 名字那一列是 `minmax(0, 1fr)` 且 `text-overflow: ellipsis`，长名字压在这一列里，
/// 不会挤到右边的大小与按钮上。完整路径不占列：悬停看 `title`，点一下在系统文件
/// 管理器里显示它（`api.reveal`，Android 上这个调用会失败并被忽略）。
///
/// `folder` 为真时大小那一列整个不要：`stat_paths` 对目录返回 `None`（递归求和要遍历
/// 整棵树），没有可显示的数，也就不必去查。
export function FileList({
  paths,
  folder = false,
  onRemove,
}: {
  paths: string[];
  folder?: boolean;
  onRemove: (path: string) => void;
}) {
  const t = useT();
  const [sizes, setSizes] = useState<Record<string, number | null>>({});

  useEffect(() => {
    if (folder || paths.length === 0) {
      setSizes({});
      return;
    }
    let cancelled = false;
    void api
      .statPaths(paths)
      .then((bytes) => {
        if (cancelled) return;
        setSizes(Object.fromEntries(paths.map((path, index) => [path, bytes[index] ?? null])));
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [paths, folder]);

  const total = paths.reduce((sum, path) => sum + (sizes[path] ?? 0), 0);

  return (
    <>
      <div className="selection-summary">
        <span>
          {folder
            ? t('send.summary.folders', { count: paths.length })
            : t('send.summary.files', { count: paths.length })}
        </span>
        {!folder && <span>{formatBytes(total)}</span>}
      </div>

      {paths.length > 0 && (
        <ul className={folder ? 'file-list folder' : 'file-list'}>
          {paths.map((path) => {
            const size = sizes[path];
            return (
              <li key={path}>
                {/* 目录用文件夹图标：`stat_paths` 对目录报 `None`，那就是它。 */}
                {folder || size === null ? <FolderOpen aria-hidden="true" /> : <File aria-hidden="true" />}
                <button
                  type="button"
                  className="file-name"
                  title={path}
                  onClick={() => void api.reveal(path).catch(() => undefined)}
                >
                  {baseName(path)}
                </button>
                {/* 大小要等后端查回来，那一格先占个位，免得整行跳一下；目录与读不到的留一道横线。 */}
                {!folder && <span>{size === undefined ? '·' : size === null ? '—' : formatBytes(size)}</span>}
                <button
                  type="button"
                  className="list-action"
                  onClick={() => onRemove(path)}
                  aria-label={`${t('transfers.cancel')} · ${baseName(path)}`}
                >
                  <X aria-hidden="true" />
                </button>
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}

/// Tauri 的文件选择器给的是完整路径；`File` 对象给不到路径，
/// 所以桌面版只认这条路与拖放事件里的路径数组。
///
/// `directory` 为真时在 Android 上走自研的 SAF 选择器（返回 `content://` 目录 URI）：
/// 官方 dialog 插件用的是 `ACTION_GET_CONTENT`，那个 intent 选不了目录，拿到手也只是
/// 个文件 URI。桌面端没有 SAF，仍走系统文件管理器。
export async function chooseFiles(multiple: boolean, directory: boolean): Promise<string[]> {
  if (directory && isAndroid) {
    // 用户取消时返回的也是 null——Android 上不该再退回 dialog 插件，
    // 那个 intent 给不出目录。
    const uri = await api.pickSafDir().catch(() => null);
    return uri ? [uri] : [];
  }
  const { open } = await import('@tauri-apps/plugin-dialog');
  const picked = await open({ multiple, directory });
  if (!picked) return [];
  return Array.isArray(picked) ? picked : [picked];
}
