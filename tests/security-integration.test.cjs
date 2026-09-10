// Requires a built app and Playwright. Uses an isolated temporary data directory.
const { test } = require('node:test');
const assert = require('node:assert/strict');
const { mkdtempSync, writeFileSync, rmSync } = require('node:fs');
const { tmpdir } = require('node:os');
const { join, resolve } = require('node:path');
const { spawn } = require('node:child_process');
const net = require('node:net');
const { setTimeout: delay } = require('node:timers/promises');

test('built app supports offline OCR and revokes terminal sockets on encryption lock', {
  skip: !process.env.SECURITY_APP_BINARY,
  timeout: 120000,
}, async () => {
  const { chromium } = require('playwright');
  const dir = mkdtempSync(join(tmpdir(), 'tensor-security-'));
  const config = join(dir, 'config.toml');
  writeFileSync(config, '');
  const reservation = net.createServer();
  await new Promise(r => reservation.listen(0, '127.0.0.1', r));
  const port = reservation.address().port;
  await new Promise(r => reservation.close(r));
  const origin = `http://127.0.0.1:${port}`;
  const child = spawn(resolve(process.env.SECURITY_APP_BINARY), ['--headless', '--config', config, '--bind', `127.0.0.1:${port}`], { windowsHide: true, stdio: 'pipe' });
  let logs = '';
  child.stdout.on('data', b => { logs += b; });
  child.stderr.on('data', b => { logs += b; });
  let browser;
  try {
    let ready = false;
    for (let i = 0; i < 100; i++) {
      try { ready = (await fetch(origin)).ok; } catch {}
      if (ready) break;
      if (child.exitCode !== null) throw new Error(logs);
      await delay(100);
    }
    assert.ok(ready, logs);
    browser = await chromium.launch({ headless: true, channel: process.env.SECURITY_BROWSER_CHANNEL || undefined });
    const page = await browser.newPage();
    const externalCode = [];
    await page.route('**/*', route => {
      if (new URL(route.request().url()).origin !== origin) {
        if (['script', 'worker'].includes(route.request().resourceType())) externalCode.push(route.request().url());
        return route.abort();
      }
      return route.continue();
    });
    await page.goto(origin);
    await page.waitForFunction(() => typeof ocrAttachmentImage === 'function');
    const text = await page.evaluate(async () => {
      const canvas = document.createElement('canvas');
      canvas.width = 1000; canvas.height = 150;
      const ctx = canvas.getContext('2d');
      ctx.fillStyle = 'white'; ctx.fillRect(0, 0, 1000, 150);
      ctx.fillStyle = 'black'; ctx.font = '60px Arial';
      ctx.fillText('SECURE LOCAL OCR', 30, 100);
      return ocrAttachmentImage({ dataUrl: canvas.toDataURL() });
    });
    assert.match(text, /SECURE LOCAL OCR/);
    assert.deepEqual(externalCode, []);
    const call = (path, body) => page.evaluate(async ({ path, body }) => {
      const response = await fetch(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) });
      return { status: response.status, body: await response.json().catch(() => null) };
    }, { path, body });
    const passphrase = 'isolated integration test passphrase';
    assert.equal((await call('/api/data/encryption/enable', { passphrase, passphrase_confirm: passphrase })).status, 200);
    const terminal = await call('/api/terminal/open', { workspace: dir, cols: 80, rows: 24 });
    assert.equal(terminal.status, 200, JSON.stringify(terminal.body));
    await page.evaluate(async id => {
      window.securitySocketClosed = false;
      const socket = new WebSocket(`ws://${location.host}/api/terminal/ws/${id}`);
      socket.onclose = () => { window.securitySocketClosed = true; };
      await new Promise((resolve, reject) => { socket.onopen = resolve; socket.onerror = reject; });
    }, terminal.body.id);
    assert.equal((await call('/api/data/encryption/lock', {})).status, 200);
    await page.waitForFunction(() => window.securitySocketClosed === true);
    assert.equal((await call('/api/terminal/open', { workspace: dir })).status, 423);
    assert.equal((await call('/api/data/encryption/unlock', { passphrase: 'wrong' })).status, 400);
    assert.equal((await call('/api/data/encryption/unlock', { passphrase })).status, 200);
    const status = await page.evaluate(async id => {
      const socket = new WebSocket(`ws://${location.host}/api/terminal/ws/${id}`);
      return new Promise(resolve => { socket.onopen = () => { socket.close(); resolve('opened'); }; socket.onerror = () => resolve('rejected'); });
    }, terminal.body.id);
    assert.equal(status, 'rejected');
  } finally {
    if (browser) await browser.close();
    child.kill();
    if (child.exitCode === null) await new Promise(r => child.once('exit', r));
    // Only the exact test-created directory is removed.
    rmSync(dir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  }
});
