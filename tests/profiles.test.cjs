// Run without compiling the app: node --test tests/profiles.test.cjs
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { test } = require('node:test');
const vm = require('node:vm');

const source = readFileSync(
  join(__dirname, '../src/ui/chat/scripts/state.js'),
  'utf8'
).replace(/\r\n/g, '\n');

function declaration(name) {
  const match = source.match(new RegExp('^function ' + name + '\\([\\s\\S]*?^\\}$', 'm'));
  assert.ok(match, 'missing function ' + name);
  return match[0];
}

function context() {
  const state = vm.createContext({
    DEFAULT_PROFILE_ID: 'personal',
    PROFILE_CONTEXT_KEYS: ['about', 'instructions', 'memory'],
    settings: { about: '', instructions: '', memory: '' },
    normalizeProject: (value) => value,
    normalizeConversation: (value) => value,
    normalizeBot: (value) => value,
    normalizeSettings: (value) => ({ ...value }),
    newId: () => 'generated-profile',
    Date,
    outboundQueues: new Map(),
    activeStreams: new Map(),
  });
  for (const name of [
    'ensureConversationSortOrders',
    'normalizeProfile',
    'profileContextFromSettings',
    'settingsForProfile',
    'preferencesPayload',
    'parseStorePayload',
    'normalizeLoadedStore',
    'profileInitials',
    'outboundQueueForStore',
    'conversationStoreRecord',
  ]) vm.runInContext(declaration(name), state);
  return state;
}

test('dense privacy mosaics follow a circular avatar silhouette', () => {
  const classes = new Set();
  const properties = new Map();
  const state = vm.createContext({ Math });
  vm.runInContext(declaration('applyPrivacyMosaic'), state);
  state.applyPrivacyMosaic({
    classList: { add: (name) => classes.add(name) },
    style: { setProperty: (name, value) => properties.set(name, value) },
  }, 'profile-avatar:test', { dense: true });

  const shadows = properties.get('--privacy-pixels');
  const coordinates = [...shadows.matchAll(/([\d.]+)px ([\d.]+)px/g)]
    .map((match) => [Number(match[1]) / 4, Number(match[2]) / 4]);
  assert.ok(coordinates.length > 12);
  assert.ok(coordinates.every(([column, row]) => (
    Math.hypot(column - 2.5, row - 2.5) <= 2.6
  )));
  assert.equal(coordinates.some(([column, row]) => column === 0 && row === 0), false);
  assert.equal(classes.has('privacy-mask-dense'), true);
});

test('legacy data migrates into one Personal profile', () => {
  const state = context();
  const loaded = state.parseStorePayload({
    projects: [{ id: 'project' }],
    conversations: [{ id: 'chat', updatedAt: 10 }],
    bots: [{ id: 'bot' }],
  });
  assert.equal(loaded.migratedFromLegacy, true);
  assert.equal(loaded.activeProfileId, 'personal');
  assert.equal(loaded.profiles[0].name, 'Personal');
  assert.equal(loaded.projects[0].id, 'project');
  assert.equal(loaded.conversations[0].id, 'chat');
});

test('profile context overrides shared preferences without duplicating it', () => {
  const state = context();
  const store = state.parseStorePayload({
    version: 3,
    activeProfileId: 'work',
    profiles: [{
      id: 'work',
      name: 'Work',
      personalization: { about: 'Designer', instructions: 'Be concise', memory: 'Launch Q4' },
      projects: [],
      conversations: [],
      bots: [],
    }],
  });
  const loaded = state.normalizeLoadedStore(store, {
    name: 'Ada Lovelace',
    about: 'Legacy value',
    theme: 'dark',
  });
  assert.equal(loaded.preferences.name, 'Ada Lovelace');
  assert.equal(loaded.preferences.about, 'Designer');
  assert.equal(loaded.preferences.memory, 'Launch Q4');
  const shared = state.preferencesPayload(loaded.preferences);
  assert.equal(shared.name, 'Ada Lovelace');
  assert.equal(shared.theme, 'dark');
  assert.equal(Object.hasOwn(shared, 'about'), false);
  assert.equal(Object.hasOwn(shared, 'instructions'), false);
  assert.equal(Object.hasOwn(shared, 'memory'), false);
});

test('account avatars use up to two initials', () => {
  const state = context();
  assert.equal(state.profileInitials('Ada Lovelace'), 'AL');
  assert.equal(state.profileInitials('Prince'), 'P');
  assert.equal(state.profileInitials(''), 'Y');
});

test('saving another profile preserves its queued messages', () => {
  const state = context();
  const record = state.conversationStoreRecord({
    id: 'other-profile-chat',
    outboundQueue: [{ id: 'queued', displayText: 'Continue later' }],
  });
  assert.equal(record.outboundQueue.length, 1);
  assert.equal(record.outboundQueue[0].id, 'queued');
});

test('profile menu remains rendered while its close transition finishes', () => {
  const classes = new Set(['is-hidden']);
  const attributes = new Map([['aria-expanded', 'false']]);
  let finishClose = null;
  const state = vm.createContext({
    sidebarProfileMenu: {
      classList: {
        add: (name) => classes.add(name),
        remove: (name) => classes.delete(name),
      },
      offsetWidth: 260,
    },
    btnProfileMenu: {
      setAttribute: (name, value) => attributes.set(name, value),
      getAttribute: (name) => attributes.get(name),
      focus() {},
    },
    profileMenuTransitionTimer: null,
    diskEncryptionLocked: () => false,
    prefersReducedMotion: () => false,
    requestAnimationFrame: (callback) => callback(),
    clearTimeout() {},
    window: {
      setTimeout: (callback) => {
        finishClose = callback;
        return 1;
      },
    },
  });
  vm.runInContext(declaration('setProfileMenuOpen'), state);
  state.setProfileMenuOpen(true);
  assert.equal(classes.has('is-hidden'), false);
  assert.equal(classes.has('is-open'), true);

  state.setProfileMenuOpen(false);
  assert.equal(classes.has('is-open'), false);
  assert.equal(classes.has('is-hidden'), false);
  finishClose();
  assert.equal(classes.has('is-hidden'), true);
});

test('profile switching waits until active work is finished', () => {
  const state = vm.createContext({
    activeStreams: new Map([['chat-1', {}]]),
    outboundStarting: new Set(),
    conversations: [{ id: 'chat-1' }],
    isBotsOutboundActive: () => false,
  });
  vm.runInContext(declaration('profileSwitchHasActiveWork'), state);
  assert.equal(state.profileSwitchHasActiveWork(), true);
  state.activeStreams.clear();
  assert.equal(state.profileSwitchHasActiveWork(), false);
  state.isBotsOutboundActive = (id) => id === 'chat-1';
  assert.equal(state.profileSwitchHasActiveWork(), true);
});
