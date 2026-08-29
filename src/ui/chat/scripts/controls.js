function showSettingsPane(pane) {
  document.querySelectorAll('.settings-nav-btn').forEach((btn) => {
    btn.classList.toggle('is-active', btn.dataset.settingsPane === pane);
  });
  document.querySelectorAll('.settings-pane').forEach((el) => {
    el.classList.toggle('is-active', el.dataset.settingsPane === pane);
  });
  const scroll = document.getElementById('settingsScroll');
  if (scroll) scroll.scrollTop = 0;
  if (pane === 'data') {
    refreshLocalDataPane();
    refreshSettingsDataSummary();
  }
}

function refreshSettingsDataSummary() {
  const el = document.getElementById('settingsDataSummary');
  if (!el) return;
  const chatCount = conversations.filter((convo) => !(typeof isBotsConvo === 'function' && isBotsConvo(convo))).length;
  const botCount = typeof bots !== 'undefined' ? bots.length : 0;
  const projectCount = projects.length;
  const chatLabel = chatCount === 1 ? '1 chat' : chatCount + ' chats';
  const projectLabel = projectCount === 1 ? '1 project' : projectCount + ' projects';
  const where = 'on disk';
  const botLabel = botCount === 1 ? '1 bot' : botCount + ' bots';
  el.textContent = 'Stored ' + where + ': ' + chatLabel + ' · ' + botLabel + ' · ' + projectLabel + '.';
  const clearChats = document.getElementById('btnClearChats');
  const clearProjects = document.getElementById('btnClearProjects');
  const clearAll = document.getElementById('btnClearAllData');
  if (clearChats) clearChats.disabled = chatCount === 0;
  if (clearProjects) clearProjects.disabled = projectCount === 0;
  if (clearAll) clearAll.disabled = chatCount === 0 && projectCount === 0;
}

const outboundStarting = new Set();

function isConvoBusy(convoId) {
  return !!convoId && (
    activeStreams.has(convoId)
    || outboundStarting.has(convoId)
    || (typeof isBotsOutboundActive === 'function' && isBotsOutboundActive(convoId))
  );
}

function activeStream() {
  return activeId ? activeStreams.get(activeId) || null : null;
}

/** Hard stop for bot group hop loops — lives here so Stop works even before bots.js loads. */
const botsOutboundStopFlags = new Set();
/** Turns this page already started or stopped — do not resume from linger. */
const handledLiveTurnIds = new Set();
/** Conversations the user Stopped before a turn id arrived. */
const cancelledLiveConvoIds = new Set();

function rememberHandledLiveTurn(turnId) {
  const id = String(turnId || '').trim();
  if (id) handledLiveTurnIds.add(id);
}

function shouldSkipLiveTurnResume(info) {
  const convoId = String(info?.conversation_id || '').trim();
  const turnId = String(info?.turn_id || '').trim();
  if (turnId && handledLiveTurnIds.has(turnId)) return true;
  if (convoId && cancelledLiveConvoIds.has(convoId)) return true;
  return false;
}

function noteLiveTurnUserCancel(convoId, turnId) {
  if (convoId) cancelledLiveConvoIds.add(convoId);
  rememberHandledLiveTurn(turnId);
}

function clearLiveTurnUserCancel(convoId) {
  if (convoId) cancelledLiveConvoIds.delete(convoId);
}

function markBotsOutboundStopped(convoId) {
  if (convoId) botsOutboundStopFlags.add(convoId);
  if (typeof clearBotsInjects === 'function') clearBotsInjects(convoId);
}

function clearBotsOutboundStopped(convoId) {
  if (convoId) botsOutboundStopFlags.delete(convoId);
}

function isBotsOutboundStopped(convoId) {
  return !!convoId && botsOutboundStopFlags.has(convoId);
}

const cancelInFlight = new Map();

function markOutboundStarting(convoId) {
  if (!convoId) return;
  outboundStarting.add(convoId);
  if (typeof syncComposerStreamUi === 'function') syncComposerStreamUi();
}

function clearOutboundStarting(convoId) {
  if (!convoId) return;
  outboundStarting.delete(convoId);
  if (typeof syncComposerStreamUi === 'function') syncComposerStreamUi();
}

function scheduleCancel(convoId, turnId) {
  if (!convoId) return Promise.resolve();
  const prev = cancelInFlight.get(convoId) || Promise.resolve();
  const next = prev.catch(() => {}).then(() => fetch('/api/chat/cancel', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      conversation_id: convoId,
      ...(turnId ? { turn_id: turnId } : {}),
    }),
  }).then(() => {}).catch(() => {}));
  cancelInFlight.set(convoId, next);
  next.finally(() => {
    if (cancelInFlight.get(convoId) === next) cancelInFlight.delete(convoId);
  });
  return next;
}

function waitForCancel(convoId) {
  const pending = convoId ? cancelInFlight.get(convoId) : null;
  return pending ? pending.catch(() => {}) : Promise.resolve();
}

function abortStream(convoId, { cancelServer = true, soft = false } = {}) {
  if (cancelServer && !soft) markBotsOutboundStopped(convoId);
  if (cancelServer && !soft && typeof bumpBotsOutboundEpoch === 'function') {
    bumpBotsOutboundEpoch(convoId);
  }
  const stream = activeStreams.get(convoId);
  if (stream) {
    if (cancelServer) stream.cancelled = true;
    if (cancelServer && !soft) stream.hardStopped = true;
    rememberHandledLiveTurn(stream.turnId);
    if (cancelServer && !soft) noteLiveTurnUserCancel(convoId, stream.turnId);
    try { stream.controller.abort(); } catch { /* ignore */ }
    if (cancelServer && !soft) {
      if (typeof discardLiveStreamRow === 'function') discardLiveStreamRow(stream);
      else {
        try { stream.dom?.thinkingOrb?.stop(); } catch { /* ignore */ }
        stream.dom?.row?.remove();
        stream.dom = null;
      }
      if (activeStreams.get(convoId) === stream) activeStreams.delete(convoId);
    }
  } else if (cancelServer && !soft) {
    noteLiveTurnUserCancel(convoId, null);
  }
  const cancelDone = cancelServer && convoId
    ? scheduleCancel(convoId, stream?.turnId || null)
    : Promise.resolve();
  syncComposerStreamUi();
  return cancelDone;
}

/** Abort the live hop without ending the bots outbound loop. */
function softAbortStream(convoId) {
  abortStream(convoId, { cancelServer: true, soft: true });
}

function abortAllStreams({ cancelServer = true } = {}) {
  for (const id of [...activeStreams.keys()]) abortStream(id, { cancelServer });
  if (typeof stopAllBotsOutbound === 'function') stopAllBotsOutbound();
}

function syncComposerStreamUi() {
  const busy = isConvoBusy(activeId);
  // Keep Send visible while streaming so the next message can be queued.
  btnSend.classList.remove('is-hidden');
  btnSend.classList.toggle('is-queueing', busy);
  btnStop.classList.toggle('is-hidden', !busy);
  const botsBusy = busy
    && typeof isBotsConvo === 'function'
    && isBotsConvo(conversations.find((item) => item.id === activeId));
  btnSend.title = busy
    ? (botsBusy ? 'Queue for after this bot turn' : 'Queue message')
    : 'Send message';
  btnSend.setAttribute(
    'aria-label',
    busy
      ? (botsBusy ? 'Queue for after this bot turn' : 'Queue message')
      : 'Send message'
  );
  updateSendEnabled();
  updateComposerHint();
  // Steer appears only while an agent reply can accept mid-turn guidance.
  if (activeId) {
    const convo = conversations.find((c) => c.id === activeId);
    if (convo && getOutboundQueue(activeId).length) renderOutboundQueue(convo);
  }
  if (typeof syncSidebarBusyUi === 'function') syncSidebarBusyUi();
}

function getOutboundQueue(convoId) {
  if (!convoId) return [];
  let queue = outboundQueues.get(convoId);
  if (!queue) {
    queue = [];
    outboundQueues.set(convoId, queue);
  }
  return queue;
}

function clearOutboundQueue(convoId) {
  if (!convoId) return;
  if (editingQueueId && getOutboundQueue(convoId).some((item) => item.id === editingQueueId)) {
    editingQueueId = null;
  }
  outboundQueues.delete(convoId);
  if (activeId === convoId) renderOutboundQueue(conversations.find((c) => c.id === convoId));
  persistOutboundQueues();
}

function canSteerLiveStream(stream = activeStream()) {
  return !!(stream && stream.useAgent);
}

function steerTextFromItem(item) {
  if (!item) return '';
  const text = String(item.editText || item.displayText || '').trim();
  if (text && text !== '(attachment)') return text;
  if (item.attachments?.length) {
    const names = item.attachments
      .map((file) => file.name || 'attachment')
      .filter(Boolean);
    return names.length
      ? 'Please account for the attached file(s): ' + names.join(', ')
      : '';
  }
  return '';
}

function findQueuedItem(convoId, queueId) {
  return getOutboundQueue(convoId).find((item) => item.id === queueId) || null;
}

function isQueuePausedForEdit(convoId) {
  if (!convoId || !editingQueueId) return false;
  const head = getOutboundQueue(convoId)[0];
  return !!(head && head.id === editingQueueId);
}

function refreshQueuedBubble(row, item, { paused = false, canSteer = false } = {}) {
  if (!row || !item) return;
  row.dataset.raw = item.editText || item.displayText || '';
  const bubble = row.querySelector('.msg-bubble');
  if (bubble && !row.classList.contains('is-editing')) {
    bubble.innerHTML = formatUserMessageHtml({
      content: item.displayText,
      attachments: item.attachments,
      replyQuote: item.replyQuote,
      replyToSpeakerHandle: item.replyToSpeakerHandle,
    });
  }
  const label = row.querySelector('.msg-queued-label');
  if (label) {
    label.textContent = paused ? 'Paused · editing' : 'Queued';
    label.classList.toggle('is-paused', !!paused);
  }
  const meta = row.querySelector('.msg-queued-meta');
  if (meta) {
    meta.innerHTML = queuedMetaActionsHtml(item.id, { paused, canSteer });
  }
}

function queuedMetaActionsHtml(queueId, { paused = false, canSteer = false } = {}) {
  const id = escapeHtml(queueId);
  return (
    '<span class="msg-queued-label' + (paused ? ' is-paused' : '') + '">' +
      (paused ? 'Paused · editing' : 'Queued') +
    '</span>' +
    (canSteer
      ? '<button type="button" class="msg-queued-action" data-queue-steer="' + id +
        '" title="Guide the current reply without stopping it" aria-label="Steer current activity">Steer</button>'
      : '') +
    '<button type="button" class="msg-queued-action" data-queue-edit="' + id +
      '" aria-label="Edit queued message">Edit</button>' +
    '<button type="button" class="msg-queued-action" data-queue-remove="' + id +
      '" aria-label="Remove queued message">Remove</button>'
  );
}

function buildQueuedBubble(item, { paused = false, canSteer = false } = {}) {
  const row = document.createElement('div');
  row.className = 'msg msg-role-user msg-queued';
  row.dataset.queueId = item.id;
  row.dataset.raw = item.editText || item.displayText || '';
  const bubble = document.createElement('div');
  bubble.className = 'msg-bubble';
    bubble.innerHTML = formatUserMessageHtml({
      content: item.displayText,
      attachments: item.attachments,
      replyQuote: item.replyQuote,
      replyToSpeakerHandle: item.replyToSpeakerHandle,
    });
  const meta = document.createElement('div');
  meta.className = 'msg-queued-meta';
  meta.innerHTML = queuedMetaActionsHtml(item.id, { paused, canSteer });
  row.appendChild(bubble);
  row.appendChild(meta);
  return row;
}

function liveStreamRow() {
  if (!activeId || !chatThread) return null;
  const row = activeStreams.get(activeId)?.dom?.row;
  return row && chatThread.contains(row) ? row : null;
}

function lastRowBeforeQueue() {
  const queued = chatThread.querySelector(':scope > .msg-queued:not(.msg-steering)');
  const streamRow = liveStreamRow();
  let node = streamRow || chatThread.firstElementChild;
  if (!node) return null;
  let last = streamRow;
  while (node) {
    if (node === queued) break;
    last = node;
    node = node.nextElementSibling;
  }
  return last;
}

function insertAfterLiveReply(row) {
  if (!chatThread || !row) return;
  const queued = chatThread.querySelector(':scope > .msg-queued:not(.msg-steering)');
  if (queued) {
    chatThread.insertBefore(row, queued);
    return;
  }
  const anchor = lastRowBeforeQueue();
  if (anchor) anchor.after(row);
  else chatThread.appendChild(row);
}

function appendQueuedBubble(row) {
  if (!chatThread || !row) return;
  const queued = chatThread.querySelectorAll(':scope > .msg-queued:not(.msg-steering)');
  if (queued.length) {
    queued[queued.length - 1].after(row);
    return;
  }
  insertAfterLiveReply(row);
}

function renderOutboundQueue(convo) {
  if (!chatThread) return;
  const keepEditing = editingRow
    && editingRow.classList.contains('msg-queued')
    && editingRow.isConnected
    && chatThread.contains(editingRow)
    ? editingRow
    : null;
  const keepId = keepEditing?.dataset.queueId || null;
  chatThread.querySelectorAll('.msg-queued:not(.msg-steering)').forEach((el) => {
    if (keepId && el.dataset.queueId === keepId) return;
    el.remove();
  });
  if (!convo || activeId !== convo.id) return;
  const queue = getOutboundQueue(convo.id);
  const pausedHead = isQueuePausedForEdit(convo.id);
  const canSteer = canSteerLiveStream() && !pausedHead;
  for (const item of queue) {
    if (keepId && item.id === keepId) {
      refreshQueuedBubble(keepEditing, item, {
        paused: pausedHead && item.id === editingQueueId,
        canSteer: canSteer && item.id !== editingQueueId,
      });
      appendQueuedBubble(keepEditing);
      continue;
    }
    appendQueuedBubble(buildQueuedBubble(item, {
      paused: pausedHead && item.id === editingQueueId,
      canSteer,
    }));
  }
}

function enqueueOutbound(convo, item) {
  getOutboundQueue(convo.id).push(item);
  if (activeId === convo.id) {
    showThread(convo);
    const canSteer = canSteerLiveStream() && !isQueuePausedForEdit(convo.id);
    const row = buildQueuedBubble(item, { canSteer });
    appendQueuedBubble(row);
    queueMicrotask(() => motionEnter(row, { y: 10 }));
    scrollToBottom({ force: true });
  }
  updateComposerHint();
  syncComposerStreamUi();
  persistOutboundQueues();
}

function removeQueuedOutbound(convoId, queueId) {
  const queue = getOutboundQueue(convoId);
  const next = queue.filter((item) => item.id !== queueId);
  outboundQueues.set(convoId, next);
  if (editingQueueId === queueId) {
    if (editingRow?.dataset.queueId === queueId) {
      editingRow = null;
      mentionInput = composerInput;
      closeMentionMenu();
    }
    editingQueueId = null;
  }
  if (activeId === convoId) {
    const row = [...chatThread.querySelectorAll('.msg-queued')]
      .find((el) => el.dataset.queueId === queueId);
    if (row) row.remove();
  }
  updateComposerHint();
  persistOutboundQueues();
  maybeSendNextQueued(convoId);
}

/**
 * Mid-turn steer (Copilot/Claude pattern): keep the live agent turn running and
 * inject this message before the next LLM call (usually after the current tool
 * round). Does not abort. If the turn ends first, the message is re-queued.
 */
function steerQueuedOutbound(convoId, queueId) {
  if (!convoId || !queueId) return;
  const stream = activeStreams.get(convoId);
  if (!canSteerLiveStream(stream)) {
    showAttachHint('Steer is available while an agent reply is in progress.');
    return;
  }
  const queue = getOutboundQueue(convoId);
  const idx = queue.findIndex((item) => item.id === queueId);
  if (idx < 0) return;
  const [item] = queue.splice(idx, 1);
  outboundQueues.set(convoId, queue);
  if (editingQueueId === queueId) {
    if (editingRow?.dataset.queueId === queueId) {
      cancelMessageEdit({ resumeQueue: false });
    } else {
      editingQueueId = null;
    }
  }
  if (activeId === convoId) {
    const row = [...chatThread.querySelectorAll('.msg-queued')]
      .find((el) => el.dataset.queueId === queueId);
    if (row) row.remove();
    const convo = conversations.find((c) => c.id === convoId);
    if (convo) renderOutboundQueue(convo);
  }
  updateComposerHint();
  const text = steerTextFromItem(item);
  if (!text) {
    queue.unshift(item);
    outboundQueues.set(convoId, queue);
    persistOutboundQueues();
    if (activeId === convoId) {
      const convo = conversations.find((c) => c.id === convoId);
      if (convo) renderOutboundQueue(convo);
    }
    showAttachHint('Nothing to steer with — add text first.');
    updateComposerHint();
    return;
  }
  if (!stream.pendingSteers) stream.pendingSteers = [];
  const entry = { item, text, posted: false, applied: false };
  stream.pendingSteers.push(entry);
  persistOutboundQueues();
  renderPendingSteerBubble(convoId, entry);
  void flushPendingSteers(stream);
}

function renderPendingSteerBubble(convoId, entry) {
  if (activeId !== convoId || !chatThread || !entry?.item) return;
  const existing = chatThread.querySelector(
    '.msg-steering[data-steer-queue-id="' + CSS.escape(entry.item.id) + '"]'
  );
  if (existing) return;
  const row = document.createElement('div');
  row.className = 'msg msg-role-user msg-queued msg-steering';
  row.dataset.steerQueueId = entry.item.id;
  row.dataset.raw = entry.text;
  const bubble = document.createElement('div');
  bubble.className = 'msg-bubble';
  bubble.innerHTML = formatUserMessageHtml({
    content: entry.item.displayText || entry.text,
    attachments: entry.item.attachments,
  });
  const meta = document.createElement('div');
  meta.className = 'msg-queued-meta';
  meta.innerHTML =
    '<span class="msg-queued-label is-steering">Steering…</span>' +
    '<button type="button" class="msg-queued-action" data-steer-cancel="' +
      escapeHtml(entry.item.id) +
      '" aria-label="Cancel steer">Cancel</button>';
  row.appendChild(bubble);
  row.appendChild(meta);
  insertAfterLiveReply(row);
  queueMicrotask(() => motionEnter(row, { y: 8 }));
  scrollToBottom({ force: true });
}

async function postSteerToAgent(stream, entry) {
  const response = await fetch('/api/chat/steer', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      id: stream.steerId,
      text: entry.text,
      client_id: entry.item?.id || undefined,
    }),
  });
  if (!response.ok) {
    const problem = await response.json().catch(() => null);
    throw new Error((problem && problem.error) || 'Could not steer this turn');
  }
}

async function flushPendingSteers(stream) {
  if (!stream?.steerId || !Array.isArray(stream.pendingSteers)) return;
  for (const entry of stream.pendingSteers.slice()) {
    if (entry.posted || entry.applied) continue;
    try {
      await postSteerToAgent(stream, entry);
      entry.posted = true;
    } catch (error) {
      entry.posted = false;
      showAttachHint(error?.message || 'Could not steer this turn');
      if (!entry.applied) restoreSteerToQueue(stream, entry);
    }
  }
}

function restoreSteerToQueue(stream, entry) {
  if (!stream?.pendingSteers || !entry?.item) return;
  const idx = stream.pendingSteers.indexOf(entry);
  if (idx >= 0) stream.pendingSteers.splice(idx, 1);
  const convoId = [...activeStreams.entries()].find(([, live]) => live === stream)?.[0];
  if (!convoId) return;
  const row = chatThread?.querySelector(
    '.msg-steering[data-steer-queue-id="' + CSS.escape(entry.item.id) + '"]'
  );
  if (row) row.remove();
  getOutboundQueue(convoId).unshift(entry.item);
  persistOutboundQueues();
  const convo = conversations.find((c) => c.id === convoId);
  if (convo && activeId === convoId) renderOutboundQueue(convo);
  updateComposerHint();
}

function cancelPendingSteer(convoId, queueId) {
  const stream = activeStreams.get(convoId);
  if (!stream?.pendingSteers) return;
  const idx = stream.pendingSteers.findIndex((entry) => entry.item?.id === queueId);
  if (idx < 0) return;
  const [entry] = stream.pendingSteers.splice(idx, 1);
  if (activeId === convoId) {
    const row = chatThread.querySelector(
      '.msg-steering[data-steer-queue-id="' + CSS.escape(queueId) + '"]'
    );
    if (row) row.remove();
  }
  if (entry?.item && !entry.applied && !entry.posted) {
    getOutboundQueue(convoId).unshift(entry.item);
    persistOutboundQueues();
    const convo = conversations.find((c) => c.id === convoId);
    if (convo && activeId === convoId) renderOutboundQueue(convo);
  } else if (entry?.posted && !entry.applied) {
    showAttachHint('That steer already reached this turn.');
  }
  updateComposerHint();
}

function applySteeredEntry(convo, stream, text, entry) {
  const content = String(text || entry?.text || '').trim();
  if (!content) return;
  if (entry) entry.applied = true;
  const userMessage = {
    role: 'user',
    content: entry?.item?.displayText || content,
    steered: true,
  };
  if (entry?.item?.attachments?.length) {
    userMessage.attachments = entry.item.attachments;
  }
  convo.messages.push(userMessage);
  convo.updatedAt = Date.now();
  saveConversations();
  if (stream.dom?.row) {
    stream.dom.row.dataset.msgIndex = String(liveTurnSlices(convo).followUpStart);
    if (activeId === convo.id) {
      selectTraceMessage(liveTurnSlices(convo).followUpStart, { animate: false, ensureOpen: false });
    }
  }
  if (stream.timeline) {
    stream.timeline.push({
      type: 'notice',
      content: 'Steered: ' + content,
      tone: 'ok',
      kind: 'steer',
    });
  }
  if (activeId === convo.id) {
    const pendingRow = entry?.item?.id
      ? chatThread.querySelector(
        '.msg-steering[data-steer-queue-id="' + CSS.escape(entry.item.id) + '"]'
      )
      : null;
    const idx = convo.messages.length - 1;
    const row = buildBubble('user', userMessage.content, idx, userMessage, { animate: true });
    if (pendingRow) pendingRow.replaceWith(row);
    else insertAfterLiveReply(row);
  }
}

function reclaimUnappliedSteers(convoId, stream) {
  const pending = stream?.pendingSteers;
  if (!pending?.length) return;
  const queue = getOutboundQueue(convoId);
  const failed = !!(stream.cancelled || stream.hardStopped || stream.errorMessage);
  for (let i = pending.length - 1; i >= 0; i -= 1) {
    const entry = pending[i];
    if (entry.applied || !entry.item) continue;
    // Posted steers on a finished turn were consumed server-side; re-queueing
    // them would send a duplicate user turn. Reclaim only if they never left
    // or the turn died before the model could use them.
    if (entry.posted && !failed) continue;
    queue.unshift(entry.item);
  }
  stream.pendingSteers = [];
  persistOutboundQueues();
  if (activeId === convoId) {
    chatThread.querySelectorAll('.msg-steering').forEach((el) => el.remove());
    const convo = conversations.find((c) => c.id === convoId);
    if (convo) renderOutboundQueue(convo);
  }
}

function beginQueuedMessageEdit(row) {
  if (!row?.classList.contains('msg-queued') || row.classList.contains('is-editing')) return;
  const queueId = row.dataset.queueId;
  if (!queueId || !activeId) return;
  const item = findQueuedItem(activeId, queueId);
  if (!item) return;
  if (editingRow && editingRow !== row) cancelMessageEdit();

  const raw = item.editText || item.displayText || '';
  const bubble = row.querySelector('.msg-bubble');
  if (!bubble) return;

  const editor = document.createElement('div');
  editor.className = 'msg-edit';
  const input = document.createElement('textarea');
  input.className = 'msg-edit-input';
  input.value = raw === '(attachment)' ? '' : raw;
  input.setAttribute('aria-label', 'Edit queued message');
  input.placeholder = 'Edit queued message… Type @ to mention';
  const bar = document.createElement('div');
  bar.className = 'msg-edit-bar';
  const btnCancel = document.createElement('button');
  btnCancel.type = 'button';
  btnCancel.className = 'btn btn-ghost';
  btnCancel.textContent = 'Cancel';
  const btnSave = document.createElement('button');
  btnSave.type = 'button';
  btnSave.className = 'btn btn-primary';
  btnSave.textContent = 'Save';
  bar.appendChild(btnCancel);
  bar.appendChild(btnSave);
  editor.appendChild(input);
  editor.appendChild(bar);

  bubble.replaceWith(editor);
  row.classList.add('is-editing');
  editingRow = row;
  editingQueueId = queueId;
  mentionInput = input;
  refreshQueuedBubble(row, item, { paused: isQueuePausedForEdit(activeId) });
  updateComposerHint();

  const commit = () => {
    const next = input.value.trim();
    if (!next && !(item.attachments && item.attachments.length)) return;
    closeMentionMenu();
    mentionInput = composerInput;
    saveQueuedMessageEdit(row, next);
  };
  btnCancel.addEventListener('click', () => cancelMessageEdit());
  btnSave.addEventListener('click', commit);
  input.addEventListener('input', () => {
    autoResize(input);
    updateMentionMenu(input);
  });
  input.addEventListener('click', () => updateMentionMenu(input));
  input.addEventListener('keyup', (event) => {
    if (['ArrowLeft', 'ArrowRight', 'Home', 'End'].includes(event.key)) {
      updateMentionMenu(input);
    }
  });
  input.addEventListener('blur', () => {
    setTimeout(() => {
      if (document.activeElement !== input) closeMentionMenu();
    }, 120);
  });
  input.addEventListener('keydown', (event) => {
    if (handleMentionKeydown(event)) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      cancelMessageEdit();
      return;
    }
    if (event.key !== 'Enter') return;
    if (settings.enterSends) {
      if (!event.shiftKey) {
        event.preventDefault();
        commit();
      }
    } else if (event.ctrlKey || event.metaKey) {
      event.preventDefault();
      commit();
    }
  });
  input.focus();
  input.setSelectionRange(input.value.length, input.value.length);
  autoResize(input);
  // If this item is already at the head, pause drain until save/cancel.
  if (isQueuePausedForEdit(activeId)) updateComposerHint();
}

function saveQueuedMessageEdit(row, rawText) {
  const queueId = row?.dataset.queueId;
  if (!queueId || !activeId) return;
  const item = findQueuedItem(activeId, queueId);
  if (!item) return;

  const mentioned = parseCapabilityMentions(rawText);
  const text = mentioned.text;
  const mentionIds = new Set(mentioned.mentions);
  mentionIds.delete('agent');
  const turn = resolveTurnSkills(mentionIds);
  const displayText = displayTextWithMentions(text, mentioned.mentions)
    || (item.attachments?.length ? '(attachment)' : '');
  item.editText = rawText.trim() || displayText;
  item.displayText = displayText;
  item.apiText = userMessageApiContent({
    content: displayText,
    attachments: item.attachments || [],
  });
  item.turn = {
    useAgent: turn.useAgent,
    skills: turn.skills,
    deepResearch: turn.deepResearch,
    deepResearchOutput: turn.deepResearchOutput,
    forceTools: turn.forceTools,
  };

  const editor = row.querySelector('.msg-edit');
  const nextBubble = document.createElement('div');
  nextBubble.className = 'msg-bubble';
  nextBubble.innerHTML = formatUserMessageHtml({
    content: item.displayText,
    attachments: item.attachments,
    replyQuote: item.replyQuote,
    replyToSpeakerHandle: item.replyToSpeakerHandle,
  });
  if (editor) editor.replaceWith(nextBubble);
  row.classList.remove('is-editing');
  row.dataset.raw = item.editText;
  editingRow = null;
  editingQueueId = null;
  mentionInput = composerInput;
  closeMentionMenu();
  refreshQueuedBubble(row, item, { paused: false });
  updateComposerHint();
  persistOutboundQueues();
  maybeSendNextQueued(activeId);
  focusComposer();
}

function dispatchOutboundTurn(convo, item) {
  const previousTitle = convo.title;
  const userMessage = {
    role: 'user',
    content: item.displayText || (item.attachments?.length ? '(attachment)' : ''),
  };
  if (item.attachments?.length) userMessage.attachments = item.attachments;
  if (item.replyQuote) userMessage.replyQuote = item.replyQuote;
  if (item.replyToSpeakerId) {
    userMessage.replyToSpeakerId = item.replyToSpeakerId;
    if (item.replyToSpeakerHandle) userMessage.replyToSpeakerHandle = item.replyToSpeakerHandle;
  }
  convo.messages.push(userMessage);
  if (convo.messages.length === 1 && !convo.titleEdited && !(typeof isBotsConvo === 'function' && isBotsConvo(convo))) {
    convo.title = provisionalTitle(
      item.displayText || (item.attachments?.[0]?.name || 'Attachment')
    );
  }
  convo.updatedAt = Date.now();
  saveConversations({ immediate: true });
  renderSidebar();
  if (activeId === convo.id) {
    showThread(convo);
    // Drop the matching queued ghost if still present, then append the real bubble.
    const ghost = [...chatThread.querySelectorAll('.msg-queued')]
      .find((row) => row.dataset.queueId === item.id);
    if (ghost) ghost.remove();
    chatThread.appendChild(
      buildBubble('user', userMessage.content, convo.messages.length - 1, userMessage, { animate: true })
    );
    scrollToBottom({ force: true });
  }
  if (typeof isBotsConvo === 'function' && isBotsConvo(convo) && typeof runBotsOutbound === 'function') {
    void runBotsOutbound(convo, item, userMessage, previousTitle);
    return;
  }
  void runAssistantTurn(convo, {
    useAgent: item.turn.useAgent,
    text: item.apiText,
    skills: item.turn.skills,
    deepResearch: item.turn.deepResearch,
    deepResearchOutput: item.turn.deepResearchOutput,
    forceTools: item.turn.forceTools,
    dispatchedMessage: userMessage,
    queueItem: item,
    previousTitle,
  });
}

function maybeSendNextQueued(convoId) {
  if (!convoId || isConvoBusy(convoId)) return;
  const queue = getOutboundQueue(convoId);
  if (!queue.length) {
    updateComposerHint();
    return;
  }
  if (isQueuePausedForEdit(convoId)) {
    const convo = conversations.find((item) => item.id === convoId);
    if (convo && activeId === convoId) renderOutboundQueue(convo);
    updateComposerHint();
    return;
  }
  const convo = conversations.find((item) => item.id === convoId);
  if (!convo) {
    outboundQueues.delete(convoId);
    persistOutboundQueues();
    return;
  }
  const next = queue.shift();
  outboundQueues.set(convoId, queue);
  persistOutboundQueues();
  dispatchOutboundTurn(convo, next);
  if (activeId === convoId) renderOutboundQueue(convo);
  updateComposerHint();
}

function clearAllChats() {
  if (!requireUnlockedData()) return;
  const chats = conversations.filter((convo) => !(typeof isBotsConvo === 'function' && isBotsConvo(convo)));
  if (chats.length === 0) return;
  const n = chats.length;
  if (!confirm('Delete all ' + n + ' chat' + (n === 1 ? '' : 's') + '? This cannot be undone.')) return;
  const keep = conversations.filter((convo) => typeof isBotsConvo === 'function' && isBotsConvo(convo));
  const dropIds = new Set(chats.map((convo) => convo.id));
  for (const id of dropIds) {
    abortStream(id);
    outboundQueues.delete(id);
    stickByConvo.delete(id);
  }
  if (editingQueueId && dropIds.has(activeId)) {
    editingQueueId = null;
    if (editingRow) {
      editingRow = null;
      mentionInput = composerInput;
      closeMentionMenu();
    }
  }
  conversations = keep;
  if (activeId && !conversations.some((item) => item.id === activeId)) activeId = null;
  saveStore();
  startDraft();
  renderSidebar();
  if (mainView === 'projects') renderProjectsPage();
  refreshSettingsDataSummary();
}

function clearAllProjects() {
  if (!requireUnlockedData()) return;
  if (projects.length === 0) return;
  const n = projects.length;
  if (!confirm('Delete all ' + n + ' project' + (n === 1 ? '' : 's') + '? Chats move back to Recents.')) return;
  conversations.forEach((convo) => { convo.projectId = null; });
  projects = [];
  activeProjectId = null;
  saveStore();
  renderSidebar();
  if (mainView === 'projects') renderProjectsPage();
  else {
    showChatView();
    syncProjectChrome();
  }
  updateGreeting();
  refreshSettingsDataSummary();
}

function clearAllChatsAndProjects() {
  if (!requireUnlockedData()) return;
  if (conversations.length === 0 && projects.length === 0) return;
  if (!confirm('Delete all chats and projects from local data? This cannot be undone.')) return;
  const drop = conversations.filter((convo) => !(typeof isBotsConvo === 'function' && isBotsConvo(convo)));
  const dropIds = new Set(drop.map((convo) => convo.id));
  for (const id of dropIds) {
    abortStream(id);
    outboundQueues.delete(id);
    stickByConvo.delete(id);
  }
  if (editingQueueId && dropIds.has(activeId)) {
    editingQueueId = null;
    if (editingRow) {
      editingRow = null;
      mentionInput = composerInput;
      closeMentionMenu();
    }
  }
  conversations = conversations.filter((convo) => typeof isBotsConvo === 'function' && isBotsConvo(convo));
  projects = [];
  activeId = null;
  activeProjectId = null;
  saveStore();
  startDraft();
  renderSidebar();
  if (mainView === 'projects') renderProjectsPage();
  updateGreeting();
  refreshSettingsDataSummary();
}

let pendingChatBackgroundImage = '';
let pendingChatBackgroundImageName = '';

function syncChatBackgroundForm() {
  const source = normalizeChatBackgroundImage(pendingChatBackgroundImage);
  const isLocal = source.startsWith('data:image/');
  const urlInput = document.getElementById('settingChatBackgroundUrl');
  const hint = document.getElementById('chatBackgroundFileHint');
  const clearButton = document.getElementById('btnChatBackgroundClear');
  const hasInvalidUrl = !!pendingChatBackgroundImage && !source;
  if (urlInput && document.activeElement !== urlInput) {
    urlInput.value = isLocal ? '' : source;
  }
  if (urlInput) {
    urlInput.setCustomValidity(hasInvalidUrl ? 'Enter a valid http:// or https:// image URL.' : '');
    urlInput.setAttribute('aria-invalid', hasInvalidUrl ? 'true' : 'false');
  }
  if (hint) {
    hint.textContent = hasInvalidUrl
      ? 'Enter a valid URL beginning with http:// or https://.'
      : (isLocal
        ? 'Local image: ' + (pendingChatBackgroundImageName || 'Selected image')
        : 'Enter a remote URL or choose a PNG, JPEG, WebP, GIF, or AVIF file up to 1 MB.');
  }
  if (clearButton) clearButton.disabled = !source && !pendingChatBackgroundImage;
  const positionInput = document.getElementById('settingChatBackgroundPosition');
  const position = CHAT_BACKGROUND_POSITIONS.includes(positionInput?.value)
    ? positionInput.value
    : DEFAULT_SETTINGS.chatBackgroundPosition;
  document.querySelectorAll('[data-background-position]').forEach((button) => {
    const selected = button.dataset.backgroundPosition === position;
    button.setAttribute('aria-checked', selected ? 'true' : 'false');
    button.tabIndex = selected ? 0 : -1;
  });
  const preview = document.getElementById('chatBackgroundPositionPreview');
  const previewImage = document.getElementById('chatBackgroundPositionImage');
  if (preview && previewImage) {
    preview.classList.toggle('has-image', !!source);
    previewImage.style.objectPosition = position;
    if (source) previewImage.src = source;
    else previewImage.removeAttribute('src');
  }
  const range = document.getElementById('settingChatBackgroundOverlay');
  const output = document.getElementById('settingChatBackgroundOverlayValue');
  if (range && output) {
    output.value = range.value + '%';
    const previewShade = document.querySelector('.background-position-shade');
    if (previewShade) previewShade.style.opacity = String(Number(range.value) / 100);
  }
}

function fillDefaultModelSetting() {
  const select = document.getElementById('settingDefaultModel');
  if (!select) return;
  const options = (typeof modelMenuOptions !== 'undefined' && modelMenuOptions.length)
    ? modelMenuOptions
    : ((latestState?.network?.remote_models || []).map((model) => (
      typeof catalogOptionFromRemote === 'function'
        ? catalogOptionFromRemote(model)
        : {
          value: model.id,
          label: model.model || model.label || model.id,
          provider: String(model.provider_name || '').trim(),
        }
    )));
  const current = String(selectedChatModel || settings.selectedChatModel || '').trim();
  const previous = select.value;
  select.replaceChildren();
  if (!options.length) {
    select.disabled = true;
    const empty = document.createElement('option');
    empty.value = '';
    empty.textContent = 'No models available yet';
    select.appendChild(empty);
    return;
  }
  select.disabled = false;
  let hasCurrent = false;
  options.forEach((option) => {
    const opt = document.createElement('option');
    opt.value = option.value;
    opt.textContent = option.provider
      ? (option.label + ' · ' + option.provider)
      : option.label;
    if (option.value === current) {
      opt.selected = true;
      hasCurrent = true;
    }
    select.appendChild(opt);
  });
  if (current && !hasCurrent) {
    const opt = document.createElement('option');
    opt.value = current;
    opt.textContent = (typeof modelIdLabel === 'function' ? modelIdLabel(current) : '') || current;
    opt.selected = true;
    select.insertBefore(opt, select.firstChild);
  } else if (!current && select.options.length) {
    select.selectedIndex = 0;
  } else if (!hasCurrent && previous && [...select.options].some((opt) => opt.value === previous)) {
    select.value = previous;
  }
}

function fillSettingsFormFromState() {
  document.getElementById('settingName').value = settings.name;
  document.getElementById('settingAbout').value = settings.about;
  document.getElementById('settingInstructions').value = settings.instructions;
  document.getElementById('settingMemory').value = settings.memory || '';
  document.getElementById('settingThinking').value = settings.thinking;
  document.getElementById('settingThinkingEffort').value = settings.thinkingEffort;
  document.getElementById('settingEnterSends').checked = settings.enterSends;
  document.getElementById('settingSkillWebSearch').checked = settings.skillWebSearch;
  document.getElementById('settingWebSearchDepth').value = settings.webSearchDepth || 'off';
  document.getElementById('settingWebSearchProvider').value = settings.webSearchProvider || 'auto';
  document.getElementById('settingWebSearchSearxng').value = settings.webSearchSearxng || '';
  document.getElementById('settingWebSearchParallelApiKey').value = settings.webSearchParallelApiKey || '';
  document.getElementById('settingWebSearchParallelMode').value = settings.webSearchParallelMode || 'fast';
  document.getElementById('settingWebSearchTinyfishApiKey').value = settings.webSearchTinyfishApiKey || '';
  document.getElementById('settingWebSearchResults').value = String(settings.webSearchResults || 6);
  document.getElementById('settingWebSearchRegion').value = settings.webSearchRegion || 'us-en';
  document.getElementById('settingWebSearchSafeSearch').value = settings.webSearchSafeSearch || 'moderate';
  document.getElementById('settingWebSearchRecency').value = settings.webSearchRecency || 'any';
  document.getElementById('settingSkillFetchUrl').checked = settings.skillFetchUrl;
  document.getElementById('settingFetchUrlMaxChars').value = String(settings.fetchUrlMaxChars || 8000);
  document.getElementById('settingWebSearchPageMaxChars').value = String(
    Number.isFinite(settings.webSearchPageMaxChars) ? settings.webSearchPageMaxChars : 0
  );
  document.getElementById('settingSkillDeepResearch').checked = settings.skillDeepResearch !== false;
  document.getElementById('settingSkillFilesystem').checked = !!settings.skillFilesystem;
  document.getElementById('settingSkillTerminal').checked = !!settings.skillTerminal;
  document.getElementById('settingSkillBrowser').checked = !!settings.skillBrowser;
  document.getElementById('settingTerminalTimeout').value = String(settings.terminalTimeoutSecs || 30);
  syncApprovalModeControls();
  syncTerminalSkillControls();
  document.getElementById('settingAttachmentsMode').value = settings.attachmentsMode || 'auto';
  document.getElementById('settingAttachmentTextFallback').checked = !!settings.attachmentTextFallback;
  document.getElementById('settingAttachmentOcr').checked = !!settings.attachmentOcr;
  document.getElementById('settingAttachmentMaxChars').value = String(settings.attachmentMaxChars || 48000);
  pendingChatBackgroundImage = settings.chatBackgroundImage || '';
  pendingChatBackgroundImageName = settings.chatBackgroundImageName || '';
  document.getElementById('settingChatBackgroundPosition').value = settings.chatBackgroundPosition || 'center';
  document.getElementById('settingChatBackgroundOverlay').value = String(settings.chatBackgroundOverlay ?? 72);
  syncChatBackgroundForm();
  syncWebSearchControls();
  syncFetchUrlControls();
  syncTerminalSkillControls();
  syncAttachmentFallbackControls();
  fillDefaultModelSetting();
}

function readSettingsForm() {
  const maxCharsRaw = Number(document.getElementById('settingAttachmentMaxChars').value);
  const fetchUrlMaxCharsRaw = Number(document.getElementById('settingFetchUrlMaxChars').value);
  const webSearchPageMaxCharsRaw = Number(document.getElementById('settingWebSearchPageMaxChars').value);
  const searchResultsRaw = Number(document.getElementById('settingWebSearchResults').value);
  const searchRegionRaw = document.getElementById('settingWebSearchRegion').value.trim().toLowerCase();
  const defaultModel = String(document.getElementById('settingDefaultModel')?.value || '').trim();
  return normalizeSettings({
    ...settings,
    name: document.getElementById('settingName').value.trim(),
    about: document.getElementById('settingAbout').value,
    instructions: document.getElementById('settingInstructions').value,
    memory: document.getElementById('settingMemory').value,
    thinking: document.getElementById('settingThinking').value,
    thinkingEffort: document.getElementById('settingThinkingEffort').value,
    enterSends: document.getElementById('settingEnterSends').checked,
    skillWebSearch: document.getElementById('settingSkillWebSearch').checked,
    webSearchDepth: document.getElementById('settingWebSearchDepth').value,
    webSearchProvider: document.getElementById('settingWebSearchProvider').value,
    webSearchSearxng: document.getElementById('settingWebSearchSearxng').value.trim(),
    webSearchParallelApiKey: document.getElementById('settingWebSearchParallelApiKey').value.trim(),
    webSearchParallelMode: document.getElementById('settingWebSearchParallelMode').value,
    webSearchTinyfishApiKey: document.getElementById('settingWebSearchTinyfishApiKey').value.trim(),
    webSearchResults: searchResultsRaw,
    webSearchRegion: searchRegionRaw,
    webSearchSafeSearch: document.getElementById('settingWebSearchSafeSearch').value,
    webSearchRecency: document.getElementById('settingWebSearchRecency').value,
    skillFetchUrl: document.getElementById('settingSkillFetchUrl').checked,
    fetchUrlMaxChars: fetchUrlMaxCharsRaw,
    webSearchPageMaxChars: webSearchPageMaxCharsRaw,
    skillDeepResearch: document.getElementById('settingSkillDeepResearch').checked,
    skillFilesystem: document.getElementById('settingSkillFilesystem').checked,
    skillTerminal: document.getElementById('settingSkillTerminal').checked,
    skillBrowser: document.getElementById('settingSkillBrowser').checked,
    terminalTimeoutSecs: Number(document.getElementById('settingTerminalTimeout').value),
    approvalMode: document.querySelector('#approvalModeToggle [data-approval-mode].is-active')?.dataset.approvalMode
      || settings.approvalMode
      || 'manual',
    agentMode: settings.agentMode,
    deepResearch: settings.deepResearch,
    attachmentsMode: document.getElementById('settingAttachmentsMode').value,
    attachmentTextFallback: document.getElementById('settingAttachmentTextFallback').checked,
    attachmentOcr: document.getElementById('settingAttachmentOcr').checked,
    attachmentMaxChars: maxCharsRaw,
    chatBackgroundImage: pendingChatBackgroundImage,
    chatBackgroundImageName: pendingChatBackgroundImageName,
    chatBackgroundPosition: document.getElementById('settingChatBackgroundPosition').value,
    chatBackgroundOverlay: Number(document.getElementById('settingChatBackgroundOverlay').value),
    selectedChatModel: defaultModel || settings.selectedChatModel || '',
  });
}

function settingsFormIsDirty() {
  const invalidBackgroundUrl = !!pendingChatBackgroundImage
    && !normalizeChatBackgroundImage(pendingChatBackgroundImage);
  return invalidBackgroundUrl
    || JSON.stringify(readSettingsForm()) !== JSON.stringify(normalizeSettings(settings));
}

function syncSettingsSaveButton({ saved = false, saving = false, failed = false } = {}) {
  const btn = document.getElementById('btnSettingsSave');
  const label = document.getElementById('btnSettingsSaveLabel');
  if (!btn || !label) return;
  const check = btn.querySelector('.settings-save-check');
  const dirty = settingsFormIsDirty();
  const mode = saving ? 'saving'
    : failed ? 'failed'
      : (saved && !dirty) ? 'saved'
        : dirty ? 'dirty' : 'idle';
  if (btn.dataset.saveMode === mode) return;
  btn.dataset.saveMode = mode;

  btn.classList.toggle('is-saved', mode === 'saved');
  check?.classList.toggle('is-hidden', mode !== 'saved');
  btn.disabled = mode === 'saving' || mode === 'saved' || mode === 'idle';
  if (mode === 'saving') {
    label.textContent = 'Saving…';
    btn.setAttribute('aria-label', 'Saving settings');
    return;
  }
  if (mode === 'failed') {
    label.textContent = 'Retry save';
    btn.setAttribute('aria-label', 'Settings could not be saved; retry');
    return;
  }
  if (mode === 'saved') {
    label.textContent = 'Saved';
    btn.setAttribute('aria-label', 'Settings saved');
    return;
  }
  label.textContent = 'Save';
  btn.setAttribute('aria-label', dirty ? 'Save settings' : 'No changes to save');
}

function openSettings() {
  fillSettingsFormFromState();
  setCapabilityAdvancedOpen(
    document.getElementById('btnWebSearchAdvanced'),
    document.getElementById('webSearchOptions'),
    false
  );
  setCapabilityAdvancedOpen(
    document.getElementById('btnFetchUrlAdvanced'),
    document.getElementById('fetchUrlOptions'),
    false
  );
  refreshLocalDataPane();
  showSettingsPane('personalization');
  openBackdrop(settingsModal);
  syncSettingsSaveButton();
  document.getElementById('settingName').focus();
  loadUserSkills();
}

function setCapabilityAdvancedOpen(toggle, panel, open) {
  if (!toggle || !panel) return;
  toggle.setAttribute('aria-expanded', open ? 'true' : 'false');
  panel.classList.toggle('is-collapsed', !open);
  panel.hidden = !open;
}

function syncWebSearchControls() {
  const enabled = document.getElementById('settingSkillWebSearch').checked;
  const options = document.getElementById('webSearchOptions');
  const toggle = document.getElementById('btnWebSearchAdvanced');
  const provider = document.getElementById('settingWebSearchProvider')?.value || 'auto';
  if (toggle) toggle.disabled = !enabled;
  if (!options) return;
  options.querySelectorAll('input, select').forEach((control) => {
    control.disabled = !enabled;
  });
  options.querySelectorAll('[data-web-search-provider]').forEach((row) => {
    const tokens = String(row.dataset.webSearchProvider || '')
      .split(/\s+/)
      .filter(Boolean);
    row.hidden = !tokens.includes(provider);
  });
  options.style.opacity = enabled ? '' : '0.55';
}

function syncFetchUrlControls() {
  const enabled = document.getElementById('settingSkillFetchUrl').checked;
  const options = document.getElementById('fetchUrlOptions');
  const toggle = document.getElementById('btnFetchUrlAdvanced');
  if (toggle) toggle.disabled = !enabled;
  if (!options) return;
  options.querySelectorAll('input, select').forEach((control) => {
    control.disabled = !enabled;
  });
  options.style.opacity = enabled ? '' : '0.55';
}

function syncTerminalSkillControls() {
  const enabled = document.getElementById('settingSkillTerminal').checked;
  const options = document.getElementById('terminalOptions');
  const toggle = document.getElementById('btnTerminalAdvanced');
  if (toggle) toggle.disabled = !enabled;
  if (!options) return;
  options.querySelectorAll('input, select').forEach((control) => {
    control.disabled = !enabled;
  });
  options.style.opacity = enabled ? '' : '0.55';
}

function syncApprovalModeControls() {
  const mode = APPROVAL_MODES.includes(settings.approvalMode)
    ? settings.approvalMode
    : 'manual';
  document.querySelectorAll('#approvalModeToggle [data-approval-mode]').forEach((btn) => {
    const on = btn.dataset.approvalMode === mode;
    btn.classList.toggle('is-active', on);
    btn.setAttribute('aria-selected', on ? 'true' : 'false');
  });
}

function syncAttachmentFallbackControls() {
  const fallback = document.getElementById('settingAttachmentTextFallback');
  const ocr = document.getElementById('settingAttachmentOcr');
  const maxChars = document.getElementById('settingAttachmentMaxChars');
  const enabled = !!(fallback && fallback.checked);
  if (ocr) ocr.disabled = !enabled;
  if (maxChars) maxChars.disabled = !enabled;
  if (ocr) ocr.closest('.field-check').style.opacity = enabled ? '' : '0.55';
  if (maxChars) {
    const row = maxChars.closest('.settings-row');
    if (row) row.style.opacity = enabled ? '' : '0.55';
  }
}

function closeSettings() {
  closeBackdrop(settingsModal);
  hideSkillEditor();
}

function showSkillsError(error) {
  const el = document.getElementById('skillsError');
  if (!el) return;
  el.textContent = error?.message || String(error || '');
  el.classList.toggle('is-hidden', !el.textContent);
}

function hideSkillEditor() {
  const editor = document.getElementById('skillEditor');
  if (!editor) return;
  editor.classList.add('is-hidden');
  document.getElementById('skillEditId').value = '';
  showSkillsError('');
}

function openSkillEditor(skill) {
  const editor = document.getElementById('skillEditor');
  editor.classList.remove('is-hidden');
  document.getElementById('skillEditorTitle').textContent = skill?.id ? 'Edit skill' : 'New skill';
  document.getElementById('skillEditId').value = skill?.id || '';
  document.getElementById('skillEditName').value = skill?.name || '';
  document.getElementById('skillEditDescription').value = skill?.description || '';
  document.getElementById('skillEditContent').value = skill?.content || '';
  document.getElementById('skillEditEnabled').checked = skill?.enabled !== false;
  showSkillsError('');
  document.getElementById('skillEditName').focus();
  editor.scrollIntoView({ block: 'nearest' });
}

function renderUserSkills() {
  const list = document.getElementById('skillsList');
  const empty = document.getElementById('skillsEmpty');
  if (!list || !empty) return;
  empty.classList.toggle('is-hidden', userSkills.length > 0);
  list.innerHTML = userSkills.map((skill) => `
    <li data-skill-id="${escapeHtml(skill.id)}">
      <div class="skill-head">
        <div>
          <div class="skill-title">${escapeHtml(skill.name)}</div>
          <p class="skill-desc">${escapeHtml(skill.description || 'No description')}</p>
        </div>
        <label class="field-check" style="margin:0">
          <input type="checkbox" data-skill-enabled ${skill.enabled ? 'checked' : ''}>
          <span style="font-size:var(--text-xs)">On</span>
        </label>
      </div>
      <div class="skill-meta">${skill.content_chars || 0} chars${skill.source_filename ? ' · ' + escapeHtml(skill.source_filename) : ''}</div>
      <div class="skill-actions">
        <button type="button" class="btn btn-outline" data-skill-edit>Edit</button>
        <button type="button" class="btn btn-outline" data-skill-delete>Delete</button>
      </div>
    </li>
  `).join('');
}

async function loadUserSkills() {
  try {
    const data = await fetch('/api/skills').then(async (response) => {
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || 'Could not load skills');
      return body;
    });
    userSkills = data.skills || [];
    renderUserSkills();
  } catch (error) {
    userSkills = [];
    renderUserSkills();
    showSkillsError(error);
  }
}

async function withBusyControl(el, busyLabel, work, opts = {}) {
  if (!el || el.dataset.busy === '1') return;
  const restore = opts.restore !== false;
  const prevText = el.textContent;
  const prevDisabled = el.disabled;
  el.dataset.busy = '1';
  el.disabled = true;
  if (busyLabel != null) el.textContent = busyLabel;
  el.setAttribute('aria-busy', 'true');
  try {
    return await work();
  } finally {
    if (!el.isConnected) return;
    delete el.dataset.busy;
    el.removeAttribute('aria-busy');
    el.disabled = prevDisabled;
    if (busyLabel != null && restore) el.textContent = prevText;
  }
}

async function saveSkillFromEditor() {
  const id = document.getElementById('skillEditId').value.trim();
  const payload = {
    name: document.getElementById('skillEditName').value.trim(),
    description: document.getElementById('skillEditDescription').value.trim(),
    content: document.getElementById('skillEditContent').value,
    enabled: document.getElementById('skillEditEnabled').checked,
  };
  const btn = document.getElementById('btnSkillSave');
  await withBusyControl(btn, 'Saving…', async () => {
    try {
      showSkillsError('');
      const response = await fetch(id ? '/api/skills/' + encodeURIComponent(id) : '/api/skills', {
        method: id ? 'PATCH' : 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(payload),
      });
      const body = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(body.error || 'Could not save skill');
      hideSkillEditor();
      await loadUserSkills();
    } catch (error) {
      showSkillsError(error);
    }
  });
}

async function importSkillFile(file) {
  const content = await file.text();
  const response = await fetch('/api/skills/import', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ content, filename: file.name }),
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error || 'Could not import skill');
  await loadUserSkills();
  openSkillEditor(body);
}

const THINKING_EFFORT_SUFFIX = {
  auto: '',
  off: 'off',
  low: 'low',
  medium: 'med',
  high: 'high',
  max: 'max',
};
let thinkMenuCloseTimer = 0;

function syncThinkingEffortControls(effort) {
  const value = THINKING_EFFORTS.includes(effort) ? effort : 'auto';
  const setting = document.getElementById('settingThinkingEffort');
  if (setting && setting.value !== value) setting.value = value;

  const btn = document.getElementById('btnThink');
  const effortEl = document.getElementById('btnThinkEffort');
  const menu = document.getElementById('thinkMenu');
  const suffix = THINKING_EFFORT_SUFFIX[value] || '';
  if (effortEl) {
    if (suffix) {
      effortEl.textContent = suffix;
      effortEl.classList.remove('is-hidden');
      effortEl.classList.toggle('is-off', value === 'off');
    } else {
      effortEl.textContent = '';
      effortEl.classList.add('is-hidden');
      effortEl.classList.remove('is-off');
    }
  }
  if (btn) {
    btn.classList.toggle('is-active', value !== 'auto' && value !== 'off');
    btn.title = 'Thinking intensity: ' + value + ' — how hard reasoning models should think';
  }
  if (menu) {
    menu.querySelectorAll('[data-effort]').forEach((item) => {
      item.classList.toggle('is-active', item.dataset.effort === value);
      item.setAttribute('aria-checked', item.dataset.effort === value ? 'true' : 'false');
    });
  }
}

function thinkMenuIsOpen() {
  const menu = document.getElementById('thinkMenu');
  return !!(menu && !menu.classList.contains('is-hidden') && menu.classList.contains('is-open'));
}

function setThinkMenuOpen(open) {
  const menu = document.getElementById('thinkMenu');
  const btn = document.getElementById('btnThink');
  if (!menu || !btn) return;
  if (thinkMenuCloseTimer) {
    window.clearTimeout(thinkMenuCloseTimer);
    thinkMenuCloseTimer = 0;
  }
  if (open) {
    setPlusMenuOpen(false);
    if (typeof setWordmarkMenuOpen === 'function') setWordmarkMenuOpen(false);
    menu.classList.remove('is-hidden');
    btn.setAttribute('aria-expanded', 'true');
    void menu.offsetWidth;
    requestAnimationFrame(() => menu.classList.add('is-open'));
    return;
  }
  menu.classList.remove('is-open');
  btn.setAttribute('aria-expanded', 'false');
  const finish = () => {
    menu.classList.add('is-hidden');
    thinkMenuCloseTimer = 0;
  };
  if (prefersReducedMotion()) {
    finish();
    return;
  }
  thinkMenuCloseTimer = window.setTimeout(finish, 180);
}

let plusMenuCloseTimer = 0;

const PLUS_CHECK_SVG =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M20 6 9 17l-5-5"/></svg>';

function plusMenuIcons() {
  return {
    attach:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m21.44 11.05-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48"/></svg>',
    search:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><circle cx="12" cy="12" r="9"/><path d="M12 3a14 14 0 0 1 0 18"/><path d="M12 3a14 14 0 0 0 0 18"/><path d="M3 12h18"/><path d="M3.6 8h16.8"/><path d="M3.6 16h16.8"/></svg>',
    fetch:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71"/><path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71"/></svg>',
    research:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m10.5 14.5-6 6"/><path d="m14 6 4 4"/><path d="M15.2 4.8a2.2 2.2 0 0 1 3.1 3.1L10.5 15.7 7.3 12.5Z"/><path d="M4 20h5"/><path d="M6.5 17.5v5"/></svg>',
    agent:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 8V4H8"/><rect width="16" height="12" x="4" y="8" rx="2"/><path d="M2 14h2M20 14h2M15 13v2M9 13v2"/></svg>',
    terminal:
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><polyline points="4 17 10 11 4 5"/><line x1="12" x2="20" y1="19" y2="19"/></svg>',
  };
}

function renderPlusMenuItem({ id, iconClass, icon, title, desc, descHtml, badge, on, disabled, titleAttr }) {
  const btn = document.createElement('button');
  btn.type = 'button';
  btn.className = 'plus-menu-item' + (on ? ' is-on' : '');
  btn.dataset.plusAction = id;
  btn.setAttribute('role', 'menuitem');
  if (disabled) btn.disabled = true;
  if (titleAttr) btn.title = titleAttr;
  const descPart = descHtml
    ? '<span class="plus-menu-desc">' + descHtml + '</span>'
    : (desc
      ? '<span class="plus-menu-desc">' + escapeHtml(desc) + '</span>'
      : '');
  const badgePart = badge
    ? '<span class="plus-menu-badge">' + escapeHtml(badge) + '</span>'
    : '';
  btn.innerHTML =
    '<span class="plus-menu-icon ' + iconClass + '" aria-hidden="true">' + icon + '</span>' +
    '<span class="plus-menu-copy">' +
      '<span class="plus-menu-title">' + escapeHtml(title) + '</span>' +
      descPart +
    '</span>' +
    '<span class="plus-menu-trail">' +
      badgePart +
      '<span class="plus-menu-check" aria-hidden="true">' + PLUS_CHECK_SVG + '</span>' +
    '</span>';
  return btn;
}

function renderPlusMenu() {
  if (!plusMenu) return;
  const icons = plusMenuIcons();
  const attachEnabled = attachmentsUiEnabled() && serverReady && !diskEncryptionLocked();
  const attachReason = attachDisabledReason();
  const researchAllowed = settings.skillDeepResearch !== false;
  const agentOn = !!settings.agentMode;

  plusMenu.innerHTML = '';
  plusMenu.appendChild(renderPlusMenuItem({
    id: 'agent',
    iconClass: 'is-agent',
    icon: icons.agent,
    title: 'Agent mode',
    desc: agentOn
      ? 'All agent skills available when needed'
      : 'Unlocks all agent skills when needed',
    badge: 'Recommended',
    on: agentOn,
    disabled: false,
  }));
  plusMenu.appendChild(renderPlusMenuItem({
    id: 'attach',
    iconClass: 'is-attach',
    icon: icons.attach,
    title: 'Add photos & files',
    desc: attachEnabled ? 'Upload from computer' : (attachReason || 'Configure in Settings'),
    on: false,
    disabled: false,
    titleAttr: attachEnabled ? 'Attach files' : (attachReason || 'Go to Settings → Attachments'),
  }));
  if (settings.skillWebSearch !== false) {
    plusMenu.appendChild(renderPlusMenuItem({
      id: 'web_search',
      iconClass: 'is-search',
      icon: icons.search,
      title: agentOn ? 'Require web search' : 'Web search',
      desc: agentOn
        ? 'Must search before answering'
        : 'Keep enabled for this chat',
      on: composerMentionIds.has('web_search'),
      disabled: false,
    }));
  }
  if (settings.skillFetchUrl !== false) {
    plusMenu.appendChild(renderPlusMenuItem({
      id: 'fetch_url',
      iconClass: 'is-fetch',
      icon: icons.fetch,
      title: agentOn ? 'Require fetch URL' : 'Fetch URL',
      desc: agentOn
        ? 'Must open a page before answering'
        : 'Keep enabled for this chat',
      on: composerMentionIds.has('fetch_url'),
      disabled: false,
    }));
  }
  if (researchAllowed) {
    plusMenu.appendChild(renderPlusMenuItem({
      id: 'deep_research_long',
      iconClass: 'is-research',
      icon: icons.research,
      title: 'Deep research · comprehensive',
      desc: 'Detailed report from many sources',
      on: settings.deepResearch === 'long',
      disabled: false,
    }));
    plusMenu.appendChild(renderPlusMenuItem({
      id: 'deep_research_brief',
      iconClass: 'is-research',
      icon: icons.research,
      title: 'Deep research · concise',
      desc: 'Brief answer from many sources',
      on: settings.deepResearch === 'brief',
      disabled: false,
    }));
  }
  plusMenu.appendChild(renderPlusMenuItem({
    id: 'terminal',
    iconClass: 'is-agent',
    icon: icons.terminal,
    title: 'Terminal',
    desc: 'Open a command panel under the chat box',
    on: !!terminalOpen,
    disabled: false,
  }));
}

function renderComposerModes() {
  if (!composerModes) return;
  composerModes.innerHTML = '';
  if (settings.agentMode) {
    const chip = document.createElement('span');
    chip.className = 'composer-mode-chip is-agent-available';
    const forced = [];
    if (composerMentionIds.has('web_search')) forced.push('web_search');
    if (composerMentionIds.has('fetch_url')) forced.push('fetch_url');
    const label = forced.length
      ? 'Agent · required skills'
      : 'Agent · skills when needed';
    chip.innerHTML = '<span>' + escapeHtml(label) + '</span>';
    chip.title = forced.length
      ? 'Pinned skills are required this turn: ' + forced.join(', ')
      : 'Recommended — all agent skills are available; the model uses them when needed';
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.setAttribute('aria-label', 'Turn off agent mode');
    remove.textContent = '×';
    remove.addEventListener('click', () => {
      if (!requireUnlockedData()) return;
      settings.agentMode = false;
      saveSettings({ ...settings });
    });
    chip.appendChild(remove);
    composerModes.appendChild(chip);
  }
  if (
    settings.skillDeepResearch !== false
    && (settings.deepResearch === 'long' || settings.deepResearch === 'brief')
  ) {
    const chip = document.createElement('span');
    chip.className = 'composer-mode-chip';
    const style = settings.deepResearch === 'brief' ? 'concise' : 'comprehensive';
    chip.innerHTML = '<span>Research · ' + escapeHtml(style) + '</span>';
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.setAttribute('aria-label', 'Turn off deep research');
    remove.textContent = '×';
    remove.addEventListener('click', () => setDeepResearch('off', true));
    chip.appendChild(remove);
    composerModes.appendChild(chip);
  }
}

function plusMenuIsOpen() {
  return !!(plusMenu && !plusMenu.classList.contains('is-hidden') && plusMenu.classList.contains('is-open'));
}

function placePlusMenu() {
  if (!plusMenu || !composerCard) return;
  plusMenu.classList.remove('is-above');
  plusMenu.style.maxHeight = '';
  const gap = 10;
  const cardRect = composerCard.getBoundingClientRect();
  const spaceBelow = Math.max(0, window.innerHeight - cardRect.bottom - gap);
  const spaceAbove = Math.max(0, cardRect.top - gap);
  const natural = plusMenu.scrollHeight;
  const cap = Math.min(22 * 16, window.innerHeight - gap * 2);
  const openAbove = spaceBelow < Math.min(natural, 12 * 16) && spaceAbove > spaceBelow;
  if (openAbove) {
    plusMenu.classList.add('is-above');
    plusMenu.style.maxHeight = Math.min(cap, spaceAbove) + 'px';
  } else {
    plusMenu.style.maxHeight = Math.min(cap, Math.max(spaceBelow, 8 * 16)) + 'px';
  }
}

function setPlusMenuOpen(open) {
  if (!plusMenu || !btnPlus) return;
  if (plusMenuCloseTimer) {
    window.clearTimeout(plusMenuCloseTimer);
    plusMenuCloseTimer = 0;
  }
  if (open) {
    if (thinkMenuIsOpen()) setThinkMenuOpen(false);
    if (typeof setWordmarkMenuOpen === 'function') setWordmarkMenuOpen(false);
    renderPlusMenu();
    plusMenu.classList.remove('is-hidden');
    placePlusMenu();
    btnPlus.setAttribute('aria-expanded', 'true');
    btnPlus.title = 'Close';
    btnPlus.setAttribute('aria-label', 'Close');
    void plusMenu.offsetWidth;
    requestAnimationFrame(() => plusMenu.classList.add('is-open'));
    return;
  }
  plusMenu.classList.remove('is-open');
  btnPlus.setAttribute('aria-expanded', 'false');
  btnPlus.title = 'Add files and capabilities';
  btnPlus.setAttribute('aria-label', 'Add files and capabilities');
  const finish = () => {
    plusMenu.classList.add('is-hidden');
    plusMenu.classList.remove('is-above');
    plusMenu.style.maxHeight = '';
    plusMenuCloseTimer = 0;
  };
  if (prefersReducedMotion()) {
    finish();
    return;
  }
  plusMenuCloseTimer = window.setTimeout(finish, 180);
}

function toggleComposerMention(id) {
  if (composerMentionIds.has(id)) composerMentionIds.delete(id);
  else composerMentionIds.add(id);
  renderComposerMentions();
  renderComposerModes();
  syncPlusButton();
  renderPlusMenu();
  composerInput.focus();
}

function handlePlusMenuAction(action) {
  if (action === 'attach') {
    const enabled = attachmentsUiEnabled() && serverReady && !diskEncryptionLocked();
    setPlusMenuOpen(false);
    if (!enabled) {
      openSettings();
      showSettingsPane('attachments');
      return;
    }
    attachFileInput.click();
    return;
  }
  if (action === 'web_search') {
    if (settings.skillWebSearch === false) return;
    toggleComposerMention('web_search');
    setPlusMenuOpen(false);
    return;
  }
  if (action === 'fetch_url') {
    if (settings.skillFetchUrl === false) return;
    toggleComposerMention('fetch_url');
    setPlusMenuOpen(false);
    return;
  }
  if (action === 'deep_research_long') {
    if (settings.skillDeepResearch === false) return;
    setDeepResearch(settings.deepResearch === 'long' ? 'off' : 'long', true);
    setPlusMenuOpen(false);
    return;
  }
  if (action === 'deep_research_brief') {
    if (settings.skillDeepResearch === false) return;
    setDeepResearch(settings.deepResearch === 'brief' ? 'off' : 'brief', true);
    setPlusMenuOpen(false);
    return;
  }
  if (action === 'agent') {
    if (!requireUnlockedData()) return;
    settings.agentMode = !settings.agentMode;
    saveSettings({ ...settings });
    setPlusMenuOpen(false);
    return;
  }
  if (action === 'terminal') {
    setPlusMenuOpen(false);
    if (typeof toggleTerminalPanel === 'function') toggleTerminalPanel();
  }
}

function syncResearchControls() {
  const allowed = settings.skillDeepResearch !== false;
  if (!allowed && settings.deepResearch !== 'off') {
    settings.deepResearch = 'off';
  }
  renderComposerModes();
  renderPlusMenu();
  syncPlusButton();
}

function setDeepResearch(mode, persist) {
  if (settings.skillDeepResearch === false && mode !== 'off') return;
  const value = DEEP_RESEARCH_MODES.includes(mode) ? mode : 'off';
  settings.deepResearch = value;
  syncResearchControls();
  updateComposerHint();
  if (persist) {
    saveSettings({
      ...settings,
      deepResearch: value,
    });
  }
}

let thinkingSupported = false;
let activeThinkingEffort = 'auto';
let availableThinkingEfforts = new Set(['auto']);

function syncComposerThinkVisibility(model) {
  const efforts = Array.isArray(model?.thinking_efforts) ? model.thinking_efforts : [];
  const canDisable = !!model?.thinking_can_disable;
  thinkingSupported = !!model?.thinking_control && (efforts.length > 0 || canDisable);
  const allowed = (value) => value === 'auto'
    || (value === 'off' ? canDisable : efforts.includes(value));
  availableThinkingEfforts = new Set(THINKING_EFFORTS.filter(allowed));
  activeThinkingEffort = allowed(settings.thinkingEffort) ? settings.thinkingEffort : 'auto';
  const wrap = document.getElementById('composerThinkWrap');
  if (!wrap) return;
  wrap.classList.toggle('is-hidden', !thinkingSupported);
  if (!thinkingSupported) setThinkMenuOpen(false);
  document.querySelectorAll('#thinkMenu [data-effort]').forEach((item) => {
    item.classList.toggle('is-hidden', !allowed(item.dataset.effort));
  });
  const effort = document.getElementById('settingThinkingEffort');
  const effortRow = effort && effort.closest('.settings-row');
  if (effort) {
    effort.disabled = !thinkingSupported;
    effort.querySelectorAll('option').forEach((option) => {
      option.hidden = !allowed(option.value);
      option.disabled = !allowed(option.value);
    });
    if (effortRow) {
      effortRow.style.opacity = thinkingSupported ? '' : '0.55';
      effortRow.title = thinkingSupported
        ? ''
        : 'Current model does not expose controllable thinking.';
    }
  }
  syncThinkingEffortControls(activeThinkingEffort);
}

function setThinkingEffort(effort, persist = true) {
  const value = availableThinkingEfforts.has(effort) ? effort : 'auto';
  activeThinkingEffort = value;
  syncThinkingEffortControls(value);
  if (persist) {
    if (!requireUnlockedData()) {
      syncThinkingEffortControls(settings.thinkingEffort);
      return;
    }
    saveSettings({ ...settings, thinkingEffort: value });
  }
}

async function commitSettings() {
  if (!requireUnlockedData()) return;
  const backgroundUrl = document.getElementById('settingChatBackgroundUrl');
  if (pendingChatBackgroundImage && !normalizeChatBackgroundImage(pendingChatBackgroundImage)) {
    backgroundUrl?.reportValidity();
    backgroundUrl?.focus();
    return;
  }
  if (!settingsFormIsDirty()) {
    syncSettingsSaveButton({ saved: true });
    return;
  }
  const next = readSettingsForm();
  if (!next.skillDeepResearch) {
    next.deepResearch = 'off';
  }
  const previous = settings;
  syncSettingsSaveButton({ saving: true });
  const saved = await saveSettings(next, { immediate: true });
  if (!saved) {
    if (!diskEncryptionLocked()) {
      applySettingsInMemory(previous);
      syncSettingsSaveButton({ failed: true });
    }
    return;
  }
  if (typeof hydrateModelPickerState === 'function') hydrateModelPickerState();
  if (next.selectedChatModel) {
    rememberRecentModel(next.selectedChatModel);
    selectedRemoteModelId = next.selectedChatModel;
  }
  if (latestState && typeof syncModelSelector === 'function') syncModelSelector(latestState);
  syncThinkingEffortControls(settings.thinkingEffort);
  syncResearchControls();
  syncAttachButton();
  syncSettingsSaveButton({ saved: true });
  const convo = conversations.find((item) => item.id === activeId);
  if (convo) renderThread(convo);
}
