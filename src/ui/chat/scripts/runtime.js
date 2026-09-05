function parseSseEvent(raw) {
  const lines = raw.split('\n').map((line) => line.trim()).filter(Boolean);
  if (lines.length === 0) return null;
  let event = 'message';
  const dataLines = [];
  for (const line of lines) {
    if (line.startsWith('event:')) event = line.slice(6).trim();
    else if (line.startsWith('data:')) dataLines.push(line.slice(5).trim());
  }
  return { event, data: dataLines.join('\n') };
}

/**
 * Reveal streamed text a few code points per frame so chunks feel like a
 * typewriter. Catches up when backlog grows, and snap-flushes on end so
 * we never trail the network after the response finishes.
 */
function createStreamTyper(onPaint) {
  let target = '';
  let shown = '';
  let raf = 0;
  let inReasoning = false;
  const reduceMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
  let catchingUp = false;

  function pending() {
    return target.length - shown.length;
  }

  function advance(step) {
    let i = shown.length;
    let taken = 0;
    while (i < target.length && taken < step) {
      const code = target.charCodeAt(i);
      // Keep surrogate pairs intact.
      if (code >= 0xd800 && code <= 0xdbff && i + 1 < target.length) i += 2;
      else i += 1;
      taken += 1;
    }
    // Caught-up only: finish the word if a break is a few characters away.
    if (step <= 2 && i < target.length) {
      const limit = Math.min(target.length, i + 5);
      for (let j = i; j < limit; j++) {
        const ch = target.charCodeAt(j);
        if (ch === 32 || ch === 9 || ch === 10 || ch === 13) {
          i = j + 1;
          break;
        }
      }
    }
    shown = target.slice(0, i);
  }

  function tick() {
    raf = 0;
    const behind = pending();
    if (behind <= 0) return;
    // ~1–3 code points/frame when near real-time; sprint when buffered.
    let step = 1;
    if (behind > 96) step = Math.ceil(behind / 6);
    else if (behind > 32) step = 4;
    else if (behind > 10) step = 2;
    advance(step);
    onPaint(shown, true);
    if (pending() > 0) raf = requestAnimationFrame(tick);
  }

  function schedule() {
    if (reduceMotion || catchingUp) {
      shown = target;
      onPaint(shown, true);
      return;
    }
    if (!raf) raf = requestAnimationFrame(tick);
  }

  function closeReasoning() {
    if (!inReasoning) return;
    target += '</think>';
    inReasoning = false;
  }

  return {
    get target() { return target; },
    get shown() { return shown; },
    /** OpenAI / llama.cpp reasoning channel (separate from answer content). */
    pushReasoning(delta) {
      if (!delta) return;
      if (!inReasoning) {
        target += '<think>';
        inReasoning = true;
      }
      target += delta;
      schedule();
    },
    push(delta) {
      if (!delta) return;
      closeReasoning();
      target += delta;
      schedule();
    },
    clear() {
      target = '';
      shown = '';
      inReasoning = false;
      if (raf) {
        cancelAnimationFrame(raf);
        raf = 0;
      }
    },
    setCatchingUp(next) {
      catchingUp = !!next;
      if (catchingUp) {
        if (raf) {
          cancelAnimationFrame(raf);
          raf = 0;
        }
        shown = target;
        onPaint(shown, true);
      }
    },
    /** Instantly catch up — call when the SSE stream closes or aborts. */
    flush() {
      closeReasoning();
      if (raf) {
        cancelAnimationFrame(raf);
        raf = 0;
      }
      shown = target;
      onPaint(shown, false);
    },
  };
}

function updateSendEnabled() {
  const hasText = composerInput.value.trim() !== '';
  const hasFiles = pendingAttachments.length > 0;
  const hasQuote = !!(pendingReplyQuote && String(pendingReplyQuote).trim());
  const editingLive = !!(editingRow && !editingRow.classList.contains('msg-queued'));
  const botsDraft = typeof isBotsSurface === 'function' && isBotsSurface() && !activeId;
  const canCompose = !diskEncryptionLocked()
    && serverReady
    && !botsDraft
    && (hasText || hasFiles || hasQuote || editingLive);
  btnSend.disabled = !canCompose;
  if (btnBranch) {
    const botsConvo = typeof isBotsConvo === 'function'
      && isBotsConvo(conversations.find((item) => item.id === activeId));
    btnBranch.disabled = !canCompose || botsConvo || !canBranchFromActiveConversation();
  }
}

function syncModelOriginPill(visible, providerName) {
  if (!chatModelOriginPill) return;
  const name = (providerName || '').trim();
  if (!visible || !name || chatModelSelectWrap.classList.contains('is-hidden')) {
    chatModelOriginPill.classList.add('is-hidden');
    chatModelOriginPill.setAttribute('aria-hidden', 'true');
    chatModelOriginPill.textContent = '';
    chatModelOriginPill.removeAttribute('title');
    return;
  }
  chatModelOriginPill.classList.remove('is-hidden');
  chatModelOriginPill.setAttribute('aria-hidden', 'false');
  chatModelOriginPill.textContent = name;
  applyPrivacyMosaic(chatModelOriginPill, 'model-origin-provider:' + name);
  setIdentityTitle(chatModelOriginPill, name);
}

function modelOptionTitle(label, provider) {
  const model = String(label || '');
  const origin = String(provider || '').trim();
  return origin ? model + ' · ' + origin : model;
}

function hostLooksLocal(host) {
  const value = String(host || '').replace(/^\[|\]$/g, '').toLowerCase();
  if (!value) return false;
  if (
    value === 'localhost'
    || value === '::1'
    || value === '0.0.0.0'
    || value.endsWith('.local')
  ) return true;
  if (
    value === '127.0.0.1'
    || value.startsWith('127.')
    || /^(?:fc|fd)[0-9a-f]{2}:/.test(value)
    || value.startsWith('fe80:')
  ) return true;
  if (value.startsWith('10.') || value.startsWith('192.168.') || value.startsWith('169.254.')) return true;
  const match = value.match(/^172\.(\d+)\./);
  if (!match) return false;
  const octet = Number(match[1]);
  return octet >= 16 && octet <= 31;
}

function modelLooksLocal(option) {
  const base = String(option?.base || '');
  if (!base) return false;
  try {
    const url = new URL(base.includes('://') ? base : 'http://' + base);
    return hostLooksLocal(url.hostname);
  } catch {
    return /localhost|127\.0\.0\.1|\.local/i.test(base);
  }
}

function modelProviderKey(option) {
  return String(option?.providerId || option?.provider || option?.base || 'provider').trim() || 'provider';
}

function modelProviderLabel(option) {
  return String(option?.provider || '').trim() || 'Provider';
}

function modelMenuHasLocal() {
  return modelMenuOptions.some(modelLooksLocal);
}

function modelMenuHasCloud() {
  return modelMenuOptions.some((option) => !modelLooksLocal(option));
}

function fallbackModelMenuTab() {
  if (pinnedModelIds.length) return 'pins';
  if (recentModelIds.length) return 'recents';
  if (modelMenuHasCloud()) return 'cloud';
  if (modelMenuHasLocal()) return 'local';
  return 'recents';
}

function normalizeModelMenuTab(tab) {
  if (tab === 'recents' || tab === 'pins' || tab === 'local' || tab === 'cloud') return tab;
  return fallbackModelMenuTab();
}

function modelMenuUsesProviderGroups() {
  return modelMenuTab === 'local' || modelMenuTab === 'cloud';
}

function groupModelOptions(options) {
  const groups = [];
  const byKey = new Map();
  options.forEach((option) => {
    const key = modelProviderKey(option);
    let group = byKey.get(key);
    if (!group) {
      group = { key, label: modelProviderLabel(option), options: [] };
      byKey.set(key, group);
      groups.push(group);
    }
    group.options.push(option);
  });
  return groups;
}

function visibleModelMenuOptions() {
  if (!modelMenuUsesProviderGroups()) return modelMenuMatches;
  const forceOpen = modelFilterTerms().length > 0;
  return groupModelOptions(modelMenuMatches).flatMap((group) => (
    forceOpen || !isModelProviderCollapsed(group.key) ? group.options : []
  ));
}

function modelMenuIsOpen() {
  return chatModelMenu && !chatModelMenu.classList.contains('is-hidden');
}

let modelMenuCloseTimer = null;

/** When set, the shared menu is picking a model for a loop agent instead of the default. */
let modelMenuContext = null;

function modelMenuSelectedId() {
  if (modelMenuContext && modelMenuContext.selectedId) return modelMenuContext.selectedId;
  return selectedChatModel;
}

function modelMenuAnchorEl() {
  return (modelMenuContext && modelMenuContext.anchor) || chatModelSelectWrap;
}

function modelMenuTriggerEl() {
  return (modelMenuContext && modelMenuContext.trigger) || chatModelSelect;
}

function modelSearchEnabled() {
  if (modelMenuContext) return modelMenuOptions.length > 0;
  return modelMenuOptions.length >= MODEL_SEARCH_MIN_OPTIONS;
}

function modelFilterTerms() {
  return modelMenuFilter.trim().toLowerCase().split(/\s+/).filter(Boolean);
}

function modelMenuSourceOptions() {
  if (modelMenuTab === 'local') return modelMenuOptions.filter(modelLooksLocal);
  if (modelMenuTab === 'cloud') return modelMenuOptions.filter((option) => !modelLooksLocal(option));
  const byValue = new Map(modelMenuOptions.map((option) => [option.value, option]));
  const order = modelMenuTab === 'pins' ? pinnedModelIds : recentModelIds;
  return order
    .map((id) => byValue.get(id))
    .filter(Boolean);
}

function syncModelMenuTabs() {
  const tabs = chatModelMenu?.querySelectorAll('[data-model-tab]');
  if (!tabs) return;
  const hasLocal = modelMenuHasLocal();
  const hasCloud = modelMenuHasCloud();
  tabs.forEach((tab) => {
    const name = tab.getAttribute('data-model-tab');
    if (name === 'local') tab.classList.toggle('is-hidden', !hasLocal);
    if (name === 'cloud') tab.classList.toggle('is-hidden', !hasCloud);
    const active = name === modelMenuTab;
    tab.classList.toggle('is-active', active);
    tab.setAttribute('aria-selected', active ? 'true' : 'false');
  });
  if (chatModelList) {
    const label = modelMenuTab === 'recents'
      ? 'Recent models'
      : (modelMenuTab === 'pins'
        ? 'Pinned models'
        : (modelMenuTab === 'local' ? 'Local models' : 'Cloud models'));
    chatModelList.setAttribute('aria-label', label);
  }
}

function setModelMenuTab(tab, { keepActive = false } = {}) {
  const next = normalizeModelMenuTab(tab);
  if (modelMenuTab === next && modelMenuIsOpen()) {
    applyModelFilter({ keepActive });
    return;
  }
  modelMenuTab = next;
  syncModelMenuTabs();
  if (modelMenuIsOpen()) applyModelFilter({ keepActive });
}

/**
 * Every term must appear somewhere in "<label> <provider>". Ranked so
 * label-prefix hits float above mid-string ones; ties keep source order.
 */
function computeModelMatches() {
  const source = modelMenuSourceOptions();
  const terms = modelFilterTerms();
  if (!terms.length) {
    modelMenuMatches = source;
    return;
  }
  const scored = [];
  source.forEach((option, index) => {
    const label = String(option.label || '').toLowerCase();
    const haystack = (label + ' ' + String(option.provider || '')).toLowerCase();
    if (!terms.every((term) => haystack.includes(term))) return;
    let rank = 2;
    if (label.startsWith(terms[0])) rank = 0;
    else if (label.includes(terms[0])) rank = 1;
    scored.push({ option, rank, index });
  });
  scored.sort((a, b) => (a.rank - b.rank) || (a.index - b.index));
  modelMenuMatches = scored.map((entry) => entry.option);
}

/** Escape, then wrap matched runs in <mark>. Overlapping terms merge. */
function highlightModelText(value, terms) {
  const raw = String(value || '');
  if (!terms.length || !raw) return escapeModelText(raw);
  const lower = raw.toLowerCase();
  const hit = new Array(raw.length).fill(false);
  terms.forEach((term) => {
    if (!term) return;
    let from = 0;
    for (;;) {
      const at = lower.indexOf(term, from);
      if (at === -1) break;
      for (let i = at; i < at + term.length; i += 1) hit[i] = true;
      from = at + 1;
    }
  });
  let out = '';
  let i = 0;
  while (i < raw.length) {
    const on = hit[i];
    let j = i;
    while (j < raw.length && hit[j] === on) j += 1;
    const chunk = escapeModelText(raw.slice(i, j));
    out += on ? '<mark class="chat-model-mark">' + chunk + '</mark>' : chunk;
    i = j;
  }
  return out;
}

function renderModelOptionHtml(option, index, { showProvider, terms }) {
  const isSelected = option.value === modelMenuSelectedId();
  const picking = !!modelMenuContext;
  const pinned = isModelPinned(option.value);
  const badge = showProvider && option.provider
    ? '<span class="chat-model-origin-pill" title="'
      + escapeModelAttr(chatShell.classList.contains('privacy-mode') ? '' : option.provider) + '">'
      + highlightModelText(option.provider, terms) + '</span>'
    : '';
  const defaultBadge = isSelected && !picking
    ? '<span class="chat-model-default-pill" title="Default model">Default</span>'
    : '';
  const prefix = picking
    ? (isSelected ? 'Selected · ' : '')
    : (isSelected ? 'Default · ' : 'Set as default · ');
  return '<div class="chat-model-option' + (isSelected ? ' is-selected' : '')
    + '" role="option" id="chat-model-option-' + index + '"'
    + ' data-value="' + escapeModelAttr(option.value) + '"'
    + ' title="' + escapeModelAttr(
      prefix + modelOptionTitle(option.label, option.provider)
    ) + '"'
    + ' aria-selected="' + (isSelected ? 'true' : 'false') + '" tabindex="-1">'
    + '<button type="button" class="chat-model-option-main" data-model-pick="'
    + escapeModelAttr(option.value) + '">'
    + '<span class="chat-model-option-lead">'
    + MODEL_MARK_ICON
    + '<span class="chat-model-option-name">' + highlightModelText(option.label, terms) + '</span>'
    + defaultBadge
    + '</span>'
    + badge
    + '</button>'
    + '<button type="button" class="chat-model-pin' + (pinned ? ' is-pinned' : '') + '"'
    + ' data-model-pin="' + escapeModelAttr(option.value) + '"'
    + ' aria-label="' + (pinned ? 'Unpin model' : 'Pin model') + '"'
    + ' aria-pressed="' + (pinned ? 'true' : 'false') + '"'
    + ' title="' + (pinned ? 'Unpin' : 'Pin') + '">'
    + MODEL_PIN_ICON
    + '</button>'
    + '</div>';
}

function renderModelMenuList() {
  const terms = modelFilterTerms();
  const sourceCount = modelMenuSourceOptions().length;
  const grouped = modelMenuUsesProviderGroups();
  const forceOpen = !!terms.length;
  let html = '';
  let optionIndex = 0;
  if (grouped) {
    groupModelOptions(modelMenuMatches).forEach((group) => {
      const collapsed = !forceOpen && isModelProviderCollapsed(group.key);
      html += '<section class="chat-model-group' + (collapsed ? ' is-collapsed' : '') + '"'
        + ' data-provider-key="' + escapeModelAttr(group.key) + '">'
        + '<button type="button" class="chat-model-group-toggle" data-model-group="'
        + escapeModelAttr(group.key) + '"'
        + ' aria-expanded="' + (collapsed ? 'false' : 'true') + '">'
        + THINK_CHEVRON
        + '<span class="chat-model-group-name">' + highlightModelText(group.label, terms) + '</span>'
        + '<span class="chat-model-group-count">' + group.options.length + '</span>'
        + '</button>'
        + '<div class="chat-model-group-list" role="group" aria-label="'
        + escapeModelAttr(group.label) + '">';
      if (!collapsed) {
        group.options.forEach((option) => {
          html += renderModelOptionHtml(option, optionIndex, { showProvider: false, terms });
          optionIndex += 1;
        });
      }
      html += '</div></section>';
    });
  } else {
    html = modelMenuMatches.map((option, index) => (
      renderModelOptionHtml(option, index, { showProvider: true, terms })
    )).join('');
  }
  chatModelList.innerHTML = html;
  chatModelList.querySelectorAll('.chat-model-origin-pill').forEach((badge) => {
    applyPrivacyMosaic(badge, 'model-menu-provider:' + badge.textContent);
    setIdentityTitle(badge, badge.textContent);
  });
  chatModelList.querySelectorAll('.chat-model-group-name').forEach((label) => {
    applyPrivacyMosaic(label, 'model-menu-provider-group:' + label.textContent);
  });
  chatModelList.querySelectorAll('.chat-model-option').forEach((optionEl) => {
    const option = modelMenuOptions.find((item) => item.value === optionEl.dataset.value);
    const picking = !!modelMenuContext;
    const prefix = picking
      ? (optionEl.classList.contains('is-selected') ? 'Selected · ' : '')
      : (optionEl.classList.contains('is-selected') ? 'Default · ' : 'Set as default · ');
    setIdentityTitle(optionEl, option ? prefix + modelOptionTitle(option.label, option.provider) : '');
  });

  const empty = !modelMenuMatches.length;
  chatModelEmpty.classList.toggle('is-hidden', !empty);
  chatModelList.classList.toggle('is-hidden', empty);
  if (empty) {
    if (modelMenuTab === 'recents' && !terms.length) {
      chatModelEmpty.textContent = 'Pick a model from Local or Cloud to set your default.';
    } else if (modelMenuTab === 'pins' && !terms.length) {
      chatModelEmpty.textContent = 'Pin models from Recents, Local, or Cloud to keep them here.';
    } else if (modelMenuTab === 'local' && !terms.length) {
      chatModelEmpty.textContent = 'No local models.';
    } else if (modelMenuTab === 'cloud' && !terms.length) {
      chatModelEmpty.textContent = 'No cloud models.';
    } else if (terms.length) {
      chatModelEmpty.textContent = 'No models match “' + modelMenuFilter.trim() + '”';
    } else {
      chatModelEmpty.textContent = 'No models available.';
    }
  }
  if (chatModelSearchCount) {
    chatModelSearchCount.textContent = terms.length
      ? modelMenuMatches.length + '/' + sourceCount
      : String(sourceCount);
  }
}

/** Re-filter and repaint the open menu after the query changed. */
function applyModelFilter({ keepActive = false } = {}) {
  const visible = () => visibleModelMenuOptions();
  const previous = keepActive && modelMenuActiveIndex >= 0
    ? visible()[modelMenuActiveIndex]
    : null;
  computeModelMatches();
  renderModelMenuList();
  const options = visible();
  const restored = previous
    ? options.findIndex((option) => option.value === previous.value)
    : -1;
  if (restored >= 0) modelMenuActiveIndex = restored;
  else {
    modelMenuActiveIndex = options.findIndex((option) => option.value === modelMenuSelectedId());
  }
  paintModelMenuActive();
  if (modelMenuIsOpen()) positionModelMenu();
}

function modelMenuViewport() {
  const view = window.visualViewport;
  if (view) {
    return {
      top: view.offsetTop,
      left: view.offsetLeft,
      width: view.width,
      height: view.height,
    };
  }
  return { top: 0, left: 0, width: window.innerWidth, height: window.innerHeight };
}

function modelMenuAnchorIsUsable(anchor) {
  if (!anchor || !anchor.isConnected || typeof anchor.getBoundingClientRect !== 'function') {
    return false;
  }
  return typeof anchor.getClientRects !== 'function' || anchor.getClientRects().length > 0;
}

function positionModelMenu() {
  const anchor = modelMenuAnchorEl();
  if (!chatModelMenu) return false;
  if (!modelMenuAnchorIsUsable(anchor)) {
    closeModelMenu();
    return false;
  }
  const rect = anchor.getBoundingClientRect();
  const view = modelMenuViewport();
  const pad = 8;
  const gap = 6;
  const viewRight = view.left + view.width;
  const viewBottom = view.top + view.height;
  const rem = parseFloat(getComputedStyle(document.documentElement).fontSize) || 16;
  // Match CSS max-height: min(23rem, 60vh), then shrink to the open side.
  const preferred = Math.min(23 * rem, view.height * 0.6);

  const menuWidth = Math.min(
    chatModelMenu.offsetWidth || 352,
    Math.max(0, view.width - pad * 2)
  );
  const maxLeft = viewRight - menuWidth - pad;
  const idealLeft = rect.left + (rect.width - menuWidth) / 2;
  const left = Math.min(Math.max(view.left + pad, idealLeft), Math.max(view.left + pad, maxLeft));

  const spaceBelow = viewBottom - rect.bottom - pad - gap;
  const spaceAbove = rect.top - view.top - pad - gap;
  const openAbove = spaceBelow < preferred && spaceAbove > spaceBelow;
  const available = Math.max(0, openAbove ? spaceAbove : spaceBelow);
  const maxHeight = Math.max(8, Math.min(preferred, available));
  chatModelMenu.style.maxHeight = Math.round(maxHeight) + 'px';

  const menuHeight = Math.min(chatModelMenu.offsetHeight || maxHeight, maxHeight);
  let top;
  if (openAbove) {
    top = Math.max(view.top + pad, rect.top - gap - menuHeight);
  } else {
    top = rect.bottom + gap;
    if (top + menuHeight > viewBottom - pad) {
      top = Math.max(view.top + pad, viewBottom - pad - menuHeight);
    }
  }

  chatModelMenu.style.top = Math.round(top) + 'px';
  chatModelMenu.style.left = Math.round(left) + 'px';
  chatModelMenu.style.width = Math.round(menuWidth) + 'px';
  chatModelMenu.style.right = 'auto';
  chatModelMenu.style.transformOrigin = openAbove ? 'bottom center' : 'top center';
  return true;
}

function closeModelMenu({ restoreFocus = false } = {}) {
  const trigger = modelMenuTriggerEl();
  const customAnchor = modelMenuContext && modelMenuContext.anchor;
  modelMenuContext = null;
  if (customAnchor) customAnchor.classList.remove('is-open');
  chatModelSelectWrap?.classList.remove('is-open');
  chatModelSelect?.setAttribute('aria-expanded', 'false');
  if (trigger && trigger !== chatModelSelect) trigger.setAttribute('aria-expanded', 'false');
  modelMenuActiveIndex = -1;
  modelMenuFilter = '';
  if (chatModelSearch) chatModelSearch.value = '';
  if (restoreFocus) trigger?.focus();
  if (!chatModelMenu || chatModelMenu.classList.contains('is-hidden')) return;
  clearTimeout(modelMenuCloseTimer);
  modelMenuCloseTimer = null;
  chatModelMenu.classList.remove('is-open');
  const finish = () => {
    chatModelMenu.classList.add('is-hidden');
    chatModelMenu.style.maxHeight = '';
    if (chatModelMenu.parentElement !== chatModelSelectWrap) {
      chatModelSelectWrap.appendChild(chatModelMenu);
    }
  };
  if (prefersReducedMotion()) {
    finish();
    return;
  }
  modelMenuCloseTimer = window.setTimeout(() => {
    modelMenuCloseTimer = null;
    finish();
  }, 180);
}

function paintModelMenuActive() {
  const activeValue = visibleModelMenuOptions()[modelMenuActiveIndex]?.value || '';
  const nodes = chatModelList.querySelectorAll('.chat-model-option');
  let active = null;
  nodes.forEach((node) => {
    const isActive = !!activeValue && node.getAttribute('data-value') === activeValue;
    node.classList.toggle('is-active', isActive);
    if (isActive) active = node;
  });
  if (active) active.scrollIntoView({ block: 'nearest' });
  if (chatModelSearch) {
    if (active) chatModelSearch.setAttribute('aria-activedescendant', active.id);
    else chatModelSearch.removeAttribute('aria-activedescendant');
  }
}

function openModelMenu(opts) {
  if (!modelMenuOptions.length) return;
  const context = opts && typeof opts === 'object' ? opts : null;
  if (!context && chatModelSelectWrap.classList.contains('is-hidden')) return;
  clearTimeout(modelMenuCloseTimer);
  modelMenuCloseTimer = null;
  if (modelMenuContext && modelMenuContext.anchor && modelMenuContext.anchor !== context?.anchor) {
    modelMenuContext.anchor.classList.remove('is-open');
    modelMenuContext.trigger?.setAttribute('aria-expanded', 'false');
  }
  modelMenuContext = context;
  if (chatModelMenu.parentElement !== document.body) {
    document.body.appendChild(chatModelMenu);
  }
  const searchable = modelSearchEnabled();
  chatModelSearchWrap.classList.toggle('is-hidden', !searchable);
  modelMenuFilter = '';
  if (chatModelSearch) chatModelSearch.value = '';
  if (modelMenuTab === 'recents' && !recentModelIds.length) {
    modelMenuTab = pinnedModelIds.length ? 'pins' : fallbackModelMenuTab();
  } else if (modelMenuTab === 'pins' && !pinnedModelIds.length) {
    modelMenuTab = recentModelIds.length ? 'recents' : fallbackModelMenuTab();
  } else if (modelMenuTab === 'local' && !modelMenuHasLocal()) {
    modelMenuTab = fallbackModelMenuTab();
  } else if (modelMenuTab === 'cloud' && !modelMenuHasCloud()) {
    modelMenuTab = fallbackModelMenuTab();
  }
  syncModelMenuTabs();
  chatModelMenu.classList.remove('is-hidden');
  chatModelSelectWrap.classList.toggle('is-open', !context);
  chatModelSelect.setAttribute('aria-expanded', context ? 'false' : 'true');
  const trigger = modelMenuTriggerEl();
  if (trigger) trigger.setAttribute('aria-expanded', 'true');
  if (context?.anchor) context.anchor.classList.add('is-open');
  computeModelMatches();
  renderModelMenuList();
  modelMenuActiveIndex = visibleModelMenuOptions().findIndex((o) => o.value === modelMenuSelectedId());
  positionModelMenu();
  paintModelMenuActive();
  // Typing filters immediately when the field is there; otherwise the
  // trigger keeps focus and its own arrow-key handler stays in charge.
  if (searchable) chatModelSearch.focus();
  void chatModelMenu.offsetWidth;
  requestAnimationFrame(() => {
    if (!positionModelMenu()) return;
    chatModelMenu.classList.add('is-open');
  });
}

function chooseModelOption(value) {
  if (!value) {
    closeModelMenu({ restoreFocus: true });
    return;
  }
  rememberRecentModel(value);
  if (modelMenuContext && typeof modelMenuContext.onPick === 'function') {
    modelMenuContext.selectedId = value;
    modelMenuContext.onPick(value);
    closeModelMenu({ restoreFocus: true });
    return;
  }
  rememberRecentModel(value);
  if (value !== selectedChatModel) {
    selectedChatModel = value;
    selectedRemoteModelId = selectedChatModel;
    persistModelPickerState();
    const selected = modelMenuOptions.find((o) => o.value === selectedChatModel);
    chatModelSelect.textContent = selected ? selected.label : 'Model';
    setIdentityTitle(chatModelSelect, selected ? modelOptionTitle(selected.label, selected.provider) : '');
    syncModelOriginPill(true, selected && selected.provider);
  } else {
    persistModelPickerState();
  }
  closeModelMenu({ restoreFocus: true });
  if (latestState) updateInferenceState(latestState);
  if (typeof updateSendEnabled === 'function') updateSendEnabled();
  if (typeof fillDefaultModelSetting === 'function'
    && settingsModal
    && !settingsModal.classList.contains('is-hidden')) {
    fillDefaultModelSetting();
    if (typeof syncSettingsSaveButton === 'function') syncSettingsSaveButton();
  }
}

function refreshStarfieldClearZone() {
  if (typeof repaintStarfield === 'function') repaintStarfield();
}

function escapeModelAttr(value) {
  return String(value || '')
    .replace(/&/g, '&amp;')
    .replace(/"/g, '&quot;')
    .replace(/</g, '&lt;');
}

function escapeModelText(value) {
  return String(value || '')
    .replace(/&/g, '&amp;')
    .replace(/</g, '&lt;');
}

/** Map a saved picker id onto the current catalog without dropping unknown ids. */
function resolveSavedModelId(savedId, options) {
  const id = String(savedId || '').trim();
  if (!id || !options.length) return id;
  if (options.some((option) => option.value === id)) return id;
  const stripped = id.replace(/^remote\|/, '');
  const suffixHits = options.filter((option) => option.value.endsWith('|' + stripped));
  if (suffixHits.length === 1) return suffixHits[0].value;
  const parts = stripped.split('|');
  if (parts.length >= 2) {
    const model = parts[parts.length - 1];
    const base = parts.slice(0, -1).join('|');
    const baseHits = options.filter((option) => {
      if (option.label !== model) return false;
      return option.base === base
        || option.value.includes('|' + base + '|')
        || option.value.endsWith('|' + base + '|' + model);
    });
    if (baseHits.length === 1) return baseHits[0].value;
  }
  const labelHits = options.filter((option) => option.label && id.endsWith('|' + option.label));
  if (labelHits.length === 1) return labelHits[0].value;
  return id;
}

function remapSavedModelIds(ids, options) {
  const seen = new Set();
  const next = [];
  (Array.isArray(ids) ? ids : []).forEach((id) => {
    const resolved = resolveSavedModelId(id, options);
    if (!resolved || seen.has(resolved)) return;
    seen.add(resolved);
    next.push(resolved);
  });
  return next;
}

function sameIdList(a, b) {
  return a.length === b.length && a.every((id, index) => id === b[index]);
}

function modelIdLabel(id) {
  const parts = String(id || '').split('|').filter(Boolean);
  return parts[parts.length - 1] || '';
}

function catalogOptionFromRemote(model) {
  return {
    value: model.id,
    label: model.model || model.label,
    provider: String(model.provider_name || '').trim(),
    providerId: String(model.provider_id || '').trim(),
    base: String(model.base || '').trim(),
    ready: !!model.ready,
    thinking: !!model.thinking_supported,
  };
}

/**
 * `remote_count` counts configured providers, not providers that returned models, so
 * a provider with an empty or failing catalog must not keep this permanently false.
 */
function modelCatalogIsComplete(network, options) {
  return options.length > 0 && !network?.remote_checking;
}

function resolveCollapsedProviderKey(savedKey, options) {
  const key = String(savedKey || '').trim();
  if (!key || !options.length) return key;
  if (options.some((option) => modelProviderKey(option) === key)) return key;
  const nameHits = options.filter((option) => option.provider === key);
  if (nameHits.length) return modelProviderKey(nameHits[0]);
  const baseHits = options.filter((option) => option.base === key);
  if (baseHits.length) return modelProviderKey(baseHits[0]);
  return key;
}

function remapCollapsedProviderKeys(keys, options) {
  const seen = new Set();
  const next = [];
  (Array.isArray(keys) ? keys : []).forEach((key) => {
    const resolved = resolveCollapsedProviderKey(key, options);
    if (!resolved || seen.has(resolved)) return;
    seen.add(resolved);
    next.push(resolved);
  });
  return next;
}

function syncModelSelector(data) {
  const network = data.network || {};
  const remoteModels = network.remote_models || [];
  const wasHidden = chatModelSelectWrap.classList.contains('is-hidden');
  const menuOpen = modelMenuIsOpen();

  if (!remoteModels.length) {
    if (!menuOpen) closeModelMenu();
    chatModelSelectWrap.classList.add('is-hidden');
    syncModelOriginPill(false);
    modelMenuOptions = [];
    if (!wasHidden) refreshStarfieldClearZone();
    return;
  }
  chatModelSelectWrap.classList.remove('is-hidden');
  if (wasHidden) refreshStarfieldClearZone();

  const remoteOptions = remoteModels.map(catalogOptionFromRemote);
  modelMenuOptions = remoteOptions;
  const remappedPins = remapSavedModelIds(pinnedModelIds, remoteOptions);
  const remappedRecents = remapSavedModelIds(recentModelIds, remoteOptions);
  const remappedCollapsed = remapCollapsedProviderKeys(collapsedModelProviders, remoteOptions);
  let pickerChanged = !sameIdList(remappedPins, pinnedModelIds)
    || !sameIdList(remappedRecents, recentModelIds)
    || !sameIdList(remappedCollapsed, collapsedModelProviders);
  pinnedModelIds = remappedPins;
  recentModelIds = remappedRecents;
  collapsedModelProviders = remappedCollapsed;
  if (!recentModelIds.length && selectedChatModel && remoteOptions.some((o) => o.value === selectedChatModel)) {
    recentModelIds = [selectedChatModel];
    pickerChanged = true;
  }
  const resolvedSelected = resolveSavedModelId(selectedRemoteModelId || selectedChatModel, remoteOptions);
  if (resolvedSelected && resolvedSelected !== selectedChatModel) {
    selectedChatModel = resolvedSelected;
    pickerChanged = true;
  }
  const allValues = remoteOptions.map((o) => o.value);
  const catalogComplete = modelCatalogIsComplete(network, remoteOptions);
  if ((!selectedChatModel || !allValues.includes(selectedChatModel)) && catalogComplete && !menuOpen) {
    selectedChatModel = remoteOptions[0].value;
    pickerChanged = true;
  }
  if (pickerChanged) persistModelPickerState();

  const signature = [
    selectedChatModel,
    ...remoteOptions.map((o) => o.value + '|' + o.label + '|' + o.provider),
  ].join(';');
  // Avoid nuking option nodes under the cursor while the menu is open
  // (state poll runs every 2s and was cancelling clicks).
  if (!menuOpen && chatModelMenu.dataset.signature !== signature) {
    chatModelMenu.dataset.signature = signature;
    // Menu is closed, so there is no live filter to preserve.
    modelMenuFilter = '';
    computeModelMatches();
    renderModelMenuList();
  }
  const selected = remoteOptions.find((o) => o.value === selectedChatModel);
  if (!menuOpen) {
    chatModelSelect.textContent = selected
      ? selected.label
      : (modelIdLabel(selectedChatModel) || 'Model');
    setIdentityTitle(
      chatModelSelect,
      selected
        ? modelOptionTitle(selected.label, selected.provider)
        : (selectedChatModel ? modelIdLabel(selectedChatModel) : '')
    );
  }
  selectedRemoteModelId = selectedChatModel;
  syncModelOriginPill(true, selected && selected.provider);
  if (menuOpen) positionModelMenu();
  if (typeof fillDefaultModelSetting === 'function'
    && settingsModal
    && !settingsModal.classList.contains('is-hidden')) {
    fillDefaultModelSetting();
  }
  const loopModal = document.getElementById('groupModal');
  if (loopModal && !loopModal.classList.contains('is-hidden') && typeof paintLoopModelPickerButtons === 'function') {
    paintLoopModelPickerButtons();
  }
  if (typeof renderTraceMembers === 'function') renderTraceMembers();
  if (typeof paintLoopModelHint === 'function') paintLoopModelHint();
}

function selectedRemoteModel(data, modelId) {
  const models = data?.network?.remote_models || [];
  if (!models.length) return null;
  const options = modelMenuOptions.length ? modelMenuOptions : models.map(catalogOptionFromRemote);
  const saved = modelId || selectedRemoteModelId || selectedChatModel;
  const resolved = resolveSavedModelId(saved, options);
  const matched = models.find((model) => model.id === resolved)
    || models.find((model) => model.id === saved);
  if (matched) return matched;
  if (saved && !modelCatalogIsComplete(data?.network, options)) return null;
  return saved ? null : (models[0] || null);
}

/** Paint the ready-state model line from source data, never from stale surface copy. */
function paintReadyInferenceModelHint(data) {
  if (!serverReady || !data || !modelHintEl) return false;
  if (typeof paintLoopModelHint === 'function' && paintLoopModelHint()) return true;

  const network = data.network || {};
  const remoteSelected = selectedRemoteModel(data);
  const modelName = remoteSelected?.model || network.remote_model || 'remote model';
  const providerLabel = remoteSelected?.provider_name
    || network.remote_name
    || network.remote_label
    || 'provider';
  const project = inProjectChat() ? getProject(activeProjectId) : null;

  modelHintEl.classList.remove('is-hidden');
  if (isIncognitoContext() && !activeId) {
    modelHintEl.textContent = 'Temporary session — stays in memory only until you close the tab.'
      + (project ? ' · Project: ' + project.name : '');
  } else if (project && !activeId) {
    setModelHintWithProvider('Shared instructions & memory apply · ' + modelName, providerLabel);
  } else {
    setModelHintWithProvider(
      project
        ? 'Chatting with ' + modelName + ' · Project: ' + project.name
        : 'Chatting with ' + modelName,
      providerLabel
    );
  }
  return true;
}

function updateInferenceState(data) {
  latestState = data;
  applyAppearance(data);
  syncModelSelector(data);
  const network = data.network || {};
  if (network.inference_mode === 'locked' || diskEncryptionLocked()) {
    serverReady = false;
    modelHintEl.textContent = '';
    modelHintEl.classList.add('is-hidden');
    hideComposerHint();
    syncComposerThinkVisibility(null);
    syncAttachButton();
    updateSendEnabled();
    return;
  }
  const remoteOk = !!network.remote_ok || !!(network.remote_models || []).length;
  const remoteChecking = !!network.remote_checking
    || (!!network.remote_saved && !remoteOk && !network.remote_kind && !(network.remote_models || []).length);
  const remoteSelected = selectedRemoteModel(data);
  const connected = !!(remoteSelected?.ready || remoteOk);
  // Switching models can briefly look "checking" even though we already have a
  // catalog to talk to. Don't disable Send/resend across that blip.
  if (!(remoteChecking && !connected && serverReady && selectedChatModel)) {
    serverReady = connected;
  }

  if (remoteChecking) {
    modelHintEl.textContent = '';
    modelHintEl.classList.add('is-hidden');
    hideComposerHint();
  } else if (!serverReady) {
    modelHintEl.textContent = '';
    modelHintEl.classList.add('is-hidden');
    hideComposerHint();
  } else {
    paintReadyInferenceModelHint(data);
    updateComposerHint();
  }
  syncComposerThinkVisibility(remoteSelected);
  syncAttachButton();
  updateSendEnabled();
  if (typeof syncProviderSettingsFromState === 'function') {
    syncProviderSettingsFromState(data);
  }
}

let encryptionStateSync = null;

function syncExternalEncryptionState(data) {
  if (!dataInfo || typeof data?.encryption_enabled !== 'boolean') return;
  const serverEnabled = data.encryption_enabled;
  const serverUnlocked = data.encryption_unlocked === true;
  if (serverEnabled && !serverUnlocked) {
    if (!dataInfo.encryption_enabled || dataInfo.encryption_unlocked) {
      dataInfo.encryption_enabled = true;
      dataInfo.encryption_unlocked = false;
      clearMemoryAfterLock();
    }
    return;
  }
  if (
    dataInfo.encryption_enabled === serverEnabled
    && (!serverEnabled || dataInfo.encryption_unlocked)
  ) {
    return;
  }
  if (encryptionStateSync) return;
  encryptionStateSync = (async () => {
    const response = await fetch('/api/data');
    if (!response.ok) throw new Error('Could not refresh encryption state');
    dataInfo = await response.json();
    await loadDiskDataAfterUnlock();
    refreshLocalDataPane();
    refreshSettingsDataSummary();
  })()
    .catch((error) => {
      console.warn('Could not synchronize encryption state', error);
      promptUnlockSession();
    })
    .finally(() => {
      encryptionStateSync = null;
    });
}

let statePollInFlight = false;

async function pollState() {
  if (statePollInFlight) return;
  statePollInFlight = true;
  try {
    const response = await fetch('/api/state');
    if (!response.ok) return;
    const data = await response.json();
    syncExternalEncryptionState(data);
    updateInferenceState(data);
    await resumeLiveTurns(data.live_turns);
  } catch {
    // control server briefly unreachable — retry on the next tick
  } finally {
    statePollInFlight = false;
  }
}

async function sendMessage({ branch = false } = {}) {
  if (
    !branch
    && editingRow
    && !editingRow.classList.contains('msg-queued')
    && typeof submitEditedMessage === 'function'
  ) {
    const input = editingRow.querySelector('.msg-edit-input');
    const next = (input?.value || '').trim();
    if (next) {
      await submitEditedMessage(editingRow, next);
      return;
    }
  }
  const typed = composerInput.value.trim();
  const queuedFiles = pendingAttachments.slice();
  const replyQuote = typeof pendingReplyQuote === 'string' ? pendingReplyQuote.trim() : '';
  const replyTarget = pendingReplyTarget && pendingReplyTarget.speakerId
    ? {
      speakerId: pendingReplyTarget.speakerId,
      speakerHandle: pendingReplyTarget.speakerHandle || '',
    }
    : null;
  if ((!typed && !queuedFiles.length && !replyQuote) || !serverReady) return;
  if (!requireUnlockedData()) return;

  let source = conversations.find((item) => item.id === activeId);
  if (branch) {
    if (!source || !Array.isArray(source.messages) || !source.messages.length) return;
  }

  stopVoiceInput({ silent: true });
  cancelMessageEdit();
  showChatView();

  let prepared;
  try {
    if (queuedFiles.length) {
      attachHintUntil = 0;
      showComposerHint('Preparing attachments…');
      prepared = await prepareAttachmentsForSend(queuedFiles);
    } else {
      prepared = [];
    }
  } catch (error) {
    showAttachHint(error?.message || 'Attachment prep failed');
    focusComposer();
    return;
  }

  if (branch) {
    source = conversations.find((item) => item.id === source.id) || source;
    if (!source.messages?.length) {
      focusComposer();
      return;
    }
  }

  const mentioned = parseCapabilityMentions(typed);
  const text = mentioned.text;
  const mentionIds = new Set([
    ...composerMentionIds,
    ...mentioned.mentions,
  ]);
  // Drop legacy chip id if somehow present.
  mentionIds.delete('agent');
  const turn = resolveTurnSkills(mentionIds);
  const displayText = displayTextWithMentions(text, mentioned.mentions);
  const storedAttachments = storedAttachmentsFromPrepared(prepared);
  const apiText = buildUserApiContent(text, prepared, replyQuote, replyTarget?.speakerHandle);

  let convo = branch
    ? null
    : conversations.find((item) => item.id === activeId);
  if (branch) {
    const projectId = source.projectId || null;
    convo = {
      id: newId('c'),
      title: branchConversationTitle(source),
      titleEdited: false,
      messages: cloneConversationMessages(source.messages),
      updatedAt: Date.now(),
      projectId,
      sortOrder: nextTopSortOrder(projectId),
      incognito: !!source.incognito,
      pinned: false,
      pinnedAt: null,
      workspaceRoot: typeof source.workspaceRoot === 'string' ? source.workspaceRoot : '',
    };
    conversations.push(convo);
    if (activeId) stickByConvo.set(activeId, stickToBottom);
    activeId = convo.id;
    draftIncognito = !!convo.incognito;
    activeProjectId = projectId;
    stickToBottom = true;
    userScrollOverride = false;
    resumeBottomIntent = false;
    selectedTraceMsgIndex = null;
    resetTraceAutoOpenState();
    syncUrlFromState();
    showThread(convo);
    renderThread(convo);
    renderSidebar();
    syncComposerStreamUi();
  } else if (!convo) {
    if (typeof isBotsSurface === 'function' && isBotsSurface()) {
      showComposerHint('Create a loop first.');
      if (typeof openGroupDialog === 'function') openGroupDialog(null);
      focusComposer();
      return;
    }
    const id = newId('c');
    convo = {
      id,
      title: draftIncognito ? 'Ghost Chat' : 'New chat',
      titleEdited: false,
      messages: [],
      updatedAt: Date.now(),
      projectId: activeProjectId || null,
      sortOrder: nextTopSortOrder(activeProjectId || null),
      incognito: !!draftIncognito,
      pinned: false,
      pinnedAt: null,
      workspaceRoot: draftWorkspaceRoot,
    };
    conversations.push(convo);
    activeId = id;
    syncUrlFromState({ replace: true });
  }

  composerInput.value = '';
  clearPendingAttachments();
  clearPendingReplyQuote();
  autoResize(composerInput);
  renderComposerMentions();
  renderComposerModes();
  closeMentionMenu();

  const outbound = {
    id: newId('q'),
    editText: typed,
    displayText: displayText || (storedAttachments.length ? '(attachment)' : ''),
    apiText,
    attachments: storedAttachments,
    replyQuote: replyQuote || '',
    replyToSpeakerId: replyTarget?.speakerId || '',
    replyToSpeakerHandle: replyTarget?.speakerHandle || '',
    turn: {
      useAgent: turn.useAgent,
      skills: turn.skills,
      deepResearch: turn.deepResearch,
      deepResearchOutput: turn.deepResearchOutput,
      forceTools: turn.forceTools,
    },
  };

  // Branch always starts a fresh turn on the new chat (never queue on the parent).
  const busy = !branch && isConvoBusy(convo.id);
  const hasQueued = !branch && getOutboundQueue(convo.id).length > 0;
  if (!branch && (busy || hasQueued)) {
    enqueueOutbound(convo, outbound);
    if (!busy) {
      if (typeof resumeOutboundQueue === 'function') resumeOutboundQueue(convo.id);
      maybeSendNextQueued(convo.id);
    }
    focusComposer();
    return;
  }

  dispatchOutboundTurn(convo, outbound);
  focusComposer();
}

function focusComposer() {
  if (!composerInput) return;
  // Defer so a clicked Send button does not steal focus back.
  queueMicrotask(() => {
    if (document.activeElement === composerInput) return;
    composerInput.focus({ preventScroll: true });
  });
}

function ensureStreamDom(convo, stream) {
  if (stream?.hardStopped) return null;
  if (activeId !== convo.id) return null;
  if (stream.dom && stream.dom.row.isConnected) return stream.dom;

  const assistantRow = document.createElement('div');
  assistantRow.className = 'msg msg-role-assistant';
  assistantRow.dataset.streamId = convo.id;
  assistantRow.dataset.msgIndex = String(liveTurnSlices(convo).followUpStart);
  const bubble = document.createElement('div');
  bubble.className = 'msg-bubble';

  const statusEl = document.createElement('div');
  statusEl.className = 'thinking';
  const orbCanvas = document.createElement('canvas');
  orbCanvas.className = 'orb-sm';
  const thinkingLabel = document.createElement('span');
  thinkingLabel.className = 'thinking-label';
  thinkingLabel.dataset.base = 'Processing…';
  thinkingLabel.textContent = streamStatusLabel(stream, 'Processing…');
  statusEl.appendChild(orbCanvas);
  statusEl.appendChild(thinkingLabel);

  const traceEl = document.createElement('div');
  traceEl.className = 'agent-trace is-hidden';
  const answerEl = document.createElement('div');
  answerEl.className = 'agent-answer';

  bubble.appendChild(statusEl);
  bubble.appendChild(traceEl);
  bubble.appendChild(answerEl);
  assistantRow.appendChild(bubble);
  const speakerBot = streamSpeakerBot(stream);
  if (speakerBot) {
    syncMessageSpeaker(assistantRow, {
      role: 'assistant',
      speakerId: speakerBot.id,
      speakerHandle: speakerBot.handle,
    });
  } else if (typeof isBotsConvo === 'function' && isBotsConvo(convo)) {
    // DM fallback when speaker is known from the room, not the stream yet.
    syncMessageSpeaker(assistantRow, { role: 'assistant' });
  }
  attachMessageActions(assistantRow);
  const followUp = chatThread.querySelector('.msg-queued');
  if (followUp) chatThread.insertBefore(assistantRow, followUp);
  else chatThread.appendChild(assistantRow);
  queueMicrotask(() => motionEnter(assistantRow, { y: 14 }));
  const thinkingOrb = mountOrb(orbCanvas, 20);
  stream.dom = {
    row: assistantRow,
    statusEl,
    thinkingLabel,
    traceEl,
    answerEl,
    thinkingOrb,
    enteredSteps: 0,
  };
  const liveClarify = stream.timeline.find((part) => part.type === 'clarify');
  if (liveClarify) mountClarifyForm(stream, liveClarify);
  return stream.dom;
}

/**
 * While an open <think> block is streaming alone, patch the existing
 * .think-stream markdown instead of rebuilding the whole answer tree every
 * typer frame (full rebuilds starve the canvas orb's rAF and reset CSS animations).
 */
function paintLiveThinkOnly(rootEl, cleaned) {
  if (settings.thinking === 'hidden') {
    if (rootEl.innerHTML) rootEl.innerHTML = '';
    return true;
  }
  const segments = parseThinkSegments(cleaned);
  const openThink = segments.find((segment) => segment.type === 'think' && segment.open);
  if (!openThink) return false;
  const otherSegments = segments.filter((segment) => {
    if (segment === openThink) return false;
    return segment.type !== 'think' ? !!segment.content.trim() : (!segment.open && !!segment.content.trim());
  });
  const closedSig = otherSegments.map((s) => s.type + ':' + s.content).join('\0');
  let streamEl = rootEl.querySelector(
    ':scope > .agent-step.is-live-think .think-stream, :scope > .agent-step.is-think .think-stream, :scope > .think-live > .think-stream, :scope > .think-block.is-streaming > .think-stream'
  );
  if (!streamEl || rootEl.dataset.closedSig !== closedSig) {
    let prefixHtml = '';
    for (const seg of otherSegments) {
      if (seg.type === 'think') {
        prefixHtml += renderThinkBlock(seg.content, { open: false, streaming: false });
      } else {
        prefixHtml += renderMarkdown(seg.content);
      }
    }
    rootEl.innerHTML = prefixHtml + renderThinkBlock(openThink.content, { open: true, streaming: true });
    rootEl.dataset.closedSig = closedSig;
    const liveStep = rootEl.querySelector(':scope > .agent-step:last-child');
    if (liveStep) liveStep.classList.add('is-live-think');
    streamEl = rootEl.querySelector('.is-live-think .think-stream, .think-stream');
  }
  if (!streamEl) return false;
  if (streamEl.dataset.thinkRaw !== openThink.content) {
    streamEl.dataset.thinkRaw = openThink.content;
    streamEl.innerHTML = renderThinkMarkdown(openThink.content);
  }
  enhanceCodeBlocks(rootEl);
  streamEl.scrollTop = streamEl.scrollHeight;
  return true;
}

function toolDetailFromPayload(payload) {
  const args = payload && payload.arguments && typeof payload.arguments === 'object'
    ? payload.arguments
    : {};
  if (args.command) return String(args.command);
  if (args.session_id) return String(args.session_id);
  if (args.patch) return (String(args.patch).match(/^\*\*\* (?:Add|Update|Delete|Move to)(?: File)?: .+$/gm) || []).map((line) => line.replace(/^\*\*\* [^:]+: /, '').trim()).join(', ');
  if (args.path) return String(args.path);
  if (args.pattern) return String(args.pattern);
  if (args.url) return String(args.url);
  if (args.ref) return String(args.ref);
  if (args.expression) return String(args.expression);
  if (args.key) return String(args.key);
  if (args.selector) return String(args.selector);
  if (args.query) return String(args.query);
  if (args.name) return String(args.name);
  if (args.id) return String(args.id);
  if (payload && payload.summary) return String(payload.summary);
  return '';
}

function upsertLiveToolPart(stream, payload) {
  const id = payload && payload.id ? String(payload.id) : '';
  const args = payload && payload.arguments && typeof payload.arguments === 'object'
    ? payload.arguments
    : {};
  const detail = toolDetailFromPayload(payload);
  const body = toolBodyFromArgs((payload && payload.name) || '', args);
  let part = id
    ? stream.timeline.find((item) => item.type === 'tool' && item.id === id)
    : null;
  if (!part && !id) {
    const liveTools = stream.timeline.filter((item) => item.type === 'tool' && item.live);
    part = liveTools.find((item) => payload && item.name === payload.name && !item.result && !item.id) || null;
  }
  if (!part) {
    part = {
      type: 'tool',
      id,
      name: (payload && payload.name) || 'skill',
      detail,
      kind: args.kind ? String(args.kind) : '',
      args: { ...args },
      body,
      result: '',
      note: '',
      live: true,
      executing: false,
      startedAt: Date.now(),
    };
    stream.timeline.push(part);
  } else {
    if (id) part.id = id;
    if (payload && payload.name) part.name = payload.name;
    if (detail) part.detail = detail;
    if (args.kind) part.kind = String(args.kind);
    if (Object.keys(args).length) part.args = { ...(part.args || {}), ...args };
    if (body) part.body = body;
  }
  if (payload && (payload.needs_approval || payload.phase === 'tool_approval')) {
    part.approval = 'pending';
    part.approvalRisk = payload.risk ? String(payload.risk) : (part.approvalRisk || 'write');
    part.executing = false;
  }
  if (payload && payload.phase === 'tool_executing') {
    part.approval = 'allowed';
    part.executing = true;
  }
  return part;
}

function timelineSignature(timeline) {
  return (timeline || []).map((part) => {
    if (!part) return '';
    if (part.type === 'tool') {
      return [
        'tool',
        part.id || '',
        part.name,
        part.detail,
        part.result,
        part.note || '',
        typeof part.ok === 'boolean' ? String(part.ok) : '',
        part.running ? '1' : '0',
        part.commandSessionId || '',
        part.live ? '1' : '0',
        part.approval || '',
        part.approvalRisk || '',
        part.executing ? '1' : '0',
        part.live ? '' : (part.body || ''),
        part.image ? String(part.image.length) : '',
      ].join('\0');
    }
    if (part.type === 'clarify') {
      return ['clarify', part.id || '', part.live ? '1' : '0', part.summary || ''].join('\0');
    }
    return [part.type, part.content || ''].join('\0');
  }).join('\n');
}

function streamingAnswerText(text) {
  if (!text) return '';
  let out = '';
  for (const segment of parseThinkSegments(text)) {
    if (segment.type === 'think') continue;
    out += segment.content || '';
  }
  return out;
}

function ensureLiveAnswerBox(liveRoot) {
  let box = liveRoot.querySelector(':scope > .agent-final-answer');
  if (!box) {
    liveRoot.innerHTML = '<div class="agent-final-answer"></div>';
    box = liveRoot.querySelector(':scope > .agent-final-answer');
  }
  return box;
}

function paintLiveStreamingAnswer(host, cleaned, { streaming = true, wrapFinal = false } = {}) {
  const liveText = streamingAnswerText(cleaned);
  if (wrapFinal) {
    paintIncrementalMarkdown(ensureLiveAnswerBox(host), liveText, { streaming });
    return;
  }
  paintIncrementalMarkdown(host, liveText, { streaming });
}

function paintStreamIntoView(convo, stream, replyText, streaming) {
  if (stream?.hardStopped) return;
  setMarkdownImages(stream.images);
  stream.partial = replyText;
  const extracted = applyMemoryUpdateProtocol(replyText, { streaming });
  const { cleaned } = extracted;
  if (typeof isSilentNoReply === 'function' && isSilentNoReply(cleaned)) {
    if (activeId === convo.id && stream.dom?.answerEl) {
      if (stream.dom.answerEl.innerHTML) stream.dom.answerEl.innerHTML = '';
    }
    return;
  }
  if (activeId !== convo.id) {
    // Background stream — keep state only; sidebar shows the live marker.
    return;
  }
  const dom = ensureStreamDom(convo, stream);
  if (!dom) return;
  const { row, statusEl, thinkingLabel, traceEl, answerEl, thinkingOrb } = dom;
  const savedClarifyHost = preserveClarifyHost(answerEl);
  if (traceEl) {
    traceEl.innerHTML = '';
    traceEl.classList.add('is-hidden');
  }
  const thinkingOpen = streaming && isThinkingOpen(cleaned);
  const hasTimeline = stream.timeline.length > 0;
  const desktop = isDesktopTraceLayout();
  const notesCollapsed = readProcessNotesCollapsed(
    answerEl,
    stream.processNotesCollapsed !== false
  );
  stream.processNotesCollapsed = notesCollapsed;
  const sealedAnswerHtml = renderSealedAnswerHtml(stream.timeline, { notesCollapsed });
  const liveText = streamingAnswerText(cleaned);
  const hasLiveAnswer = !!liveText.trim();
  const hasThinkFallback = !hasTimeline && !thinkingOpen && !!cleaned && !hasLiveAnswer;
  const hasVisibleAnswer = desktop
    ? !!(sealedAnswerHtml || hasLiveAnswer || hasThinkFallback)
    : !!(hasLiveAnswer || sealedAnswerHtml);

  if (desktop) {
    const hasContent = !!(sealedAnswerHtml || hasLiveAnswer || hasThinkFallback);
    if (!hasContent && !streaming) {
      if (answerEl.innerHTML) answerEl.innerHTML = '';
      delete answerEl.dataset.renderedHtml;
    } else if (hasContent || streaming) {
      let sealedRoot = answerEl.querySelector(':scope > .agent-sealed');
      let liveRoot = answerEl.querySelector(':scope > .agent-live');
      if (!sealedRoot || !liveRoot) {
        answerEl.innerHTML = '<div class="agent-sealed"></div><div class="agent-live"></div>';
        sealedRoot = answerEl.querySelector(':scope > .agent-sealed');
        liveRoot = answerEl.querySelector(':scope > .agent-live');
      }
      const sealedSig = sealedContentSignature(stream.timeline);
      if (sealedRoot.dataset.sealedSig !== sealedSig) {
        sealedRoot.innerHTML = sealedAnswerHtml;
        sealedRoot.dataset.sealedSig = sealedSig;
        enhanceCodeBlocks(sealedRoot);
      }
      if (hasThinkFallback) {
        const html = renderAssistantHtml(cleaned, { streaming });
        if (liveRoot.dataset.renderedHtml !== html) {
          liveRoot.innerHTML = html;
          liveRoot.dataset.renderedHtml = html;
          enhanceCodeBlocks(liveRoot);
        }
      } else {
        delete liveRoot.dataset.renderedHtml;
        paintLiveStreamingAnswer(liveRoot, cleaned, { streaming, wrapFinal: true });
      }
    }
    delete answerEl.dataset.committedSig;
    delete answerEl.dataset.renderedHtml;
    stream.enteredSteps = 0;
  } else if (cleaned || hasTimeline || sealedAnswerHtml) {
    const committedHtml = renderCommittedParts(stream.timeline);
    const committedSig = timelineSignature(stream.timeline) + '\0' + sealedContentSignature(stream.timeline);
    const liveOnly =
      streaming &&
      thinkingOpen &&
      !hasTimeline &&
      !sealedAnswerHtml &&
      paintLiveThinkOnly(answerEl, cleaned);

    if (!liveOnly) {
      let liveRoot = answerEl.querySelector(':scope > .agent-live');
      if ((hasTimeline || sealedAnswerHtml) && streaming) {
        if (answerEl.dataset.committedSig !== committedSig || !liveRoot) {
          answerEl.innerHTML = committedHtml + sealedAnswerHtml + '<div class="agent-live"></div>';
          answerEl.dataset.committedSig = committedSig;
          liveRoot = answerEl.querySelector(':scope > .agent-live');
          enhanceCodeBlocks(answerEl);
        }
        patchLiveToolBodies(answerEl, stream.timeline);
        if (cleaned && liveRoot && !(thinkingOpen && paintLiveThinkOnly(liveRoot, cleaned))) {
          paintStreamingAssistant(liveRoot, cleaned, { streaming: true });
        }
      } else if (streaming && cleaned && !hasTimeline && !sealedAnswerHtml) {
        if (!(thinkingOpen && paintLiveThinkOnly(answerEl, cleaned))) {
          paintStreamingAssistant(answerEl, cleaned, { streaming: true });
        }
        answerEl.dataset.committedSig = committedSig;
      } else {
        answerEl.innerHTML = committedHtml + sealedAnswerHtml + (cleaned ? renderAssistantHtml(cleaned, { streaming }) : '');
        answerEl.dataset.committedSig = committedSig;
        enhanceCodeBlocks(answerEl);
      }
      scrollThinkStreams(answerEl);
      const steps = answerEl.querySelectorAll(':scope > .agent-step');
      const seen = stream.enteredSteps || 0;
      steps.forEach((el, index) => {
        if (index < seen || el.classList.contains('is-live')) return;
        motionEnter(el, { y: 10, duration: 200, delay: Math.min(index - seen, 6) * 28 });
      });
      stream.enteredSteps = steps.length;
    }
  } else if (!streaming) {
    if (answerEl.innerHTML) answerEl.innerHTML = '';
    delete answerEl.dataset.committedSig;
    delete answerEl.dataset.renderedHtml;
    stream.enteredSteps = 0;
  }

  const toolLive = stream.timeline.some((part) => part.type === 'tool' && part.live);
  const clarifyLive = stream.timeline.some((part) => part.type === 'clarify' && part.live);
  restoreClarifyHost(answerEl, savedClarifyHost || stream.dom?.clarifyHost);
  // Agent turns keep the status chip for the whole stream — including the quiet
  // gap after a tool finishes and before the next think/answer tokens arrive.
  const agentStreaming = streaming && !!stream.useAgent;
  if (toolLive || clarifyLive || thinkingOpen || agentStreaming || (streaming && !cleaned && !hasTimeline)) {
    statusEl.classList.remove('is-hidden');
    thinkingOrb.play();
    if (toolLive) {
      // Label is set by the tool_call agent event.
    } else if (clarifyLive) {
      setStreamThinkingLabel(stream, 'Waiting for your answers…');
    } else if (thinkingOpen) {
      setStreamThinkingLabel(stream, 'Reasoning…');
    } else if (agentStreaming) {
      const base = String(thinkingLabel.dataset.base || '').trim();
      // After tools, never leave a blank/stale chip — fall back to Processing.
      if (!base || base === 'Reading results') {
        setStreamThinkingLabel(stream, 'Processing…');
      }
    }
  } else if (hasVisibleAnswer || (!streaming && (cleaned || hasTimeline))) {
    statusEl.classList.add('is-hidden');
    // Pause only — stop() is irreversible and a brief think/answer flicker
    // would leave a dead orb while the label shimmer keeps running.
    thinkingOrb.pause();
  }
  row.dataset.raw = cleaned;
  // Never force — respect user scroll-away while tokens arrive.
  scrollToBottom();
  if (desktop) {
    const idx = Number(row.dataset.msgIndex);
    const hasActivity = hasTimeline || thinkingOpen;
    // Open the pane before painting live cards. WebKit freezes CSS/WAAPI
    // animations that start while the sidebar is still width:0 / opacity:0.
    if (hasActivity) maybeAutoOpenTraceSidebar(convo.id);
    if (selectedTraceMsgIndex !== idx) {
      selectTraceMessage(idx, { animate: false, ensureOpen: false });
    } else {
      refreshTraceSidebar({ animate: false });
    }
  } else {
    syncLiveToolClocks(answerEl);
  }
  if (typeof kickLiveToolMotion === 'function' && (toolLive || clarifyLive)) {
    kickLiveToolMotion(answerEl);
    if (desktop && traceSidebarBody) kickLiveToolMotion(traceSidebarBody);
  }
}

/** Stream an assistant reply; safe to leave the conversation while it runs. */
async function runAssistantTurn(convo, {
  useAgent,
  text,
  skills,
  deepResearch = false,
  deepResearchOutput = 'long',
  forceTools = [],
  dispatchedMessage = null,
  queueItem = null,
  previousTitle = '',
  speakerBotId = null,
  skipQueue = false,
  replaceLive = false,
  botMessageLimit = null,
  loopTurnDirective = '',
  loopPhase = null,
}) {
  if (outboundStarting.has(convo.id) && !replaceLive) return false;
  if (activeStreams.has(convo.id)) {
    if (!replaceLive) return false;
    const live = activeStreams.get(convo.id);
    if (live) {
      live.replaced = true;
      live.skipQueue = true;
    }
    abortStream(convo.id, { cancelServer: true });
  }
  const startEpoch = markOutboundStarting(convo.id);
  let liveStarted = false;
  try {
  if (typeof clearBotsOutboundStopped === 'function') clearBotsOutboundStopped(convo.id);
  if (typeof clearLiveTurnUserCancel === 'function') clearLiveTurnUserCancel(convo.id);
  if (typeof waitForCancel === 'function') await waitForCancel(convo.id);
  if (typeof outboundStartIsCurrent === 'function'
    && !outboundStartIsCurrent(convo.id, startEpoch)) {
    return false;
  }
  if (!serverReady) {
    if (typeof showComposerHint === 'function') {
      showComposerHint('Model is not ready yet. Try Send again.');
    }
    return false;
  }
  resetTraceAutoOpenState();

  const turnSkills = skills || {
    web_search: !!settings.skillWebSearch,
    web_search_depth: WEB_SEARCH_DEPTHS.includes(settings.webSearchDepth)
      ? settings.webSearchDepth
      : DEFAULT_SETTINGS.webSearchDepth,
    web_search_provider: WEB_SEARCH_PROVIDERS.includes(settings.webSearchProvider)
      ? settings.webSearchProvider
      : DEFAULT_SETTINGS.webSearchProvider,
    web_search_searxng: typeof settings.webSearchSearxng === 'string'
      ? settings.webSearchSearxng.trim()
      : '',
    web_search_parallel_api_key: typeof settings.webSearchParallelApiKey === 'string'
      ? settings.webSearchParallelApiKey.trim()
      : '',
    web_search_parallel_mode: WEB_SEARCH_PARALLEL_MODES.includes(settings.webSearchParallelMode)
      ? settings.webSearchParallelMode
      : DEFAULT_SETTINGS.webSearchParallelMode,
    web_search_tinyfish_api_key: typeof settings.webSearchTinyfishApiKey === 'string'
      ? settings.webSearchTinyfishApiKey.trim()
      : '',
    web_search_max_results: Math.min(20, Math.max(1, Number(settings.webSearchResults) || 6)),
    web_search_region: /^[a-z]{2}-[a-z]{2}$/.test(settings.webSearchRegion)
      ? settings.webSearchRegion
      : 'us-en',
    web_search_safesearch: WEB_SEARCH_SAFESEARCH.includes(settings.webSearchSafeSearch)
      ? settings.webSearchSafeSearch
      : 'moderate',
    web_search_recency: WEB_SEARCH_RECENCIES.includes(settings.webSearchRecency)
      ? settings.webSearchRecency
      : 'any',
    fetch_url: !!settings.skillFetchUrl,
    fetch_url_max_chars: Math.min(
      200000,
      Math.max(1000, Number(settings.fetchUrlMaxChars) || 8000)
    ),
    web_search_page_max_chars: (() => {
      const raw = Number(settings.webSearchPageMaxChars);
      if (!Number.isFinite(raw) || raw <= 0) return 0;
      return Math.min(200000, Math.max(1000, Math.round(raw)));
    })(),
    approval_mode: APPROVAL_MODES.includes(settings.approvalMode)
      ? settings.approvalMode
      : 'manual',
    filesystem: !!settings.skillFilesystem,
    workspace_root: sessionWorkspaceRoot(),
    terminal: !!settings.skillTerminal,
    terminal_timeout_secs: Math.min(30, Math.max(5, Number(settings.terminalTimeoutSecs) || 30)),
    browser: !!settings.skillBrowser,
  };
  if (deepResearch) {
    useAgent = true;
    turnSkills.web_search = true;
    turnSkills.fetch_url = true;
    turnSkills.web_search_depth = 'deep';
    turnSkills.web_search_max_results = Math.max(turnSkills.web_search_max_results || 6, 10);
  }
  const turnForceTools = Array.isArray(forceTools)
    ? forceTools.filter((name) => name === 'web_search' || name === 'fetch_url')
    : [];
  if (deepResearch || !useAgent || !settings.agentMode) {
    turnForceTools.length = 0;
  } else {
    if (turnForceTools.includes('web_search')) turnSkills.web_search = true;
    if (turnForceTools.includes('fetch_url')) turnSkills.fetch_url = true;
  }

  const stream = beginLiveStream(convo, {
    useAgent,
    deepResearch,
    deepResearchOutput,
    turnSkills,
    turnForceTools,
    speakerBotId: speakerBotId || null,
    loopPhase,
  });
  liveStarted = true;
  clearOutboundStarting(convo.id, startEpoch);
  stream.speakerBotId = speakerBotId || null;
  stream.skipQueue = !!skipQueue;
  syncStreamSpeakerChrome(convo, stream);

  const speakerBot = speakerBotId && typeof getBot === 'function' ? getBot(speakerBotId, convo) : null;
  stream.speakerModelId = speakerBot && speakerBot.model ? speakerBot.model : null;
  let apiMessages;
  if (speakerBot && typeof botApiMessages === 'function') {
    apiMessages = botApiMessages(convo, speakerBot, dispatchedMessage ? text : null, {
      messageLimit: botMessageLimit,
    });
  } else {
    apiMessages = convo.messages.map((message) => {
      if (message.role !== 'user') {
        return { role: message.role, content: message.content };
      }
      return {
        role: 'user',
        content: userMessageApiContent(message),
      };
    });
    if (apiMessages.length > 0) {
      const last = apiMessages[apiMessages.length - 1];
      if (last.role === 'user') last.content = text;
    }
  }
  const systemPrompt = buildSystemPrompt(convo.projectId, {
    excludeConvoId: convo.id,
    convo,
    speakerBot,
  });
  const systemParts = [systemPrompt, String(loopTurnDirective || '').trim()].filter(Boolean);
  if (systemParts.length) {
    apiMessages.unshift({ role: 'system', content: systemParts.join('\n\n') });
  }

  const remote = selectedRemoteModel(latestState, stream.speakerModelId);
  const speakerEffort = typeof thinkingEffortForModel === 'function'
    ? thinkingEffortForModel(remote)
    : (thinkingSupported && activeThinkingEffort !== 'auto' ? activeThinkingEffort : null);
  const requestBody = {
    messages: apiMessages,
    agent: useAgent,
    skills: turnSkills,
    force_tools: turnForceTools,
    ...(convo.incognito ? {} : { conversation_id: convo.id }),
    ...(speakerEffort ? { thinking_effort: speakerEffort } : {}),
  };
  if (deepResearch) {
    requestBody.deep_research = true;
    requestBody.deep_research_output = deepResearchOutput === 'brief' ? 'brief' : 'long';
  }
  if (remote) {
    requestBody.remote_base = remote.base;
    requestBody.model = remote.model;
  }

  let response = null;
  try {
    response = await fetch('/api/chat/completions', {
      method: 'POST',
      headers: {
        'content-type': 'application/json',
      },
      body: JSON.stringify(requestBody),
      signal: stream.controller.signal,
    });
    if (!response.ok) {
      if (response.status === 409) {
        dropLiveSubscriber(convo.id, stream);
        if (dispatchedMessage && queueItem) {
          const index = convo.messages.indexOf(dispatchedMessage);
          if (index >= 0) convo.messages.splice(index, 1);
          if (!convo.messages.length && previousTitle) convo.title = previousTitle;
          getOutboundQueue(convo.id).unshift(queueItem);
          convo.updatedAt = Date.now();
          saveConversations({ immediate: true });
          if (activeId === convo.id) {
            renderThread(convo);
            renderOutboundQueue(convo);
          }
          renderSidebar();
          updateComposerHint();
        }
        await attachLiveTurn(convo, {});
        return true;
      }
      const problem = await response.json().catch(() => null);
      throw new Error((problem && problem.error) || ('Request failed with status ' + response.status));
    }
  } catch (error) {
    if (error.name === 'AbortError') {
      if (stream.replaced) {
        dropLiveSubscriber(convo.id, stream);
        return;
      }
      if (!stream.cancelled) {
        dropLiveSubscriber(convo.id, stream);
        if (!stream.skipQueue) maybeSendNextQueued(convo.id);
        return;
      }
    } else {
      stream.errorMessage = error.message || 'The request failed.';
    }
  }
  await driveAssistantSse(convo, stream, response);
  return true;
  } finally {
    if (!liveStarted) clearOutboundStarting(convo.id, startEpoch);
  }
}

function dropLiveSubscriber(convoId, stream) {
  if (activeStreams.get(convoId) !== stream) return;
  activeStreams.delete(convoId);
  renderSidebar();
  syncComposerStreamUi();
}

function finishLiveStream(convoId, stream) {
  if (!stream || activeStreams.get(convoId) !== stream) return false;
  try { stream.dom?.thinkingOrb?.stop(); } catch { /* ignore */ }
  activeStreams.delete(convoId);
  reclaimUnappliedSteers(convoId, stream);
  renderSidebar();
  syncComposerStreamUi();
  return true;
}

function commitLiveAssistant(convo, message, turnId) {
  if (turnId) message.liveTurnId = turnId;
  const existing = turnId
    ? convo.messages.find(
      (item) => item.role === 'assistant' && item.liveTurnId === turnId
    )
    : null;
  if (existing) {
    Object.assign(existing, message);
    return existing;
  }
  const insertAt = liveTurnSlices(convo).followUpStart;
  if (insertAt < convo.messages.length) convo.messages.splice(insertAt, 0, message);
  else convo.messages.push(message);
  return message;
}

function liveTurnSlices(convo) {
  const messages = convo.messages || [];
  let lastAsst = -1;
  for (let i = 0; i < messages.length; i++) {
    if (messages[i]?.role === 'assistant') lastAsst = i;
  }
  const rest = messages.slice(lastAsst + 1);
  return {
    lastAsst,
    head: messages.slice(0, lastAsst + 1),
    prompt: rest[0] || null,
    promptIndex: lastAsst + 1,
    followUps: rest.slice(1),
    followUpStart: lastAsst + 2,
  };
}

async function syncConvoFromStore(convoId) {
  try {
    const response = await fetch('/api/data/store');
    if (!response.ok) return conversations.find((item) => item.id === convoId) || null;
    const parsed = parseStorePayload(await response.json());
    const fresh = (parsed.conversations || []).find((item) => item.id === convoId);
    if (!fresh) return conversations.find((item) => item.id === convoId) || null;
    if (typeof adoptPersistedOutboundQueue === 'function') adoptPersistedOutboundQueue(fresh);
    const idx = conversations.findIndex((item) => item.id === convoId);
    if (idx >= 0) {
      const localUpdated = Number(conversations[idx]?.updatedAt) || 0;
      const freshUpdated = Number(fresh.updatedAt) || 0;
      if (freshUpdated >= localUpdated) conversations[idx] = fresh;
    } else {
      conversations.push(fresh);
    }
    return conversations.find((item) => item.id === convoId) || null;
  } catch {
    return conversations.find((item) => item.id === convoId) || null;
  }
}

async function resumeLiveTurns(list) {
  if (!Array.isArray(list) || diskEncryptionLocked() || !storageReady) return;
  for (const info of list) {
    const id = info && String(info.conversation_id || '').trim();
    if (!id || activeStreams.has(id)) continue;
    if (typeof shouldSkipLiveTurnResume === 'function' && shouldSkipLiveTurnResume(info)) {
      continue;
    }
    let convo = conversations.find((item) => item.id === id);
    if (!convo) convo = await syncConvoFromStore(id);
    if (!convo) continue;
    if (
      info.finished
      && info.turn_id
      && convo.messages.some(
        (message) => message.role === 'assistant' && message.liveTurnId === info.turn_id
      )
    ) {
      continue;
    }
    const viewing = activeId === convo.id;
    if (!viewing) {
      const synced = await syncConvoFromStore(id);
      if (synced) convo = synced;
    }
    if (viewing && emptyState && !emptyState.classList.contains('is-hidden')) {
      showThread(convo);
    }
    void attachLiveTurn(convo, info);
  }
}

async function attachLiveTurn(convo, info) {
  if (!convo || activeStreams.has(convo.id) || !serverReady) return;
  if (typeof shouldSkipLiveTurnResume === 'function' && shouldSkipLiveTurnResume(info)) return;
  beginLiveStream(convo, {
    useAgent: !!info?.agent,
    deepResearch: !!info?.deep_research,
    deepResearchOutput: info?.deep_research_output === 'brief' ? 'brief' : 'long',
    turnSkills: {},
    turnForceTools: [],
    catchingUp: true,
    turnId: info?.turn_id || null,
    turnModel: info?.model || '',
  });
  const stream = activeStreams.get(convo.id);
  if (!stream) return;
  try {
    const response = await fetch('/api/chat/live/' + encodeURIComponent(convo.id), {
      signal: stream.controller.signal,
    });
    if (!response.ok) {
      if (response.status === 404) {
        finishLiveStream(convo.id, stream);
        return;
      }
      const problem = await response.json().catch(() => null);
      throw new Error((problem && problem.error) || ('Request failed with status ' + response.status));
    }
    await driveAssistantSse(convo, stream, response);
  } catch (error) {
    if (error.name === 'AbortError' && stream.cancelled) {
      await driveAssistantSse(convo, stream, null);
      return;
    }
    dropLiveSubscriber(convo.id, stream);
  }
}

function beginLiveStream(convo, {
  useAgent,
  deepResearch,
  deepResearchOutput,
  turnSkills,
  turnForceTools,
  catchingUp = false,
  turnId = null,
  turnModel = '',
  speakerBotId = null,
  loopPhase = null,
} = {}) {
  if (activeStreams.has(convo.id)) return activeStreams.get(convo.id);
  const stream = {
    controller: new AbortController(),
    useAgent,
    deepResearch,
    deepResearchOutput,
    turnSkills,
    turnForceTools,
    partial: '',
    timeline: [],
    images: Object.create(null),
    errorMessage: null,
    processNotesCollapsed: true,
    dom: null,
    steerId: null,
    pendingSteers: [],
    catchingUp: !!catchingUp,
    turnId: turnId || null,
    turnModel: String(turnModel || ''),
    speakerBotId: speakerBotId || null,
    loopPhase: loopPhase && typeof loopPhase === 'object' ? { ...loopPhase } : null,
    convoId: convo.id,
    cancelled: false,
    hardStopped: false,
  };
  if (stream.turnId && typeof rememberHandledLiveTurn === 'function') {
    rememberHandledLiveTurn(stream.turnId);
  }
  activeStreams.set(convo.id, stream);
  renderSidebar();
  syncComposerStreamUi();
  if (activeId === convo.id) {
    ensureStreamDom(convo, stream);
    scrollToBottom({ force: true });
  }
  return stream;
}

function streamSpeakerBot(stream) {
  if (!stream?.speakerBotId || typeof getBot !== 'function') return null;
  const convo = stream.convoId
    ? conversations.find((item) => item.id === stream.convoId)
    : null;
  return getBot(stream.speakerBotId, convo);
}

function streamStatusLabel(stream, base) {
  const label = String(base || 'Processing…').trim() || 'Processing…';
  const bot = streamSpeakerBot(stream);
  return bot ? ('@' + bot.handle + ' · ' + label) : label;
}

function setStreamThinkingLabel(stream, base) {
  if (!stream?.dom?.thinkingLabel) return;
  const baseLabel = String(base || 'Processing…').trim() || 'Processing…';
  stream.dom.thinkingLabel.dataset.base = baseLabel;
  const next = streamStatusLabel(stream, baseLabel);
  if (stream.dom.thinkingLabel.textContent !== next) {
    stream.dom.thinkingLabel.textContent = next;
  }
}

function syncStreamSpeakerChrome(convo, stream) {
  if (!stream?.dom?.row) return;
  const bot = streamSpeakerBot(stream);
  if (bot) {
    syncMessageSpeaker(stream.dom.row, {
      role: 'assistant',
      speakerId: bot.id,
      speakerHandle: bot.handle,
      loopPhaseLabel: stream.loopPhase?.label || '',
      loopPhaseIndex: stream.loopPhase?.index || null,
      loopPhaseTotal: stream.loopPhase?.total || null,
    });
    setStreamThinkingLabel(stream, stream.dom.thinkingLabel?.dataset?.base || 'Processing…');
  } else if (typeof isBotsConvo === 'function' && isBotsConvo(convo)) {
    syncMessageSpeaker(stream.dom.row, { role: 'assistant' });
  }
}

function discardLiveStreamRow(stream) {
  const row = stream?.dom?.row;
  if (!row) return;
  try { stream.dom?.thinkingOrb?.stop(); } catch { /* ignore */ }
  if (row.isConnected) row.remove();
  stream.dom = null;
}

async function driveAssistantSse(convo, stream, response) {
  const remote = selectedRemoteModel(latestState, stream.speakerModelId);
  const fallbackTurnModel = String(remote?.model || latestState?.network?.remote_model || 'model').trim();
  const usageStats = {
    completionTokens: 0,
    promptTokens: 0,
    providerTokPerSec: null,
    upstreamModel: null,
  };
  let firstTokenAt = null;

  const typer = createStreamTyper((replyText, streaming) => {
    if (activeId === convo.id && stream.dom) {
      const nextLabel = isThinkingOpen(replyText)
        ? 'Reasoning…'
        : stream.useAgent
          ? (stream.deepResearch
            ? (stream.deepResearchOutput === 'brief' ? 'Writing brief' : 'Writing report')
            : 'Writing answer')
          : (replyText && replyText.trim())
            ? 'Writing…'
            : 'Processing…';
      setStreamThinkingLabel(stream, nextLabel);
    }
    paintStreamIntoView(convo, stream, replyText, streaming);
  });
  const markFirstToken = () => {
    if (firstTokenAt == null) firstTokenAt = Date.now();
  };
  const onAgentEvent = (payload) => {
    if (payload.phase === 'content_clear') {
      // Keep prior think/tool cards — seal the live buffer, then apply any
      // authoritative preface the server resolved for this tool round.
      commitStreamBuffer(stream, typer);
      if (payload.reasoning != null) ensureSealedTimelineThink(stream, payload.reasoning);
      if (payload.text != null) ensureSealedTimelineText(stream, payload.text);
      if (stream.dom) stream.dom.row.dataset.raw = '';
      if (activeId === convo.id && stream.dom) {
        paintStreamIntoView(convo, stream, typer.shown || '', true);
      }
    } else if (payload.phase === 'tool_prepare' || payload.phase === 'tool_call') {
      commitStreamBuffer(stream, typer);
      if (stream.dom) stream.dom.row.dataset.raw = '';
      const part = upsertLiveToolPart(stream, payload);
      if (stream.dom) {
        setStreamThinkingLabel(stream, liveToolStatusLabel(stream, payload));
      }
    } else if (payload.phase === 'tool_approval') {
      const part = upsertLiveToolPart(stream, payload);
      if (part) {
        part.approval = 'pending';
        part.approvalRisk = payload.risk ? String(payload.risk) : (part.approvalRisk || 'write');
        part.executing = false;
      }
      if (stream.dom) {
        setStreamThinkingLabel(stream, liveToolStatusLabel(stream, payload));
      }
      if (typeof refreshNotificationsUi === 'function') refreshNotificationsUi();
    } else if (payload.phase === 'tool_executing') {
      const part = upsertLiveToolPart(stream, payload);
      if (part) {
        part.approval = 'allowed';
        part.executing = true;
      }
      if (stream.dom) {
        setStreamThinkingLabel(stream, liveToolStatusLabel(stream, payload));
      }
      if (typeof refreshNotificationsUi === 'function') refreshNotificationsUi();
    } else if (payload.phase === 'terminal') {
      if (typeof onAgentTerminalEvent === 'function') onAgentTerminalEvent(payload);
    } else if (payload.phase === 'tool_result') {
      const resultText = payload.result
        ? String(payload.result).trim()
        : payload.preview
          ? String(payload.preview).trim()
          : 'Done';
      const note = payload.note ? String(payload.note).trim() : '';
      const name = payload.name || 'skill';
      const id = payload.id ? String(payload.id) : '';
      const commandSessionId = payload.command_session_id ? String(payload.command_session_id) : '';
      if (commandSessionId && !payload.running) {
        for (const part of stream.timeline) {
          if (part.type === 'tool' && part.commandSessionId === commandSessionId && part.running) {
            part.running = false;
            part.ok = payload.ok !== false;
            part.note = payload.ok === false ? 'Process failed; see latest session result' : 'Process completed; see latest session result';
          }
        }
      }
      const liveTools = stream.timeline.filter((part) => part.type === 'tool' && part.live);
      const last = (id && stream.timeline.find((part) => part.type === 'tool' && part.id === id))
        || (!id && (liveTools.find((part) => part.name === name) || liveTools[liveTools.length - 1]));
      if (last && (!id || last.id === id || last.name === name)) {
        last.live = false;
        last.executing = false;
        last.result = resultText;
        last.ok = payload.ok !== false;
        last.running = !!payload.running;
        last.commandSessionId = commandSessionId;
        last.endedAt = Date.now();
        last.durationMs = last.startedAt
          ? Math.max(0, last.endedAt - last.startedAt)
          : 0;
        last.justSettled = true;
        scheduleJustSettledClear(last);
        last.note = note;
        last.approval = payload.ok === false && /denied/i.test(resultText) ? 'denied' : '';
        if (payload.image && /^data:image\//i.test(String(payload.image))) {
          last.image = String(payload.image);
        }
        if (payload.image_id) {
          const imageId = String(payload.image_id);
          last.imageId = imageId;
          if (last.image) {
            if (!stream.images) stream.images = Object.create(null);
            stream.images[imageId] = last.image;
          }
        }
      } else {
        stream.timeline.push({
          type: 'tool',
          id,
          name: payload.name || 'skill',
          detail: '',
          result: resultText,
          ok: payload.ok !== false,
          running: !!payload.running,
          commandSessionId,
          approval: payload.ok === false && /denied/i.test(resultText) ? 'denied' : '',
          note,
          ...(payload.image && /^data:image\//i.test(String(payload.image))
            ? { image: String(payload.image) }
            : {}),
          ...(payload.image_id ? { imageId: String(payload.image_id) } : {}),
          live: false,
          justSettled: true,
        });
        scheduleJustSettledClear(stream.timeline[stream.timeline.length - 1]);
      }
      if (
        payload.image_id
        && payload.image
        && /^data:image\//i.test(String(payload.image))
      ) {
        if (!stream.images) stream.images = Object.create(null);
        stream.images[String(payload.image_id)] = String(payload.image);
      }
      if (stream.dom) {
        setStreamThinkingLabel(stream, liveToolStatusLabel(stream, payload));
      }
      if (typeof refreshNotificationsUi === 'function') refreshNotificationsUi();
    } else if (payload.phase === 'clarify') {
      commitStreamBuffer(stream, typer);
      if (stream.dom) stream.dom.row.dataset.raw = '';
      const clarifyPart = {
        type: 'clarify',
        id: String(payload.id || ''),
        startedAt: Date.now(),
        questions: Array.isArray(payload.questions) ? payload.questions : [],
        draft: {},
        answers: null,
        summary: '',
        live: true,
        submitting: false,
      };
      stream.timeline.push(clarifyPart);
      if (stream.dom) {
        setStreamThinkingLabel(stream, 'Waiting for your answers…');
        mountClarifyForm(stream, clarifyPart);
      }
      if (typeof refreshNotificationsUi === 'function') refreshNotificationsUi();
    } else if (payload.phase === 'clarify_done') {
      const part = findClarifyPart(stream, String(payload.id || ''));
      if (part) {
        part.live = false;
        part.submitting = false;
        part.answers = payload.answers || {};
        part.summary = String(payload.summary || '').trim();
        mountClarifyForm(stream, part);
      }
      if (stream.dom) setStreamThinkingLabel(stream, 'Researching…');
      if (typeof refreshNotificationsUi === 'function') refreshNotificationsUi();
    } else if (payload.phase === 'steer_ready' && payload.id) {
      stream.steerId = String(payload.id);
      void flushPendingSteers(stream);
      if (activeId === convo.id) renderOutboundQueue(convo);
    } else if (payload.phase === 'steer' && payload.content != null) {
      const text = String(payload.content).trim();
      const clientId = payload.client_id != null ? String(payload.client_id) : '';
      const pending = stream.pendingSteers || [];
      let entryIdx = -1;
      if (clientId) {
        entryIdx = pending.findIndex(
          (entry) => !entry.applied && entry.item?.id === clientId
        );
      }
      if (entryIdx < 0) {
        entryIdx = pending.findIndex((entry) => entry.text === text && !entry.applied);
      }
      if (entryIdx < 0) {
        entryIdx = pending.findIndex((entry) => !entry.applied);
      }
      const entry = entryIdx >= 0 ? pending.splice(entryIdx, 1)[0] : null;
      if (entry) entry.applied = true;
      applySteeredEntry(convo, stream, text, entry);
      if (stream.dom) setStreamThinkingLabel(stream, 'Steering…');
    } else if (payload.phase === 'status' && payload.message) {
      const msg = String(payload.message);
      const waiting = /waiting for (your )?approval/i.test(msg);
      const canAsk = stream.timeline.some((part) => part.type === 'tool' && part.approval === 'pending');
      if (stream.dom && (!waiting || canAsk)) {
        setStreamThinkingLabel(stream, msg);
      }
    } else if (payload.phase === 'notice' && payload.message) {
      const text = String(payload.message).trim();
      if (text && !stream.timeline.some((part) => part.type === 'notice' && part.content === text)) {
        stream.timeline.push({ type: 'notice', content: text });
      }
      if (stream.dom && text) setStreamThinkingLabel(stream, text);
    }
    paintStreamIntoView(convo, stream, typer.shown, true);
  };

  if (stream.catchingUp) typer.setCatchingUp(true);

  try {
    if (response && response.body) {
      const reader = response.body.getReader();
      const decoder = new TextDecoder();
      let buffer = '';
      for (;;) {
        const { value, done } = await reader.read();
        if (done) break;
        buffer += decoder.decode(value, { stream: true });
        const events = buffer.split(/\r?\n\r?\n/);
        buffer = events.pop() ?? '';
        for (const rawEvent of events) {
          const parsed = parseSseEvent(rawEvent);
          if (!parsed || parsed.data === '' || parsed.data === '[DONE]') continue;
          if (parsed.event === 'meta') {
            try {
              const meta = JSON.parse(parsed.data);
              if (meta.turn_id) {
                stream.turnId = String(meta.turn_id);
                if (typeof rememberHandledLiveTurn === 'function') {
                  rememberHandledLiveTurn(stream.turnId);
                }
              }
              if (typeof meta.agent === 'boolean') stream.useAgent = meta.agent;
              if (typeof meta.deep_research === 'boolean') stream.deepResearch = meta.deep_research;
              if (meta.deep_research_output === 'brief' || meta.deep_research_output === 'long') {
                stream.deepResearchOutput = meta.deep_research_output;
              }
              if (meta.model) stream.turnModel = String(meta.model);
            } catch {
              // ignore malformed meta
            }
            continue;
          }
          if (parsed.event === 'live') {
            stream.catchingUp = false;
            typer.setCatchingUp(false);
            continue;
          }
          if (parsed.event === 'error') {
            try {
              stream.errorMessage = JSON.parse(parsed.data).error || parsed.data;
            } catch {
              stream.errorMessage = parsed.data;
            }
            continue;
          }
          if (parsed.event === 'agent') {
            try {
              onAgentEvent(JSON.parse(parsed.data));
            } catch {
              // ignore malformed agent frames
            }
            continue;
          }
          let json;
          try {
            json = JSON.parse(parsed.data);
          } catch {
            continue;
          }
          ingestStreamUsage(usageStats, json);
          const choice = json.choices && json.choices[0];
          const deltaObj = (choice && choice.delta) || {};
          const reasoning =
            deltaObj.reasoning_content ||
            deltaObj.reasoning ||
            (choice && choice.message && (choice.message.reasoning_content || choice.message.reasoning)) ||
            '';
          if (reasoning) {
            markFirstToken();
            typer.pushReasoning(String(reasoning));
          }
          const delta = deltaObj.content || '';
          if (delta) {
            markFirstToken();
            typer.push(delta);
          }
        }
      }
    }
  } catch (error) {
    if (error.name === 'AbortError') {
      if (stream.replaced) {
        dropLiveSubscriber(convo.id, stream);
        return;
      }
      if (!stream.cancelled) {
        dropLiveSubscriber(convo.id, stream);
        if (!stream.skipQueue) maybeSendNextQueued(convo.id);
        return;
      }
    } else {
      stream.errorMessage = error.message || 'The request failed.';
    }
  }

  let queueAfter = !stream.skipQueue && !stream.replaced;
  try {
  if (stream.replaced || activeStreams.get(convo.id) !== stream) {
    queueAfter = false;
    return;
  }
  typer.flush();
  if (!conversations.some((item) => item.id === convo.id)) {
    return;
  }

  // Stop mid-turn: keep any useful partial reply; never spam empty "No response." rows.
  if (stream.cancelled || (typeof isBotsOutboundStopped === 'function' && isBotsOutboundStopped(convo.id))) {
    queueAfter = false;
    stream.cancelled = true;
    const cancelledExtracted = collectTurnMemoryExtraction(stream, typer.target);
    const cancelledText = String(cancelledExtracted.cleaned || '').trim();
    const cancelledVisible = streamingAnswerText(cancelledText).trim();
    const silentNoReply = typeof isSilentNoReply === 'function'
      && (isSilentNoReply(cancelledVisible) || isSilentNoReply(cancelledText));
    const speakerBotId = stream.speakerBotId || null;
    const speakerBot = speakerBotId && typeof getBot === 'function' ? getBot(speakerBotId, convo) : null;
    // Never persist the placeholder error as a real message.
    if (cancelledVisible && cancelledVisible !== 'No response.' && !silentNoReply) {
      const message = {
        role: 'assistant',
        content: cancelledText,
        model: stream.turnModel
          || fallbackTurnModel
          || String(selectedChatModel || '').trim()
          || 'model',
      };
      if (speakerBot) {
        message.speakerId = speakerBot.id;
        message.speakerHandle = speakerBot.handle;
      }
      if (stream.loopPhase) {
        message.loopRunId = stream.loopPhase.runId || '';
        message.loopPhaseId = stream.loopPhase.id || '';
        message.loopPhaseLabel = stream.loopPhase.label || '';
        message.loopPhaseIndex = stream.loopPhase.index || null;
        message.loopPhaseTotal = stream.loopPhase.total || null;
      }
      const committedMessage = commitLiveAssistant(convo, message, stream.turnId);
      const msgIndex = convo.messages.indexOf(committedMessage);
      const viewing = activeId === convo.id;
      // Do not recreate a discarded live row just to settle a cancel.
      let dom = stream.dom && stream.dom.row && stream.dom.row.isConnected
        ? stream.dom
        : null;
      if (!dom && viewing && !stream.hardStopped) {
        // Partial text exists but UI was torn down — rebuild once to settle.
        dom = ensureStreamDom(convo, stream);
      }
      if (dom && dom.row.isConnected) {
        try { dom.thinkingOrb?.stop(); } catch { /* ignore */ }
        dom.statusEl.classList.add('is-hidden');
        delete dom.row.dataset.streamId;
        dom.row.dataset.msgIndex = String(msgIndex);
        dom.row.dataset.raw = committedMessage.content || '';
        syncMessageSpeaker(dom.row, committedMessage);
        settleAssistantRow(dom.row, committedMessage, { animateCollapse: false });
      }
      convo.updatedAt = Date.now();
      saveConversations({ immediate: true });
    } else {
      discardLiveStreamRow(stream);
    }
    return;
  }

  const endedAt = Date.now();
  const finalStats = finalizeTurnStats(usageStats, firstTokenAt, endedAt);
  const speakerBotId = stream.speakerBotId || null;
  const speakerBot = speakerBotId && typeof getBot === 'function' ? getBot(speakerBotId, convo) : null;
  const extracted = collectTurnMemoryExtraction(stream, typer.target);
  const memoryChanges = applyExtractedMemories(convo, extracted, speakerBotId);
  const memoryNotices = memoryNoticeLabels(memoryChanges);
  // Memory chips live on the message; do not also push tone:ok notices into the
  // timeline (those were deferred to the bottom and duplicated the chips).
  let assistantText = extracted.cleaned;
  if (!assistantText && !stream.errorMessage) {
    assistantText = memoryOnlyAssistantFallback(extracted);
  }
  const visibleAnswer = streamingAnswerText(assistantText).trim();
  const silentNoReply = typeof isSilentNoReply === 'function'
    && (isSilentNoReply(visibleAnswer) || isSilentNoReply(assistantText));
  if (silentNoReply) {
    discardLiveStreamRow(stream);
    return;
  }
  // Think-only finals used to persist a blank desktop bubble while Activity looked fine.
  if (!visibleAnswer && !stream.errorMessage && stream.timeline.length) {
    stream.errorMessage = 'No user-visible answer after tools. Try again.';
  }

  // If Stop won the race after we left the cancel branch, still refuse placeholder spam.
  if (
    stream.cancelled
    || (typeof isBotsOutboundStopped === 'function' && isBotsOutboundStopped(convo.id))
  ) {
    queueAfter = false;
    discardLiveStreamRow(stream);
    return;
  }

  stream.timeline.forEach((part) => {
    if (part.type === 'tool') {
      if (part.live && part.startedAt && !Number.isFinite(part.durationMs)) {
        part.endedAt = Date.now();
        part.durationMs = Math.max(0, part.endedAt - part.startedAt);
      }
      part.live = false;
    } else if (part.type === 'clarify') {
      part.live = false;
    }
  });
  // Seal any leftover buffer so a final think/answer round joins the timeline
  // only as live content (not duplicated into parts).
  const viewing = activeId === convo.id;
  let dom = viewing ? ensureStreamDom(convo, stream) : stream.dom;
  const persistedParts = stream.timeline.length
    ? stream.timeline.map((part) => (
      part.type === 'tool'
        ? {
          type: 'tool',
          name: part.name,
          detail: part.detail,
          result: part.result,
          ...(typeof part.ok === 'boolean' ? { ok: part.ok } : {}),
          ...(part.commandSessionId ? { commandSessionId: part.commandSessionId, running: !!part.running } : {}),
          ...(part.approval === 'denied' ? { approval: 'denied' } : {}),
          ...(part.note ? { note: part.note } : {}),
          ...(part.kind ? { kind: part.kind } : {}),
          ...(Number.isFinite(part.durationMs) ? { durationMs: Math.round(part.durationMs) } : {}),
          ...(part.image ? { image: part.image } : {}),
          live: false,
        }
        : part.type === 'clarify'
          ? {
            type: 'clarify',
            id: part.id,
            questions: part.questions || [],
            answers: part.answers || null,
            summary: part.summary || '',
            live: false,
          }
        : part.type === 'notice'
          ? {
            type: 'notice',
            content: part.content,
            ...(part.tone ? { tone: part.tone } : {}),
            ...(part.kind ? { kind: part.kind } : {}),
          }
          : { type: part.type, content: part.content }
    ))
    : null;

  if (viewing && dom) {
    const savedClarify = preserveClarifyHost(dom.answerEl);
    if (stream.errorMessage && !visibleAnswer) {
      const errHtml = '<span class="msg-error">' + escapeHtml(stream.errorMessage) + '</span>';
      dom.answerEl.innerHTML = isDesktopTraceLayout()
        ? errHtml
        : (renderCommittedParts(stream.timeline) + errHtml);
    } else if (stream.errorMessage) {
      paintStreamIntoView(convo, stream, assistantText, false);
      dom = stream.dom;
      if (dom) {
        dom.answerEl.insertAdjacentHTML(
          'beforeend',
          '<span class="msg-error">' + escapeHtml(stream.errorMessage) + '</span>'
        );
      }
    } else if (!visibleAnswer) {
      if (!stream.errorMessage) stream.errorMessage = 'No response.';
      const errHtml = '<span class="msg-error">' + escapeHtml(stream.errorMessage) + '</span>';
      if (isDesktopTraceLayout()) {
        dom.answerEl.innerHTML = errHtml;
      } else {
        const committed = renderCommittedParts(stream.timeline);
        dom.answerEl.innerHTML = committed ? committed + errHtml : errHtml;
      }
    } else {
      paintStreamIntoView(convo, stream, assistantText, false);
      dom = stream.dom;
    }
    if (dom) {
      restoreClarifyHost(dom.answerEl, savedClarify || dom.clarifyHost);
      const doneClarify = stream.timeline.find((part) => part.type === 'clarify');
      if (doneClarify) mountClarifyForm(stream, doneClarify);
      dom.thinkingOrb.stop();
      dom.statusEl.classList.add('is-hidden');
      delete dom.row.dataset.streamId;
    }
  }

  if (assistantText || persistedParts || memoryNotices.length || stream.errorMessage) {
    const message = {
      role: 'assistant',
      content: visibleAnswer ? assistantText : '',
      model: stream.turnModel
        || finalStats?.upstreamModel
        || fallbackTurnModel
        || String(selectedChatModel || '').trim()
        || 'model',
    };
    if (speakerBot) {
      message.speakerId = speakerBot.id;
      message.speakerHandle = speakerBot.handle;
    }
    if (stream.loopPhase) {
      message.loopRunId = stream.loopPhase.runId || '';
      message.loopPhaseId = stream.loopPhase.id || '';
      message.loopPhaseLabel = stream.loopPhase.label || '';
      message.loopPhaseIndex = stream.loopPhase.index || null;
      message.loopPhaseTotal = stream.loopPhase.total || null;
    }
    if (persistedParts) message.parts = persistedParts;
    if (stream.images && Object.keys(stream.images).length) {
      message.images = { ...stream.images };
    }
    if (memoryNotices.length) message.memoryNotices = memoryNotices;
    if (stream.errorMessage && !visibleAnswer) {
      message.error = stream.errorMessage;
      if (!message.content) message.content = stream.errorMessage;
    }
    if (finalStats?.tokensPerSec != null) message.tokensPerSec = finalStats.tokensPerSec;
    if (finalStats?.completionTokens != null) message.completionTokens = finalStats.completionTokens;
    if (finalStats?.promptTokens != null) message.promptTokens = finalStats.promptTokens;
    const committedMessage = commitLiveAssistant(convo, message, stream.turnId);
    if (typeof applyBotActions === 'function' && speakerBot) {
      applyBotActions(convo, speakerBot, extracted.botActions);
    }
    const msgIndex = convo.messages.indexOf(committedMessage);
    if (dom && dom.row.isConnected) {
      dom.row.dataset.msgIndex = String(msgIndex);
      dom.row.dataset.raw = committedMessage.content || '';
      syncMessageSpeaker(dom.row, committedMessage);
      settleAssistantRow(dom.row, committedMessage, { animateCollapse: true });
    }
    convo.updatedAt = Date.now();
    if (typeof recordConversationNotification === 'function') {
      recordConversationNotification(convo, {
        kind: stream.errorMessage ? 'error' : 'complete',
        at: endedAt,
      });
    }
    if (convo.projectId) {
      const project = getProject(convo.projectId);
      if (project) project.updatedAt = Date.now();
    }
    saveConversations({ immediate: true });
    if (activeId === convo.id) {
      selectTraceMessage(msgIndex, {
        animate: true,
        ensureOpen: false,
      });
      if (messageHasActivity(committedMessage)) maybeAutoOpenTraceSidebar(convo.id);
    }
  } else {
    discardLiveStreamRow(stream);
  }

  // After the first reply finishes — local servers often reject concurrent
  // title + chat requests, so we wait until the stream is done.
  if (needsGeneratedTitle(convo)) {
    generateConversationTitle(convo, firstUserText(convo));
  }
  if (activeId === convo.id) {
    // If we finished while away and came back to a thread without the row, re-render.
    if (!dom || !dom.row.isConnected) {
      renderThread(convo);
    }
    composerInput.focus();
  }
  } catch (error) {
    console.warn('Assistant turn finalize failed:', error?.message || error);
    if (!stream.errorMessage) {
      stream.errorMessage = error?.message || 'The request failed.';
    }
  } finally {
    const finished = activeStreams.get(convo.id) === stream && finishLiveStream(convo.id, stream);
    if (finished && queueAfter) {
      maybeSendNextQueued(convo.id);
    }
    if (!stream.skipQueue && typeof flushBotNavigation === 'function') {
      flushBotNavigation();
    }
  }
}

chatThread.addEventListener('click', (event) => {
  const queueSteer = event.target.closest('[data-queue-steer]');
  if (queueSteer && chatThread.contains(queueSteer)) {
    event.preventDefault();
    const queueId = queueSteer.getAttribute('data-queue-steer');
    if (activeId && queueId) steerQueuedOutbound(activeId, queueId);
    return;
  }
  const steerCancel = event.target.closest('[data-steer-cancel]');
  if (steerCancel && chatThread.contains(steerCancel)) {
    event.preventDefault();
    const queueId = steerCancel.getAttribute('data-steer-cancel');
    if (activeId && queueId) cancelPendingSteer(activeId, queueId);
    return;
  }
  const queueRemove = event.target.closest('[data-queue-remove]');
  if (queueRemove && chatThread.contains(queueRemove)) {
    event.preventDefault();
    const queueId = queueRemove.getAttribute('data-queue-remove');
    if (activeId && queueId) removeQueuedOutbound(activeId, queueId);
    return;
  }
  const queueEdit = event.target.closest('[data-queue-edit]');
  if (queueEdit && chatThread.contains(queueEdit)) {
    event.preventDefault();
    const queueId = queueEdit.getAttribute('data-queue-edit');
    const row = queueId
      ? [...chatThread.querySelectorAll('.msg-queued')].find((el) => el.dataset.queueId === queueId)
      : null;
    if (row) beginQueuedMessageEdit(row);
    return;
  }
  const clarifySubmit = event.target.closest('[data-clarify-submit]');
  if (clarifySubmit && chatThread.contains(clarifySubmit)) {
    event.preventDefault();
    const clarifyId = clarifySubmit.getAttribute('data-clarify-submit');
    const stream = activeStream();
    if (stream && clarifyId) void submitClarifyForm(stream, clarifyId);
    return;
  }
  const allowBtn = event.target.closest('[data-tool-allow]');
  const denyBtn = event.target.closest('[data-tool-deny]');
  if ((allowBtn || denyBtn) && chatThread.contains(allowBtn || denyBtn)) {
    event.preventDefault();
    const id = (allowBtn || denyBtn).getAttribute(allowBtn ? 'data-tool-allow' : 'data-tool-deny');
    if (id) void submitToolApproval(id, !!allowBtn);
    return;
  }
  const foldToggle = event.target.closest('.agent-timeline-fold-toggle');
  if (foldToggle && chatThread.contains(foldToggle)) {
    event.preventDefault();
    const fold = foldToggle.closest('.agent-timeline-fold');
    if (!fold) return;
    const collapsed = fold.classList.toggle('is-collapsed');
    foldToggle.setAttribute('aria-expanded', collapsed ? 'false' : 'true');
    if (fold.classList.contains('agent-process-notes')) {
      const row = fold.closest('.msg-role-assistant');
      const live = row && activeId ? activeStreams.get(activeId) : null;
      if (live && live.dom?.row === row) live.processNotesCollapsed = collapsed;
    }
    return;
  }
  const btn = event.target.closest('.md-code-copy');
  if (btn && chatThread.contains(btn)) {
    event.preventDefault();
    copyCodeBlock(btn);
    return;
  }
  if (event.target.closest('.msg-action, a, button, summary, details, input, textarea, .clarify-card, #selectionReplyBar')) {
    return;
  }
  const sel = window.getSelection();
  if (sel && !sel.isCollapsed && String(sel.toString() || '').trim()) {
    // Don't steal focus from a text selection / Reply affordance.
    return;
  }
  const row = event.target.closest('.msg-role-assistant');
  if (!row || !chatThread.contains(row)) return;
  const index = Number(row.dataset.msgIndex);
  if (!Number.isFinite(index)) return;
  selectTraceMessage(index, { animate: true, ensureOpen: false });
});

document.addEventListener('click', (event) => {
  const allowBtn = event.target.closest('[data-tool-allow]');
  const denyBtn = event.target.closest('[data-tool-deny]');
  if (!allowBtn && !denyBtn) return;
  if (chatThread.contains(allowBtn || denyBtn)) return;
  event.preventDefault();
  const id = (allowBtn || denyBtn).getAttribute(allowBtn ? 'data-tool-allow' : 'data-tool-deny');
  if (id) void submitToolApproval(id, !!allowBtn);
});

chatThread.addEventListener('change', (event) => {
  const input = event.target.closest('.clarify-card input');
  if (!input || !chatThread.contains(input)) return;
  const card = input.closest('.clarify-card');
  const host = card?.closest('.clarify-host');
  const clarifyId = card?.dataset.clarifyId;
  const stream = activeStream();
  const part = stream && clarifyId ? findClarifyPart(stream, clarifyId) : null;
  if (!part || !part.live) return;
  const qi = Number(input.dataset.clarifyQ);
  if (!Number.isFinite(qi)) return;
  if (!part.draft) part.draft = {};
  const qEl = card.querySelector('.clarify-q[data-clarify-q="' + qi + '"]');
  const multi = !!part.questions?.[qi]?.multiSelect;
  if (input.dataset.clarifyOther) {
    if (input.checked) {
      if (!multi) {
        qEl?.querySelectorAll('input[data-clarify-opt]').forEach((el) => { el.checked = false; });
      }
      part.draft[qi] = {
        labels: [],
        custom: String(qEl?.querySelector('[data-clarify-custom]')?.value || ''),
        useCustom: true,
      };
    }
  } else if (input.dataset.clarifyOpt != null) {
    const other = qEl?.querySelector('input[data-clarify-other]');
    if (other && !multi) other.checked = false;
    const labels = [...(qEl?.querySelectorAll('input[data-clarify-opt]:checked') || [])].map((el) => el.value);
    part.draft[qi] = {
      labels,
      custom: String(part.draft[qi]?.custom || ''),
      useCustom: false,
    };
  }
  mountClarifyForm(stream, part);
  const custom = host?.querySelector('.clarify-q[data-clarify-q="' + qi + '"] [data-clarify-custom]');
  if (part.draft[qi]?.useCustom && custom) {
    custom.focus();
    custom.selectionStart = custom.value.length;
  }
});

chatThread.addEventListener('input', (event) => {
  const custom = event.target.closest('[data-clarify-custom]');
  if (!custom || !chatThread.contains(custom)) return;
  const card = custom.closest('.clarify-card');
  const host = card?.closest('.clarify-host');
  const clarifyId = card?.dataset.clarifyId;
  const stream = activeStream();
  const part = stream && clarifyId ? findClarifyPart(stream, clarifyId) : null;
  if (!part || !part.live) return;
  const qi = Number(custom.dataset.clarifyQ);
  if (!Number.isFinite(qi)) return;
  if (!part.draft) part.draft = {};
  part.draft[qi] = {
    labels: [],
    custom: custom.value,
    useCustom: true,
  };
  syncClarifySubmitEnabled(host);
});

btnSend.addEventListener('click', () => { void sendMessage(); });
btnBranch?.addEventListener('click', () => { void sendMessage({ branch: true }); });
btnSelectionReply?.addEventListener('mousedown', (event) => {
  // Keep the selection until we capture it in the click handler.
  event.preventDefault();
});
btnSelectionReply?.addEventListener('click', (event) => {
  event.preventDefault();
  event.stopPropagation();
  const quote = assistantSelectionQuote();
  if (quote) {
    const index = Number(quote.row?.dataset.msgIndex);
    const convo = conversations.find((item) => item.id === activeId);
    const message = Number.isInteger(index) ? convo?.messages?.[index] : null;
    setPendingReply(quote.text, resolveReplySpeaker(convo, quote.row, message));
  } else {
    hideSelectionReplyBar();
  }
});
document.addEventListener('selectionchange', () => {
  // Defer so mouseup can finish updating the range first.
  queueMicrotask(syncSelectionReplyBar);
});
document.addEventListener('mouseup', () => {
  queueMicrotask(syncSelectionReplyBar);
});
chatViewport?.addEventListener('scroll', hideSelectionReplyBar, { passive: true });
window.addEventListener('scroll', hideSelectionReplyBar, true);
window.addEventListener('resize', hideSelectionReplyBar);
function kickVisibleLiveToolMotion() {
  if (typeof kickLiveToolMotion !== 'function') return;
  if (traceSidebarBody) kickLiveToolMotion(traceSidebarBody);
  if (chatThread) kickLiveToolMotion(chatThread);
}
window.addEventListener('focus', kickVisibleLiveToolMotion);
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState === 'visible') kickVisibleLiveToolMotion();
});
document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') hideSelectionReplyBar();
});
btnStop.addEventListener('click', () => {
  if (!activeId) return;
  const stoppedId = activeId;
  if (typeof pauseOutboundQueueAfterStop === 'function') pauseOutboundQueueAfterStop(stoppedId);
  void abortStream(stoppedId);
  syncComposerStreamUi();
  renderSidebar();
  const convo = conversations.find((item) => item.id === stoppedId);
  // Drop any live "Processing…" / empty error bubble that finalize hasn't cleared yet.
  if (convo && activeId === stoppedId) {
    chatThread.querySelectorAll('.msg-role-assistant[data-stream-id]').forEach((row) => {
      row.remove();
    });
  }
  updateComposerHint();
});
btnPlus?.addEventListener('click', (event) => {
  event.stopPropagation();
  setPlusMenuOpen(!plusMenuIsOpen());
});
plusMenu?.addEventListener('click', (event) => {
  const item = event.target.closest('[data-plus-action]');
  if (!item || item.disabled) return;
  handlePlusMenuAction(item.dataset.plusAction);
});
btnMic?.addEventListener('click', () => {
  if (btnMic.getAttribute('aria-disabled') === 'true') {
    showVoiceHint(
      diskEncryptionLocked()
        ? 'Unlock local data to use voice input.'
        : 'Connect a provider before using voice input.'
    );
    return;
  }
  toggleVoiceInput();
});
attachFileInput.addEventListener('change', async (event) => {
  const files = event.target.files;
  event.target.value = '';
  if (files && files.length) await addFilesToPending(files);
});
composerAttachmentsEl.addEventListener('click', (event) => {
  const btn = event.target.closest('[data-attach-remove]');
  if (!btn) return;
  removePendingAttachment(btn.getAttribute('data-attach-remove'));
});
;['dragenter', 'dragover'].forEach((type) => {
  composerCard.addEventListener(type, (event) => {
    if (!attachmentsUiEnabled()) return;
    event.preventDefault();
    composerCard.classList.add('is-dragover');
  });
});
;['dragleave', 'drop'].forEach((type) => {
  composerCard.addEventListener(type, (event) => {
    event.preventDefault();
    if (type === 'dragleave' && composerCard.contains(event.relatedTarget)) return;
    composerCard.classList.remove('is-dragover');
    if (type === 'drop' && event.dataTransfer?.files?.length) {
      void addFilesToPending(event.dataTransfer.files);
    }
  });
});
composerInput.addEventListener('paste', (event) => {
  const items = event.clipboardData?.items;
  if (!items || !attachmentsUiEnabled()) return;
  const files = [];
  for (const item of items) {
    if (item.kind === 'file') {
      const file = item.getAsFile();
      if (file) files.push(file);
    }
  }
  if (!files.length) return;
  event.preventDefault();
  void addFilesToPending(files);
});
document.getElementById('settingAttachmentTextFallback').addEventListener('change', syncAttachmentFallbackControls);

const APP_SURFACES = {
  chat: 'Agent',
  bots: 'Loops',
};
let wordmarkMenuCloseTimer = 0;
let wordmarkLabelTimer = 0;

function wordmarkMenuIsOpen() {
  const wrap = document.getElementById('wordmarkSwitch');
  const menu = document.getElementById('wordmarkMenu');
  return !!(wrap && menu && wrap.classList.contains('is-open') && !menu.classList.contains('is-hidden'));
}

function setWordmarkMenuOpen(open) {
  const wrap = document.getElementById('wordmarkSwitch');
  const menu = document.getElementById('wordmarkMenu');
  const btn = document.getElementById('btnWordmarkSurface');
  if (!wrap || !menu || !btn) return;
  if (wordmarkMenuCloseTimer) {
    window.clearTimeout(wordmarkMenuCloseTimer);
    wordmarkMenuCloseTimer = 0;
  }
  if (open) {
    setThinkMenuOpen(false);
    setPlusMenuOpen(false);
    closeConvoMenu();
    menu.classList.remove('is-hidden');
    btn.setAttribute('aria-expanded', 'true');
    void menu.offsetWidth;
    requestAnimationFrame(() => wrap.classList.add('is-open'));
    return;
  }
  wrap.classList.remove('is-open');
  btn.setAttribute('aria-expanded', 'false');
  const finish = () => {
    menu.classList.add('is-hidden');
    wordmarkMenuCloseTimer = 0;
  };
  if (prefersReducedMotion()) {
    finish();
    return;
  }
  wordmarkMenuCloseTimer = window.setTimeout(finish, 220);
}

function paintWordmarkSurface(id) {
  const next = APP_SURFACES[id] ? id : 'chat';
  const label = APP_SURFACES[next];
  const btn = document.getElementById('btnWordmarkSurface');
  const labelEl = document.getElementById('wordmarkSurfaceLabel');
  const menu = document.getElementById('wordmarkMenu');
  const changed = appSurface !== next;
  appSurface = next;
  document.getElementById('chatShell')?.setAttribute('data-surface', next);
  document.title = 'TensorMI Harness | ' + label;
  if (btn) btn.setAttribute('aria-label', 'Surface: ' + label + '. Switch surface');
  menu?.querySelectorAll('[data-surface]').forEach((item) => {
    const on = item.dataset.surface === next;
    item.classList.toggle('is-active', on);
    item.setAttribute('aria-checked', on ? 'true' : 'false');
  });
  if (!labelEl || labelEl.textContent === label) return;
  if (!changed || prefersReducedMotion()) {
    labelEl.classList.remove('is-exit', 'is-enter');
    labelEl.textContent = label;
    return;
  }
  if (wordmarkLabelTimer) window.clearTimeout(wordmarkLabelTimer);
  labelEl.classList.remove('is-enter');
  labelEl.classList.add('is-exit');
  wordmarkLabelTimer = window.setTimeout(() => {
    labelEl.textContent = label;
    labelEl.classList.add('is-enter');
    labelEl.classList.remove('is-exit');
    void labelEl.offsetWidth;
    labelEl.classList.remove('is-enter');
    wordmarkLabelTimer = 0;
  }, 160);
}

function wordmarkMenuItems() {
  const menu = document.getElementById('wordmarkMenu');
  return menu ? [...menu.querySelectorAll('[data-surface]')] : [];
}

function focusWordmarkMenuItem(index) {
  const items = wordmarkMenuItems();
  if (!items.length) return;
  const next = (index + items.length) % items.length;
  items[next]?.focus();
}

document.getElementById('btnWordmarkSurface')?.addEventListener('click', (event) => {
  event.stopPropagation();
  setWordmarkMenuOpen(!wordmarkMenuIsOpen());
});
document.getElementById('btnWordmarkSurface')?.addEventListener('keydown', (event) => {
  if (event.key !== 'ArrowDown' && event.key !== 'ArrowUp') return;
  event.preventDefault();
  setWordmarkMenuOpen(true);
  const items = wordmarkMenuItems();
  focusWordmarkMenuItem(event.key === 'ArrowUp' ? items.length - 1 : 0);
});
document.getElementById('wordmarkMenu')?.addEventListener('click', (event) => {
  const item = event.target.closest('[data-surface]');
  if (!item) return;
  if (typeof applyAppSurface === 'function') applyAppSurface(item.dataset.surface);
  else paintWordmarkSurface(item.dataset.surface);
  setWordmarkMenuOpen(false);
  document.getElementById('btnWordmarkSurface')?.focus();
});
document.getElementById('wordmarkMenu')?.addEventListener('keydown', (event) => {
  const items = wordmarkMenuItems();
  const index = items.indexOf(document.activeElement);
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    focusWordmarkMenuItem(index + 1);
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    focusWordmarkMenuItem(index < 0 ? items.length - 1 : index - 1);
  } else if (event.key === 'Home') {
    event.preventDefault();
    focusWordmarkMenuItem(0);
  } else if (event.key === 'End') {
    event.preventDefault();
    focusWordmarkMenuItem(items.length - 1);
  } else if (event.key === 'Escape') {
    event.preventDefault();
    setWordmarkMenuOpen(false);
    document.getElementById('btnWordmarkSurface')?.focus();
  }
});
paintWordmarkSurface(appSurface);

document.getElementById('btnThink').addEventListener('click', (event) => {
  event.stopPropagation();
  setThinkMenuOpen(!thinkMenuIsOpen());
});
document.getElementById('thinkMenu').addEventListener('click', (event) => {
  const item = event.target.closest('[data-effort]');
  if (!item) return;
  setThinkingEffort(item.dataset.effort, true);
  setThinkMenuOpen(false);
});
document.addEventListener('click', (event) => {
  const thinkWrap = document.getElementById('composerThinkWrap');
  const plusWrap = document.getElementById('composerPlusWrap');
  const wordmarkWrap = document.getElementById('wordmarkSwitch');
  if (thinkWrap && !thinkWrap.contains(event.target) && thinkMenuIsOpen()) {
    setThinkMenuOpen(false);
  }
  const clickedPlus = (plusWrap && plusWrap.contains(event.target))
    || (plusMenu && plusMenu.contains(event.target));
  if (!clickedPlus && plusMenuIsOpen()) {
    setPlusMenuOpen(false);
  }
  if (wordmarkWrap && !wordmarkWrap.contains(event.target) && wordmarkMenuIsOpen()) {
    setWordmarkMenuOpen(false);
  }
});
document.getElementById('settingThinkingEffort').addEventListener('change', (event) => {
  setThinkingEffort(event.target.value, true);
});
syncThinkingEffortControls(settings.thinkingEffort);
syncResearchControls();
renderPlusMenu();
syncComposerThinkVisibility(null);
syncAttachButton();
syncMicButton();
syncAttachmentFallbackControls();
document.getElementById('btnNewChat').addEventListener('click', () => {
  // New chat stays in the current project context when one is active.
  startDraft({ incognito: false });
});
btnNewIncognitoChat?.addEventListener('click', () => {
  startDraft({ incognito: true });
});
btnProjectsNav.addEventListener('click', showProjectsView);
btnNotificationsNav.addEventListener('click', showNotificationsView);
btnNotificationsMarkRead.addEventListener('click', () => {
  let changed = false;
  for (const convo of conversations) {
    changed = markConversationNotificationRead(convo, { persist: false }) || changed;
  }
  if (changed) saveConversations({ immediate: true });
  refreshNotificationsUi();
  renderSidebar();
});
document.addEventListener('visibilitychange', () => {
  if (document.visibilityState !== 'visible' || mainView !== 'chat' || !activeId) return;
  const convo = conversations.find((item) => item.id === activeId);
  if (!markConversationNotificationRead(convo)) return;
  renderSidebar();
});
document.getElementById('btnNewProject').addEventListener('click', createProject);
projectsSearch.addEventListener('input', renderProjectsPage);
projectsSort.addEventListener('change', renderProjectsPage);
const btnToggleSidebar = document.getElementById('btnToggleSidebar');
const btnExpandSidebar = document.getElementById('btnExpandSidebar');
const btnPrivacyMode = document.getElementById('btnPrivacyMode');
const sidebarMobileMq = window.matchMedia('(max-width: 820px)');

initTraceSidebarPreferred();
btnToggleTrace?.addEventListener('click', () => {
  const open = chatShell.classList.contains('trace-collapsed');
  setTraceSidebarOpen(open, { fromUser: true });
});
btnExpandTrace?.addEventListener('click', () => {
  stickTraceSidebar = true;
  setTraceSidebarOpen(true, { fromUser: true });
  scrollTraceSidebarToBottom({ force: true });
});
if (traceSidebarBody) {
  let lastTraceScrollTop = 0;
  traceSidebarBody.addEventListener('scroll', () => {
    const top = traceSidebarBody.scrollTop;
    const goingUp = top < lastTraceScrollTop - 1;
    lastTraceScrollTop = top;
    if (goingUp && !isTraceSidebarNearBottom()) {
      stickTraceSidebar = false;
    } else if (isTraceSidebarNearBottom()) {
      stickTraceSidebar = true;
    }
  }, { passive: true });
}
TRACE_DESKTOP_MQ.addEventListener('change', () => {
  const convo = conversations.find((item) => item.id === activeId);
  if (convo && !threadWrap.classList.contains('is-hidden')) {
    // Re-render so bubbles pick the desktop/mobile activity layout.
    const keep = selectedTraceMsgIndex;
    const wasLive = activeStreams.has(convo.id);
    renderThread(convo);
    // Live reattach already reselects the in-flight row.
    if (!wasLive && keep != null) selectTraceMessage(keep, { animate: false });
  } else {
    refreshTraceSidebar({ animate: false });
  }
});

function sidebarIsMobileDrawer() {
  return sidebarMobileMq.matches;
}

function sidebarIsOpen() {
  return sidebarIsMobileDrawer()
    ? chatShell.classList.contains('sidebar-open')
    : !chatShell.classList.contains('sidebar-collapsed');
}

function syncSidebarToggleUi() {
  const open = sidebarIsOpen();
  if (btnToggleSidebar) {
    btnToggleSidebar.setAttribute('aria-expanded', open ? 'true' : 'false');
    btnToggleSidebar.setAttribute('aria-label', open ? 'Hide sidebar' : 'Show sidebar');
    btnToggleSidebar.title = open ? 'Hide sidebar' : 'Show sidebar';
  }
  if (btnExpandSidebar) {
    btnExpandSidebar.setAttribute('aria-expanded', open ? 'true' : 'false');
    const encryptionDetail = btnExpandSidebar.dataset.encryptionDetail;
    const label = 'Show sidebar' + (encryptionDetail ? ' · ' + encryptionDetail : '');
    btnExpandSidebar.setAttribute('aria-label', label);
    btnExpandSidebar.title = label;
  }
}

function setSidebarOpen(open) {
  if (sidebarIsMobileDrawer()) {
    chatShell.classList.toggle('sidebar-open', open);
  } else {
    chatShell.classList.toggle('sidebar-collapsed', !open);
    if (storageReady && settings.sidebarCollapsed === open) {
      saveSettings({ ...settings, sidebarCollapsed: !open });
    }
  }
  syncSidebarToggleUi();
}

function syncPrivacyModeUi(enabled) {
  chatShell.classList.toggle('privacy-mode', enabled);
  syncIdentityTitles(enabled);
  const selected = modelMenuOptions.find((option) => option.value === selectedChatModel);
  if (chatModelSelect) {
    setIdentityTitle(
      chatModelSelect,
      selected ? modelOptionTitle(selected.label, selected.provider) : ''
    );
  }
  if (chatModelOriginPill) {
    setIdentityTitle(chatModelOriginPill, chatModelOriginPill.textContent);
  }
  chatModelList?.querySelectorAll('.chat-model-option').forEach((optionEl) => {
    const option = modelMenuOptions.find((item) => item.value === optionEl.dataset.value);
    const picking = !!modelMenuContext;
    const prefix = picking
      ? (optionEl.classList.contains('is-selected') ? 'Selected · ' : '')
      : (optionEl.classList.contains('is-selected') ? 'Default · ' : 'Set as default · ');
    setIdentityTitle(optionEl, option ? prefix + modelOptionTitle(option.label, option.provider) : '');
    const badge = optionEl.querySelector('.chat-model-origin-pill');
    if (badge) setIdentityTitle(badge, badge.textContent);
  });
  chatModelList?.querySelectorAll('.chat-model-group-name').forEach((label) => {
    applyPrivacyMosaic(label, 'model-menu-provider-group:' + label.textContent);
  });
  if (!btnPrivacyMode) return;
  btnPrivacyMode.classList.toggle('is-active', enabled);
  btnPrivacyMode.setAttribute('aria-pressed', enabled ? 'true' : 'false');
  btnPrivacyMode.setAttribute('aria-label', enabled ? 'Turn off privacy mode' : 'Turn on privacy mode');
  btnPrivacyMode.title = enabled
    ? 'Privacy mode on — hover a surface to reveal it'
    : 'Turn on privacy mode';
}

function setPrivacyMode(enabled, { persist = true } = {}) {
  syncPrivacyModeUi(!!enabled);
  if (!persist) return;
  if (storageReady && settings.privacyMode !== !!enabled) {
    saveSettings({ ...settings, privacyMode: !!enabled });
  }
}

function applyStoredPrivacyMode() {
  setPrivacyMode(settings.privacyMode === true, { persist: false });
}

function closeMobileSidebar() {
  if (sidebarIsMobileDrawer()) setSidebarOpen(false);
  else syncSidebarToggleUi();
}

function applyStoredSidebarCollapsed() {
  if (sidebarIsMobileDrawer()) {
    syncSidebarToggleUi();
    return;
  }
  const collapsed = settings.sidebarCollapsed === true;
  chatShell.classList.toggle('sidebar-collapsed', collapsed);
  syncSidebarToggleUi();
}

btnToggleSidebar?.addEventListener('click', () => setSidebarOpen(false));
btnExpandSidebar?.addEventListener('click', () => setSidebarOpen(true));
btnPrivacyMode?.addEventListener('click', () => {
  setPrivacyMode(!chatShell.classList.contains('privacy-mode'));
});
document.getElementById('sidebarBackdrop')?.addEventListener('click', () => {
  closeMobileSidebar();
});
sidebarMobileMq.addEventListener?.('change', () => {
  if (!sidebarIsMobileDrawer()) {
    chatShell.classList.remove('sidebar-open');
    applyStoredSidebarCollapsed();
  } else {
    chatShell.classList.remove('sidebar-collapsed');
    syncSidebarToggleUi();
  }
});
// —— Search chats / projects ——
const searchModal = document.getElementById('searchModal');
const searchModalInput = document.getElementById('searchModalInput');
const searchModalResults = document.getElementById('searchModalResults');
let searchActiveIndex = 0;
let searchResultItems = [];

function closeSearchModal() {
  if (!searchModal) return;
  closeBackdrop(searchModal);
  if (searchModalInput) searchModalInput.value = '';
  searchActiveIndex = 0;
  searchResultItems = [];
}

function openSearchModal() {
  if (!searchModal || !requireUnlockedData()) return;
  closeSettings();
  openBackdrop(searchModal);
  if (searchModalInput) {
    searchModalInput.value = '';
    queueMicrotask(() => searchModalInput.focus());
  }
  renderSearchResults('');
}

function renderSearchResults(query) {
  if (!searchModalResults) return;
  const q = String(query || '').trim().toLowerCase();
  const matchedProjects = (typeof isBotsSurface === 'function' && isBotsSurface())
    ? []
    : projects.filter((project) => {
    if (!q) return true;
    const hay = [project.name, project.instructions, project.memory]
      .filter(Boolean)
      .join('\n')
      .toLowerCase();
    return hay.includes(q);
  });
  const pool = typeof conversationsOnSurface === 'function' ? conversationsOnSurface() : conversations;
  const matchedConvos = bySidebarOrder(pool).filter((convo) => {
    if (!q) return true;
    const projectName = getProject(convo.projectId)?.name || '';
    const hay = [
      convo.title,
      projectName,
      ...(convo.messages || []).slice(0, 6).map((m) => m.content || ''),
    ]
      .join('\n')
      .toLowerCase();
    return hay.includes(q);
  });

  searchModalResults.innerHTML = '';
  searchResultItems = [];
  if (matchedProjects.length === 0 && matchedConvos.length === 0) {
    const empty = document.createElement('p');
    empty.className = 'search-dialog-empty';
    empty.textContent = q ? 'No matches.' : 'No chats or projects yet.';
    searchModalResults.appendChild(empty);
    return;
  }

  const pushItem = (kind, title, meta, onPick) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'search-dialog-item';
    btn.dataset.kind = kind;
    btn.innerHTML =
      '<span class="search-dialog-item-title"></span>' +
      '<span class="search-dialog-item-meta"></span>';
    btn.querySelector('.search-dialog-item-title').textContent = title;
    btn.querySelector('.search-dialog-item-meta').textContent = meta;
    btn.addEventListener('click', () => {
      onPick();
      closeSearchModal();
    });
    searchModalResults.appendChild(btn);
    searchResultItems.push(btn);
  };

  if (matchedProjects.length) {
    const label = document.createElement('div');
    label.className = 'search-dialog-group';
    label.textContent = 'Projects';
    searchModalResults.appendChild(label);
    for (const project of matchedProjects.slice(0, 12)) {
      const n = conversationsForProject(project.id).length;
      pushItem(
        'project',
        project.name || 'Untitled project',
        n + (n === 1 ? ' chat' : ' chats'),
        () => openProject(project.id)
      );
    }
  }
  if (matchedConvos.length) {
    const label = document.createElement('div');
    label.className = 'search-dialog-group';
    label.textContent = 'Chats';
    searchModalResults.appendChild(label);
    for (const convo of matchedConvos.slice(0, 24)) {
      const project = getProject(convo.projectId);
      const preview = (convo.messages || [])
        .find((m) => m.role === 'user')
        ?.content
        ?.replace(/\s+/g, ' ')
        .trim() || 'Empty chat';
      pushItem(
        'chat',
        convo.title || 'New chat',
        (project ? project.name + ' · ' : '') + preview.slice(0, 80),
        () => selectConversation(convo.id)
      );
    }
  }
  searchActiveIndex = 0;
  syncSearchActiveItem();
}

function syncSearchActiveItem() {
  searchResultItems.forEach((el, index) => {
    el.classList.toggle('is-active', index === searchActiveIndex);
  });
  const active = searchResultItems[searchActiveIndex];
  if (active) active.scrollIntoView({ block: 'nearest' });
}

document.getElementById('btnSearchChats')?.addEventListener('click', openSearchModal);
searchModalInput?.addEventListener('input', () => {
  renderSearchResults(searchModalInput.value);
});
searchModalInput?.addEventListener('keydown', (event) => {
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    if (!searchResultItems.length) return;
    searchActiveIndex = (searchActiveIndex + 1) % searchResultItems.length;
    syncSearchActiveItem();
  } else if (event.key === 'ArrowUp') {
    event.preventDefault();
    if (!searchResultItems.length) return;
    searchActiveIndex =
      (searchActiveIndex - 1 + searchResultItems.length) % searchResultItems.length;
    syncSearchActiveItem();
  } else if (event.key === 'Enter') {
    event.preventDefault();
    searchResultItems[searchActiveIndex]?.click();
  }
});
searchModal?.addEventListener('click', (event) => {
  if (event.target === searchModal) closeSearchModal();
});
document.getElementById('btnSettings').addEventListener('click', () => openSettings());
btnProfileMenu?.addEventListener('click', (event) => {
  event.stopPropagation();
  if (profileMenuIsOpen()) setProfileMenuOpen(false, { restoreFocus: true });
  else {
    syncAccountProfileUi();
    setProfileMenuOpen(true);
  }
});
document.getElementById('btnProfileCreate')?.addEventListener('click', () => {
  setProfileMenuOpen(false);
  openProfileModal();
});
document.getElementById('btnProfileManage')?.addEventListener('click', () => {
  setProfileMenuOpen(false);
  openSettings('profiles');
});
document.getElementById('btnSettingsProfileCreate')?.addEventListener('click', () => openProfileModal());
document.getElementById('btnProfileModalCancel')?.addEventListener('click', closeProfileModal);
document.getElementById('btnProfileModalClose')?.addEventListener('click', closeProfileModal);
document.getElementById('btnProfileModalSave')?.addEventListener('click', commitProfileModal);
document.getElementById('profileNameInput')?.addEventListener('input', (event) => {
  event.target.setCustomValidity('');
  const mark = document.getElementById('profileModalMark');
  if (mark) mark.textContent = profileInitials(event.target.value || 'Profile');
});
document.getElementById('profileNameInput')?.addEventListener('keydown', (event) => {
  if (event.key !== 'Enter') return;
  event.preventDefault();
  commitProfileModal();
});
document.getElementById('profileModal')?.addEventListener('click', (event) => {
  if (event.target === event.currentTarget) closeProfileModal();
});
encryptionIndicator?.addEventListener('click', () => {
  openSettings();
  showSettingsPane('data');
  document.getElementById('settingsEncryptionSection')?.scrollIntoView({ block: 'start' });
});
document.getElementById('btnSettingsCancel').addEventListener('click', closeSettings);
document.getElementById('btnSettingsClose').addEventListener('click', closeSettings);
document.getElementById('btnSettingsSave').addEventListener('mousedown', (event) => {
  if (event.button === 0) event.preventDefault();
});
document.getElementById('btnSettingsSave').addEventListener('click', commitSettings);
document.getElementById('settingChatBackgroundUrl')?.addEventListener('input', (event) => {
  pendingChatBackgroundImage = event.target.value.trim();
  pendingChatBackgroundImageName = '';
  syncChatBackgroundForm();
});
document.getElementById('settingChatBackgroundOverlay')?.addEventListener('input', syncChatBackgroundForm);
document.getElementById('btnChatBackgroundFile')?.addEventListener('click', () => {
  document.getElementById('settingChatBackgroundFile')?.click();
});
document.getElementById('settingChatBackgroundFile')?.addEventListener('change', (event) => {
  const file = event.target.files?.[0];
  event.target.value = '';
  if (!file) return;
  const allowed = ['image/png', 'image/jpeg', 'image/webp', 'image/gif', 'image/avif'];
  if (!allowed.includes(file.type)) {
    alert('Choose a PNG, JPEG, WebP, GIF, or AVIF image.');
    return;
  }
  if (file.size > CHAT_BACKGROUND_MAX_BYTES) {
    alert('Background images must be 1 MB or smaller.');
    return;
  }
  const reader = new FileReader();
  reader.addEventListener('load', () => {
    pendingChatBackgroundImage = typeof reader.result === 'string' ? reader.result : '';
    pendingChatBackgroundImageName = file.name;
    syncChatBackgroundForm();
    syncSettingsSaveButton();
  });
  reader.addEventListener('error', () => alert('Could not read that image.'));
  reader.readAsDataURL(file);
});
document.getElementById('btnChatBackgroundClear')?.addEventListener('click', () => {
  pendingChatBackgroundImage = '';
  pendingChatBackgroundImageName = '';
  const urlInput = document.getElementById('settingChatBackgroundUrl');
  if (urlInput) urlInput.value = '';
  syncChatBackgroundForm();
  syncSettingsSaveButton();
});
document.querySelectorAll('[data-background-position]').forEach((button) => {
  button.addEventListener('click', () => {
    const positionInput = document.getElementById('settingChatBackgroundPosition');
    if (!positionInput) return;
    positionInput.value = button.dataset.backgroundPosition;
    syncChatBackgroundForm();
    syncSettingsSaveButton();
  });
  button.addEventListener('keydown', (event) => {
    if (!['ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown'].includes(event.key)) return;
    event.preventDefault();
    const buttons = [...document.querySelectorAll('[data-background-position]')];
    const index = buttons.indexOf(button);
    const delta = event.key === 'ArrowLeft' ? -1
      : event.key === 'ArrowRight' ? 1
        : event.key === 'ArrowUp' ? -3 : 3;
    const next = buttons[Math.min(buttons.length - 1, Math.max(0, index + delta))];
    next?.click();
    next?.focus();
  });
});
settingsModal.querySelector('.settings-dialog')?.addEventListener('input', (event) => {
  if (!event.target.closest('input, textarea, select')) return;
  syncWebSearchControls();
  syncFetchUrlControls();
  syncTerminalSkillControls();
  syncAttachmentFallbackControls();
  syncSettingsSaveButton();
});
settingsModal.querySelector('.settings-dialog')?.addEventListener('change', (event) => {
  if (!event.target.closest('input, textarea, select')) return;
  syncWebSearchControls();
  syncFetchUrlControls();
  syncTerminalSkillControls();
  syncAttachmentFallbackControls();
  syncSettingsSaveButton();
});
document.getElementById('settingSkillWebSearch').addEventListener('change', syncWebSearchControls);
document.getElementById('btnWebSearchAdvanced')?.addEventListener('click', () => {
  const toggle = document.getElementById('btnWebSearchAdvanced');
  const panel = document.getElementById('webSearchOptions');
  if (!toggle || toggle.disabled) return;
  setCapabilityAdvancedOpen(toggle, panel, toggle.getAttribute('aria-expanded') !== 'true');
});
document.getElementById('btnWebSearchMore')?.addEventListener('click', () => {
  const toggle = document.getElementById('btnWebSearchMore');
  const panel = document.getElementById('webSearchMoreOptions');
  if (!toggle || toggle.disabled) return;
  setCapabilityAdvancedOpen(toggle, panel, toggle.getAttribute('aria-expanded') !== 'true');
});
document.getElementById('settingSkillFetchUrl').addEventListener('change', syncFetchUrlControls);
document.getElementById('btnFetchUrlAdvanced')?.addEventListener('click', () => {
  const toggle = document.getElementById('btnFetchUrlAdvanced');
  const panel = document.getElementById('fetchUrlOptions');
  if (!toggle || toggle.disabled) return;
  setCapabilityAdvancedOpen(toggle, panel, toggle.getAttribute('aria-expanded') !== 'true');
});
document.getElementById('settingSkillTerminal')?.addEventListener('change', syncTerminalSkillControls);
document.getElementById('btnTerminalAdvanced')?.addEventListener('click', () => {
  const toggle = document.getElementById('btnTerminalAdvanced');
  const panel = document.getElementById('terminalOptions');
  if (!toggle || toggle.disabled) return;
  setCapabilityAdvancedOpen(toggle, panel, toggle.getAttribute('aria-expanded') !== 'true');
});
document.querySelectorAll('#approvalModeToggle [data-approval-mode]').forEach((btn) => {
  btn.addEventListener('click', () => {
    document.querySelectorAll('#approvalModeToggle [data-approval-mode]').forEach((other) => {
      const on = other === btn;
      other.classList.toggle('is-active', on);
      other.setAttribute('aria-selected', on ? 'true' : 'false');
    });
    syncSettingsSaveButton();
  });
});
document.getElementById('btnSkillCreate').addEventListener('click', () => {
  openSkillEditor({
    name: '',
    description: '',
    content: '# Skill\n\nDescribe what the model should do when this skill applies.\n',
    enabled: true,
  });
});
document.getElementById('btnSkillUpload').addEventListener('click', () => {
  document.getElementById('skillFileInput').click();
});
document.getElementById('skillFileInput').addEventListener('change', async (event) => {
  const file = event.target.files && event.target.files[0];
  event.target.value = '';
  if (!file) return;
  const uploadBtn = document.getElementById('btnSkillUpload');
  await withBusyControl(uploadBtn, 'Importing…', async () => {
    try {
      showSkillsError('');
      await importSkillFile(file);
    } catch (error) {
      showSkillsError(error);
    }
  });
});
document.getElementById('btnSkillSave').addEventListener('click', () => {
  saveSkillFromEditor();
});
document.getElementById('btnSkillCancelEdit').addEventListener('click', hideSkillEditor);
document.getElementById('skillsList').addEventListener('click', async (event) => {
  const row = event.target.closest('[data-skill-id]');
  if (!row) return;
  const id = row.getAttribute('data-skill-id');
  const skill = userSkills.find((item) => item.id === id);
  if (!skill) return;
  if (event.target.closest('[data-skill-edit]')) {
    openSkillEditor(skill);
    return;
  }
  const deleteBtn = event.target.closest('[data-skill-delete]');
  if (deleteBtn) {
    if (!window.confirm('Delete skill “' + skill.name + '”?')) return;
    await withBusyControl(deleteBtn, 'Deleting…', async () => {
      try {
        const response = await fetch('/api/skills/' + encodeURIComponent(id), { method: 'DELETE' });
        const body = await response.json().catch(() => ({}));
        if (!response.ok) throw new Error(body.error || 'Could not delete skill');
        userSkills = body.skills || [];
        hideSkillEditor();
        renderUserSkills();
      } catch (error) {
        showSkillsError(error);
      }
    }, { restore: false });
  }
});
document.getElementById('skillsList').addEventListener('change', async (event) => {
  const toggle = event.target.closest('[data-skill-enabled]');
  if (!toggle) return;
  const row = event.target.closest('[data-skill-id]');
  if (!row) return;
  const id = row.getAttribute('data-skill-id');
  toggle.disabled = true;
  try {
    const response = await fetch('/api/skills/' + encodeURIComponent(id), {
      method: 'PATCH',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ enabled: !!toggle.checked }),
    });
    const body = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(body.error || 'Could not update skill');
    const index = userSkills.findIndex((item) => item.id === id);
    if (index >= 0) userSkills[index] = body;
    renderUserSkills();
  } catch (error) {
    toggle.checked = !toggle.checked;
    toggle.disabled = false;
    showSkillsError(error);
  }
});
document.getElementById('btnProjectCancel').addEventListener('click', closeProjectSettings);
document.getElementById('btnProjectClose').addEventListener('click', closeProjectSettings);
document.getElementById('btnProjectSave').addEventListener('click', commitProjectSettings);
document.getElementById('projectMemoryModeToggle')?.addEventListener('click', (event) => {
  const btn = event.target.closest('[data-memory-mode]');
  if (!btn) return;
  syncProjectMemoryModeControls(btn.getAttribute('data-memory-mode'));
});
document.getElementById('btnProjectDelete').addEventListener('click', () => {
  if (editingProjectId) deleteProject(editingProjectId);
});
document.querySelectorAll('.settings-nav-btn').forEach((btn) => {
  btn.addEventListener('click', () => showSettingsPane(btn.dataset.settingsPane));
});
document.getElementById('btnClearChats')?.addEventListener('click', clearAllChats);
document.getElementById('btnClearLoops')?.addEventListener('click', clearAllLoops);
document.getElementById('btnClearProjects')?.addEventListener('click', clearAllProjects);
document.getElementById('btnClearAllData')?.addEventListener('click', clearAllChatsAndProjects);
document.getElementById('btnOpenDataDir')?.addEventListener('click', async () => {
  const btn = document.getElementById('btnOpenDataDir');
  if (!btn || btn.disabled) return;
  btn.disabled = true;
  try {
    const response = await fetch('/api/data/open', { method: 'POST' });
    if (!response.ok) {
      const body = await response.json().catch(() => ({}));
      throw new Error(body.error || 'Could not open folder');
    }
  } catch (error) {
    alert(error.message || 'Could not open folder');
  } finally {
    refreshLocalDataPane();
  }
});
function syncEncryptionPassphraseWarning() {
  const passphraseInput = document.getElementById('encryptionPassphrase');
  const confirmInput = document.getElementById('encryptionPassphraseConfirm');
  const warning = document.getElementById('encryptionPassphraseWarning');
  if (!passphraseInput || !confirmInput || !warning) return;

  const passphrase = passphraseInput.value;
  const confirm = confirmInput.value;
  const length = Array.from(passphrase).length;
  const messages = [];
  if (length > 0 && length < 16) {
    messages.push('This passphrase is short (' + length + ' characters). 16 or more is recommended, but you can continue.');
  } else if (length > 1024) {
    messages.push('This passphrase is unusually long (' + length + ' characters). It is allowed, but may be difficult to enter reliably.');
  }
  if (confirm && passphrase !== confirm) {
    messages.push('The confirmation does not match.');
  }

  warning.textContent = messages.join(' ');
  warning.classList.toggle('is-hidden', messages.length === 0);
}

['encryptionPassphrase', 'encryptionPassphraseConfirm'].forEach((id) => {
  document.getElementById(id)?.addEventListener('input', syncEncryptionPassphraseWarning);
});
document.getElementById('btnEnableEncryption')?.addEventListener('click', async () => {
  const passphrase = document.getElementById('encryptionPassphrase')?.value || '';
  const confirm = document.getElementById('encryptionPassphraseConfirm')?.value || '';
  const btn = document.getElementById('btnEnableEncryption');
  if (btn) btn.disabled = true;
  try {
    await postEncryption('/api/data/encryption/enable', {
      passphrase,
      passphrase_confirm: confirm,
    });
    document.getElementById('encryptionPassphrase').value = '';
    document.getElementById('encryptionPassphraseConfirm').value = '';
    syncEncryptionPassphraseWarning();
  } catch (error) {
    alert(error.message || 'Could not enable encryption');
  } finally {
    if (btn) btn.disabled = false;
  }
});
document.getElementById('btnUnlockEncryption')?.addEventListener('click', async () => {
  const passphrase = document.getElementById('encryptionUnlockPassphrase')?.value || '';
  const btn = document.getElementById('btnUnlockEncryption');
  try {
    await unlockDiskEncryption(passphrase, btn);
  } catch (error) {
    alert(error.message || 'Could not unlock');
  }
});
document.getElementById('encryptionUnlockPassphrase')?.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') {
    event.preventDefault();
    document.getElementById('btnUnlockEncryption')?.click();
  }
});
function setUnlockModalLoading(loading) {
  const btn = document.getElementById('btnUnlockModalSubmit');
  if (!btn) return;
  btn.disabled = loading;
  btn.classList.toggle('is-loading', loading);
  btn.querySelector('.unlock-button-label')?.classList.toggle('is-hidden', loading);
  btn.querySelector('.button-loading-spinner')?.classList.toggle('is-hidden', !loading);
  btn.toggleAttribute('aria-busy', loading);
  btn.setAttribute('aria-label', loading ? 'Unlocking encrypted data' : 'Unlock');
}
document.getElementById('btnUnlockModalSubmit')?.addEventListener('click', async () => {
  const passphrase = document.getElementById('unlockModalPassphrase')?.value || '';
  const btn = document.getElementById('btnUnlockModalSubmit');
  setUnlockModalLoading(true);
  try {
    await unlockDiskEncryption(passphrase, btn);
  } catch (error) {
    setUnlockModalError(error.message || 'Could not unlock');
    document.getElementById('unlockModalPassphrase')?.focus();
  } finally {
    setUnlockModalLoading(false);
  }
});
document.getElementById('unlockModalPassphrase')?.addEventListener('keydown', (event) => {
  if (event.key === 'Enter') {
    event.preventDefault();
    document.getElementById('btnUnlockModalSubmit')?.click();
  }
});
document.getElementById('btnUnlockModalDismiss')?.addEventListener('click', () => {
  hideUnlockSession();
});
document.getElementById('btnLockEncryption')?.addEventListener('click', async () => {
  try {
    await postEncryption('/api/data/encryption/lock', {});
    clearMemoryAfterLock();
  } catch (error) {
    alert(error.message || 'Could not lock');
  }
});
document.getElementById('btnDisableEncryption')?.addEventListener('click', () => {
  document.getElementById('settingsEncryptionDisable')?.classList.remove('is-hidden');
});
document.getElementById('btnConfirmDisableEncryption')?.addEventListener('click', async () => {
  const passphrase = document.getElementById('encryptionDisablePassphrase')?.value || '';
  const btn = document.getElementById('btnConfirmDisableEncryption');
  if (btn) btn.disabled = true;
  try {
    await postEncryption('/api/data/encryption/disable', { passphrase });
    document.getElementById('encryptionDisablePassphrase').value = '';
    document.getElementById('settingsEncryptionDisable')?.classList.add('is-hidden');
  } catch (error) {
    alert(error.message || 'Could not disable encryption');
  } finally {
    if (btn) btn.disabled = false;
  }
});
document.querySelectorAll('#themeToggle [data-theme-choice]').forEach((btn) => {
  btn.addEventListener('click', () => setTheme(btn.dataset.themeChoice));
});
document.querySelectorAll('#fontScaleToggle [data-font-scale]').forEach((btn) => {
  btn.addEventListener('click', () => patchAppearance({ font_scale: btn.dataset.fontScale }));
});
['settingFontBody', 'settingFontDisplay'].forEach((id) => {
  const el = document.getElementById(id);
  if (!el) return;
  el.addEventListener('change', () => {
    const key = id === 'settingFontBody' ? 'font_body' : 'font_display';
    patchAppearance({ [key]: el.value });
  });
});
document.getElementById('btnAppearanceReset').addEventListener('click', resetAppearance);
settingsModal.addEventListener('click', (event) => {
  if (event.target === settingsModal) closeSettings();
});
projectModal.addEventListener('click', (event) => {
  if (event.target === projectModal) closeProjectSettings();
});
document.addEventListener('click', (event) => {
  if (profileMenuIsOpen() && !sidebarProfileMenu.contains(event.target) && !btnProfileMenu.contains(event.target)) {
    setProfileMenuOpen(false);
  }
  if (openConvoMenu && !openConvoMenu.contains(event.target) && !event.target.closest('.convo-more')) {
    closeConvoMenu();
  }
});
document.getElementById('btnConfirmModalCancel')?.addEventListener('click', () => settleConfirmDanger(false));
document.getElementById('btnConfirmModalOk')?.addEventListener('click', () => settleConfirmDanger(true));
document.getElementById('confirmModal')?.addEventListener('click', (event) => {
  if (event.target === event.currentTarget) settleConfirmDanger(false);
});
convoTitleEl.addEventListener('click', beginTopbarTitleEdit);
convoTitleEl.addEventListener('keydown', (event) => {
  if ((event.key === 'Enter' || event.key === ' ') && !convoTitleEl.querySelector('input')) {
    event.preventDefault();
    beginTopbarTitleEdit();
  }
});
document.addEventListener('keydown', (event) => {
  const key = event.key.toLowerCase();
  if ((event.ctrlKey || event.metaKey) && key === 'k') {
    event.preventDefault();
    if (searchModal && !searchModal.classList.contains('is-hidden')) closeSearchModal();
    else openSearchModal();
    return;
  }
  if ((event.ctrlKey || event.metaKey) && !event.altKey && !event.shiftKey) {
    const field = event.target;
    const textField = field instanceof HTMLTextAreaElement
      || (field instanceof HTMLInputElement
        && !['button', 'checkbox', 'color', 'file', 'hidden', 'image', 'radio', 'range', 'reset', 'submit'].includes((field.type || 'text').toLowerCase()));
    if (textField && (key === 'a' || key === 'c' || key === 'x')) {
      if (key === 'a') {
        event.preventDefault();
        field.select();
        return;
      }
      const start = field.selectionStart ?? 0;
      const end = field.selectionEnd ?? 0;
      if (end > start && navigator.clipboard?.writeText) {
        const selected = field.value.slice(start, end);
        event.preventDefault();
        void navigator.clipboard.writeText(selected).then(() => {
          if (key !== 'x') return;
          field.setRangeText('', start, end, 'start');
          field.dispatchEvent(new Event('input', { bubbles: true }));
        });
        return;
      }
    }
  }
  if (event.key === 'Escape') {
    if (profileMenuIsOpen()) {
      setProfileMenuOpen(false, { restoreFocus: true });
      return;
    }
    const profileModal = document.getElementById('profileModal');
    if (profileModal && !profileModal.classList.contains('is-hidden')) {
      closeProfileModal();
      return;
    }
    if (voiceListening) {
      stopVoiceInput();
      return;
    }
    if (searchModal && !searchModal.classList.contains('is-hidden')) {
      closeSearchModal();
      return;
    }
    const confirmModal = document.getElementById('confirmModal');
    if (confirmModal && !confirmModal.classList.contains('is-hidden')) {
      settleConfirmDanger(false);
      return;
    }
    const unlockModal = document.getElementById('unlockModal');
    if (unlockModal && !unlockModal.classList.contains('is-hidden')) {
      hideUnlockSession();
      return;
    }
    if (!projectModal.classList.contains('is-hidden')) {
      closeProjectSettings();
      return;
    }
    if (!settingsModal.classList.contains('is-hidden')) {
      closeSettings();
      return;
    }
    if (!chatModelMenu.classList.contains('is-hidden')) {
      closeModelMenu({ restoreFocus: true });
      return;
    }
    if (wordmarkMenuIsOpen()) {
      setWordmarkMenuOpen(false);
      document.getElementById('btnWordmarkSurface')?.focus();
      return;
    }
    closeConvoMenu();
  }
});

chatModelSelectWrap.addEventListener('click', (event) => {
  if (event.target.closest('.chat-model-menu')) return;
  event.preventDefault();
  event.stopPropagation();
  if (modelMenuIsOpen()) {
    closeModelMenu({ restoreFocus: true });
    return;
  }
  openModelMenu();
  // openModelMenu() puts focus in the filter field when it is shown;
  // don't yank it back to the trigger.
  if (!modelSearchEnabled()) chatModelSelect.focus();
});

/** Shared arrow/Enter/Escape handling for the trigger and the filter field. */
function handleModelMenuKeydown(event) {
  const open = modelMenuIsOpen();
  if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
    event.preventDefault();
    if (!open) {
      openModelMenu();
      return;
    }
    const count = visibleModelMenuOptions().length;
    if (!count) return;
    const delta = event.key === 'ArrowDown' ? 1 : -1;
    const from = modelMenuActiveIndex < 0 ? (delta > 0 ? -1 : 0) : modelMenuActiveIndex;
    modelMenuActiveIndex = (from + delta + count) % count;
    paintModelMenuActive();
  } else if (event.key === 'Home' || event.key === 'End') {
    if (!open || !visibleModelMenuOptions().length) return;
    event.preventDefault();
    modelMenuActiveIndex = event.key === 'Home' ? 0 : visibleModelMenuOptions().length - 1;
    paintModelMenuActive();
  } else if (event.key === 'Enter') {
    event.preventDefault();
    if (!open) {
      openModelMenu();
      return;
    }
    const choice = visibleModelMenuOptions()[modelMenuActiveIndex];
    if (choice) chooseModelOption(choice.value);
  } else if (event.key === ' ' && event.target === modelMenuTriggerEl()) {
    // Space only toggles from the button; inside the field it is a query character.
    event.preventDefault();
    if (!open) openModelMenu();
  } else if (event.key === 'Escape' && open) {
    event.preventDefault();
    event.stopPropagation();
    closeModelMenu({ restoreFocus: true });
  } else if (event.key === 'Tab' && open) {
    closeModelMenu();
  }
}

chatModelSelect.addEventListener('keydown', handleModelMenuKeydown);
chatModelSearch.addEventListener('keydown', handleModelMenuKeydown);
chatModelSearch.addEventListener('input', () => {
  modelMenuFilter = chatModelSearch.value;
  applyModelFilter();
});
// Clicking the field must not fall through to the wrap's toggle handler.
chatModelSearchWrap.addEventListener('click', (event) => {
  event.stopPropagation();
  chatModelSearch.focus();
});
chatModelMenu.addEventListener('click', (event) => {
  const tab = event.target.closest('[data-model-tab]');
  if (tab) {
    event.preventDefault();
    event.stopPropagation();
    setModelMenuTab(tab.getAttribute('data-model-tab'));
    return;
  }
  const group = event.target.closest('[data-model-group]');
  if (group) {
    event.preventDefault();
    event.stopPropagation();
    toggleModelProviderCollapsed(group.getAttribute('data-model-group') || '');
    return;
  }
  if (event.target.closest('[data-model-pin], [data-model-pick], .chat-model-option')) {
    event.preventDefault();
    event.stopPropagation();
  }
});
// pointerdown so selection wins before the document "outside click" closer.
chatModelMenu.addEventListener('pointerdown', (event) => {
  // Let the filter field take the caret normally.
  if (event.target.closest('.chat-model-search')) return;
  if (event.target.closest('[data-model-tab]')) return;
  if (event.target.closest('[data-model-group]')) return;
  const pin = event.target.closest('[data-model-pin]');
  if (pin) {
    event.preventDefault();
    event.stopPropagation();
    togglePinnedModel(pin.getAttribute('data-model-pin') || '');
    return;
  }
  const pick = event.target.closest('[data-model-pick]');
  const row = event.target.closest('.chat-model-option');
  const value = pick?.getAttribute('data-model-pick')
    || row?.getAttribute('data-value')
    || '';
  if (!value) return;
  event.preventDefault();
  event.stopPropagation();
  chooseModelOption(value);
});
document.addEventListener('click', (event) => {
  if (!modelMenuIsOpen()) return;
  if (chatModelSelectWrap.contains(event.target)) return;
  if (modelMenuAnchorEl()?.contains(event.target)) return;
  if (chatModelMenu.contains(event.target)) return;
  closeModelMenu();
});
function repositionOpenModelMenu() {
  if (modelMenuIsOpen()) positionModelMenu();
}
window.addEventListener('resize', repositionOpenModelMenu);
document.addEventListener('scroll', repositionOpenModelMenu, true);
if (window.visualViewport) {
  window.visualViewport.addEventListener('resize', repositionOpenModelMenu);
  window.visualViewport.addEventListener('scroll', repositionOpenModelMenu);
}
composerInput.addEventListener('input', () => {
  if (voiceListening) stopVoiceInput({ silent: true });
  mentionInput = composerInput;
  promoteTypedMentions();
  autoResize(composerInput);
  updateSendEnabled();
  updateMentionMenu(composerInput);
});
composerInput.addEventListener('click', () => updateMentionMenu(composerInput));
composerInput.addEventListener('keyup', (event) => {
  if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
    updateMentionMenu(composerInput);
  }
});
composerInput.addEventListener('blur', () => {
  // Delay so mousedown on a menu option can fire first.
  setTimeout(() => {
    if (document.activeElement !== composerInput) closeMentionMenu();
  }, 120);
});
composerInput.addEventListener('keydown', (event) => {
  mentionInput = composerInput;
  if (handleMentionKeydown(event)) return;

  if (event.key !== 'Enter') return;
  if (settings.enterSends) {
    if (!event.shiftKey) {
      event.preventDefault();
      if (!btnSend.disabled) sendMessage();
    }
  } else if ((event.ctrlKey || event.metaKey) && !event.shiftKey) {
    event.preventDefault();
    if (!btnSend.disabled) sendMessage();
  }
});

function mountStarfield(canvas) {
  if (!canvas) return;
  const host = canvas.parentElement;
  if (!host) return;

  function rand(seed) {
    let t = seed + 0x6d2b79f5;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  }

  function paint() {
    const width = host.clientWidth || 1;
    const height = host.clientHeight || 1;
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.max(1, Math.floor(width * dpr));
    canvas.height = Math.max(1, Math.floor(height * dpr));
    const ctx = canvas.getContext('2d');
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.clearRect(0, 0, width, height);

    // Dense field of tiny dim dots; seeded so resize doesn't reshuffle wildly.
    // Light mode keeps the field but uses dark ink so it reads on pale paper.
    const light = document.documentElement.dataset.theme === 'light';
    const ink = light ? '18, 22, 32' : '245, 248, 255';
    // Keep stars clear of the model selector so they don't look like UI glitches.
    const clearZones = [];
    const modelWrap = document.getElementById('chatModelSelectWrap');
    if (modelWrap && !modelWrap.classList.contains('is-hidden')) {
      const hostRect = host.getBoundingClientRect();
      const rect = modelWrap.getBoundingClientRect();
      const pad = 16;
      clearZones.push({
        left: rect.left - hostRect.left - pad,
        top: rect.top - hostRect.top - pad,
        right: rect.right - hostRect.left + pad,
        bottom: rect.bottom - hostRect.top + pad,
      });
    }
    const inClearZone = (x, y) => clearZones.some((zone) =>
      x >= zone.left && x <= zone.right && y >= zone.top && y <= zone.bottom
    );
    // Sparser and dimmer than a real starfield — this is texture on the
    // canvas, not a subject. The host's CSS mask fades it out before the
    // thread column, so density only has to read in the upper area.
    const count = Math.round((width * height) / 4200);
    for (let i = 0; i < count; i++) {
      const x = rand(i * 1289 + 17) * width;
      const y = rand(i * 2657 + 91) * height;
      if (inClearZone(x, y)) continue;
      const size = rand(i * 409 + 3) < 0.86 ? 0.5 : 0.85;
      const alpha = light
        ? 0.11 + rand(i * 733 + 11) * 0.18
        : 0.09 + rand(i * 733 + 11) * 0.2;
      ctx.beginPath();
      ctx.fillStyle = 'rgba(' + ink + ', ' + alpha.toFixed(3) + ')';
      ctx.arc(x, y, size, 0, Math.PI * 2);
      ctx.fill();
    }
  }

  repaintStarfield = paint;
  paint();
  if (typeof ResizeObserver !== 'undefined') {
    const observer = new ResizeObserver(paint);
    observer.observe(host);
  } else {
    window.addEventListener('resize', paint);
  }
}

const emptyStateOrb = mountOrb(document.getElementById('orbMark'), 64, {
  ink: () => {
    const tone = chatMain.dataset.backgroundTone;
    if (tone === 'dark') return 'bright';
    if (tone === 'light') return 'dark';
    return 'auto';
  },
});
repaintEmptyOrb = () => emptyStateOrb.paint();
mountStarfield(document.getElementById('starfieldCanvas'));

const updateToast = document.getElementById('updateToast');
const updateToastTitle = document.getElementById('updateToastTitle');
const updateToastBody = document.getElementById('updateToastBody');
const btnUpdateView = document.getElementById('btnUpdateView');
const btnUpdateLater = document.getElementById('btnUpdateLater');
const btnUpdateDismiss = document.getElementById('btnUpdateDismiss');

function dismissedUpdateVersion() {
  return String(settings.updateDismissed || '');
}

function dismissUpdateNotice(version) {
  const tag = String(version || '').trim();
  if (tag && settings.updateDismissed !== tag) {
    saveSettings({ ...settings, updateDismissed: tag });
  }
  hideUpdateToast();
}

function hideUpdateToast() {
  if (!updateToast) return;
  updateToast.classList.remove('is-visible');
  window.setTimeout(() => {
    if (!updateToast.classList.contains('is-visible')) {
      updateToast.classList.add('is-hidden');
      updateToast.hidden = true;
    }
  }, prefersReducedMotion() ? 0 : 220);
}

function showUpdateToast(status) {
  if (!updateToast || !status || !status.update_available || !status.latest) return;
  if (dismissedUpdateVersion() === String(status.latest)) return;
  const latestLabel = 'v' + String(status.latest).replace(/^v/i, '');
  const currentLabel = 'v' + String(status.current || '').replace(/^v/i, '');
  if (updateToastTitle) {
    updateToastTitle.textContent = status.release_name
      ? String(status.release_name)
      : ('TensorMI Harness ' + latestLabel);
  }
  if (updateToastBody) {
    updateToastBody.textContent =
      'You’re on ' + currentLabel + '. ' + latestLabel + ' is available on GitHub.';
  }
  if (btnUpdateView && status.release_url) {
    btnUpdateView.href = status.release_url;
  }
  updateToast.dataset.latest = String(status.latest);
  updateToast.hidden = false;
  updateToast.classList.remove('is-hidden');
  requestAnimationFrame(() => updateToast.classList.add('is-visible'));
}

async function checkForAppUpdate({ force = false } = {}) {
  try {
    const response = await fetch('/api/updates/check' + (force ? '?force=1' : ''));
    if (!response.ok) return null;
    const status = await response.json();
    if (status && status.update_available) showUpdateToast(status);
    return status;
  } catch {
    return null;
  }
}

btnUpdateLater?.addEventListener('click', () => {
  dismissUpdateNotice(updateToast?.dataset?.latest);
});
btnUpdateDismiss?.addEventListener('click', () => {
  dismissUpdateNotice(updateToast?.dataset?.latest);
});

(async () => {
  await initLocalData();
  applyStoredSidebarCollapsed();
  document.documentElement.classList.add('ui-ready');
  applyStoredPrivacyMode();
  applyChatBackground(settings);
  updateGreeting();
  syncAgentButton();
  syncResearchControls();
  renderSidebar();
  await pollState();
  applyLocationRoute();
  const startupUrl = new URL(window.location.href);
  if (startupUrl.searchParams.get('settings') === 'providers' && !diskEncryptionLocked()) {
    openSettings('providers');
    startupUrl.searchParams.delete('settings');
    history.replaceState(
      { tensorui: 1 },
      '',
      startupUrl.pathname + startupUrl.search + startupUrl.hash
    );
  }
  // Route rendering focuses the composer. When encrypted data is locked, put
  // focus back in the blocking unlock field after that startup work completes.
  if (diskEncryptionLocked()) focusUnlockPassphrase();
  window.addEventListener('popstate', () => {
    applyLocationRoute();
  });
  setInterval(pollState, 2000);
  loadUserSkills();
  // Soft update check after the UI settles; GitHub answers are cached server-side.
  window.setTimeout(() => { checkForAppUpdate(); }, 2500);
  setInterval(() => { checkForAppUpdate(); }, 12 * 60 * 60 * 1000);
})();
