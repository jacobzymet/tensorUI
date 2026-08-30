use std::{io, path::Path, process::Command};

/// True for http(s) and mailto URLs that are safe to hand to the OS opener.
pub fn is_openable_external_url(url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return false;
    }
    let scheme = url
        .split_once(':')
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    matches!(scheme.as_str(), "http" | "https" | "mailto")
}

/// Keep local UI routes in the webview; everything else belongs in the OS browser.
pub fn url_stays_in_webview(app_origin: &str, url: &str) -> bool {
    let url = url.trim();
    if url.is_empty() || url == "about:blank" || url.starts_with("about:") {
        return true;
    }
    let origin = app_origin.trim_end_matches('/');
    url == origin
        || url.starts_with(&(origin.to_owned() + "/"))
        || url.starts_with(&(origin.to_owned() + "?"))
        || url.starts_with(&(origin.to_owned() + "#"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openable_urls() {
        assert!(is_openable_external_url("https://x.ai/news"));
        assert!(is_openable_external_url("http://127.0.0.1:8787/"));
        assert!(is_openable_external_url("mailto:hi@example.com"));
        assert!(!is_openable_external_url("javascript:alert(1)"));
        assert!(!is_openable_external_url("file:///etc/passwd"));
        assert!(!is_openable_external_url("https://x.ai/news\n-a"));
    }

    #[test]
    fn webview_origin_check() {
        let origin = "http://127.0.0.1:8787";
        assert!(url_stays_in_webview(
            origin,
            "http://127.0.0.1:8787/settings"
        ));
        assert!(url_stays_in_webview(origin, "http://127.0.0.1:8787"));
        assert!(!url_stays_in_webview(origin, "https://x.ai/blog"));
        assert!(!url_stays_in_webview(origin, "http://127.0.0.1:8787.evil"));
    }
}

pub fn open_in_browser(url: &str) -> io::Result<()> {
    if !is_openable_external_url(url) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "unsupported url",
        ));
    }
    #[cfg(target_os = "windows")]
    {
        // URL metacharacters such as `&` are valid content but become command
        // separators if this is routed through cmd.exe.
        Command::new("explorer.exe").arg(url).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").args(["--", url]).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(url).spawn()?;
    }
    Ok(())
}

pub fn open_in_file_manager(path: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("explorer").arg(path).spawn()?;
    }
    #[cfg(target_os = "macos")]
    {
        Command::new("open").arg(path).spawn()?;
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        Command::new("xdg-open").arg(path).spawn()?;
    }
    Ok(())
}
