# Security review — 2026-09-07

This is a source review and targeted hardening pass, not a security certification.
The owner subsequently authorized the remaining implementation and validation, including compilation.

## Changes in this review

- Updated the vendored DOMPurify from 3.2.6 to the official 3.4.15 release. The old version is covered by [CVE-2025-15599](https://github.com/advisories/GHSA-v8jm-5vwx-cfxm). Markdown sanitization also rejects page styles, forms, embedded documents, and attributes that can interfere with app element lookup.
- API requests reject malformed Origin headers and same-site requests from another local origin. Existing random session-token and exact Host checks remain required. Browser headers also prohibit inline event attributes, base-URL replacement, embedded objects, and framing.
- Lock revokes existing request and response streams, terminal WebSockets, and detached chat generation. Old work stays revoked after unlocking. Chat replay buffers are removed, terminal processes are stopped/cancelled, and agent browser processes are killed. Encryption controls are serialized so unlock cannot race lock cleanup.
- Requests starting sessions during a lock are rejected. Frontend terminal responses arriving after local teardown cannot reconnect a disposed terminal.
- Lock clears draft text, attachment references, reply quotes, rendered conversations/activity, model state, cached markdown images, and terminal instances from the UI. This is reference/DOM cleanup, not a promise to zero JavaScript heap memory.
- Failed unlocks clear provider credentials that may have been restored before a later encrypted file failed validation. Serialized plaintext encryption buffers are now zeroized on both success and failure.
- Provider probes completing after lock cannot repopulate credential-keyed caches.
- OCR uses pinned, integrity-verified local script/worker/WASM/language assets. Its workers terminate on lock, pending recognition is cancelled, and no OCR language cache is retained. CSP now disallows external scripts and restricts workers to this origin.
- Windows private data paths receive protected ACLs for the current user and LocalSystem. Junctions/reparse points in data paths are rejected. Native ACL regression tests verify the resulting permissions.
- Agent browsing uses a fresh temporary profile and an explicit incognito context, verifies HTTPS certificates, and cleans up on lock/normal shutdown. Stale sessions and failed tab creation also stop their browser process. Existing legacy profiles are not reused or silently deleted.

## Encryption assessment

The existing format uses Argon2id (64 MiB, three passes, one lane), fresh random salts, AES-256-GCM with fresh 96-bit nonces, and purpose-specific associated data. Keys zeroize on drop. An authenticated verifier checks the passphrase, and authenticated transition snapshots support recovery from interrupted encryption changes. KDF settings loaded from disk are restricted to supported application profiles.

These choices are consistent with [OWASP password derivation guidance](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html) and [authenticated encryption guidance](https://cheatsheetseries.owasp.org/cheatsheets/Cryptographic_Storage_Cheat_Sheet.html). They do not protect a weak passphrase against offline guessing or an unlocked session against malware.

## Dependency findings and disposition

The public package names and versions of 574 registry dependencies in Cargo.lock were checked through OSV querybatch. The audit returned:

| Dependency | Finding | Disposition |
| --- | --- | --- |
| glib 0.18.5 | [RUSTSEC-2024-0429](https://rustsec.org/advisories/RUSTSEC-2024-0429.html), also GHSA-wrw7-89jp-8q8g: unsound VariantStrIter implementation; fixed in 0.20 | Patched locally using upstream gtk-rs-core#1343. Cargo uses `vendor/glib`; its version is deliberately unchanged. Optimized upstream iterator tests are added to Linux CI. Local Linux execution is blocked by WSL package-server connectivity. |
| atk/atk-sys, gdk/gdk-sys, gdkwayland-sys, gdkx11/gdkx11-sys, gtk/gtk-sys, gtk3-macros 0.18.2 | GTK3 bindings unmaintained (RUSTSEC-2024-0411 through 0420 as applicable) | Open maintenance risk in the Linux desktop stack. |
| fxhash 0.2.1 | RUSTSEC-2025-0057, unmaintained | Open transitive maintenance risk. |
| proc-macro-error 1.0.4 | RUSTSEC-2024-0370, unmaintained | Open transitive maintenance risk. |
| ttf-parser 0.25.1 | RUSTSEC-2026-0192, unmaintained | Open transitive maintenance risk. |

Maintenance notices are not proof of exploitable vulnerabilities; the remaining notices are tracked lifecycle risks rather than known bugs with compatible drop-in fixes. No advisory match was returned for the locked aes-gcm or argon2 versions, or the three newly bundled OCR package versions. This check does not cover every bundled/native library or the installed OS webview/browser.

## Boundaries and remaining validation

- Encryption covers chat/preferences/provider/skill stores, not workspace files, terminal history, downloads, screenshots, legacy `browser-profiles`, browser/OS caches, swap/crash dumps, or older backups. These limits are now visible in encryption settings. Temporary browser data is not encryption: crashes or OS termination failures can leave artifacts. Existing legacy profiles were not deleted by this review.
- Terminal tools run with the app user's OS permissions. A chosen working directory is not a process sandbox. Tool approval and workspace checks are useful controls but do not sandbox arbitrary shell commands or agent-controlled browser activity.
- Unix protected-data paths use restrictive modes; Windows app-owned paths now receive explicit private ACLs. This review does not claim protection against a malicious process already running as the same OS user or every filesystem TOCTOU case.
- Lock can cancel running work and stop future access; it cannot undo a command or network request already executed. Browser/OS process termination failures and server shutdown recovery need platform integration coverage.
- Windows Rust tests cover lease revocation, origin checks, failed-unlock cleanup, ACLs, authenticated encryption, wrong passphrases, tampering, and interrupted transitions.
- Browser sanitizer and frontend lock/race tests can run without building the app: `node --test tests/security-ui.test.cjs`. The real-browser case requires Playwright; set `NODE_PATH` to its installation and optionally `SECURITY_BROWSER_CHANNEL=msedge` on Windows. Missing Playwright is reported as a skipped test, not a passed browser check.

The built-app integration test (`tests/security-integration.test.cjs`) uses an isolated data directory and headless Edge. It checks local OCR with external requests blocked, enable/lock/wrong-passphrase/unlock, terminal WebSocket revocation, and rejection of a stale terminal after unlocking. Set `SECURITY_APP_BINARY` to the built executable to run it. Missing prerequisites are explicit skips.

CI now runs Rust tests on Windows, Linux, and macOS, plus optimized GLib iterator tests on Linux. The Linux/macOS jobs must pass before claiming cross-platform release readiness. WSL dependency installation was attempted locally but failed because its package servers were unreachable; it is not counted as successful Linux validation.

## Completed local validation

- Windows `cargo build --locked --offline`, Clippy on all targets with warnings denied, and project rustfmt checks passed.
- Final full Windows Rust suite: `cargo test --all-targets --locked --offline -- --include-ignored --test-threads=1` passed all 213 tests, with zero failures or ignored tests. This includes live web search and installed-Edge lifecycle/profile cleanup. Doc-test execution also passed (no doc tests are defined).
- Full JavaScript/integration suite: 59 passed, zero failures/skips, including vendored asset hashes, OCR cancellation, malicious HTML in Edge, chat cancellation/edit regressions, and the built-app integration test.
- Built-app integration: passed against the final executable with isolated test data, real offline OCR, and encryption/terminal lock lifecycle checks.
- No successful Linux or macOS execution is claimed. CI was configured, not run remotely in this task. Upstream dependency maintenance notices remain tracked above.

The expanded Windows run found an Edge compatibility-layer relaunch problem: the initial process could exit before Chromiumoxide read its DevTools endpoint. Agent launch now uses `edge-skip-compat-layer-relaunch` for Windows Edge, matching [Playwright's upstream startup switch](https://github.com/microsoft/playwright/blob/main/packages/playwright-core/src/server/chromium/chromiumSwitches.ts). The browser lifecycle regression passed after this fix. Live DuckDuckGo requests failed intermittently during earlier runs and passed individually and in the final serial suite; this does not guarantee continuous external-service availability.
