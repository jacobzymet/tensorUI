# <img src="assets/browser-favicon.png" alt="" width="36" height="36"> tensorUI

A lightweight, more permissive alternative to [Open WebUI](https://github.com/open-webui/open-webui), a local frontend for LLM APIs you connect to, whether they run on your machine (Ollama, llama-server) or in the cloud (OpenAI, Gemini, Anthropic, and other OpenAI-compatible or **Anthropic Messages**–compatible endpoints).

**Encryption at rest** is a first-class feature: lock chats and preferences on disk with a passphrase (Argon2id + AES-256-GCM). After each restart you unlock once for the session; **Lock session** clears the key from memory without turning encryption off.

Turn models into agents with **Agent mode**: toggle **Agent** in the composer for every message, or `@web_search` / `@fetch_url` once, and the model can call **Agent Capabilities** — web search, URL fetch, custom skills, and more.

tensorUI does **not** run an inference server. You point it at a base URL such as `http://127.0.0.1:11434/v1`, `https://api.openai.com/v1`, or `https://api.anthropic.com/v1`, and it proxies chat through a loopback-only control plane.

## Requirements

- [Rust](https://www.rust-lang.org/tools/install) (for building from source)
- A modern web browser
- An OpenAI-compatible or Anthropic Messages–compatible API endpoint

## Run

```
cargo run -- --open
```

Default URL: `http://127.0.0.1:3930`

| Path | Surface |
| --- | --- |
| `/` | **Chat** — conversations, projects, agent mode |
| `/settings` | **Settings** — providers, appearance |

## Data & config

By default everything lives in the OS app data folder for **tensorUI**:

| OS | Typical path |
| --- | --- |
| Windows | `%APPDATA%\tensorUI\` |
| macOS | `~/Library/Application Support/tensorUI/` |
| Linux | `~/.config/tensorUI/` |

| File / folder | Contents |
| --- | --- |
| `config.toml` | Providers, API tokens, UI theme/fonts, storage mode |
| `chats.json` | Conversations and projects (encrypted when encryption is on) |
| `preferences.json` | Chat preferences (encrypted when encryption is on) |
| `encryption.json` | Salt / meta for disk encryption (no passphrase stored) |
| `chat-skills/` | Imported skill markdown |

Pass `--config PATH` to use a different `config.toml` (chats/preferences still sit beside it).

### Encryption at rest

In **Chat → Preferences → Local Data**, enable encryption with a passphrase. tensorUI then:

- Derives a 256-bit key with **Argon2id** (OWASP interactive defaults: 19 MiB memory, 2 iterations)
- Encrypts `chats.json` and `preferences.json` with **AES-256-GCM** (random 96-bit nonces, purpose-bound AAD so files cannot be swapped)
- Stores only salt + KDF params + a key **verifier** in `encryption.json` (never the passphrase or raw key)
- Keeps the session key in process memory only until you **Lock session** or quit (memory is zeroized on lock)
- Prompts you to unlock on launch (and after lock)

**Threat model:** offline confidentiality of chat/preference files (stolen disk, backups, casual filesystem access).

**Not covered:** provider API tokens in `config.toml`, skill files under `chat-skills/`, malware or another process in your logged-in session, cold-boot / memory forensics while unlocked, or network exposure of the loopback UI.

Encryption applies to **disk** storage only. Browser `localStorage` mode does not use it. Prefer a long passphrase; **forgotten passphrases cannot be recovered.**

In **Preferences → Local Data** you can also open the data folder, or optionally switch chats/preferences to browser `localStorage` only.

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
api_style = "openai"   # or "anthropic"; Settings auto-detects on add/save
token = ""   # optional; leave empty for local servers that need no key
```

When you add a provider in Settings, tensorUI probes the endpoint and picks OpenAI-compatible vs Anthropic Messages (host/`sk-ant-` hints, `/models` auth, and whether `/chat/completions` or `/messages` exists). Existing configs without `api_style` still default to `openai`. Anthropic-style providers talk to `{base}/messages` and translate streams so Chat stays on one SSE shape.

Env override for bind: `TENSORUI_BIND=127.0.0.1:3930`. Non-loopback binds (`0.0.0.0`, LAN IPs, etc.) print a warning and **refuse to start** — the UI has no authentication.

## Build

The release binary embeds Chat/Settings HTML, `orb.js`, highlight.js, marked, DOMPurify, and icons:

```powershell
cargo build --release
```