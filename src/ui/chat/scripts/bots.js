const BOT_HANDLE_RE = /^[a-z][a-z0-9_]{1,31}$/;
const BOT_MENTION_RE = /@([a-z][a-z0-9_]{1,31}|everyone|user)\b/gi;
const BOT_COMPACT_CHARS = 28000;
const BOT_COMPACT_KEEP = 10;
const BOT_GROUP_MAX_HOPS = 8;
/** Bumped on Stop so in-flight group hop loops exit instead of spawning more turns. */
const botsOutboundEpoch = new Map();
/** Refcount: a group stays busy between hops, not only while a stream is live. */
const botsOutboundActive = new Map();

function isBotsOutboundActive(convoId) {
  return !!convoId && (botsOutboundActive.get(convoId) || 0) > 0;
}

function markBotsOutboundActive(convoId) {
  if (!convoId) return;
  botsOutboundActive.set(convoId, (botsOutboundActive.get(convoId) || 0) + 1);
  if (typeof syncComposerStreamUi === 'function') syncComposerStreamUi();
}

function clearBotsOutboundActive(convoId) {
  if (!convoId) return;
  const next = (botsOutboundActive.get(convoId) || 0) - 1;
  if (next <= 0) botsOutboundActive.delete(convoId);
  else botsOutboundActive.set(convoId, next);
  if (typeof syncComposerStreamUi === 'function') syncComposerStreamUi();
}

function bumpBotsOutboundEpoch(convoId) {
  if (!convoId) return 0;
  const next = (botsOutboundEpoch.get(convoId) || 0) + 1;
  botsOutboundEpoch.set(convoId, next);
  return next;
}

function botsOutboundEpochOf(convoId) {
  return botsOutboundEpoch.get(convoId) || 0;
}

function stopAllBotsOutbound() {
  for (const id of [...botsOutboundActive.keys()]) {
    if (typeof markBotsOutboundStopped === 'function') markBotsOutboundStopped(id);
    bumpBotsOutboundEpoch(id);
  }
}

/** True when the model used the silent skip token (not a real reply). */
function isSilentNoReply(text) {
  let raw = String(text || '');
  if (!raw.trim()) return false;
  raw = raw.replace(/<(?:think|thinking)>[\s\S]*?<\/(?:think|thinking)>/gi, ' ');
  raw = raw.replace(/<\/?(?:think|thinking)>/gi, ' ');
  raw = raw.replace(/<[^>]+>/g, ' ');
  raw = raw.replace(/[*_`~]+/g, '');
  raw = raw.replace(/^\s*[-*•>]+\s*/gm, '');
  raw = raw.replace(/["'“”‘’]/g, '');
  raw = raw.replace(/\s+/g, ' ').trim();
  return /^NO[_ -]?REPLY\.?$/i.test(raw);
}

function isBotsSurface() {
  return appSurface === 'bots';
}

function convoSurfaceOf(convo) {
  return convo && convo.surface === 'bots' ? 'bots' : 'chat';
}

function isBotsConvo(convo) {
  return convoSurfaceOf(convo) === 'bots';
}

function isBotGroup(convo) {
  return !!(convo && convo.surface === 'bots' && convo.botKind === 'group');
}

function conversationsOnSurface(surface) {
  const want = surface || appSurface;
  return conversations.filter((convo) => convoSurfaceOf(convo) === want);
}

function getBot(id) {
  return bots.find((bot) => bot.id === id) || null;
}

function botByHandle(handle) {
  const key = String(handle || '').replace(/^@/, '').trim().toLowerCase();
  return bots.find((bot) => bot.handle === key) || null;
}

function normalizeHandle(raw) {
  return String(raw || '')
    .trim()
    .replace(/^@+/, '')
    .toLowerCase()
    .replace(/[^a-z0-9_]+/g, '_')
    .replace(/^_+|_+$/g, '')
    .slice(0, 32);
}

function normalizeBot(bot) {
  const handle = normalizeHandle(bot && bot.handle);
  const safeHandle = BOT_HANDLE_RE.test(handle)
    ? handle
    : ('bot_' + String((bot && bot.id) || 'x').replace(/[^a-z0-9]/gi, '').slice(0, 8).toLowerCase());
  const avatarSeed = typeof bot?.avatarSeed === 'string' && bot.avatarSeed.trim()
    ? bot.avatarSeed.trim().slice(0, 64)
    : (safeHandle || 'bot');
  return {
    id: (bot && bot.id) || newId('b'),
    handle: safeHandle || 'bot',
    name: typeof bot?.name === 'string' && bot.name.trim() ? bot.name.trim() : ('@' + (safeHandle || 'bot')),
    description: typeof bot?.description === 'string' ? bot.description : '',
    memory: typeof bot?.memory === 'string' ? bot.memory : '',
    avatarSeed,
    sessionId: typeof bot?.sessionId === 'string' ? bot.sessionId : null,
    createdAt: typeof bot?.createdAt === 'number' ? bot.createdAt : Date.now(),
    updatedAt: typeof bot?.updatedAt === 'number' ? bot.updatedAt : Date.now(),
  };
}

function hashAvatarSeed(seed) {
  let h = 2166136261;
  const text = String(seed || 'bot');
  for (let i = 0; i < text.length; i += 1) {
    h ^= text.charCodeAt(i);
    h = Math.imul(h, 16777619);
  }
  return h >>> 0;
}

/** Curated hues — varied, readable on light/dark paper. */
const BOT_AVATAR_HUES = [18, 42, 72, 145, 175, 198, 222, 248, 312, 348];

function botAvatarSeed(botOrSeed) {
  if (typeof botOrSeed === 'string') return botOrSeed.trim() || 'bot';
  if (botOrSeed && typeof botOrSeed.avatarSeed === 'string' && botOrSeed.avatarSeed.trim()) {
    return botOrSeed.avatarSeed.trim();
  }
  if (botOrSeed && botOrSeed.handle) return String(botOrSeed.handle);
  return 'bot';
}

function buildBotAvatarSvg(seedInput) {
  const seed = botAvatarSeed(seedInput);
  const h = hashAvatarSeed(seed);
  const hue = BOT_AVATAR_HUES[h % BOT_AVATAR_HUES.length];
  const hue2 = BOT_AVATAR_HUES[(h >>> 5) % BOT_AVATAR_HUES.length];
  const bg = `oklch(0.78 0.11 ${hue})`;
  const mid = `oklch(0.62 0.14 ${hue2})`;
  const ink = `oklch(0.28 0.06 ${hue})`;
  const letter = (seed.replace(/^@/, '').charAt(0) || '?').toUpperCase();
  const cx = 18 + (h % 28);
  const cy = 14 + ((h >>> 8) % 24);
  const r = 10 + ((h >>> 12) % 14);
  const x2 = 8 + ((h >>> 16) % 36);
  const y2 = 30 + ((h >>> 20) % 18);
  const r2 = 6 + ((h >>> 24) % 12);
  const rot = (h >>> 3) % 360;
  return (
    '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" role="img" aria-hidden="true">' +
      '<rect width="64" height="64" rx="32" fill="' + bg + '"/>' +
      '<g opacity="0.88">' +
        '<circle cx="' + cx + '" cy="' + cy + '" r="' + r + '" fill="' + mid + '"/>' +
        '<circle cx="' + x2 + '" cy="' + y2 + '" r="' + r2 + '" fill="' + ink + '" opacity="0.22"/>' +
        '<rect x="28" y="8" width="22" height="22" rx="8" fill="' + ink + '" opacity="0.16" transform="rotate(' + rot + ' 39 19)"/>' +
      '</g>' +
      '<text x="32" y="38" text-anchor="middle" font-family="ui-sans-serif, system-ui, sans-serif" font-size="26" font-weight="700" fill="' + ink + '">' +
        letter +
      '</text>' +
    '</svg>'
  );
}

function createBotAvatarEl(botOrSeed, { className = 'convo-avatar' } = {}) {
  const el = document.createElement('span');
  el.className = className;
  el.setAttribute('aria-hidden', 'true');
  el.innerHTML = buildBotAvatarSvg(botOrSeed);
  return el;
}

function createConvoAvatarEl(convo) {
  if (!isBotsConvo(convo)) return null;
  if (isBotGroup(convo)) {
    const members = participantBots(convo).slice(0, 2);
    const stack = document.createElement('span');
    stack.className = 'convo-avatar convo-avatar-stack';
    stack.setAttribute('aria-hidden', 'true');
    if (!members.length) {
      stack.appendChild(createBotAvatarEl(convo.title || convo.id || 'group', { className: 'convo-avatar-piece' }));
    } else {
      members.forEach((bot) => {
        stack.appendChild(createBotAvatarEl(bot, { className: 'convo-avatar-piece' }));
      });
    }
    return stack;
  }
  const bot = convo.botId ? getBot(convo.botId) : null;
  return createBotAvatarEl(bot || { handle: 'bot' });
}

function participantBots(convo) {
  if (!isBotsConvo(convo)) return [];
  if (convo.botKind === 'dm' && convo.botId) {
    const bot = getBot(convo.botId);
    return bot ? [bot] : [];
  }
  return (Array.isArray(convo.participantBotIds) ? convo.participantBotIds : [])
    .map(getBot)
    .filter(Boolean);
}

function botDisplayHandle(bot) {
  return bot ? '@' + bot.handle : '@bot';
}

function parseBotMentions(raw) {
  const result = { everyone: false, user: false, handles: [], botIds: [] };
  const seen = new Set();
  String(raw || '').replace(BOT_MENTION_RE, (_, token) => {
    const key = String(token).toLowerCase();
    if (key === 'everyone') result.everyone = true;
    else if (key === 'user') result.user = true;
    else if (!seen.has(key)) {
      seen.add(key);
      result.handles.push(key);
      const bot = botByHandle(key);
      if (bot) result.botIds.push(bot.id);
    }
    return _;
  });
  return result;
}

function persistBotMemory(botId, memoryText) {
  const bot = getBot(botId);
  if (!bot) return false;
  const next = String(memoryText || '');
  if ((bot.memory || '') === next) return false;
  bot.memory = next;
  bot.updatedAt = Date.now();
  saveStore();
  return true;
}

function persistGroupMemory(convo, memoryText) {
  if (!convo) return false;
  const next = String(memoryText || '');
  if ((convo.groupMemory || '') === next) return false;
  convo.groupMemory = next;
  convo.updatedAt = Date.now();
  saveStore();
  return true;
}

function botSystemPromptParts(convo, speakerBot) {
  const P = window.TENSORUI_PROMPTS || {};
  const fill = window.fillPrompt || ((t) => t);
  if (!speakerBot) return [];
  const description = String(speakerBot.description || '').trim() || '(No description provided.)';
  const parts = [
    fill(P['chat.botIdentity'] || 'You are @{{handle}}. Purpose:\n{{description}}', {
      handle: speakerBot.handle,
      description,
    }),
    fill(P['chat.botMemory'] || 'Private memory:\n{{memory}}', {
      memory: String(speakerBot.memory || '').trim() || '(empty)',
    }),
  ];
  if (isBotGroup(convo)) {
    const names = ['@user'].concat(participantBots(convo).map(botDisplayHandle));
    parts.push(fill(P['chat.botGroup'] || 'Group chat with {{participants}}. Speak only as @{{handle}}.', {
      participants: names.join(', '),
      handle: speakerBot.handle,
    }));
    parts.push(fill(P['chat.botGroupMemory'] || 'Shared group notes:\n{{memory}}', {
      memory: String(convo.groupMemory || '').trim() || '(empty)',
    }));
    if (convo.botsHeldBy) {
      const holder = getBot(convo.botsHeldBy);
      parts.push(fill(P['chat.botHold'] || 'Room is held by @{{handle}}. Specialists: NO_REPLY unless @user pings you.', {
        handle: holder ? holder.handle : 'holder',
      }));
    }
  } else if (convo.botKind === 'dm') {
    const group = findLinkedGroup(convo);
    const groupNote = group
      ? ('Linked group: ' + (group.title || 'group') + ' (' + participantBots(group).map(botDisplayHandle).join(', ') + '). You can <bot_group_post> back there.')
      : 'No linked group. <bot_group_post> is unavailable unless you opened this DM from a group.';
    parts.push(fill(P['chat.botDm'] || 'Private DM with @user. {{group_note}} Speak as @{{handle}}.', {
      handle: speakerBot.handle,
      group_note: groupNote,
    }));
  }
  return parts;
}

function labeledBotContent(message, speakerBot) {
  const text = typeof message.content === 'string' ? message.content : '';
  if (message.compact) return '[Compacted earlier conversation]\n' + text;
  if (message.role === 'user') return isBotGroup(conversations.find((c) => (c.messages || []).includes(message))) ? ('@user: ' + text) : text;
  if (speakerBot && message.speakerId === speakerBot.id) return text;
  const handle = message.speakerHandle || getBot(message.speakerId)?.handle || 'bot';
  return '@' + handle + ': ' + text;
}

function botApiMessages(convo, speakerBot, fallbackUserText) {
  const group = isBotGroup(convo);
  const rows = [];
  (convo.messages || []).forEach((message) => {
    if (!message || (message.role !== 'user' && message.role !== 'assistant')) return;
    let content = String(message.content || '');
    if (message.compact) content = '[Compacted earlier conversation]\n' + content;
    else if (group) {
      if (message.role === 'user') content = '@user: ' + content;
      else if (!(speakerBot && message.speakerId === speakerBot.id)) {
        content = '@' + (message.speakerHandle || getBot(message.speakerId)?.handle || 'bot') + ': ' + content;
      }
    }
    if (!content.trim()) return;
    const mine = message.role === 'assistant' && speakerBot && message.speakerId === speakerBot.id;
    rows.push({ role: mine ? 'assistant' : 'user', content });
  });
  const lastStored = (convo.messages || []).filter((message) => (
    message && (message.role === 'user' || message.role === 'assistant')
  )).pop();
  if (fallbackUserText && lastStored && lastStored.role === 'user' && rows.length && rows[rows.length - 1].role === 'user') {
    rows[rows.length - 1].content = group ? ('@user: ' + fallbackUserText) : fallbackUserText;
  }
  return rows;
}

function estimateConvoChars(convo) {
  return (convo.messages || []).reduce((sum, message) => sum + String(message.content || '').length, 0);
}

async function collectCompletionText(messages) {
  const remote = selectedRemoteModel(latestState);
  const body = { messages, agent: false, skills: {}, force_tools: [] };
  if (remote) {
    body.remote_base = remote.base;
    body.model = remote.model;
  }
  const response = await fetch('/api/chat/completions', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
  });
  if (!response.ok) throw new Error('Compact request failed');
  if (!response.body) return '';
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '';
  let text = '';
  for (;;) {
    const { value, done } = await reader.read();
    if (done) break;
    buffer += decoder.decode(value, { stream: true });
    const chunks = buffer.split(/\r?\n\r?\n/);
    buffer = chunks.pop() ?? '';
    for (const raw of chunks) {
      const parsed = parseSseEvent(raw);
      if (!parsed || parsed.data === '' || parsed.data === '[DONE]') continue;
      if (parsed.event === 'error') throw new Error(parsed.data);
      try {
        const json = JSON.parse(parsed.data);
        const delta = json.choices && json.choices[0] && json.choices[0].delta && json.choices[0].delta.content;
        if (typeof delta === 'string') text += delta;
      } catch { /* ignore */ }
    }
  }
  return text.trim();
}

function parseCompactJson(raw) {
  const source = String(raw || '').trim();
  const start = source.indexOf('{');
  const end = source.lastIndexOf('}');
  if (start < 0 || end <= start) return null;
  try {
    const parsed = JSON.parse(source.slice(start, end + 1));
    return {
      summary: String(parsed.summary || '').trim(),
      memory: String(parsed.memory || '').trim(),
    };
  } catch {
    return null;
  }
}

async function maybeCompactBotsConvo(convo, speakerBot) {
  if (!convo || convo.incognito || estimateConvoChars(convo) < BOT_COMPACT_CHARS) return;
  if ((convo.messages || []).length <= BOT_COMPACT_KEEP + 2) return;
  const dropCount = convo.messages.length - BOT_COMPACT_KEEP;
  const dropped = convo.messages.slice(0, dropCount);
  const kept = convo.messages.slice(dropCount);
  const transcript = dropped.map((message) => {
    const who = message.role === 'user'
      ? '@user'
      : ('@' + (message.speakerHandle || getBot(message.speakerId)?.handle || 'bot'));
    return who + ': ' + String(message.content || '');
  }).join('\n\n');
  const P = window.TENSORUI_PROMPTS || {};
  const fill = window.fillPrompt || ((t) => t);
  const group = isBotGroup(convo);
  const template = group
    ? (P['chat.botCompactGroup'] || 'Return JSON {"summary","memory"}. Current notes:\n{{memory}}')
    : (P['chat.botCompact'] || 'Return JSON {"summary","memory"}. Current memory:\n{{memory}}');
  try {
    const raw = await collectCompletionText([
      { role: 'system', content: fill(template, {
        memory: group
          ? (String(convo.groupMemory || '').trim() || '(empty)')
          : (String(speakerBot?.memory || '').trim() || '(empty)'),
      }) },
      { role: 'user', content: transcript.slice(0, 24000) },
    ]);
    const parsed = parseCompactJson(raw);
    if (!parsed || !parsed.summary) return;
    convo.messages = [{
      role: 'assistant',
      content: parsed.summary,
      compact: true,
      speakerId: speakerBot ? speakerBot.id : null,
      speakerHandle: speakerBot ? speakerBot.handle : null,
    }].concat(kept);
    if (group) persistGroupMemory(convo, parsed.memory || convo.groupMemory || '');
    else if (speakerBot) persistBotMemory(speakerBot.id, parsed.memory || speakerBot.memory || '');
    convo.updatedAt = Date.now();
    saveStore({ immediate: true });
  } catch (error) {
    console.warn('Bot compact failed:', error?.message || error);
  }
}

function botsToPing(convo, userText, { fromBotId = null, replyToSpeakerId = null } = {}) {
  const members = participantBots(convo);
  if (!members.length) return [];
  const pings = parseBotMentions(userText);
  if (!isBotGroup(convo)) return members.slice(0, 1);
  if (replyToSpeakerId && !fromBotId && members.some((bot) => bot.id === replyToSpeakerId)) {
    const target = getBot(replyToSpeakerId);
    return target ? [target] : [];
  }
  const heldBy = convo.botsHeldBy || null;
  if (heldBy && fromBotId) return [];
  let ids;
  if (heldBy && !fromBotId) {
    if (pings.everyone) ids = members.map((bot) => bot.id);
    else if (pings.botIds.length) {
      ids = pings.botIds.filter((id) => members.some((bot) => bot.id === id));
    } else {
      ids = [heldBy];
    }
  } else if (pings.everyone || (!pings.botIds.length && !fromBotId)) {
    ids = members.map((bot) => bot.id);
  } else {
    ids = pings.botIds.filter((id) => members.some((bot) => bot.id === id));
  }
  if (fromBotId) ids = ids.filter((id) => id !== fromBotId);
  return ids.map(getBot).filter(Boolean);
}

function findLinkedGroup(convo) {
  if (!convo) return null;
  if (convo.sideThreadOf) {
    return conversations.find((item) => item.id === convo.sideThreadOf) || null;
  }
  if (!convo.botId) return null;
  return conversations
    .filter((item) => isBotGroup(item) && (item.participantBotIds || []).includes(convo.botId))
    .sort((a, b) => (b.updatedAt || 0) - (a.updatedAt || 0))[0] || null;
}

function ensureBotDm(bot, { sideThreadOf = null } = {}) {
  if (!bot) return null;
  let dm = conversations.find((item) => (
    isBotsConvo(item) && item.botKind === 'dm' && item.botId === bot.id
  ));
  if (!dm) {
    dm = createBotSession(bot);
    bot.sessionId = dm.id;
  }
  if (sideThreadOf) dm.sideThreadOf = sideThreadOf;
  return dm;
}

function appendBotSpokenMessage(convo, speakerBot, text) {
  const message = {
    role: 'assistant',
    content: String(text || '').trim(),
    speakerId: speakerBot.id,
    speakerHandle: speakerBot.handle,
  };
  convo.messages.push(message);
  convo.updatedAt = Date.now();
  return message;
}

let pendingBotNavigation = null;

function requestBotNavigation(convoId) {
  if (convoId) pendingBotNavigation = convoId;
}

function flushBotNavigation() {
  const id = pendingBotNavigation;
  pendingBotNavigation = null;
  if (!id || id === activeId) return;
  selectConversation(id);
}

function persistGroupHold(convo, botId) {
  if (!convo) return;
  const next = botId || null;
  if ((convo.botsHeldBy || null) === next) return;
  convo.botsHeldBy = next;
  convo.updatedAt = Date.now();
  saveStore();
  if (activeId === convo.id) {
    if (typeof syncProjectChrome === 'function') syncProjectChrome();
    if (typeof updateComposerHint === 'function') updateComposerHint();
  }
}

function defaultBotsTurn() {
  return {
    useAgent: !!settings.agentMode,
    skills: {
      web_search: !!settings.skillWebSearch,
      web_search_provider: WEB_SEARCH_PROVIDERS.includes(settings.webSearchProvider)
        ? settings.webSearchProvider
        : 'auto',
      web_search_searxng: typeof settings.webSearchSearxng === 'string'
        ? settings.webSearchSearxng.trim()
        : '',
      web_search_parallel_api_key: typeof settings.webSearchParallelApiKey === 'string'
        ? settings.webSearchParallelApiKey.trim()
        : '',
      web_search_parallel_mode: WEB_SEARCH_PARALLEL_MODES.includes(settings.webSearchParallelMode)
        ? settings.webSearchParallelMode
        : 'fast',
      web_search_tinyfish_api_key: typeof settings.webSearchTinyfishApiKey === 'string'
        ? settings.webSearchTinyfishApiKey.trim()
        : '',
      fetch_url: !!settings.skillFetchUrl,
      approval_mode: settings.approvalMode === 'auto_safe' ? 'auto_safe' : 'manual',
      filesystem: !!settings.skillFilesystem,
      workspace_root: sessionWorkspaceRoot(),
      terminal: !!settings.skillTerminal,
      terminal_timeout_secs: Math.min(120, Math.max(5, Number(settings.terminalTimeoutSecs) || 30)),
      browser: !!settings.skillBrowser,
    },
    deepResearch: false,
    deepResearchOutput: 'long',
    forceTools: [],
  };
}

function applyBotActions(convo, speakerBot, actions) {
  if (!convo || !speakerBot || !actions) return;
  const hasAction = actions.hold || actions.resume || actions.dmUser || actions.groupPost;
  if (!hasAction) return;

  if (isBotGroup(convo)) {
    if (actions.hold) persistGroupHold(convo, speakerBot.id);
    if (actions.resume) persistGroupHold(convo, null);
  }

  if (actions.dmUser) {
    const sourceGroup = isBotGroup(convo) ? convo : findLinkedGroup(convo);
    const dm = ensureBotDm(speakerBot, { sideThreadOf: sourceGroup ? sourceGroup.id : null });
    if (dm) {
      appendBotSpokenMessage(dm, speakerBot, actions.dmUser);
      dm.updatedAt = Date.now();
      saveStore({ immediate: true });
      if (typeof renderSidebar === 'function') renderSidebar();
      requestBotNavigation(dm.id);
    }
  }

  if (actions.groupPost) {
    const group = isBotGroup(convo) ? convo : findLinkedGroup(convo);
    if (group && isBotGroup(convo)) {
      persistGroupHold(group, null);
    } else if (group) {
      persistGroupHold(group, null);
      appendBotSpokenMessage(group, speakerBot, actions.groupPost);
      group.updatedAt = Date.now();
      saveStore({ immediate: true });
      if (typeof renderSidebar === 'function') renderSidebar();
      if (activeId === group.id && typeof renderThread === 'function') renderThread(group);
      else requestBotNavigation(group.id);
      const item = {
        id: newId('q'),
        editText: actions.groupPost,
        displayText: actions.groupPost,
        apiText: actions.groupPost,
        fromBotId: speakerBot.id,
        alreadyPosted: true,
        turn: defaultBotsTurn(),
      };
      if (isConvoBusy(group.id)) {
        queueBotsInject(group.id, item);
      } else if (typeof runBotsOutbound === 'function') {
        void runBotsOutbound(group, item, null, group.title);
      }
    }
  }
}

/** Soft mid-turn injects while a bots hop is streaming (not a hard Stop). */
const botsPendingInjects = new Map();

function queueBotsInject(convoId, item) {
  if (!convoId || !item) return;
  const list = botsPendingInjects.get(convoId) || [];
  list.push(item);
  botsPendingInjects.set(convoId, list);
}

function drainBotsInjects(convoId) {
  const list = botsPendingInjects.get(convoId) || [];
  botsPendingInjects.delete(convoId);
  return list;
}

function clearBotsInjects(convoId) {
  if (convoId) botsPendingInjects.delete(convoId);
}

/**
 * While a bot turn is live: steer into the agent loop when possible; otherwise
 * soft-abort the current hop and inject the user message into the hop queue.
 */
function injectBotsOutbound(convo, item) {
  if (!convo || !item) return false;
  const stream = typeof activeStreams !== 'undefined' ? activeStreams.get(convo.id) : null;
  if (stream && typeof canSteerLiveStream === 'function' && canSteerLiveStream(stream)) {
    const text = typeof steerTextFromItem === 'function' ? steerTextFromItem(item) : '';
    if (!text) return false;
    if (!stream.pendingSteers) stream.pendingSteers = [];
    const entry = { item, text, posted: false, applied: false };
    stream.pendingSteers.push(entry);
    if (typeof renderPendingSteerBubble === 'function') renderPendingSteerBubble(convo.id, entry);
    if (typeof flushPendingSteers === 'function') void flushPendingSteers(stream);
    return true;
  }
  queueBotsInject(convo.id, item);
  if (typeof softAbortStream === 'function') softAbortStream(convo.id);
  else if (typeof abortStream === 'function') {
    abortStream(convo.id, { cancelServer: true, soft: true });
  }
  return true;
}

function appendBotsInjectMessage(convo, item) {
  const userMessage = {
    role: 'user',
    content: item.displayText || (item.attachments?.length ? '(attachment)' : ''),
    steered: true,
  };
  if (item.attachments?.length) userMessage.attachments = item.attachments;
  if (item.replyQuote) userMessage.replyQuote = item.replyQuote;
  if (item.replyToSpeakerId) {
    userMessage.replyToSpeakerId = item.replyToSpeakerId;
    if (item.replyToSpeakerHandle) userMessage.replyToSpeakerHandle = item.replyToSpeakerHandle;
  }
  convo.messages.push(userMessage);
  convo.updatedAt = Date.now();
  saveConversations({ immediate: true });
  return userMessage;
}

async function runBotsOutbound(convo, item, userMessage, previousTitle) {
  markBotsOutboundActive(convo.id);
  // Edit-resubmit marks outboundStarting so the composer stays busy during abort.
  // Hops must not see that flag or runAssistantTurn returns false immediately.
  if (typeof clearOutboundStarting === 'function') clearOutboundStarting(convo.id);
  let stopped = false;
  let messageCountAtStart = convo.messages.length;
  try {
    clearBotsOutboundStopped(convo.id);
    if (typeof clearLiveTurnUserCancel === 'function') clearLiveTurnUserCancel(convo.id);
    const epoch = bumpBotsOutboundEpoch(convo.id);
    const members = participantBots(convo);
    const primary = members[0] || null;
    if (isBotsOutboundStopped(convo.id) || botsOutboundEpochOf(convo.id) !== epoch) {
      stopped = true;
    } else {
      await maybeCompactBotsConvo(convo, primary);
      if (isBotsOutboundStopped(convo.id) || botsOutboundEpochOf(convo.id) !== epoch) {
        stopped = true;
      }
    }
    if (!stopped) {
    clearBotsInjects(convo.id);
    const queue = botsToPing(convo, item.editText || item.displayText || '', {
      fromBotId: item.fromBotId || null,
      replyToSpeakerId: item.replyToSpeakerId || null,
    });
    if (!queue.length && primary && !item.fromBotId && !item.replyToSpeakerId) queue.push(primary);
    let hops = 0;
    messageCountAtStart = convo.messages.length;
    while (
      hops < BOT_GROUP_MAX_HOPS
      && (queue.length || botsPendingInjects.has(convo.id))
    ) {
      if (isBotsOutboundStopped(convo.id) || botsOutboundEpochOf(convo.id) !== epoch) {
        stopped = true;
        break;
      }
      // Apply injects that arrived between hops (or after a soft abort cleared the stream).
      for (const inject of drainBotsInjects(convo.id)) {
        const injected = inject.alreadyPosted ? null : appendBotsInjectMessage(convo, inject);
        botsToPing(convo, inject.editText || inject.displayText || '', {
          fromBotId: inject.fromBotId || null,
          replyToSpeakerId: inject.replyToSpeakerId || null,
        }).forEach((next) => {
          if (!queue.some((queued) => queued.id === next.id)) queue.unshift(next);
        });
        if (injected && activeId === convo.id) {
          chatThread.appendChild(
            buildBubble('user', injected.content, convo.messages.length - 1, injected, { animate: true })
          );
          scrollToBottom({ force: true });
        }
      }
      if (!queue.length) break;
      const bot = queue.shift();
      if (!bot) continue;
      hops += 1;
      const before = convo.messages.length;
      await runAssistantTurn(convo, {
        useAgent: item.turn.useAgent,
        text: item.apiText,
        skills: item.turn.skills,
        deepResearch: item.turn.deepResearch,
        deepResearchOutput: item.turn.deepResearchOutput,
        forceTools: item.turn.forceTools,
        dispatchedMessage: hops === 1 ? userMessage : null,
        queueItem: hops === 1 ? item : null,
        previousTitle,
        speakerBotId: bot.id,
        skipQueue: true,
      });
      if (isBotsOutboundStopped(convo.id) || botsOutboundEpochOf(convo.id) !== epoch) {
        stopped = true;
        break;
      }
      const last = convo.messages[convo.messages.length - 1];
      const producedAssistant = !!(
        last
        && last.role === 'assistant'
        && convo.messages.length > before
      );
      if (producedAssistant && typeof isSilentNoReply === 'function' && isSilentNoReply(last.content)) {
        convo.messages.pop();
        saveStore();
        if (activeId === convo.id) renderThread(convo, { drainQueue: false });
      } else if (
        producedAssistant
        && String(last.content || '').trim() !== 'No response.'
        && last.error !== 'No response.'
      ) {
        if (convo.botsHeldBy) {
          for (let i = queue.length - 1; i >= 0; i -= 1) {
            if (queue[i].id !== convo.botsHeldBy) queue.splice(i, 1);
          }
        } else {
          botsToPing(convo, last.content, { fromBotId: bot.id }).forEach((next) => {
            if (!queue.some((queued) => queued.id === next.id)) queue.push(next);
          });
        }
      }
      // Mid-turn user injects (steer/interrupt while a bot was thinking).
      const injects = drainBotsInjects(convo.id);
      for (const inject of injects) {
        const injected = inject.alreadyPosted ? null : appendBotsInjectMessage(convo, inject);
        const pinged = botsToPing(convo, inject.editText || inject.displayText || '', {
          fromBotId: inject.fromBotId || null,
          replyToSpeakerId: inject.replyToSpeakerId || null,
        });
        const resume = [];
        if (bot && !inject.replyToSpeakerId && !pinged.some((item) => item.id === bot.id)) resume.push(bot);
        [...resume, ...pinged].forEach((next) => {
          if (!queue.some((queued) => queued.id === next.id)) queue.push(next);
        });
        if (injected && activeId === convo.id) {
          chatThread.appendChild(
            buildBubble('user', injected.content, convo.messages.length - 1, injected, { animate: true })
          );
          scrollToBottom({ force: true });
        }
      }
    }
    }
    if (stopped) {
      purgeTrailingNoResponseMessages(convo, messageCountAtStart);
      saveStore();
    }
  } finally {
    clearBotsOutboundActive(convo.id);
  }
  if (!stopped) maybeSendNextQueued(convo.id);
  if (activeId === convo.id) renderThread(convo);
  if (typeof renderSidebar === 'function') renderSidebar();
  if (typeof flushBotNavigation === 'function') flushBotNavigation();
}

function purgeTrailingNoResponseMessages(convo, sinceIndex) {
  if (!convo || !Array.isArray(convo.messages)) return;
  const floor = Math.max(0, Number(sinceIndex) || 0);
  while (convo.messages.length > floor) {
    const last = convo.messages[convo.messages.length - 1];
    const text = String(last?.content || '').trim();
    if (last?.role === 'assistant' && (
      text === 'No response.'
      || last.error === 'No response.'
      || (typeof isSilentNoReply === 'function' && isSilentNoReply(last.content))
    )) {
      convo.messages.pop();
      continue;
    }
    break;
  }
}

function extraMentionItems(query, excluded) {
  if (!isBotsSurface()) return [];
  const convo = conversations.find((item) => item.id === activeId);
  const q = String(query || '').toLowerCase();
  const items = [];
  [
    { id: 'everyone', label: 'everyone', description: 'Notify every bot here', inline: true, section: 'Room' },
    { id: 'user', label: 'user', description: 'The human in this room', inline: true, section: 'Room' },
  ].forEach((item) => {
    if (item.label.startsWith(q) && !excluded.has(item.id)) items.push(item);
  });
  participantBots(convo).forEach((bot) => {
    if (q && !bot.handle.startsWith(q)) return;
    if (excluded.has(bot.handle)) return;
    items.push({
      id: bot.handle,
      label: bot.handle,
      description: bot.description || 'Bot',
      inline: true,
      section: 'Bots',
    });
  });
  return items;
}

function createBotSession(bot, { group = false, title = '', participantIds = [] } = {}) {
  const convo = {
    id: newId('c'),
    title: title || botDisplayHandle(bot),
    titleEdited: !group,
    messages: [],
    updatedAt: Date.now(),
    projectId: null,
    sortOrder: nextTopSortOrder(null),
    incognito: false,
    pinned: false,
    pinnedAt: null,
    surface: 'bots',
    botKind: group ? 'group' : 'dm',
    botId: group ? null : bot.id,
    participantBotIds: group ? participantIds.slice() : (bot ? [bot.id] : []),
    groupMemory: '',
    botsHeldBy: null,
    sideThreadOf: null,
    workspaceRoot: '',
  };
  conversations.push(convo);
  return convo;
}

function openBotDialog() {
  const modal = document.getElementById('botModal');
  const handle = document.getElementById('botHandle');
  const desc = document.getElementById('botDescription');
  const err = document.getElementById('botModalError');
  if (!modal) return;
  if (handle) handle.value = '';
  if (desc) desc.value = '';
  if (err) { err.textContent = ''; err.classList.add('is-hidden'); }
  openBackdrop(modal);
  queueMicrotask(() => handle && handle.focus());
}

function closeBotDialog() {
  closeBackdrop(document.getElementById('botModal'));
}

function saveBotFromDialog() {
  if (!requireUnlockedData()) return;
  const handle = normalizeHandle(document.getElementById('botHandle')?.value);
  const description = String(document.getElementById('botDescription')?.value || '').trim();
  const err = document.getElementById('botModalError');
  const showErr = (msg) => {
    if (!err) return;
    err.textContent = msg || '';
    err.classList.toggle('is-hidden', !msg);
  };
  if (!BOT_HANDLE_RE.test(handle)) {
    showErr('Use a handle like financial — start with a letter.');
    return;
  }
  if (bots.some((bot) => bot.handle === handle)) {
    showErr('That @handle is already taken.');
    return;
  }
  const bot = normalizeBot({
    id: newId('b'),
    handle,
    name: '@' + handle,
    description,
    avatarSeed: handle,
  });
  const convo = createBotSession(bot);
  bot.sessionId = convo.id;
  bots.push(bot);
  saveStore({ immediate: true });
  closeBotDialog();
  selectConversation(convo.id);
}

function fillGroupBotPicks(selectedIds) {
  const host = document.getElementById('groupBotPicks');
  if (!host) return;
  host.innerHTML = '';
  if (!bots.length) {
    const empty = document.createElement('p');
    empty.className = 'bot-pick-empty';
    empty.textContent = 'Create a bot first, then add it to a group.';
    host.appendChild(empty);
    return;
  }
  const selected = new Set(selectedIds || []);
  bots.forEach((bot) => {
    const label = document.createElement('label');
    label.className = 'bot-pick';
    const input = document.createElement('input');
    input.type = 'checkbox';
    input.value = bot.id;
    input.checked = selected.has(bot.id);
    label.appendChild(input);
    label.appendChild(createBotAvatarEl(bot, { className: 'bot-pick-avatar' }));
    const copy = document.createElement('span');
    copy.className = 'bot-pick-copy';
    const handle = document.createElement('span');
    handle.className = 'bot-pick-handle';
    handle.textContent = botDisplayHandle(bot);
    const desc = document.createElement('span');
    desc.className = 'bot-pick-desc';
    desc.textContent = bot.description || 'No description';
    copy.appendChild(handle);
    copy.appendChild(desc);
    label.appendChild(copy);
    host.appendChild(label);
  });
}

let editingGroupId = null;

function openGroupDialog(convo) {
  editingGroupId = convo ? convo.id : null;
  const modal = document.getElementById('groupModal');
  const title = document.getElementById('groupModalTitle');
  const name = document.getElementById('groupName');
  const err = document.getElementById('groupModalError');
  if (!modal) return;
  if (title) title.textContent = convo ? 'Edit group' : 'New group';
  if (name) name.value = convo ? (convo.title || '') : '';
  if (err) { err.textContent = ''; err.classList.add('is-hidden'); }
  fillGroupBotPicks(convo ? convo.participantBotIds : []);
  openBackdrop(modal);
  queueMicrotask(() => name && name.focus());
}

function closeGroupDialog() {
  closeBackdrop(document.getElementById('groupModal'));
  editingGroupId = null;
}

function saveGroupFromDialog() {
  if (!requireUnlockedData()) return;
  const picks = [...document.querySelectorAll('#groupBotPicks input[type="checkbox"]:checked')]
    .map((input) => input.value);
  const err = document.getElementById('groupModalError');
  const showErr = (msg) => {
    if (!err) return;
    err.textContent = msg || '';
    err.classList.toggle('is-hidden', !msg);
  };
  if (picks.length < 2) {
    showErr('Add at least two bots so they can talk to each other.');
    return;
  }
  const title = String(document.getElementById('groupName')?.value || '').trim()
    || picks.map((id) => botDisplayHandle(getBot(id))).filter(Boolean).join(', ');
  if (editingGroupId) {
    const convo = conversations.find((item) => item.id === editingGroupId);
    if (convo) {
      convo.title = title;
      convo.titleEdited = true;
      convo.participantBotIds = picks;
      convo.updatedAt = Date.now();
      saveStore({ immediate: true });
    }
    closeGroupDialog();
    renderSidebar();
    if (typeof syncProjectChrome === 'function') syncProjectChrome();
    return;
  }
  const convo = createBotSession(getBot(picks[0]), {
    group: true,
    title,
    participantIds: picks,
  });
  saveStore({ immediate: true });
  closeGroupDialog();
  selectConversation(convo.id);
}

async function deleteBotAndSession(botId) {
  const bot = getBot(botId);
  if (!bot) return;
  const ok = await confirmDanger({
    title: 'Delete ' + botDisplayHandle(bot) + ' and its session?',
    body: 'This cannot be undone.',
    confirmLabel: 'Delete',
  });
  if (!ok) return;
  abortStream(bot.sessionId);
  conversations = conversations.filter((convo) => convo.id !== bot.sessionId);
  conversations.forEach((convo) => {
    if (!Array.isArray(convo.participantBotIds)) return;
    convo.participantBotIds = convo.participantBotIds.filter((id) => id !== botId);
  });
  bots = bots.filter((item) => item.id !== botId);
  saveStore({ immediate: true });
  if (activeId === bot.sessionId) startDraft();
  else {
    renderSidebar();
    if (typeof syncProjectChrome === 'function') syncProjectChrome();
  }
}

function persistAppSurface(next) {
  const surface = next === 'bots' ? 'bots' : 'chat';
  appSurface = surface;
  document.getElementById('chatShell')?.setAttribute('data-surface', surface);
  if (typeof paintWordmarkSurface === 'function') paintWordmarkSurface(surface);
  if (storageReady && settings.appSurface !== surface) {
    saveSettings({ ...settings, appSurface: surface }, { immediate: false });
  }
}

function applyAppSurface(next, { skipRoute = false } = {}) {
  persistAppSurface(next);
  const surface = appSurface;
  if (surface === 'bots' && mainView === 'projects') {
    mainView = 'chat';
    document.getElementById('projectsView')?.classList.add('is-hidden');
  }
  if (surface === 'bots') activeProjectId = null;
  const convo = conversations.find((item) => item.id === activeId);
  if (!convo || convoSurfaceOf(convo) !== surface) startDraft();
  syncProjectChrome();
  renderSidebar();
  if (!skipRoute) syncUrlFromState({ replace: true });
}

function restoreAppSurface() {
  persistAppSurface(settings.appSurface === 'bots' ? 'bots' : 'chat');
}

function botsNotInGroup(convo) {
  const inGroup = new Set(Array.isArray(convo?.participantBotIds) ? convo.participantBotIds : []);
  return bots.filter((bot) => !inGroup.has(bot.id));
}

function persistGroupParticipants(convo, ids) {
  convo.participantBotIds = ids;
  if (convo.botsHeldBy && !ids.includes(convo.botsHeldBy)) convo.botsHeldBy = null;
  convo.updatedAt = Date.now();
  saveStore({ immediate: true });
  renderSidebar();
  if (typeof syncProjectChrome === 'function') syncProjectChrome();
}

function closeTraceMembersPicker() {
  if (!traceMembersPicker) return;
  traceMembersPicker.classList.add('is-hidden');
  traceMembersPicker.hidden = true;
  traceMembersPicker.innerHTML = '';
  btnTraceMemberAdd?.setAttribute('aria-expanded', 'false');
}

function renderTraceMembersPicker(convo) {
  if (!traceMembersPicker) return;
  traceMembersPicker.innerHTML = '';
  const available = botsNotInGroup(convo);
  if (!available.length) {
    const empty = document.createElement('p');
    empty.className = 'trace-members-empty';
    empty.textContent = 'Every bot is already in this group.';
    traceMembersPicker.appendChild(empty);
    return;
  }
  available.forEach((bot) => {
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'trace-member-pick';
    btn.dataset.botId = bot.id;
    const avatar = createBotAvatarEl(bot, { className: 'trace-member-avatar' });
    applyPrivacyMosaic(avatar, 'bot-avatar:' + bot.id, { dense: true });
    btn.appendChild(avatar);
    const copy = document.createElement('span');
    copy.className = 'trace-member-copy';
    const handle = document.createElement('span');
    handle.className = 'trace-member-handle';
    handle.textContent = botDisplayHandle(bot);
    applyPrivacyMosaic(handle, 'bot-handle:' + bot.id);
    const desc = document.createElement('span');
    desc.className = 'trace-member-desc';
    desc.textContent = bot.description || 'No description';
    copy.appendChild(handle);
    copy.appendChild(desc);
    btn.appendChild(copy);
    traceMembersPicker.appendChild(btn);
  });
}

function toggleTraceMembersPicker() {
  if (!requireUnlockedData()) return;
  const convo = conversations.find((item) => item.id === activeId);
  if (!traceMembersPicker || !isBotGroup(convo)) return;
  if (traceMembersFolded) setTraceMembersFolded(false);
  if (traceMembersPicker.hidden) {
    traceMembersPicker.hidden = false;
    traceMembersPicker.classList.remove('is-hidden');
    btnTraceMemberAdd?.setAttribute('aria-expanded', 'true');
    renderTraceMembersPicker(convo);
  } else {
    closeTraceMembersPicker();
  }
}

function kickBotFromActiveGroup(botId) {
  if (!requireUnlockedData()) return;
  const convo = conversations.find((item) => item.id === activeId);
  if (!isBotGroup(convo)) return;
  const ids = (Array.isArray(convo.participantBotIds) ? convo.participantBotIds : [])
    .filter((id) => id !== botId);
  if (ids.length < 2) return;
  persistGroupParticipants(convo, ids);
}

function addBotToActiveGroup(botId) {
  if (!requireUnlockedData()) return;
  const convo = conversations.find((item) => item.id === activeId);
  if (!isBotGroup(convo)) return;
  if (!getBot(botId)) return;
  const ids = Array.isArray(convo.participantBotIds) ? convo.participantBotIds.slice() : [];
  if (ids.includes(botId)) return;
  ids.push(botId);
  persistGroupParticipants(convo, ids);
}

const TRACE_ACTIVITY_SHARE_DEFAULT = 0.6;
const TRACE_ACTIVITY_SHARE_MIN = 0.22;
const TRACE_ACTIVITY_SHARE_MAX = 0.78;

let traceActivityShare = TRACE_ACTIVITY_SHARE_DEFAULT;
let traceActivityFolded = false;
let traceMembersFolded = false;
let traceSplitDrag = null;

function clampTraceActivityShare(value) {
  const n = Number(value);
  if (!Number.isFinite(n)) return TRACE_ACTIVITY_SHARE_DEFAULT;
  return Math.min(TRACE_ACTIVITY_SHARE_MAX, Math.max(TRACE_ACTIVITY_SHARE_MIN, n));
}

function loadTraceSplitPrefs() {
  traceActivityShare = clampTraceActivityShare(settings.traceActivityShare);
  traceActivityFolded = !!settings.traceActivityFolded;
  traceMembersFolded = !!settings.traceMembersFolded;
  if (typeof applyTraceSplitLayout === 'function') applyTraceSplitLayout();
}

function saveTraceSplitPrefs() {
  if (!storageReady) return;
  if (
    settings.traceActivityShare === traceActivityShare
    && !!settings.traceActivityFolded === !!traceActivityFolded
    && !!settings.traceMembersFolded === !!traceMembersFolded
  ) {
    return;
  }
  saveSettings({
    ...settings,
    traceActivityShare,
    traceActivityFolded,
    traceMembersFolded,
  }, { immediate: false });
}

function applyTraceSplitLayout() {
  const show = !!(traceSidebar && traceSidebar.classList.contains('has-members'));
  if (btnTraceActivityFold) {
    btnTraceActivityFold.hidden = !show;
    btnTraceActivityFold.setAttribute('aria-hidden', show ? 'false' : 'true');
  }
  if (traceSplitHandle) {
    traceSplitHandle.hidden = !show;
    traceSplitHandle.setAttribute('aria-valuenow', String(Math.round(traceActivityShare * 100)));
    traceSplitHandle.setAttribute(
      'aria-disabled',
      (!show || traceActivityFolded || traceMembersFolded) ? 'true' : 'false'
    );
  }
  if (!traceSidebar) return;
  if (!show) {
    traceSidebar.classList.remove('activity-folded', 'members-folded', 'is-resizing');
    traceSidebarSplit?.style.removeProperty('--trace-activity-share');
    traceSidebarSplit?.style.removeProperty('--trace-members-share');
    return;
  }
  traceSidebar.classList.toggle('activity-folded', traceActivityFolded);
  traceSidebar.classList.toggle('members-folded', traceMembersFolded);
  traceSidebarSplit?.style.setProperty('--trace-activity-share', traceActivityShare + 'fr');
  traceSidebarSplit?.style.setProperty('--trace-members-share', (1 - traceActivityShare) + 'fr');
  if (btnTraceActivityFold) {
    const expanded = !traceActivityFolded;
    btnTraceActivityFold.setAttribute('aria-expanded', expanded ? 'true' : 'false');
    btnTraceActivityFold.title = expanded ? 'Collapse live activity' : 'Show live activity';
    btnTraceActivityFold.setAttribute(
      'aria-label',
      expanded ? 'Collapse live activity' : 'Show live activity'
    );
  }
  if (btnTraceMembersFold) {
    const expanded = !traceMembersFolded;
    btnTraceMembersFold.setAttribute('aria-expanded', expanded ? 'true' : 'false');
    btnTraceMembersFold.title = expanded ? 'Collapse members' : 'Show members';
    btnTraceMembersFold.setAttribute(
      'aria-label',
      expanded ? 'Collapse members' : 'Show members'
    );
  }
}

function setTraceActivityFolded(next) {
  traceActivityFolded = !!next;
  saveTraceSplitPrefs();
  applyTraceSplitLayout();
}

function setTraceMembersFolded(next) {
  traceMembersFolded = !!next;
  if (traceMembersFolded) closeTraceMembersPicker();
  saveTraceSplitPrefs();
  applyTraceSplitLayout();
}

function setTraceActivityShare(next, { persist = true } = {}) {
  traceActivityShare = clampTraceActivityShare(next);
  applyTraceSplitLayout();
  if (persist) saveTraceSplitPrefs();
}

function beginTraceSplitDrag(event) {
  if (event.pointerType === 'mouse' && event.button !== 0) return;
  if (!traceSidebar?.classList.contains('has-members')) return;
  if (traceActivityFolded || traceMembersFolded) return;
  if (!traceSidebarSplit || !traceSplitHandle) return;
  event.preventDefault();
  traceSplitDrag = { pointerId: event.pointerId };
  traceSidebar.classList.add('is-resizing');
  try { traceSplitHandle.setPointerCapture(event.pointerId); } catch { /* ignore */ }
  window.addEventListener('pointermove', moveTraceSplitDrag);
  window.addEventListener('pointerup', endTraceSplitDrag);
  window.addEventListener('pointercancel', endTraceSplitDrag);
  moveTraceSplitDrag(event);
}

function moveTraceSplitDrag(event) {
  if (!traceSplitDrag || !traceSidebarSplit) return;
  const rect = traceSidebarSplit.getBoundingClientRect();
  const handleH = traceSplitHandle ? traceSplitHandle.getBoundingClientRect().height : 8;
  const available = Math.max(1, rect.height - handleH);
  const y = event.clientY - rect.top - handleH / 2;
  setTraceActivityShare(y / available, { persist: false });
}

function endTraceSplitDrag(event) {
  if (!traceSplitDrag) return;
  if (event && event.pointerId != null && traceSplitHandle) {
    try { traceSplitHandle.releasePointerCapture(event.pointerId); } catch { /* ignore */ }
  }
  traceSplitDrag = null;
  window.removeEventListener('pointermove', moveTraceSplitDrag);
  window.removeEventListener('pointerup', endTraceSplitDrag);
  window.removeEventListener('pointercancel', endTraceSplitDrag);
  traceSidebar?.classList.remove('is-resizing');
  saveTraceSplitPrefs();
}

function bindTraceSplitChrome() {
  loadTraceSplitPrefs();
  applyTraceSplitLayout();
  btnTraceActivityFold?.addEventListener('click', (event) => {
    event.stopPropagation();
    setTraceActivityFolded(!traceActivityFolded);
  });
  btnTraceMembersFold?.addEventListener('click', (event) => {
    event.stopPropagation();
    setTraceMembersFolded(!traceMembersFolded);
  });
  if (!traceSplitHandle) return;
  traceSplitHandle.addEventListener('pointerdown', beginTraceSplitDrag);
  traceSplitHandle.addEventListener('pointermove', moveTraceSplitDrag);
  traceSplitHandle.addEventListener('pointerup', endTraceSplitDrag);
  traceSplitHandle.addEventListener('pointercancel', endTraceSplitDrag);
  traceSplitHandle.addEventListener('dblclick', (event) => {
    event.preventDefault();
    setTraceActivityShare(TRACE_ACTIVITY_SHARE_DEFAULT);
  });
  traceSplitHandle.addEventListener('keydown', (event) => {
    if (traceActivityFolded || traceMembersFolded) return;
    if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
      event.preventDefault();
      const delta = event.key === 'ArrowUp' ? -0.04 : 0.04;
      setTraceActivityShare(traceActivityShare + delta);
    } else if (event.key === 'Home') {
      event.preventDefault();
      setTraceActivityShare(TRACE_ACTIVITY_SHARE_DEFAULT);
    }
  });
}

function renderTraceMembers() {
  if (!traceSidebar || !traceMembers) return;
  const convo = conversations.find((item) => item.id === activeId);
  const show = isBotsSurface() && isBotGroup(convo);
  traceSidebar.classList.toggle('has-members', show);
  traceMembers.hidden = !show;
  if (!show) {
    closeTraceMembersPicker();
    if (traceMembersList) traceMembersList.replaceChildren();
    applyTraceSplitLayout();
    return;
  }
  const members = participantBots(convo);
  const title = document.getElementById('traceMembersTitle');
  if (title) title.textContent = 'Members · ' + members.length;
  const available = botsNotInGroup(convo);
  if (btnTraceMemberAdd) {
    btnTraceMemberAdd.disabled = available.length === 0;
    btnTraceMemberAdd.title = available.length ? 'Add a bot' : 'Every bot is already in this group';
  }
  if (!traceMembersList) {
    applyTraceSplitLayout();
    return;
  }
  traceMembersList.replaceChildren();
  const canKick = members.length > 2;
  members.forEach((bot) => {
    const row = document.createElement('div');
    row.className = 'trace-member';
    const avatar = createBotAvatarEl(bot, { className: 'trace-member-avatar' });
    applyPrivacyMosaic(avatar, 'bot-avatar:' + bot.id, { dense: true });
    row.appendChild(avatar);
    const copy = document.createElement('div');
    copy.className = 'trace-member-copy';
    const handle = document.createElement('span');
    handle.className = 'trace-member-handle';
    handle.textContent = botDisplayHandle(bot);
    applyPrivacyMosaic(handle, 'bot-handle:' + bot.id);
    setIdentityTitle(handle, botDisplayHandle(bot));
    const desc = document.createElement('span');
    desc.className = 'trace-member-desc';
    desc.textContent = bot.description || 'No description';
    copy.appendChild(handle);
    copy.appendChild(desc);
    row.appendChild(copy);
    const kick = document.createElement('button');
    kick.type = 'button';
    kick.className = 'btn btn-ghost btn-icon trace-member-kick';
    kick.dataset.botId = bot.id;
    kick.setAttribute('aria-label', 'Remove ' + botDisplayHandle(bot) + ' from this group');
    kick.title = canKick ? 'Remove from group' : 'Groups need at least two bots';
    kick.disabled = !canKick;
    kick.textContent = '×';
    row.appendChild(kick);
    traceMembersList.appendChild(row);
  });
  if (traceMembersPicker && !traceMembersPicker.hidden) {
    if (!available.length) closeTraceMembersPicker();
    else renderTraceMembersPicker(convo);
  }
  applyTraceSplitLayout();
}

function bindBotChrome() {
  document.getElementById('btnNewBot')?.addEventListener('click', () => openBotDialog());
  document.getElementById('btnNewGroup')?.addEventListener('click', () => openGroupDialog(null));
  document.getElementById('btnBotCancel')?.addEventListener('click', closeBotDialog);
  document.getElementById('btnBotClose')?.addEventListener('click', closeBotDialog);
  document.getElementById('btnBotSave')?.addEventListener('click', saveBotFromDialog);
  document.getElementById('btnGroupCancel')?.addEventListener('click', closeGroupDialog);
  document.getElementById('btnGroupClose')?.addEventListener('click', closeGroupDialog);
  document.getElementById('btnGroupSave')?.addEventListener('click', saveGroupFromDialog);
  document.getElementById('botModal')?.addEventListener('click', (event) => {
    if (event.target.id === 'botModal') closeBotDialog();
  });
  document.getElementById('groupModal')?.addEventListener('click', (event) => {
    if (event.target.id === 'groupModal') closeGroupDialog();
  });
  btnTraceMemberAdd?.addEventListener('click', (event) => {
    event.stopPropagation();
    toggleTraceMembersPicker();
  });
  traceMembersList?.addEventListener('click', (event) => {
    const kick = event.target.closest('.trace-member-kick');
    if (!kick || kick.disabled) return;
    kickBotFromActiveGroup(kick.dataset.botId);
  });
  traceMembersPicker?.addEventListener('click', (event) => {
    const pick = event.target.closest('.trace-member-pick');
    if (!pick) return;
    addBotToActiveGroup(pick.dataset.botId);
  });
  document.addEventListener('click', (event) => {
    if (!traceMembersPicker || traceMembersPicker.hidden) return;
    if (event.target.closest('#traceMembers')) return;
    closeTraceMembersPicker();
  });
  bindTraceSplitChrome();
}

bindBotChrome();
