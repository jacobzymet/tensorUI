//! Compile-time embed: the release binary ships alone — no HTML/JS/PNG sidecars.

pub const SETTINGS_HTML: &str = include_str!("../ui/settings.html");
pub const CHAT_HTML: &str = include_str!("../ui/chat.html");
pub const CHAT_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.css"));
pub const CHAT_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.js"));
pub const ORB_JS: &str = include_str!("../ui/orb.js");
pub const HIGHLIGHT_JS: &str = include_str!("../ui/vendor/highlight.min.js");
pub const MARKED_JS: &str = include_str!("../ui/vendor/marked.min.js");
pub const PURIFY_JS: &str = include_str!("../ui/vendor/purify.min.js");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/browser-favicon.png");
pub const UI_MARK_DARK_PNG: &[u8] = include_bytes!("../../assets/icon-darkmode.png");
pub const UI_MARK_LIGHT_PNG: &[u8] = include_bytes!("../../assets/icon-lightmode.png");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ui_does_not_write_application_state_to_local_storage() {
        assert!(!CHAT_JS.contains("localStorage.setItem"));
        assert!(!SETTINGS_HTML.contains("localStorage.setItem"));
        assert!(!CHAT_HTML.contains("settingBrowserStorage"));
    }

    #[test]
    fn transient_model_catalogs_do_not_prune_saved_picker_state() {
        assert!(!CHAT_JS.contains("prunePinnedModels("));
        assert!(!CHAT_JS.contains("pruneRecentModels("));
        assert!(CHAT_JS.contains("pinnedModelIds: pinnedModelIds.slice()"));
    }
}
