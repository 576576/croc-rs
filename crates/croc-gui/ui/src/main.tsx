import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { LocaleProvider } from './i18n';
import { App } from './App';
import './styles.css';

/// 挡掉缩放页面的一切入口。
///
/// 触摸那条在 CSS（`html { touch-action: pan-x pan-y }`）与 viewport 元信息里，
/// 这里补的是桌面的两条：Ctrl+滚轮（以及触控板的双指滑动，Chromium 会把它合成
/// 带 `ctrlKey` 的 wheel）与 Safari 的 `gesture*` 事件。键盘的 Ctrl+加号由 WebView
/// 自己处理，前端拦不到。
function blockZoom(): void {
  window.addEventListener('wheel', (event) => {
    if (event.ctrlKey) event.preventDefault();
  }, { passive: false });
  for (const name of ['gesturestart', 'gesturechange', 'gestureend']) {
    document.addEventListener(name, (event) => event.preventDefault());
  }
}

blockZoom();

const container = document.getElementById('root');
if (!container) throw new Error('#root missing from index.html');

createRoot(container).render(
  <StrictMode>
    <LocaleProvider>
      <App />
    </LocaleProvider>
  </StrictMode>,
);
