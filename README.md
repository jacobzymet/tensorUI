# <img src="assets/browser-favicon.png" alt="" width="36" height="36"> TensorMI Harness

A local, lightweight, open source LLM harness for humanity.

A more permissive alternative to [Open WebUI](https://github.com/open-webui/open-webui), with:

- OpenAI-compatible and Anthropic Messages–compatible providers, local or cloud
- Chats, projects, custom bots, group chats, memory, model pins, and attachments
- Agent mode with approvals, web search, URL fetching, deep research, browser control, filesystem access, terminal access, and custom skills
- Optional local GGUF model management through an installed `llama-server`
- Passphrase-based encryption at rest for chats, preferences, provider credentials, and skills

TensorMI does not bundle an inference engine. Connect Ollama, OpenAI, Gemini, Anthropic, or another compatible endpoint; or install `llama-server` and launch a Hugging Face/GGUF model from **Settings → Providers → Local LLMs**.

## Install and run

Download a platform archive from [GitHub Releases](https://github.com/jacobzymet/tensorUI/releases). Releases include a standalone `tensorui` executable for Windows x64, Linux x64/ARM64, and macOS Apple Silicon/Intel.

To run from source:

```powershell
cargo run
```

This starts a loopback-only control plane and, by default, opens TensorMI Harness in a native desktop window.

| Option | Behavior |
| --- | --- |
| `--browser` | Open the UI in the default browser |
| `--headless` | Run without opening a window or browser |
| `--bind ADDR` | Override the loopback listen address |
| `--config PATH` | Use another `config.toml`; other data is stored beside it |

Default URL: `http://tensormi.localhost:3930`. `/settings` redirects to **Settings → Providers**.

### Platform requirements

- Building from source requires [Rust](https://www.rust-lang.org/tools/install).
- The Linux desktop window requires WebKitGTK 4.1; source builds also need its development packages:

```sh
sudo apt install pkg-config libglib2.0-dev libgtk-3-dev libwebkit2gtk-4.1-dev
```

- Using **Local LLMs** requires `llama-server` on `PATH`, or `TENSORUI_LLAMA_SERVER` set to its executable.
- The optional browser-control tool requires Chrome or Edge.

## Web search

Configure search under **Settings → Agent Capabilities → Web search**:

| Provider | Behavior |
| --- | --- |
| Auto | Uses a configured SearXNG instance first; otherwise DuckDuckGo |
| Parallel | Uses free MCP without a key, or the Search API with a key |
| TinyFish | Uses its Search API and requires a key |
| SearXNG | Uses the configured instance only |
| DuckDuckGo | Uses the HTML and Lite endpoints |

Parallel and TinyFish keys can be entered in Settings or supplied as `PARALLEL_API_KEY` and `TINYFISH_API_KEY`. Result count, recency, region, SafeSearch, and optional result-page fetching are configurable.

## Data and configuration

On-disk paths retain the legacy **`tensorUI`** folder name so upgrades do not orphan existing data:

| OS | Default directory |
| --- | --- |
| Windows | `%APPDATA%\tensorUI\` |
| macOS | `~/Library/Application Support/tensorUI/` |
| Linux | `~/.config/tensorUI/` |

| Path | Contents |
| --- | --- |
| `config.toml` | Bind, appearance, and unencrypted provider configuration |
| `chats.json` | Conversations and projects |
| `preferences.json` | Settings, model state, and search credentials |
| `provider-tokens.json` | Encrypted provider definitions and credentials; present only when encryption is enabled |
| `encryption.json` | Encryption salt, KDF parameters, and key verifier |
| `encryption-transition.json` | Recovery state used only during encryption-mode changes |
| `chat-skills/skills.json` | Atomic custom-skill snapshot |

Chats, preferences, and skills are encrypted in place when encryption is enabled. Provider configuration is then removed from plaintext `config.toml` and stored in `provider-tokens.json`.

The UI manages these settings, but a minimal configuration is:

```toml
[ui]
host = "127.0.0.1"
port = 3930
theme = "dark" # dark, light, or system

[data]
storage = "disk"

active_provider_id = "…"

[[providers]]
id = "…"
name = "ollama"
base = "http://127.0.0.1:11434/v1"
api_style = "openai" # openai or anthropic
token = ""           # optional for local endpoints
```

Provider API style is detected when a provider is added or saved. Existing entries without `api_style` default to `openai`.

Bind precedence is `--bind`, then `TENSORUI_BIND`, then `[ui]`. Network-reachable addresses are refused because the local UI has no authentication.

LLM system and tool prompts live under [`prompts/`](prompts/) and are embedded at compile time.

## Encryption at rest

Enable encryption under **Settings → Local Data**. TensorMI derives a 256-bit key with Argon2id (64 MiB, three iterations, one lane) and encrypts protected data with AES-256-GCM using random 96-bit nonces and purpose-bound authenticated data. The passphrase and raw key are never stored; the session key remains in memory until **Lock session** or exit and is then zeroized. Writes use private permissions, atomic replacement, and an exclusive data-directory lock.

The protection covers offline confidentiality and integrity of chats, preferences, provider definitions and credentials, and skills. It does not protect plaintext copies or backups made before encryption, filesystem snapshots, malware or another process in the logged-in session, rollback to an older complete encrypted data set, memory forensics while unlocked, forgotten passphrases, or hardware failure. Secure deletion cannot be guaranteed on SSDs or copy-on-write filesystems. **Forgotten passphrases cannot be recovered.**

## Build and verify

```powershell
cargo build --release
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
```

The binary embeds the UI, prompts, fonts loader, syntax highlighting, Markdown renderer, sanitizer, terminal assets, and icons.

Licensed under the [MIT License](LICENSE).
