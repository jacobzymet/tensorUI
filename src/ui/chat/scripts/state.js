const STORAGE_KEY = 'tensorui.chat.v1';
const SETTINGS_KEY = 'tensorui.chat.settings.v1';
const SIDEBAR_COLLAPSED_KEY = 'tensorui.chat.sidebarCollapsed';
const UPDATE_DISMISS_KEY = 'tensorui.update.dismissed';
const TRACE_DESKTOP_MQ = window.matchMedia('(min-width: 821px)');
const THINKING_EFFORTS = ['auto', 'off', 'low', 'medium', 'high', 'max'];
const WEB_SEARCH_DEPTHS = ['auto', 'off', 'light', 'standard', 'deep'];
const WEB_SEARCH_BACKENDS = ['auto', 'duckduckgo', 'brave', 'bing', 'google', 'mojeek', 'startpage', 'yahoo', 'yandex', 'wikipedia'];
const WEB_SEARCH_SAFESEARCH = ['on', 'moderate', 'off'];
const WEB_SEARCH_RECENCIES = ['any', 'day', 'week', 'month', 'year'];
const DEEP_RESEARCH_MODES = ['off', 'long', 'brief'];
const ATTACHMENTS_MODES = ['auto', 'on', 'off'];
const ATTACHMENT_MIN_CONTEXT = 16384;
const ATTACHMENT_MAX_FILES = 6;
const ATTACHMENT_MAX_BYTES = 8 * 1024 * 1024;
const PROJECT_MEMORY_MODES = ['default', 'project_only'];
const SIBLING_CHAT_MAX = 8;
const SIBLING_CHAT_BUDGET = 10000;
const SIBLING_MSG_CHARS = 420;
const SIBLING_MSGS_PER_CHAT = 4;
const DEFAULT_SETTINGS = {
  name: '',
  about: '',
  instructions: '',
  memory: '',
  thinking: 'collapsed', // collapsed | hidden | visible
  thinkingEffort: 'auto', // auto | off | low | medium | high | max
  enterSends: true,
  skillWebSearch: true,
  webSearchDepth: 'auto', // auto | off | light | standard | deep
  webSearchBackend: 'auto',
  webSearchResults: 6,
  webSearchRegion: 'us-en',
  webSearchSafeSearch: 'moderate',
  webSearchRecency: 'any',
  skillFetchUrl: true,
  fetchUrlMaxChars: 8000,
  webSearchPageMaxChars: 0,
  skillDeepResearch: true,
  agentMode: false,
  deepResearch: 'off', // off | long | brief — composer Research mode
  attachmentsMode: 'auto', // auto | on | off
  attachmentTextFallback: false,
  attachmentOcr: false,
  attachmentMaxChars: 48000,
};

const chatShell = document.getElementById('chatShell');
const chatTopbar = document.getElementById('chatTopbar');
const emptyState = document.getElementById('emptyState');
const emptyStateInner = document.getElementById('emptyStateInner');
const threadWrap = document.getElementById('threadWrap');
const chatThread = document.getElementById('chatThread');
const chatViewport = document.getElementById('chatViewport');
const composerDock = document.getElementById('composerDock');
const composerCard = document.getElementById('composerCard');
const composerShell = document.getElementById('composerShell');
const composerInput = document.getElementById('composerInput');
const composerHint = document.getElementById('composerHint');
const composerMentions = document.getElementById('composerMentions');
const composerReply = document.getElementById('composerReply');
const selectionReplyBar = document.getElementById('selectionReplyBar');
const btnSelectionReply = document.getElementById('btnSelectionReply');
const mentionMenu = document.getElementById('mentionMenu');
const settingsModal = document.getElementById('settingsModal');
const projectModal = document.getElementById('projectModal');
const projectsView = document.getElementById('projectsView');
const projectsGrid = document.getElementById('projectsGrid');
const projectsSearch = document.getElementById('projectsSearch');
const projectsSort = document.getElementById('projectsSort');
const btnProjectsNav = document.getElementById('btnProjectsNav');
const sidebarProjectContext = document.getElementById('sidebarProjectContext');
const sidebarConvoLabel = document.getElementById('sidebarConvoLabel');
const sidebarPinnedSection = document.getElementById('sidebarPinnedSection');
const pinnedConvoList = document.getElementById('pinnedConvoList');
const topbarProject = document.getElementById('topbarProject');
const emptyEyebrow = document.getElementById('emptyEyebrow');
const btnNewChat = document.getElementById('btnNewChat');
const btnNewChatLabel = document.getElementById('btnNewChatLabel');
const btnNewIncognitoChat = document.getElementById('btnNewIncognitoChat');
const topbarIncognito = document.getElementById('topbarIncognito');
const btnSend = document.getElementById('btnSend');
const btnBranch = document.getElementById('btnBranch');
const btnStop = document.getElementById('btnStop');
const btnPlus = document.getElementById('btnPlus');
const plusMenu = document.getElementById('plusMenu');
const composerModes = document.getElementById('composerModes');
const btnMic = document.getElementById('btnMic');
const attachFileInput = document.getElementById('attachFileInput');
const composerAttachmentsEl = document.getElementById('composerAttachments');
const SpeechRecognitionAPI = window.SpeechRecognition || window.webkitSpeechRecognition;
let voiceRecognition = null;
let voiceListening = false;
let voicePrefix = '';
let voiceSuffix = '';
let voiceFinal = '';
let voiceHintUntil = 0;
const traceSidebar = document.getElementById('traceSidebar');
const traceSidebarBody = document.getElementById('traceSidebarBody');
const traceSidebarTitle = document.getElementById('traceSidebarTitle');
const btnToggleTrace = document.getElementById('btnToggleTrace');
const btnExpandTrace = document.getElementById('btnExpandTrace');

/** Selected assistant message index for the activity sidebar (desktop). */
let selectedTraceMsgIndex = null;
let traceSidebarSwapTimer = 0;
/** Convo id we already auto-opened the rail for during the current turn. */
let traceAutoOpenedForStream = null;
/** User manually hid the rail this turn — don't fight them with auto-open. */
let traceUserCollapsed = false;
/** Keep the activity rail pinned to the latest step unless the user scrolls up. */
let stickTraceSidebar = true;
let currentFonts = {
  font_body: 'inter',
  font_display: 'space-grotesk',
  font_scale: 'default',
};
let repaintStarfield = null;

const FONT_BODY_STACKS = {
  inter: '"Inter", system-ui, sans-serif',
  'source-sans-3': '"Source Sans 3", system-ui, sans-serif',
  'ibm-plex-sans': '"IBM Plex Sans", system-ui, sans-serif',
  'atkinson-hyperlegible': '"Atkinson Hyperlegible", system-ui, sans-serif',
  literata: '"Literata", Georgia, serif',
};
const FONT_DISPLAY_STACKS = {
  'space-grotesk': '"Space Grotesk", system-ui, sans-serif',
  syne: '"Syne", system-ui, sans-serif',
  'dm-sans': '"DM Sans", system-ui, sans-serif',
  'instrument-sans': '"Instrument Sans", system-ui, sans-serif',
  fraunces: '"Fraunces", Georgia, serif',
};

function normalizeThemePref(theme) {
  if (theme === 'light' || theme === 'system') return theme;
  return 'dark';
}

function resolveTheme(pref) {
  if (pref === 'light') return 'light';
  if (pref === 'system') {
    return window.matchMedia('(prefers-color-scheme: light)').matches ? 'light' : 'dark';
  }
  return 'dark';
}

function prefersReducedMotion() {
  return !!(window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches);
}

function motionEnter(el, { y = 14, duration = 220, delay = 0 } = {}) {
  if (!el || prefersReducedMotion() || typeof el.animate !== 'function') return null;
  el.classList.add('motion-enter');
  const anim = el.animate(
    [
      { opacity: 0, transform: 'translateY(' + y + 'px)' },
      { opacity: 1, transform: 'translateY(0)' },
    ],
    {
      duration,
      delay,
      easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
      fill: 'both',
    }
  );
  anim.finished.finally(() => el.classList.remove('motion-enter')).catch(() => {});
  return anim;
}

function openBackdrop(el) {
  if (!el) return;
  el.classList.remove('is-hidden');
  // Force layout so the enter transition runs from opacity 0.
  void el.offsetWidth;
  requestAnimationFrame(() => el.classList.add('is-open'));
}

function closeBackdrop(el) {
  if (!el) return;
  el.classList.remove('is-open');
  if (prefersReducedMotion() || el.classList.contains('is-hidden')) {
    el.classList.add('is-hidden');
    return;
  }
  let done = false;
  const finish = () => {
    if (done) return;
    done = true;
    el.classList.add('is-hidden');
    el.removeEventListener('transitionend', onEnd);
  };
  const onEnd = (event) => {
    if (event.target === el && event.propertyName === 'opacity') finish();
  };
  el.addEventListener('transitionend', onEnd);
  window.setTimeout(finish, 360);
}

function applyTheme(theme) {
  const pref = normalizeThemePref(theme);
  const resolved = resolveTheme(pref);
  const prevResolved = document.documentElement.dataset.theme;
  currentTheme = pref;
  const paint = () => {
    document.documentElement.dataset.theme = resolved;
    document.documentElement.dataset.themePref = pref;
    document.querySelectorAll('[data-theme-choice]').forEach((btn) => {
      const active = btn.dataset.themeChoice === pref;
      btn.classList.toggle('is-active', active);
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
    });
    if (prevResolved !== resolved && typeof repaintStarfield === 'function') {
      repaintStarfield();
    }
  };
  if (
    prevResolved
    && prevResolved !== resolved
    && !prefersReducedMotion()
    && typeof document.startViewTransition === 'function'
  ) {
    document.startViewTransition(paint);
  } else {
    paint();
  }
}

if (window.matchMedia) {
  const systemThemeMq = window.matchMedia('(prefers-color-scheme: light)');
  const onSystemTheme = () => {
    if (currentTheme === 'system') applyTheme('system');
  };
  if (systemThemeMq.addEventListener) systemThemeMq.addEventListener('change', onSystemTheme);
  else if (systemThemeMq.addListener) systemThemeMq.addListener(onSystemTheme);
}

function applyFonts(appearance) {
  const body = FONT_BODY_STACKS[appearance.font_body] ? appearance.font_body : 'inter';
  const display = FONT_DISPLAY_STACKS[appearance.font_display]
    ? appearance.font_display
    : 'space-grotesk';
  const scale = ['compact', 'default', 'large'].includes(appearance.font_scale)
    ? appearance.font_scale
    : 'default';
  currentFonts = { font_body: body, font_display: display, font_scale: scale };
  const root = document.documentElement;
  root.style.setProperty('--font-body', FONT_BODY_STACKS[body]);
  root.style.setProperty('--font-display', FONT_DISPLAY_STACKS[display]);
  // No monospace anywhere — code, chips, and labels use the body face.
  root.style.setProperty('--font-mono', FONT_BODY_STACKS[body]);
  root.style.setProperty('--font-logo', '"Syne", ' + FONT_DISPLAY_STACKS[display]);
  if (scale === 'default') delete root.dataset.fontScale;
  else root.dataset.fontScale = scale;
  const bodySelect = document.getElementById('settingFontBody');
  const displaySelect = document.getElementById('settingFontDisplay');
  if (bodySelect) bodySelect.value = body;
  if (displaySelect) displaySelect.value = display;
  document.querySelectorAll('[data-font-scale]').forEach((btn) => {
    const active = btn.dataset.fontScale === scale;
    btn.classList.toggle('is-active', active);
    btn.setAttribute('aria-selected', active ? 'true' : 'false');
  });
}

function applyAppearance(data) {
  if (!data) return;
  if (data.theme) applyTheme(data.theme);
  applyFonts({
    font_body: data.font_body || currentFonts.font_body,
    font_display: data.font_display || currentFonts.font_display,
    font_scale: data.font_scale || currentFonts.font_scale,
  });
}

async function patchAppearance(patch) {
  applyAppearance({ ...currentFonts, theme: currentTheme, ...patch });
  try {
    const response = await fetch('/api/ui/appearance', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(patch),
    });
    if (!response.ok) return;
    applyAppearance(await response.json());
  } catch {
    // keep local appearance; next poll will reconcile
  }
}

async function setTheme(theme) {
  await patchAppearance({ theme });
}

async function resetAppearance() {
  try {
    const response = await fetch('/api/ui/appearance/reset', { method: 'POST' });
    if (!response.ok) return;
    applyAppearance(await response.json());
  } catch {
    // ignore
  }
}

const MENTION_OPTIONS = [
  {
    id: 'web_search',
    label: 'web_search',
    description: 'Search the web (stays on until you remove it)',
  },
  {
    id: 'fetch_url',
    label: 'fetch_url',
    description: 'Open a URL (stays on until you remove it)',
  },
  {
    id: 'deep_research',
    label: 'deep_research',
    description: 'Investigate across many sources for this message',
  },
];
const MENTION_IDS = MENTION_OPTIONS.map((item) => item.id);
const MENTION_TOKEN_RE = /@(?:web_search|fetch_url|deep_research|agent)\b/gi;
let mentionState = null; // { start, query, activeIndex, items }
/** Active composer mentions shown as chips (not as literal text in the textarea). */
const composerMentionIds = new Set();
/** Textarea currently driving the @mention menu (composer or message edit). */
let mentionInput = composerInput;
const convoTitleEl = document.getElementById('convoTitle');
const greetingEl = document.getElementById('greeting');
const modelHintEl = document.getElementById('modelHint');
const serverChip = document.getElementById('serverChip');
const serverProviderName = document.getElementById('serverProviderName');
const chatModelSelect = document.getElementById('chatModelSelect');
const chatModelSelectWrap = document.getElementById('chatModelSelectWrap');
const chatModelOriginPill = document.getElementById('chatModelOriginPill');
const chatModelMenu = document.getElementById('chatModelMenu');
const chatModelList = document.getElementById('chatModelList');
const chatModelSearchWrap = document.getElementById('chatModelSearchWrap');
const chatModelSearch = document.getElementById('chatModelSearch');
const chatModelSearchCount = document.getElementById('chatModelSearchCount');
const chatModelEmpty = document.getElementById('chatModelEmpty');
const REMOTE_MODEL_KEY = 'tensorui_remote_model_id';
const CHAT_MODEL_KEY = 'tensorui_chat_model';
const RECENT_MODELS_KEY = 'tensorui_recent_models';
const PINNED_MODELS_KEY = 'tensorui_pinned_models';
const RECENT_MODELS_MAX = 12;
const PINNED_MODELS_MAX = 48;
/** Below this many models the filter field is more noise than help. */
const MODEL_SEARCH_MIN_OPTIONS = 6;
const MODEL_PIN_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V6a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1z"/></svg>';
let selectedRemoteModelId = localStorage.getItem(REMOTE_MODEL_KEY) || '';
let selectedChatModel = localStorage.getItem(CHAT_MODEL_KEY) || '';
let recentModelIds = loadRecentModelIds();
let pinnedModelIds = loadPinnedModelIds();
let modelMenuOptions = [];
/** Options passing the current filter — what the arrow keys actually walk. */
let modelMenuMatches = [];
let modelMenuFilter = '';
let modelMenuActiveIndex = -1;
let modelMenuTab = recentModelIds.length ? 'recents' : (pinnedModelIds.length ? 'pins' : 'all');
let latestState = null;

function loadRecentModelIds() {
  try {
    const parsed = JSON.parse(localStorage.getItem(RECENT_MODELS_KEY) || '[]');
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((id) => typeof id === 'string' && id.trim())
      .map((id) => id.trim())
      .slice(0, RECENT_MODELS_MAX);
  } catch {
    return [];
  }
}

function saveRecentModelIds(ids) {
  recentModelIds = (Array.isArray(ids) ? ids : [])
    .filter((id) => typeof id === 'string' && id)
    .slice(0, RECENT_MODELS_MAX);
  try {
    localStorage.setItem(RECENT_MODELS_KEY, JSON.stringify(recentModelIds));
  } catch {
    // ignore quota / private-mode failures
  }
}

function rememberRecentModel(value) {
  if (!value) return;
  saveRecentModelIds([value, ...recentModelIds.filter((id) => id !== value)]);
}

function pruneRecentModels(availableValues) {
  const allowed = new Set(availableValues || []);
  const next = recentModelIds.filter((id) => allowed.has(id));
  if (next.length !== recentModelIds.length) saveRecentModelIds(next);
  if (modelMenuTab === 'recents' && !recentModelIds.length) {
    modelMenuTab = pinnedModelIds.length ? 'pins' : 'all';
  }
}

function loadPinnedModelIds() {
  try {
    const parsed = JSON.parse(localStorage.getItem(PINNED_MODELS_KEY) || '[]');
    if (!Array.isArray(parsed)) return [];
    return parsed
      .filter((id) => typeof id === 'string' && id.trim())
      .map((id) => id.trim())
      .slice(0, PINNED_MODELS_MAX);
  } catch {
    return [];
  }
}

function savePinnedModelIds(ids) {
  pinnedModelIds = (Array.isArray(ids) ? ids : [])
    .filter((id) => typeof id === 'string' && id)
    .slice(0, PINNED_MODELS_MAX);
  try {
    localStorage.setItem(PINNED_MODELS_KEY, JSON.stringify(pinnedModelIds));
  } catch {
    // ignore quota / private-mode failures
  }
}

function isModelPinned(value) {
  return !!value && pinnedModelIds.includes(value);
}

function togglePinnedModel(value) {
  if (!value) return;
  if (isModelPinned(value)) {
    savePinnedModelIds(pinnedModelIds.filter((id) => id !== value));
    if (modelMenuTab === 'pins' && !pinnedModelIds.length) {
      modelMenuTab = recentModelIds.length ? 'recents' : 'all';
    }
  } else {
    savePinnedModelIds([value, ...pinnedModelIds.filter((id) => id !== value)]);
  }
  if (modelMenuIsOpen()) {
    syncModelMenuTabs();
    applyModelFilter({ keepActive: true });
  }
}

function prunePinnedModels(availableValues) {
  const allowed = new Set(availableValues || []);
  const next = pinnedModelIds.filter((id) => allowed.has(id));
  if (next.length !== pinnedModelIds.length) savePinnedModelIds(next);
  if (modelMenuTab === 'pins' && !pinnedModelIds.length) {
    modelMenuTab = recentModelIds.length ? 'recents' : 'all';
  }
}

function newId(prefix) {
  return crypto.randomUUID
    ? crypto.randomUUID()
    : prefix + Date.now() + Math.random().toString(16).slice(2);
}

function normalizeProject(project) {
  return {
    id: project.id || newId('p'),
    name: typeof project.name === 'string' && project.name.trim()
      ? project.name.trim()
      : 'Untitled project',
    instructions: typeof project.instructions === 'string' ? project.instructions : '',
    memory: typeof project.memory === 'string' ? project.memory : '',
    memoryMode: PROJECT_MEMORY_MODES.includes(project.memoryMode)
      ? project.memoryMode
      : 'default',
    createdAt: typeof project.createdAt === 'number' ? project.createdAt : Date.now(),
    updatedAt: typeof project.updatedAt === 'number' ? project.updatedAt : Date.now(),
  };
}

function normalizeConversation(convo) {
  return {
    id: convo.id || newId('c'),
    title: typeof convo.title === 'string' ? convo.title : 'New chat',
    titleEdited: !!convo.titleEdited,
    messages: Array.isArray(convo.messages) ? convo.messages : [],
    updatedAt: typeof convo.updatedAt === 'number' ? convo.updatedAt : Date.now(),
    projectId: typeof convo.projectId === 'string' ? convo.projectId : null,
    sortOrder: typeof convo.sortOrder === 'number' && Number.isFinite(convo.sortOrder)
      ? convo.sortOrder
      : null,
    incognito: !!convo.incognito,
    pinned: !!convo.pinned && !convo.incognito,
    pinnedAt: typeof convo.pinnedAt === 'number' && Number.isFinite(convo.pinnedAt)
      ? convo.pinnedAt
      : null,
  };
}

/** Fill missing sortOrder so Recents keep a stable, draggable order. */
function ensureConversationSortOrders(list) {
  let changed = false;
  const groups = new Map();
  for (const convo of list) {
    const key = convo.projectId || '';
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(convo);
  }
  for (const group of groups.values()) {
    const missing = group.filter((convo) => typeof convo.sortOrder !== 'number');
    if (missing.length === 0) continue;
    changed = true;
    if (missing.length === group.length) {
      group
        .slice()
        .sort((a, b) => b.updatedAt - a.updatedAt)
        .forEach((convo, index) => {
          convo.sortOrder = index;
        });
      continue;
    }
    const max = Math.max(
      ...group
        .filter((convo) => typeof convo.sortOrder === 'number')
        .map((convo) => convo.sortOrder)
    );
    missing
      .slice()
      .sort((a, b) => b.updatedAt - a.updatedAt)
      .forEach((convo, index) => {
        convo.sortOrder = max + 1 + index;
      });
  }
  list._sortOrderMigrated = changed;
  return list;
}

function parseStorePayload(parsed) {
  if (Array.isArray(parsed)) {
    return {
      projects: [],
      conversations: ensureConversationSortOrders(parsed.map(normalizeConversation)),
    };
  }
  if (parsed && typeof parsed === 'object') {
    return {
      projects: Array.isArray(parsed.projects)
        ? parsed.projects.map(normalizeProject)
        : [],
      conversations: ensureConversationSortOrders(
        Array.isArray(parsed.conversations)
          ? parsed.conversations.map(normalizeConversation)
          : []
      ),
    };
  }
  return { projects: [], conversations: [] };
}

function loadStoreFromLocal() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { projects: [], conversations: [] };
    return parseStorePayload(JSON.parse(raw));
  } catch {
    return { projects: [], conversations: [] };
  }
}

function saveStoreToLocal() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(storePayload()));
  } catch {
    // private browsing / quota
  }
}

let projects = [];
let conversations = [];
let dataInfo = null;
let browserStorage = false;
let storageReady = false;
let saveStoreTimer = null;
let saveSettingsTimer = null;

function storePayload() {
  return {
    version: 2,
    projects,
    conversations: conversations.filter((convo) => !convo.incognito),
  };
}

function diskEncryptionLocked() {
  return !!(
    dataInfo
    && dataInfo.encryption_enabled
    && !dataInfo.encryption_unlocked
    && !browserStorage
  );
}

function requireUnlockedData() {
  if (!diskEncryptionLocked()) return true;
  promptUnlockSession();
  return false;
}

function saveStore() {
  if (!storageReady || diskEncryptionLocked()) return;
  if (browserStorage) {
    saveStoreToLocal();
    return;
  }
  clearTimeout(saveStoreTimer);
  saveStoreTimer = setTimeout(() => {
    if (diskEncryptionLocked()) return;
    fetch('/api/data/store', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(storePayload()),
    }).catch(() => {});
  }, 120);
}

function saveConversations() {
  saveStore();
}

function provisionalTitle(text) {
  const src = String(text || '').replace(/\s+/g, ' ').trim() || 'New chat';
  return src.length > 48 ? src.slice(0, 48).trim() + '…' : src;
}

function branchConversationTitle(source) {
  if (source?.incognito) return 'Ghost Chat';
  const base = String(source?.title || '').trim() || 'chat';
  return provisionalTitle('Branch · ' + base);
}

function cloneConversationMessages(messages) {
  const list = Array.isArray(messages) ? messages : [];
  try {
    return structuredClone(list);
  } catch {
    return JSON.parse(JSON.stringify(list));
  }
}

function canBranchFromActiveConversation() {
  const convo = conversations.find((item) => item.id === activeId);
  return !!(convo && Array.isArray(convo.messages) && convo.messages.length > 0);
}

async function requestGeneratedTitle(userText) {
  const body = { message: String(userText || '').trim() };
  const remote = selectedRemoteModel(latestState);
  if (remote) {
    body.remote_base = remote.base;
    body.model = remote.model;
  }
  const response = await fetch('/api/chat/title', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    const problem = await response.json().catch(() => ({}));
    throw new Error(problem.error || 'Could not generate title');
  }
  const payload = await response.json();
  const title = String(payload.title || '').trim();
  if (!title) throw new Error('empty title');
  return title;
}

function firstUserText(convo) {
  const msg = (convo?.messages || []).find((item) => item.role === 'user');
  if (!msg) return '';
  return parseCapabilityMentions(String(msg.content || '')).text.trim();
}

function needsGeneratedTitle(convo) {
  if (!convo || convo.incognito) return false;
  if (convo.titleEdited) return false;
  const userText = firstUserText(convo);
  if (!userText) return false;
  const current = String(convo.title || '').trim();
  if (!current || current === 'New chat') return true;
  // Still the provisional first-message slug — replace with a model title.
  return current === provisionalTitle(userText)
    || current === provisionalTitle(String(
      (convo.messages || []).find((item) => item.role === 'user')?.content || ''
    ));
}

/** @type {Map<HTMLElement, number>} */
const titleTypeTimers = new Map();

function stopTitleTyping(el) {
  if (!el) return;
  const timer = titleTypeTimers.get(el);
  if (timer) {
    clearTimeout(timer);
    titleTypeTimers.delete(el);
  }
  el.classList.remove('is-typing-title');
}

function typeTitleInto(el, fullText) {
  if (!el) return;
  stopTitleTyping(el);
  const text = String(fullText || '');
  if (!text) {
    el.textContent = '';
    return;
  }
  if (window.matchMedia('(prefers-reduced-motion: reduce)').matches) {
    el.textContent = text;
    return;
  }
  el.classList.add('is-typing-title');
  el.textContent = '';
  let i = 0;
  const tick = () => {
    i += 1;
    el.textContent = text.slice(0, i);
    if (i < text.length) {
      const delay = i === 1 ? 40 : 22 + Math.floor(Math.random() * 28);
      const id = window.setTimeout(tick, delay);
      titleTypeTimers.set(el, id);
    } else {
      titleTypeTimers.delete(el);
      el.classList.remove('is-typing-title');
    }
  };
  tick();
}

function revealGeneratedTitle(convo, title) {
  const sidebarLabel = title;
  let sidebarEl = document.querySelector(
    '.convo-item[data-convo-id="' + CSS.escape(convo.id) + '"] .convo-title'
  );
  if (!sidebarEl) {
    renderSidebar();
    sidebarEl = document.querySelector(
      '.convo-item[data-convo-id="' + CSS.escape(convo.id) + '"] .convo-title'
    );
  }
  if (sidebarEl) {
    sidebarEl.title = title;
    const row = sidebarEl.closest('.convo-item');
    if (row) row.title = title;
    typeTitleInto(sidebarEl, sidebarLabel);
  } else renderSidebar();
  if (activeId === convo.id && convoTitleEl && !convo.incognito) {
    typeTitleInto(convoTitleEl, title);
  }
}

function generateConversationTitle(convo, userText) {
  if (convo?.incognito) return;
  const text = String(userText || firstUserText(convo) || '').trim();
  if (!convo || !text) return;
  if (convo._titleBusy) return;
  const requestId = (convo._titleReq = (convo._titleReq || 0) + 1);
  convo._titleBusy = true;
  void (async () => {
    try {
      const title = await requestGeneratedTitle(text);
      if (convo._titleReq !== requestId) return;
      if (!conversations.some((item) => item.id === convo.id)) return;
      // Ignore useless echo of the user message.
      if (title.toLowerCase() === text.toLowerCase()) return;
      convo.title = title;
      saveConversations();
      revealGeneratedTitle(convo, title);
    } catch (error) {
      console.warn('Chat title generation failed:', error?.message || error);
    } finally {
      if (convo._titleReq === requestId) convo._titleBusy = false;
    }
  })();
}

// null = draft: nothing is written to storage until the first message is sent,
// so opening the page does not litter the sidebar with empty conversations.
let activeId = null;
let draftIncognito = false;
/** Project context for drafts / New chat. null = general Recents. */
let activeProjectId = null;
/** 'chat' | 'projects' */
let mainView = 'chat';
let suppressUrlSync = false;
let editingProjectId = null;
let creatingProject = false;
let openConvoMenu = null;
let serverReady = false;
/**
 * In-flight assistant turns keyed by conversation id.
 * Multiple chats can stream at once; the composer Stop/Send chrome follows
 * whichever conversation is currently active.
 * @type {Map<string, {
 *   controller: AbortController,
 *   useAgent: boolean,
 *   partial: string,
 *   timeline: Array<
 *     | {type:'think', content:string}
 *     | {type:'text', content:string}
 *     | {type:'notice', content:string}
 *     | {type:'tool', name:string, detail:string, result:string, note?:string, live:boolean}
 *   >,
 *   errorMessage: string|null,
 *   dom: null|{row:HTMLElement,statusEl:HTMLElement,thinkingLabel:HTMLElement,traceEl:HTMLElement,answerEl:HTMLElement,thinkingOrb:any},
 * }>}
 */
const activeStreams = new Map();
/** @type {Map<string, Array<{
 *   id: string,
 *   editText: string,
 *   displayText: string,
 *   apiText: string|object,
 *   attachments: Array<object>,
 *   turn: { useAgent: boolean, skills: object, deepResearch: boolean, deepResearchOutput: string, forceTools: string[] }
 * }>>} */
const outboundQueues = new Map();
/** Queue item id currently open in the in-thread editor, if any. */
let editingQueueId = null;
/** Remember pin state per conversation across switches. */
const stickByConvo = new Map();

function normalizeSettings(parsed) {
  if (!parsed || typeof parsed !== 'object') return { ...DEFAULT_SETTINGS };
  const maxChars = Number(parsed.attachmentMaxChars);
  const fetchUrlMaxChars = Number(parsed.fetchUrlMaxChars);
  const webSearchPageMaxChars = Number(parsed.webSearchPageMaxChars);
  const searchResults = Number(parsed.webSearchResults);
  const searchRegion = String(parsed.webSearchRegion || '').trim().toLowerCase();
  return {
    name: typeof parsed.name === 'string' ? parsed.name : '',
    about: typeof parsed.about === 'string' ? parsed.about : '',
    instructions: typeof parsed.instructions === 'string' ? parsed.instructions : '',
    memory: typeof parsed.memory === 'string' ? parsed.memory : '',
    thinking: ['collapsed', 'hidden', 'visible'].includes(parsed.thinking)
      ? parsed.thinking
      : 'collapsed',
    thinkingEffort: THINKING_EFFORTS.includes(parsed.thinkingEffort)
      ? parsed.thinkingEffort
      : DEFAULT_SETTINGS.thinkingEffort,
    enterSends: parsed.enterSends !== false,
    skillWebSearch: parsed.skillWebSearch !== false,
    webSearchDepth: WEB_SEARCH_DEPTHS.includes(parsed.webSearchDepth)
      ? parsed.webSearchDepth
      : DEFAULT_SETTINGS.webSearchDepth,
    webSearchBackend: WEB_SEARCH_BACKENDS.includes(parsed.webSearchBackend)
      ? parsed.webSearchBackend
      : DEFAULT_SETTINGS.webSearchBackend,
    webSearchResults: Number.isFinite(searchResults)
      ? Math.min(20, Math.max(1, Math.round(searchResults)))
      : DEFAULT_SETTINGS.webSearchResults,
    webSearchRegion: /^[a-z]{2}-[a-z]{2}$/.test(searchRegion)
      ? searchRegion
      : DEFAULT_SETTINGS.webSearchRegion,
    webSearchSafeSearch: WEB_SEARCH_SAFESEARCH.includes(parsed.webSearchSafeSearch)
      ? parsed.webSearchSafeSearch
      : DEFAULT_SETTINGS.webSearchSafeSearch,
    webSearchRecency: WEB_SEARCH_RECENCIES.includes(parsed.webSearchRecency)
      ? parsed.webSearchRecency
      : DEFAULT_SETTINGS.webSearchRecency,
    skillFetchUrl: parsed.skillFetchUrl !== false,
    fetchUrlMaxChars: Number.isFinite(fetchUrlMaxChars)
      ? Math.min(200000, Math.max(1000, Math.round(fetchUrlMaxChars)))
      : DEFAULT_SETTINGS.fetchUrlMaxChars,
    webSearchPageMaxChars: Number.isFinite(webSearchPageMaxChars)
      ? (Math.round(webSearchPageMaxChars) <= 0
        ? 0
        : Math.min(200000, Math.max(1000, Math.round(webSearchPageMaxChars))))
      : DEFAULT_SETTINGS.webSearchPageMaxChars,
    skillDeepResearch: parsed.skillDeepResearch !== false,
    agentMode: parsed.agentMode === true,
    deepResearch: DEEP_RESEARCH_MODES.includes(parsed.deepResearch)
      ? parsed.deepResearch
      : DEFAULT_SETTINGS.deepResearch,
    attachmentsMode: ATTACHMENTS_MODES.includes(parsed.attachmentsMode)
      ? parsed.attachmentsMode
      : DEFAULT_SETTINGS.attachmentsMode,
    attachmentTextFallback: parsed.attachmentTextFallback === true,
    attachmentOcr: parsed.attachmentOcr === true,
    attachmentMaxChars: Number.isFinite(maxChars)
      ? Math.min(500000, Math.max(2000, Math.round(maxChars)))
      : DEFAULT_SETTINGS.attachmentMaxChars,
  };
}

function loadSettingsFromLocal() {
  try {
    const raw = localStorage.getItem(SETTINGS_KEY);
    if (!raw) return { ...DEFAULT_SETTINGS };
    return normalizeSettings(JSON.parse(raw));
  } catch {
    return { ...DEFAULT_SETTINGS };
  }
}

function saveSettingsToLocal() {
  try {
    localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
  } catch {
    // ignore
  }
}

function saveSettings(next) {
  settings = next;
  updateGreeting();
  updateComposerHint();
  syncAgentButton();
  syncResearchControls();
  syncAttachButton();
  if (!storageReady || diskEncryptionLocked()) return;
  if (browserStorage) {
    saveSettingsToLocal();
    return;
  }
  clearTimeout(saveSettingsTimer);
  saveSettingsTimer = setTimeout(() => {
    if (diskEncryptionLocked()) return;
    fetch('/api/data/preferences', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(settings),
    }).catch(() => {});
  }, 120);
}

let settings = { ...DEFAULT_SETTINGS };

function storeIsEmpty(store) {
  return !(store.projects && store.projects.length)
    && !(store.conversations && store.conversations.length);
}

function settingsLookEmpty(value) {
  return !value.name
    && !value.about
    && !value.instructions
    && !value.memory
    && value.thinking === DEFAULT_SETTINGS.thinking
    && value.thinkingEffort === DEFAULT_SETTINGS.thinkingEffort
    && value.enterSends === DEFAULT_SETTINGS.enterSends
    && value.skillWebSearch === DEFAULT_SETTINGS.skillWebSearch
    && value.webSearchDepth === DEFAULT_SETTINGS.webSearchDepth
    && value.webSearchBackend === DEFAULT_SETTINGS.webSearchBackend
    && value.webSearchResults === DEFAULT_SETTINGS.webSearchResults
    && value.webSearchRegion === DEFAULT_SETTINGS.webSearchRegion
    && value.webSearchSafeSearch === DEFAULT_SETTINGS.webSearchSafeSearch
    && value.webSearchRecency === DEFAULT_SETTINGS.webSearchRecency
    && value.skillFetchUrl === DEFAULT_SETTINGS.skillFetchUrl
    && value.fetchUrlMaxChars === DEFAULT_SETTINGS.fetchUrlMaxChars
    && value.webSearchPageMaxChars === DEFAULT_SETTINGS.webSearchPageMaxChars
    && value.skillDeepResearch === DEFAULT_SETTINGS.skillDeepResearch
    && value.agentMode === DEFAULT_SETTINGS.agentMode
    && value.deepResearch === DEFAULT_SETTINGS.deepResearch
    && value.attachmentsMode === DEFAULT_SETTINGS.attachmentsMode
    && value.attachmentTextFallback === DEFAULT_SETTINGS.attachmentTextFallback
    && value.attachmentOcr === DEFAULT_SETTINGS.attachmentOcr
    && value.attachmentMaxChars === DEFAULT_SETTINGS.attachmentMaxChars;
}

function refreshLocalDataPane() {
  const lede = document.getElementById('settingsDataLede');
  const pathEl = document.getElementById('settingsDataPath');
  const filesEl = document.getElementById('settingsDataFiles');
  const openBtn = document.getElementById('btnOpenDataDir');
  const toggle = document.getElementById('settingBrowserStorage');
  const hint = document.getElementById('settingsStorageHint');
  const personalizationLede = document.querySelector(
    '.settings-pane[data-settings-pane="personalization"] .settings-pane-lede'
  );

  if (toggle) toggle.checked = !!browserStorage;
  if (lede) {
    lede.textContent = browserStorage
      ? 'Chats, projects, and settings are stored in this browser profile only.'
      : 'Chats, projects, and settings are stored on disk in your OS data folder.';
  }
  if (personalizationLede) {
    personalizationLede.textContent = browserStorage
      ? 'Saved in this browser. Sent as a system prompt with each message.'
      : 'Saved on disk with your local data. Sent as a system prompt with each message.';
  }
  if (pathEl) {
    pathEl.textContent = dataInfo?.data_dir || (browserStorage ? 'Browser localStorage' : '—');
  }
  if (filesEl) {
    if (browserStorage) {
      filesEl.textContent = 'Browser mode ignores the data folder for chats and settings. Providers, appearance, and skills still use disk.';
    } else if (dataInfo) {
      filesEl.textContent = dataInfo.encryption_enabled
        ? 'Chats, preferences, provider tokens, and skill contents are encrypted. Non-secret app configuration remains in config.toml.'
        : 'Includes config.toml, chats.json, preferences.json, and chat-skills/. Providers & appearance stay in config.toml.';
    } else {
      filesEl.textContent = '—';
    }
  }
  if (openBtn) {
    openBtn.textContent = dataInfo?.open_label || 'Open folder';
    openBtn.disabled = !!browserStorage || !dataInfo?.data_dir;
  }
  if (hint) {
    hint.textContent = browserStorage
      ? 'On: chats and settings stay in this browser only. Providers/skills remain on disk.'
      : 'Off: chats and settings are saved under the data folder above.';
  }
  refreshEncryptionPane();
}

function refreshEncryptionPane() {
  const browserHint = document.getElementById('settingsEncryptionBrowserHint');
  const statusEl = document.getElementById('settingsEncryptionStatus');
  const enableEl = document.getElementById('settingsEncryptionEnable');
  const unlockEl = document.getElementById('settingsEncryptionUnlock');
  const activeEl = document.getElementById('settingsEncryptionActive');
  const disableForm = document.getElementById('settingsEncryptionDisable');
  if (!statusEl || !enableEl || !unlockEl || !activeEl) return;

  const enabled = !!(dataInfo && dataInfo.encryption_enabled);
  const unlocked = !!(dataInfo && dataInfo.encryption_unlocked);
  const browser = !!browserStorage;

  if (browserHint) browserHint.classList.toggle('is-hidden', !browser);

  enableEl.classList.add('is-hidden');
  unlockEl.classList.add('is-hidden');
  activeEl.classList.add('is-hidden');
  if (disableForm) disableForm.classList.add('is-hidden');

  if (browser) {
    statusEl.textContent = enabled
      ? 'On for disk files, but browser mode is active — chats in this session use localStorage.'
      : 'Unavailable in browser localStorage mode.';
    return;
  }

  if (!enabled) {
    statusEl.textContent = 'Off — chats, settings, provider tokens, and skills are stored unencrypted on disk.';
    enableEl.classList.remove('is-hidden');
    return;
  }

  if (!unlocked) {
    statusEl.textContent = 'Locked — enter your passphrase to read and write encrypted data.';
    unlockEl.classList.remove('is-hidden');
    return;
  }

  statusEl.textContent = 'Unlocked — chats, settings, provider tokens, and skill contents are encrypted on disk.';
  activeEl.classList.remove('is-hidden');
}

async function postEncryption(path, body) {
  const response = await fetch(path, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body || {}),
  });
  const payload = await response.json().catch(() => ({}));
  if (!response.ok) {
    throw new Error(payload.error || 'Encryption request failed');
  }
  dataInfo = payload;
  browserStorage = !!dataInfo.browser_storage;
  refreshLocalDataPane();
  refreshSettingsDataSummary();
  return dataInfo;
}

async function initLocalData() {
  try {
    const response = await fetch('/api/data');
    if (response.ok) dataInfo = await response.json();
  } catch {
    dataInfo = null;
  }
  browserStorage = !!(dataInfo && dataInfo.browser_storage);

  if (browserStorage || !dataInfo) {
    browserStorage = true;
    const store = loadStoreFromLocal();
    projects = store.projects;
    conversations = store.conversations;
    settings = loadSettingsFromLocal();
  } else if (dataInfo.encryption_enabled && !dataInfo.encryption_unlocked) {
    projects = [];
    conversations = [];
    settings = { ...DEFAULT_SETTINGS };
    // Stay on disk mode but wait for unlock before reading encrypted files.
  } else {
    try {
      const storeRes = await fetch('/api/data/store');
      if (storeRes.status === 403) {
        const problem = await storeRes.json().catch(() => ({}));
        if (problem.code === 'encrypted_locked') {
          projects = [];
          conversations = [];
          settings = { ...DEFAULT_SETTINGS };
          if (dataInfo) dataInfo.encryption_unlocked = false;
        } else {
          throw new Error(problem.error || 'Could not load chats');
        }
      } else {
        let store = storeRes.ok ? parseStorePayload(await storeRes.json()) : { projects: [], conversations: [] };
        if (storeIsEmpty(store)) {
          const local = loadStoreFromLocal();
          if (!storeIsEmpty(local)) {
            store = local;
            await fetch('/api/data/store', {
              method: 'PUT',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify({
                version: 2,
                projects: store.projects,
                conversations: store.conversations,
              }),
            });
          }
        }
        projects = store.projects;
        conversations = store.conversations;

        const prefRes = await fetch('/api/data/preferences');
        let prefs = prefRes.ok ? normalizeSettings(await prefRes.json()) : { ...DEFAULT_SETTINGS };
        if (settingsLookEmpty(prefs)) {
          const localPrefs = loadSettingsFromLocal();
          if (!settingsLookEmpty(localPrefs)) {
            prefs = localPrefs;
            await fetch('/api/data/preferences', {
              method: 'PUT',
              headers: { 'content-type': 'application/json' },
              body: JSON.stringify(prefs),
            });
          }
        }
        settings = prefs;
      }
    } catch {
      browserStorage = true;
      const store = loadStoreFromLocal();
      projects = store.projects;
      conversations = store.conversations;
      settings = loadSettingsFromLocal();
    }
  }

  storageReady = true;
  if (conversations._sortOrderMigrated) {
    delete conversations._sortOrderMigrated;
    saveStore();
  }
  refreshLocalDataPane();
  syncAgentButton();
  syncResearchControls();
  if (diskEncryptionLocked()) promptUnlockSession();
}

function setUnlockModalError(message) {
  const el = document.getElementById('unlockModalError');
  if (!el) return;
  el.textContent = message || '';
  el.classList.toggle('is-hidden', !message);
}

function promptUnlockSession() {
  const modal = document.getElementById('unlockModal');
  if (!modal) return;
  closeSettings();
  setUnlockModalError('');
  const input = document.getElementById('unlockModalPassphrase');
  if (input) input.value = '';
  openBackdrop(modal);
  queueMicrotask(() => input?.focus());
}

function hideUnlockSession() {
  const modal = document.getElementById('unlockModal');
  if (!modal) return;
  closeBackdrop(modal);
  setUnlockModalError('');
  const input = document.getElementById('unlockModalPassphrase');
  if (input) input.value = '';
}

function refreshUiFromMemoryStore() {
  updateGreeting();
  updateComposerHint();
  syncAgentButton();
  syncResearchControls();
  if (settingsModal && !settingsModal.classList.contains('is-hidden')) {
    fillSettingsFormFromState();
  }
  applyLocationRoute();
  refreshSettingsDataSummary();
}

function clearMemoryAfterLock() {
  clearTimeout(saveStoreTimer);
  clearTimeout(saveSettingsTimer);
  abortAllStreams();
  activeStreams.clear();
  outboundQueues.clear();
  editingQueueId = null;
  stickByConvo.clear();
  projects = [];
  conversations = [];
  activeProjectId = null;
  settings = { ...DEFAULT_SETTINGS };
  refreshUiFromMemoryStore();
  promptUnlockSession();
}

async function loadDiskDataAfterUnlock() {
  const storeRes = await fetch('/api/data/store');
  if (!storeRes.ok) {
    const problem = await storeRes.json().catch(() => ({}));
    throw new Error(problem.error || 'Could not load chats');
  }
  const store = parseStorePayload(await storeRes.json());
  projects = store.projects;
  conversations = store.conversations;
  const prefRes = await fetch('/api/data/preferences');
  settings = prefRes.ok ? normalizeSettings(await prefRes.json()) : { ...DEFAULT_SETTINGS };
  hideUnlockSession();
  refreshUiFromMemoryStore();
}

async function unlockDiskEncryption(passphrase, buttonEl) {
  if (buttonEl) buttonEl.disabled = true;
  try {
    await postEncryption('/api/data/encryption/unlock', { passphrase });
    const settingsInput = document.getElementById('encryptionUnlockPassphrase');
    if (settingsInput) settingsInput.value = '';
    const modalInput = document.getElementById('unlockModalPassphrase');
    if (modalInput) modalInput.value = '';
    setUnlockModalError('');
    await loadDiskDataAfterUnlock();
  } finally {
    if (buttonEl) buttonEl.disabled = false;
  }
}

async function setBrowserStorageMode(enabled) {
  // Flush current in-memory data into the destination before switching.
  if (enabled) {
    saveStoreToLocal();
    saveSettingsToLocal();
  } else {
    await fetch('/api/data/store', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(storePayload()),
    });
    await fetch('/api/data/preferences', {
      method: 'PUT',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(settings),
    });
  }
  const response = await fetch('/api/data', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ browser_storage: !!enabled }),
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    throw new Error(body.error || 'Could not update storage mode');
  }
  dataInfo = await response.json();
  browserStorage = !!dataInfo.browser_storage;
  refreshLocalDataPane();
  refreshSettingsDataSummary();
}

function syncAgentButton() {
  renderPlusMenu();
  renderComposerModes();
  renderComposerMentions();
  syncPlusButton();
}

function syncPlusButton() {
  if (!btnPlus) return;
  const active = !!settings.agentMode
    || settings.deepResearch === 'long'
    || settings.deepResearch === 'brief'
    || composerMentionIds.has('web_search')
    || composerMentionIds.has('fetch_url')
    || composerMentionIds.has('deep_research');
  btnPlus.classList.toggle('is-active', active);
}

function getProject(id) {
  if (!id) return null;
  return projects.find((project) => project.id === id) || null;
}

/** Project for the active conversation, else the sidebar project context. */
function currentProjectId() {
  const convo = conversations.find((item) => item.id === activeId);
  if (convo && convo.projectId) return convo.projectId;
  return activeProjectId;
}

function buildSystemPrompt(projectIdOverride, { excludeConvoId = null } = {}) {
  const P = window.TENSORUI_PROMPTS || {};
  const fill = window.fillPrompt || ((t) => t);
  const parts = [];
  const name = settings.name.trim();
  const about = settings.about.trim();
  const instructions = settings.instructions.trim();
  const globalMemory = typeof settings.memory === 'string' ? settings.memory.trim() : '';

  const project = getProject(
    projectIdOverride !== undefined ? projectIdOverride : currentProjectId()
  );
  const projectOnly = !!(project && project.memoryMode === 'project_only');

  if (name) parts.push(fill(P['chat.userName'], { name }));
  if (about) parts.push(fill(P['chat.userAbout'], { about }));

  // ChatGPT-style: project instructions override global custom instructions.
  if (project) {
    const projectInstructions = project.instructions.trim();
    if (projectInstructions) {
      parts.push(fill(P['chat.projectInstructions'], {
        name: project.name,
        instructions: projectInstructions,
      }));
    } else if (instructions) {
      parts.push(fill(P['chat.globalInstructions'], { instructions }));
    }
  } else if (instructions) {
    parts.push(fill(P['chat.globalInstructions'], { instructions }));
  }

  if (!projectOnly) {
    parts.push(fill(P['chat.globalMemory'], {
      memory: globalMemory || '(empty)',
    }));
  }

  if (project) {
    const memory = project.memory.trim();
    const scopeNote = projectOnly
      ? (P['chat.projectMemoryScopeProjectOnly'] || '')
      : (P['chat.projectMemoryScopeDefault'] || '');
    parts.push(fill(P['chat.projectMemory'], {
      name: project.name,
      scope_note: scopeNote,
      memory: memory || '(empty)',
    }));

    const continuity = buildProjectContinuityDigest(project.id, excludeConvoId);
    if (continuity) parts.push(continuity);
  }

  if (parts.length === 0) return null;
  const base = P['chat.base'] || 'You are a helpful assistant.';
  return base + '\n\n' + parts.join('\n\n');
}

function messagePlainExcerpt(message, maxChars) {
  if (!message) return '';
  let content = typeof message.content === 'string' ? message.content : '';
  if (!content && Array.isArray(message.content)) {
    content = message.content
      .map((part) => {
        if (typeof part === 'string') return part;
        if (part && typeof part.text === 'string') return part.text;
        return '';
      })
      .filter(Boolean)
      .join(' ');
  }
  if (message.role === 'assistant') {
    content = stripThinkingTags(content);
  }
  content = String(content || '')
    .replace(/<\/?(?:global_)?memory_update>/gi, '')
    .replace(/\s+/g, ' ')
    .trim();
  if (!content) return '';
  if (content.length > maxChars) return content.slice(0, Math.max(0, maxChars - 1)) + '…';
  return content;
}

/** Recent sibling-chat excerpts so project chats share continuity (minus file sources). */
function buildProjectContinuityDigest(projectId, excludeConvoId) {
  if (!projectId) return null;
  const siblings = conversations
    .filter((convo) => convo.projectId === projectId && convo.id !== excludeConvoId)
    .filter((convo) => Array.isArray(convo.messages) && convo.messages.some((m) => (
      m && (m.role === 'user' || m.role === 'assistant') && messagePlainExcerpt(m, SIBLING_MSG_CHARS)
    )))
    .sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0))
    .slice(0, SIBLING_CHAT_MAX);
  if (!siblings.length) return null;

  let remaining = SIBLING_CHAT_BUDGET;
  const blocks = [];
  for (const convo of siblings) {
    if (remaining < 180) break;
    const title = (convo.title || 'Untitled').trim() || 'Untitled';
    const lines = ['### Chat: "' + title + '"'];
    const msgs = (convo.messages || [])
      .filter((m) => m && (m.role === 'user' || m.role === 'assistant'))
      .slice(-SIBLING_MSGS_PER_CHAT);
    for (const message of msgs) {
      const text = messagePlainExcerpt(message, SIBLING_MSG_CHARS);
      if (!text) continue;
      lines.push((message.role === 'user' ? 'User' : 'Assistant') + ': ' + text);
    }
    if (lines.length < 2) continue;
    let block = lines.join('\n');
    if (block.length > remaining) {
      block = block.slice(0, Math.max(0, remaining - 1)) + '…';
      blocks.push(block);
      break;
    }
    blocks.push(block);
    remaining -= block.length + 2;
  }
  if (!blocks.length) return null;
  const P = window.TENSORUI_PROMPTS || {};
  const fill = window.fillPrompt || ((t) => t);
  return fill(P['chat.projectContinuity'] || (
    'Other chats in this project (for multi-chat continuity — prior context from sibling chats, not the current conversation):\n' +
    'Use these when the user refers to earlier work in the project. Do not invent details that are not present.\n\n{{blocks}}'
  ), { blocks: blocks.join('\n\n') });
}

/**
 * Pull memory update blocks out of assistant text.
 * While streaming, hide an unclosed opener so the XML never flashes in the UI.
 */
function applyMemoryUpdateProtocol(text, { streaming = false } = {}) {
  let cleaned = text || '';
  let memory = null;
  let globalMemory = null;
  cleaned = cleaned.replace(/<global_memory_update>\s*([\s\S]*?)\s*<\/global_memory_update>/gi, (_, body) => {
    const next = String(body || '').trim();
    if (next) globalMemory = next;
    return '';
  });
  cleaned = cleaned.replace(/<memory_update>\s*([\s\S]*?)\s*<\/memory_update>/gi, (_, body) => {
    const next = String(body || '').trim();
    if (next) memory = next;
    return '';
  });
  if (streaming) {
    const lower = cleaned.toLowerCase();
    const globalOpen = lower.lastIndexOf('<global_memory_update>');
    const projectOpen = lower.lastIndexOf('<memory_update>');
    const open = Math.max(globalOpen, projectOpen);
    if (open !== -1) cleaned = cleaned.slice(0, open);
  }
  cleaned = cleaned.replace(/\n{3,}/g, '\n\n').replace(/[ \t]+\n/g, '\n').trimEnd();
  return { cleaned, memory, globalMemory };
}

function projectIsProjectOnly(projectId) {
  const project = getProject(projectId);
  return !!(project && project.memoryMode === 'project_only');
}

function persistGlobalMemory(memoryText) {
  const next = String(memoryText || '');
  if ((settings.memory || '') === next) return false;
  saveSettings({ ...settings, memory: next });
  const el = document.getElementById('settingMemory');
  if (el) el.value = next;
  if (settingsModal && !settingsModal.classList.contains('is-hidden')) {
    syncSettingsSaveButton();
  }
  return true;
}

function persistProjectMemory(projectId, memoryText) {
  const project = getProject(projectId);
  if (!project) return false;
  if ((project.memory || '') === String(memoryText || '')) return false;
  project.memory = memoryText;
  project.updatedAt = Date.now();
  saveStore();
  if (editingProjectId === projectId && !projectModal.classList.contains('is-hidden')) {
    document.getElementById('projectMemory').value = memoryText;
  }
  if (mainView === 'projects') renderProjectsPage();
  return true;
}

function applyExtractedMemories(convo, extracted) {
  const result = { globalUpdated: false, projectUpdated: false };
  if (!extracted || convo?.incognito) return result;
  if (extracted.memory != null && convo.projectId) {
    result.projectUpdated = persistProjectMemory(convo.projectId, extracted.memory);
  }
  if (extracted.globalMemory != null && !projectIsProjectOnly(convo.projectId)) {
    result.globalUpdated = persistGlobalMemory(extracted.globalMemory);
  }
  return result;
}

function collectTurnMemoryExtraction(stream, finalText) {
  const chunks = [];
  for (const part of stream?.timeline || []) {
    if (!part) continue;
    if (part.type === 'text' || part.type === 'think') {
      const content = String(part.content || '');
      if (content) chunks.push(content);
    }
  }
  if (finalText) chunks.push(String(finalText));
  const fromAll = applyMemoryUpdateProtocol(chunks.join('\n\n'), { streaming: false });
  for (const part of stream?.timeline || []) {
    if (!part || (part.type !== 'text' && part.type !== 'think')) continue;
    part.content = applyMemoryUpdateProtocol(String(part.content || ''), { streaming: false }).cleaned;
  }
  const fromFinal = applyMemoryUpdateProtocol(String(finalText || ''), { streaming: false });
  return {
    cleaned: fromFinal.cleaned,
    memory: fromAll.memory,
    globalMemory: fromAll.globalMemory,
  };
}

function memoryNoticeLabels(changes) {
  const labels = [];
  if (changes?.globalUpdated) labels.push('Updated long-term memory');
  if (changes?.projectUpdated) labels.push('Updated project memory');
  return labels;
}

function memoryOnlyAssistantFallback(extracted) {
  if (!extracted) return '';
  const projectHit = extracted.memory != null;
  const globalHit = extracted.globalMemory != null;
  if (projectHit && globalHit) return 'Updated memory.';
  if (projectHit) return 'Updated project memory.';
  if (globalHit) return 'Updated long-term memory.';
  return '';
}

function mentionOptionById(id) {
  return MENTION_OPTIONS.find((item) => item.id === id) || null;
}

/** Strip capability mentions; returns { text, mentions: Set<string> }. */
function parseCapabilityMentions(raw) {
  const mentions = new Set();
  const source = String(raw || '');
  source.replace(MENTION_TOKEN_RE, (token) => {
    const id = token.slice(1).toLowerCase();
    if (id === 'agent') {
      // Legacy @agent → treat as all built-in capabilities for this message.
      mentions.add('web_search');
      mentions.add('fetch_url');
    } else if (MENTION_IDS.includes(id)) {
      mentions.add(id);
    }
    return token;
  });
  const text = source
    .replace(/(^|\s)@(?:web_search|fetch_url|deep_research|agent)\b/gi, '$1')
    .replace(/\s{2,}/g, ' ')
    .trim();
  return { text: text || source.trim(), mentions };
}

