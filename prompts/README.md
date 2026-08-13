# LLM prompts

Markdown sources for every prompt TensorMI Harness sends to a model.

The app loads these at compile time (`include_str!`) via `src/prompts/`. Chat UI templates are also served as `/prompts.js` so `chat.html` can assemble the system prompt without duplicating wording.

## Conventions

- One file ≈ one prompt (or one reusable fragment).
- `{{placeholders}}` are filled by the caller (`{{name}}`, `{{memory}}`, …).
- File body is the literal prompt text — no YAML frontmatter.
- Prefer editing here over hunting through Rust/JS string literals.

## Layout

| Path | Used by |
| --- | --- |
| `chat/` | System prompt pieces assembled in the chat UI |
| `title/` | Auto-generated conversation titles |
| `agent/` | Agent mode / deep research system blocks & nudges |
| `tools/` | OpenAI-style tool `description` strings |
