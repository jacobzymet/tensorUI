// Run without compiling the app: node --test tests/session-lifecycle.test.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const source = readFileSync(
  join(__dirname, '../src/ui/chat/scripts/controls.js'),
  'utf8'
).replace(/\r\n/g, '\n');

function declaration(name) {
  const match = source.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

const runtime = readFileSync(
  join(__dirname, '../src/ui/chat/scripts/runtime.js'), 'utf8'
).replace(/\r\n/g, '\n');

const chatState = readFileSync(
  join(__dirname, '../src/ui/chat/scripts/state.js'), 'utf8'
).replace(/\r\n/g, '\n');

function runtimeDeclaration(name) {
  const match = runtime.match(new RegExp('^(?:async )?function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

function stateDeclaration(name) {
  const match = chatState.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing state function ' + name);
  return match[0];
}

function resumeHarness() {
  const state = vm.createContext({
    activeStreams: new Map(),
    outboundStarting: new Set(),
    serverReady: true,
    storageReady: true,
    diskEncryptionLocked: () => false,
    conversations: [{ id: 'chat-1', messages: [] }],
    activeId: 'chat-1',
    selectedChatModel: 'model',
    emptyState: null,
    AbortController,
    Promise,
    renderSidebar() {},
    syncComposerStreamUi() {},
    ensureStreamDom() {},
    scrollToBottom() {},
    reclaimUnappliedSteers() {},
    maybeSendNextQueued() {},
    maybeEnsureConversationTitle() {},
    commitLiveAssistant(convo, message) {
      convo.messages.push(message);
      return message;
    },
    discardLiveStreamRow(stream) { stream.discarded = true; },
    dropLiveSubscriber(convoId, stream) {
      stream.discarded = true;
      state.activeStreams.delete(convoId);
    },
    whenAborted(signal) {
      if (!signal) return new Promise(() => {});
      if (signal.aborted) return Promise.resolve();
      return new Promise((resolve) => {
        signal.addEventListener('abort', resolve, { once: true });
      });
    },
  });
  for (const name of [
    'resumeLiveTurns',
    'attachLiveTurn',
    'beginLiveStream',
    'finishLiveStream',
    'settleStoppedLiveStream',
  ]) {
    vm.runInContext(runtimeDeclaration(name), state);
  }
  return state;
}

test('background resume cannot occupy the stream while an edited resend starts', async () => {
  const state = resumeHarness();
  state.outboundStarting.add('chat-1');
  state.fetch = () => { throw new Error('must not reconnect to the old turn'); };
  await state.resumeLiveTurns([{ conversation_id: 'chat-1', turn_id: 'old-turn' }]);
  await state.attachLiveTurn(state.conversations[0], { turn_id: 'old-turn' });
  assert.equal(state.activeStreams.size, 0);
  assert.equal(state.outboundStarting.has('chat-1'), true);
});

test('a resume already awaiting store sync yields to a newly started resend', async () => {
  const state = resumeHarness();
  state.activeId = 'another-chat';
  let finishSync;
  state.syncConvoFromStore = () => new Promise((resolve) => { finishSync = resolve; });
  state.fetch = () => { throw new Error('must not reconnect to the old turn'); };
  const pending = state.resumeLiveTurns([{ conversation_id: 'chat-1', turn_id: 'old-turn' }]);
  state.outboundStarting.add('chat-1');
  finishSync(state.conversations[0]);
  await pending;
  assert.equal(state.activeStreams.size, 0);
});

test('a vanished live turn removes its Processing row and releases the composer', async () => {
  const state = resumeHarness();
  let stream;
  state.fetch = async () => {
    stream = state.activeStreams.get('chat-1');
    return { ok: false, status: 404 };
  };
  await state.attachLiveTurn(state.conversations[0], { turn_id: 'old-turn' });
  assert.equal(stream.discarded, true);
  assert.equal(state.activeStreams.size, 0);
});

test('state polling immediately retires a Processing row the server no longer owns', async () => {
  const state = resumeHarness();
  const stream = state.beginLiveStream(state.conversations[0], { turnId: 'lost-turn' });
  await state.resumeLiveTurns([]);
  assert.equal(stream.discarded, true);
  assert.equal(stream.controller.signal.aborted, true);
  assert.equal(state.activeStreams.size, 0);
});

test('state polling keeps a Processing row while the server still owns its turn', async () => {
  const state = resumeHarness();
  const stream = state.beginLiveStream(state.conversations[0], { turnId: 'live-turn' });
  await state.resumeLiveTurns([{ conversation_id: 'chat-1', turn_id: 'live-turn' }]);
  assert.equal(stream.discarded, undefined);
  assert.equal(state.activeStreams.get('chat-1'), stream);
});

test('state polling settles a stopped reply the server no longer owns', async () => {
  const state = resumeHarness();
  const stream = state.beginLiveStream(state.conversations[0], { turnId: 'stopping-turn' });
  stream.cancelled = true;
  stream.hardStopped = false;
  stream.partial = '';
  await state.resumeLiveTurns([]);
  assert.equal(stream.discarded, true);
  assert.equal(stream.settled, true);
  assert.equal(state.activeStreams.size, 0);
});

test('state polling keeps a stopped partial while the server still owns its turn', async () => {
  const state = resumeHarness();
  const stream = state.beginLiveStream(state.conversations[0], { turnId: 'stopping-turn' });
  stream.cancelled = true;
  stream.hardStopped = false;
  await state.resumeLiveTurns([{ conversation_id: 'chat-1', turn_id: 'stopping-turn' }]);
  assert.equal(stream.discarded, undefined);
  assert.equal(state.activeStreams.get('chat-1'), stream);
});

test('a finished server turn replaces a stuck browser stream with one replay', async () => {
  const state = resumeHarness();
  state.shouldSkipLiveTurnResume = () => true;
  state.fetch = () => new Promise(() => {});
  const old = state.beginLiveStream(state.conversations[0], { turnId: 'done-turn' });
  const info = { conversation_id: 'chat-1', turn_id: 'done-turn', finished: true };
  await state.resumeLiveTurns([info]);
  const replay = state.activeStreams.get('chat-1');
  assert.notEqual(replay, old);
  assert.equal(old.controller.signal.aborted, true);
  assert.equal(replay.catchingUp, true);

  await state.resumeLiveTurns([info]);
  assert.equal(state.activeStreams.get('chat-1'), replay);
});

test('title generation waits until the main response finishes', () => {
  const turn = runtimeDeclaration('runAssistantTurn');
  const finish = runtimeDeclaration('finishLiveStream');
  assert.equal(turn.includes('maybeEnsureConversationTitle(convo)'), false);
  assert.match(finish, /maybeEnsureConversationTitle\(convo, \{ delay: 500 \}\)/);
});

test('title fallback is bounded to six useful words', () => {
  const state = vm.createContext({
    firstUserText: () => '',
  });
  vm.runInContext(stateDeclaration('fallbackConversationTitle'), state);
  assert.equal(
    state.fallbackConversationTitle(null, 'Please help me debug unreliable automatic title generation today'),
    'debug unreliable automatic title generation today'
  );
});

test('attachment-only chats provide title source text', () => {
  const state = vm.createContext({
    parseCapabilityMentions(value) {
      return { text: String(value || '').trim() };
    },
  });
  vm.runInContext(stateDeclaration('firstUserText'), state);
  const convo = {
    messages: [{
      role: 'user',
      content: '(attachment)',
      attachments: [{ name: 'report.pdf', extractedText: 'Quarterly revenue analysis' }],
    }],
  };
  assert.equal(state.firstUserText(convo), 'report.pdf: Quarterly revenue analysis');
});

test('failed title requests retry before applying a deterministic fallback', async () => {
  const timers = [];
  const convo = {
    id: 'chat-title',
    title: 'Original provisional title',
    titleStatus: 'pending',
    titleAttempts: 0,
    messages: [{ role: 'user', content: 'Please explain automatic title generation reliability' }],
  };
  const state = vm.createContext({
    conversations: [convo],
    window: {
      setTimeout(callback) {
        timers.push(callback);
        return timers.length;
      },
      clearTimeout() {},
    },
    requestGeneratedTitle: async () => { throw new Error('provider busy'); },
    firstUserText: () => convo.messages[0].content,
    needsGeneratedTitle: (item) => item.titleStatus === 'pending',
    saveConversations() {},
    revealGeneratedTitle() {},
    console: { warn() {} },
  });
  vm.runInContext(
    'const TITLE_RETRY_DELAYS = [500, 2000, 8000]; const titleRetryTimers = new Map();',
    state
  );
  for (const name of [
    'fallbackConversationTitle',
    'conversationTitleProviderBusy',
    'maybeEnsureConversationTitle',
    'generateConversationTitle',
  ]) vm.runInContext(stateDeclaration(name), state);

  state.generateConversationTitle(convo, convo.messages[0].content);
  await new Promise(setImmediate);
  assert.equal(convo.titleAttempts, 1);
  assert.equal(timers.length, 1);

  timers.shift()();
  await new Promise(setImmediate);
  assert.equal(convo.titleAttempts, 2);
  assert.equal(timers.length, 1);

  timers.shift()();
  await new Promise(setImmediate);
  assert.equal(convo.titleAttempts, 3);
  assert.equal(convo.titleStatus, 'fallback');
  assert.equal(convo.title, 'explain automatic title generation reliability');
});

test('Stop invalidates a pending start without clearing a newer attempt', () => {
  const state = vm.createContext({
    outboundStarting: new Set(),
    outboundStartEpochs: new Map(),
    syncComposerStreamUi() {},
  });
  for (const name of [
    'markOutboundStarting',
    'outboundStartIsCurrent',
    'clearOutboundStarting',
    'invalidateOutboundStart',
  ]) vm.runInContext(declaration(name), state);

  const stale = state.markOutboundStarting('chat-1');
  state.invalidateOutboundStart('chat-1');
  assert.equal(state.outboundStartIsCurrent('chat-1', stale), false);
  assert.equal(state.outboundStarting.has('chat-1'), false);

  const current = state.markOutboundStarting('chat-1');
  state.clearOutboundStarting('chat-1', stale);
  assert.equal(state.outboundStarting.has('chat-1'), true);
  state.clearOutboundStarting('chat-1', current);
  assert.equal(state.outboundStarting.has('chat-1'), false);
});

test('Stop cancels a wedged SSE reader so the finalizer can run', () => {
  let readerCancelled = false;
  const stream = {
    cancelled: false,
    hardStopped: false,
    turnId: 'turn-1',
    controller: { abort() {} },
    bodyReader: { cancel() { readerCancelled = true; } },
  };
  const state = vm.createContext({
    activeStreams: new Map([['chat-1', stream]]),
    invalidateOutboundStart() {},
    markBotsOutboundStopped() {},
    bumpBotsOutboundEpoch() {},
    rememberHandledLiveTurn() {},
    noteLiveTurnUserCancel() {},
    discardLiveStreamRow() {},
    scheduleCancel: () => Promise.resolve(),
    syncComposerStreamUi() {},
  });
  vm.runInContext(declaration('cancelLiveBodyReader'), state);
  vm.runInContext(declaration('abortStream'), state);

  state.abortStream('chat-1', { preservePartial: true });

  assert.equal(stream.cancelled, true);
  assert.equal(readerCancelled, true);
  assert.equal(stream.bodyReader, null);
  assert.equal(state.activeStreams.get('chat-1'), stream);
});

test('Stop swallows an already-aborted body reader instead of leaking AbortError', async () => {
  const rejections = [];
  const onUnhandled = (reason) => { rejections.push(reason); };
  process.on('unhandledRejection', onUnhandled);
  const stream = {
    cancelled: false,
    hardStopped: false,
    turnId: 'turn-1',
    controller: { abort() {} },
    bodyReader: {
      cancel() {
        return Promise.reject(Object.assign(new Error('BodyStreamBuffer was aborted'), {
          name: 'AbortError',
        }));
      },
    },
  };
  const state = vm.createContext({
    activeStreams: new Map([['chat-1', stream]]),
    invalidateOutboundStart() {},
    markBotsOutboundStopped() {},
    bumpBotsOutboundEpoch() {},
    rememberHandledLiveTurn() {},
    noteLiveTurnUserCancel() {},
    discardLiveStreamRow() {},
    scheduleCancel: () => Promise.resolve(),
    syncComposerStreamUi() {},
  });
  vm.runInContext(declaration('cancelLiveBodyReader'), state);
  vm.runInContext(declaration('abortStream'), state);

  state.abortStream('chat-1', { preservePartial: true });
  await Promise.resolve();
  process.off('unhandledRejection', onUnhandled);

  assert.equal(stream.cancelled, true);
  assert.equal(rejections.length, 0);
});

test('Stop leaves a live stream with its finalizer so partial text can be saved', () => {
  let discarded = false;
  let aborted = false;
  const stream = {
    cancelled: false,
    hardStopped: false,
    turnId: 'turn-1',
    controller: { abort() { aborted = true; } },
  };
  const state = vm.createContext({
    activeStreams: new Map([['chat-1', stream]]),
    invalidateOutboundStart() {},
    markBotsOutboundStopped() {},
    bumpBotsOutboundEpoch() {},
    rememberHandledLiveTurn() {},
    noteLiveTurnUserCancel() {},
    discardLiveStreamRow() { discarded = true; },
    scheduleCancel: () => Promise.resolve(),
    syncComposerStreamUi() {},
  });
  vm.runInContext(declaration('cancelLiveBodyReader'), state);
  vm.runInContext(declaration('abortStream'), state);

  state.abortStream('chat-1', { preservePartial: true });

  assert.equal(stream.cancelled, true);
  assert.equal(stream.hardStopped, false);
  assert.equal(aborted, true);
  assert.equal(discarded, false);
  assert.equal(state.activeStreams.get('chat-1'), stream);
});

test('a stopped Loop no longer keeps the composer busy during teardown', () => {
  const state = vm.createContext({
    activeStreams: new Map(),
    outboundStarting: new Set(),
    isBotsOutboundActive: () => true,
    isBotsOutboundStopped: () => true,
  });
  vm.runInContext(declaration('isConvoBusy'), state);
  assert.equal(state.isConvoBusy('loop-1'), false);
  state.isBotsOutboundStopped = () => false;
  assert.equal(state.isConvoBusy('loop-1'), true);
});

test('ending all sessions also cancels requests still in startup', () => {
  const aborted = [];
  const state = vm.createContext({
    activeStreams: new Map([['live-chat', {}]]),
    outboundStarting: new Set(['starting-chat']),
    abortStream(id, options) { aborted.push([id, options.cancelServer]); },
    stopAllBotsOutbound() {},
    Set,
  });
  vm.runInContext(declaration('abortAllStreams'), state);
  state.abortAllStreams({ cancelServer: false });
  assert.deepEqual(aborted.sort(), [
    ['live-chat', false],
    ['starting-chat', false],
  ]);
});

test('queued messages stay paused after Stop until explicitly resumed', () => {
  let dispatched = 0;
  const state = vm.createContext({
    activeId: 'chat-1',
    outboundQueues: new Map([['chat-1', [{ id: 'queued-1' }]]]),
    stoppedOutboundQueues: new Set(),
    conversations: [{ id: 'chat-1' }],
    isConvoBusy: () => false,
    isQueuePausedForEdit: () => false,
    updateComposerHint() {},
    renderOutboundQueue() {},
    dispatchOutboundTurn() { dispatched += 1; },
    persistOutboundQueues() {},
  });
  for (const name of [
    'getOutboundQueue',
    'pauseOutboundQueueAfterStop',
    'resumeOutboundQueue',
    'isOutboundQueueStopped',
    'maybeSendNextQueued',
  ]) vm.runInContext(declaration(name), state);

  state.pauseOutboundQueueAfterStop('chat-1');
  state.maybeSendNextQueued('chat-1');
  assert.equal(dispatched, 0);
  assert.equal(state.isOutboundQueueStopped('chat-1'), true);

  state.resumeOutboundQueue('chat-1');
  state.maybeSendNextQueued('chat-1');
  assert.equal(dispatched, 1);
});

test('server cancellation is complete only after teardown is settled', async () => {
  const state = vm.createContext({
    cancelInFlight: new Map(), AbortController, setTimeout, clearTimeout,
    fetch: async () => ({
      ok: true,
      json: async () => ({ ok: true, cancelled: true, settled: true }),
    }),
    Promise, JSON,
  });
  for (const name of ['scheduleCancel', 'waitForCancel']) {
    vm.runInContext(declaration(name), state);
  }
  assert.equal(await state.scheduleCancel('chat-1', 'turn-1'), true);
  await state.waitForCancel('chat-1');
});

test('server cancellation is bounded and carries an abort signal', async () => {
  let timeoutMs = 0;
  let requestSignal = null;
  let aborted = false;
  class FakeAbortController {
    constructor() { this.signal = {}; }
    abort() { aborted = true; }
  }
  const state = vm.createContext({
    cancelInFlight: new Map(),
    AbortController: FakeAbortController,
    setTimeout(callback, ms) {
      timeoutMs = ms;
      callback();
      return 1;
    },
    clearTimeout() {},
    fetch(_url, options) {
      requestSignal = options.signal;
      return Promise.reject(new Error('cancel endpoint stalled'));
    },
    Promise,
    JSON,
  });
  vm.runInContext(declaration('scheduleCancel'), state);

  const settled = await state.scheduleCancel('chat-1', 'turn-1');
  assert.equal(timeoutMs, 6000);
  assert.equal(aborted, true);
  assert.ok(requestSignal);
  assert.equal(settled, false);
});

test('server cancellation settles when fetch ignores its abort signal', async () => {
  let expire;
  let aborted = false;
  class FakeAbortController {
    constructor() { this.signal = {}; }
    abort() { aborted = true; }
  }
  const state = vm.createContext({
    cancelInFlight: new Map(),
    AbortController: FakeAbortController,
    setTimeout(callback) {
      expire = callback;
      return 1;
    },
    clearTimeout() {},
    fetch: () => new Promise(() => {}),
    Promise, JSON,
  });
  vm.runInContext(declaration('scheduleCancel'), state);

  const pending = state.scheduleCancel('chat-1', 'turn-1');
  await new Promise((resolve) => setImmediate(resolve));
  expire();

  assert.equal(await pending, false);
  assert.equal(aborted, true);
  assert.equal(state.cancelInFlight.size, 0);
});

test('an unsettled cancel does not freeze later prompts', async () => {
  const state = vm.createContext({
    cancelInFlight: new Map(), AbortController, setTimeout, clearTimeout,
    fetch: async () => ({
      ok: true,
      json: async () => ({ ok: false, cancelled: false, settled: false }),
    }),
    Promise, JSON,
  });
  for (const name of ['scheduleCancel', 'waitForCancel']) {
    vm.runInContext(declaration(name), state);
  }
  assert.equal(await state.scheduleCancel('chat-1', 'turn-1'), false);
  await state.waitForCancel('chat-1');
});
