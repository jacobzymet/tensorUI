const test = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');

function harness(fetch) {
  const source = readFileSync(join(__dirname, '../src/ui/chat/scripts/state.js'), 'utf8').replace(/\r\n/g, '\n');
  const state = vm.createContext({
    fetch, AbortController, setTimeout, clearTimeout,
    reportPersistenceFailure() {},
    storageWriteEpoch: 0,
    diskEncryptionLocked: () => false,
    storeWriteChain: Promise.resolve(),
  });
  for (const name of ['putJsonWithRetry', 'enqueueStoreWrite']) {
    const match = source.match(new RegExp('(?:async )?function ' + name + '\\([^]*?\\n\\}'));
    vm.runInContext(match[0], state);
  }
  return state;
}

test('save releases echoed response bodies before the next queued write', async () => {
  let occupied = false;
  let released = 0;
  const state = harness(async () => {
    assert.equal(occupied, false);
    occupied = true;
    return { ok: true, body: { async cancel() { occupied = false; released++; } } };
  });
  await Promise.all([state.enqueueStoreWrite({ a: 1 }), state.enqueueStoreWrite({ a: 2 })]);
  assert.equal(released, 2);
});

test('a stalled save times out and later writes can complete', async () => {
  let firstSignal;
  let calls = 0;
  const state = harness(async (_, options) => {
    if (++calls === 1) {
      firstSignal = options.signal;
      return new Promise(() => {});
    }
    return { ok: true, body: null };
  });
  assert.equal(await state.putJsonWithRetry('/save', {}, { attempts: 1, timeoutMs: 5 }), false);
  assert.equal(firstSignal.aborted, true);
  assert.equal(await state.enqueueStoreWrite({ a: 2 }), true);
});
