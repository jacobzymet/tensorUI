let providerSettings = [];
let lastProviderSignature = '';
let localLlmPollTimer = null;

function providerSettingsEls() {
  return {
    list: document.getElementById('providerList'),
    empty: document.getElementById('providerEmpty'),
    error: document.getElementById('providerError'),
    hint: document.getElementById('providerFormHint'),
    test: document.getElementById('providerConnectionTest'),
    mark: document.getElementById('providerConnectionMark'),
    title: document.getElementById('providerConnectionTitle'),
    detail: document.getElementById('providerConnectionDetail'),
    save: document.getElementById('btnSaveProvider'),
    cancel: document.getElementById('btnCancelProviderEdit'),
  };
}

function showProviderError(message) {
  const { error } = providerSettingsEls();
  if (!error) return;
  error.textContent = message || 'Something went wrong.';
  error.classList.remove('is-hidden');
}

function clearProviderError() {
  const { error } = providerSettingsEls();
  if (!error) return;
  error.textContent = '';
  error.classList.add('is-hidden');
}

function setProviderFormHint(text, sticky) {
  const { hint } = providerSettingsEls();
  if (!hint) return;
  if (sticky) hint.dataset.sticky = '1';
  else delete hint.dataset.sticky;
  hint.textContent = text;
}

function hideProviderConnectionTest() {
  const { test } = providerSettingsEls();
  if (!test) return;
  test.classList.add('is-hidden');
  test.classList.remove('is-checking', 'is-ok', 'is-fail');
}

function showProviderConnectionTest(state, title, detail) {
  const els = providerSettingsEls();
  if (!els.test) return;
  els.test.classList.remove('is-hidden', 'is-checking', 'is-ok', 'is-fail');
  els.test.classList.add('is-' + state);
  els.mark.textContent = state === 'checking' ? '…' : (state === 'ok' ? '✓' : '!');
  els.title.textContent = title;
  els.detail.textContent = detail || '';
}

function describeConnectionHealth(result) {
  const health = result?.health || {};
  const kind = health.kind || (health.ok ? 'ready' : 'error');
  const style = result?.api_style;
  const styleBit = style
    ? ((result.detected ? 'Detected ' : '') + apiStyleLabel(style))
    : '';
  if (result?.ok || health.ok) {
    if (kind === 'empty') {
      return {
        state: 'ok',
        title: 'Connected',
        detail: [styleBit, 'Host responded, but /models returned no models yet.']
          .filter(Boolean)
          .join(' · '),
      };
    }
    const count = Number(result?.models) || 0;
    const models = count
      ? count + (count === 1 ? ' model available' : ' models available')
      : 'Model list ok';
    return {
      state: 'ok',
      title: 'Connection successful',
      detail: [styleBit, models].filter(Boolean).join(' · '),
    };
  }
  if (kind === 'auth') {
    return {
      state: 'fail',
      title: 'Authentication failed',
      detail: [styleBit, health.error || 'Check the API token and try again.']
        .filter(Boolean)
        .join(' · '),
    };
  }
  return {
    state: 'fail',
    title: 'Connection failed',
    detail: [styleBit, health.error || 'Could not reach that base URL.']
      .filter(Boolean)
      .join(' · '),
  };
}

function selectedApiStyle() {
  const value = document.getElementById('providerApiStyle')?.value || 'auto';
  if (value === 'anthropic') return 'anthropic';
  if (value === 'openai') return 'openai';
  return 'auto';
}

function apiStyleLabel(style) {
  if (style === 'anthropic') return 'Anthropic Messages';
  if (style === 'openai') return 'OpenAI-compatible';
  return 'Auto-detect';
}

function apiStyleShort(style) {
  if (style === 'anthropic') return 'Anthropic';
  if (style === 'openai') return 'OpenAI';
  return 'Auto';
}

function syncProviderStyleHints() {
  const style = selectedApiStyle();
  const base = document.getElementById('providerBase');
  const token = document.getElementById('providerToken');
  if (!base || !token) return;
  if (style === 'anthropic') {
    base.placeholder = 'https://api.anthropic.com/v1';
    if (!token.placeholder.startsWith('saved')) {
      token.placeholder = 'sk-ant-… or leave blank';
    }
  } else if (style === 'openai') {
    base.placeholder = 'https://api.openai.com/v1';
    if (!token.placeholder.startsWith('saved')) {
      token.placeholder = 'sk-… or leave blank';
    }
  } else {
    base.placeholder = 'https://api.openai.com/v1 or https://api.anthropic.com/v1';
    if (!token.placeholder.startsWith('saved')) {
      token.placeholder = 'sk-… / sk-ant-… or leave blank';
    }
  }
}

async function testProviderConnection({ base, token, id, api_style, allow_insecure_tls }) {
  const style = api_style || selectedApiStyle();
  const detecting = !style || style === 'auto';
  showProviderConnectionTest(
    'checking',
    detecting ? 'Detecting API style…' : 'Testing connection…',
    'Calling ' + base + '/models'
  );
  const body = { base, token: token || undefined, allow_insecure_tls: !!allow_insecure_tls };
  if (!detecting) body.api_style = style;
  if (id) body.id = id;
  const result = await providerApi('/api/providers/test', {
    method: 'POST',
    body: JSON.stringify(body),
  });
  if (result?.api_style === 'openai' || result?.api_style === 'anthropic') {
    if (!detecting) {
      document.getElementById('providerApiStyle').value = result.api_style;
    }
    syncProviderStyleHints();
  }
  const summary = describeConnectionHealth(result);
  showProviderConnectionTest(summary.state, summary.title, summary.detail);
  return { result, summary };
}

function normalizeProviderBase(raw) {
  let base = String(raw || '').trim().replace(/\/+$/, '');
  if (!base) return '';
  if (!base.includes('://')) base = 'https://' + base;
  if (!/\/v1$/i.test(base)) base += '/v1';
  return base;
}

function extractProviders(state) {
  if (!state) return [];
  if (Array.isArray(state.providers) && state.providers.length) return state.providers;
  const network = state.network || {};
  if (Array.isArray(network.remotes)) return network.remotes;
  if (Array.isArray(state.remotes)) return state.remotes;
  return [];
}

function providerHealthLabel(provider) {
  const health = provider.health || {};
  if (!provider.health) return { text: 'Checking…', chip: 'checking', kind: 'checking' };
  const kind = health.kind || (health.ok ? 'ready' : 'error');
  if (kind === 'ready' || health.ok) return { text: 'Ready', chip: 'ready', kind: 'ready' };
  if (kind === 'checking') return { text: 'Checking…', chip: 'checking', kind };
  if (kind === 'waiting') return { text: 'No model running', chip: 'warn', kind, hint: 'Start a model on the host.' };
  if (kind === 'empty') return { text: 'No models listed', chip: 'warn', kind, hint: 'Reachable, but the endpoint returned an empty model list.' };
  if (kind === 'auth') return { text: 'Auth failed', chip: 'failed', kind, hint: 'Check the API token.' };
  return { text: 'Unreachable', chip: 'failed', kind: 'error', hint: health.error || 'Check the base URL.' };
}

function providerModelCount(providerId) {
  const models = latestState?.network?.remote_models || [];
  return models.filter((m) => m.provider_id === providerId).length;
}

function activeProvider() {
  const network = latestState?.network || {};
  const activeId = network.active_remote_id
    || providerSettings.find((p) => p.active)?.id
    || '';
  return providerSettings.find((p) => p.id === activeId)
    || providerSettings.find((p) => p.active)
    || null;
}

function providerCardMeta(provider, health) {
  const bits = [health.text];
  const count = providerModelCount(provider.id);
  if (count) bits.push(count + (count === 1 ? ' model' : ' models'));
  bits.push(apiStyleShort(provider.api_style));
  bits.push(provider.token_set || provider.token_masked ? 'Token set' : 'No token');
  return bits.join(' · ');
}

function renderProviderSettings() {
  const { list, empty, hint } = providerSettingsEls();
  if (!list || !empty) return;
  empty.classList.toggle('is-hidden', providerSettings.length > 0);
  const signature = JSON.stringify(providerSettings.map((p) => [
    p.id, p.name, p.base, p.api_style, p.active, p.token_set, p.token_masked,
    p.health?.ok, p.health?.kind, p.health?.error, providerModelCount(p.id),
  ]));
  if (signature === lastProviderSignature) return;
  lastProviderSignature = signature;
  list.innerHTML = providerSettings.map((provider) => {
    const health = providerHealthLabel(provider);
    const defaultMark = provider.active
      ? '<span class="profile-active-pill">Default</span>'
      : `<button type="button" class="btn btn-outline" data-provider-activate="${escapeHtml(provider.id)}" title="Use this provider when a request does not name a model">Make default</button>`;
    return `
      <li class="provider-card${provider.active ? ' is-active' : ''}" data-provider-id="${escapeHtml(provider.id)}">
        <div class="provider-card-head">
          <div class="provider-card-copy">
            <strong>${escapeHtml(provider.name || 'Provider')}</strong>
            <span>${escapeHtml(provider.base || '')}</span>
          </div>
          <div class="provider-card-actions">
            ${defaultMark}
            <button type="button" class="btn btn-outline" data-provider-edit="${escapeHtml(provider.id)}">Edit</button>
            <button type="button" class="btn btn-outline" data-provider-delete="${escapeHtml(provider.id)}">Remove</button>
          </div>
        </div>
        <p class="field-hint"${health.hint ? ` title="${escapeHtml(health.hint)}"` : ''}>${escapeHtml(providerCardMeta(provider, health))}</p>
      </li>`;
  }).join('');
  if (hint && !hint.dataset.sticky) {
    setProviderFormHint('Add runs a connection test automatically.');
  }
}

function syncProviderSettingsFromState(state) {
  if (!state) return;
  providerSettings = extractProviders(state);
  renderProviderSettings();
}

function clearProviderForm() {
  const { save, cancel } = providerSettingsEls();
  document.getElementById('providerName').value = '';
  document.getElementById('providerApiStyle').value = 'auto';
  document.getElementById('providerBase').value = '';
  document.getElementById('providerToken').value = '';
  document.getElementById('providerToken').placeholder = 'sk-… / sk-ant-… or leave blank';
  document.getElementById('providerAllowInsecureTls').checked = false;
  if (save) {
    delete save.dataset.editingId;
    save.textContent = 'Add';
  }
  cancel?.classList.add('is-hidden');
  document.getElementById('providerFormTitle').textContent = 'Add a provider';
  clearProviderError();
  hideProviderConnectionTest();
  syncProviderStyleHints();
  setProviderFormHint('Base URL should end in /v1. Style is auto-detected unless you override it. Add runs a connection test automatically.');
}

function beginProviderEdit(provider) {
  const { save, cancel } = providerSettingsEls();
  document.getElementById('providerName').value = provider.name || '';
  document.getElementById('providerApiStyle').value = 'auto';
  document.getElementById('providerBase').value = provider.base || '';
  document.getElementById('providerToken').value = '';
  document.getElementById('providerToken').placeholder = provider.token_set || provider.token_masked
    ? ('saved · ' + (provider.token_masked || '••••'))
    : 'sk-… / sk-ant-… or leave blank';
  document.getElementById('providerAllowInsecureTls').checked = !!provider.allow_insecure_tls;
  if (save) {
    save.dataset.editingId = provider.id;
    save.textContent = 'Save';
  }
  cancel?.classList.remove('is-hidden');
  document.getElementById('providerFormTitle').textContent = 'Edit provider';
  clearProviderError();
  hideProviderConnectionTest();
  syncProviderStyleHints();
  setProviderFormHint(
    provider.token_set || provider.token_masked
      ? 'Update name, URL, or paste a new token. Leave the token blank to keep the saved one. Save retests and re-detects API style.'
      : 'Paste a token if the host requires auth, then Save. Save retests and detects API style.',
    true
  );
  document.getElementById('providerToken').focus();
  document.getElementById('providerFormTitle')?.scrollIntoView({ block: 'nearest' });
}

async function providerApi(path, options = {}) {
  const response = await fetch(path, {
    headers: { 'content-type': 'application/json', ...(options.headers || {}) },
    ...options,
  });
  let body = null;
  const text = await response.text();
  if (text) {
    try { body = JSON.parse(text); } catch { body = { error: text }; }
  }
  if (!response.ok) {
    const message = body?.error || body?.message || ('HTTP ' + response.status);
    throw new Error(typeof message === 'string' ? message : 'Request failed');
  }
  return body;
}

async function withProviderBusy(button, label, work) {
  if (!button || button.dataset.busy === '1') return;
  const previous = button.textContent;
  button.dataset.busy = '1';
  button.disabled = true;
  button.textContent = label;
  try {
    await work();
  } finally {
    delete button.dataset.busy;
    button.disabled = false;
    button.textContent = previous;
  }
}

async function mutateProvider(path, options) {
  const result = await providerApi(path, options);
  if (result?.state && typeof updateInferenceState === 'function') {
    updateInferenceState(result.state);
  } else if (result?.network || result?.providers) {
    if (typeof updateInferenceState === 'function') {
      updateInferenceState({
        ...(latestState || {}),
        ...result,
        network: result.network || latestState?.network,
        providers: result.providers || latestState?.providers,
      });
    }
  } else if (typeof pollState === 'function') {
    await pollState();
  }
  syncProviderSettingsFromState(latestState);
  return result;
}

function formatLocalLlmBytes(n) {
  const value = Number(n) || 0;
  if (value < 1024) return value + ' B';
  if (value < 1024 * 1024) return (value / 1024).toFixed(1) + ' KB';
  if (value < 1024 * 1024 * 1024) return (value / (1024 * 1024)).toFixed(1) + ' MB';
  return (value / (1024 * 1024 * 1024)).toFixed(2) + ' GB';
}

function setLocalLlmError(message) {
  const el = document.getElementById('localLlmError');
  if (!el) return;
  if (!message) {
    el.classList.add('is-hidden');
    el.textContent = '';
    return;
  }
  el.textContent = message;
  el.classList.remove('is-hidden');
}

function applyLocalLlmStatus(payload) {
  const install = payload?.install || {};
  const running = payload?.running || null;
  const missing = document.getElementById('localLlmMissing');
  const body = document.getElementById('localLlmBody');
  const detail = document.getElementById('localLlmInstallDetail');
  const ready = document.getElementById('localLlmReady');
  const controls = document.getElementById('localLlmControls');
  const runningBox = document.getElementById('localLlmRunning');
  const runningTitle = document.getElementById('localLlmRunningTitle');
  const runningDetail = document.getElementById('localLlmRunningDetail');

  if (payload?.default_threads && document.getElementById('localLlmThreads') && !document.getElementById('localLlmThreads').dataset.touched) {
    document.getElementById('localLlmThreads').value = String(payload.default_threads);
  }
  if (payload?.default_port && document.getElementById('localLlmPort') && !document.getElementById('localLlmPort').dataset.touched) {
    document.getElementById('localLlmPort').value = String(payload.default_port);
  }

  if (!missing || !body) return;

  const cacheWrap = document.getElementById('localLlmCacheWrap');

  if (install.installed) {
    missing.classList.add('is-hidden');
    body.classList.remove('is-hidden');
    cacheWrap?.classList.remove('is-hidden');
    if (ready) {
      ready.textContent = 'llama-server ready';
      ready.title = [install.path, install.version].filter(Boolean).join(' · ');
    }
  } else {
    body.classList.add('is-hidden');
    cacheWrap?.classList.add('is-hidden');
    missing.classList.remove('is-hidden');
    if (detail) {
      detail.textContent = install.error
        || 'Install llama.cpp and put llama-server on your PATH (or set TENSORUI_LLAMA_SERVER).';
    }
  }

  if (running) {
    runningBox?.classList.remove('is-hidden');
    controls?.classList.add('is-disabled');
    ready?.classList.add('is-hidden');
    if (runningTitle) runningTitle.textContent = running.model || 'Local model';
    if (runningDetail) {
      runningDetail.textContent = [
        running.base_url,
        running.mmap ? 'memory-map' : 'standard',
        running.threads + ' threads',
      ].join(' · ');
    }
  } else {
    runningBox?.classList.add('is-hidden');
    controls?.classList.remove('is-disabled');
    if (install.installed) ready?.classList.remove('is-hidden');
  }

  if (payload?.state && typeof updateInferenceState === 'function') {
    updateInferenceState(payload.state);
  }
}

async function refreshLocalLlmCache() {
  const list = document.getElementById('localLlmCacheList');
  const empty = document.getElementById('localLlmCacheEmpty');
  const hint = document.getElementById('localLlmCacheHint');
  const wrap = document.getElementById('localLlmCacheWrap');
  if (!list || !empty) return;
  try {
    const response = await fetch('/api/local-llms/cache');
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(data.error || ('HTTP ' + response.status));
    const models = Array.isArray(data.models) ? data.models : [];
    if (hint) {
      hint.textContent = data.cache_dir || 'Cached GGUFs';
      hint.title = data.cache_dir || '';
    }
    wrap?.classList.remove('is-hidden');
    if (!models.length) {
      list.innerHTML = '';
      empty.classList.remove('is-hidden');
      return;
    }
    empty.classList.add('is-hidden');
    list.innerHTML = models.map((model) => {
      const label = model.hf_id || model.name;
      const meta = formatLocalLlmBytes(model.size_bytes);
      const useHf = model.hf_id
        ? `data-hf="${escapeHtml(model.hf_id)}"`
        : `data-path="${escapeHtml(model.path)}"`;
      return `
        <li class="provider-card">
          <div class="provider-card-head">
            <div class="provider-card-copy">
              <strong title="${escapeHtml(model.path)}">${escapeHtml(label)}</strong>
              <span>${escapeHtml(meta)}</span>
            </div>
            <div class="provider-card-actions">
              <button type="button" class="btn btn-outline" data-local-run ${useHf}>Run</button>
            </div>
          </div>
        </li>`;
    }).join('');
  } catch (error) {
    empty.classList.remove('is-hidden');
    empty.textContent = 'Could not read cache.';
    empty.title = error.message || String(error);
    list.innerHTML = '';
  }
}

async function refreshLocalLlms() {
  try {
    const response = await fetch('/api/local-llms');
    const data = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(data.error || ('HTTP ' + response.status));
    applyLocalLlmStatus(data);
    if (data?.install?.installed) await refreshLocalLlmCache();
  } catch (error) {
    const missing = document.getElementById('localLlmMissing');
    const body = document.getElementById('localLlmBody');
    const detail = document.getElementById('localLlmInstallDetail');
    body?.classList.add('is-hidden');
    document.getElementById('localLlmCacheWrap')?.classList.add('is-hidden');
    missing?.classList.remove('is-hidden');
    if (detail) detail.textContent = 'Could not check llama-server: ' + (error.message || error);
  }
}

async function startLocalLlm(overrides = {}) {
  setLocalLlmError('');
  const hfInput = document.getElementById('localLlmHf');
  const body = {
    hf: overrides.hf != null ? overrides.hf : (hfInput?.value || '').trim(),
    model_path: overrides.model_path || null,
    mmap: !!document.getElementById('localLlmMmap')?.checked,
    port: Number(document.getElementById('localLlmPort')?.value) || undefined,
    threads: Number(document.getElementById('localLlmThreads')?.value) || undefined,
  };
  if (!body.hf && !body.model_path) {
    setLocalLlmError('Enter a Hugging Face model as org/repo:quant.');
    return;
  }
  if (!body.hf) delete body.hf;
  if (!body.model_path) delete body.model_path;
  const btn = document.getElementById('btnStartLocalLlm');
  await withProviderBusy(btn, 'Starting…', async () => {
    try {
      const response = await fetch('/api/local-llms/start', {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      });
      const data = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(data.error || ('HTTP ' + response.status));
      applyLocalLlmStatus(data);
      await refreshLocalLlmCache();
      if (typeof pollState === 'function') await pollState();
    } catch (error) {
      setLocalLlmError(error.message || String(error));
    }
  });
}

function setProviderSettingsActive(active) {
  if (active) {
    syncProviderSettingsFromState(latestState);
    refreshLocalLlms();
    if (!localLlmPollTimer) {
      localLlmPollTimer = setInterval(refreshLocalLlms, 5000);
    }
    return;
  }
  if (localLlmPollTimer) {
    clearInterval(localLlmPollTimer);
    localLlmPollTimer = null;
  }
}

function bindProviderSettings() {
  const { list, save, cancel, test } = providerSettingsEls();
  if (!list || !save) return;

  list.addEventListener('click', async (event) => {
    const activate = event.target.closest('[data-provider-activate]');
    if (activate && !activate.disabled) {
      const id = activate.getAttribute('data-provider-activate');
      await withProviderBusy(activate, 'Switching…', async () => {
        try {
          clearProviderError();
          await mutateProvider('/api/providers/' + encodeURIComponent(id) + '/activate', {
            method: 'POST',
          });
          setProviderFormHint('Chat now uses that provider.', true);
        } catch (error) {
          showProviderError(error.message);
        }
      });
      return;
    }
    const edit = event.target.closest('[data-provider-edit]');
    if (edit) {
      const id = edit.getAttribute('data-provider-edit');
      const provider = providerSettings.find((p) => p.id === id);
      if (provider) beginProviderEdit(provider);
      return;
    }
    const remove = event.target.closest('[data-provider-delete]');
    if (remove) {
      const id = remove.getAttribute('data-provider-delete');
      const provider = providerSettings.find((p) => p.id === id);
      const isDefault = activeProvider()?.id === id;
      const ok = await confirmDanger({
        title: 'Remove provider?',
        body: 'Remove “' + (provider?.name || 'this provider') + '” from TensorMI Harness? The API host is unchanged.'
          + (isDefault ? ' Its models leave Chat’s picker, and another provider becomes the default.' : ''),
        confirmLabel: 'Remove',
      });
      if (!ok) return;
      try {
        await mutateProvider('/api/providers/' + encodeURIComponent(id), { method: 'DELETE' });
        clearProviderForm();
        setProviderFormHint('Provider removed.', true);
      } catch (error) {
        showProviderError(error.message);
      }
    }
  });

  cancel?.addEventListener('click', () => clearProviderForm());

  save.addEventListener('click', async () => {
    clearProviderError();
    const name = document.getElementById('providerName').value.trim();
    const styleChoice = selectedApiStyle();
    const baseRaw = document.getElementById('providerBase').value.trim();
    const token = document.getElementById('providerToken').value.trim();
    const allow_insecure_tls = document.getElementById('providerAllowInsecureTls').checked;
    const editingId = save.dataset.editingId || '';
    if (!baseRaw) {
      hideProviderConnectionTest();
      showProviderError('Enter a base URL ending in /v1.');
      document.getElementById('providerBase').focus();
      return;
    }
    const base = normalizeProviderBase(baseRaw);
    document.getElementById('providerBase').value = base;

    await withProviderBusy(save, 'Testing…', async () => {
      try {
        const { result, summary } = await testProviderConnection({
          base,
          token,
          api_style: styleChoice,
          id: editingId || undefined,
          allow_insecure_tls,
        });
        if (summary.state !== 'ok') {
          showProviderError(summary.detail || summary.title);
          return;
        }
        const api_style = result?.api_style === 'anthropic' ? 'anthropic' : 'openai';

        save.textContent = editingId ? 'Saving…' : 'Adding…';
        if (editingId) {
          await mutateProvider('/api/providers/' + encodeURIComponent(editingId), {
            method: 'PATCH',
            body: JSON.stringify({
              name: name || undefined,
              base,
              token: token || undefined,
              api_style,
              allow_insecure_tls,
            }),
          });
          clearProviderForm();
          showProviderConnectionTest('ok', 'Provider updated', summary.detail);
          setProviderFormHint('Provider updated · connection verified.', true);
        } else {
          await mutateProvider('/api/providers', {
            method: 'POST',
            body: JSON.stringify({
              name: name || undefined,
              base,
              token: token || undefined,
              api_style,
              allow_insecure_tls,
              activate: true,
            }),
          });
          clearProviderForm();
          showProviderConnectionTest('ok', 'Provider added', summary.detail);
          setProviderFormHint('Provider added, activated for Chat, and connection verified.', true);
        }
      } catch (error) {
        showProviderConnectionTest(
          'fail',
          'Connection test failed',
          error.message || 'Could not test that provider.'
        );
        showProviderError(error.message);
      }
    });
  });

  document.getElementById('providerApiStyle')?.addEventListener('change', () => {
    syncProviderStyleHints();
    if (!test?.classList.contains('is-checking')) {
      hideProviderConnectionTest();
      clearProviderError();
    }
  });

  ['providerName', 'providerBase', 'providerToken'].forEach((id) => {
    const el = document.getElementById(id);
    if (!el) return;
    el.addEventListener('keydown', (event) => {
      if (event.key === 'Enter') {
        event.preventDefault();
        save.click();
      }
    });
    el.addEventListener('input', () => {
      if (!test?.classList.contains('is-checking')) {
        hideProviderConnectionTest();
        clearProviderError();
      }
    });
  });

  document.getElementById('localLlmThreads')?.addEventListener('input', () => {
    document.getElementById('localLlmThreads').dataset.touched = '1';
  });
  document.getElementById('localLlmPort')?.addEventListener('input', () => {
    document.getElementById('localLlmPort').dataset.touched = '1';
  });
  document.getElementById('localLlmHf')?.addEventListener('keydown', (event) => {
    if (event.key === 'Enter') {
      event.preventDefault();
      startLocalLlm();
    }
  });
  document.getElementById('btnStartLocalLlm')?.addEventListener('click', () => {
    startLocalLlm();
  });
  document.getElementById('btnStopLocalLlm')?.addEventListener('click', async () => {
    const btn = document.getElementById('btnStopLocalLlm');
    setLocalLlmError('');
    await withProviderBusy(btn, 'Stopping…', async () => {
      try {
        const response = await fetch('/api/local-llms/stop', { method: 'POST' });
        const data = await response.json().catch(() => ({}));
        if (!response.ok) throw new Error(data.error || ('HTTP ' + response.status));
        applyLocalLlmStatus(data);
        if (typeof pollState === 'function') await pollState();
      } catch (error) {
        setLocalLlmError(error.message || String(error));
      }
    });
  });
  document.getElementById('localLlmCacheList')?.addEventListener('click', (event) => {
    const btn = event.target.closest('[data-local-run]');
    if (!btn) return;
    const hf = btn.getAttribute('data-hf');
    const path = btn.getAttribute('data-path');
    if (hf) {
      const input = document.getElementById('localLlmHf');
      if (input) input.value = hf;
      startLocalLlm({ hf });
    } else if (path) {
      startLocalLlm({ model_path: path, hf: '' });
    }
  });

  syncProviderStyleHints();
}

bindProviderSettings();
window.addEventListener('beforeunload', () => {
  if (localLlmPollTimer) clearInterval(localLlmPollTimer);
});
