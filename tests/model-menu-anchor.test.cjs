// Run without compiling the app: node --test tests/model-menu-anchor.test.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const root = join(__dirname, '..');
const runtime = readFileSync(
  join(root, 'src/ui/chat/scripts/runtime.js'),
  'utf8'
).replace(/\r\n/g, '\n');
const bots = readFileSync(
  join(root, 'src/ui/chat/scripts/bots.js'),
  'utf8'
).replace(/\r\n/g, '\n');

function declaration(source, name) {
  const match = source.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

test('the shared model menu rejects detached and hidden anchors', () => {
  const context = vm.createContext({});
  vm.runInContext(declaration(runtime, 'modelMenuAnchorIsUsable'), context);
  const visible = {
    isConnected: true,
    getBoundingClientRect() { return { top: 10, left: 20 }; },
    getClientRects() { return [{}]; },
  };
  assert.equal(context.modelMenuAnchorIsUsable(visible), true);
  assert.equal(context.modelMenuAnchorIsUsable({ ...visible, isConnected: false }), false);
  assert.equal(context.modelMenuAnchorIsUsable({
    ...visible,
    getClientRects() { return []; },
  }), false);
});

test('stream state does not invalidate an unchanged Loops member list', () => {
  const context = vm.createContext({
    loopModelTriggerLabel: (model) => 'Label for ' + model,
    JSON,
  });
  vm.runInContext(declaration(bots, 'traceMembersRenderSignature'), context);
  const members = [{
    id: 'agent-1',
    handle: 'reviewer',
    name: '@reviewer',
    model: 'provider/model-a',
    stage: 'challenge',
    description: 'Challenge assumptions',
  }];
  const before = context.traceMembersRenderSignature({ id: 'loop-1', loopRun: { phaseIndex: 0 } }, members);
  const during = context.traceMembersRenderSignature({ id: 'loop-1', loopRun: { phaseIndex: 2 } }, members);
  assert.equal(before, during);

  const changed = context.traceMembersRenderSignature(
    { id: 'loop-1' },
    [{ ...members[0], model: 'provider/model-b' }]
  );
  assert.notEqual(before, changed);
});
