//! Compile-time embed: the release binary ships alone — no HTML/JS/PNG sidecars.

pub const SETTINGS_HTML: &str = include_str!("../ui/settings.html");
pub const CHAT_HTML: &str = include_str!("../ui/chat.html");
pub const CHAT_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.css"));
pub const CHAT_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.js"));
pub const ORB_JS: &str = include_str!("../ui/orb.js");
pub const HIGHLIGHT_JS: &str = include_str!("../ui/vendor/highlight.min.js");
pub const MARKED_JS: &str = include_str!("../ui/vendor/marked.min.js");
pub const PURIFY_JS: &str = include_str!("../ui/vendor/purify.min.js");
pub const OPTIONAL_FONTS_JS: &str = include_str!("../ui/optional-fonts.js");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/browser-favicon.png");
pub const UI_MARK_DARK_PNG: &[u8] = include_bytes!("../../assets/icon-darkmode.png");
pub const UI_MARK_LIGHT_PNG: &[u8] = include_bytes!("../../assets/icon-lightmode.png");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ui_does_not_block_paint_on_google_fonts() {
        for html in [CHAT_HTML, SETTINGS_HTML] {
            assert!(!html.contains("fonts.googleapis.com"));
            assert!(!html.contains("fonts.gstatic.com"));
            assert!(html.contains("/optional-fonts.js"));
        }
        assert!(OPTIONAL_FONTS_JS.contains("fonts.googleapis.com"));
        assert!(OPTIONAL_FONTS_JS.contains("media = 'print'"));
    }

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
    fn custom_backgrounds_remove_the_composer_scrim() {
        let custom_background_scrim = CHAT_CSS
            .split(".chat-main[data-background-tone] .chat-composer-dock::before {")
            .nth(1)
            .and_then(|css| css.split('}').next())
            .expect("custom background composer styles should be embedded");

        assert!(custom_background_scrim.contains("display: none;"));
    }

    #[test]
    fn persisted_writes_stay_under_no_keepalive_quota() {
        // Chats and the background image exceed the browser's 64 KiB keepalive quota,
        // which silently rejects the request and freezes preferences on disk.
        assert!(!CHAT_JS.contains("keepalive:"));
        assert!(CHAT_JS.contains("putJsonWithRetry('/api/data/store'"));
        assert!(CHAT_JS.contains("putJsonWithRetry('/api/data/preferences'"));
    }

    #[test]
    fn locked_chat_ui_labels_provider_state_as_unavailable() {
        assert!(CHAT_JS.contains("network.inference_mode === 'locked'"));
        assert!(CHAT_JS.contains("serverProviderName.textContent = 'Encrypted'"));
        assert!(CHAT_JS.contains("serverChip.textContent = 'Locked'"));
    }
}
