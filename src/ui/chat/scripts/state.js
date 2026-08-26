const TRACE_DESKTOP_MQ = window.matchMedia('(min-width: 821px)');

/**
 * Build a synthetic pixel mosaic from an opaque UI identifier. The pattern is
 * deliberately unrelated to the protected text, so it cannot be sharpened or
 * processed back into the original words.
 */
function applyPrivacyMosaic(el, seed, { dense = false } = {}) {
  if (!el) return;
  let state = 2166136261;
  const source = String(seed || 'private-surface');
  for (let i = 0; i < source.length; i += 1) {
    state ^= source.charCodeAt(i);
    state = Math.imul(state, 16777619);
  }
  const random = () => {
    state += 0x6d2b79f5;
    let value = state;
    value = Math.imul(value ^ (value >>> 15), value | 1);
    value ^= value + Math.imul(value ^ (value >>> 7), value | 61);
    return ((value ^ (value >>> 14)) >>> 0) / 4294967296;
  };

  const rows = dense ? 6 : 3;
  const columns = dense ? 6 : (15 + Math.floor(random() * 12));
  const shadows = [];
  for (let row = 0; row < rows; row += 1) {
    for (let column = 0; column < columns; column += 1) {
      if (column > 0 && random() < 0.2) continue;
      const shade = 1 + Math.floor(random() * 4);
      const x = (column * 4).toFixed(1);
      const y = (row * 4).toFixed(1);
      shadows.push(x + 'px ' + y + 'px 0 var(--privacy-pixel-' + shade + ')');
    }
  }
  el.classList.add('privacy-mask');
  if (dense) el.classList.add('privacy-mask-dense');
  el.style.setProperty('--privacy-pixels', shadows.join(', '));
}

function isPrivacyModeOn() {
  return !!document.getElementById('chatShell')?.classList.contains('privacy-mode');
}

/** Native `title` tooltips leak mosaiced identity; keep the value for when privacy turns off. */
function setIdentityTitle(el, text) {
  if (!el) return;
  const value = String(text || '');
  if (value) el.dataset.identityTitle = value;
  else delete el.dataset.identityTitle;
  el.title = isPrivacyModeOn() ? '' : value;
}

function syncIdentityTitles(privacyOn) {
  document.querySelectorAll('[data-identity-title]').forEach((el) => {
    el.title = privacyOn ? '' : (el.dataset.identityTitle || '');
  });
}

const THINKING_EFFORTS = ['auto', 'off', 'low', 'medium', 'high', 'max'];
const WEB_SEARCH_DEPTHS = ['auto', 'off', 'light', 'standard', 'deep'];
const WEB_SEARCH_BACKENDS = ['auto', 'duckduckgo', 'bing', 'wikipedia'];
const WEB_SEARCH_SAFESEARCH = ['on', 'moderate', 'off'];
const WEB_SEARCH_RECENCIES = ['any', 'day', 'week', 'month', 'year'];
const DEEP_RESEARCH_MODES = ['off', 'long', 'brief'];
const ATTACHMENTS_MODES = ['auto', 'on', 'off'];
const ATTACHMENT_MIN_CONTEXT = 16384;
const ATTACHMENT_MAX_FILES = 6;
const ATTACHMENT_MAX_BYTES = 8 * 1024 * 1024;
const CHAT_BACKGROUND_MAX_BYTES = 1024 * 1024;
const CHAT_BACKGROUND_POSITIONS = [
  'center', 'top', 'bottom', 'left', 'right',
  'top left', 'top right', 'bottom left', 'bottom right',
];
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
  webSearchDepth: 'off', // off | auto | light | standard | deep
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
  chatBackgroundImage: '',
  chatBackgroundImageName: '',
  chatBackgroundPosition: 'center',
  chatBackgroundOverlay: 72,
  selectedChatModel: '',
  recentModelIds: [],
  pinnedModelIds: [],
  collapsedModelProviders: [],
  sidebarCollapsed: false,
  privacyMode: false,
  appSurface: 'chat',
  traceActivityShare: 0.6,
  traceActivityFolded: false,
  traceMembersFolded: false,
  updateDismissed: '',
  browserMigrationVersion: 1,
};

const chatShell = document.getElementById('chatShell');
const chatMain = document.querySelector('.chat-main');
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
const encryptionIndicator = document.getElementById('encryptionIndicator');
const encryptionIndicatorLabel = document.getElementById('encryptionIndicatorLabel');
const encryptionIndicatorDetail = document.getElementById('encryptionIndicatorDetail');
const sidebarEncryptionBadge = document.getElementById('sidebarEncryptionBadge');
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
const topbarBotsHold = document.getElementById('topbarBotsHold');
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
const traceSidebarSplit = document.getElementById('traceSidebarSplit');
const traceSplitHandle = document.getElementById('traceSplitHandle');
const traceMembers = document.getElementById('traceMembers');
const traceMembersList = document.getElementById('traceMembersList');
const traceMembersPicker = document.getElementById('traceMembersPicker');
const btnTraceMemberAdd = document.getElementById('btnTraceMemberAdd');
const btnTraceActivityFold = document.getElementById('btnTraceActivityFold');
const btnTraceMembersFold = document.getElementById('btnTraceMembersFold');
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
let repaintEmptyOrb = null;
let chatBackgroundApplyToken = 0;

function normalizeChatBackgroundImage(value) {
  const source = typeof value === 'string' ? value.trim() : '';
  if (!source) return '';
  if (/^data:image\/(?:png|jpe?g|webp|gif|avif);base64,/i.test(source)) {
    const encoded = source.slice(source.indexOf(',') + 1);
    if (encoded.length % 4 !== 0 || !/^[A-Za-z0-9+/]*={0,2}$/.test(encoded)) return '';
    const padding = encoded.endsWith('==') ? 2 : (encoded.endsWith('=') ? 1 : 0);
    const estimatedBytes = Math.floor(encoded.length * 3 / 4) - padding;
    return estimatedBytes <= CHAT_BACKGROUND_MAX_BYTES ? source : '';
  }
  if (source.length > 4096) return '';
  try {
    const url = new URL(source);
    return url.protocol === 'http:' || url.protocol === 'https:' ? url.href : '';
  } catch {
    return '';
  }
}

function chatBackgroundRelativeLuminance(red, green, blue) {
  const linear = (channel) => {
    const value = channel / 255;
    return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * linear(red) + 0.7152 * linear(green) + 0.0722 * linear(blue);
}

function setChatBackgroundTone(luminance, token) {
  if (token !== chatBackgroundApplyToken) return;
  // WCAG contrast curves cross at ~0.179: above it black is more legible,
  // below it white is more legible.
  chatMain.dataset.backgroundTone = luminance > 0.179 ? 'light' : 'dark';
  if (typeof repaintEmptyOrb === 'function') repaintEmptyOrb();
}

function fallbackChatBackgroundTone(overlayOpacity, token) {
  // Cross-origin images without CORS cannot be sampled. A mid-bright image is
  // a better neutral estimate than blindly following the UI theme.
  const visibleChannel = 255 * 0.62 * (1 - overlayOpacity);
  setChatBackgroundTone(
    chatBackgroundRelativeLuminance(visibleChannel, visibleChannel, visibleChannel),
    token
  );
}

function sampleChatBackgroundTone(sampleImage, overlayOpacity, token) {
  if (token !== chatBackgroundApplyToken) return;
  try {
    const canvas = document.createElement('canvas');
    canvas.width = 32;
    canvas.height = 32;
    const context = canvas.getContext('2d', { willReadFrequently: true });
    if (!context) throw new Error('Canvas unavailable');
    context.drawImage(sampleImage, 0, 0, canvas.width, canvas.height);
    const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
    const baseChannel = document.documentElement.dataset.theme === 'light' ? 245 : 30;
    const overlayFactor = 1 - overlayOpacity;
    let luminance = 0;
    let samples = 0;
    for (let index = 0; index < pixels.length; index += 4) {
      const imageAlpha = pixels[index + 3] / 255;
      const red = (pixels[index] * imageAlpha + baseChannel * (1 - imageAlpha)) * overlayFactor;
      const green = (pixels[index + 1] * imageAlpha + baseChannel * (1 - imageAlpha)) * overlayFactor;
      const blue = (pixels[index + 2] * imageAlpha + baseChannel * (1 - imageAlpha)) * overlayFactor;
      luminance += chatBackgroundRelativeLuminance(red, green, blue);
      samples += 1;
    }
    setChatBackgroundTone(luminance / Math.max(1, samples), token);
  } catch {
    fallbackChatBackgroundTone(overlayOpacity, token);
  }
}

function resolveChatBackgroundTone(image, source, overlayOpacity, token) {
  let isSameOrigin = source.startsWith('data:');
  try {
    isSameOrigin = isSameOrigin || new URL(source, window.location.href).origin === window.location.origin;
  } catch {
    // normalizeChatBackgroundImage already rejects malformed sources.
  }
  if (isSameOrigin) {
    sampleChatBackgroundTone(image, overlayOpacity, token);
    return;
  }
  // Use a separate CORS request for analysis so servers without CORS still
  // display normally through the primary image element.
  const sampler = new Image();
  sampler.crossOrigin = 'anonymous';
  sampler.addEventListener('load', () => sampleChatBackgroundTone(sampler, overlayOpacity, token));
  sampler.addEventListener('error', () => fallbackChatBackgroundTone(overlayOpacity, token));
  sampler.src = source;
}

function applyChatBackground(appearance) {
  const host = document.getElementById('chatBackground');
  const image = document.getElementById('chatBackgroundImage');
  const overlay = document.getElementById('chatBackgroundOverlay');
  if (!host || !image || !overlay) return;
  const source = normalizeChatBackgroundImage(appearance?.chatBackgroundImage);
  const position = CHAT_BACKGROUND_POSITIONS.includes(appearance?.chatBackgroundPosition)
    ? appearance.chatBackgroundPosition
    : DEFAULT_SETTINGS.chatBackgroundPosition;
  const opacity = Number(appearance?.chatBackgroundOverlay);
  const normalizedOpacity = Number.isFinite(opacity)
    ? Math.min(100, Math.max(0, Math.round(opacity)))
    : DEFAULT_SETTINGS.chatBackgroundOverlay;
  const overlayOpacity = normalizedOpacity / 100;
  const token = ++chatBackgroundApplyToken;
  image.style.objectPosition = position;
  overlay.style.opacity = String(overlayOpacity);
  if (!source) {
    image.removeAttribute('src');
    host.classList.remove('has-image');
    delete chatMain.dataset.backgroundTone;
    if (typeof repaintEmptyOrb === 'function') repaintEmptyOrb();
    return;
  }
  image.onload = () => {
    if (token !== chatBackgroundApplyToken) return;
    host.classList.add('has-image');
    resolveChatBackgroundTone(image, source, overlayOpacity, token);
  };
  image.onerror = () => {
    if (token !== chatBackgroundApplyToken) return;
    host.classList.remove('has-image');
    delete chatMain.dataset.backgroundTone;
    if (typeof repaintEmptyOrb === 'function') repaintEmptyOrb();
  };
  image.src = source;
  if (image.complete && image.naturalWidth > 0) image.onload();
}

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
      { opacity: 1, transform: 'none' },
    ],
    {
      duration,
      delay,
      easing: 'cubic-bezier(0.22, 1, 0.36, 1)',
      fill: 'backwards',
    }
  );
  const clear = () => {
    el.classList.remove('motion-enter');
    try { anim.cancel(); } catch { /* ignore */ }
  };
  anim.finished.then(clear).catch(clear);
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

let confirmDangerResolver = null;

function settleConfirmDanger(ok) {
  const resolve = confirmDangerResolver;
  confirmDangerResolver = null;
  const modal = document.getElementById('confirmModal');
  if (modal) closeBackdrop(modal);
  if (resolve) resolve(!!ok);
}

function confirmDanger({ title, body, confirmLabel = 'Delete' } = {}) {
  const modal = document.getElementById('confirmModal');
  if (!modal) return Promise.resolve(window.confirm(body || title || 'Are you sure?'));
  if (confirmDangerResolver) settleConfirmDanger(false);
  const titleEl = document.getElementById('confirmModalTitle');
  const bodyEl = document.getElementById('confirmModalBody');
  const okBtn = document.getElementById('btnConfirmModalOk');
  if (titleEl) titleEl.textContent = title || 'Are you sure?';
  if (bodyEl) {
    bodyEl.textContent = body || '';
    bodyEl.classList.toggle('is-hidden', !body);
  }
  if (okBtn) okBtn.textContent = confirmLabel;
  return new Promise((resolve) => {
    confirmDangerResolver = resolve;
    openBackdrop(modal);
    window.requestAnimationFrame(() => okBtn?.focus());
  });
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
    if (prevResolved !== resolved && typeof repaintEmptyOrb === 'function') {
      repaintEmptyOrb();
    }
    if (prevResolved !== resolved && settings?.chatBackgroundImage) {
      applyChatBackground(settings);
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
const topbarBotRoster = document.getElementById('topbarBotRoster');
const btnTopbarGroupEdit = document.getElementById('btnTopbarGroupEdit');
const greetingEl = document.getElementById('greeting');
const modelHintEl = document.getElementById('modelHint');

function setModelHintWithProvider(prefix, providerLabel) {
  if (!modelHintEl) return;
  modelHintEl.replaceChildren(document.createTextNode(String(prefix || '')));
  const provider = String(providerLabel || '').trim();
  if (!provider) return;
  modelHintEl.appendChild(document.createTextNode(' via '));
  const protectedProvider = document.createElement('span');
  protectedProvider.className = 'model-hint-provider';
  protectedProvider.textContent = provider;
  applyPrivacyMosaic(protectedProvider, 'model-hint-provider:' + provider);
  modelHintEl.appendChild(protectedProvider);
}
const serverChip = document.getElementById('serverChip');
const serverProviderName = document.getElementById('serverProviderName');
applyPrivacyMosaic(serverProviderName, 'server-provider');
const chatModelSelect = document.getElementById('chatModelSelect');
const chatModelSelectWrap = document.getElementById('chatModelSelectWrap');
const chatModelOriginPill = document.getElementById('chatModelOriginPill');
const chatModelMenu = document.getElementById('chatModelMenu');
const chatModelList = document.getElementById('chatModelList');
const chatModelSearchWrap = document.getElementById('chatModelSearchWrap');
const chatModelSearch = document.getElementById('chatModelSearch');
const chatModelSearchCount = document.getElementById('chatModelSearchCount');
const chatModelEmpty = document.getElementById('chatModelEmpty');
const RECENT_MODELS_MAX = 12;
const PINNED_MODELS_MAX = 48;
/** Below this many models the filter field is more noise than help. */
const MODEL_SEARCH_MIN_OPTIONS = 6;
const MODEL_PIN_ICON = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 17v5"/><path d="M9 10.76a2 2 0 0 1-1.11 1.79l-1.78.9A2 2 0 0 0 5 15.24V16a1 1 0 0 0 1 1h12a1 1 0 0 0 1-1v-.76a2 2 0 0 0-1.11-1.79l-1.78-.9A2 2 0 0 1 15 10.76V6a1 1 0 0 0-1-1h-4a1 1 0 0 0-1 1z"/></svg>';
let selectedRemoteModelId = '';
let selectedChatModel = '';
let recentModelIds = [];
let pinnedModelIds = [];
let collapsedModelProviders = [];
let modelMenuOptions = [];
/** Options passing the current filter — what the arrow keys actually walk. */
let modelMenuMatches = [];
let modelMenuFilter = '';
let modelMenuActiveIndex = -1;
let modelMenuTab = recentModelIds.length ? 'recents' : (pinnedModelIds.length ? 'pins' : 'cloud');
let latestState = null;

function normalizeModelIds(ids, limit) {
  return [...new Set((Array.isArray(ids) ? ids : [])
    .filter((id) => typeof id === 'string' && id.trim())
    .map((id) => id.trim()))].slice(0, limit);
}

function persistModelPickerState() {
  saveSettings({
    ...settings,
    selectedChatModel,
    recentModelIds: recentModelIds.slice(),
    pinnedModelIds: pinnedModelIds.slice(),
    collapsedModelProviders: collapsedModelProviders.slice(),
  }, { immediate: true });
}

function saveRecentModelIds(ids) {
  recentModelIds = normalizeModelIds(ids, RECENT_MODELS_MAX);
  persistModelPickerState();
}

function rememberRecentModel(value) {
  if (!value) return;
  saveRecentModelIds([value, ...recentModelIds.filter((id) => id !== value)]);
}

function savePinnedModelIds(ids) {
  pinnedModelIds = normalizeModelIds(ids, PINNED_MODELS_MAX);
  persistModelPickerState();
}

function isModelPinned(value) {
  return !!value && pinnedModelIds.includes(value);
}

function isModelProviderCollapsed(key) {
  return !!key && collapsedModelProviders.includes(key);
}

function toggleModelProviderCollapsed(key) {
  if (!key) return;
  if (isModelProviderCollapsed(key)) {
    collapsedModelProviders = collapsedModelProviders.filter((id) => id !== key);
  } else {
    collapsedModelProviders = normalizeModelIds([key, ...collapsedModelProviders], 64);
  }
  persistModelPickerState();
  if (modelMenuIsOpen()) applyModelFilter({ keepActive: true });
}

function togglePinnedModel(value) {
  if (!value) return;
  if (isModelPinned(value)) {
    savePinnedModelIds(pinnedModelIds.filter((id) => id !== value));
    if (modelMenuTab === 'pins' && !pinnedModelIds.length) {
      modelMenuTab = recentModelIds.length ? 'recents' : fallbackModelMenuTab();
    }
  } else {
    savePinnedModelIds([value, ...pinnedModelIds.filter((id) => id !== value)]);
  }
  if (modelMenuIsOpen()) {
    syncModelMenuTabs();
    applyModelFilter({ keepActive: true });
  }
}

function hydrateModelPickerState() {
  selectedChatModel = typeof settings.selectedChatModel === 'string'
    ? settings.selectedChatModel
    : '';
  selectedRemoteModelId = selectedChatModel;
  recentModelIds = normalizeModelIds(settings.recentModelIds, RECENT_MODELS_MAX);
  pinnedModelIds = normalizeModelIds(settings.pinnedModelIds, PINNED_MODELS_MAX);
  collapsedModelProviders = normalizeModelIds(settings.collapsedModelProviders, 64);
  modelMenuTab = recentModelIds.length ? 'recents' : (pinnedModelIds.length ? 'pins' : 'cloud');
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

function normalizeOutboundItem(raw) {
  if (!raw || typeof raw !== 'object') return null;
  const attachments = Array.isArray(raw.attachments) ? raw.attachments : [];
  const displayText = String(raw.displayText || '').trim();
  const editText = String(raw.editText || displayText);
  if (!displayText && !editText && !attachments.length) return null;
  const turn = raw.turn && typeof raw.turn === 'object' ? raw.turn : {};
  return {
    id: typeof raw.id === 'string' && raw.id.trim() ? raw.id : newId('q'),
    editText,
    displayText: displayText || (attachments.length ? '(attachment)' : editText),
    apiText: raw.apiText != null ? raw.apiText : (displayText || editText),
    attachments,
    replyQuote: typeof raw.replyQuote === 'string' ? raw.replyQuote : '',
    replyToSpeakerId: typeof raw.replyToSpeakerId === 'string' ? raw.replyToSpeakerId : '',
    replyToSpeakerHandle: typeof raw.replyToSpeakerHandle === 'string' ? raw.replyToSpeakerHandle : '',
    turn: {
      useAgent: !!turn.useAgent,
      skills: turn.skills && typeof turn.skills === 'object' ? turn.skills : {},
      deepResearch: !!turn.deepResearch,
      deepResearchOutput: turn.deepResearchOutput === 'brief' ? 'brief' : 'long',
      forceTools: Array.isArray(turn.forceTools) ? turn.forceTools : [],
    },
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
    surface: convo.surface === 'bots' ? 'bots' : 'chat',
    botKind: convo.botKind === 'group' || convo.botKind === 'dm' ? convo.botKind : null,
    botId: typeof convo.botId === 'string' ? convo.botId : null,
    participantBotIds: Array.isArray(convo.participantBotIds)
      ? convo.participantBotIds.filter((id) => typeof id === 'string')
      : [],
    groupMemory: typeof convo.groupMemory === 'string' ? convo.groupMemory : '',
    botsHeldBy: typeof convo.botsHeldBy === 'string' ? convo.botsHeldBy : null,
    sideThreadOf: typeof convo.sideThreadOf === 'string' ? convo.sideThreadOf : null,
    outboundQueue: Array.isArray(convo.outboundQueue)
      ? convo.outboundQueue.map(normalizeOutboundItem).filter(Boolean)
      : [],
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
      bots: [],
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
      bots: Array.isArray(parsed.bots) ? parsed.bots.map(normalizeBot) : [],
    };
  }
  return { projects: [], conversations: [], bots: [] };
}

let projects = [];
let conversations = [];
let bots = [];
let dataInfo = null;
let storageReady = false;
let saveStoreTimer = null;
let saveSettingsTimer = null;
let settingsWriteChain = Promise.resolve();
let storeWriteChain = Promise.resolve();
let storageWriteEpoch = 0;
let persistenceWarningAt = 0;

function reportPersistenceFailure(path, error) {
  console.warn('Could not persist ' + path, error);
  const now = Date.now();
  if (now - persistenceWarningAt < 5000 || typeof showComposerHint !== 'function') return;
  persistenceWarningAt = now;
  showComposerHint('Could not save local changes. Check the server and retry.', { warn: true });
}

/**
 * Never set `keepalive` here: chats and the background image push these payloads
 * far past the browser's 64 KiB keepalive quota, and over-quota requests are
 * rejected before they ever reach the server.
 */
async function putJsonWithRetry(path, payload, { attempts = 3, valid = () => true } = {}) {
  const body = JSON.stringify(payload);
  let lastError = null;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    if (!valid()) return false;
    if (attempt) await new Promise((resolve) => setTimeout(resolve, 200 * attempt));
    if (!valid()) return false;
    try {
      const response = await fetch(path, {
        method: 'PUT',
        headers: { 'content-type': 'application/json' },
        body,
      });
      if (response.ok) return true;
      // Most client errors are deterministic; only timeout and throttling merit a retry.
      if (
        response.status >= 400 &&
        response.status < 500 &&
        response.status !== 408 &&
        response.status !== 429
      ) {
        lastError = new Error(path + ' rejected the write (' + response.status + ')');
        break;
      }
      lastError = new Error(path + ' responded ' + response.status);
    } catch (error) {
      lastError = error;
    }
  }
  reportPersistenceFailure(path, lastError);
  return false;
}

function outboundQueueForStore(convoId) {
  const queue = Array.isArray(outboundQueues.get(convoId))
    ? outboundQueues.get(convoId).slice()
    : [];
  const stream = typeof activeStreams !== 'undefined' ? activeStreams.get(convoId) : null;
  const pending = (stream?.pendingSteers || [])
    .filter((entry) => entry?.item && !entry.applied)
    .map((entry) => entry.item);
  if (!pending.length) return queue;
  const seen = new Set(pending.map((item) => item.id));
  return pending.concat(queue.filter((item) => !seen.has(item.id)));
}

function conversationStoreRecord(convo) {
  const record = { ...convo };
  delete record.outboundQueue;
  const queue = outboundQueueForStore(convo.id);
  if (queue.length) record.outboundQueue = queue;
  return record;
}

function restoreOutboundQueues(list) {
  outboundQueues.clear();
  for (const convo of list || []) {
    const items = Array.isArray(convo.outboundQueue)
      ? convo.outboundQueue.map(normalizeOutboundItem).filter(Boolean)
      : [];
    if (items.length) outboundQueues.set(convo.id, items);
    if (convo && Object.prototype.hasOwnProperty.call(convo, 'outboundQueue')) {
      delete convo.outboundQueue;
    }
  }
}

function adoptPersistedOutboundQueue(convo) {
  if (!convo?.id) return;
  const persisted = Array.isArray(convo.outboundQueue)
    ? convo.outboundQueue.map(normalizeOutboundItem).filter(Boolean)
    : [];
  if (Object.prototype.hasOwnProperty.call(convo, 'outboundQueue')) {
    delete convo.outboundQueue;
  }
  if (outboundQueues.has(convo.id)) return;
  if (persisted.length) outboundQueues.set(convo.id, persisted);
}

function persistOutboundQueues() {
  saveConversations({ immediate: true });
}

function storePayload() {
  return {
    version: 2,
    projects,
    conversations: conversations
      .filter((convo) => !convo.incognito)
      .map(conversationStoreRecord),
    bots,
  };
}

function diskEncryptionLocked() {
  return !!(
    dataInfo
    && dataInfo.encryption_enabled
    && !dataInfo.encryption_unlocked
  );
}

function requireUnlockedData() {
  if (!diskEncryptionLocked()) return true;
  promptUnlockSession();
  return false;
}

function enqueueStoreWrite(snapshot) {
  const epoch = storageWriteEpoch;
  storeWriteChain = storeWriteChain
    .catch(() => {})
    .then(() => putJsonWithRetry('/api/data/store', snapshot, {
      valid: () => epoch === storageWriteEpoch && !diskEncryptionLocked(),
    }));
  return storeWriteChain;
}

function saveStore({ immediate = true } = {}) {
  if (!storageReady || diskEncryptionLocked()) return;
  const put = () => {
    if (diskEncryptionLocked()) return;
    enqueueStoreWrite(storePayload());
  };
  clearTimeout(saveStoreTimer);
  saveStoreTimer = null;
  if (immediate) {
    put();
    return;
  }
  saveStoreTimer = setTimeout(() => {
    saveStoreTimer = null;
    put();
  }, 120);
}

function saveConversations({ immediate = true } = {}) {
  saveStore({ immediate });
}

/** Best-effort flush when a tab is backgrounded; normal user actions save earlier. */
function flushPendingWrites() {
  if (!storageReady || diskEncryptionLocked()) return;
  if (saveStoreTimer) {
    clearTimeout(saveStoreTimer);
    saveStoreTimer = null;
    enqueueStoreWrite(storePayload());
  }
  if (saveSettingsTimer) {
    clearTimeout(saveSettingsTimer);
    saveSettingsTimer = null;
    enqueueSettingsWrite({ ...settings });
  }
}

document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'hidden') flushPendingWrites();
});
window.addEventListener('pagehide', flushPendingWrites);

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
  if (typeof isBotsConvo === 'function' && isBotsConvo(convo)) return false;
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
  el.querySelector('.title-typing-caret')?.remove();
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
  const textNode = document.createTextNode('');
  const caret = document.createElement('span');
  caret.className = 'title-typing-caret';
  caret.setAttribute('aria-hidden', 'true');
  el.append(textNode, caret);
  let i = 0;
  const tick = () => {
    i += 1;
    textNode.data = text.slice(0, i);
    if (i < text.length) {
      const delay = i === 1 ? 40 : 22 + Math.floor(Math.random() * 28);
      const id = window.setTimeout(tick, delay);
      titleTypeTimers.set(el, id);
    } else {
      titleTypeTimers.delete(el);
      el.classList.remove('is-typing-title');
      caret.remove();
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
    setIdentityTitle(sidebarEl, title);
    const row = sidebarEl.closest('.convo-item');
    if (row) setIdentityTitle(row, title);
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
/** 'chat' | 'bots' — wordmark surface. Bots is a label-only destination for now. */
let appSurface = 'chat';
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
  const chatBackgroundOverlay = Number(parsed.chatBackgroundOverlay);
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
    chatBackgroundImage: normalizeChatBackgroundImage(parsed.chatBackgroundImage),
    chatBackgroundImageName: typeof parsed.chatBackgroundImageName === 'string'
      ? parsed.chatBackgroundImageName.slice(0, 255)
      : '',
    chatBackgroundPosition: CHAT_BACKGROUND_POSITIONS.includes(parsed.chatBackgroundPosition)
      ? parsed.chatBackgroundPosition
      : DEFAULT_SETTINGS.chatBackgroundPosition,
    chatBackgroundOverlay: Number.isFinite(chatBackgroundOverlay)
      ? Math.min(100, Math.max(0, Math.round(chatBackgroundOverlay)))
      : DEFAULT_SETTINGS.chatBackgroundOverlay,
    selectedChatModel: typeof parsed.selectedChatModel === 'string'
      ? parsed.selectedChatModel.trim()
      : '',
    recentModelIds: normalizeModelIds(parsed.recentModelIds, RECENT_MODELS_MAX),
    pinnedModelIds: normalizeModelIds(parsed.pinnedModelIds, PINNED_MODELS_MAX),
    collapsedModelProviders: normalizeModelIds(parsed.collapsedModelProviders, 64),
    sidebarCollapsed: parsed.sidebarCollapsed === true,
    privacyMode: parsed.privacyMode === true,
    appSurface: parsed.appSurface === 'bots' ? 'bots' : 'chat',
    traceActivityShare: (() => {
      const n = Number(parsed.traceActivityShare);
      if (!Number.isFinite(n)) return DEFAULT_SETTINGS.traceActivityShare;
      return Math.min(0.78, Math.max(0.22, n));
    })(),
    traceActivityFolded: parsed.traceActivityFolded === true,
    traceMembersFolded: parsed.traceMembersFolded === true,
    updateDismissed: typeof parsed.updateDismissed === 'string'
      ? parsed.updateDismissed.trim()
      : '',
    browserMigrationVersion: Number.isInteger(parsed.browserMigrationVersion)
      ? parsed.browserMigrationVersion
      : 0,
  };
}

function enqueueSettingsWrite(snapshot) {
  const epoch = storageWriteEpoch;
  settingsWriteChain = settingsWriteChain
    .catch(() => {})
    .then(() => putJsonWithRetry('/api/data/preferences', snapshot, {
      valid: () => epoch === storageWriteEpoch && !diskEncryptionLocked(),
    }));
  return settingsWriteChain;
}

function applySettingsInMemory(next) {
  settings = next;
  applyChatBackground(settings);
  updateGreeting();
  updateComposerHint();
  syncAgentButton();
  syncResearchControls();
  syncAttachButton();
}

function saveSettings(next, { immediate = true } = {}) {
  applySettingsInMemory(next);
  if (!storageReady || diskEncryptionLocked()) return Promise.resolve(false);
  clearTimeout(saveSettingsTimer);
  saveSettingsTimer = null;
  if (immediate) {
    return enqueueSettingsWrite({ ...settings });
  }
  saveSettingsTimer = setTimeout(() => {
    saveSettingsTimer = null;
    if (diskEncryptionLocked()) return;
    enqueueSettingsWrite({ ...settings });
  }, 120);
  return Promise.resolve(true);
}

let settings = { ...DEFAULT_SETTINGS };

function refreshLocalDataPane() {
  const lede = document.getElementById('settingsDataLede');
  const pathEl = document.getElementById('settingsDataPath');
  const filesEl = document.getElementById('settingsDataFiles');
  const openBtn = document.getElementById('btnOpenDataDir');
  const personalizationLede = document.querySelector(
    '.settings-pane[data-settings-pane="personalization"] .settings-pane-lede'
  );

  if (lede) {
    lede.textContent = 'Chats, projects, and settings are stored on disk in your OS data folder.';
  }
  if (personalizationLede) {
    personalizationLede.textContent = 'Saved on disk with your local data. Sent as a system prompt with each message.';
  }
  if (pathEl) {
    pathEl.textContent = dataInfo?.data_dir || '—';
  }
  if (filesEl) {
    if (dataInfo) {
      filesEl.textContent = dataInfo.encryption_enabled
        ? 'Chats, preferences, provider configuration and credentials, and skill contents are encrypted. Only non-sensitive boot configuration remains in config.toml.'
        : 'Includes config.toml, chats.json, preferences.json, and chat-skills/. Providers & appearance stay in config.toml.';
    } else {
      filesEl.textContent = '—';
    }
  }
  if (openBtn) {
    openBtn.textContent = dataInfo?.open_label || 'Open folder';
    openBtn.disabled = !dataInfo?.data_dir;
  }
  refreshEncryptionPane();
  refreshEncryptionIndicator();
}

function refreshEncryptionIndicator() {
  if (!encryptionIndicator || !encryptionIndicatorLabel || !encryptionIndicatorDetail) return;
  const enabled = !!dataInfo?.encryption_enabled;
  encryptionIndicator.classList.toggle('is-hidden', !enabled);
  sidebarEncryptionBadge?.classList.toggle('is-hidden', !enabled);
  encryptionIndicator.classList.remove('is-locked', 'is-recovery', 'is-browser');
  sidebarEncryptionBadge?.classList.remove('is-locked', 'is-recovery', 'is-browser');
  const expandSidebar = document.getElementById('btnExpandSidebar');
  if (!enabled) {
    if (expandSidebar) delete expandSidebar.dataset.encryptionDetail;
    if (typeof syncSidebarToggleUi === 'function') syncSidebarToggleUi();
    return;
  }

  let label = 'Encrypted';
  let shortDetail = 'At rest';
  let detail = 'Encrypted at rest · unlocked for this session';
  if (dataInfo.encryption_transition_pending) {
    label = 'Recovery';
    shortDetail = 'Action needed';
    detail = 'Encryption recovery needed · enter your passphrase to finish safely';
    encryptionIndicator.classList.add('is-recovery');
    sidebarEncryptionBadge?.classList.add('is-recovery');
  } else if (!dataInfo.encryption_unlocked) {
    label = 'Locked';
    shortDetail = 'Session locked';
    detail = 'Encrypted at rest · locked for this session';
    encryptionIndicator.classList.add('is-locked');
    sidebarEncryptionBadge?.classList.add('is-locked');
  }
  encryptionIndicatorLabel.textContent = label;
  encryptionIndicatorDetail.textContent = shortDetail;
  encryptionIndicator.setAttribute('aria-label', detail + '. Open encryption settings');
  encryptionIndicator.title = detail + ' — open encryption settings';
  if (sidebarEncryptionBadge) sidebarEncryptionBadge.title = detail;
  if (expandSidebar) expandSidebar.dataset.encryptionDetail = detail;
  if (typeof syncSidebarToggleUi === 'function') syncSidebarToggleUi();
}

function refreshEncryptionPane() {
  const statusEl = document.getElementById('settingsEncryptionStatus');
  const enableEl = document.getElementById('settingsEncryptionEnable');
  const unlockEl = document.getElementById('settingsEncryptionUnlock');
  const activeEl = document.getElementById('settingsEncryptionActive');
  const disableForm = document.getElementById('settingsEncryptionDisable');
  if (!statusEl || !enableEl || !unlockEl || !activeEl) return;

  const enabled = !!(dataInfo && dataInfo.encryption_enabled);
  const unlocked = !!(dataInfo && dataInfo.encryption_unlocked);

  enableEl.classList.add('is-hidden');
  unlockEl.classList.add('is-hidden');
  activeEl.classList.add('is-hidden');
  if (disableForm) disableForm.classList.add('is-hidden');

  if (!enabled) {
    statusEl.textContent = 'Off — chats, settings, provider configuration and credentials, and skills are stored unencrypted on disk.';
    enableEl.classList.remove('is-hidden');
    return;
  }

  if (!unlocked) {
    statusEl.textContent = 'Locked — enter your passphrase to read and write encrypted data.';
    unlockEl.classList.remove('is-hidden');
    return;
  }

  statusEl.textContent = 'Unlocked — chats, settings, provider configuration and credentials, and skill contents are encrypted on disk.';
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
  refreshLocalDataPane();
  refreshSettingsDataSummary();
  return dataInfo;
}

function normalizeLoadedStore(store, rawPreferences) {
  return {
    store,
    preferences: normalizeSettings(
      rawPreferences && typeof rawPreferences === 'object' ? rawPreferences : {}
    ),
  };
}

async function initLocalData() {
  try {
    const response = await fetch('/api/data');
    if (response.ok) dataInfo = await response.json();
  } catch {
    dataInfo = null;
  }
  if (dataInfo?.browser_storage) {
    try {
      const response = await fetch('/api/data', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ browser_storage: false }),
      });
      if (response.ok) dataInfo = await response.json();
    } catch {
      /* Disk is the only storage mode; ignore a failed rewrite of an old flag. */
    }
  }

  if (!dataInfo) {
    projects = [];
    conversations = [];
    bots = [];
    settings = { ...DEFAULT_SETTINGS };
  } else if (dataInfo.encryption_enabled && !dataInfo.encryption_unlocked) {
    projects = [];
    conversations = [];
    bots = [];
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
          bots = [];
          settings = { ...DEFAULT_SETTINGS };
          if (dataInfo) dataInfo.encryption_unlocked = false;
        } else {
          throw new Error(problem.error || 'Could not load chats');
        }
      } else {
        const store = storeRes.ok ? parseStorePayload(await storeRes.json()) : { projects: [], conversations: [], bots: [] };
        const prefRes = await fetch('/api/data/preferences');
        const rawPrefs = prefRes.ok ? await prefRes.json() : {};
        const loaded = normalizeLoadedStore(store, rawPrefs);
        projects = loaded.store.projects;
        conversations = loaded.store.conversations;
        bots = loaded.store.bots || [];
        settings = loaded.preferences;
      }
    } catch {
      projects = [];
      conversations = [];
      bots = [];
      settings = { ...DEFAULT_SETTINGS };
    }
  }

  hydrateModelPickerState();
  restoreOutboundQueues(conversations);
  storageReady = true;
  if (typeof restoreAppSurface === 'function') restoreAppSurface();
  if (typeof loadTraceSplitPrefs === 'function') loadTraceSplitPrefs();
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

function focusUnlockPassphrase() {
  const modal = document.getElementById('unlockModal');
  const input = document.getElementById('unlockModalPassphrase');
  if (!modal || !input || modal.classList.contains('is-hidden')) return;
  requestAnimationFrame(() => {
    if (!modal.classList.contains('is-hidden')) input.focus({ preventScroll: true });
  });
}

function promptUnlockSession() {
  const modal = document.getElementById('unlockModal');
  if (!modal) return;
  closeSettings();
  setUnlockModalError('');
  const input = document.getElementById('unlockModalPassphrase');
  if (input) input.value = '';
  openBackdrop(modal);
  focusUnlockPassphrase();
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
  applyChatBackground(settings);
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
  storageWriteEpoch += 1;
  clearTimeout(saveStoreTimer);
  saveStoreTimer = null;
  clearTimeout(saveSettingsTimer);
  saveSettingsTimer = null;
  abortAllStreams({ cancelServer: false });
  activeStreams.clear();
  outboundQueues.clear();
  editingQueueId = null;
  stickByConvo.clear();
  selectedChatModel = '';
  selectedRemoteModelId = '';
  recentModelIds = [];
  pinnedModelIds = [];
  collapsedModelProviders = [];
  modelMenuOptions = [];
  modelMenuMatches = [];
  projects = [];
  conversations = [];
  bots = [];
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
  const prefRes = await fetch('/api/data/preferences');
  const rawPrefs = prefRes.ok ? await prefRes.json() : {};
  const loaded = normalizeLoadedStore(store, rawPrefs);
  projects = loaded.store.projects;
  conversations = loaded.store.conversations;
  bots = loaded.store.bots || [];
  settings = loaded.preferences;
  hydrateModelPickerState();
  restoreOutboundQueues(conversations);
  if (typeof restoreAppSurface === 'function') restoreAppSurface();
  if (typeof loadTraceSplitPrefs === 'function') loadTraceSplitPrefs();
  hideUnlockSession();
  if (typeof pollState === 'function') await pollState();
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

function buildSystemPrompt(projectIdOverride, opts = {}) {
  const excludeConvoId = opts.excludeConvoId || opts.excludeConvoId || null;
  const convo = opts.convo || null;
  const speakerBot = opts.speakerBot || opts.speakerBot || null;
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

  if (convo && typeof botSystemPromptParts === 'function') {
    botSystemPromptParts(convo, speakerBot).forEach((part) => {
      if (part) parts.push(part);
    });
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
    .replace(/<\/?bot_(?:hold|resume|dm_user|group_post|memory_update)\b[^>]*>/gi, '')
    .replace(/<\/?group_memory_update>/gi, '')
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

function emptyBotActions() {
  return { hold: false, resume: false, dmUser: null, groupPost: null };
}

function mergeBotActions(into, extra) {
  if (!extra) return into;
  if (extra.hold) into.hold = true;
  if (extra.resume) into.resume = true;
  if (extra.dmUser) into.dmUser = extra.dmUser;
  if (extra.groupPost) into.groupPost = extra.groupPost;
  return into;
}

function extractTaggedBlock(source, names) {
  const list = Array.isArray(names) ? names : [names];
  let cleaned = source;
  let captured = null;
  list.forEach((name) => {
    const re = new RegExp(
      '<' + name + '\\b[^>]*>\\s*([\\s\\S]*?)\\s*</' + name + '>',
      'gi'
    );
    cleaned = cleaned.replace(re, (_, body) => {
      const next = String(body || '').trim();
      if (next) captured = next;
      return '';
    });
    const wrap = new RegExp('\\[\\[' + name + '\\]\\]\\s*([\\s\\S]*?)\\s*\\[\\[/' + name + '\\]\\]', 'gi');
    cleaned = cleaned.replace(wrap, (_, body) => {
      const next = String(body || '').trim();
      if (next) captured = next;
      return '';
    });
  });
  return { cleaned, captured };
}

/**
 * Pull memory update blocks and bot coordination tags out of assistant text.
 * While streaming, hide an unclosed opener so the XML never flashes in the UI.
 */
function applyMemoryUpdateProtocol(text, { streaming = false } = {}) {
  let cleaned = text || '';
  let memory = null;
  let globalMemory = null;
  let botMemory = null;
  let groupMemory = null;
  const botActions = emptyBotActions();
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
  cleaned = cleaned.replace(/<bot_memory_update>\s*([\s\S]*?)\s*<\/bot_memory_update>/gi, (_, body) => {
    const next = String(body || '').trim();
    if (next) botMemory = next;
    return '';
  });
  cleaned = cleaned.replace(/<group_memory_update>\s*([\s\S]*?)\s*<\/group_memory_update>/gi, (_, body) => {
    const next = String(body || '').trim();
    if (next) groupMemory = next;
    return '';
  });
  if (/<bot_hold\b[^>]*\/?\s*>|\[\[hold\]\]/i.test(cleaned)) botActions.hold = true;
  if (/<bot_resume\b[^>]*\/?\s*>|\[\[resume\]\]/i.test(cleaned)) botActions.resume = true;
  cleaned = cleaned.replace(/<bot_hold\b[^>]*\/?\s*>\s*(?:<\/bot_hold>)?/gi, '');
  cleaned = cleaned.replace(/<bot_resume\b[^>]*\/?\s*>\s*(?:<\/bot_resume>)?/gi, '');
  cleaned = cleaned.replace(/\[\[hold\]\]/gi, '');
  cleaned = cleaned.replace(/\[\[resume\]\]/gi, '');
  const dm = extractTaggedBlock(cleaned, ['bot_dm_user', 'dm_user']);
  cleaned = dm.cleaned;
  if (dm.captured) botActions.dmUser = dm.captured;
  const post = extractTaggedBlock(cleaned, ['bot_group_post', 'group_post']);
  cleaned = post.cleaned;
  if (post.captured) botActions.groupPost = post.captured;
  if (streaming) {
    const lower = cleaned.toLowerCase();
    const tags = [
      '<global_memory_update>',
      '<memory_update>',
      '<bot_memory_update>',
      '<group_memory_update>',
      '<bot_hold',
      '<bot_resume',
      '<bot_dm_user',
      '<bot_group_post',
      '[[hold]]',
      '[[resume]]',
      '[[dm_user]]',
      '[[group_post]]',
    ];
    let open = -1;
    tags.forEach((tag) => {
      const at = lower.lastIndexOf(tag);
      if (at > open) open = at;
    });
    if (open !== -1) cleaned = cleaned.slice(0, open);
  }
  cleaned = cleaned.replace(/\n{3,}/g, '\n\n').replace(/[ \t]+\n/g, '\n').trimEnd();
  return { cleaned, memory, globalMemory, botMemory, groupMemory, botActions };
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

function applyExtractedMemories(convo, extracted, speakerBotId = null) {
  const result = { globalUpdated: false, projectUpdated: false, botUpdated: false, groupUpdated: false };
  if (!extracted || convo?.incognito) return result;
  if (extracted.memory != null && convo.projectId) {
    result.projectUpdated = persistProjectMemory(convo.projectId, extracted.memory);
  }
  if (extracted.globalMemory != null && !projectIsProjectOnly(convo.projectId)) {
    result.globalUpdated = persistGlobalMemory(extracted.globalMemory);
  }
  if (extracted.botMemory != null && speakerBotId && typeof persistBotMemory === 'function') {
    result.botUpdated = persistBotMemory(speakerBotId, extracted.botMemory);
  }
  if (extracted.groupMemory != null && typeof persistGroupMemory === 'function' && typeof isBotGroup === 'function' && isBotGroup(convo)) {
    result.groupUpdated = persistGroupMemory(convo, extracted.groupMemory);
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
    botMemory: fromAll.botMemory,
    groupMemory: fromAll.groupMemory,
    botActions: mergeBotActions(fromAll.botActions || emptyBotActions(), fromFinal.botActions),
  };
}

function memoryNoticeLabels(changes) {
  const labels = [];
  if (changes?.globalUpdated) labels.push('Updated long-term memory');
  if (changes?.projectUpdated) labels.push('Updated project memory');
  if (changes?.botUpdated) labels.push('Updated bot memory');
  if (changes?.groupUpdated) labels.push('Updated group notes');
  return labels;
}

function memoryOnlyAssistantFallback(extracted) {
  if (!extracted) return '';
  if (extracted.botActions?.dmUser) return 'Opened a private thread.';
  if (extracted.botActions?.groupPost) return 'Posted in the group.';
  if (extracted.botActions?.hold) return 'Holding the room.';
  if (extracted.botActions?.resume) return 'Resuming the room.';
  const projectHit = extracted.memory != null;
  const globalHit = extracted.globalMemory != null;
  const botHit = extracted.botMemory != null;
  const groupHit = extracted.groupMemory != null;
  if (projectHit && globalHit) return 'Updated memory.';
  if (projectHit) return 'Updated project memory.';
  if (globalHit) return 'Updated long-term memory.';
  if (botHit && groupHit) return 'Updated memory.';
  if (botHit) return 'Updated bot memory.';
  if (groupHit) return 'Updated group notes.';
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
