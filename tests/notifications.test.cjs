// Run without compiling the app: node --test tests/notifications.test.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const root = join(__dirname, '..');
const render = readFileSync(join(root, 'src/ui/chat/scripts/render.js'), 'utf8').replace(/\r\n/g, '\n');

function declaration(name) {
  const match = render.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

function context(overrides = {}) {
  const state = vm.createContext({
    activeId: null,
    mainView: 'chat',
    document: { visibilityState: 'visible' },
    activeStreams: new Map(),
    conversations: [],
    saveConversations() {},
    ...overrides,
  });
  for (const name of [
    'notificationIsUnread',
    'markConversationNotificationRead',
    'recordConversationNotification',
    'pendingAttentionNotifications',
  ]) vm.runInContext(declaration(name), state);
  return state;
}

test('background completions become unread until the task is opened', () => {
  let saves = 0;
  const state = context({ saveConversations: () => { saves += 1; } });
  const convo = { id: 'task-1' };
  state.recordConversationNotification(convo, { at: 200 });
  assert.equal(state.notificationIsUnread(convo), true);
  assert.equal(state.markConversationNotificationRead(convo), true);
  assert.equal(convo.notificationReadAt, 200);
  assert.equal(saves, 1);
});

test('a visible open task records its completion as already read', () => {
  const convo = { id: 'task-1' };
  const state = context({ activeId: convo.id });
  state.recordConversationNotification(convo, { at: 300 });
  assert.equal(convo.notificationReadAt, 300);
  assert.equal(state.notificationIsUnread(convo), false);
});

test('pending approvals and clarification questions are both attention items', () => {
  const convo = { id: 'task-1', title: 'Ship the release' };
  const state = context({
    conversations: [convo],
    activeStreams: new Map([[convo.id, { timeline: [
      { type: 'tool', approval: 'pending', name: 'run_terminal', detail: 'deploy', startedAt: 20 },
      { type: 'clarify', live: true, questions: [{}, {}], startedAt: 10 },
    ] }]]),
  });
  const items = state.pendingAttentionNotifications();
  assert.equal(items.map((item) => item.type).join(','), 'approval,input');
  assert.match(items[1].detail, /Answer 2 questions/);
});

test('notification excerpts render sanitized inline markdown', () => {
  let options = null;
  const state = vm.createContext({
    window: {
      marked: {
        parseInline: (source) => source.replace(/\*\*(.*?)\*\*/g, '<strong>$1</strong>'),
      },
      DOMPurify: {
        sanitize: (html, config) => {
          options = config;
          return html;
        },
      },
    },
    escapeHtml: (value) => value,
  });
  vm.runInContext(declaration('renderNotificationMarkdown'), state);
  const html = state.renderNotificationMarkdown('**Critique** with `code`');
  assert.match(html, /<strong>Critique<\/strong>/);
  assert.deepEqual(Array.from(options.ALLOWED_TAGS), ['strong', 'em', 'code', 'del', 'br']);
  assert.equal(options.ALLOWED_ATTR.length, 0);
});
