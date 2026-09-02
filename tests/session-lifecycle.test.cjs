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

  await state.scheduleCancel('chat-1', 'turn-1');
  assert.equal(timeoutMs, 4000);
  assert.equal(aborted, true);
  assert.ok(requestSignal);
});
