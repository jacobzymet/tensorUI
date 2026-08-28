function formatUserHtml(text) {
  return escapeHtml(text)
    .replace(/\n/g, '<br>')
    .replace(
      /(^|[\s>])(@(?:[a-z][a-z0-9_]{1,31}|everyone|user))\b/gi,
      (_, lead, token) => {
        const id = token.slice(1).toLowerCase();
        return lead + '<span class="mention" data-mention="' + id + '">' + token + '</span>';
      }
    );
}

const BOT_PING_RE = /@(?:[a-z][a-z0-9_]{1,31}|everyone|user)\b/gi;

/** Wrap @pings in rendered assistant HTML (bots surface) to match user bubbles. */
function decorateMentionTextNodes(root) {
  if (!root || typeof isBotsSurface !== 'function' || !isBotsSurface()) return;
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT, {
    acceptNode(node) {
      const parent = node.parentElement;
      if (!parent) return NodeFilter.FILTER_REJECT;
      if (parent.closest('pre, code, .mention, a, script, style, .md-code-lang, button')) {
        return NodeFilter.FILTER_REJECT;
      }
      const value = node.nodeValue || '';
      if (!value.includes('@')) return NodeFilter.FILTER_REJECT;
      BOT_PING_RE.lastIndex = 0;
      if (!BOT_PING_RE.test(value)) return NodeFilter.FILTER_REJECT;
      return NodeFilter.FILTER_ACCEPT;
    },
  });
  const nodes = [];
  while (walker.nextNode()) nodes.push(walker.currentNode);
  nodes.forEach((textNode) => {
    const text = textNode.nodeValue || '';
    BOT_PING_RE.lastIndex = 0;
    const frag = document.createDocumentFragment();
    let last = 0;
    let match;
    while ((match = BOT_PING_RE.exec(text))) {
      if (match.index > last) {
        frag.appendChild(document.createTextNode(text.slice(last, match.index)));
      }
      const span = document.createElement('span');
      span.className = 'mention';
      span.dataset.mention = match[0].slice(1).toLowerCase();
      span.textContent = match[0];
      frag.appendChild(span);
      last = match.index + match[0].length;
    }
    if (last < text.length) frag.appendChild(document.createTextNode(text.slice(last)));
    textNode.parentNode.replaceChild(frag, textNode);
  });
}

function formatUserMessageHtml(message) {
  let text = typeof message === 'string' ? message : (message?.content || '');
  const attachments = Array.isArray(message?.attachments) ? message.attachments : [];
  const replyQuote = typeof message === 'object' && message?.replyQuote
    ? String(message.replyQuote).trim()
    : '';
  if (attachments.length && text === '(attachment)') text = '';
  const replyHandle = typeof message === 'object' && message?.replyToSpeakerHandle
    ? String(message.replyToSpeakerHandle).replace(/^@/, '').trim()
    : '';
  const quoteHtml = replyQuote
    ? '<div class="msg-reply-quote"><span class="msg-reply-quote-label">Replying to'
      + (replyHandle
        ? ' <span class="mention msg-reply-quote-who" data-mention="'
          + escapeHtml(replyHandle.toLowerCase()) + '">@' + escapeHtml(replyHandle) + '</span>'
        : '')
      + '</span>'
      + escapeHtml(replyQuote) + '</div>'
    : '';
  const textHtml = formatUserHtml(text);
  if (!attachments.length) return quoteHtml + textHtml;
  const chips = attachments.map((att) => {
    if (att.kind === 'image' && att.previewUrl) {
      return '<img class="msg-attach-thumb" src="' + escapeHtml(att.previewUrl)
        + '" alt="' + escapeHtml(att.name || 'image') + '">';
    }
    return '<span class="msg-attach-file">' + escapeHtml(att.name || 'attachment') + '</span>';
  }).join('');
  const media = '<div class="msg-attachments">' + chips + '</div>';
  return quoteHtml + (textHtml ? media + textHtml : media);
}

/** @type {Array<{id:string,name:string,mime:string,kind:'image'|'file',size:number,dataUrl:string,previewUrl:string}>} */
let pendingAttachments = [];
/** Selected assistant snippet the next send will reply to. */
let pendingReplyQuote = null;
/** @type {{speakerId:string,speakerHandle:string}|null} */
let pendingReplyTarget = null;
let attachmentsSupported = false;
let modelContextLength = null;
let tesseractLoading = null;

function selectedModelMeta() {
  const remote = selectedRemoteModel(latestState);
  return {
    attachmentsSupported: !!(remote && remote.attachments_supported),
    contextLength: remote && Number(remote.context_length) > 0
      ? Number(remote.context_length)
      : null,
  };
}

function textFallbackContextOk() {
  if (!settings.attachmentTextFallback) return false;
  if (modelContextLength == null) return true;
  return modelContextLength >= ATTACHMENT_MIN_CONTEXT;
}

function attachmentsUiEnabled() {
  const mode = ATTACHMENTS_MODES.includes(settings.attachmentsMode)
    ? settings.attachmentsMode
    : 'auto';
  if (mode === 'off') return false;
  if (mode === 'on') return true;
  return attachmentsSupported || textFallbackContextOk();
}

function attachDisabledReason() {
  const mode = ATTACHMENTS_MODES.includes(settings.attachmentsMode)
    ? settings.attachmentsMode
    : 'auto';
  if (mode === 'off') {
    return 'Attachments disabled in Settings → Attachments';
  }
  if (mode === 'auto' && !attachmentsSupported && !settings.attachmentTextFallback) {
    return 'This model does not report attachment support. Go to Settings → Attachments to configure.';
  }
  if (mode === 'auto' && !attachmentsSupported && settings.attachmentTextFallback && !textFallbackContextOk()) {
    return 'Context window looks too small for text extraction. Go to Settings → Attachments to configure.';
  }
  return '';
}

function syncAttachButton() {
  const meta = selectedModelMeta();
  attachmentsSupported = meta.attachmentsSupported;
  modelContextLength = meta.contextLength;
  renderPendingAttachments();
  updateSendEnabled();
  syncMicButton();
  renderPlusMenu();
  syncPlusButton();
}

function voiceInputSupported() {
  return typeof SpeechRecognitionAPI === 'function';
}

function syncMicButton() {
  if (!btnMic) return;
  const supported = voiceInputSupported();
  btnMic.classList.toggle('is-hidden', !supported);
  btnMic.hidden = !supported;
  if (!supported) {
    stopVoiceInput({ silent: true });
    return;
  }
  const locked = diskEncryptionLocked();
  const enabled = serverReady && !locked;
  if (!enabled && voiceListening) stopVoiceInput({ silent: true });
  btnMic.classList.toggle('is-disabled', !enabled);
  btnMic.setAttribute('aria-disabled', enabled ? 'false' : 'true');
  btnMic.classList.toggle('is-listening', voiceListening);
  btnMic.setAttribute('aria-pressed', voiceListening ? 'true' : 'false');
  btnMic.setAttribute('aria-label', voiceListening ? 'Stop voice input' : 'Voice input');
  if (!enabled) {
    btnMic.title = locked
      ? 'Unlock local data to use voice input'
      : 'Connect a provider to use voice input';
  } else if (voiceListening) {
    btnMic.title = 'Listening… click to stop';
  } else {
    btnMic.title = 'Voice input';
  }
}

function showComposerHint(message, { warn = false } = {}) {
  if (!composerHint || !message) return;
  composerHint.textContent = message;
  composerHint.classList.toggle('is-warn', !!warn);
  composerHint.classList.remove('is-hidden');
}

function hideComposerHint() {
  if (!composerHint) return;
  composerHint.textContent = '';
  composerHint.classList.remove('is-warn');
  composerHint.classList.add('is-hidden');
}

function showVoiceHint(message, { warn = true, ms = 8000 } = {}) {
  if (!message) return;
  showComposerHint(message, { warn });
  voiceHintUntil = Date.now() + ms;
}

let voiceAudioCtx = null;
function playVoiceCue(kind) {
  try {
    const AC = window.AudioContext || window.webkitAudioContext;
    if (!AC) return;
    if (!voiceAudioCtx) voiceAudioCtx = new AC();
    if (voiceAudioCtx.state === 'suspended') void voiceAudioCtx.resume();
    const t0 = voiceAudioCtx.currentTime;
    // Soft two-note blips: up for start, down for stop.
    const tones = kind === 'start'
      ? [
          { f: 620, d: 0.055, g: 0.16 },
          { f: 880, d: 0.08, g: 0.13, delay: 0.05 },
        ]
      : [
          { f: 700, d: 0.05, g: 0.14 },
          { f: 460, d: 0.075, g: 0.11, delay: 0.045 },
        ];
    for (const tone of tones) {
      const osc = voiceAudioCtx.createOscillator();
      const gain = voiceAudioCtx.createGain();
      osc.type = 'sine';
      osc.frequency.value = tone.f;
      const startAt = t0 + (tone.delay || 0);
      gain.gain.setValueAtTime(0.0001, startAt);
      gain.gain.exponentialRampToValueAtTime(tone.g, startAt + 0.01);
      gain.gain.exponentialRampToValueAtTime(0.0001, startAt + tone.d);
      osc.connect(gain);
      gain.connect(voiceAudioCtx.destination);
      osc.start(startAt);
      osc.stop(startAt + tone.d + 0.02);
    }
  } catch (_) {
    // Audio cues are optional; ignore Autoplay / Web Audio failures.
  }
}

function paintVoiceTranscript(interim) {
  const spoken = (voiceFinal + (interim || '')).replace(/^\s+/, '');
  const needsSpace = spoken
    && voicePrefix.length > 0
    && !/\s$/.test(voicePrefix)
    && !/^[.,!?;:]/.test(spoken);
  const lead = needsSpace ? ' ' : '';
  composerInput.value = voicePrefix + lead + spoken + voiceSuffix;
  const cursor = voicePrefix.length + lead.length + spoken.length;
  composerInput.setSelectionRange(cursor, cursor);
  autoResize(composerInput);
  updateSendEnabled();
}

function stopVoiceInput({ silent = false, cue = true } = {}) {
  const wasListening = voiceListening;
  voiceListening = false;
  if (voiceRecognition) {
    const rec = voiceRecognition;
    voiceRecognition = null;
    rec.onresult = null;
    rec.onerror = null;
    rec.onend = null;
    try { rec.stop(); } catch (_) { /* already stopped */ }
    try { rec.abort(); } catch (_) { /* unsupported / already stopped */ }
  }
  if (btnMic) {
    btnMic.classList.remove('is-listening');
    btnMic.setAttribute('aria-pressed', 'false');
    btnMic.setAttribute('aria-label', 'Voice input');
    btnMic.title = 'Voice input';
  }
  if (wasListening && cue) playVoiceCue('stop');
  if (!silent && wasListening) updateComposerHint();
}

function startVoiceInput() {
  if (!voiceInputSupported() || !btnMic) return;
  if (!serverReady || diskEncryptionLocked()) {
    showVoiceHint(
      diskEncryptionLocked()
        ? 'Unlock local data to use voice input.'
        : 'Connect a provider before using voice input.'
    );
    return;
  }
  if (!requireUnlockedData()) return;

  stopVoiceInput({ silent: true, cue: false });

  const start = composerInput.selectionStart ?? composerInput.value.length;
  const end = composerInput.selectionEnd ?? start;
  voicePrefix = composerInput.value.slice(0, start);
  voiceSuffix = composerInput.value.slice(end);
  voiceFinal = '';

  const recognition = new SpeechRecognitionAPI();
  recognition.continuous = true;
  recognition.interimResults = true;
  recognition.lang = navigator.language || 'en-US';

  recognition.onresult = (event) => {
    if (!voiceListening) return;
    let interim = '';
    for (let i = event.resultIndex; i < event.results.length; i += 1) {
      const result = event.results[i];
      const chunk = result?.[0]?.transcript || '';
      if (result.isFinal) voiceFinal += chunk;
      else interim += chunk;
    }
    paintVoiceTranscript(interim);
  };

  recognition.onerror = (event) => {
    const code = event.error || '';
    if (code === 'aborted') return;
    if (code === 'no-speech') {
      stopVoiceInput();
      return;
    }
    stopVoiceInput({ silent: true });
    if (code === 'not-allowed' || code === 'service-not-allowed') {
      showVoiceHint('Microphone permission is blocked for this site.');
    } else if (code === 'audio-capture') {
      showVoiceHint('No microphone was found.');
    } else if (code === 'network') {
      showVoiceHint('Voice recognition needs a network connection.');
    } else {
      showVoiceHint('Voice input failed (' + code + ').');
    }
    syncMicButton();
  };

  recognition.onend = () => {
    if (!voiceListening) return;
    // Chromium sometimes ends continuous sessions; restart while armed.
    try {
      recognition.start();
    } catch (_) {
      stopVoiceInput();
    }
  };

  try {
    recognition.start();
  } catch (error) {
    showVoiceHint(error?.message || 'Could not start voice input.');
    return;
  }

  voiceRecognition = recognition;
  voiceListening = true;
  voiceHintUntil = 0;
  playVoiceCue('start');
  syncMicButton();
  showComposerHint('Listening… click the mic or press Esc to stop');
  composerInput.focus();
}

function toggleVoiceInput() {
  if (voiceListening) stopVoiceInput();
  else startVoiceInput();
}

function renderPendingAttachments() {
  if (!composerAttachmentsEl) return;
  if (!pendingAttachments.length) {
    composerAttachmentsEl.innerHTML = '';
    composerAttachmentsEl.classList.add('is-hidden');
    return;
  }
  composerAttachmentsEl.classList.remove('is-hidden');
  composerAttachmentsEl.innerHTML = pendingAttachments.map((att) => {
    const thumb = att.kind === 'image' && att.previewUrl
      ? '<img src="' + escapeHtml(att.previewUrl) + '" alt="">'
      : '';
    return '<div class="composer-attach-chip" data-attach-id="' + escapeHtml(att.id) + '">'
      + thumb
      + '<span class="attach-name" title="' + escapeHtml(att.name) + '">' + escapeHtml(att.name) + '</span>'
      + '<button type="button" class="attach-remove" data-attach-remove="' + escapeHtml(att.id) + '" aria-label="Remove attachment">×</button>'
      + '</div>';
  }).join('');
}

function clearPendingAttachments() {
  pendingAttachments = [];
  renderPendingAttachments();
  updateSendEnabled();
}

function removePendingAttachment(id) {
  pendingAttachments = pendingAttachments.filter((att) => att.id !== id);
  renderPendingAttachments();
  updateSendEnabled();
}

function fileToDataUrl(file) {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(String(reader.result || ''));
    reader.onerror = () => reject(new Error('Failed to read ' + (file.name || 'file')));
    reader.readAsDataURL(file);
  });
}

function isImageFile(file) {
  if (!file) return false;
  if (file.type && file.type.startsWith('image/')) return true;
  // Some pickers omit MIME (notably certain HEIC/PNG paths). Fall back to name.
  return /\.(avif|bmp|gif|heic|heif|jpe?g|png|svg|webp)$/i.test(String(file.name || ''));
}

function isExtractableFile(file) {
  if (!file) return false;
  if (isImageFile(file)) return false;
  const name = String(file.name || '').toLowerCase();
  const mime = String(file.type || '').toLowerCase();
  if (mime.includes('pdf') || name.endsWith('.pdf')) return true;
  if (mime.startsWith('text/') || mime === 'application/json' || mime === 'application/xml') return true;
  return /\.(txt|md|markdown|csv|tsv|json|jsonl|xml|html?|css|js|tsx?|jsx|py|rs|go|java|c|cpp|h|hpp|ya?ml|toml|ini|log|sql|sh|bat|ps1)$/i.test(name);
}

let attachHintUntil = 0;
function showAttachHint(message, { warn = true } = {}) {
  if (!message) return;
  showComposerHint(message, { warn });
  attachHintUntil = Date.now() + 8000;
}

async function addFilesToPending(fileList) {
  if (!attachmentsUiEnabled()) {
    showAttachHint(attachDisabledReason() || 'Attachments are disabled.');
    return;
  }
  const files = [...(fileList || [])].filter(Boolean);
  let added = 0;
  for (const file of files) {
    if (pendingAttachments.length >= ATTACHMENT_MAX_FILES) {
      showAttachHint('Attachment limit is ' + ATTACHMENT_MAX_FILES + ' files.');
      break;
    }
    if (file.size > ATTACHMENT_MAX_BYTES) {
      showAttachHint((file.name || 'File') + ' is larger than 8 MB.');
      continue;
    }
    if (!isImageFile(file) && !isExtractableFile(file)) {
      showAttachHint('Unsupported file type: ' + (file.name || 'file'));
      continue;
    }
    // Queue anything the paperclip allows. Capability checks (vision / OCR /
    // text extraction) run at send time so the chip always appears.
    try {
      const dataUrl = await fileToDataUrl(file);
      const image = isImageFile(file);
      pendingAttachments.push({
        id: newId('a'),
        name: file.name || (image ? 'image' : 'attachment'),
        mime: file.type || (image ? 'image/png' : 'application/octet-stream'),
        kind: image ? 'image' : 'file',
        size: file.size || 0,
        dataUrl,
        previewUrl: image ? dataUrl : '',
      });
      added += 1;
    } catch (error) {
      showAttachHint(error?.message || 'Failed to read file');
    }
  }
  renderPendingAttachments();
  updateSendEnabled();
  if (added && serverReady) {
    attachHintUntil = 0;
    updateComposerHint();
  }
}

function truncateAttachmentText(text, maxChars) {
  const value = String(text || '').trim();
  if (!value) return '';
  if (value.length <= maxChars) return value;
  return value.slice(0, Math.max(0, maxChars - 80))
    + '\n\n…[truncated ' + (value.length - maxChars + 80) + ' characters]';
}

async function extractAttachmentText(att) {
  const response = await fetch('/api/attachments/extract', {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({
      filename: att.name,
      mime: att.mime,
      content_base64: att.dataUrl,
    }),
  });
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    throw new Error((payload && payload.error) || ('Extract failed (' + response.status + ')'));
  }
  return String(payload?.text || '').trim();
}

async function ensureTesseract() {
  if (window.Tesseract) return window.Tesseract;
  if (tesseractLoading) return tesseractLoading;
  tesseractLoading = new Promise((resolve, reject) => {
    const script = document.createElement('script');
    script.src = 'https://cdn.jsdelivr.net/npm/tesseract.js@5/dist/tesseract.min.js';
    script.async = true;
    script.onload = () => {
      if (window.Tesseract) resolve(window.Tesseract);
      else reject(new Error('Tesseract.js failed to load'));
    };
    script.onerror = () => reject(new Error('Could not load OCR engine (network required the first time)'));
    document.head.appendChild(script);
  }).finally(() => {
    // keep resolved module on window; allow retry on hard failure
  });
  try {
    return await tesseractLoading;
  } catch (error) {
    tesseractLoading = null;
    throw error;
  }
}

async function ocrAttachmentImage(att) {
  const Tesseract = await ensureTesseract();
  const result = await Tesseract.recognize(att.dataUrl || att.previewUrl, 'eng', {
    logger: () => {},
  });
  return String(result?.data?.text || '').trim();
}

async function prepareAttachmentsForSend(attachments) {
  const list = Array.isArray(attachments) ? attachments : [];
  const prepared = [];
  const maxChars = settings.attachmentMaxChars || DEFAULT_SETTINGS.attachmentMaxChars;
  for (const att of list) {
    if (att.kind === 'image' && attachmentsSupported) {
      prepared.push({
        ...att,
        sendMode: 'native',
        apiPart: {
          type: 'image_url',
          image_url: { url: att.dataUrl },
        },
      });
      continue;
    }
    if (att.kind === 'image') {
      if (!(settings.attachmentTextFallback && settings.attachmentOcr && textFallbackContextOk())) {
        throw new Error('Image attachments need a vision model, or OCR enabled in Settings → Attachments.');
      }
      const text = truncateAttachmentText(await ocrAttachmentImage(att), maxChars);
      if (!text) throw new Error('OCR found no text in ' + (att.name || 'image'));
      prepared.push({
        ...att,
        sendMode: 'text',
        extractedText: text,
        apiText: 'Attachment: ' + (att.name || 'image') + '\n\n' + text,
      });
      continue;
    }
    // Non-image files: extract to text (native PDF document APIs are provider-specific).
    if (!settings.attachmentTextFallback || !textFallbackContextOk()) {
      if (attachmentsSupported) {
        throw new Error('Non-image files need text extraction. Enable it in Settings → Attachments.');
      }
      throw new Error('This model cannot take that file. Enable text extraction in Settings → Attachments.');
    }
    const text = truncateAttachmentText(await extractAttachmentText(att), maxChars);
    if (!text) throw new Error('No extractable text in ' + (att.name || 'file'));
    prepared.push({
      ...att,
      sendMode: 'text',
      extractedText: text,
      apiText: 'Attachment: ' + (att.name || 'file') + '\n\n' + text,
    });
  }
  return prepared;
}

function storedAttachmentsFromPrepared(prepared) {
  return prepared.map((att) => ({
    id: att.id,
    name: att.name,
    mime: att.mime,
    kind: att.kind,
    size: att.size,
    previewUrl: att.kind === 'image' ? (att.previewUrl || att.dataUrl) : '',
    dataUrl: att.dataUrl,
    extractedText: att.extractedText || '',
    sendMode: att.sendMode || 'native',
  }));
}

function buildUserApiContent(text, attachments, replyQuote, replyHandle) {
  const prepared = Array.isArray(attachments) ? attachments : [];
  const textBits = [];
  const quote = String(replyQuote || '').trim();
  if (quote) textBits.push(formatReplyQuoteForApi(quote, replyHandle));
  if (text && String(text).trim()) textBits.push(String(text).trim());
  for (const att of prepared) {
    if (att.sendMode === 'text' && att.apiText) textBits.push(att.apiText);
  }
  const imageParts = prepared
    .filter((att) => att.sendMode === 'native' && att.apiPart)
    .map((att) => att.apiPart);
  const combinedText = textBits.join('\n\n');
  if (!imageParts.length) return combinedText;
  const parts = [];
  if (combinedText) parts.push({ type: 'text', text: combinedText });
  parts.push(...imageParts);
  return parts;
}

function formatReplyQuoteForApi(quote, handle) {
  const body = String(quote || '').trim().split('\n').map((line) => '> ' + line).join('\n');
  const who = String(handle || '').replace(/^@/, '').trim();
  const lead = who
    ? 'Replying to @' + who + ':'
    : 'Replying to this from your previous message:';
  return lead + '\n\n' + body;
}

function copyReplyFields(source, target) {
  if (!source || !target) return target;
  if (source.replyQuote) target.replyQuote = source.replyQuote;
  if (source.replyToSpeakerId) {
    target.replyToSpeakerId = source.replyToSpeakerId;
    if (source.replyToSpeakerHandle) target.replyToSpeakerHandle = source.replyToSpeakerHandle;
  }
  return target;
}

function messageReplyExcerpt(raw) {
  const text = typeof stripThinkingTags === 'function'
    ? stripThinkingTags(String(raw || ''))
    : String(raw || '');
  return text.replace(/\u00a0/g, ' ').trim().slice(0, 4000);
}

function resolveReplySpeaker(convo, row, message) {
  if (message?.speakerId) {
    const handle = message.speakerHandle
      || (typeof getBot === 'function' ? getBot(message.speakerId)?.handle : '')
      || '';
    return { speakerId: message.speakerId, speakerHandle: handle };
  }
  if (message?.speakerHandle && typeof botByHandle === 'function') {
    const bot = botByHandle(message.speakerHandle);
    if (bot) return { speakerId: bot.id, speakerHandle: bot.handle };
  }
  const rowId = row?.dataset?.speakerId;
  if (rowId) {
    return {
      speakerId: rowId,
      speakerHandle: row.dataset.speakerHandle || '',
    };
  }
  const stream = typeof activeStreams !== 'undefined' ? activeStreams.get(activeId) : null;
  if (stream?.dom?.row === row && typeof streamSpeakerBot === 'function') {
    const bot = streamSpeakerBot(stream);
    if (bot) return { speakerId: bot.id, speakerHandle: bot.handle };
  }
  if (typeof isBotsConvo === 'function' && isBotsConvo(convo) && convo?.botKind === 'dm' && convo.botId) {
    const bot = typeof getBot === 'function' ? getBot(convo.botId) : null;
    if (bot) return { speakerId: bot.id, speakerHandle: bot.handle };
  }
  return null;
}

function userMessageApiContent(message) {
  let raw = typeof message.content === 'string'
    ? parseCapabilityMentions(message.content).text
    : '';
  const quote = typeof message.replyQuote === 'string' ? message.replyQuote.trim() : '';
  if (quote) {
    raw = formatReplyQuoteForApi(quote, message.replyToSpeakerHandle)
      + (raw && raw !== '(attachment)' ? '\n\n' + raw : '');
  }
  const attachments = Array.isArray(message.attachments) ? message.attachments : [];
  if (!attachments.length) return raw;
  const textBits = [];
  if (raw && raw !== '(attachment)') textBits.push(raw);
  const parts = [];
  for (const att of attachments) {
    const nativeImage = att.kind === 'image'
      && att.dataUrl
      && !att.extractedText
      && (att.sendMode === 'native' || !att.sendMode);
    if (nativeImage) {
      parts.push({
        type: 'image_url',
        image_url: { url: att.dataUrl },
      });
    } else if (att.extractedText) {
      textBits.push('Attachment: ' + (att.name || 'file') + '\n\n' + att.extractedText);
    }
  }
  const combinedText = textBits.join('\n\n');
  if (!parts.length) return combinedText;
  const content = [];
  if (combinedText) content.push({ type: 'text', text: combinedText });
  content.push(...parts);
  return content;
}

function displayTextWithMentions(text, mentionIds) {
  const prefix = [...mentionIds]
    .filter((id) => MENTION_IDS.includes(id))
    .map((id) => '@' + id)
    .join(' ');
  return prefix ? (prefix + ' ' + text).trim() : text;
}

function resolveTurnSkills(mentionIds) {
  const mentioned = mentionIds instanceof Set ? mentionIds : new Set(mentionIds || []);
  const deepAllowed = !!settings.skillDeepResearch;
  const deepFromMention = deepAllowed && mentioned.has('deep_research');
  const savedDeep = deepAllowed && DEEP_RESEARCH_MODES.includes(settings.deepResearch)
    ? settings.deepResearch
    : 'off';
  const deepOutput = savedDeep !== 'off'
    ? savedDeep
    : (deepFromMention ? 'long' : null);
  const deep = !!deepOutput;
  const webSearch = deep
    || (!!settings.agentMode && !!settings.skillWebSearch)
    || mentioned.has('web_search');
  const fetchUrl = deep
    || (!!settings.agentMode && !!settings.skillFetchUrl)
    || mentioned.has('fetch_url');
  const filesystem = !!settings.agentMode && !!settings.skillFilesystem;
  const terminalCap = !!settings.agentMode && !!settings.skillTerminal;
  const browserCap = !!settings.agentMode && !!settings.skillBrowser;
  const useAgent = deep
    || webSearch
    || fetchUrl
    || filesystem
    || terminalCap
    || browserCap
    || (!!settings.agentMode && userSkills.some((skill) => skill.enabled));
  const skills = {
      web_search: webSearch,
      web_search_depth: WEB_SEARCH_DEPTHS.includes(settings.webSearchDepth)
        ? settings.webSearchDepth
        : DEFAULT_SETTINGS.webSearchDepth,
      web_search_searxng: typeof settings.webSearchSearxng === 'string'
        ? settings.webSearchSearxng.trim()
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
      fetch_url: fetchUrl,
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
      filesystem,
      workspace_root: sessionWorkspaceRoot(),
      terminal: terminalCap,
      terminal_timeout_secs: Math.min(120, Math.max(5, Number(settings.terminalTimeoutSecs) || 30)),
      browser: browserCap,
  };
  if (deep) {
    skills.web_search_depth = 'deep';
    skills.web_search_max_results = Math.max(skills.web_search_max_results, 10);
  }
  return {
    useAgent,
    skills,
    deepResearch: deep,
    deepResearchOutput: deepOutput || 'long',
    forceTools: resolveForcedTools(mentioned),
  };
}

function resolveForcedTools(mentionIds) {
  if (!settings.agentMode) return [];
  const mentioned = mentionIds instanceof Set ? mentionIds : new Set(mentionIds || []);
  const forced = [];
  if (mentioned.has('web_search') && settings.skillWebSearch !== false) forced.push('web_search');
  if (mentioned.has('fetch_url') && settings.skillFetchUrl !== false) forced.push('fetch_url');
  return forced;
}

function getMentionContext(textarea) {
  const pos = textarea.selectionStart;
  const before = textarea.value.slice(0, pos);
  const match = before.match(/(^|\s)@([a-z0-9_]*)$/i);
  if (!match) return null;
  const query = match[2].toLowerCase();
  const start = pos - match[2].length - 1; // index of '@'
  return { start, query, pos };
}

function closeMentionMenu() {
  mentionState = null;
  mentionMenu.classList.add('is-hidden');
  mentionMenu.innerHTML = '';
  if (mentionMenu.parentElement !== composerCard) composerCard.prepend(mentionMenu);
}

function placeMentionMenu() {
  const host = mentionInput?.closest('.msg-edit') || composerCard;
  if (mentionMenu.parentElement !== host) {
    if (host === composerCard) {
      composerCard.prepend(mentionMenu);
    } else {
      host.appendChild(mentionMenu);
    }
  }
}

function activeMentionExcludeIds() {
  if (mentionInput === composerInput) return composerMentionIds;
  return parseCapabilityMentions(mentionInput?.value || '').mentions;
}

function mentionOptionDescription(item) {
  if (!item) return '';
  if (item.id === 'web_search' || item.id === 'fetch_url') {
    if (settings.agentMode) {
      return item.id === 'web_search'
        ? 'Required: must search before answering'
        : 'Required: must fetch a page before answering';
    }
    return 'Stays enabled for this chat until removed';
  }
  return item.description;
}

function renderMentionMenu() {
  if (!mentionState || mentionState.items.length === 0) {
    closeMentionMenu();
    return;
  }
  placeMentionMenu();
  mentionMenu.innerHTML = '';
  let lastSection = '';
  mentionState.items.forEach((item, index) => {
    const section = item.section || 'Capabilities';
    if (section !== lastSection) {
      lastSection = section;
      const label = document.createElement('div');
      label.className = 'mention-menu-label';
      label.textContent = section;
      mentionMenu.appendChild(label);
    }
    const btn = document.createElement('button');
    btn.type = 'button';
    btn.className = 'mention-option' + (index === mentionState.activeIndex ? ' is-active' : '');
    btn.setAttribute('role', 'option');
    btn.setAttribute('aria-selected', index === mentionState.activeIndex ? 'true' : 'false');
    btn.innerHTML =
      '<span class="mention-option-name">' + escapeHtml(item.label) + '</span>' +
      '<span class="mention-option-desc">' + escapeHtml(mentionOptionDescription(item)) + '</span>';
    btn.addEventListener('mousedown', (event) => {
      event.preventDefault(); // keep focus in textarea
      insertMention(item);
    });
    mentionMenu.appendChild(btn);
  });
  mentionMenu.classList.remove('is-hidden');
}

function updateMentionMenu(forInput) {
  if (forInput) mentionInput = forInput;
  const input = mentionInput || composerInput;
  const ctx = getMentionContext(input);
  if (!ctx) {
    closeMentionMenu();
    return;
  }
  const excluded = activeMentionExcludeIds();
  const extras = typeof extraMentionItems === 'function'
    ? extraMentionItems(ctx.query, excluded)
    : [];
  const items = extras.concat(MENTION_OPTIONS.filter((item) => {
    if (!item.label.startsWith(ctx.query) || excluded.has(item.id)) return false;
    if (item.id === 'deep_research' && settings.skillDeepResearch === false) return false;
    return true;
  }));
  if (items.length === 0) {
    closeMentionMenu();
    return;
  }
  const prev = mentionState && mentionState.query === ctx.query ? mentionState.activeIndex : 0;
  mentionState = {
    start: ctx.start,
    query: ctx.query,
    activeIndex: Math.min(prev, items.length - 1),
    items,
  };
  renderMentionMenu();
}

/** Promote completed @capability tokens in the textarea into chips (strip text). */
function promoteTypedMentions() {
  const value = composerInput.value;
  if (!/(^|\s)@(?:web_search|fetch_url|deep_research)\b/i.test(value)) return;
  const cursor = composerInput.selectionStart;
  const before = value.slice(0, cursor);
  value.replace(/(^|\s)@(web_search|fetch_url|deep_research)\b/gi, (_, _lead, name) => {
    composerMentionIds.add(name.toLowerCase());
    return _;
  });
  const next = value.replace(/(^|\s)@(?:web_search|fetch_url|deep_research)\b/gi, '$1').replace(/[ \t]{2,}/g, ' ');
  const beforeClean = before.replace(/(^|\s)@(?:web_search|fetch_url|deep_research)\b/gi, '$1').replace(/[ \t]{2,}/g, ' ');
  composerInput.value = next;
  const nextPos = Math.min(beforeClean.length, next.length);
  composerInput.setSelectionRange(nextPos, nextPos);
  renderComposerMentions();
  renderComposerModes();
}

function insertMention(item) {
  if (!mentionState || !mentionInput) return;
  const input = mentionInput;
  const value = input.value;
  const cursor = input.selectionStart;
  const before = value.slice(0, mentionState.start);
  const after = value.slice(cursor);
  if (input === composerInput && !item.inline) {
    input.value = before + after;
    input.setSelectionRange(before.length, before.length);
    composerMentionIds.add(item.id);
    closeMentionMenu();
    autoResize(composerInput);
    updateSendEnabled();
    renderComposerMentions();
    renderComposerModes();
    syncPlusButton();
  } else {
    const inserted = '@' + item.label + (after.startsWith(' ') ? '' : ' ');
    input.value = before + inserted + after;
    const pos = before.length + inserted.length;
    input.setSelectionRange(pos, pos);
    closeMentionMenu();
    autoResize(input);
  }
  input.focus();
}

function handleMentionKeydown(event) {
  if (!mentionState) return false;
  if (event.key === 'ArrowDown') {
    event.preventDefault();
    mentionState.activeIndex = (mentionState.activeIndex + 1) % mentionState.items.length;
    renderMentionMenu();
    return true;
  }
  if (event.key === 'ArrowUp') {
    event.preventDefault();
    mentionState.activeIndex =
      (mentionState.activeIndex - 1 + mentionState.items.length) % mentionState.items.length;
    renderMentionMenu();
    return true;
  }
  if (event.key === 'Enter' || event.key === 'Tab') {
    event.preventDefault();
    insertMention(mentionState.items[mentionState.activeIndex]);
    return true;
  }
  if (event.key === 'Escape') {
    event.preventDefault();
    closeMentionMenu();
    return true;
  }
  return false;
}

function renderComposerReply() {
  if (!composerReply) return;
  const quote = typeof pendingReplyQuote === 'string' ? pendingReplyQuote.trim() : '';
  if (!quote) {
    composerReply.innerHTML = '';
    composerReply.classList.add('is-hidden');
    return;
  }
  composerReply.classList.remove('is-hidden');
  composerReply.innerHTML = '';
  const chip = document.createElement('div');
  chip.className = 'composer-reply-chip';
  const label = document.createElement('span');
  label.className = 'composer-reply-chip-label';
  const handle = pendingReplyTarget?.speakerHandle
    ? String(pendingReplyTarget.speakerHandle).replace(/^@/, '').trim()
    : '';
  if (handle) {
    label.classList.add('has-handle');
    label.textContent = 'Reply to @' + handle;
  } else {
    label.textContent = 'Reply';
  }
  const text = document.createElement('div');
  text.className = 'composer-reply-chip-text';
  text.textContent = quote;
  const remove = document.createElement('button');
  remove.type = 'button';
  remove.setAttribute('aria-label', 'Remove reply');
  remove.textContent = '×';
  remove.addEventListener('click', () => {
    clearPendingReplyQuote();
    focusComposer();
  });
  chip.appendChild(label);
  chip.appendChild(text);
  chip.appendChild(remove);
  composerReply.appendChild(chip);
}

function clearPendingReplyQuote() {
  pendingReplyQuote = null;
  pendingReplyTarget = null;
  renderComposerReply();
  updateSendEnabled();
}

function setPendingReply(text, target = null) {
  const quote = messageReplyExcerpt(text);
  if (!quote) return;
  pendingReplyQuote = quote;
  pendingReplyTarget = target && target.speakerId
    ? {
      speakerId: String(target.speakerId),
      speakerHandle: String(target.speakerHandle || '').replace(/^@/, ''),
    }
    : null;
  hideSelectionReplyBar();
  renderComposerReply();
  updateSendEnabled();
  focusComposer();
}

function setPendingReplyQuote(text) {
  setPendingReply(text, null);
}

function hideSelectionReplyBar() {
  if (!selectionReplyBar) return;
  selectionReplyBar.classList.add('is-hidden');
  selectionReplyBar.hidden = true;
}

function assistantSelectionQuote() {
  const sel = window.getSelection();
  if (!sel || sel.isCollapsed || !sel.rangeCount || !chatThread) return null;
  const text = String(sel.toString() || '').replace(/\u00a0/g, ' ').trim();
  if (!text) return null;
  const range = sel.getRangeAt(0);
  const node = range.commonAncestorContainer;
  const el = node.nodeType === 1 ? node : node.parentElement;
  if (!el || !chatThread.contains(el)) return null;
  const row = el.closest('.msg-role-assistant');
  if (!row || !chatThread.contains(row) || row.classList.contains('msg-queued')) return null;
  if (el.closest('.thinking, .msg-actions, .clarify-card, .msg-edit, button, textarea, input')) {
    return null;
  }
  const bubble = row.querySelector('.msg-bubble');
  if (!bubble || !bubble.contains(el)) return null;
  const rect = range.getBoundingClientRect();
  if (!rect || (rect.width === 0 && rect.height === 0)) return null;
  return {
    text: text.slice(0, 4000),
    rect,
    row,
  };
}

function syncSelectionReplyBar() {
  if (!selectionReplyBar || mainView !== 'chat') {
    hideSelectionReplyBar();
    return;
  }
  const quote = assistantSelectionQuote();
  if (!quote) {
    hideSelectionReplyBar();
    return;
  }
  selectionReplyBar.hidden = false;
  selectionReplyBar.classList.remove('is-hidden');
  const top = Math.max(8, quote.rect.top - 44);
  const left = Math.min(
    window.innerWidth - 24,
    Math.max(24, quote.rect.left + quote.rect.width / 2)
  );
  selectionReplyBar.style.top = top + 'px';
  selectionReplyBar.style.left = left + 'px';
}

function renderComposerMentions() {
  composerMentions.innerHTML = '';
  const agentOn = !!settings.agentMode;
  MENTION_IDS.forEach((id) => {
    if (!composerMentionIds.has(id)) return;
    const option = mentionOptionById(id);
    const forced = agentOn && (id === 'web_search' || id === 'fetch_url');
    const chip = document.createElement('span');
    chip.className = 'composer-mention-chip' + (forced ? ' is-forced' : '');
    const label = document.createElement('span');
    label.textContent = '@' + (option ? option.label : id);
    chip.appendChild(label);
    if (forced) {
      const badge = document.createElement('span');
      badge.className = 'chip-badge';
      badge.textContent = 'required';
      badge.title = 'Agent will use this skill before answering';
      chip.appendChild(badge);
    }
    const remove = document.createElement('button');
    remove.type = 'button';
    remove.setAttribute('aria-label', 'Remove @' + id);
    remove.textContent = '×';
    remove.addEventListener('click', () => {
      composerMentionIds.delete(id);
      renderComposerMentions();
      renderComposerModes();
      syncPlusButton();
      composerInput.focus();
    });
    chip.appendChild(remove);
    composerMentions.appendChild(chip);
  });
  syncPlusButton();
}

let userSkills = [];

