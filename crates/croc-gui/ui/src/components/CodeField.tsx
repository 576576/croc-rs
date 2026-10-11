import { useCallback, useEffect, useRef, useState } from 'react';
import { Check, ClipboardPaste, Copy } from 'lucide-react';
import { api } from '../bridge';
import { useT } from '../i18n';

/// 长按多久算切换，不到就算单击。
const SWITCH_AFTER_MS = 500;

/// 口令码一格：输入框 + 后置一枚按钮，形与 SSH 页的邀请码那处相同。
///
/// 按钮的动作可切换：复制是把这一格的内容放出去，粘贴是把外面的内容读进来。切换靠
/// 长按（触屏）或右键/菜单键（`onContextMenu`，键盘与桌面端），因为一个位置只放得下
/// 一枚按钮。切换后要点掉紧接着的那次 click——长按结束时浏览器会补一个 click，
/// 不拦掉就会在切换的同一瞬间又执行一次动作。
export function CodeField({
  value,
  action = 'copy',
  copyLabel,
  placeholder,
  readOnly = false,
  onValue,
}: {
  value: string;
  /// 初始动作。只读的格子只能复制（取件链接、已生成的码）。
  action?: 'copy' | 'paste';
  /// 复制动作的名字，默认"复制口令码"；展示取件链接那处要换成"复制链接"。
  copyLabel?: string;
  /// 可编辑时提示"留空则自动生成"；只读时不需要。
  placeholder?: string;
  readOnly?: boolean;
  onValue?: (next: string) => void;
}) {
  const t = useT();
  const [mode, setMode] = useState<'copy' | 'paste'>(action);
  const [done, setDone] = useState(false);
  const timer = useRef(0);
  /// 这次 click 是不是长按切换带出来的。
  const swallowed = useRef(false);
  const canSwitch = onValue !== undefined && !readOnly;

  useEffect(() => () => window.clearTimeout(timer.current), []);

  const flash = useCallback(() => {
    setDone(true);
    window.setTimeout(() => setDone(false), 1200);
  }, []);

  const run = () => {
    if (mode === 'copy') {
      // 空格子没什么可复制的，按了也不该冒成功提示。
      if (value.trim() === '') return;
      void api.copy(value).then(flash).catch(() => undefined);
      return;
    }
    void api
      .paste()
      .then((text) => {
        onValue?.(text);
        flash();
      })
      .catch(() => undefined);
  };

  const toggle = () => setMode((current) => (current === 'copy' ? 'paste' : 'copy'));

  const pressStart = () => {
    if (!canSwitch) return;
    swallowed.current = false;
    timer.current = window.setTimeout(() => {
      swallowed.current = true;
      toggle();
    }, SWITCH_AFTER_MS);
  };

  const pressEnd = () => window.clearTimeout(timer.current);

  const label = mode === 'copy' ? (copyLabel ?? t('code.copy')) : t('code.paste');
  const hint = canSwitch ? `${label} · ${t('code.longPress')}` : label;

  return (
    <div className="code-field">
      <input
        type="text"
        value={value}
        readOnly={readOnly}
        placeholder={placeholder}
        spellCheck={false}
        onChange={(event) => onValue?.(event.target.value)}
      />
      <button
        type="button"
        aria-label={hint}
        title={hint}
        onPointerDown={pressStart}
        onPointerUp={pressEnd}
        onPointerLeave={pressEnd}
        onContextMenu={(event) => {
          if (!canSwitch) return;
          event.preventDefault();
          toggle();
        }}
        onClick={() => {
          if (swallowed.current) {
            swallowed.current = false;
            return;
          }
          run();
        }}
      >
        {done ? <Check aria-hidden="true" /> : mode === 'copy' ? <Copy aria-hidden="true" /> : <ClipboardPaste aria-hidden="true" />}
      </button>
    </div>
  );
}
