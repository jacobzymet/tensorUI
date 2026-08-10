//! Compile-time embed: the release binary ships alone — no HTML/JS/PNG sidecars.

pub const SETTINGS_HTML: &str = include_str!("../ui/settings.html");
pub const CHAT_HTML: &str = include_str!("../ui/chat.html");
pub const ORB_JS: &str = include_str!("../ui/orb.js");
pub const HIGHLIGHT_JS: &str = include_str!("../ui/vendor/highlight.min.js");
pub const MARKED_JS: &str = include_str!("../ui/vendor/marked.min.js");
pub const PURIFY_JS: &str = include_str!("../ui/vendor/purify.min.js");
pub const APP_ICON_PNG: &[u8] = include_bytes!("../../assets/browser-favicon.png");
pub const UI_MARK_DARK_PNG: &[u8] = include_bytes!("../../assets/icon-darkmode.png");
pub const UI_MARK_LIGHT_PNG: &[u8] = include_bytes!("../../assets/icon-lightmode.png");
