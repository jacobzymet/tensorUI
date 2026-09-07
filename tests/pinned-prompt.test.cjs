// Run without compiling the app: node --test tests/pinned-prompt.test.cjs
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
  const match = source.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

function row(top) {
  const classes = new Set();
  return {
    top,
    classList: {
      contains: (name) => classes.has(name),
      toggle(name, force) {
        if (force) classes.add(name);
        else classes.delete(name);
      },
    },
    getBoundingClientRect() { return { top: this.top }; },
  };
}

test('only the prompt whose response is being scrolled stays pinned', () => {
  const prompts = [row(-300), row(8), row(500)];
  const context = vm.createContext({
    PROMPT_PIN_TOP_PX: 8,
    chatViewport: { getBoundingClientRect: () => ({ top: 0 }) },
    chatThread: { querySelectorAll: () => prompts },
  });
  vm.runInContext(declaration('syncPinnedUserPrompt'), context);

  context.syncPinnedUserPrompt();
  assert.deepEqual(prompts.map((item) => item.classList.contains('is-pinned-prompt')), [false, true, false]);

  prompts[1].top = -200;
  prompts[2].top = 8;
  context.syncPinnedUserPrompt();
  assert.deepEqual(prompts.map((item) => item.classList.contains('is-pinned-prompt')), [false, false, true]);
});
