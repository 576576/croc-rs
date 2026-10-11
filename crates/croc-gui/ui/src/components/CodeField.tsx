import { type PointerEvent, useCallback, useEffect, useRef, useState } from 'react';
import { Check, ClipboardPaste, Copy } from 'lucide-react';
import { api } from '../bridge';
import { useT } from '../i18n';

/// 长按多久算切换，不到就算单击。
const SWITCH_AFTER_MS = 500;

/// 指针挪出这么远就不算长按：那是在滚页面，不是要切换。
const MOVE_CANCEL_PX = 10;

/// 口令码一格：输入框 + 后置一枚按钮，形与 SSH 页的邀请码那处相同。
///
/// 按钮的动作可切换：复制是把这一格的内容放出去，粘贴是把外面的内容读进来。切换靠
/// 长按（触屏）或右键/菜单键（`onContextMenu`，键盘与桌面端），因为一个位置只放得下
/// 一枚按钮。
///
/// 切换要过滤：一次按压只准切一次。触屏长按是"定时器切一次 + 松手时浏览器补的
/// `contextmenu` 再切一次"，右键则是"`contextmenu` 切一次 + 按久了定时器再切一次"，
/// 两处都会连切两次、切完看着像没变。切换之后紧跟着的 click 也要点掉——那是同一次
/// 按压带出来的，不是用户想执行动作。
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
  /// 本次按压已经切换过了：定时器与 `contextmenu` 谁先到算谁的，后到的那个不作数。
  const switched = useRef(false);
  /// 切换带出来的那次 click 要吞掉：它不是用户想执行动作。
  const suppressClick = useRef(false);
  /// 按下时的位置，用来把"滚页面"和"长按"分开。
  const origin = useRef({ x: 0, y: 0 });
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

  /// 一次按压只切一次；切过之后把紧跟着的 click 一并作废。
  const switchOnce = () => {
    if (switched.current) return;
    switched.current = true;
    suppressClick.current = true;
    setMode((current) => (current === 'copy' ? 'paste' : 'copy'));
  };

  const pressStart = (event: PointerEvent<HTMLButtonElement>) => {
    if (!canSwitch) return;
    switched.current = false;
    suppressClick.current = false;
    origin.current = { x: event.clientX, y: event.clientY };
    timer.current = window.setTimeout(switchOnce, SWITCH_AFTER_MS);
  };

  const pressEnd = () => window.clearTimeout(timer.current);

  const pressMove = (event: PointerEvent<HTMLButtonElement>) => {
    const moved =
      Math.abs(event.clientX - origin.current.x) > MOVE_CANCEL_PX ||
      Math.abs(event.clientY - origin.current.y) > MOVE_CANCEL_PX;
    if (moved) window.clearTimeout(timer.current);
  };

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
        onPointerMove={pressMove}
        onContextMenu={(event) => {
          // 右键的菜单要自己吃掉，不然在按钮上右键会弹出浏览器菜单。
          event.preventDefault();
          if (!canSwitch) return;
          switchOnce();
        }}
        onClick={() => {
          if (suppressClick.current) {
            suppressClick.current = false;
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
