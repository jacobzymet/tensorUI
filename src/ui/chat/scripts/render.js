function stripThinkingTags(text) {
  return text
    .replace(/<(?:think|thinking)>[\s\S]*?<\/(?:think|thinking)>/gi, '')
    .replace(/<\/?(?:think|thinking)>/gi, '')
    .trim();
}

/** Split assistant text on <think>/<thinking> tags (incomplete open tags allowed while streaming). */
function parseThinkSegments(text) {
  const segments = [];
  const re = /<\/?(?:think|thinking)>/gi;
  let last = 0;
  let inThink = false;
  let match;
  while ((match = re.exec(text)) !== null) {
    const chunk = text.slice(last, match.index);
    if (chunk) segments.push({ type: inThink ? 'think' : 'text', content: chunk, open: false });
    inThink = !match[0].startsWith('</');
    last = match.index + match[0].length;
  }
  const tail = text.slice(last);
  if (tail || inThink) {
    segments.push({ type: inThink ? 'think' : 'text', content: tail, open: inThink });
  }
  return segments;
}

function renderThinkMarkdown(content) {
  return renderMarkdown(String(content || '').replace(/\s+$/, ''));
}

const THINK_CHEVRON =
  '<svg class="think-chevron" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.25" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="m6 9 6 6 6-6"/></svg>';

function agentStepRailHtml() {
  return (
    '<div class="agent-step-rail">' +
      '<span class="agent-step-dot" aria-hidden="true">' +
        '<svg class="agent-step-check" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.75" stroke-linecap="round" stroke-linejoin="round">' +
          '<path d="M20 6 9 17l-5-5"/>' +
        '</svg>' +
        '<span class="agent-step-spinner"></span>' +
      '</span>' +
    '</div>'
  );
}

function wrapTimelineStep(innerHtml, { think = false, live = false, done = false } = {}) {
  const classes = ['agent-step'];
  if (think) classes.push('is-think');
  if (live) classes.push('is-live');
  if (done) classes.push('is-done');
  return (
    '<div class="' + classes.join(' ') + '">' +
      agentStepRailHtml() +
      innerHtml +
    '</div>'
  );
}

function renderThinkBlock(content, { open, streaming, forceOpen = false } = {}) {
  const mode = settings.thinking;
  if (mode === 'hidden') return '';
  const live = streaming && open;
  const md = renderThinkMarkdown(content);
  // Live ticker stays chrome-less unless the sidebar forces a details shell.
  if (live && mode !== 'visible' && !forceOpen) {
    return wrapTimelineStep(
      '<div class="think-live" aria-label="Reasoning progress">' +
        '<div class="think-stream">' + md + '</div>' +
      '</div>',
      { think: true, live: true }
    );
  }
  // Sidebar prefers expanded reasoning; main bubble still respects collapse setting.
  const detailsOpen = forceOpen || mode === 'visible' || (mode === 'collapsed' && live);
  const summary = live
    ? '<span class="think-summary-label thinking-label">Reasoning</span>' + THINK_CHEVRON
    : '<span class="think-summary-label">Reasoning</span>' + THINK_CHEVRON;
  const body = live
    ? '<div class="think-stream">' + md + '</div>'
    : '<div class="think-body">' + md + '</div>';
  const details =
    '<details class="think-block' + (live ? ' is-streaming' : '') + '"' +
    (detailsOpen ? ' open' : '') + '>' +
    '<summary>' + summary + '</summary>' +
    body +
    '</details>';
  return wrapTimelineStep(details, { think: true, live, done: !live });
}

function renderCommittedParts(parts, { forceOpenThinking = false } = {}) {
  if (!parts || !parts.length) return '';
  let html = '';
  for (const part of parts) {
    if (!part) continue;
    if (part.type === 'think') {
      if (!String(part.content || '').trim()) continue;
      html += renderThinkBlock(part.content, {
        open: false,
        streaming: false,
        forceOpen: forceOpenThinking,
      });
    } else if (part.type === 'tool') {
      html += agentStepHtml(part);
    } else if (part.type === 'clarify') {
      const header = 'Clarifying questions';
      const detail = part.live
        ? 'Waiting for your answers'
        : (part.summary || 'Goal refined');
      html += wrapTimelineStep(
        '<div class="agent-step-card">' +
          '<div class="agent-step-kind">' + escapeHtml(header) + '</div>' +
          '<div class="agent-step-detail">' + escapeHtml(detail) + '</div>' +
        '</div>',
        { live: !!part.live, done: !part.live }
      );
    } else if (part.type === 'notice') {
      const text = String(part.content || '').trim();
      if (!text) continue;
      const toneClass = part.tone === 'ok' ? ' is-ok' : '';
      html +=
        '<div class="agent-step is-notice is-done' + toneClass + '">' +
          agentStepRailHtml() +
          '<div class="agent-step-card"><div class="agent-step-notice">' +
            escapeHtml(text) +
          '</div></div>' +
        '</div>';
    }
  }
  return html;
}

function splitSealedTextParts(parts) {
  const list = Array.isArray(parts) ? parts : [];
  let lastToolIndex = -1;
  for (let i = 0; i < list.length; i++) {
    if (list[i]?.type === 'tool') lastToolIndex = i;
  }
  const processNotes = [];
  const answerChunks = [];
  for (let i = 0; i < list.length; i++) {
    const part = list[i];
    if (!part || part.type !== 'text') continue;
    const chunk = String(part.content || '');
    if (!chunk.trim()) continue;
    if (lastToolIndex >= 0 && i < lastToolIndex) {
      processNotes.push(chunk);
    } else {
      answerChunks.push(chunk);
    }
  }
  return { processNotes, answerChunks };
}

function renderProcessNotesFold(notes, { collapsed = true } = {}) {
  if (!notes || !notes.length) return '';
  const label = notes.length === 1
    ? '1 note while working'
    : (notes.length + ' notes while working');
  const body = notes.map((note) => (
    '<div class="agent-process-note">' + renderMarkdown(note) + '</div>'
  )).join('');
  return (
    '<div class="agent-timeline-fold agent-process-notes' + (collapsed ? ' is-collapsed' : '') + '">' +
      '<button type="button" class="agent-timeline-fold-toggle" aria-expanded="' +
        (collapsed ? 'false' : 'true') + '">' +
        '<span>' + escapeHtml(label) + '</span>' +
        THINK_CHEVRON +
      '</button>' +
      '<div class="agent-timeline-fold-panel">' +
        '<div class="agent-timeline-fold-inner">' + body + '</div>' +
      '</div>' +
    '</div>'
  );
}

function renderSealedAnswerHtml(parts, { notesCollapsed = false } = {}) {
  const { processNotes, answerChunks } = splitSealedTextParts(parts);
  const notesHtml = renderProcessNotesFold(processNotes, { collapsed: notesCollapsed });
  let text = '';
  for (const chunk of answerChunks) {
    if (text) text += '\n\n';
    text += chunk;
  }
  const answerHtml = text.trim()
    ? '<div class="agent-final-answer">' + renderMarkdown(text) + '</div>'
    : '';
  return notesHtml + answerHtml;
}

function ensureSealedTimelineText(stream, text) {
  const sealed = String(text || '').trim();
  if (!sealed) return;
  const texts = stream.timeline.filter((part) => part && part.type === 'text');
  const last = texts[texts.length - 1];
  if (last && String(last.content || '').trim() === sealed) return;
  if (last && sealed.startsWith(String(last.content || '').trim())) {
    last.content = sealed;
    return;
  }
  stream.timeline.push({ type: 'text', content: sealed });
}

function ensureSealedTimelineThink(stream, reasoning) {
  const sealed = String(reasoning || '').trim();
  if (!sealed) return;
  const thinks = stream.timeline.filter((part) => part && part.type === 'think');
  const last = thinks[thinks.length - 1];
  if (last && String(last.content || '').trim() === sealed) return;
  if (last && sealed.startsWith(String(last.content || '').trim())) {
    last.content = sealed;
    return;
  }
  stream.timeline.push({ type: 'think', content: sealed });
}

/** Move the current typer buffer into stream.timeline, then clear the buffer. */
function commitStreamBuffer(stream, typer) {
  let text = typer.target || '';
  if (!String(text).trim()) {
    typer.clear();
    stream.partial = '';
    return false;
  }
  if (isThinkingOpen(text)) text += '</think>';
  const { cleaned } = applyMemoryUpdateProtocol(text, { streaming: false });
  const segments = parseThinkSegments(cleaned || text);
  for (const segment of segments) {
    const content = String(segment.content || '');
    if (!content.trim()) continue;
    if (segment.type === 'think') {
      stream.timeline.push({ type: 'think', content });
    } else {
      stream.timeline.push({ type: 'text', content });
    }
  }
  typer.clear();
  stream.partial = '';
  return true;
}

function isDesktopTraceLayout() {
  return TRACE_DESKTOP_MQ.matches;
}

function timelinePartCount(parts) {
  if (!Array.isArray(parts)) return 0;
  return parts.reduce((count, part) => {
    if (!part) return count;
    if (part.type === 'think' || part.type === 'tool' || part.type === 'clarify' || part.type === 'notice') return count + 1;
    return count;
  }, 0);
}

function isDeferredMemoryNotice(part) {
  // Memory notices are stamped at turn end; steer notices must stay in-sequence.
  if (!part || part.type !== 'notice') return false;
  if (part.kind === 'steer') return false;
  if (part.kind === 'memory') return true;
  // Legacy saves: tone:ok without kind, excluding old "Steered:" rows.
  if (part.tone === 'ok' && !/^steered:/i.test(String(part.content || '').trim())) return true;
  return false;
}

function collectTimelineAndAnswer(message) {
  const timelineParts = [];
  const deferredNotices = [];
  let answerText = '';
  const pushAnswer = (chunk) => {
    const text = String(chunk || '');
    if (!text.trim()) return;
    if (answerText) answerText += '\n\n';
    answerText += text;
  };
  const { processNotes, answerChunks } = splitSealedTextParts(message?.parts);
  for (const part of message?.parts || []) {
    if (!part) continue;
    if (isDeferredMemoryNotice(part)) {
      deferredNotices.push(part);
      continue;
    }
    if (part.type === 'think' || part.type === 'tool' || part.type === 'clarify' || part.type === 'notice') {
      timelineParts.push(part);
    }
  }
  for (const chunk of answerChunks) pushAnswer(chunk);
  const content = typeof message?.content === 'string' ? message.content : '';
  if (content) {
    for (const segment of parseThinkSegments(content)) {
      if (segment.type === 'think') {
        if (String(segment.content || '').trim()) {
          timelineParts.push({ type: 'think', content: segment.content });
        }
      } else {
        pushAnswer(segment.content);
      }
    }
  }
  for (const notice of deferredNotices) timelineParts.push(notice);
  return {
    timelineParts,
    processNotes,
    processNotesHtml: renderProcessNotesFold(processNotes, { collapsed: true }),
    answerText,
    answerHtml: answerText ? renderMarkdown(answerText) : '',
    stepCount: timelinePartCount(timelineParts),
  };
}

function messageHasActivity(message) {
  return collectTimelineAndAnswer(message).stepCount > 0;
}

function renderTimelineFold(timelineHtml, stepCount, { collapsed = true } = {}) {
  const label = stepCount === 1
    ? '1 activity step'
    : (stepCount + ' activity steps');
  return (
    '<div class="agent-timeline-fold' + (collapsed ? ' is-collapsed' : '') + '">' +
      '<button type="button" class="agent-timeline-fold-toggle" aria-expanded="' +
        (collapsed ? 'false' : 'true') + '">' +
        '<span>' + escapeHtml(label) + '</span>' +
        THINK_CHEVRON +
      '</button>' +
      '<div class="agent-timeline-fold-panel">' +
        '<div class="agent-timeline-fold-inner">' + timelineHtml + '</div>' +
      '</div>' +
    '</div>'
  );
}

function renderMemoryNoticesHtml(message) {
  const notices = Array.isArray(message?.memoryNotices)
    ? message.memoryNotices.map((item) => String(item || '').trim()).filter(Boolean)
    : [];
  if (!notices.length) return '';
  return (
    '<div class="msg-memory-notices">' +
      notices.map((label) => (
        '<div class="msg-memory-notice">' + escapeHtml(label) + '</div>'
      )).join('') +
    '</div>'
  );
}

function renderAssistantMessage(message, { streaming = false, collapseTimeline = null } = {}) {
  const memoryHtml = renderMemoryNoticesHtml(message);
  if (streaming) {
    const partsHtml = renderCommittedParts(message.parts);
    const sealedHtml = renderSealedAnswerHtml(message.parts, { notesCollapsed: false });
    const bodyHtml = message.content
      ? renderAssistantHtml(message.content, { streaming: true })
      : '';
    return partsHtml + sealedHtml + bodyHtml + memoryHtml;
  }

  const { timelineParts, answerHtml, stepCount, processNotesHtml } =
    collectTimelineAndAnswer(message);
  const timelineHtml = renderCommittedParts(timelineParts);
  const answerBlock = answerHtml
    ? '<div class="agent-final-answer">' + answerHtml + '</div>'
    : '';
  const finalHtml = (processNotesHtml || '') + answerBlock + memoryHtml;

  if (!timelineHtml) {
    if (processNotesHtml || memoryHtml) return finalHtml;
    return message.content
      ? renderAssistantHtml(message.content, { streaming: false }) + memoryHtml
      : finalHtml;
  }

  if (isDesktopTraceLayout()) {
    if (finalHtml) return finalHtml;
    if (message?.error) {
      return '<div class="agent-final-answer msg-error">' + escapeHtml(message.error) + '</div>';
    }
    return '<div class="agent-final-answer msg-error">No response.</div>';
  }

  const collapsed = collapseTimeline !== null ? !!collapseTimeline : true;
  return renderTimelineFold(timelineHtml, stepCount, { collapsed }) + finalHtml;
}

function settleAssistantRow(row, message, { animateCollapse = false } = {}) {
  if (!row || !message) return;
  const bubble = row.querySelector('.msg-bubble');
  if (!bubble) return;
  const collapseTimeline = animateCollapse ? false : null;
  bubble.innerHTML = renderAssistantMessage(message, {
    streaming: false,
    collapseTimeline,
  });
  enhanceCodeBlocks(bubble);
  bubble.querySelectorAll('.think-body, .agent-timeline-fold-inner').forEach((el) => {
    enhanceCodeBlocks(el);
  });
  if (animateCollapse && !isDesktopTraceLayout()) {
    const fold = bubble.querySelector('.agent-timeline-fold');
    if (fold) {
      fold.classList.remove('is-collapsed');
      const toggle = fold.querySelector('.agent-timeline-fold-toggle');
      if (toggle) toggle.setAttribute('aria-expanded', 'true');
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          fold.classList.add('is-collapsed');
          if (toggle) toggle.setAttribute('aria-expanded', 'false');
        });
      });
    }
  }
  attachMessageMeta(row, message);
}

function syncTraceSelectionClasses() {
  if (!chatThread) return;
  chatThread.querySelectorAll('.msg-role-assistant').forEach((row) => {
    const idx = Number(row.dataset.msgIndex);
    row.classList.toggle(
      'is-trace-selected',
      selectedTraceMsgIndex != null && idx === selectedTraceMsgIndex
    );
  });
}

function isTraceSidebarNearBottom() {
  if (!traceSidebarBody) return true;
  const gap = traceSidebarBody.scrollHeight - traceSidebarBody.scrollTop - traceSidebarBody.clientHeight;
  return gap <= 48;
}

function scrollTraceSidebarToBottom({ force = false } = {}) {
  if (!traceSidebarBody) return;
  if (!force && !stickTraceSidebar) return;
  traceSidebarBody.scrollTop = traceSidebarBody.scrollHeight;
  scrollThinkStreams(traceSidebarBody);
}

function paintTraceSidebarContent(message, { live = false } = {}) {
  if (!traceSidebarBody) return;
  const wasNearBottom = isTraceSidebarNearBottom();
  const title = traceSidebarTitle;
  if (!message) {
    if (title && title.textContent !== 'Activity') title.textContent = 'Activity';
    if (!traceSidebarBody.querySelector('.trace-sidebar-empty')) {
      traceSidebarBody.innerHTML =
        '<p class="trace-sidebar-empty">Select an assistant reply to inspect reasoning and tool steps.</p>';
    }
    traceSidebar?.classList.add('is-empty');
    delete traceSidebarBody.dataset.sidebarSig;
    return;
  }
  let timelineHtml = '';
  let stepCount = 0;
  if (live && Array.isArray(message.parts)) {
    // Memory notices land on the timeline at turn end; keep them after live reasoning.
    // Steer notices stay in chronological order.
    const earlyParts = [];
    const deferredNotices = [];
    for (const part of message.parts) {
      if (!part) continue;
      if (isDeferredMemoryNotice(part)) deferredNotices.push(part);
      else earlyParts.push(part);
    }
    timelineHtml = renderCommittedParts(earlyParts, { forceOpenThinking: true });
    stepCount = timelinePartCount(earlyParts);
    const liveSegments = message.content ? parseThinkSegments(message.content) : [];
    const openThink = liveSegments.find((seg) => seg.type === 'think' && seg.open);
    const closedThinks = liveSegments.filter(
      (seg) => seg.type === 'think' && !seg.open && String(seg.content || '').trim()
    );
    for (const segment of closedThinks) {
      timelineHtml += renderThinkBlock(segment.content, {
        open: false,
        streaming: false,
        forceOpen: true,
      });
      stepCount += 1;
    }
    if (deferredNotices.length) {
      timelineHtml += renderCommittedParts(deferredNotices, { forceOpenThinking: true });
      stepCount += timelinePartCount(deferredNotices);
    }
    if (title) {
      const totalSteps = stepCount + (openThink ? 1 : 0);
      const nextTitle = live
        ? (totalSteps ? ('Live Activity · ' + totalSteps) : 'Live Activity')
        : (totalSteps ? ('Activity · ' + totalSteps) : 'Activity');
      if (title.textContent !== nextTitle) title.textContent = nextTitle;
    }
    if (!timelineHtml.trim() && !openThink) {
      if (!traceSidebarBody.querySelector('.trace-sidebar-empty')) {
        traceSidebarBody.innerHTML =
          '<p class="trace-sidebar-empty">This reply has no reasoning or tool activity.</p>';
      }
      traceSidebar?.classList.add('is-empty');
      delete traceSidebarBody.dataset.sidebarSig;
      return;
    }
    traceSidebar?.classList.remove('is-empty');

    const committedSig =
      'live\0' +
      timelineSignature(earlyParts) +
      '\0' +
      closedThinks.map((s) => s.content).join('\0') +
      '\0' +
      timelineSignature(deferredNotices);

    if (openThink) {
      if (
        traceSidebarBody.dataset.sidebarSig !== committedSig ||
        !traceSidebarBody.querySelector(':scope > .agent-step.is-live-think')
      ) {
        traceSidebarBody.innerHTML =
          timelineHtml +
          wrapTimelineStep(
            '<details class="think-block is-streaming" open>' +
              '<summary>' +
                '<span class="think-summary-label thinking-label">Reasoning</span>' +
                THINK_CHEVRON +
              '</summary>' +
              '<div class="think-stream"></div>' +
            '</details>',
            { think: true, live: true }
          );
        const liveStep = traceSidebarBody.querySelector(':scope > .agent-step:last-child');
        if (liveStep) liveStep.classList.add('is-live-think');
        traceSidebarBody.dataset.sidebarSig = committedSig;
        enhanceCodeBlocks(traceSidebarBody);
        traceSidebarBody.querySelectorAll('.think-body').forEach((el) => enhanceCodeBlocks(el));
      }
      const streamEl = traceSidebarBody.querySelector(
        ':scope > .agent-step.is-live-think .think-stream, :scope > .agent-step.is-think.is-live .think-stream'
      );
      if (streamEl) {
        if (streamEl.dataset.thinkRaw !== openThink.content) {
          streamEl.dataset.thinkRaw = openThink.content;
          streamEl.innerHTML = renderThinkMarkdown(openThink.content);
        }
        streamEl.scrollTop = streamEl.scrollHeight;
      }
    } else {
      if (traceSidebarBody.dataset.sidebarSig !== committedSig) {
        traceSidebarBody.innerHTML = timelineHtml;
        traceSidebarBody.dataset.sidebarSig = committedSig;
        enhanceCodeBlocks(traceSidebarBody);
        traceSidebarBody.querySelectorAll('.think-body').forEach((el) => enhanceCodeBlocks(el));
      }
    }
    if (stickTraceSidebar || wasNearBottom) {
      stickTraceSidebar = true;
      scrollTraceSidebarToBottom({ force: true });
    }
    return;
  } else {
    const collected = collectTimelineAndAnswer(message);
    timelineHtml = renderCommittedParts(collected.timelineParts, { forceOpenThinking: true });
    stepCount = collected.stepCount;
    if (title) {
      const nextTitle = stepCount ? ('Activity · ' + stepCount) : 'Activity';
      if (title.textContent !== nextTitle) title.textContent = nextTitle;
    }
    if (!timelineHtml.trim()) {
      if (!traceSidebarBody.querySelector('.trace-sidebar-empty')) {
        traceSidebarBody.innerHTML =
          '<p class="trace-sidebar-empty">This reply has no reasoning or tool activity.</p>';
      }
      traceSidebar?.classList.add('is-empty');
      delete traceSidebarBody.dataset.sidebarSig;
      return;
    }
    traceSidebar?.classList.remove('is-empty');
    const staticSig = 'static\0' + timelineSignature(collected.timelineParts);
    if (traceSidebarBody.dataset.sidebarSig !== staticSig) {
      traceSidebarBody.innerHTML = timelineHtml;
      traceSidebarBody.dataset.sidebarSig = staticSig;
      enhanceCodeBlocks(traceSidebarBody);
      traceSidebarBody.querySelectorAll('.think-body').forEach((el) => enhanceCodeBlocks(el));
    }
    if (stickTraceSidebar || wasNearBottom) {
      stickTraceSidebar = true;
      scrollTraceSidebarToBottom({ force: true });
    }
  }
}

function refreshTraceSidebar({ animate = false } = {}) {
  if (!isDesktopTraceLayout() || !traceSidebarBody) return;
  const convo = conversations.find((item) => item.id === activeId);
  let message = null;
  let live = false;
  if (convo && selectedTraceMsgIndex != null) {
    const stream = activeStreams.get(convo.id);
    const streamingThis =
      stream
      && Number(stream.dom?.row?.dataset?.msgIndex) === selectedTraceMsgIndex;
    if (streamingThis) {
      live = true;
      message = {
        parts: stream.timeline,
        content: stream.partial || '',
      };
    } else {
      message = convo.messages[selectedTraceMsgIndex] || null;
      if (!message || message.role !== 'assistant') message = null;
    }
  }
  const paint = () => paintTraceSidebarContent(message, { live });
  if (!animate) {
    paint();
    return;
  }
  traceSidebarBody.classList.add('is-swap');
  window.clearTimeout(traceSidebarSwapTimer);
  traceSidebarSwapTimer = window.setTimeout(() => {
    paint();
    requestAnimationFrame(() => {
      traceSidebarBody.classList.remove('is-swap');
    });
  }, 140);
}

function selectTraceMessage(index, { animate = true, ensureOpen = false } = {}) {
  const next = Number.isFinite(index) ? Number(index) : null;
  const changed = selectedTraceMsgIndex !== next;
  selectedTraceMsgIndex = next;
  syncTraceSelectionClasses();
  if (changed) stickTraceSidebar = true;
  if (ensureOpen && isDesktopTraceLayout() && messageHasActivity(
    conversations.find((item) => item.id === activeId)?.messages?.[next]
  )) {
    maybeAutoOpenTraceSidebar(activeId);
  }
  refreshTraceSidebar({ animate: animate && changed });
}

function setTraceSidebarOpen(open, { fromUser = false } = {}) {
  if (!chatShell) return;
  if (fromUser) {
    traceUserCollapsed = !open;
    if (open) traceAutoOpenedForStream = activeId;
  }
  chatShell.classList.toggle('trace-collapsed', !open);
  if (btnToggleTrace) {
    btnToggleTrace.setAttribute('aria-expanded', open ? 'true' : 'false');
    btnToggleTrace.setAttribute('aria-label', open ? 'Hide activity sidebar' : 'Show activity sidebar');
    btnToggleTrace.title = open ? 'Hide activity' : 'Show activity';
  }
  if (btnExpandTrace) {
    btnExpandTrace.setAttribute('aria-expanded', open ? 'true' : 'false');
  }
  if (open && stickTraceSidebar) {
    requestAnimationFrame(() => scrollTraceSidebarToBottom({ force: true }));
  }
}

function maybeAutoOpenTraceSidebar(convoId) {
  if (!isDesktopTraceLayout() || !convoId) return;
  if (traceUserCollapsed) return;
  if (traceAutoOpenedForStream === convoId) return;
  if (!chatShell.classList.contains('trace-collapsed')) {
    traceAutoOpenedForStream = convoId;
    return;
  }
  traceAutoOpenedForStream = convoId;
  stickTraceSidebar = true;
  setTraceSidebarOpen(true);
}

function resetTraceAutoOpenState() {
  traceAutoOpenedForStream = null;
  traceUserCollapsed = false;
  stickTraceSidebar = true;
}

function initTraceSidebarPreferred() {
  // Always start closed; open only when reasoning / tool activity begins.
  setTraceSidebarOpen(false);
}

function scrollThinkStreams(root) {
  if (!root) return;
  root.querySelectorAll('.think-stream').forEach((el) => {
    el.scrollTop = el.scrollHeight;
  });
}

function isThinkingOpen(text) {
  if (!text) return false;
  return parseThinkSegments(text).some((segment) => segment.type === 'think' && segment.open);
}

function skillLabel(name, args) {
  if (name === 'web_search') {
    return String(args?.kind || '').toLowerCase() === 'news' ? 'News search' : 'Web search';
  }
  if (name === 'fetch_url') return 'Fetch URL';
  if (name === 'ask_user') return 'Clarifying questions';
  if (name === 'activate_skill' || name === 'read_skill') return 'Activate skill';
  return name || 'Skill';
}

function findClarifyPart(stream, id) {
  return (stream?.timeline || []).find((part) => part.type === 'clarify' && part.id === id) || null;
}

function buildClarifyFormHtml(part) {
  const done = !part.live || !!part.answers;
  const questions = Array.isArray(part.questions) ? part.questions : [];
  if (done && part.summary) {
    return (
      '<div class="clarify-card is-done" data-clarify-id="' + escapeHtml(part.id) + '">' +
        '<div class="clarify-card-head">' +
          '<span class="clarify-card-kicker">Deep research</span>' +
          '<span class="clarify-card-title">Goal refined</span>' +
        '</div>' +
        '<div class="clarify-summary">' + escapeHtml(part.summary).replace(/\n/g, '<br>') + '</div>' +
      '</div>'
    );
  }
  let body = '';
  questions.forEach((q, qi) => {
    const multi = !!q.multiSelect;
    const inputType = multi ? 'checkbox' : 'radio';
    const group = 'clarify-' + part.id + '-' + qi;
    const selected = part.draft?.[qi] || { labels: [], custom: '', useCustom: false };
    const options = Array.isArray(q.options) ? q.options : [];
    let opts = '';
    options.forEach((opt, oi) => {
      const label = String(opt.label || '');
      const checked = !selected.useCustom && selected.labels.includes(label);
      opts +=
        '<label class="clarify-option' + (checked ? ' is-selected' : '') + '">' +
          '<input type="' + inputType + '" name="' + escapeHtml(group) + '" value="' + escapeHtml(label) + '"' +
            (checked ? ' checked' : '') + (done ? ' disabled' : '') +
            ' data-clarify-q="' + qi + '" data-clarify-opt="' + oi + '">' +
          '<span>' +
            '<div class="clarify-option-label">' + escapeHtml(label) + '</div>' +
            (opt.description
              ? '<div class="clarify-option-desc">' + escapeHtml(String(opt.description)) + '</div>'
              : '') +
          '</span>' +
        '</label>';
    });
    opts +=
      '<div class="clarify-other">' +
        '<label class="clarify-other-toggle' + (selected.useCustom ? ' is-selected' : '') + '">' +
          '<input type="' + inputType + '" name="' + escapeHtml(group) + '" value="__other__"' +
            (selected.useCustom ? ' checked' : '') + (done ? ' disabled' : '') +
            ' data-clarify-q="' + qi + '" data-clarify-other="1">' +
          '<span class="clarify-option-label">Other</span>' +
        '</label>' +
        '<textarea class="clarify-other-input" rows="2" placeholder="Describe what you want instead…"' +
          ' data-clarify-q="' + qi + '" data-clarify-custom="1"' +
          (selected.useCustom ? '' : ' hidden') +
          (done ? ' disabled' : '') + '>' +
          escapeHtml(selected.custom || '') +
        '</textarea>' +
      '</div>';
    body +=
      '<div class="clarify-q" data-clarify-q="' + qi + '">' +
        (q.header ? '<span class="clarify-q-chip">' + escapeHtml(String(q.header)) + '</span>' : '') +
        '<div class="clarify-q-text">' + escapeHtml(String(q.question || '')) + '</div>' +
        '<div class="clarify-options">' + opts + '</div>' +
      '</div>';
  });
  return (
    '<div class="clarify-card' + (done ? ' is-done' : '') + '" data-clarify-id="' + escapeHtml(part.id) + '">' +
      '<div class="clarify-card-head">' +
        '<span class="clarify-card-kicker">Deep research</span>' +
        '<span class="clarify-card-title">A couple of quick questions before searching</span>' +
      '</div>' +
      body +
      (done
        ? ''
        : '<div class="clarify-actions">' +
            '<button type="button" class="clarify-submit" data-clarify-submit="' + escapeHtml(part.id) + '">Continue research</button>' +
          '</div>') +
    '</div>'
  );
}

function mountClarifyForm(stream, part) {
  if (!stream.dom?.answerEl) return;
  let host = stream.dom.clarifyHost;
  if (!host) {
    host = document.createElement('div');
    host.className = 'clarify-host';
    stream.dom.clarifyHost = host;
  }
  host.innerHTML = buildClarifyFormHtml(part);
  const answerEl = stream.dom.answerEl;
  if (host.parentElement !== answerEl) {
    answerEl.insertBefore(host, answerEl.firstChild);
  }
  syncClarifySubmitEnabled(host);
}

function preserveClarifyHost(answerEl) {
  const host = answerEl?.querySelector?.(':scope > .clarify-host');
  if (host) host.remove();
  return host || null;
}

function restoreClarifyHost(answerEl, host) {
  if (!answerEl || !host) return;
  answerEl.insertBefore(host, answerEl.firstChild);
}

function syncClarifySubmitEnabled(host) {
  if (!host) return;
  const card = host.querySelector('.clarify-card');
  if (!card || card.classList.contains('is-done')) return;
  const submit = card.querySelector('[data-clarify-submit]');
  if (!submit) return;
  const questions = card.querySelectorAll('.clarify-q');
  let ok = questions.length > 0;
  questions.forEach((qEl) => {
    const qi = Number(qEl.dataset.clarifyQ);
    const otherOn = !!qEl.querySelector('input[data-clarify-other]:checked');
    if (otherOn) {
      const custom = qEl.querySelector('[data-clarify-custom]');
      if (!String(custom?.value || '').trim()) ok = false;
    } else if (!qEl.querySelector('input[data-clarify-opt]:checked')) {
      ok = false;
    }
  });
  submit.disabled = !ok;
}

function collectClarifyAnswers(part, host) {
  const answers = {};
  const questions = Array.isArray(part.questions) ? part.questions : [];
  questions.forEach((q, qi) => {
    const qEl = host.querySelector('.clarify-q[data-clarify-q="' + qi + '"]');
    const key = String(q.question || ('q' + qi));
    const otherOn = !!qEl?.querySelector('input[data-clarify-other]:checked');
    if (otherOn) {
      answers[key] = String(qEl.querySelector('[data-clarify-custom]')?.value || '').trim();
      return;
    }
    const selected = [...(qEl?.querySelectorAll('input[data-clarify-opt]:checked') || [])]
      .map((el) => el.value);
    answers[key] = q.multiSelect ? selected : (selected[0] || '');
  });
  return answers;
}

async function submitClarifyForm(stream, clarifyId) {
  const part = findClarifyPart(stream, clarifyId);
  const host = stream.dom?.clarifyHost;
  if (!part || !host || part.submitting) return;
  const answers = collectClarifyAnswers(part, host);
  part.submitting = true;
  syncClarifySubmitEnabled(host);
  const btn = host.querySelector('[data-clarify-submit]');
  if (btn) {
    btn.disabled = true;
    btn.textContent = 'Submitting…';
  }
  try {
    const response = await fetch('/api/chat/clarify', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ id: clarifyId, answers }),
    });
    if (!response.ok) {
      const problem = await response.json().catch(() => null);
      throw new Error((problem && problem.error) || 'Could not submit answers');
    }
  } catch (error) {
    part.submitting = false;
    if (btn) btn.textContent = 'Continue research';
    syncClarifySubmitEnabled(host);
    showAttachHint(error?.message || 'Could not submit answers');
  }
}

function skillDetailLabel(name) {
  if (name === 'fetch_url') return 'URL';
  if (name === 'activate_skill' || name === 'read_skill') return 'Skill';
  return 'Query';
}

function agentStepResultHtml(result) {
  const text = String(result || '').trim();
  if (!text) return '';
  const expandable = text.length > 180 || (text.match(/\n/g) || []).length >= 3;
  if (!expandable) {
    return '<div class="agent-step-result is-short">' + escapeHtml(text) + '</div>';
  }
  return (
    '<details class="agent-step-result">' +
      '<summary>' +
        '<span class="agent-step-result-text">' + escapeHtml(text) + '</span>' +
        THINK_CHEVRON +
      '</summary>' +
    '</details>'
  );
}

function agentStepHtml({ name, detail, result, note, live, kind }) {
  let kindLabel = skillLabel(name, { kind });
  const resultText = String(result || '');
  if (name === 'ask_user' && !live && /ask_user\b/i.test(resultText)) {
    kindLabel = 'Clarifying questions · rejected';
  }
  return wrapTimelineStep(
    '<div class="agent-step-card">' +
      '<div class="agent-step-kind">' + escapeHtml(kindLabel) + '</div>' +
      (note
        ? '<div class="agent-step-note">' + escapeHtml(note) + '</div>'
        : '') +
      (detail
        ? '<div class="agent-step-detail"><em>' + escapeHtml(skillDetailLabel(name)) + '</em>'
          + '<span class="agent-step-detail-value">' + escapeHtml(detail) + '</span></div>'
        : '') +
      agentStepResultHtml(result) +
    '</div>',
    { live: !!live, done: !live }
  );
}

function renderAssistantHtml(text, { streaming = false } = {}) {
  const segments = parseThinkSegments(text);
  let html = '';
  let textBuf = '';
  const flushText = () => {
    if (textBuf) {
      html += renderMarkdown(textBuf);
      textBuf = '';
    }
  };
  for (const segment of segments) {
    if (segment.type === 'think') {
      flushText();
      if (!segment.content.trim() && !segment.open) continue;
      html += renderThinkBlock(segment.content, { open: segment.open, streaming });
    } else {
      textBuf += segment.content;
    }
  }
  flushText();
  if (html) return html;
  if (streaming) return '<span class="thinking-label">Processing…</span>';
  return '';
}

function inProjectChat() {
  return mainView === 'chat' && !!getProject(activeProjectId);
}

function currentRoutePath() {
  if (mainView === 'projects') return '/projects';
  if (activeId) return '/c/' + encodeURIComponent(activeId);
  if (activeProjectId && getProject(activeProjectId)) {
    return draftIncognito
      ? '/p/' + encodeURIComponent(activeProjectId) + '/ghost'
      : '/p/' + encodeURIComponent(activeProjectId);
  }
  return draftIncognito ? '/ghost' : '/';
}

function syncUrlFromState({ replace = false } = {}) {
  if (suppressUrlSync) return;
  const path = currentRoutePath();
  if (window.location.pathname === path) return;
  if (replace) history.replaceState({ tensorui: 1 }, '', path);
  else history.pushState({ tensorui: 1 }, '', path);
}

function parseLocationRoute() {
  const parts = (window.location.pathname || '/')
    .split('/')
    .map((part) => part.trim())
    .filter(Boolean)
    .map((part) => {
      try { return decodeURIComponent(part); }
      catch { return part; }
    });
  if (parts.length === 0) return { kind: 'draft', incognito: false };
  if (parts[0] === 'projects') return { kind: 'projects' };
  if (parts[0] === 'ghost') return { kind: 'draft', incognito: true };
  if (parts[0] === 'c' && parts[1]) return { kind: 'convo', id: parts[1] };
  if (parts[0] === 'p' && parts[1]) {
    return {
      kind: 'project',
      id: parts[1],
      incognito: parts[2] === 'ghost',
    };
  }
  return { kind: 'draft', incognito: false };
}

function applyLocationRoute() {
  const route = parseLocationRoute();
  const locked = diskEncryptionLocked();
  suppressUrlSync = true;
  try {
    if (route.kind === 'projects') {
      showProjectsView();
    } else if (route.kind === 'convo') {
      const convo = conversations.find((item) => item.id === route.id);
      if (convo) {
        selectConversation(convo.id);
      } else if (locked) {
        activeId = null;
        draftIncognito = false;
        showChatView();
        showEmptyState();
        renderSidebar();
      } else {
        startDraft({ incognito: false });
      }
    } else if (route.kind === 'project') {
      if (getProject(route.id)) {
        openProject(route.id, { incognito: !!route.incognito });
      } else if (locked) {
        activeId = null;
        activeProjectId = null;
        draftIncognito = !!route.incognito;
        showChatView();
        showEmptyState();
        renderSidebar();
      } else {
        activeProjectId = null;
        startDraft({ incognito: !!route.incognito });
      }
    } else {
      activeProjectId = null;
      startDraft({ incognito: !!route.incognito });
    }
  } finally {
    suppressUrlSync = false;
  }
  if (!locked) syncUrlFromState({ replace: true });
}

function leaveProject() {
  activeProjectId = null;
  activeId = null;
  showChatView();
  showEmptyState();
  syncProjectChrome();
  renderSidebar();
  syncUrlFromState();
  composerInput.focus();
}

/** Keep chrome (shell class, topbar, empty state, composer) in sync with project context. */
function syncProjectChrome() {
  const project = inProjectChat() ? getProject(activeProjectId) : null;
  const incognito = isIncognitoContext();
  chatShell.classList.toggle('is-in-project', !!project);
  chatShell.classList.toggle('is-incognito', incognito);
  if (topbarIncognito) {
    topbarIncognito.classList.toggle('is-hidden', !incognito);
  }

  // Label only — the leading plus icon is a sibling span and must survive.
  btnNewChatLabel.textContent = project ? 'New chat in project' : 'New chat';
  btnNewChat.title = project
    ? 'Start a new chat inside ' + project.name
    : 'Start a new chat';
  if (btnNewIncognitoChat) {
    btnNewIncognitoChat.title = project
      ? 'Ghost chat in ' + project.name + ' — temporary, not saved'
      : 'New ghost chat — temporary session, not saved';
  }

  if (incognito && !activeId) {
    emptyEyebrow.textContent = 'Temporary Session';
    emptyEyebrow.classList.remove('is-hidden');
    greetingEl.textContent = project ? project.name : 'Ghost Chat';
  } else if (project && !activeId) {
    emptyEyebrow.textContent = 'Project';
    emptyEyebrow.classList.remove('is-hidden');
    greetingEl.textContent = project.name;
  } else if (!activeId) {
    emptyEyebrow.classList.add('is-hidden');
    const base = greetingForNow();
    const name = settings.name.trim();
    greetingEl.textContent = name ? base + ', ' + name : base;
  } else {
    emptyEyebrow.classList.add('is-hidden');
  }

  topbarProject.innerHTML = '';
  if (project) {
    topbarProject.classList.remove('is-hidden');
    const pill = document.createElement('button');
    pill.type = 'button';
    pill.className = 'topbar-project-pill';
    pill.textContent = project.name;
    pill.title = 'Project settings';
    pill.addEventListener('click', () => openProjectSettings(project.id));
    topbarProject.appendChild(pill);
    if (!activeId) {
      convoTitleEl.textContent = incognito ? '' : 'New chat';
    }
  } else {
    topbarProject.classList.add('is-hidden');
    if (!activeId) convoTitleEl.textContent = '';
  }
  if (incognito && activeId) convoTitleEl.textContent = '';

  if (incognito) {
    composerInput.placeholder = project
      ? 'Ghost chat in ' + project.name + ' — not saved. Type @ to mention'
      : 'Ghost chat — not saved. Type @ to mention';
  } else {
    composerInput.placeholder = project
      ? 'Message in ' + project.name + '… Type @ to mention'
      : 'How can I help you today? Type @ to mention';
  }

  updateComposerHint();
  // Refresh model line without waiting for the next poll.
  if (serverReady) {
    modelHintEl.classList.remove('is-hidden');
    const suffix = project ? ' · Project: ' + project.name : '';
    const current = modelHintEl.textContent || '';
    if (incognito && !activeId) {
      modelHintEl.textContent = 'Temporary session — stays in memory only until you close the tab.'
        + (project ? ' · Project: ' + project.name : '');
    } else if (current.startsWith('Chatting with ')) {
      const model = current.replace(/^Chatting with /, '').split(/ · | via /)[0];
      modelHintEl.textContent = project && !activeId
        ? 'Shared instructions & memory apply · ' + model
        : 'Chatting with ' + model + suffix;
    } else if (project && !activeId) {
      modelHintEl.textContent = 'Shared instructions and memory apply to chats in this project.';
    }
  } else {
    modelHintEl.textContent = '';
    modelHintEl.classList.add('is-hidden');
  }
}

function isIncognitoContext() {
  if (activeId) {
    const convo = conversations.find((item) => item.id === activeId);
    return !!(convo && convo.incognito);
  }
  return draftIncognito;
}

function updateGreeting() {
  syncProjectChrome();
}

function updateComposerHint() {
  if (diskEncryptionLocked()) {
    showComposerHint('Unlock local data to chat and save messages.', { warn: true });
    return;
  }
  if (!serverReady) {
    hideComposerHint();
    return;
  }
  if (voiceListening) {
    showComposerHint('Listening… click the mic or press Esc to stop');
    return;
  }
  // Keep recent attachment / voice warnings visible; pollState would otherwise wipe them.
  if (Date.now() < attachHintUntil && composerHint.classList.contains('is-warn')) {
    return;
  }
  if (Date.now() < voiceHintUntil && composerHint.classList.contains('is-warn')) {
    return;
  }
  attachHintUntil = 0;
  voiceHintUntil = 0;
  const queueLen = activeId ? getOutboundQueue(activeId).length : 0;
  if (queueLen > 0) {
    if (isQueuePausedForEdit(activeId)) {
      showComposerHint('Queue paused · save or cancel the edit to continue');
      return;
    }
    const busy = isConvoBusy(activeId);
    const queueHint = queueLen === 1
      ? (busy
        ? (canSteerLiveStream()
          ? '1 message queued · Steer to guide live activity, or waits until this reply finishes'
          : '1 message queued · sends when this reply finishes')
        : '1 message queued')
      : (busy
        ? (canSteerLiveStream()
          ? queueLen + ' messages queued · Steer one to guide live activity, or they send in order'
          : queueLen + ' messages queued · sending in order')
        : queueLen + ' messages queued');
    showComposerHint(queueHint);
    return;
  }
  hideComposerHint();
}

function bySidebarOrder(list = conversations) {
  return [...list].sort((a, b) => {
    const ao = typeof a.sortOrder === 'number' ? a.sortOrder : Number.POSITIVE_INFINITY;
    const bo = typeof b.sortOrder === 'number' ? b.sortOrder : Number.POSITIVE_INFINITY;
    if (ao !== bo) return ao - bo;
    return b.updatedAt - a.updatedAt;
  });
}

function uncategorizedConversations() {
  return bySidebarOrder(conversations.filter((convo) => !convo.projectId && !convo.pinned));
}

function pinnedConversations() {
  return conversations
    .filter((convo) => convo.pinned && !convo.incognito)
    .sort((a, b) => (b.pinnedAt || b.updatedAt) - (a.pinnedAt || a.updatedAt));
}

function conversationsForProject(projectId) {
  return bySidebarOrder(conversations.filter((convo) => convo.projectId === projectId));
}

function nextTopSortOrder(projectId) {
  const peers = conversations.filter((convo) =>
    projectId ? convo.projectId === projectId : !convo.projectId
  );
  if (peers.length === 0) return 0;
  return Math.min(...peers.map((convo) => (
    typeof convo.sortOrder === 'number' ? convo.sortOrder : 0
  ))) - 1;
}

function applyConversationOrder(orderedIds) {
  orderedIds.forEach((id, index) => {
    const convo = conversations.find((item) => item.id === id);
    if (convo) convo.sortOrder = index;
  });
  saveStore();
  renderSidebar();
}

function closeConvoMenu() {
  if (!openConvoMenu) return;
  const menu = openConvoMenu;
  menu._anchor?.setAttribute('aria-expanded', 'false');
  openConvoMenu = null;
  menu.classList.remove('is-open');

  if (prefersReducedMotion()) {
    menu.remove();
    return;
  }

  const removeMenu = () => {
    window.clearTimeout(menu._closeTimer);
    menu.remove();
  };
  menu.addEventListener('transitionend', removeMenu, { once: true });
  menu._closeTimer = window.setTimeout(removeMenu, 200);
}

let draggingConvoId = null;
let convoDragMoved = false;

function clearConvoDragMarkers(root) {
  if (!root) return;
  root.querySelectorAll('.convo-item.is-drag-before, .convo-item.is-drag-after, .convo-item.is-dragging')
    .forEach((el) => {
      el.classList.remove('is-drag-before', 'is-drag-after', 'is-dragging');
    });
}

function bindConvoListReorder(listEl) {
  const items = [...listEl.querySelectorAll('.convo-item[data-convo-id]')];
  if (items.length < 2) return;

  items.forEach((item) => {
    item.draggable = true;
    item.addEventListener('dragstart', (event) => {
      if (event.target.closest('button')) {
        event.preventDefault();
        return;
      }
      draggingConvoId = item.dataset.convoId || null;
      convoDragMoved = false;
      item.classList.add('is-dragging');
      event.dataTransfer.effectAllowed = 'move';
      event.dataTransfer.setData('text/plain', draggingConvoId || '');
      try {
        event.dataTransfer.setDragImage(item, 12, 12);
      } catch {
        // some browsers reject custom drag images
      }
    });
    item.addEventListener('dragend', () => {
      clearConvoDragMarkers(listEl);
      draggingConvoId = null;
    });
    item.addEventListener('dragover', (event) => {
      if (!draggingConvoId) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = 'move';
      const overId = item.dataset.convoId;
      if (!overId || overId === draggingConvoId) return;
      const rect = item.getBoundingClientRect();
      const before = event.clientY < rect.top + rect.height / 2;
      items.forEach((el) => el.classList.remove('is-drag-before', 'is-drag-after'));
      item.classList.add(before ? 'is-drag-before' : 'is-drag-after');
    });
    item.addEventListener('dragleave', (event) => {
      if (!item.contains(event.relatedTarget)) {
        item.classList.remove('is-drag-before', 'is-drag-after');
      }
    });
    item.addEventListener('drop', (event) => {
      event.preventDefault();
      const sourceId = draggingConvoId || event.dataTransfer.getData('text/plain');
      const targetId = item.dataset.convoId;
      items.forEach((el) => el.classList.remove('is-drag-before', 'is-drag-after'));
      if (!sourceId || !targetId || sourceId === targetId) return;
      const rect = item.getBoundingClientRect();
      const placeBefore = event.clientY < rect.top + rect.height / 2;
      const order = items.map((el) => el.dataset.convoId).filter(Boolean);
      const from = order.indexOf(sourceId);
      let to = order.indexOf(targetId);
      if (from < 0 || to < 0) return;
      order.splice(from, 1);
      to = order.indexOf(targetId);
      if (to < 0) return;
      order.splice(placeBefore ? to : to + 1, 0, sourceId);
      convoDragMoved = true;
      applyConversationOrder(order);
    });
  });
}

function createConvoItem(convo, { nested = false } = {}) {
  const item = document.createElement('div');
  item.className = 'convo-item' + (convo.id === activeId && mainView !== 'projects' ? ' is-active' : '');
  if (convo.incognito) item.classList.add('is-incognito');
  if (convo.pinned) item.classList.add('is-pinned');
  item.dataset.convoId = convo.id;
  if (nested) item.classList.add('is-nested');
  if (isConvoBusy(convo.id)) item.classList.add('is-streaming');
  const fullTitle = convo.title || (convo.incognito ? 'Ghost Chat' : 'New chat');
  if (isConvoBusy(convo.id)) {
    const spinner = document.createElement('span');
    spinner.className = 'convo-spinner';
    spinner.setAttribute('aria-hidden', 'true');
    item.appendChild(spinner);
  } else if (convo.incognito) {
    const mark = document.createElement('span');
    mark.className = 'convo-incognito-mark';
    mark.setAttribute('aria-hidden', 'true');
    mark.innerHTML =
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
        '<path d="M9 10h.01"/><path d="M15 10h.01"/>' +
        '<path d="M12 2a8 8 0 0 0-8 8v12l3-3 2.5 2.5L12 19l2.5 2.5L17 19l3 3V10a8 8 0 0 0-8-8z"/>' +
      '</svg>';
    item.appendChild(mark);
  }
  const title = document.createElement('span');
  title.className = 'convo-title';
  title.textContent = fullTitle;
  applyPrivacyMosaic(title, 'conversation:' + convo.id);
  title.title = convo.incognito ? fullTitle + ' (temporary session — not saved)' : fullTitle;
  item.title = title.title;
  const more = document.createElement('button');
  more.type = 'button';
  more.className = 'convo-more';
  more.setAttribute('aria-label', 'Conversation actions');
  more.setAttribute('aria-haspopup', 'menu');
  more.setAttribute('aria-expanded', 'false');
  more.title = 'More';
  more.draggable = false;
  more.innerHTML = '<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true"><circle cx="5" cy="12" r="1.45"/><circle cx="12" cy="12" r="1.45"/><circle cx="19" cy="12" r="1.45"/></svg>';
  more.addEventListener('click', (event) => {
    event.stopPropagation();
    openConversationMenu(more, convo, item);
  });
  if (convo.pinned) {
    const pinnedMark = document.createElement('span');
    pinnedMark.className = 'convo-pinned-mark';
    pinnedMark.setAttribute('aria-hidden', 'true');
    pinnedMark.innerHTML = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 17v5"/><path d="M5 17h14"/><path d="m7 10 1-7h8l1 7 2 3H5z"/></svg>';
    item.appendChild(pinnedMark);
  }
  item.appendChild(title);
  item.appendChild(more);
  item.addEventListener('click', () => {
    if (convoDragMoved) {
      convoDragMoved = false;
      return;
    }
    selectConversation(convo.id);
  });
  return item;
}

function convoMenuButton(label, icon, onClick, { danger = false } = {}) {
  const button = document.createElement('button');
  button.type = 'button';
  button.setAttribute('role', 'menuitem');
  if (danger) button.classList.add('is-danger');
  button.innerHTML = icon + '<span>' + escapeHtml(label) + '</span>';
  button.addEventListener('click', (event) => {
    event.stopPropagation();
    onClick();
  });
  return button;
}

function openConversationMenu(anchor, convo, row) {
  if (openConvoMenu?._anchor === anchor) {
    closeConvoMenu();
    return;
  }
  closeConvoMenu();
  const menu = document.createElement('div');
  menu.className = 'convo-menu';
  menu.setAttribute('role', 'menu');
  menu._anchor = anchor;
  anchor.setAttribute('aria-expanded', 'true');
  const pinIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 17v5"/><path d="M5 17h14"/><path d="m7 10 1-7h8l1 7 2 3H5z"/></svg>';
  const renameIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M12 20h9"/><path d="M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z"/></svg>';
  const folderIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 6h6l2 2h10v11H3z"/></svg>';
  const trashIcon = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M3 6h18"/><path d="M8 6V4h8v2"/><path d="m19 6-1 14H6L5 6"/></svg>';

  if (!convo.incognito) {
    menu.appendChild(convoMenuButton(convo.pinned ? 'Unpin' : 'Pin', pinIcon, () => {
      closeConvoMenu();
      togglePinnedConversation(convo.id);
    }));
  }
  menu.appendChild(convoMenuButton('Rename', renameIcon, () => {
    closeConvoMenu();
    beginSidebarTitleEdit(convo, row);
  }));

  const targets = projects.filter((project) => project.id !== convo.projectId);
  if (convo.projectId || targets.length) {
    const label = document.createElement('div');
    label.className = 'convo-menu-label';
    label.textContent = 'Move to project';
    menu.appendChild(label);
  }

  if (convo.projectId) {
    menu.appendChild(convoMenuButton('Recents (no project)', folderIcon, () => {
      moveConversation(convo.id, null);
      closeConvoMenu();
    }));
  }

  for (const project of targets) {
    menu.appendChild(convoMenuButton(project.name, folderIcon, () => {
      moveConversation(convo.id, project.id);
      closeConvoMenu();
    }));
  }

  const separator = document.createElement('div');
  separator.className = 'convo-menu-separator';
  separator.setAttribute('role', 'separator');
  menu.appendChild(separator);
  menu.appendChild(convoMenuButton('Delete', trashIcon, () => {
    closeConvoMenu();
    deleteConversation(convo.id);
  }, { danger: true }));

  document.body.appendChild(menu);
  openConvoMenu = menu;
  const rect = anchor.getBoundingClientRect();
  const top = Math.min(rect.bottom + 4, window.innerHeight - menu.offsetHeight - 8);
  const left = Math.min(rect.left, window.innerWidth - menu.offsetWidth - 8);
  menu.style.top = Math.max(8, top) + 'px';
  menu.style.left = Math.max(8, left) + 'px';
  menu.classList.toggle('opens-above', top < rect.top);
  if (prefersReducedMotion()) {
    menu.classList.add('is-open');
  } else {
    window.requestAnimationFrame(() => {
      if (openConvoMenu === menu && menu.isConnected) menu.classList.add('is-open');
    });
  }
}

function moveConversation(convoId, projectId) {
  const convo = conversations.find((item) => item.id === convoId);
  if (!convo) return;
  convo.projectId = projectId;
  convo.updatedAt = Date.now();
  convo.sortOrder = nextTopSortOrder(projectId);
  if (projectId) {
    activeProjectId = projectId;
  }
  saveStore();
  renderSidebar();
  if (mainView === 'projects') renderProjectsPage();
  updateGreeting();
}

function togglePinnedConversation(convoId) {
  const convo = conversations.find((item) => item.id === convoId);
  if (!convo || convo.incognito) return;
  convo.pinned = !convo.pinned;
  convo.pinnedAt = convo.pinned ? Date.now() : null;
  saveStore();
  renderSidebar();
}

function commitConversationTitle(convo, rawTitle) {
  if (!convo) return false;
  const next = String(rawTitle || '').trim().slice(0, 120);
  if (!next || next === convo.title) return false;
  convo.title = next;
  convo.titleEdited = true;
  convo.updatedAt = Date.now();
  convo._titleReq = (convo._titleReq || 0) + 1;
  convo._titleBusy = false;
  saveConversations();
  renderSidebar();
  if (activeId === convo.id) convoTitleEl.textContent = next;
  return true;
}

function beginSidebarTitleEdit(convo, row) {
  if (!convo || !row?.isConnected) return;
  const title = row.querySelector('.convo-title');
  if (!title) return;
  const input = document.createElement('input');
  input.className = 'convo-title-input';
  input.type = 'text';
  input.maxLength = 120;
  input.value = convo.title || 'New chat';
  input.setAttribute('aria-label', 'Chat title');
  let cancelled = false;
  input.addEventListener('click', (event) => event.stopPropagation());
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      input.blur();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      cancelled = true;
      renderSidebar();
    }
  });
  input.addEventListener('blur', () => {
    if (!cancelled) {
      commitConversationTitle(convo, input.value);
      if (input.isConnected) renderSidebar();
    }
  });
  title.replaceWith(input);
  input.focus();
  input.select();
}

function beginTopbarTitleEdit() {
  const convo = conversations.find((item) => item.id === activeId);
  if (!convo || convo.incognito || convoTitleEl.querySelector('input')) return;
  stopTitleTyping(convoTitleEl);
  const original = convo.title || 'New chat';
  const input = document.createElement('input');
  input.className = 'chat-title-input';
  input.type = 'text';
  input.maxLength = 120;
  input.value = original;
  input.setAttribute('aria-label', 'Chat title');
  let cancelled = false;
  input.addEventListener('click', (event) => event.stopPropagation());
  input.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      input.blur();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      cancelled = true;
      convoTitleEl.textContent = original;
      convoTitleEl.focus();
    }
  });
  input.addEventListener('blur', () => {
    if (cancelled) return;
    commitConversationTitle(convo, input.value);
    if (input.isConnected) convoTitleEl.textContent = convo.title || original;
  });
  convoTitleEl.textContent = '';
  convoTitleEl.appendChild(input);
  input.focus();
  input.select();
}

function formatProjectDate(ts) {
  try {
    return new Date(ts).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
  } catch {
    return '';
  }
}

function projectCardBlurb(project) {
  const instructions = project.instructions.trim();
  if (instructions) return instructions;
  const memory = project.memory.trim();
  if (memory) return memory;
  return 'No instructions yet. Open the project to start chatting, or edit settings to add context.';
}

function showProjectsView() {
  mainView = 'projects';
  cancelMessageEdit();
  closeConvoMenu();
  chatShell.classList.add('is-projects');
  chatShell.classList.remove('is-in-project');
  projectsView.classList.remove('is-hidden');
  btnProjectsNav.classList.add('is-active');
  renderProjectsPage();
  renderSidebar();
  syncUrlFromState();
  closeMobileSidebar();
}

function showChatView() {
  mainView = 'chat';
  chatShell.classList.remove('is-projects');
  projectsView.classList.add('is-hidden');
  btnProjectsNav.classList.remove('is-active');
  syncProjectChrome();
  renderSidebar();
}

function renderProjectsPage() {
  const query = (projectsSearch.value || '').trim().toLowerCase();
  const sort = projectsSort.value || 'updated';
  let list = [...projects];
  if (query) {
    list = list.filter((project) => {
      const hay = (project.name + '\n' + project.instructions + '\n' + project.memory).toLowerCase();
      return hay.includes(query);
    });
  }
  if (sort === 'name') {
    list.sort((a, b) => a.name.localeCompare(b.name));
  } else {
    list.sort((a, b) => b.updatedAt - a.updatedAt);
  }

  projectsGrid.innerHTML = '';
  if (list.length === 0) {
    const empty = document.createElement('div');
    empty.className = 'projects-empty';
    empty.innerHTML = query
      ? '<strong>No matches</strong>Try a different search.'
      : '<strong>No projects yet</strong>Create one to keep chats, instructions, and memory together.';
    projectsGrid.appendChild(empty);
    return;
  }

  for (const project of list) {
    const card = document.createElement('div');
    card.className = 'project-card';
    const chatCount = conversationsForProject(project.id).length;

    const body = document.createElement('button');
    body.type = 'button';
    body.className = 'project-card-body';
    body.innerHTML =
      '<h2 class="project-card-title"></h2>' +
      '<p class="project-card-blurb"></p>' +
      '<div class="project-card-meta"><span></span><span></span></div>';
    body.querySelector('.project-card-title').textContent = project.name;
    body.querySelector('.project-card-blurb').textContent = projectCardBlurb(project);
    const meta = body.querySelectorAll('.project-card-meta span');
    meta[0].textContent = formatProjectDate(project.updatedAt);
    meta[1].textContent = chatCount === 1 ? '1 chat' : chatCount + ' chats';
    body.addEventListener('click', () => openProject(project.id));

    const gear = document.createElement('button');
    gear.type = 'button';
    gear.className = 'project-card-gear';
    gear.setAttribute('aria-label', 'Project settings');
    gear.title = 'Settings';
    gear.textContent = '⚙';
    gear.addEventListener('click', (event) => {
      event.stopPropagation();
      openProjectSettings(project.id);
    });

    card.appendChild(body);
    card.appendChild(gear);
    projectsGrid.appendChild(card);
  }
}

function createProject() {
  if (!requireUnlockedData()) return;
  creatingProject = true;
  editingProjectId = null;
  document.getElementById('projectModalTitle').textContent = 'New project';
  document.getElementById('projectName').value = '';
  document.getElementById('projectInstructions').value = '';
  document.getElementById('projectMemory').value = '';
  syncProjectMemoryModeControls('default');
  document.getElementById('btnProjectDelete').classList.add('is-hidden');
  showProjectsView();
  openBackdrop(projectModal);
  document.getElementById('projectName').focus();
}

function syncProjectMemoryModeControls(mode) {
  const value = PROJECT_MEMORY_MODES.includes(mode) ? mode : 'default';
  const toggle = document.getElementById('projectMemoryModeToggle');
  if (toggle) {
    toggle.querySelectorAll('[data-memory-mode]').forEach((btn) => {
      const active = btn.getAttribute('data-memory-mode') === value;
      btn.classList.toggle('is-active', active);
      btn.setAttribute('aria-selected', active ? 'true' : 'false');
    });
  }
  const hint = document.getElementById('projectMemoryModeHint');
  if (hint) {
    hint.textContent = value === 'project_only'
      ? 'Project-only blocks long-term (global) memory. Sibling chats in this project still provide continuity.'
      : 'Default uses long-term memory plus other chats in this project. Project-only keeps context inside the project.';
  }
}

function selectedProjectMemoryMode() {
  const active = document.querySelector('#projectMemoryModeToggle [data-memory-mode].is-active');
  const mode = active && active.getAttribute('data-memory-mode');
  return PROJECT_MEMORY_MODES.includes(mode) ? mode : 'default';
}

function openProjectSettings(projectId) {
  const project = getProject(projectId);
  if (!project) return;
  creatingProject = false;
  editingProjectId = projectId;
  document.getElementById('projectModalTitle').textContent = 'Project';
  document.getElementById('projectName').value = project.name;
  document.getElementById('projectInstructions').value = project.instructions;
  document.getElementById('projectMemory').value = project.memory;
  syncProjectMemoryModeControls(project.memoryMode || 'default');
  document.getElementById('btnProjectDelete').classList.remove('is-hidden');
  openBackdrop(projectModal);
  document.getElementById('projectName').focus();
}

function closeProjectSettings() {
  closeBackdrop(projectModal);
  editingProjectId = null;
  creatingProject = false;
}

function commitProjectSettings() {
  if (!requireUnlockedData()) return;
  const name = document.getElementById('projectName').value.trim() || 'Untitled project';
  const instructions = document.getElementById('projectInstructions').value;
  const memory = document.getElementById('projectMemory').value;
  const memoryMode = selectedProjectMemoryMode();
  const now = Date.now();

  if (creatingProject) {
    const project = normalizeProject({
      id: newId('p'),
      name,
      instructions,
      memory,
      memoryMode,
      createdAt: now,
      updatedAt: now,
    });
    projects.unshift(project);
    creatingProject = false;
    editingProjectId = project.id;
  } else {
    const project = getProject(editingProjectId);
    if (!project) return;
    project.name = name;
    project.instructions = instructions;
    project.memory = memory;
    project.memoryMode = memoryMode;
    project.updatedAt = now;
  }

  saveStore();
  closeProjectSettings();
  renderSidebar();
  if (mainView === 'projects') renderProjectsPage();
  updateGreeting();
}

function deleteProject(projectId) {
  if (!requireUnlockedData()) return;
  const project = getProject(projectId);
  if (!project) return;
  if (!confirm('Delete project "' + project.name + '"? Chats move back to Recents.')) return;
  conversations.forEach((convo) => {
    if (convo.projectId === projectId) convo.projectId = null;
  });
  projects = projects.filter((item) => item.id !== projectId);
  if (activeProjectId === projectId) activeProjectId = null;
  saveStore();
  closeProjectSettings();
  renderSidebar();
  if (mainView === 'projects') renderProjectsPage();
  else {
    showChatView();
    startDraft();
  }
  updateGreeting();
}

function openProject(projectId, { incognito = false } = {}) {
  const project = getProject(projectId);
  if (!project) return;
  activeProjectId = projectId;
  cancelMessageEdit();
  activeId = null;
  draftIncognito = !!incognito;
  showChatView();
  showEmptyState();
  renderSidebar();
  syncUrlFromState();
  closeMobileSidebar();
  composerInput.focus();
}

function escapeHtml(text) {
  return String(text).replace(/[&<>"']/g, (ch) => ({
    '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;',
  }[ch]));
}

const CODE_COPY_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<rect x="9" y="9" width="13" height="13" rx="2"></rect>' +
    '<path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>' +
  '</svg>';
const CODE_CHECK_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<path d="M20 6 9 17l-5-5"></path>' +
  '</svg>';

const MARKED_BASE_RENDERER = (window.marked && window.marked.Renderer)
  ? new window.marked.Renderer()
  : null;

if (window.marked) {
  window.marked.use({
    gfm: true,
    breaks: true,
    renderer: {
      code({ text, lang }) {
        const language = ((lang || '').match(/^[\w+-]+/) || [''])[0].toLowerCase();
        const langClass = language ? (' language-' + language) : '';
        return (
          '<pre><code class="md-code-source' + langClass + '"' +
            (language ? (' data-lang="' + language + '"') : '') +
          '>' + escapeHtml(text) + '</code></pre>\n'
        );
      },
      table(...args) {
        const html = MARKED_BASE_RENDERER
          ? MARKED_BASE_RENDERER.table.apply(this, args)
          : '';
        return '<div class="md-table-wrap">' +
          html.replace('<table>', '<table class="md-table">') +
          '</div>';
      },
      link({ href, title, tokens }) {
        const label = this.parser.parseInline(tokens);
        if (!href || !/^(https?:|mailto:|\/|#)/i.test(href)) return label;
        const titleAttr = title ? (' title="' + escapeHtml(title) + '"') : '';
        const favicon = citeFaviconUrl(href);
        if (favicon) {
          return '<a class="md-cite" href="' + escapeHtml(href) + '"' + titleAttr +
            ' target="_blank" rel="noopener noreferrer">' +
            '<img class="md-cite-favicon" src="' + escapeHtml(favicon) + '"' +
            ' alt="" width="14" height="14" loading="lazy" decoding="async" referrerpolicy="no-referrer">' +
            '<span class="md-cite-text">' + label + '</span></a>';
        }
        return '<a href="' + escapeHtml(href) + '"' + titleAttr +
          ' target="_blank" rel="noopener noreferrer">' + label + '</a>';
      },
    },
  });
}

function citeFaviconUrl(href) {
  try {
    const url = new URL(String(href || ''), window.location.origin);
    if (url.protocol !== 'http:' && url.protocol !== 'https:') return '';
    if (!url.hostname) return '';
    return 'https://www.google.com/s2/favicons?domain=' +
      encodeURIComponent(url.hostname) + '&sz=32';
  } catch {
    return '';
  }
}

if (window.DOMPurify) {
  window.DOMPurify.addHook('afterSanitizeAttributes', (node) => {
    if (node.tagName === 'A' && node.getAttribute('href')) {
      node.setAttribute('target', '_blank');
      node.setAttribute('rel', 'noopener noreferrer');
    }
  });
}

function renderMarkdown(text) {
  if (!text) return '';
  if (!window.marked || !window.DOMPurify) {
    return '<p>' + escapeHtml(text).replace(/\n/g, '<br>') + '</p>';
  }
  const raw = window.marked.parse(String(text), { async: false });
  return window.DOMPurify.sanitize(raw, {
    USE_PROFILES: { html: true },
    ADD_ATTR: ['target', 'align', 'loading', 'decoding', 'referrerpolicy'],
  });
}

/** Hide broken citation favicons so the claim link still reads cleanly. */
function enhanceCiteFavicons(root) {
  if (!root) return;
  root.querySelectorAll('a.md-cite img.md-cite-favicon').forEach((img) => {
    if (img.dataset.citeBound) return;
    img.dataset.citeBound = '1';
    if (img.complete && img.naturalWidth === 0) {
      img.classList.add('is-missing');
      return;
    }
    img.addEventListener('error', () => {
      img.classList.add('is-missing');
    }, { once: true });
  });
}

/** Wrap fenced blocks with copy UI and syntax-highlight (skip mid-stream flicker). */
function enhanceCodeBlocks(root) {
  if (!root) return;
  enhanceCiteFavicons(root);
  root.querySelectorAll('pre > code').forEach((codeEl) => {
    let block = codeEl.closest('.md-code-block');
    if (!block) {
      const pre = codeEl.parentElement;
      if (!pre || pre.tagName !== 'PRE') return;
      const langMatch = (codeEl.className || '').match(/language-([\w+-]+)/i);
      const lang = (
        codeEl.getAttribute('data-lang') ||
        (langMatch && langMatch[1]) ||
        ''
      ).toLowerCase();
      block = document.createElement('div');
      block.className = 'md-code-block';
      block.innerHTML =
        '<div class="md-code-header">' +
          '<span class="md-code-lang">' + escapeHtml(lang || 'code') + '</span>' +
          '<button type="button" class="md-code-copy" aria-label="Copy code" title="Copy">' +
            CODE_COPY_ICON +
          '</button>' +
        '</div>';
      pre.classList.add('md-code');
      codeEl.classList.add('md-code-source');
      pre.replaceWith(block);
      block.appendChild(pre);
    }
    if (!window.hljs || codeEl.dataset.highlighted === 'yes') return;
    try {
      window.hljs.highlightElement(codeEl);
    } catch {
      // Unknown language aliases — leave plain text.
    }
  });
}

function copyCodeBlock(btn) {
  const block = btn.closest('.md-code-block');
  const code = block && block.querySelector('code.md-code-source');
  if (!code || !navigator.clipboard) return;
  const payload = code.textContent || '';
  navigator.clipboard.writeText(payload).then(() => {
    btn.classList.add('is-copied');
    btn.innerHTML = CODE_CHECK_ICON;
    btn.setAttribute('aria-label', 'Copied');
    btn.title = 'Copied';
    setTimeout(() => {
      btn.classList.remove('is-copied');
      btn.innerHTML = CODE_COPY_ICON;
      btn.setAttribute('aria-label', 'Copy code');
      btn.title = 'Copy';
    }, 1400);
  }).catch(() => {});
}

function autoResize(el) {
  el.style.height = 'auto';
  el.style.height = Math.min(el.scrollHeight, 224) + 'px';
}

/** When true, streaming/paint keeps the viewport pinned to the latest message. */
let stickToBottom = true;
/** Explicit upward input wins until the user intentionally returns downward. */
let userScrollOverride = false;
let resumeBottomIntent = false;
let lastViewportScrollTop = 0;
const STICK_BOTTOM_PX = 48;

function isNearBottom() {
  const gap = chatViewport.scrollHeight - chatViewport.scrollTop - chatViewport.clientHeight;
  return gap <= STICK_BOTTOM_PX;
}

function scrollToBottom({ force = false } = {}) {
  if (!force && !stickToBottom) return;
  if (force) {
    stickToBottom = true;
    userScrollOverride = false;
    resumeBottomIntent = false;
  }
  chatViewport.scrollTop = chatViewport.scrollHeight;
  lastViewportScrollTop = chatViewport.scrollTop;
}

function unpinFromBottom() {
  stickToBottom = false;
  userScrollOverride = true;
  resumeBottomIntent = false;
}

chatViewport.addEventListener('scroll', () => {
  const currentTop = chatViewport.scrollTop;
  const movingDown = currentTop > lastViewportScrollTop + 0.5;
  const nearBottom = isNearBottom();

  if (userScrollOverride) {
    if (nearBottom && (resumeBottomIntent || movingDown)) {
      userScrollOverride = false;
      resumeBottomIntent = false;
      stickToBottom = true;
    } else {
      stickToBottom = false;
    }
  } else {
    stickToBottom = nearBottom;
  }

  lastViewportScrollTop = currentTop;
}, { passive: true });
// Unpin immediately on intentional upward scroll (don't wait for layout).
chatViewport.addEventListener('wheel', (event) => {
  if (event.deltaY < 0) unpinFromBottom();
  else if (event.deltaY > 0 && userScrollOverride) resumeBottomIntent = true;
}, { passive: true });
chatViewport.addEventListener('keydown', (event) => {
  if (event.key === 'PageUp' || event.key === 'Home' || event.key === 'ArrowUp') {
    unpinFromBottom();
  } else if (event.key === 'PageDown' || event.key === 'End' || event.key === 'ArrowDown') {
    resumeBottomIntent = true;
  }
});
let touchStickY = null;
chatViewport.addEventListener('touchstart', (event) => {
  touchStickY = event.touches[0] ? event.touches[0].clientY : null;
}, { passive: true });
chatViewport.addEventListener('touchmove', (event) => {
  if (touchStickY == null || !event.touches[0]) return;
  // Finger moving down → content scrolls up → user is reading earlier messages.
  const nextY = event.touches[0].clientY;
  const deltaY = nextY - touchStickY;
  if (deltaY > 8) {
    unpinFromBottom();
    touchStickY = nextY;
  } else if (deltaY < -8) {
    if (userScrollOverride) resumeBottomIntent = true;
    touchStickY = nextY;
  }
}, { passive: true });

function greetingForNow() {
  const hour = new Date().getHours();
  if (hour < 5) return 'Still up?';
  if (hour < 12) return 'Good morning';
  if (hour < 18) return 'Good afternoon';
  return 'Good evening';
}

function showEmptyState() {
  emptyState.classList.remove('is-hidden');
  threadWrap.classList.add('is-hidden');
  chatTopbar.classList.remove('has-thread');
  chatShell.classList.remove('has-active-thread');
  convoTitleEl.removeAttribute('role');
  convoTitleEl.removeAttribute('tabindex');
  convoTitleEl.removeAttribute('title');
  // Drop the previous thread's nodes — sendMessage appends to whatever is
  // here, so leaving them would splice the old conversation into the new one.
  chatThread.innerHTML = '';
  emptyStateInner.appendChild(composerShell);
  syncProjectChrome();
}

function showThread(convo) {
  emptyState.classList.add('is-hidden');
  threadWrap.classList.remove('is-hidden');
  chatTopbar.classList.add('has-thread');
  chatShell.classList.add('has-active-thread');
  convoTitleEl.textContent = convo?.incognito ? '' : (convo.title || 'New chat');
  if (convo?.incognito) {
    convoTitleEl.removeAttribute('role');
    convoTitleEl.removeAttribute('tabindex');
    convoTitleEl.removeAttribute('title');
  } else {
    convoTitleEl.setAttribute('role', 'button');
    convoTitleEl.setAttribute('tabindex', '0');
    convoTitleEl.title = 'Rename chat';
  }
  composerDock.appendChild(composerShell);
  syncProjectChrome();
}

const COPY_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<rect x="9" y="9" width="13" height="13" rx="2"></rect>' +
    '<path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1"></path>' +
  '</svg>';
const CHECK_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<path d="M20 6 9 17l-5-5"></path>' +
  '</svg>';
const EDIT_ICON =
  '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">' +
    '<path d="M12 20h9"></path>' +
    '<path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4Z"></path>' +
  '</svg>';

let editingRow = null;

function cancelMessageEdit({ resumeQueue = true } = {}) {
  if (!editingRow) return;
  const row = editingRow;
  const wasQueued = row.classList.contains('msg-queued');
  const queueId = row.dataset.queueId || '';
  const raw = row.dataset.raw || '';
  const bubble = row.querySelector('.msg-bubble, .msg-edit');
  closeMentionMenu();
  mentionInput = composerInput;
  if (bubble) {
    const next = document.createElement('div');
    next.className = 'msg-bubble';
    if (wasQueued && activeId) {
      const item = findQueuedItem(activeId, queueId);
      next.innerHTML = formatUserMessageHtml({
        content: item?.displayText || raw,
        attachments: item?.attachments || [],
      });
    } else {
      next.innerHTML = formatUserHtml(raw);
    }
    bubble.replaceWith(next);
  }
  row.classList.remove('is-editing');
  editingRow = null;
  if (wasQueued) {
    editingQueueId = null;
    if (activeId && queueId) {
      const item = findQueuedItem(activeId, queueId);
      if (item) refreshQueuedBubble(row, item, { paused: false });
    }
    updateComposerHint();
    if (resumeQueue) maybeSendNextQueued(activeId);
  }
}

function beginMessageEdit(row) {
  if (isConvoBusy(activeId) || row.classList.contains('is-editing')) return;
  if (editingRow && editingRow !== row) cancelMessageEdit();

  const raw = row.dataset.raw || '';
  const bubble = row.querySelector('.msg-bubble');
  if (!bubble) return;

  const editor = document.createElement('div');
  editor.className = 'msg-edit';
  const input = document.createElement('textarea');
  input.className = 'msg-edit-input';
  input.value = raw;
  input.setAttribute('aria-label', 'Edit message');
  input.placeholder = 'Edit message… Type @ to mention';
  const bar = document.createElement('div');
  bar.className = 'msg-edit-bar';
  const btnCancel = document.createElement('button');
  btnCancel.type = 'button';
  btnCancel.className = 'btn btn-ghost';
  btnCancel.textContent = 'Cancel';
  const btnSave = document.createElement('button');
  btnSave.type = 'button';
  btnSave.className = 'btn btn-primary';
  btnSave.textContent = 'Send';
  bar.appendChild(btnCancel);
  bar.appendChild(btnSave);
  editor.appendChild(input);
  editor.appendChild(bar);

  bubble.replaceWith(editor);
  row.classList.add('is-editing');
  editingRow = row;
  mentionInput = input;

  const commit = () => {
    const next = input.value.trim();
    if (!next || isConvoBusy(activeId)) return;
    closeMentionMenu();
    mentionInput = composerInput;
    submitEditedMessage(row, next);
  };
  btnCancel.addEventListener('click', cancelMessageEdit);
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
}

async function submitEditedMessage(row, rawText) {
  const index = Number(row.dataset.msgIndex);
  if (!Number.isInteger(index) || index < 0 || isConvoBusy(activeId) || !serverReady) return;

  const convo = conversations.find((item) => item.id === activeId);
  if (!convo || index >= convo.messages.length) return;
  if (convo.messages[index].role !== 'user') return;

  const mentioned = parseCapabilityMentions(rawText);
  const text = mentioned.text;
  const mentionIds = new Set([
    ...composerMentionIds,
    ...mentioned.mentions,
  ]);
  mentionIds.delete('agent');
  const turn = resolveTurnSkills(mentionIds);
  const displayText = displayTextWithMentions(text, mentioned.mentions);

  editingRow = null;
  row.classList.remove('is-editing');

  // Drop this turn's reply and everything after, then rewrite the user message.
  const priorAttachments = Array.isArray(convo.messages[index].attachments)
    ? convo.messages[index].attachments
    : [];
  convo.messages = convo.messages.slice(0, index + 1);
  const editedMessage = { role: 'user', content: displayText };
  if (priorAttachments.length) editedMessage.attachments = priorAttachments;
  convo.messages[index] = editedMessage;
  if (index === 0 && !convo.titleEdited) {
    // Provisional slug for the sidebar; real title is generated after the
    // first assistant reply so local servers aren't hit with two requests at once.
    convo.title = provisionalTitle(text || displayText);
  }
  convo.updatedAt = Date.now();
  saveConversations();
  renderSidebar();
  renderThread(convo);
  const apiText = priorAttachments.length
    ? buildUserApiContent(text, priorAttachments.map((att) => ({
      ...att,
      sendMode: att.sendMode || (att.kind === 'image' && att.dataUrl && !att.extractedText ? 'native' : 'text'),
      apiPart: att.kind === 'image' && att.dataUrl && !att.extractedText
        ? { type: 'image_url', image_url: { url: att.dataUrl } }
        : null,
      apiText: att.extractedText
        ? ('Attachment: ' + (att.name || 'file') + '\n\n' + att.extractedText)
        : '',
    })))
    : text;
  await runAssistantTurn(convo, {
    useAgent: turn.useAgent,
    text: apiText,
    skills: turn.skills,
    deepResearch: turn.deepResearch,
    deepResearchOutput: turn.deepResearchOutput,
    forceTools: turn.forceTools,
  });
}

function formatTokPerSec(n) {
  const v = Number(n);
  if (!Number.isFinite(v) || v <= 0) return '';
  if (v >= 100) return Math.round(v) + ' tok/s';
  if (v >= 10) return v.toFixed(1) + ' tok/s';
  return v.toFixed(2) + ' tok/s';
}

function ingestStreamUsage(stats, json) {
  if (!stats || !json || typeof json !== 'object') return;
  const usage = json.usage;
  if (usage && typeof usage === 'object') {
    const completion = Number(usage.completion_tokens);
    if (Number.isFinite(completion) && completion >= 0) {
      stats.completionTokens = (stats.completionTokens || 0) + completion;
    }
    const prompt = Number(usage.prompt_tokens);
    if (Number.isFinite(prompt) && prompt >= 0) {
      stats.promptTokens = (stats.promptTokens || 0) + prompt;
    }
  }
  const timings = json.timings;
  if (timings && typeof timings === 'object') {
    const predicted = Number(timings.predicted_per_second);
    if (Number.isFinite(predicted) && predicted > 0) {
      stats.providerTokPerSec = predicted;
    } else {
      const n = Number(timings.predicted_n);
      const ms = Number(timings.predicted_ms);
      if (Number.isFinite(n) && n > 0 && Number.isFinite(ms) && ms > 0) {
        stats.providerTokPerSec = n / (ms / 1000);
      }
    }
  }
  if (typeof json.model === 'string' && json.model.trim() && !stats.upstreamModel) {
    stats.upstreamModel = json.model.trim();
  }
}

function finalizeTurnStats(stats, firstTokenAt, endedAt) {
  if (!stats) return null;
  let tokensPerSec = null;
  if (Number.isFinite(stats.providerTokPerSec) && stats.providerTokPerSec > 0) {
    tokensPerSec = stats.providerTokPerSec;
  } else if (
    Number.isFinite(stats.completionTokens)
    && stats.completionTokens > 0
    && firstTokenAt
  ) {
    const sec = (endedAt - firstTokenAt) / 1000;
    if (sec > 0.05) tokensPerSec = stats.completionTokens / sec;
  }
  return {
    completionTokens: Number.isFinite(stats.completionTokens) ? stats.completionTokens : null,
    promptTokens: Number.isFinite(stats.promptTokens) ? stats.promptTokens : null,
    tokensPerSec: tokensPerSec != null && Number.isFinite(tokensPerSec) ? tokensPerSec : null,
    upstreamModel: stats.upstreamModel || null,
  };
}

function ensureMsgFooter(row) {
  let footer = row.querySelector(':scope > .msg-footer');
  if (!footer) {
    footer = document.createElement('div');
    footer.className = 'msg-footer';
    row.appendChild(footer);
  }
  return footer;
}

function attachMessageMeta(row, message) {
  if (!row) return;
  const footer = ensureMsgFooter(row);
  let meta = footer.querySelector(':scope > .msg-meta');
  if (!message || message.role !== 'assistant') {
    meta?.remove();
    return;
  }
  const model = String(message.model || '').trim();
  const speed = formatTokPerSec(message.tokensPerSec);
  if (!model && !speed) {
    meta?.remove();
    return;
  }
  if (!meta) {
    meta = document.createElement('div');
    meta.className = 'msg-meta';
    const actions = footer.querySelector(':scope > .msg-actions');
    if (actions) footer.insertBefore(meta, actions);
    else footer.appendChild(meta);
  }
  const bits = [];
  if (model) {
    bits.push('<span class="msg-meta-model" title="' + escapeHtml(model) + '">' + escapeHtml(model) + '</span>');
  }
  if (speed) {
    if (bits.length) bits.push('<span class="msg-meta-sep" aria-hidden="true">·</span>');
    const tip = Number.isFinite(message.completionTokens)
      ? (message.completionTokens + ' completion tokens')
      : 'Generation speed';
    bits.push('<span class="msg-meta-speed" title="' + escapeHtml(tip) + '">' + escapeHtml(speed) + '</span>');
  }
  meta.innerHTML = bits.join('');
}

function attachMessageActions(row) {
  const footer = ensureMsgFooter(row);
  if (footer.querySelector(':scope > .msg-actions')) return;
  const actions = document.createElement('div');
  actions.className = 'msg-actions';

  const copyBtn = document.createElement('button');
  copyBtn.type = 'button';
  copyBtn.className = 'msg-action';
  copyBtn.setAttribute('aria-label', 'Copy message');
  copyBtn.title = 'Copy';
  copyBtn.innerHTML = COPY_ICON;
  copyBtn.addEventListener('click', () => {
    if (!navigator.clipboard) return;
    const raw = row.dataset.raw || '';
    const payload = row.classList.contains('msg-role-assistant') && settings.thinking !== 'visible'
      ? stripThinkingTags(raw)
      : raw;
    navigator.clipboard.writeText(payload).then(() => {
      copyBtn.classList.add('is-copied');
      copyBtn.innerHTML = CHECK_ICON;
      copyBtn.title = 'Copied';
      setTimeout(() => {
        copyBtn.classList.remove('is-copied');
        copyBtn.innerHTML = COPY_ICON;
        copyBtn.title = 'Copy';
      }, 1400);
    }).catch(() => {});
  });
  actions.appendChild(copyBtn);

  if (row.classList.contains('msg-role-user')) {
    const editBtn = document.createElement('button');
    editBtn.type = 'button';
    editBtn.className = 'msg-action';
    editBtn.setAttribute('aria-label', 'Edit message');
    editBtn.title = 'Edit';
    editBtn.innerHTML = EDIT_ICON;
    editBtn.addEventListener('click', () => beginMessageEdit(row));
    actions.appendChild(editBtn);
  }

  footer.appendChild(actions);
}

function buildBubble(role, content, index, message, { animate = false } = {}) {
  const row = document.createElement('div');
  row.className = 'msg msg-role-' + role;
  row.dataset.msgIndex = String(index);
  const bubble = document.createElement('div');
  bubble.className = 'msg-bubble';
  row.appendChild(bubble);
  row.dataset.raw = typeof content === 'string' ? content : String(message?.content || '');
  if (role === 'assistant') {
    bubble.innerHTML = renderAssistantMessage(message || { content }, { streaming: false });
    enhanceCodeBlocks(bubble);
    bubble.querySelectorAll('.think-body').forEach((el) => enhanceCodeBlocks(el));
  } else {
    bubble.innerHTML = formatUserMessageHtml(message || { content });
    if (message?.steered) {
      const meta = document.createElement('div');
      meta.className = 'msg-queued-meta';
      meta.innerHTML = '<span class="msg-queued-label is-steered">Steered</span>';
      row.appendChild(meta);
    }
  }
  attachMessageActions(row);
  if (role === 'assistant') {
    attachMessageMeta(row, message || { content });
  }
  if (animate) queueMicrotask(() => motionEnter(row, { y: role === 'user' ? 10 : 14 }));
  return row;
}

function reattachLiveStream(convo) {
  if (!convo || activeId !== convo.id) return null;
  const stream = activeStreams.get(convo.id);
  if (!stream) return null;
  stream.dom = null;
  paintStreamIntoView(convo, stream, stream.partial || '', true);
  for (const entry of stream.pendingSteers || []) {
    if (entry?.applied) continue;
    renderPendingSteerBubble(convo.id, entry);
  }
  if (stream.dom?.row) {
    stream.dom.row.dataset.msgIndex = String(convo.messages.length);
    selectTraceMessage(Number(stream.dom.row.dataset.msgIndex), {
      animate: false,
      ensureOpen: false,
    });
  }
  return stream;
}

function renderThread(convo) {
  cancelMessageEdit({ resumeQueue: false });
  chatThread.innerHTML = '';
  // Drop any DOM handles — rows were destroyed; streams recreate on paint/select.
  for (const stream of activeStreams.values()) {
    stream.dom = null;
  }
  convo.messages.forEach((message, index) => {
    chatThread.appendChild(buildBubble(message.role, message.content, index, message));
  });
  renderOutboundQueue(convo);
  if (reattachLiveStream(convo)) {
    scrollToBottom({ force: stickToBottom });
    maybeSendNextQueued(convo.id);
    return;
  }
  let pick = null;
  for (let i = convo.messages.length - 1; i >= 0; i -= 1) {
    if (convo.messages[i]?.role === 'assistant') {
      pick = i;
      if (messageHasActivity(convo.messages[i])) break;
    }
  }
  selectedTraceMsgIndex = pick;
  syncTraceSelectionClasses();
  refreshTraceSidebar({ animate: false });
  scrollToBottom({ force: stickToBottom });
  maybeSendNextQueued(convo.id);
}

function renderSidebar() {
  closeConvoMenu();
  const list = document.getElementById('convoList');
  list.innerHTML = '';
  sidebarProjectContext.innerHTML = '';
  sidebarProjectContext.classList.add('is-hidden');
  btnProjectsNav.classList.toggle('is-active', mainView === 'projects');

  const pinned = pinnedConversations();
  sidebarPinnedSection.classList.toggle('is-hidden', pinned.length === 0);
  pinnedConvoList.innerHTML = '';
  for (const convo of pinned) {
    pinnedConvoList.appendChild(createConvoItem(convo));
  }

  const project = getProject(activeProjectId);
  if (project && mainView === 'chat') {
    sidebarProjectContext.classList.remove('is-hidden');
    const kicker = document.createElement('p');
    kicker.className = 'sidebar-project-kicker';
    kicker.textContent = 'In project';
    const chip = document.createElement('div');
    chip.className = 'sidebar-project-chip';
    const name = document.createElement('span');
    name.className = 'sidebar-project-chip-name';
    name.textContent = project.name;
    applyPrivacyMosaic(name, 'project:' + project.id);
    name.title = project.name;
    const settingsBtn = document.createElement('button');
    settingsBtn.type = 'button';
    settingsBtn.className = 'project-settings-btn';
    settingsBtn.setAttribute('aria-label', 'Project settings');
    settingsBtn.title = 'Project settings';
    settingsBtn.textContent = '⚙';
    settingsBtn.addEventListener('click', () => openProjectSettings(project.id));
    chip.appendChild(name);
    chip.appendChild(settingsBtn);
    const leave = document.createElement('button');
    leave.type = 'button';
    leave.className = 'sidebar-leave-project';
    leave.textContent = '← Back to general chats';
    leave.addEventListener('click', leaveProject);
    sidebarProjectContext.appendChild(kicker);
    sidebarProjectContext.appendChild(chip);
    sidebarProjectContext.appendChild(leave);
    sidebarConvoLabel.textContent = 'Project chats';
    const projectConvos = conversationsForProject(project.id).filter((convo) => !convo.pinned);
    if (projectConvos.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'convo-empty-hint';
      hint.textContent = 'No chats yet — send a message to start.';
      list.appendChild(hint);
    } else {
      for (const convo of projectConvos) {
        list.appendChild(createConvoItem(convo));
      }
      bindConvoListReorder(list);
    }
    return;
  }

  sidebarConvoLabel.textContent = 'Recents';
  const recent = uncategorizedConversations();
  if (recent.length === 0) {
    const hint = document.createElement('p');
    hint.className = 'convo-empty-hint';
    hint.textContent = projects.length ? 'No general chats yet.' : 'No conversations yet.';
    list.appendChild(hint);
  } else {
    for (const convo of recent) {
      list.appendChild(createConvoItem(convo));
    }
    bindConvoListReorder(list);
  }
}

function selectConversation(id) {
  const convo = conversations.find((item) => item.id === id);
  if (!convo) return;
  const previousId = activeId;
  if (activeId) stickByConvo.set(activeId, stickToBottom);
  activeId = id;
  draftIncognito = !!convo.incognito;
  activeProjectId = convo.projectId || null;
  stickToBottom = stickByConvo.has(id) ? stickByConvo.get(id) : true;
  userScrollOverride = !stickToBottom;
  resumeBottomIntent = false;
  selectedTraceMsgIndex = null;
  resetTraceAutoOpenState();
  showChatView();
  renderSidebar();
  showThread(convo);
  renderThread(convo);
  if (previousId && previousId !== id) maybeSendNextQueued(previousId);
  clearPendingReplyQuote();
  hideSelectionReplyBar();
  syncComposerStreamUi();
  syncUrlFromState();
  closeMobileSidebar();
  composerInput.focus();
}

/** Return to the draft state — no conversation object until a message is sent. */
function startDraft({ incognito = false } = {}) {
  const previousId = activeId;
  cancelMessageEdit({ resumeQueue: false });
  clearPendingReplyQuote();
  hideSelectionReplyBar();
  if (activeId) stickByConvo.set(activeId, stickToBottom);
  activeId = null;
  draftIncognito = !!incognito;
  stickToBottom = true;
  userScrollOverride = false;
  resumeBottomIntent = false;
  selectedTraceMsgIndex = null;
  resetTraceAutoOpenState();
  composerMentionIds.clear();
  renderComposerMentions();
  renderComposerModes();
  showChatView();
  renderSidebar();
  showEmptyState();
  syncComposerStreamUi();
  refreshTraceSidebar({ animate: false });
  setTraceSidebarOpen(false);
  syncUrlFromState();
  closeMobileSidebar();
  if (previousId) maybeSendNextQueued(previousId);
  composerInput.focus();
}

function deleteConversation(id) {
  if (!confirm('Delete this conversation?')) return;
  abortStream(id);
  activeStreams.delete(id);
  const deletingEdit = editingQueueId
    && getOutboundQueue(id).some((item) => item.id === editingQueueId);
  outboundQueues.delete(id);
  if (deletingEdit) {
    editingQueueId = null;
    if (editingRow?.classList.contains('msg-queued')) {
      editingRow = null;
      mentionInput = composerInput;
      closeMentionMenu();
    }
  }
  stickByConvo.delete(id);
  conversations = conversations.filter((item) => item.id !== id);
  saveConversations();
  if (activeId === id) startDraft();
  else {
    renderSidebar();
    syncComposerStreamUi();
  }
}
