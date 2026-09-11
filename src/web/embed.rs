//! Compile-time embed: the release binary ships alone — no HTML/JS/PNG sidecars.

pub const CHAT_HTML: &str = include_str!("../ui/chat.html");
pub const CHAT_CSS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.css"));
pub const CHAT_JS: &str = include_str!(concat!(env!("OUT_DIR"), "/chat.js"));
pub const ORB_JS: &str = include_str!("../ui/orb.js");
pub const HIGHLIGHT_JS: &str = include_str!("../ui/vendor/highlight.min.js");
pub const MARKED_JS: &str = include_str!("../ui/vendor/marked.min.js");
pub const PURIFY_JS: &str = include_str!("../ui/vendor/purify.min.js");
pub const OPTIONAL_FONTS_JS: &str = include_str!("../ui/optional-fonts.js");
pub const XTERM_JS: &str = include_str!("../ui/vendor/xterm.min.js");
pub const XTERM_FIT_JS: &str = include_str!("../ui/vendor/xterm-addon-fit.min.js");
pub const XTERM_CSS: &str = include_str!("../ui/vendor/xterm.css");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/browser-favicon.png");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_ui_does_not_block_paint_on_google_fonts() {
        assert!(!CHAT_HTML.contains("fonts.googleapis.com"));
        assert!(!CHAT_HTML.contains("fonts.gstatic.com"));
        assert!(CHAT_HTML.contains("/optional-fonts.js"));
        assert!(OPTIONAL_FONTS_JS.contains("fonts.googleapis.com"));
        assert!(OPTIONAL_FONTS_JS.contains("media = 'print'"));
    }

    #[test]
    fn chat_ui_does_not_use_local_storage() {
        for blob in [CHAT_JS, CHAT_HTML] {
            assert!(
                !blob.contains("localStorage"),
                "chat UI must not read or write localStorage"
            );
        }
        assert!(!CHAT_HTML.contains("settingBrowserStorage"));
    }

    #[test]
    fn transient_model_catalogs_do_not_prune_saved_picker_state() {
        assert!(!CHAT_JS.contains("prunePinnedModels("));
        assert!(!CHAT_JS.contains("pruneRecentModels("));
        assert!(CHAT_JS.contains("pinnedModelIds: pinnedModelIds.slice()"));
        assert!(CHAT_JS.contains("remote_catalog_pending"));
        assert!(CHAT_JS.contains("if (!selectedChatModel && catalogComplete && !menuOpen)"));
        assert!(!CHAT_JS.contains("!allValues.includes(selectedChatModel)"));
    }

    #[test]
    fn model_selector_uses_deployment_neutral_network_label() {
        let model_menu = CHAT_HTML
            .split("id=\"chatModelMenu\"")
            .nth(1)
            .and_then(|html| html.split("id=\"chatModelList\"").next())
            .expect("model menu markup should be embedded");
        assert!(model_menu.contains("data-model-tab=\"network\">Network"));
        assert!(!model_menu.contains("Cloud"));
        assert!(CHAT_JS.contains("function modelMenuHasNetwork("));
        assert!(!CHAT_JS.contains("modelMenuHasCloud"));
    }

    #[test]
    fn startup_never_sends_through_a_transient_fallback_model() {
        assert!(CHAT_JS.contains("function selectedModelIsReady("));
        assert!(CHAT_JS.contains("&& modelReady"));
        assert!(
            CHAT_JS.contains("if (!modelCatalogIsComplete(data?.network, options)) return null;")
        );
        assert!(CHAT_JS.contains("if (!serverReady || !selectedTurnRemote?.ready)"));
        assert!(CHAT_JS.contains("if (!remote?.ready)"));
        assert!(CHAT_JS.contains("turnModel: remote.model"));
        assert!(CHAT_JS.contains("contextualModelError("));
    }

    #[test]
    fn sent_user_prompt_pins_while_the_thread_scrolls() {
        assert!(!CHAT_HTML.contains("id=\"userPromptPin\""));
        assert!(!CHAT_HTML.contains("id=\"userPromptPinBubble\""));
        assert!(!CHAT_JS.contains("function syncUserPromptPin("));
        assert!(!CHAT_JS.contains("function pinUserPrompt("));
        assert!(CHAT_JS.contains("function syncPinnedUserPrompt("));
        assert!(CHAT_JS.contains("row === active"));
        assert!(CHAT_CSS.contains(".is-pinned-prompt"));
        assert!(CHAT_CSS.contains("position: sticky;"));
        assert!(CHAT_CSS.contains("max-height: min(42vh, calc(var(--thread-visible-h, 70dvh) - 1.25rem));"));
        assert!(CHAT_CSS.contains("overflow-y: auto;"));
        assert!(CHAT_JS.contains("--prompt-layout-h"));
        assert!(!CHAT_CSS.contains("container-type: scroll-state;"));
        assert!(!CHAT_CSS.contains("@container scroll-state(stuck: top)"));
        assert!(CHAT_CSS.contains(".msg.msg-role-user:not(.is-pinned-prompt):not(.msg-queued)"));
        assert!(CHAT_CSS.contains(":not([data-surface=\"bots\"]) #chatThread"));
        assert!(!CHAT_CSS.contains(".msg.msg-role-user.is-stuck"));
    }

    #[test]
    fn sidebar_exposes_bulk_conversation_actions() {
        for id in [
            "btnManageConvos",
            "sidebarBulkActions",
            "btnBulkPinConvos",
            "btnBulkDeleteConvos",
        ] {
            assert!(CHAT_HTML.contains(&format!("id=\"{id}\"")));
        }
        assert!(CHAT_JS.contains("function bulkSetSelectedConversationsPinned("));
        assert!(CHAT_JS.contains("function bulkDeleteSelectedConversations("));
    }

    #[test]
    fn settings_copy_does_not_use_em_dashes() {
        let settings = CHAT_HTML
            .split("id=\"settingsModal\"")
            .nth(1)
            .and_then(|html| html.split("id=\"unlockModal\"").next())
            .expect("settings markup should be embedded");
        assert!(!settings.contains('—'));
    }

    #[test]
    fn sidebar_account_avatar_has_a_solid_background() {
        let avatar = CHAT_CSS
            .split(".sidebar-account-avatar {")
            .nth(1)
            .and_then(|css| css.split('}').next())
            .expect("sidebar account avatar styles should be embedded");
        assert!(avatar.contains("background: color-mix("));
        assert!(!avatar.contains("gradient("));
    }

    #[test]
    fn live_loading_animation_survives_thread_navigation() {
        assert!(CHAT_JS.contains("const priorMounts = Number(stream.domMountCount) || 0;"));
        assert!(CHAT_JS.contains("thinkingLabel.style.animationDelay"));
        assert!(CHAT_JS.contains("if (priorMounts === 0)"));
        assert!(CHAT_JS.contains("stream.domMountCount = priorMounts + 1;"));
        assert!(CHAT_JS.contains("stream.statusLabel = baseLabel;"));
        assert!(CHAT_JS.contains("function syncConvoBusyRingPhase("));
        assert!(CHAT_JS.contains("--convo-busy-delay"));
        assert!(CHAT_CSS.contains("animation-delay: var(--convo-busy-delay, 0ms);"));
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
    fn provider_manager_is_integrated_into_settings() {
        assert!(CHAT_HTML.contains("data-settings-pane=\"providers\""));
        assert!(CHAT_HTML.contains("id=\"providerList\""));
        assert!(!CHAT_HTML.contains("id=\"localLlmBody\""));
        assert!(CHAT_JS.contains("function bindProviderSettings("));
        assert!(!CHAT_HTML.contains("settings-providers-frame"));
        assert!(!CHAT_HTML.contains("data-src=\"/settings?embedded=1\""));
        assert!(!CHAT_HTML.contains("id=\"btnProviders\""));
        assert!(!CHAT_HTML.contains("class=\"mode-switch\""));
    }

    #[test]
    fn chat_composer_is_in_the_empty_state_on_first_paint() {
        let after_inner = CHAT_HTML
            .split("id=\"emptyStateInner\"")
            .nth(1)
            .and_then(|rest| rest.split("id=\"threadWrap\"").next())
            .expect("empty-state markup should wrap the landing composer");
        assert!(
            after_inner.contains("id=\"composerShell\""),
            "composer must live in #emptyStateInner so / first-paints centered"
        );
        let after_shell = CHAT_HTML
            .split("id=\"chatShell\"")
            .nth(1)
            .and_then(|rest| rest.split("</main>").nth(1))
            .expect("markup after the chat shell");
        assert!(
            !after_shell.contains("id=\"composerShell\""),
            "composer must not be a body sibling of .chat-shell"
        );
        assert!(CHAT_CSS.contains("body > .composer-shell"));
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
    fn transparent_content_uses_the_custom_background_tone() {
        for selector in [
            ".notifications-title",
            ".notifications-lede",
            ".notification-group-heading",
            ".notifications-empty",
            ".notifications-top > .btn",
            ".empty-eyebrow",
            ".msg-speaker",
            ".msg-footer .msg-action",
            ".think-block > summary",
        ] {
            assert!(
                CHAT_CSS.contains(selector),
                "missing adaptive foreground selector: {selector}"
            );
        }
        assert!(CHAT_CSS.contains("color: var(--chat-background-ink);"));
        assert!(CHAT_CSS.contains("color: var(--chat-background-muted);"));
    }

    #[test]
    fn steered_replies_print_after_user_notes() {
        assert!(CHAT_JS.contains("function insertBeforeLiveReply("));
        assert!(CHAT_JS.contains("function placeLiveAssistantRow("));
        assert!(CHAT_JS.contains("function sealSteerThinkAndDiscardDraft("));
        assert!(CHAT_JS.contains("followUpStart: lastAsst + 2 + steerCount"));
    }

    #[test]
    fn update_toast_can_install_in_place() {
        assert!(CHAT_HTML.contains("id=\"btnUpdateInstall\""));
        assert!(CHAT_HTML.contains("data-settings-pane=\"app\""));
        assert!(CHAT_HTML.contains("id=\"btnAppUpdateInstall\""));
        assert!(CHAT_JS.contains("function installAppUpdate("));
        assert!(CHAT_JS.contains("/api/updates/apply"));
        assert!(CHAT_JS.contains("status.can_install"));
        assert!(CHAT_JS.contains("function refreshAppUpdatePane("));
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
    fn locked_chat_ui_disables_inference() {
        assert!(CHAT_JS.contains("network.inference_mode === 'locked'"));
        assert!(CHAT_JS.contains("serverReady = false;"));
        assert!(CHAT_JS.contains("syncComposerThinkVisibility(null);"));
    }

    #[test]
    fn user_terminal_is_a_pty_emulator_not_a_command_log() {
        assert!(CHAT_HTML.contains("/xterm.min.js"));
        assert!(CHAT_HTML.contains("id=\"chatTerminalViewports\""));
        assert!(!CHAT_JS.contains("isClearCommand"));
        assert!(!CHAT_JS.contains("/api/terminal/exec"));
        assert!(CHAT_JS.contains("/api/terminal/ws/"));
    }
}
