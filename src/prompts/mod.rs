//! Compile-time LLM prompt library.
//!
//! Sources live in `/prompts/**/*.md`. Edit those files — not string literals —
//! when changing model-facing wording. Use [`fill`] for `{{placeholders}}`.

use std::sync::OnceLock;

/// Replace `{{key}}` placeholders. Unknown keys are left untouched.
pub fn fill(template: &str, vars: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in vars {
        let needle = format!("{{{{{key}}}}}");
        out = out.replace(&needle, value);
    }
    // Markdown files usually end with a trailing newline; prompts should not.
    out.trim_end().to_string()
}

pub fn trim_prompt(raw: &str) -> &str {
    raw.trim_end()
}

pub mod chat {
    pub const BASE: &str = include_str!("../../prompts/chat/base.md");
    pub const TODAY: &str = include_str!("../../prompts/chat/today.md");
    pub const USER_NAME: &str = include_str!("../../prompts/chat/user-name.md");
    pub const USER_ABOUT: &str = include_str!("../../prompts/chat/user-about.md");
    pub const GLOBAL_INSTRUCTIONS: &str = include_str!("../../prompts/chat/global-instructions.md");
    pub const PROJECT_INSTRUCTIONS: &str =
        include_str!("../../prompts/chat/project-instructions.md");
    pub const GLOBAL_MEMORY: &str = include_str!("../../prompts/chat/global-memory.md");
    pub const PROJECT_MEMORY: &str = include_str!("../../prompts/chat/project-memory.md");
    pub const PROJECT_MEMORY_SCOPE_PROJECT_ONLY: &str =
        include_str!("../../prompts/chat/project-memory-scope-project-only.md");
    pub const PROJECT_MEMORY_SCOPE_DEFAULT: &str =
        include_str!("../../prompts/chat/project-memory-scope-default.md");
    pub const PROJECT_CONTINUITY: &str = include_str!("../../prompts/chat/project-continuity.md");
    pub const BOT_IDENTITY: &str = include_str!("../../prompts/chat/bot-identity.md");
    pub const BOT_MEMORY: &str = include_str!("../../prompts/chat/bot-memory.md");
    pub const BOT_GROUP: &str = include_str!("../../prompts/chat/bot-group.md");
    pub const BOT_GROUP_MEMORY: &str = include_str!("../../prompts/chat/bot-group-memory.md");
    pub const BOT_DM: &str = include_str!("../../prompts/chat/bot-dm.md");
    pub const BOT_HOLD: &str = include_str!("../../prompts/chat/bot-hold.md");
    pub const BOT_COMPACT: &str = include_str!("../../prompts/chat/bot-compact.md");
    pub const BOT_COMPACT_GROUP: &str = include_str!("../../prompts/chat/bot-compact-group.md");
}

pub mod title {
    pub const SYSTEM: &str = include_str!("../../prompts/title/system.md");
    pub const USER: &str = include_str!("../../prompts/title/user.md");
}

pub mod agent {
    pub const INTRO: &str = include_str!("../../prompts/agent/intro.md");
    pub const INTRO_DEEP_RESEARCH: &str =
        include_str!("../../prompts/agent/intro-deep-research.md");
    pub const CORE: &str = include_str!("../../prompts/agent/core.md");
    pub const STEER: &str = include_str!("../../prompts/agent/steer.md");
    pub const IMAGES: &str = include_str!("../../prompts/agent/images.md");
    pub const REQUIRED_TOOLS: &str = include_str!("../../prompts/agent/required-tools.md");
    pub const POLICY_DEEP_RESEARCH: &str =
        include_str!("../../prompts/agent/policy-deep-research.md");
    pub const POLICY_OPTIONAL: &str = include_str!("../../prompts/agent/policy-optional.md");
    pub const WEB_SEARCH: &str = include_str!("../../prompts/agent/web-search.md");
    pub const WEB_SEARCH_DEPTH_OFF: &str =
        include_str!("../../prompts/agent/web-search-depth-off.md");
    pub const WEB_SEARCH_DEPTH_ON: &str =
        include_str!("../../prompts/agent/web-search-depth-on.md");
    pub const WEB_SEARCH_FOLLOW_FETCH: &str =
        include_str!("../../prompts/agent/web-search-follow-fetch.md");
    pub const FETCH_URL: &str = include_str!("../../prompts/agent/fetch-url.md");
    pub const FETCH_URL_WITH_SEARCH: &str =
        include_str!("../../prompts/agent/fetch-url-with-search.md");
    pub const FETCH_URL_ALONE: &str = include_str!("../../prompts/agent/fetch-url-alone.md");
    pub const CITATIONS: &str = include_str!("../../prompts/agent/citations.md");
    pub const FILESYSTEM: &str = include_str!("../../prompts/agent/filesystem.md");
    pub const FILESYSTEM_NO_WORKSPACE: &str =
        include_str!("../../prompts/agent/filesystem-no-workspace.md");
    pub const TERMINAL: &str = include_str!("../../prompts/agent/terminal.md");
    pub const TERMINAL_NO_WORKSPACE: &str =
        include_str!("../../prompts/agent/terminal-no-workspace.md");
    pub const BROWSER: &str = include_str!("../../prompts/agent/browser.md");
    pub const SKILLS_ACTIVATE: &str = include_str!("../../prompts/agent/skills-activate.md");
    pub const SKILLS_CATALOG_INTRO: &str =
        include_str!("../../prompts/agent/skills-catalog-intro.md");
    pub const SKILLS_CATALOG_FOOTER: &str =
        include_str!("../../prompts/agent/skills-catalog-footer.md");
    pub const SKILLS_NEED_AGENT: &str = include_str!("../../prompts/agent/skills-need-agent.md");
    pub const DEEP_RESEARCH: &str = include_str!("../../prompts/agent/deep-research.md");
    pub const DEEP_RESEARCH_OUTPUT_LONG: &str =
        include_str!("../../prompts/agent/deep-research-output-long.md");
    pub const DEEP_RESEARCH_OUTPUT_BRIEF: &str =
        include_str!("../../prompts/agent/deep-research-output-brief.md");
    pub const CONTINUE_ENOUGH: &str = include_str!("../../prompts/agent/continue-enough.md");
    pub const CONTINUE_LONG: &str = include_str!("../../prompts/agent/continue-long.md");
    pub const CONTINUE_BRIEF: &str = include_str!("../../prompts/agent/continue-brief.md");
    pub const NUDGE_FORCE_FINAL: &str = include_str!("../../prompts/agent/nudge-force-final.md");
    pub const NUDGE_STOP_AND_ANSWER: &str =
        include_str!("../../prompts/agent/nudge-stop-and-answer.md");
    pub const NUDGE_EMPTY_DEEP: &str = include_str!("../../prompts/agent/nudge-empty-deep.md");
    pub const NUDGE_EMPTY: &str = include_str!("../../prompts/agent/nudge-empty.md");
    pub const NUDGE_ASK_USER_FIRST: &str =
        include_str!("../../prompts/agent/nudge-ask-user-first.md");
    pub const NUDGE_MUST_SEARCH: &str = include_str!("../../prompts/agent/nudge-must-search.md");
    pub const NUDGE_REQUIRED_TOOLS: &str =
        include_str!("../../prompts/agent/nudge-required-tools.md");
}

pub mod tools {
    pub const ASK_USER: &str = include_str!("../../prompts/tools/ask-user.md");
    pub const WEB_SEARCH: &str = include_str!("../../prompts/tools/web-search.md");
    pub const WEB_SEARCH_DEEP: &str = include_str!("../../prompts/tools/web-search-deep.md");
    pub const WEB_SEARCH_FETCH_SUFFIX: &str =
        include_str!("../../prompts/tools/web-search-fetch-suffix.md");
    pub const FETCH_URL: &str = include_str!("../../prompts/tools/fetch-url.md");
    pub const FETCH_URL_WITH_SEARCH: &str =
        include_str!("../../prompts/tools/fetch-url-with-search.md");
    pub const ACTIVATE_SKILL: &str = include_str!("../../prompts/tools/activate-skill.md");
    pub const READ_FILE: &str = include_str!("../../prompts/tools/read-file.md");
    pub const LIST_DIR: &str = include_str!("../../prompts/tools/list-dir.md");
    pub const GLOB: &str = include_str!("../../prompts/tools/glob.md");
    pub const GREP: &str = include_str!("../../prompts/tools/grep.md");
    pub const WRITE_FILE: &str = include_str!("../../prompts/tools/write-file.md");
    pub const STR_REPLACE: &str = include_str!("../../prompts/tools/str-replace.md");
    pub const DELETE_FILE: &str = include_str!("../../prompts/tools/delete-file.md");
    pub const RUN_TERMINAL: &str = include_str!("../../prompts/tools/run-terminal.md");
    pub const BROWSER_NAVIGATE: &str = include_str!("../../prompts/tools/browser-navigate.md");
    pub const BROWSER_SNAPSHOT: &str = include_str!("../../prompts/tools/browser-snapshot.md");
    pub const BROWSER_CLICK: &str = include_str!("../../prompts/tools/browser-click.md");
    pub const BROWSER_TYPE: &str = include_str!("../../prompts/tools/browser-type.md");
    pub const BROWSER_PRESS: &str = include_str!("../../prompts/tools/browser-press.md");
    pub const BROWSER_WAIT: &str = include_str!("../../prompts/tools/browser-wait.md");
    pub const BROWSER_SCREENSHOT: &str = include_str!("../../prompts/tools/browser-screenshot.md");
    pub const BROWSER_EVALUATE: &str = include_str!("../../prompts/tools/browser-evaluate.md");
    pub const BROWSER_CLOSE: &str = include_str!("../../prompts/tools/browser-close.md");
    pub const SHOW_IMAGE: &str = include_str!("../../prompts/tools/show-image.md");
}

/// Chat UI templates served as `/prompts.js` (`window.TENSORUI_PROMPTS`).
pub fn frontend_js() -> &'static str {
    static JS: OnceLock<String> = OnceLock::new();
    JS.get_or_init(|| {
        let entries: &[(&str, &str)] = &[
            ("chat.base", chat::BASE),
            ("chat.today", chat::TODAY),
            ("chat.userName", chat::USER_NAME),
            ("chat.userAbout", chat::USER_ABOUT),
            ("chat.globalInstructions", chat::GLOBAL_INSTRUCTIONS),
            ("chat.projectInstructions", chat::PROJECT_INSTRUCTIONS),
            ("chat.globalMemory", chat::GLOBAL_MEMORY),
            ("chat.projectMemory", chat::PROJECT_MEMORY),
            (
                "chat.projectMemoryScopeProjectOnly",
                chat::PROJECT_MEMORY_SCOPE_PROJECT_ONLY,
            ),
            (
                "chat.projectMemoryScopeDefault",
                chat::PROJECT_MEMORY_SCOPE_DEFAULT,
            ),
            ("chat.projectContinuity", chat::PROJECT_CONTINUITY),
            ("chat.botIdentity", chat::BOT_IDENTITY),
            ("chat.botMemory", chat::BOT_MEMORY),
            ("chat.botGroup", chat::BOT_GROUP),
            ("chat.botGroupMemory", chat::BOT_GROUP_MEMORY),
            ("chat.botDm", chat::BOT_DM),
            ("chat.botHold", chat::BOT_HOLD),
            ("chat.botCompact", chat::BOT_COMPACT),
            ("chat.botCompactGroup", chat::BOT_COMPACT_GROUP),
        ];
        let mut map = serde_json::Map::new();
        for (key, raw) in entries {
            map.insert(
                (*key).into(),
                serde_json::Value::String(trim_prompt(raw).to_string()),
            );
        }
        let json = serde_json::Value::Object(map).to_string();
        // Keep fillPrompt in a raw string so regex escapes stay readable.
        const FILL_HELPER: &str = r#"
window.fillPrompt = function (template, vars) {
  vars = vars || {};
  return String(template || '').replace(/\{\{\s*([\w.]+)\s*\}\}/g, function (_, key) {
    return Object.prototype.hasOwnProperty.call(vars, key) ? String(vars[key]) : '';
  });
};
"#;
        format!("window.TENSORUI_PROMPTS = {json};\n{FILL_HELPER}")
    })
    .as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_replaces_placeholders() {
        assert_eq!(fill("Hello {{name}}!", &[("name", "Ada")]), "Hello Ada!");
    }

    #[test]
    fn frontend_js_exposes_chat_prompts() {
        let js = frontend_js();
        assert!(js.contains("window.TENSORUI_PROMPTS"));
        assert!(js.contains("chat.today"));
        assert!(js.contains("chat.projectMemory"));
        assert!(js.contains("chat.botIdentity"));
        assert!(js.contains("chat.botDm"));
        assert!(js.contains("chat.botHold"));
        assert!(js.contains("fillPrompt"));
        assert!(js.contains(r"/\{\{\s*([\w.]+)\s*\}\}/g"));
    }
}
