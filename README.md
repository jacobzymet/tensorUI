# <img src="assets/browser-favicon.png" alt="" width="36" height="36"> TensorMI Harness

A local, lightweight, open source LLM harness for humanity.

<p align="center">
  <img width="49%" src="https://github.com/user-attachments/assets/f7b7e455-f61e-4978-a5ff-bc587c3200c4" />
  <img width="49%" src="https://github.com/user-attachments/assets/e5ef9c8a-5401-46c0-81c5-a3c0531e5386" />
</p>

A more permissive alternative to [Open WebUI](https://github.com/open-webui/open-webui): connect to LLM APIs on your machine (Ollama, llama-server) or in the cloud (OpenAI, Gemini, Anthropic, and other OpenAI-compatible or **Anthropic Messages**–compatible endpoints).

**Encryption at rest** is a first-class feature: lock chats and preferences on disk with a passphrase (Argon2id + AES-256-GCM). After each restart you unlock once for the session; **Lock session** clears the key from memory without turning encryption off.

Turn models into agents with **Agent mode**: toggle **Agent** in the composer for every message, or `@web_search` / `@fetch_url` once, and the model can call **Agent Capabilities** — web search, URL fetch, custom skills, and more.

TensorMI Harness does **not** run an inference server. You point it at a base URL such as `http://127.0.0.1:11434/v1`, `https://api.openai.com/v1`, or `https://api.anthropic.com/v1`, and it proxies chat through a loopback-only control plane.

## Requirements

- [Rust](https://www.rust-lang.org/tools/install) (for building from source)
- [Python 3.10+](https://www.python.org/downloads/) with `ddgs` (only for web search when running from a source checkout; release archives include a standalone helper)
- An OpenAI-compatible or Anthropic Messages–compatible API endpoint

On Linux, the desktop shell needs WebKitGTK (`webkit2gtk-4.1`).

## Run

Official release archives include `tensorui-search`, a standalone web-search helper containing its own Python runtime and pinned DDGS dependency. Keep it beside the main `tensorui` executable; no system Python installation is needed.

When running from a source checkout, create an isolated environment and install the pinned search dependency once:

```powershell
py -3 -m venv .venv
.\.venv\Scripts\python -m pip install -r requirements-search.txt
```

On macOS or Linux:

```bash
python3 -m venv .venv
./.venv/bin/python -m pip install -r requirements-search.txt
```

```
cargo run
```

This starts the local control plane and opens **TensorMI Harness** in a native desktop window (not a browser tab).

| Flag | Behavior |
| --- | --- |
| _(default)_ | Desktop app window |
| `--browser` | Open in the default browser instead |
| `--headless` | Server only (no window) |

Default URL: `http://tensormi.localhost:3930`

| Path | Surface |
| --- | --- |
| `/` | **Chat** — conversations, projects, agent mode |
| `/settings` | **Server** — API providers (base URL, tokens, API style) |

## Data & config

On-disk paths still use the legacy folder name **`tensorUI`** (so renaming the product does not move or orphan chats). UI branding is TensorMI Harness.

| OS | Typical path |
| --- | --- |
| Windows | `%APPDATA%\tensorUI\` |
| macOS | `~/Library/Application Support/tensorUI/` |
| Linux | `~/.config/tensorUI/` |

| File / folder | Contents |
| --- | --- |
| `config.toml` | Boot and appearance configuration; provider definitions are removed when encryption is on |
| `chats.json` | Conversations and projects (encrypted when encryption is on) |
| `preferences.json` | Chat preferences (encrypted when encryption is on) |
| `provider-tokens.json` | Authenticated encrypted provider definitions and credentials (present only while encryption is on) |
| `encryption.json` | Salt / meta for disk encryption (no passphrase stored) |
| `encryption-transition.json` | Temporary authenticated recovery snapshot used only during encryption-mode changes |
| `chat-skills/skills.json` | Atomic skill snapshot (encrypted when encryption is on) |

Pass `--config PATH` to use a different `config.toml` (chats/preferences still sit beside it).

LLM system/tool prompts are markdown under [`prompts/`](prompts/) in this repo (compile-time `include_str!`) — see that folder’s README.

### Encryption at rest

In **Chat → Settings → Local Data**, enable encryption with a passphrase. TensorMI Harness then:

- Derives a 256-bit key with **Argon2id** (64 MiB memory, 3 iterations, one lane)
- Encrypts chats, preferences, provider definitions/credentials, and skill contents/metadata with **AES-256-GCM** (random 96-bit nonces and purpose-bound AAD)
- Stores only salt + KDF params + a key **verifier** in `encryption.json` (never the passphrase or raw key)
- Uses an authenticated, encrypted transition snapshot so interrupted enable/disable operations fail closed and can resume after the passphrase is entered
- Uses owner-private, flush-and-atomic-replace file writes; skill metadata and content commit as one snapshot
- Holds an operating-system-released exclusive data lock so concurrent app processes cannot interleave writes
- Keeps the session key in process memory only until you **Lock session** or quit (memory is zeroized on lock)
- Prompts you to unlock on launch (and after lock)

**Threat model:** offline confidentiality and integrity of chats, preferences, provider definitions/credentials, and skill data (stolen disk, backups, casual filesystem access).

**Not covered:** plaintext copies created before encryption, filesystem snapshots/backups, malware or another process in your logged-in session, rollback to an older complete encrypted data set by an attacker with write access, cold-boot/core-dump memory forensics while unlocked, forgotten passphrases, physical hardware failure, or network exposure of the loopback UI. Secure deletion cannot be guaranteed on SSDs or copy-on-write filesystems.

Encryption covers chats, preferences (including model pins and recent-model state), provider definitions and credentials, and skill contents stored on disk. Prefer a long passphrase; **forgotten passphrases cannot be recovered.**

In **Settings → Local Data** you can also open the data folder. Older browser-storage data and UI preferences are migrated to disk once and then removed from `localStorage`.

```toml
[ui]
host = "127.0.0.1"
port = 3930
theme = "dark"

[data]
storage = "disk"   # or "browser"

active_provider_id = "…"

[[providers]]
id = "…"
name = "ollama"
base = "http://127.0.0.1:11434/v1"
api_style = "openai"   # or "anthropic"; auto-detected in the Server UI on add/save
token = ""   # optional; leave empty for local servers that need no key
```

On the **Server** page, adding a provider probes the endpoint and picks OpenAI-compatible vs Anthropic Messages (host/`sk-ant-` hints, `/models` auth, and whether `/chat/completions` or `/messages` exists). Existing configs without `api_style` still default to `openai`. Anthropic-style providers talk to `{base}/messages` and translate streams so Chat stays on one SSE shape.

Env override for bind: `TENSORUI_BIND=127.0.0.1:3930`. Non-loopback binds (`0.0.0.0`, LAN IPs, etc.) print a warning and **refuse to start** — the UI has no authentication.

## Build

The Rust binary embeds Chat and Server HTML, the Chat CSS/JavaScript modules, `orb.js`, highlight.js, marked, DOMPurify, and icons:

```powershell
cargo build --release
```

That command builds the Rust application only. Web search can use the source checkout's `.venv` during development. The release workflow additionally builds `src/agent/ddgs_search.py` with PyInstaller and packages the resulting `tensorui-search` helper beside the Rust executable. To reproduce that helper locally:

```powershell
.\.venv\Scripts\python -m pip install -r requirements-search-build.txt
.\.venv\Scripts\python -m PyInstaller --noconfirm --clean --onefile --name tensorui-search --collect-all ddgs --distpath target/search-helper --workpath target/search-helper-build --specpath target src/agent/ddgs_search.py
```

On macOS/Linux, use `./.venv/bin/python` for those two commands. You can override helper discovery with `TENSORUI_SEARCH_HELPER=/path/to/tensorui-search`; otherwise TensorMI Harness checks beside its executable, the app-data `search-helper` folder, then source-development Python fallbacks.
