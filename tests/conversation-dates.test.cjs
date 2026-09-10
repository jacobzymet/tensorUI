const test = require('node:test');
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const vm = require('node:vm');

const source = readFileSync(
  join(__dirname, '../src/ui/chat/scripts/render.js'),
  'utf8'
).replace(/\r\n/g, '\n');

function declaration(name) {
  const match = source.match(new RegExp('function ' + name + '\\([^]*?\\n\\}'));
  if (!match) throw new Error('Missing function ' + name);
  return match[0];
}

function context() {
  const state = vm.createContext({ Date });
  for (const name of ['bySidebarOrder', 'startOfLocalDay', 'conversationDateBucket', 'conversationDateGroups']) {
    vm.runInContext(declaration(name), state);
  }
  return state;
}

test('conversation date groups follow calendar boundaries and newest-first order', () => {
  const state = context();
  const now = new Date(2026, 8, 10, 12).getTime();
  const at = (day, hour = 12) => new Date(2026, 8, day, hour).getTime();
  const rows = [
    { id: 'older-today', updatedAt: at(10, 8) },
    { id: 'last-week', updatedAt: at(4) },
    { id: 'yesterday', updatedAt: at(9) },
    { id: 'newer-today', updatedAt: at(10, 11) },
    { id: 'earlier-week', updatedAt: at(7) },
    { id: 'august', updatedAt: new Date(2026, 7, 20).getTime() },
  ];
  const groups = state.conversationDateGroups(rows, now);
  assert.deepEqual(
    JSON.parse(JSON.stringify(groups.map((group) => [group.label, group.conversations.map((row) => row.id)]))),
    [
      ['Today', ['newer-today', 'older-today']],
      ['Yesterday', ['yesterday']],
      ['Earlier this week', ['earlier-week']],
      ['Last week', ['last-week']],
      ['August', ['august']],
    ]
  );
});

test('older calendar groups include their year', () => {
  const state = context();
  const bucket = state.conversationDateBucket(
    new Date(2025, 11, 1).getTime(),
    new Date(2026, 8, 10).getTime()
  );
  assert.equal(bucket.label, 'December 2025');
});
