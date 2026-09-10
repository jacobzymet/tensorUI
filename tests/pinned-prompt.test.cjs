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

function row(top, height = 80) {
  const classes = new Set();
  const properties = new Map();
  return {
    top,
    height,
    properties,
    classList: {
      contains: (name) => classes.has(name),
      toggle(name, force) {
        if (force) classes.add(name);
        else classes.delete(name);
      },
    },
    style: {
      setProperty: (name, value) => properties.set(name, value),
      removeProperty: (name) => properties.delete(name),
    },
    getBoundingClientRect() { return { top: this.top, height: this.height }; },
  };
}

test('only the prompt whose response is being scrolled stays pinned', () => {
  const prompts = [row(-300), row(8), row(500)];
  const context = vm.createContext({
    PROMPT_PIN_TOP_PX: 8,
    chatShell: { dataset: { surface: 'chat' } },
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

test('the outgoing prompt fades smoothly as the next prompt covers it', () => {
  const prompts = [row(8, 120), row(68, 40)];
  const context = vm.createContext({
    PROMPT_PIN_TOP_PX: 8,
    chatShell: { dataset: { surface: 'chat' } },
    chatViewport: { getBoundingClientRect: () => ({ top: 0 }) },
    chatThread: { querySelectorAll: () => prompts },
  });
  vm.runInContext(declaration('syncPinnedUserPrompt'), context);

  context.syncPinnedUserPrompt();
  assert.equal(prompts[0].properties.get('--prompt-exit-progress'), '0.500');
  assert.equal(prompts[0].properties.get('--prompt-exit-opacity'), '0.500');
  assert.equal(prompts[0].properties.get('--prompt-exit-shift'), '-2.80px');

  prompts[1].top = 8;
  context.syncPinnedUserPrompt();
  assert.equal(prompts[0].properties.has('--prompt-exit-progress'), false);
  assert.equal(prompts[1].classList.contains('is-pinned-prompt'), true);
});

test('Loop prompts use normal scrolling without pinning or handoff transitions', () => {
  const prompts = [row(-200, 120), row(40, 60)];
  prompts[0].classList.toggle('is-pinned-prompt', true);
  prompts[0].style.setProperty('--prompt-exit-progress', '0.500');
  prompts[0].style.setProperty('--prompt-exit-opacity', '0.500');
  prompts[0].style.setProperty('--prompt-exit-shift', '-2.80px');
  prompts[0].style.setProperty('--prompt-exit-scale', '0.9940');
  const context = vm.createContext({
    PROMPT_PIN_TOP_PX: 8,
    chatShell: { dataset: { surface: 'bots' } },
    chatViewport: {
      getBoundingClientRect: () => {
        assert.fail('Loop prompts must not calculate a pin line');
      },
    },
    chatThread: { querySelectorAll: () => prompts },
  });
  vm.runInContext(declaration('syncPinnedUserPrompt'), context);

  context.syncPinnedUserPrompt();

  assert.deepEqual(prompts.map((item) => item.classList.contains('is-pinned-prompt')), [false, false]);
  assert.equal(prompts[0].properties.size, 0);
  assert.equal(prompts[1].properties.size, 0);
});
