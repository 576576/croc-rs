// 前端：只做两件事——调 Tauri 命令、渲染事件。
//
// 刻意不引任何前端框架：这个界面只有三个面板加一个列表，加一层构建链只会让
// `cargo tauri build` 多一个失败点。状态全部在内存里，刷新即重置（历史留给后续的
// SQLite 插件，见 docs/agent/plan/05-gui-plan.md）。
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
const transfers = new Map();

// ── 标签页 ────────────────────────────────────────────────────────────
document.querySelectorAll('.tab').forEach((tab) => {
  tab.addEventListener('click', () => {
    document.querySelectorAll('.tab').forEach((other) => other.classList.toggle('is-active', other === tab));
    document.querySelectorAll('.panel').forEach((panel) => {
      panel.classList.toggle('is-active', panel.id === `panel-${tab.dataset.panel}`);
    });
  });
});

// ── 设置读取 ──────────────────────────────────────────────────────────
const settings = () => ({
  relay: $('set-relay').value.trim() || undefined,
  pass: $('set-pass').value.trim() || undefined,
  curve: $('set-curve').value,
  noCompress: $('set-nocompress').checked,
});

// ── 发送 ──────────────────────────────────────────────────────────────
$('send-generate').addEventListener('click', async () => {
  try {
    const code = await invoke('generate_code', { requested: null });
    $('send-code').value = code;
    $('send-code-out').textContent = code;
  } catch (error) {
    $('send-code-out').textContent = `生成失败：${error}`;
  }
});

$('send-start').addEventListener('click', async () => {
  const paths = $('send-paths').value.split('\n').map((line) => line.trim()).filter(Boolean);
  if (paths.length === 0) {
    $('send-code-out').textContent = '请先填写要发送的路径';
    return;
  }
  try {
    const id = await invoke('send', {
      args: { paths, code: $('send-code').value.trim() || null, ...settings() },
    });
    upsert(id, { direction: 'send', name: paths.join(', '), state: '准备中…', percent: 0 });
  } catch (error) {
    $('send-code-out').textContent = `发送失败：${error}`;
  }
});

// ── 接收 ──────────────────────────────────────────────────────────────
$('recv-start').addEventListener('click', async () => {
  const code = $('recv-code').value.trim();
  if (!code) return;
  try {
    const id = await invoke('receive', {
      code,
      relay: settings().relay ?? null,
      pass: settings().pass ?? null,
      outDir: $('recv-out').value.trim() || '.',
      curve: settings().curve,
      overwrite: $('recv-overwrite').checked,
      rename: $('recv-rename').checked,
    });
    upsert(id, { direction: 'receive', name: code, state: '连接中…', percent: 0 });
  } catch (error) {
    alert(`接收失败：${error}`);
  }
});

// ── 事件 ──────────────────────────────────────────────────────────────
listen('croc://progress', ({ payload }) => {
  const existing = transfers.get(payload.id) ?? { direction: payload.direction, name: '' };
  const percent = payload.total > 0 ? Math.min(100, Math.round((payload.transferred / payload.total) * 100)) : 0;
  upsert(payload.id, {
    ...existing,
    name: payload.file ?? existing.name,
    state: payload.status || existing.state,
    percent,
  });
});

listen('croc://finished', ({ payload }) => {
  const existing = transfers.get(payload.id) ?? { direction: payload.direction, name: '' };
  upsert(payload.id, {
    ...existing,
    state: payload.ok ? payload.summary ?? '完成' : `失败：${payload.error ?? '未知原因'}`,
    done: true,
    ok: payload.ok,
    percent: payload.ok ? 100 : existing.percent ?? 0,
  });
});

// ── 渲染 ──────────────────────────────────────────────────────────────
function upsert(id, patch) {
  transfers.set(id, { ...(transfers.get(id) ?? {}), ...patch });
  render();
}

function render() {
  const list = $('transfer-list');
  if (transfers.size === 0) {
    list.innerHTML = '<li class="empty">还没有传输</li>';
    return;
  }
  list.innerHTML = '';
  for (const [id, item] of transfers) {
    const li = document.createElement('li');
    const stateClass = item.done ? (item.ok ? 't-state ok' : 't-state bad') : 't-state';
    li.innerHTML = `
      <div class="t-head">
        <span class="t-name"></span>
        <span class="${stateClass}"></span>
      </div>
      <div class="t-bar"><i style="width:${item.percent ?? 0}%"></i></div>
      <div class="t-actions"></div>`;
    li.querySelector('.t-name').textContent = `${item.direction === 'send' ? '↑' : '↓'} ${item.name || id}`;
    li.querySelector(`.${stateClass.split(' ').pop()}`).textContent = item.state ?? '';
    if (!item.done) {
      const button = document.createElement('button');
      button.className = 'ghost';
      button.textContent = '取消';
      button.addEventListener('click', async () => {
        try {
          await invoke('cancel_transfer', { id });
        } catch (error) {
          // 已经结束的传输取消不了——这是正常情况，刷新一下列表即可。
          console.warn(error);
        }
      });
      li.querySelector('.t-actions').append(button);
    }
    list.append(li);
  }
}
