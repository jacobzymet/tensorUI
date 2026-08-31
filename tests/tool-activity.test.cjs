// Run without compiling the app: node --test tests/tool-activity.test.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const root = join(__dirname, '..');
const render = readFileSync(join(root, 'src/ui/chat/scripts/render.js'), 'utf8').replace(/\r\n/g, '\n');
const runtime = readFileSync(join(root, 'src/ui/chat/scripts/runtime.js'), 'utf8').replace(/\r\n/g, '\n');

// Exercise the shipped functions, without bootstrapping the rest of the UI.
function declaration(source, name) {
  const match = source.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

function harness(timeline = []) {
  const context = vm.createContext({
    stream: { timeline },
    convo: {},
    typer: { shown: '' },
    scheduleJustSettledClear() {},
    paintStreamIntoView() {},
    escapeHtml: (value) => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('"', '&quot;'),
    THINK_CHEVRON: '',
  });
  for (const name of [
    'agentStepRailHtml', 'wrapTimelineStep', 'skillLabel', 'toolBodyFromArgs',
    'skillLiveVerb', 'skillToolIcon', 'formatToolElapsed', 'toolDurationMs',
    'skillDetailLabel', 'agentStepResultHtml', 'agentStepHtml',
  ]) vm.runInContext(declaration(render, name), context);
  vm.runInContext(declaration(runtime, 'timelineSignature'), context);

  const eventStart = runtime.indexOf('  const onAgentEvent = (payload) => {');
  const eventEnd = runtime.indexOf('  if (stream.catchingUp)', eventStart);
  assert.ok(eventStart >= 0 && eventEnd > eventStart);
  vm.runInContext(runtime.slice(eventStart, eventEnd) + '\nthis.onAgentEvent = onAgentEvent;', context);

  const persistStart = runtime.indexOf('  const persistedParts = stream.timeline.length');
  const persistEnd = runtime.indexOf('  if (viewing && dom)', persistStart);
  assert.ok(persistStart >= 0 && persistEnd > persistStart);
  vm.runInContext('function persist() {\n' + runtime.slice(persistStart, persistEnd) + '\nreturn persistedParts;\n}', context);
  return context;
}

test('failed edits remain failed through SSE, persistence, and rendering', () => {
  const context = harness([{ type: 'tool', id: 'edit', name: 'str_replace', live: true, startedAt: Date.now() }]);
  context.onAgentEvent({ phase: 'tool_result', id: 'edit', name: 'str_replace', ok: false, result: 'old_string was not found' });
  assert.equal(context.stream.timeline[0].ok, false);
  assert.equal(context.stream.timeline[0].live, false);
  const persisted = JSON.parse(JSON.stringify(context.persist()))[0];
  assert.equal(persisted.ok, false);
  const html = context.agentStepHtml(persisted);
  assert.match(html, /class="agent-step is-failed"/);
  assert.match(html, />Failed<\/span>/);
  assert.doesNotMatch(html, /is-done|is-just-done/);
});

test('terminal failures without a preceding live card retain their id and status', () => {
  const context = harness();
  context.onAgentEvent({ phase: 'tool_result', id: 'terminal', name: 'run_terminal', ok: false, result: 'exit: 7' });
  assert.equal(context.stream.timeline[0].id, 'terminal');
  assert.equal(context.stream.timeline[0].ok, false);
  assert.match(context.agentStepHtml(context.persist()[0]), /is-failed/);
});

test('denied calls stay denied after the chat is saved', () => {
  const context = harness();
  context.onAgentEvent({ phase: 'tool_result', id: 'write', name: 'write_file', ok: false, result: 'The user denied this tool call.' });
  const persisted = context.persist()[0];
  assert.equal(persisted.approval, 'denied');
  assert.match(context.agentStepHtml(persisted), />Denied<\/span>/);
});

test('successful calls clear stale failure notes and render a success marker', () => {
  const context = harness([{ type: 'tool', id: 'read', name: 'read_file', live: true, ok: false, note: 'Tool failed' }]);
  context.onAgentEvent({ phase: 'tool_result', id: 'read', name: 'read_file', ok: true, result: 'content' });
  const part = context.persist()[0];
  assert.equal(part.ok, true);
  assert.equal(part.note, undefined);
  assert.match(context.agentStepHtml(part), /class="agent-step is-done"/);
  assert.doesNotMatch(context.agentStepHtml(part), /is-failed|>Failed<\/span>/);
});

test('failure status invalidates the activity rendering signature', () => {
  const context = harness();
  const part = { type: 'tool', name: 'run_terminal', result: 'output', live: false };
  assert.notEqual(context.timelineSignature([{ ...part, ok: true }]), context.timelineSignature([{ ...part, ok: false }]));
});

test('live tools and legacy saved cards remain compatible', () => {
  const context = harness();
  assert.match(context.agentStepHtml({ name: 'read_file', live: true }), /class="agent-step is-live"/);
  assert.match(context.agentStepHtml({ name: 'read_file', live: false }), /class="agent-step is-done"/);
});
