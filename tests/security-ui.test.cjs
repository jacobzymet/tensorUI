// NODE_PATH can point to an installed Playwright package; no app build is needed.
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');
const root = join(__dirname, '..');

test('bundled OCR assets match the reviewed hashes', () => {
  const { createHash } = require('node:crypto');
  const dir = join(root, 'src/ui/vendor/ocr');
  const hashes = JSON.parse(readFileSync(join(dir, 'sha256.json'), 'utf8'));
  for (const [name, hash] of Object.entries(hashes)) {
    assert.equal(createHash('sha256').update(readFileSync(join(dir, name))).digest('hex'), hash, name);
  }
});

test('locking terminates OCR and releases a pending recognition', async () => {
  let started;
  const began = new Promise(resolve => { started = resolve; });
  let terminated = 0;
  const worker = { recognize() { started(); return new Promise(() => {}); }, async terminate() { terminated++; } };
  const context = vm.createContext({ AbortController, URL, location: { origin: 'http://localhost' },
    ensureTesseract: async () => ({ createWorker: async () => worker }),
  });
  vm.runInContext('let ocrEpoch = 0; let ocrAbortController = new AbortController(); const ocrWorkers = new Set();', context);
  vm.runInContext(declaration('input.js', 'cancelOcrWorkers'), context);
  vm.runInContext(declaration('input.js', 'ocrAttachmentImage'), context);
  const result = context.ocrAttachmentImage({ dataUrl: 'private image' });
  await began;
  context.cancelOcrWorkers();
  await assert.rejects(result, /OCR cancelled/);
  assert.ok(terminated > 0);
});

function declaration(file, name) {
  const source = readFileSync(join(root, 'src/ui/chat/scripts', file), 'utf8').replace(/\r\n/g, '\n');
  const match = source.match(new RegExp('^(?:async )?function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, name);
  return match[0];
}

test('locking removes drafts, attachments, rendered content, images, and terminal state', () => {
  const cleared = [];
  const state = vm.createContext({
    storageWriteEpoch: 0, saveStoreTimer: 1, saveSettingsTimer: 2, clearTimeout() {},
    activeStreams: new Map([['chat', {}]]), outboundQueues: new Map([['chat', []]]),
    stickByConvo: new Map(), composerMentionIds: new Set(['research']),
    composerInput: { value: 'private draft' }, activeId: 'chat', latestState: { private: true },
    draftWorkspaceRoot: 'private folder', DEFAULT_PROFILE_ID: 'personal', DEFAULT_SETTINGS: {},
    abortAllStreams() {}, clearTerminalMemory: () => cleared.push('terminal'),
    setTerminalOpen: (open) => { assert.equal(open, false); }, stopVoiceInput() {}, cancelMessageEdit() {},
    clearPendingAttachments: () => cleared.push('attachments'),
    clearPendingReplyQuote: () => cleared.push('quote'),
    setMarkdownImages: (value) => { assert.equal(value, null); cleared.push('images'); },
    chatThread: { replaceChildren: () => cleared.push('chat') },
    traceSidebarBody: { replaceChildren: () => cleared.push('trace') },
    refreshUiFromMemoryStore() {}, promptUnlockSession() {},
  });
  vm.runInContext(declaration('state.js', 'clearMemoryAfterLock'), state);
  state.clearMemoryAfterLock();
  assert.deepEqual(cleared.sort(), ['attachments', 'chat', 'images', 'quote', 'terminal', 'trace']);
  assert.equal(state.composerInput.value, '');
  assert.equal(state.latestState, null);
  assert.equal(state.activeId, null);
  assert.equal(state.draftWorkspaceRoot, '');
  assert.equal(state.conversations.length, 0);
  assert.equal(state.composerMentionIds.size, 0);
});

test('a terminal response arriving after lock cannot recreate a terminal', async () => {
  let respond;
  const disposed = [];
  const state = vm.createContext({
    terminalTabs: [], activeTerminalId: '', terminalBoundWorkspace: '', openingTerminal: false,
    terminalSessionEpoch: 0, MAX_LIVE_TERMINALS: 8,
    workspaceRootValue: () => 'workspace', paintWorkspaceField() {},
    makeTerminalTab: (tab) => tab, createTabTerminal: () => true, fitTab() {},
    renderTerminalRail() {}, renderActiveTerminalBody() {},
    fetch: () => new Promise((resolve) => { respond = resolve; }),
    disposeTab: (tab) => disposed.push(tab),
    connectTabSocket: () => assert.fail('old session must not reconnect'),
    showTerminalNotice: () => assert.fail('old response must not redraw locked UI'),
  });
  for (const name of ['addTerminalSession', 'clearTerminalMemory']) {
    vm.runInContext(declaration('terminal.js', name), state);
  }
  const pending = state.addTerminalSession();
  state.clearTerminalMemory();
  respond({ ok: true, json: async () => ({ id: 'old-terminal' }) });
  assert.equal(await pending, false);
  assert.equal(state.terminalTabs.length, 0);
  assert.equal(disposed.length, 1);
  assert.equal(state.activeTerminalId, '');
});

let chromium;
try { ({ chromium } = require('playwright')); } catch { /* Explicitly report missing browser coverage. */ }
test('actual vendored sanitizer blocks executable and UI-clobbering model HTML', {
  skip: !chromium && 'Playwright is required for the browser security test',
}, async () => {
  const browser = await chromium.launch({
    headless: true,
    ...(process.env.SECURITY_BROWSER_CHANNEL ? { channel: process.env.SECURITY_BROWSER_CHANNEL } : {}),
  });
  try {
    const page = await browser.newPage();
    await page.route('**/*', (route) => route.abort());
    await page.addScriptTag({ path: join(root, 'src/ui/vendor/purify.min.js') });
    await page.addScriptTag({ path: join(root, 'src/ui/vendor/marked.min.js') });
    await page.addScriptTag({ content: 'function applyMarkdownImageRefs(text) { return text; }\n' + declaration('render.js', 'renderMarkdown') });
    const result = await page.evaluate(() => {
      const attacks = [
        '<img src=x onerror="window.__xss=true"><script>window.__xss=true</script>',
        '[click](javascript:alert(1))<iframe srcdoc="<script>alert(1)</script>"></iframe>',
        '<style>body{display:none}</style><form id="composerInput"><input name="value"></form><p style="position:fixed" id="chatThread">fake UI</p>',
        '<math><mtext><table><mglyph><style><!--</style><img title="--><img src=x onerror=window.__xss=true>">',
      ];
      const host = document.createElement('div');
      document.body.append(host);
      for (const attack of attacks) {
        host.innerHTML = renderMarkdown(attack);
        for (const node of host.querySelectorAll('*')) {
          if (/^(SCRIPT|IFRAME|STYLE|FORM|OBJECT|EMBED)$/.test(node.tagName)) throw new Error('unsafe element');
          for (const attr of node.attributes) {
            if (/^on/i.test(attr.name) || /^(id|name|style)$/.test(attr.name)) throw new Error('unsafe attribute');
            if (/^(href|src)$/.test(attr.name) && /^javascript:/i.test(attr.value)) throw new Error('unsafe URL');
          }
        }
      }
      host.innerHTML = renderMarkdown('**Evidence** [source](https://example.com)');
      return { version: DOMPurify.version, bold: host.querySelector('strong')?.textContent, link: host.querySelector('a')?.href, executed: !!window.__xss };
    });
    assert.equal(result.version, '3.4.15');
    assert.equal(result.bold, 'Evidence');
    assert.equal(result.link, 'https://example.com/');
    assert.equal(result.executed, false);
  } finally {
    await browser.close();
  }
});
