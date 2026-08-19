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

    #[test]
    fn code_block_controls_stay_visible_while_the_conversation_scrolls() {
        let code_block = CHAT_CSS
            .split(".msg-bubble .md-code-block {")
            .nth(1)
            .and_then(|css| css.split('}').next())
            .expect("code block styles should be embedded");
        let code_header = CHAT_CSS
            .split(".msg-bubble .md-code-header {")
            .nth(1)
            .and_then(|css| css.split('}').next())
            .expect("code header styles should be embedded");

        assert!(code_block.contains("overflow: clip;"));
        assert!(code_header.contains("position: sticky;"));
        assert!(code_header.contains("top: 0;"));
        assert!(code_header.contains("z-index: 2;"));
    }

    #[test]
    fn streaming_code_blocks_render_and_highlight_before_completion() {
        assert!(CHAT_JS.contains("function renderHighlightedCode(text, language)"));
        assert!(
            CHAT_JS.contains("window.hljs.highlight(source, { language, ignoreIllegals: true })")
        );
        assert!(CHAT_JS.contains("window.hljs.highlightAuto(source)"));
        assert!(CHAT_JS.contains("data-highlighted=\"yes"));
        assert!(!CHAT_JS.contains("highlight: !streaming"));
    }

    #[test]
    fn chat_and_server_sidebars_use_the_same_width() {
        const SIDEBAR_WIDTH: &str = "--sidebar-w: 17.75rem;";
        assert!(CHAT_CSS.contains(SIDEBAR_WIDTH));
        assert!(SETTINGS_HTML.contains(SIDEBAR_WIDTH));
    }

    #[test]
    fn composer_scrim_tracks_the_combined_background_tone() {
        let dark_background = CHAT_CSS
            .split(".chat-main[data-background-tone=\"dark\"] {")
            .nth(1)
            .and_then(|css| css.split('}').next())
            .expect("dark background tone styles should be embedded");
        let composer_scrim = CHAT_CSS
            .split(".chat-composer-dock::before {")
            .nth(1)
            .and_then(|css| css.split('}').next())
            .expect("composer scrim styles should be embedded");

        assert!(dark_background.contains("--chat-composer-scrim: oklch(5% 0.006 260);"));
        assert!(composer_scrim.contains("var(--chat-composer-scrim)"));
        assert!(!composer_scrim.contains("var(--color-paper)"));
    }

    #[test]
    fn locked_chat_ui_labels_provider_state_as_unavailable() {
        assert!(CHAT_JS.contains("network.inference_mode === 'locked'"));
        assert!(CHAT_JS.contains("serverProviderName.textContent = 'Encrypted'"));
        assert!(CHAT_JS.contains("serverChip.textContent = 'Locked'"));
    }
}
