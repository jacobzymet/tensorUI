// Exercises the shell wrapper embedded in the Rust source without compiling it.
const assert = require('node:assert/strict');
const { readFileSync } = require('node:fs');
const { join } = require('node:path');
const { spawnSync } = require('node:child_process');
const { test } = require('node:test');

test('production PowerShell wrapper preserves Unicode, quotes, and failure exit codes', { skip: process.platform !== 'win32' }, () => {
  const source = readFileSync(join(__dirname, '../src/agent/terminal.rs'), 'utf8');
  const literal = source.match(/let script = format!\(\s*("[\s\S]*?")\s*\);/);
  assert.ok(literal, 'production wrapper literal not found');
  const template = JSON.parse(literal[1].replace(/\\\r?\n\s*/g, ''));
  const shell = join(process.env.SystemRoot || 'C:\\Windows', 'System32/WindowsPowerShell/v1.0/powershell.exe');
  for (const [command, status, expected] of [
    ["$value = 'it''s $literal 你好'; Write-Output $value", 0, "it's $literal 你好"],
    ['cmd.exe /d /c exit 9', 9, ''],
    ["Write-Error 'session failure'", 1, 'session failure'],
  ]) {
    const script = template.replace(/\{\{|\}\}|\{command\}/g, (token) => token === '{command}' ? command : token[0]);
    const result = spawnSync(shell, ['-NoLogo', '-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(script, 'utf16le').toString('base64')], {
      windowsHide: true, timeout: 10000, encoding: 'utf8', maxBuffer: 32000,
    });
    assert.ifError(result.error);
    assert.equal(result.status, status, command + '\n' + result.stderr);
    assert.ok((result.stdout + result.stderr).includes(expected));
  }
});
