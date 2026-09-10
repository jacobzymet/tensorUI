// Run without compiling the app: node --test tests/conversation-bulk.test.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const source = readFileSync(
  join(__dirname, '../src/ui/chat/scripts/render.js'),
  'utf8'
).replace(/\r\n/g, '\n');

function declaration(name) {
  const match = source.match(new RegExp('^(?:async )?function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

test('bulk pin ignores ghost chats and pins every saved selection', () => {
  const conversations = [
    { id: 'one', pinned: false, incognito: false },
    { id: 'two', pinned: false, incognito: false },
    { id: 'ghost', pinned: false, incognito: true },
  ];
  let saves = 0;
  let selectionClosed = false;
  const context = vm.createContext({
    conversations,
    Date: { now: () => 1000 },
    saveConversations: () => { saves += 1; },
    setConversationSelectionMode: (enabled) => { selectionClosed = !enabled; },
  });
  vm.runInContext(
    "const selectedConversationIds = new Set(['one', 'two', 'ghost']);",
    context
  );
  vm.runInContext(declaration('bulkSetSelectedConversationsPinned'), context);

  context.bulkSetSelectedConversationsPinned();
  assert.deepEqual(conversations.map((convo) => convo.pinned), [true, true, false]);
  assert.equal(saves, 1);
  assert.equal(selectionClosed, true);
});

test('bulk pin becomes bulk unpin when every saved selection is pinned', () => {
  const conversations = [
    { id: 'one', pinned: true, pinnedAt: 10, incognito: false },
    { id: 'two', pinned: true, pinnedAt: 20, incognito: false },
  ];
  const context = vm.createContext({
    conversations,
    Date,
    saveConversations() {},
    setConversationSelectionMode() {},
  });
  vm.runInContext("const selectedConversationIds = new Set(['one', 'two']);", context);
  vm.runInContext(declaration('bulkSetSelectedConversationsPinned'), context);

  context.bulkSetSelectedConversationsPinned();
  assert.deepEqual(conversations.map((convo) => convo.pinned), [false, false]);
  assert.deepEqual(conversations.map((convo) => convo.pinnedAt), [null, null]);
});

test('bulk delete confirms once and removes only selected sessions', async () => {
  const aborted = [];
  const activeStreams = new Map([['one', {}], ['keep', {}]]);
  const outboundQueues = new Map([['two', []], ['keep', []]]);
  const stickByConvo = new Map([['one', true], ['keep', true]]);
  let saves = 0;
  let renders = 0;
  const context = vm.createContext({
    conversations: [{ id: 'one' }, { id: 'two' }, { id: 'keep' }],
    confirmDanger: async () => true,
    abortStream: (id) => aborted.push(id),
    activeStreams,
    outboundQueues,
    stickByConvo,
    editingQueueId: null,
    activeId: 'keep',
    saveConversations: () => { saves += 1; },
    renderSidebar: () => { renders += 1; },
    syncComposerStreamUi() {},
  });
  vm.runInContext(
    "let conversationSelectionMode = true; const selectedConversationIds = new Set(['one', 'two']);",
    context
  );
  vm.runInContext(declaration('bulkDeleteSelectedConversations'), context);

  await context.bulkDeleteSelectedConversations();
  assert.deepEqual(context.conversations.map((convo) => convo.id), ['keep']);
  assert.deepEqual(aborted, ['one', 'two']);
  assert.equal(activeStreams.has('one'), false);
  assert.equal(activeStreams.has('keep'), true);
  assert.equal(outboundQueues.has('two'), false);
  assert.equal(outboundQueues.has('keep'), true);
  assert.equal(stickByConvo.has('one'), false);
  assert.equal(stickByConvo.has('keep'), true);
  assert.equal(saves, 1);
  assert.equal(renders, 1);
});
