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

function sendHarness() {
  let resolvePrep;
  let preparations = 0;
  const sent = [];
  const hints = [];
  const attachment = { id: 'file-1' };
  const state = vm.createContext({
    pendingComposerSend: null, editingRow: null, composerInput: { value: 'original draft' },
    pendingAttachments: [attachment], pendingReplyQuote: '', pendingReplyTarget: null,
    activeId: 'chat-1', activeProjectId: null, selectedChatModel: 'model', latestState: {},
    appSurface: 'chat', draftIncognito: false, draftWorkspaceRoot: '',
    conversations: [{ id: 'chat-1', messages: [] }, { id: 'chat-2', messages: [] }],
    composerMentionIds: new Set(), serverReady: true,
    selectedRemoteModel: () => ({ ready: true }), requireUnlockedData: () => true,
    updateSendEnabled() {}, stopVoiceInput() {}, cancelMessageEdit() {}, showChatView() {},
    showComposerHint: (text) => hints.push(text), showAttachHint: (text) => hints.push(text), focusComposer() {},
    prepareAttachmentsForSend() {
      preparations++;
      return new Promise((resolve) => { resolvePrep = resolve; });
    },
    parseCapabilityMentions: (text) => ({ text, mentions: [] }),
    resolveTurnSkills: () => ({ skills: {} }), displayTextWithMentions: (text) => text,
    storedAttachmentsFromPrepared: (files) => files, buildUserApiContent: (text) => text,
    clearPendingAttachments() { state.pendingAttachments = []; }, clearPendingReplyQuote() {},
    autoResize() {}, renderComposerMentions() {}, renderComposerModes() {}, closeMentionMenu() {},
    newId: () => 'outbound-1', isConvoBusy: () => false, getOutboundQueue: () => [],
    dispatchOutboundTurn: (convo, item) => sent.push({ id: convo.id, item }),
  });
  load(state, 'runtime.js', ['composerSendIsCurrent', 'sendMessage']);
  return { state, sent, hints, attachment, finish: () => resolvePrep([attachment]), preparations: () => preparations };
}

test('double Send during attachment preparation dispatches once', async () => {
  const h = sendHarness();
  const pending = h.state.sendMessage();
  await h.state.sendMessage();
  assert.equal(h.preparations(), 1);
  h.finish();
  await pending;
  assert.equal(h.sent.length, 1);
  assert.equal(h.sent[0].id, 'chat-1');
  assert.equal(h.state.pendingComposerSend, null);
});

for (const [name, mutate] of [
  ['switch chats', (s) => { s.activeId = 'chat-2'; }],
  ['change text', (s) => { s.composerInput.value = 'new draft'; }],
  ['add an attachment', (s) => { s.pendingAttachments.push({ id: 'new-file' }); }],
  ['change models', (s) => { s.selectedChatModel = 'other-model'; }],
  ['change privacy mode', (s) => { s.draftIncognito = true; }],
  ['change workspace', (s) => { s.draftWorkspaceRoot = 'another-workspace'; }],
  ['delete the destination', (s) => { s.conversations.shift(); }],
]) {
  test('attachment preparation cannot send or clear the draft after ' + name, async () => {
    const h = sendHarness();
    const pending = h.state.sendMessage();
    mutate(h.state);
    const draft = h.state.composerInput.value;
    const files = h.state.pendingAttachments.slice();
    h.finish();
    await pending;
    assert.equal(h.sent.length, 0);
    assert.equal(h.state.composerInput.value, draft);
    assert.deepEqual(h.state.pendingAttachments, files);
    assert.equal(h.state.pendingComposerSend, null);
  });
}

test('empty inline edit never falls through to sending the unrelated composer draft', async () => {
  const h = sendHarness();
  h.state.editingRow = { classList: { contains: () => false }, querySelector: () => ({ value: '' }) };
  h.state.submitEditedMessage = () => assert.fail('empty edit');
  await h.state.sendMessage();
  assert.equal(h.preparations(), 0);
  assert.equal(h.state.composerInput.value, 'original draft');
});

test('dropping a disconnected subscriber removes its loading row without touching a replacement', () => {
  const removed = [];
  const old = {};
  const current = {};
  const state = vm.createContext({
    activeStreams: new Map([['chat-1', current]]),
    discardLiveStreamRow: (stream) => removed.push(stream), renderSidebar() {}, syncComposerStreamUi() {},
  });
  load(state, 'runtime.js', ['dropLiveSubscriber']);
  state.dropLiveSubscriber('chat-1', old);
  assert.equal(removed.length, 0);
  state.dropLiveSubscriber('chat-1', current);
  assert.deepEqual(removed, [current]);
  assert.equal(state.activeStreams.size, 0);
});

function conflictHarness() {
  const user = { role: 'user', content: 'queued prompt' };
  const convo = { id: 'chat-1', messages: [user] };
  const queue = [];
  const events = [];
  const fetchQueue = [];
  const state = vm.createContext({
    activeStreams: new Map(), outboundStarting: new Set(), outboundStartEpochs: new Map(),
    latestState: {}, selectedChatModel: 'model', thinkingSupported: false,
    activeId: convo.id, selectedRemoteModel: () => ({ ready: true, model: 'model' }),
    settings: {},
    DEFAULT_SETTINGS: {},
    WEB_SEARCH_DEPTHS: [],
    WEB_SEARCH_PROVIDERS: [],
    WEB_SEARCH_PARALLEL_MODES: [],
    WEB_SEARCH_SAFESEARCH: [],
    WEB_SEARCH_RECENCIES: [],
    APPROVAL_MODES: ['manual'],
    sessionWorkspaceRoot: () => '',
    Promise, AbortController,
    newId: (prefix) => prefix + '-test',
    syncComposerStreamUi() {}, renderSidebar() {}, resetTraceAutoOpenState() {}, syncStreamSpeakerChrome() {},
    beginLiveStream(c) {
      const stream = { controller: new AbortController(), cancelled: false, replaced: false };
      state.activeStreams.set(c.id, stream);
      return stream;
    },
    userMessageApiContent: (message) => message.content, buildSystemPrompt: () => '',
    whenAborted(signal) {
      if (signal?.aborted) return Promise.resolve();
      return new Promise(() => {});
    },
    fetch: () => new Promise((resolve) => { fetchQueue.push(resolve); }),
    scheduleCancel: async () => { events.push('cancel'); return true; },
    discardLiveStreamRow: () => events.push('discard'),
    getOutboundQueue: () => queue, saveConversations() {}, renderOutboundQueue() {}, updateComposerHint() {},
    renderThread() { events.push('render'); },
    attachLiveTurn: async () => events.push('attach'),
    driveAssistantSse: async () => { events.push('sse'); },
    needsGeneratedTitle: () => false,
    generateConversationTitle() {},
    contextualModelError: (text) => text,
    settleStoppedLiveStream(c, stream) {
      stream.settled = true;
      events.push('settle');
      if (state.activeStreams.get(c.id) === stream) state.activeStreams.delete(c.id);
    },
  });
  load(state, 'controls.js', ['markOutboundStarting', 'clearOutboundStarting', 'outboundStartIsCurrent']);
  load(state, 'runtime.js', ['runAssistantTurn', 'dropLiveSubscriber']);
  const run = () => state.runAssistantTurn(convo, {
    useAgent: false, skills: {}, text: 'queued prompt', dispatchedMessage: user,
    queueItem: { id: 'queued-1' }, previousTitle: 'title',
  });
  return {
    state, queue, events, convo, run, fetchQueue,
    respond(status, extra = {}) {
      const resolve = fetchQueue.shift();
      assert.ok(resolve, 'no pending completions request');
      resolve({ ok: status === 200, status, ...extra });
    },
  };
}

test('conflict cancels the occupant and retries instead of attaching to it', async () => {
  const h = conflictHarness();
  const pending = h.run();
  await new Promise((resolve) => setImmediate(resolve));
  h.respond(409);
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(h.events, ['cancel']);
  h.respond(409);
  await pending;
  assert.equal(h.queue.length, 0);
  assert.equal(h.convo.messages.length, 1);
  assert.ok(h.events.includes('sse'));
  assert.ok(!h.events.includes('attach'));
});

test('a late conflict from a replaced request cannot mutate messages or reconnect', async () => {
  const h = conflictHarness();
  const pending = h.run();
  const replacement = {};
  h.state.activeStreams.set('chat-1', replacement);
  await new Promise((resolve) => setImmediate(resolve));
  h.respond(409);
  await pending;
  assert.equal(h.queue.length, 0);
  assert.equal(h.convo.messages.length, 1);
  assert.deepEqual(h.events, []);
  assert.equal(h.state.activeStreams.get('chat-1'), replacement);
});

test('a leftover cancelled Processing row is settled before the next prompt starts', async () => {
  const leftover = {
    cancelled: true,
    hardStopped: false,
    settled: false,
    partial: '',
    controller: { abort() {}, signal: { aborted: false, addEventListener() {} } },
    bodyReader: { cancel() {} },
  };
  const h = conflictHarness();
  h.state.activeStreams.set('chat-1', leftover);
  const pending = h.run();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(leftover.settled, true);
  assert.ok(h.events.includes('settle'));
  h.respond(200);
  await pending;
  assert.ok(h.events.includes('sse'));
});

test('editing a queued reply preserves its quoted context in the API payload', () => {
  const item = { replyQuote: 'quoted passage', replyToSpeakerHandle: 'researcher', attachments: [] };
  let payload;
  const state = vm.createContext({
    activeId: 'chat-1', findQueuedItem: () => item,
    parseCapabilityMentions: (text) => ({ text, mentions: [] }),
    resolveTurnSkills: () => ({}), displayTextWithMentions: (text) => text,
    userMessageApiContent(message) { payload = message; return 'with quote'; },
    document: { createElement: () => ({}) }, formatUserMessageHtml: () => '',
    composerInput: {}, closeMentionMenu() {}, refreshQueuedBubble() {}, updateComposerHint() {},
    persistOutboundQueues() {}, resumeOutboundQueue() {}, maybeSendNextQueued() {}, focusComposer() {},
  });
  load(state, 'controls.js', ['saveQueuedMessageEdit']);
  state.saveQueuedMessageEdit({ dataset: { queueId: 'queued-1' }, querySelector: () => null, classList: { remove() {} } }, 'edited');
  assert.equal(payload.replyQuote, 'quoted passage');
  assert.equal(payload.replyToSpeakerHandle, 'researcher');
  assert.equal(item.apiText, 'with quote');
});
