const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

function load(state, file, names) {
  const source = readFileSync(join(__dirname, '../src/ui/chat/scripts', file), 'utf8').replace(/\r\n/g, '\n');
  for (const name of names) {
    const match = source.match(new RegExp('^(?:async )?function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
    assert.ok(match, name);
    vm.runInContext(match[0], state);
  }
}

function harness() {
  const controls = [{ disabled: false }, { disabled: false }];
  const status = {};
  const send = {};
  const row = {
    dataset: {}, classList: { remove() {}, contains: () => false },
    querySelectorAll: () => controls,
    querySelector: (selector) => selector === '.msg-edit-status' ? status : send,
  };
  let settleCancel;
  const cancelled = new Promise((resolve) => { settleCancel = resolve; });
  const starts = [];
  const hints = [];
  const renders = [];
  const convo = { id: 'chat-1', messages: [{ role: 'user', content: 'original' }, { role: 'assistant', content: 'old answer' }] };
  const state = vm.createContext({
    conversations: [convo], activeId: convo.id, serverReady: true,
    activeStreams: new Map([[convo.id, {}]]), outboundStarting: new Set(), outboundStartEpochs: new Map(),
    editingRow: row, composerInput: {}, composerMentionIds: [],
    resolveUserMessageIndex: () => 0,
    syncComposerStreamUi() {}, updateSendEnabled() {}, closeMentionMenu() {},
    showComposerHint: (text) => hints.push(text),
    parseCapabilityMentions: (text) => ({ text, mentions: [] }),
    resolveTurnSkills: () => ({ skills: {} }), displayTextWithMentions: (text) => text,
    copyReplyFields() {}, provisionalTitle: (text) => text,
    saveConversations() {}, renderSidebar() {}, renderThread: (c) => renders.push(c.id),
    buildUserApiContent: (text) => text,
    waitForCancel: () => cancelled,
    runAssistantTurn: async (c, options) => {
      starts.push(options);
      state.clearOutboundStarting(c.id);
      return true;
    },
  });
  load(state, 'controls.js', ['markOutboundStarting', 'clearOutboundStarting', 'outboundStartIsCurrent', 'invalidateOutboundStart', 'isConvoBusy']);
  state.abortStream = (id) => {
    state.activeStreams.delete(id);
    state.invalidateOutboundStart(id);
    return cancelled;
  };
  load(state, 'render.js', ['submitEditedMessage']);
  return { state, row, controls, status, starts, hints, renders, convo, settleCancel };
}

test('live edit shows restart feedback and sends the edited prompt only once after cancellation', async () => {
  const h = harness();
  const pending = h.state.submitEditedMessage(h.row, 'edited');
  assert.ok(h.controls.every((c) => c.disabled));
  assert.match(h.status.textContent, /Stopping/);
  await h.state.submitEditedMessage(h.row, 'duplicate');
  assert.equal(h.starts.length, 0);
  h.settleCancel();
  await pending;
  assert.equal(h.starts.length, 1);
  assert.equal(h.starts[0].text, 'edited');
  assert.equal(h.starts[0].replaceLive, true);
  assert.equal(h.convo.messages.length, 1);
  assert.equal(h.convo.messages[0].content, 'edited');
  assert.equal(h.state.outboundStarting.size, 0);
});

test('Stop during edit cancellation prevents restart and restores the editor', async () => {
  const h = harness();
  const pending = h.state.submitEditedMessage(h.row, 'edited');
  h.state.invalidateOutboundStart(h.convo.id);
  h.settleCancel();
  await pending;
  assert.equal(h.starts.length, 0);
  assert.equal(h.convo.messages[0].content, 'original');
  assert.equal(h.state.editingRow, h.row);
  assert.ok(h.controls.every((c) => !c.disabled));
  assert.equal(h.row.dataset.editSubmitting, undefined);
});

test('edit preparation errors release startup state and retain a retryable draft', async () => {
  const h = harness();
  h.state.parseCapabilityMentions = () => { throw new Error('prepare failed'); };
  h.settleCancel();
  await h.state.submitEditedMessage(h.row, 'edited');
  assert.equal(h.state.outboundStarting.size, 0);
  assert.equal(h.state.editingRow, h.row);
  assert.ok(h.controls.every((c) => !c.disabled));
  assert.match(h.hints[0], /try again/);
});

test('switching chats during cancellation does not render the edited chat over the new chat', async () => {
  const h = harness();
  const pending = h.state.submitEditedMessage(h.row, 'edited');
  const otherEditor = {};
  h.state.activeId = 'chat-2';
  h.state.editingRow = otherEditor;
  h.settleCancel();
  await pending;
  assert.deepEqual(h.renders, []);
  assert.equal(h.state.editingRow, otherEditor);
  assert.equal(h.starts.length, 1);
});

test('a failed restart preserves the edited prompt instead of restoring stale replies', async () => {
  const h = harness();
  h.state.runAssistantTurn = async () => {
    h.state.clearOutboundStarting(h.convo.id);
    return false;
  };
  h.settleCancel();
  await h.state.submitEditedMessage(h.row, 'edited');
  assert.equal(h.convo.messages.length, 1);
  assert.equal(h.convo.messages[0].content, 'edited');
  assert.match(h.hints[0], /edited message is saved/);
});

test('composer presents an edit as restart, not queue, and disables duplicate sends', () => {
  const h = harness();
  const classes = new Set(['is-queueing']);
  const attributes = {};
  Object.assign(h.state, {
    composerInput: { value: '' }, pendingAttachments: [], pendingReplyQuote: '',
    selectedChatModel: 'model', selectedModelIsReady: () => true,
    diskEncryptionLocked: () => false, btnBranch: null,
    btnSend: {
      classList: { toggle(name, on) { if (on) classes.add(name); else classes.delete(name); } },
      setAttribute(name, value) { attributes[name] = value; },
    },
    modelIdLabel: (id) => id,
  });
  load(h.state, 'runtime.js', ['updateSendEnabled']);
  h.state.updateSendEnabled();
  assert.equal(classes.has('is-queueing'), false);
  assert.equal(attributes['aria-label'], 'Send edit and restart response');
  assert.equal(h.state.btnSend.disabled, false);
  h.row.dataset.editSubmitting = 'true';
  h.state.updateSendEnabled();
  assert.equal(h.state.btnSend.disabled, true);
  assert.equal(attributes['aria-label'], 'Restarting response…');
});
