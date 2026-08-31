let terminalTabs = [];
let activeTerminalId = '';
let terminalBoundWorkspace = '';
let pickingWorkspace = false;
let terminalHeightDrag = null;
let openingTerminal = false;
let terminalFitTimer = 0;

const TERMINAL_HEIGHT_VH_MIN = 0.16;
const TERMINAL_HEIGHT_VH_MAX = 0.72;
const TERMINAL_HEIGHT_VH_DEFAULT = 0.25;
const MAX_LIVE_TERMINALS = 8;
const TERM_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/></svg>';

function workspaceRootValue() {
  return sessionWorkspaceRoot();
}

function clampTerminalHeightVh(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return TERMINAL_HEIGHT_VH_DEFAULT;
  return Math.min(TERMINAL_HEIGHT_VH_MAX, Math.max(TERMINAL_HEIGHT_VH_MIN, n));
}

function applyTerminalHeight(vh) {
  const next = clampTerminalHeightVh(vh != null ? vh : settings.terminalHeightVh);
  const panel = document.getElementById('chatTerminal');
  if (panel) panel.style.setProperty('--terminal-h', (next * 100) + 'vh');
  scheduleTerminalFit();
}

function persistTerminalHeight(vh) {
  const next = clampTerminalHeightVh(vh);
  applyTerminalHeight(next);
  if (settings.terminalHeightVh === next) return;
  saveSettings({ ...settings, terminalHeightVh: next }, { immediate: false });
}

function beginTerminalResize(event) {
  if (event.pointerType === 'mouse' && event.button !== 0) return;
  const pane = document.querySelector('.chat-terminal-pane');
  const handle = document.getElementById('chatTerminalResize');
  if (!pane || !handle || !terminalOpen) return;
  event.preventDefault();
  terminalHeightDrag = {
    pointerId: event.pointerId,
    startY: event.clientY,
    startHeight: pane.getBoundingClientRect().height,
  };
  chatShell.classList.add('is-terminal-resizing');
  try { handle.setPointerCapture(event.pointerId); } catch { /* ignore */ }
  window.addEventListener('pointermove', moveTerminalResize);
  window.addEventListener('pointerup', endTerminalResize);
  window.addEventListener('pointercancel', endTerminalResize);
}

function moveTerminalResize(event) {
  if (!terminalHeightDrag) return;
  const delta = terminalHeightDrag.startY - event.clientY;
  const vh = (terminalHeightDrag.startHeight + delta) / Math.max(1, window.innerHeight);
  applyTerminalHeight(vh);
}

function endTerminalResize(event) {
  if (!terminalHeightDrag) return;
  const handle = document.getElementById('chatTerminalResize');
  if (event && event.pointerId != null && handle) {
    try { handle.releasePointerCapture(event.pointerId); } catch { /* ignore */ }
  }
  const pane = document.querySelector('.chat-terminal-pane');
  const vh = pane
    ? pane.getBoundingClientRect().height / Math.max(1, window.innerHeight)
    : settings.terminalHeightVh;
  terminalHeightDrag = null;
  window.removeEventListener('pointermove', moveTerminalResize);
  window.removeEventListener('pointerup', endTerminalResize);
  window.removeEventListener('pointercancel', endTerminalResize);
  chatShell.classList.remove('is-terminal-resizing');
  persistTerminalHeight(vh);
}

function isTerminalOpen() {
  return terminalOpen;
}

function paintWorkspaceField(value, { force = false } = {}) {
  const el = document.getElementById('chatTerminalCwd');
  if (el && (force || document.activeElement !== el)) {
    el.value = value || '';
    el.title = value || 'Folder for this chat';
  }
  const tab = getActiveTab();
  if (tab && tab.cwd) setTerminalCwd(tab.cwd);
  syncWorkspaceButton();
}

function syncTerminalButton() {
  const btn = document.getElementById('btnTerminal');
  if (!btn) return;
  btn.setAttribute('aria-pressed', terminalOpen ? 'true' : 'false');
  btn.classList.toggle('is-active', terminalOpen);
  btn.title = terminalOpen ? 'Hide terminal' : 'Open terminal';
  btn.setAttribute('aria-label', btn.title);
}

function syncWorkspaceButton() {
  const btn = document.getElementById('btnWorkspace');
  if (!btn) return;
  const workspace = workspaceRootValue();
  const name = workspaceFolderName(workspace);
  btn.classList.toggle('is-active', !!workspace);
  btn.setAttribute('aria-pressed', workspace ? 'true' : 'false');
  const title = workspace
    ? 'Workspace · ' + workspace + ' (click to change)'
    : 'Choose workspace folder';
  btn.title = title;
  btn.setAttribute('aria-label', workspace ? ('Workspace folder: ' + name) : 'Choose workspace folder');
}

function setTerminalOpen(open) {
  const next = !!open;
  terminalOpen = next;
  chatShell.classList.toggle('is-terminal-open', terminalOpen);
  const panel = document.getElementById('chatTerminal');
  if (panel) {
    panel.inert = !terminalOpen;
    panel.setAttribute('aria-hidden', terminalOpen ? 'false' : 'true');
  }
  syncTerminalButton();
  if (terminalOpen) {
    paintWorkspaceField(workspaceRootValue(), { force: true });
    void ensureTerminalSession();
    const focusTarget = () => {
      if (!terminalOpen) return;
      const workspace = workspaceRootValue();
      if (!workspace) document.getElementById('chatTerminalCwd')?.focus();
      else focusActiveTerminal();
    };
    if (prefersReducedMotion()) {
      scheduleTerminalFit();
      focusTarget();
    } else {
      window.setTimeout(() => {
        scheduleTerminalFit();
        focusTarget();
      }, 280);
    }
  }
}

function toggleTerminalPanel() {
  setTerminalOpen(!terminalOpen);
}

function setTerminalCwd(cwd) {
  const live = String(cwd || '').trim();
  if (live) {
    const el = document.getElementById('chatTerminalCwd');
    if (el) el.title = live;
  }
}

function commitWorkspaceField() {
  const el = document.getElementById('chatTerminalCwd');
  if (!el) return;
  setSessionWorkspaceRoot(el.value);
}

async function pickSessionWorkspace() {
  if (pickingWorkspace) return;
  pickingWorkspace = true;
  try {
    const response = await fetch('/api/workspace/pick', { method: 'POST' });
    const payload = await response.json().catch(() => null);
    if (!response.ok) {
      throw new Error((payload && payload.error) || 'Could not choose a folder');
    }
    if (payload && payload.path) {
      setSessionWorkspaceRoot(String(payload.path));
      return;
    }
  } catch (error) {
    if (!terminalOpen) setTerminalOpen(true);
    showTerminalNotice(error?.message || 'Could not choose a folder. Paste an absolute path instead.');
    document.getElementById('chatTerminalCwd')?.focus();
  } finally {
    pickingWorkspace = false;
  }
}

function onSessionWorkspaceChanged(changed) {
  const workspace = workspaceRootValue();
  paintWorkspaceField(workspace);
  if (!changed) return;
  void reopenTerminalForWorkspace();
}

function bindTerminalToSession() {
  const workspace = workspaceRootValue();
  paintWorkspaceField(workspace, { force: true });
  if (terminalBoundWorkspace === workspace && terminalTabs.length) {
    renderTerminalRail();
    renderActiveTerminalBody();
    return;
  }
  void reopenTerminalForWorkspace();
}

async function reopenTerminalForWorkspace() {
  await closeAllTerminalSessions();
  if (terminalOpen) await ensureTerminalSession();
}

function getActiveTab() {
  return terminalTabs.find((tab) => tab.id === activeTerminalId) || terminalTabs[0] || null;
}

function terminalTheme() {
  const light = document.documentElement.dataset.theme === 'light';
  if (light) {
    return {
      background: '#f3f4f6',
      foreground: '#1c1f26',
      cursor: '#2563eb',
      cursorAccent: '#f3f4f6',
      selectionBackground: '#c9d4e8',
    };
  }
  return {
    background: '#12141a',
    foreground: '#e8eaef',
    cursor: '#7eb6ff',
    cursorAccent: '#12141a',
    selectionBackground: '#3a4558',
  };
}

function refreshTerminalThemes() {
  const theme = terminalTheme();
  for (const tab of terminalTabs) {
    if (tab.term) tab.term.options.theme = theme;
  }
}

function makeTerminalTab({ id, title, cwd }) {
  return {
    id: String(id || ''),
    title: String(title || 'shell'),
    cwd: String(cwd || ''),
    term: null,
    fit: null,
    ws: null,
    viewport: null,
    encoder: new TextEncoder(),
  };
}

function xtermCtor() {
  return window.Terminal;
}

function fitAddonCtor() {
  const addon = window.FitAddon;
  if (!addon) return null;
  return addon.FitAddon || addon;
}

function createTabTerminal(tab) {
  const host = document.getElementById('chatTerminalViewports');
  const Terminal = xtermCtor();
  const FitAddon = fitAddonCtor();
  if (!host || !Terminal || !FitAddon) return false;
  const viewport = document.createElement('div');
  viewport.className = 'term-viewport';
  viewport.dataset.termId = tab.id;
  host.appendChild(viewport);
  const term = new Terminal({
    cursorBlink: true,
    cursorStyle: 'bar',
    fontSize: 13,
    lineHeight: 1.25,
    fontFamily: 'ui-monospace, "Cascadia Code", "Cascadia Mono", Consolas, "SF Mono", Menlo, monospace',
    theme: terminalTheme(),
    scrollback: 8000,
    convertEol: false,
    windowsMode: /Windows/i.test(navigator.userAgent),
    allowTransparency: false,
  });
  const fit = new FitAddon();
  term.loadAddon(fit);
  term.open(viewport);
  term.onData((data) => {
    if (tab.ws && tab.ws.readyState === WebSocket.OPEN) {
      tab.ws.send(tab.encoder.encode(data));
    }
  });
  term.onResize(({ cols, rows }) => {
    sendPtyResize(tab, cols, rows);
  });
  tab.viewport = viewport;
  tab.term = term;
  tab.fit = fit;
  return true;
}

function sendPtyResize(tab, cols, rows) {
  if (!tab?.ws || tab.ws.readyState !== WebSocket.OPEN) return;
  tab.ws.send(JSON.stringify({
    type: 'resize',
    cols: Math.max(20, cols | 0),
    rows: Math.max(4, rows | 0),
  }));
}

function fitTab(tab) {
  if (!tab?.fit || !tab.term || !tab.viewport) return;
  if (tab.viewport.classList.contains('is-hidden')) return;
  const rect = tab.viewport.getBoundingClientRect();
  if (rect.width < 8 || rect.height < 8) return;
  try {
    tab.fit.fit();
  } catch {
    // xterm can throw if the renderer isn't ready yet
  }
}

function scheduleTerminalFit() {
  if (terminalFitTimer) window.clearTimeout(terminalFitTimer);
  terminalFitTimer = window.setTimeout(() => {
    terminalFitTimer = 0;
    fitTab(getActiveTab());
  }, 40);
}

function focusActiveTerminal() {
  const tab = getActiveTab();
  tab?.term?.focus();
}

function connectTabSocket(tab) {
  if (!tab?.id) return;
  if (tab.ws && (tab.ws.readyState === WebSocket.OPEN || tab.ws.readyState === WebSocket.CONNECTING)) {
    return;
  }
  const proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  const ws = new WebSocket(proto + '//' + location.host + '/api/terminal/ws/' + encodeURIComponent(tab.id));
  ws.binaryType = 'arraybuffer';
  tab.ws = ws;
  ws.addEventListener('open', () => {
    if (tab.term) sendPtyResize(tab, tab.term.cols, tab.term.rows);
  });
  ws.addEventListener('message', (event) => {
    if (!tab.term) return;
    if (event.data instanceof ArrayBuffer) {
      tab.term.write(new Uint8Array(event.data));
      return;
    }
    if (typeof event.data === 'string') {
      tab.term.write(event.data);
    }
  });
  ws.addEventListener('close', () => {
    if (tab.ws === ws) tab.ws = null;
  });
}

function disposeTab(tab) {
  if (!tab) return;
  try { tab.ws?.close(); } catch { /* ignore */ }
  tab.ws = null;
  try { tab.term?.dispose(); } catch { /* ignore */ }
  tab.term = null;
  tab.fit = null;
  tab.viewport?.remove();
  tab.viewport = null;
}

function renderTerminalRail() {
  const list = document.getElementById('chatTerminalSessions');
  if (!list) return;
  list.replaceChildren();
  for (const tab of terminalTabs) {
    const item = document.createElement('li');
    item.className = 'term-session' + (tab.id === activeTerminalId ? ' is-active' : '');
    const select = document.createElement('button');
    select.type = 'button';
    select.className = 'term-session-select';
    select.title = tab.title;
    if (tab.id === activeTerminalId) select.setAttribute('aria-current', 'true');
    select.innerHTML = TERM_ICON + '<span></span>';
    const label = select.querySelector('span');
    if (label) label.textContent = tab.title;
    select.addEventListener('click', () => selectTerminalTab(tab.id));
    const close = document.createElement('button');
    close.type = 'button';
    close.className = 'term-session-close';
    close.setAttribute('aria-label', 'Close ' + tab.title);
    close.title = 'Close';
    close.textContent = '×';
    close.addEventListener('click', (event) => {
      event.preventDefault();
      event.stopPropagation();
      void closeTerminalTab(tab.id);
    });
    item.appendChild(select);
    item.appendChild(close);
    list.appendChild(item);
  }
}

function renderActiveTerminalBody() {
  const empty = document.getElementById('chatTerminalEmpty');
  const workspace = workspaceRootValue();
  const tab = getActiveTab();
  for (const item of terminalTabs) {
    item.viewport?.classList.toggle('is-hidden', item.id !== activeTerminalId);
  }
  if (!workspace) {
    if (empty) {
      empty.classList.remove('is-hidden');
      empty.textContent = 'Choose a folder for this chat. The model and terminal stay inside it.';
    }
    return;
  }
  if (!tab) {
    if (empty) {
      empty.classList.remove('is-hidden');
      empty.textContent = 'Use + to start a session.';
    }
    return;
  }
  empty?.classList.add('is-hidden');
  setTerminalCwd(tab.cwd);
  fitTab(tab);
}

function selectTerminalTab(id) {
  const tab = terminalTabs.find((item) => item.id === id);
  if (!tab) return;
  activeTerminalId = tab.id;
  renderTerminalRail();
  renderActiveTerminalBody();
  tab.term?.focus();
}

function showTerminalNotice(text) {
  const empty = document.getElementById('chatTerminalEmpty');
  const tab = getActiveTab();
  if (tab?.term) {
    tab.term.write('\r\n' + String(text || '') + '\r\n');
    return;
  }
  if (empty) {
    empty.classList.remove('is-hidden');
    empty.textContent = text || '';
  }
}

async function closeAllTerminalSessions() {
  const ids = terminalTabs.map((tab) => tab.id).filter(Boolean);
  for (const tab of terminalTabs) disposeTab(tab);
  terminalTabs = [];
  activeTerminalId = '';
  terminalBoundWorkspace = '';
  renderTerminalRail();
  renderActiveTerminalBody();
  await Promise.all(ids.map((id) => fetch('/api/terminal/close', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ id }),
  }).catch(() => null)));
}

async function closeTerminalTab(id) {
  const index = terminalTabs.findIndex((tab) => tab.id === id);
  if (index < 0) return;
  const wasActive = activeTerminalId === id;
  const [tab] = terminalTabs.splice(index, 1);
  disposeTab(tab);
  try {
    await fetch('/api/terminal/close', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id }),
    });
  } catch {
    // session is local; ignore
  }
  if (wasActive) {
    activeTerminalId = terminalTabs[Math.max(0, index - 1)]?.id || terminalTabs[0]?.id || '';
  }
  if (!terminalTabs.length && terminalOpen && workspaceRootValue()) {
    await addTerminalSession();
    return;
  }
  if (!terminalTabs.length) terminalBoundWorkspace = '';
  renderTerminalRail();
  renderActiveTerminalBody();
}

async function addTerminalSession() {
  const workspace = workspaceRootValue();
  paintWorkspaceField(workspace);
  if (!workspace) {
    showTerminalNotice('Choose a folder for this chat. The model and terminal stay inside it.');
    return false;
  }
  if (terminalTabs.length >= MAX_LIVE_TERMINALS) {
    showTerminalNotice('Too many terminals open. Close one first.');
    return false;
  }
  if (openingTerminal) return false;
  openingTerminal = true;
  const tab = makeTerminalTab({ id: 'pending', title: 'shell', cwd: workspace });
  try {
    if (!createTabTerminal(tab)) {
      throw new Error('Terminal emulator failed to load');
    }
    activeTerminalId = 'pending';
    terminalTabs.push(tab);
    renderTerminalRail();
    renderActiveTerminalBody();
    fitTab(tab);
    const size = {
      cols: tab.term?.cols || 80,
      rows: tab.term?.rows || 24,
    };
    const response = await fetch('/api/terminal/open', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ workspace, cols: size.cols, rows: size.rows }),
    });
    const payload = await response.json().catch(() => null);
    if (!response.ok) {
      throw new Error((payload && payload.error) || 'Could not open terminal');
    }
    tab.id = String(payload.id);
    tab.title = String(payload.title || tab.title);
    tab.cwd = String(payload.cwd || workspace);
    if (tab.viewport) tab.viewport.dataset.termId = tab.id;
    activeTerminalId = tab.id;
    terminalBoundWorkspace = workspace;
    connectTabSocket(tab);
    renderTerminalRail();
    renderActiveTerminalBody();
    tab.term?.focus();
    return true;
  } catch (error) {
    const idx = terminalTabs.indexOf(tab);
    if (idx >= 0) terminalTabs.splice(idx, 1);
    disposeTab(tab);
    activeTerminalId = terminalTabs[0]?.id || '';
    renderTerminalRail();
    renderActiveTerminalBody();
    showTerminalNotice(error?.message || 'Could not open terminal');
    return false;
  } finally {
    openingTerminal = false;
  }
}

async function ensureTerminalSession() {
  const workspace = workspaceRootValue();
  paintWorkspaceField(workspace);
  if (!workspace) {
    showTerminalNotice('Choose a folder for this chat. The model and terminal stay inside it.');
    return false;
  }
  if (terminalTabs.length && terminalBoundWorkspace === workspace) {
    renderTerminalRail();
    renderActiveTerminalBody();
    return true;
  }
  if (terminalTabs.length) await closeAllTerminalSessions();
  return addTerminalSession();
}

function onAgentTerminalEvent(_payload) {
  // Agent command sessions are separate from the user's PTY. Results appear in Activity.
}

function initTerminalPanel() {
  const cwd = document.getElementById('chatTerminalCwd');
  document.getElementById('btnTerminal')?.addEventListener('click', () => {
    toggleTerminalPanel();
  });
  document.getElementById('btnWorkspace')?.addEventListener('click', () => {
    void pickSessionWorkspace();
  });
  document.getElementById('btnWorkspace')?.addEventListener('contextmenu', (event) => {
    event.preventDefault();
    setSessionWorkspaceRoot('');
  });
  document.getElementById('btnTerminalClose')?.addEventListener('click', () => {
    setTerminalOpen(false);
  });
  document.getElementById('btnTerminalNew')?.addEventListener('click', () => {
    void addTerminalSession();
  });
  document.getElementById('btnTerminalBrowse')?.addEventListener('click', () => {
    void pickSessionWorkspace();
  });
  document.getElementById('chatTerminalResize')?.addEventListener('pointerdown', beginTerminalResize);
  document.getElementById('chatTerminalResize')?.addEventListener('dblclick', () => {
    persistTerminalHeight(TERMINAL_HEIGHT_VH_DEFAULT);
  });
  applyTerminalHeight();
  cwd?.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      commitWorkspaceField();
      focusActiveTerminal();
    }
  });
  cwd?.addEventListener('blur', () => {
    commitWorkspaceField();
  });
  const viewports = document.getElementById('chatTerminalViewports');
  if (viewports && typeof ResizeObserver === 'function') {
    const observer = new ResizeObserver(() => scheduleTerminalFit());
    observer.observe(viewports);
  }
  window.addEventListener('resize', scheduleTerminalFit);
  paintWorkspaceField(workspaceRootValue(), { force: true });
}

initTerminalPanel();
