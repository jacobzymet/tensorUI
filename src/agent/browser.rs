//! Agent-controlled Chromium over CDP (`chromiumoxide`), Playwright-style tools.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use base64::Engine;
use chromiumoxide::{
    browser::{Browser, BrowserConfig},
    cdp::browser_protocol::page::CaptureScreenshotFormat,
    page::{Page, ScreenshotParams},
};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::Mutex;

use super::fs::Workspace;
use crate::store;

const MAX_EVAL_CHARS: usize = 8_000;
const MAX_TYPE_CHARS: usize = 8_000;
const MAX_SNAPSHOT_ITEMS: usize = 180;
const NAV_TIMEOUT: Duration = Duration::from_secs(45);
const ACTION_TIMEOUT: Duration = Duration::from_secs(20);
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(40);
const MAX_EMBED_PNG_BYTES: usize = 1_500_000;

pub struct BrowserOutcome {
    pub text: String,
    pub image_data_url: Option<String>,
}

impl From<String> for BrowserOutcome {
    fn from(text: String) -> Self {
        Self {
            text,
            image_data_url: None,
        }
    }
}

pub const TOOL_NAMES: &[&str] = &[
    "browser_navigate",
    "browser_snapshot",
    "browser_click",
    "browser_type",
    "browser_press",
    "browser_wait",
    "browser_screenshot",
    "browser_evaluate",
    "browser_close",
];

static SESSIONS: OnceLock<Mutex<HashMap<String, Session>>> = OnceLock::new();

fn sessions() -> &'static Mutex<HashMap<String, Session>> {
    SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

struct Session {
    browser: Browser,
    page: Page,
}

pub fn is_browser_tool(name: &str) -> bool {
    TOOL_NAMES.contains(&name)
}

pub fn tool_summary(name: &str, args: &Value) -> String {
    match name {
        "browser_navigate" => arg_str(args, "url").unwrap_or_default(),
        "browser_click" | "browser_type" | "browser_press" | "browser_wait" => arg_str(args, "ref")
            .or_else(|| arg_str(args, "selector"))
            .or_else(|| arg_str(args, "key"))
            .unwrap_or_default(),
        "browser_evaluate" => arg_str(args, "expression")
            .map(|s| s.chars().take(80).collect())
            .unwrap_or_default(),
        "browser_screenshot" => arg_str(args, "path").unwrap_or_else(|| "screenshot".into()),
        _ => String::new(),
    }
}

pub fn ensure_http_url(raw: &str) -> Result<String, String> {
    let url = raw.trim();
    if url.is_empty() {
        return Err("browser_navigate requires a non-empty \"url\".".into());
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| {
        format!("Invalid URL '{url}'. Use an absolute http(s) address.")
    })?;
    match parsed.scheme() {
        "http" | "https" => Ok(parsed.to_string()),
        other => Err(format!(
            "Refusing {other} URL. The browser tool only opens http(s) pages."
        )),
    }
}

pub fn session_key(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "default".into();
    }
    trimmed
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .take(80)
        .collect()
}

pub async fn execute(name: &str, args: &Value, session_id: &str, workspace_root: &str) -> Result<BrowserOutcome, String> {
    match name {
        "browser_close" => close_session(session_id).await.map(BrowserOutcome::from),
        "browser_navigate" => {
            let url = ensure_http_url(&arg_str(args, "url").unwrap_or_default())?;
            with_page(session_id, |page| async move {
                timeout(NAV_TIMEOUT, page.goto(url.as_str()))
                    .await
                    .map_err(|_| "Navigation timed out.".to_string())?
                    .map_err(|err| format!("Navigation failed: {err}"))?;
                let _ = page.wait_for_navigation().await;
                page_location(&page).await
            })
            .await
            .map(BrowserOutcome::from)
        }
        "browser_snapshot" => {
            with_page(session_id, |page| async move { snapshot(&page).await })
                .await
                .map(BrowserOutcome::from)
        }
        "browser_click" => {
            let ref_id = require_ref(args)?;
            with_page(session_id, |page| async move {
                let result = eval_json(
                    &page,
                    &format!(
                        r#"() => {{
  const el = document.querySelector('[data-tensor-ref="{ref_id}"]');
  if (!el) return {{ ok: false, error: 'Unknown ref {ref_id}. Call browser_snapshot first.' }};
  el.scrollIntoView({{ block: 'center', inline: 'nearest' }});
  if (el instanceof HTMLElement) el.click();
  else el.dispatchEvent(new MouseEvent('click', {{ bubbles: true, cancelable: true, view: window }}));
  return {{ ok: true }};
}}"#
                    ),
                )
                .await?;
                ensure_ok(&result)?;
                tokio::time::sleep(Duration::from_millis(250)).await;
                snapshot(&page).await
            })
            .await
            .map(BrowserOutcome::from)
        }
        "browser_type" => {
            let ref_id = require_ref(args)?;
            let text = arg_str(args, "text").ok_or_else(|| {
                "browser_type requires a \"text\" string.".to_string()
            })?;
            if text.chars().count() > MAX_TYPE_CHARS {
                return Err(format!("text is too long (max {MAX_TYPE_CHARS} characters)."));
            }
            let submit = args.get("submit").and_then(|v| v.as_bool()).unwrap_or(false);
            let escaped = json!(text).to_string();
            with_page(session_id, move |page| async move {
                let result = eval_json(
                    &page,
                    &format!(
                        r#"() => {{
  const el = document.querySelector('[data-tensor-ref="{ref_id}"]');
  if (!el) return {{ ok: false, error: 'Unknown ref {ref_id}. Call browser_snapshot first.' }};
  el.scrollIntoView({{ block: 'center', inline: 'nearest' }});
  el.focus();
  const text = {escaped};
  if ('value' in el) {{
    el.value = text;
    el.dispatchEvent(new Event('input', {{ bubbles: true }}));
    el.dispatchEvent(new Event('change', {{ bubbles: true }}));
  }} else if (el.isContentEditable) {{
    el.textContent = text;
    el.dispatchEvent(new InputEvent('input', {{ bubbles: true }}));
  }} else {{
    return {{ ok: false, error: 'Ref {ref_id} is not an input.' }};
  }}
  if ({submit}) {{
    const form = el.closest && el.closest('form');
    if (form) form.requestSubmit ? form.requestSubmit() : form.submit();
    else el.dispatchEvent(new KeyboardEvent('keydown', {{ key: 'Enter', code: 'Enter', bubbles: true }}));
  }}
  return {{ ok: true }};
}}"#
                    ),
                )
                .await?;
                ensure_ok(&result)?;
                tokio::time::sleep(Duration::from_millis(250)).await;
                snapshot(&page).await
            })
            .await
            .map(BrowserOutcome::from)
        }
        "browser_press" => {
            let key = arg_str(args, "key").ok_or_else(|| {
                "browser_press requires a \"key\" string (e.g. Enter, Tab, Escape).".to_string()
            })?;
            if key.chars().count() > 40 || key.contains(['\'', '\\', '\n', '"']) {
                return Err("Invalid key.".into());
            }
            let ref_js = match arg_str(args, "ref") {
                Some(r) => {
                    let r = sanitize_ref(&r)?;
                    format!(r#"document.querySelector('[data-tensor-ref="{r}"]')"#)
                }
                None => "document.activeElement".into(),
            };
            with_page(session_id, move |page| async move {
                let result = eval_json(
                    &page,
                    &format!(
                        r#"() => {{
  const el = {ref_js} || document.body;
  if (!el) return {{ ok: false, error: 'Nothing to send the key to.' }};
  el.focus && el.focus();
  const opts = {{ key: '{key}', bubbles: true, cancelable: true }};
  el.dispatchEvent(new KeyboardEvent('keydown', opts));
  el.dispatchEvent(new KeyboardEvent('keyup', opts));
  return {{ ok: true }};
}}"#
                    ),
                )
                .await?;
                ensure_ok(&result)?;
                tokio::time::sleep(Duration::from_millis(200)).await;
                snapshot(&page).await
            })
            .await
            .map(BrowserOutcome::from)
        }
        "browser_wait" => {
            let ms = args
                .get("time_ms")
                .and_then(|v| v.as_u64())
                .unwrap_or(1000)
                .clamp(50, 30_000);
            let selector = arg_str(args, "selector");
            with_page(session_id, move |page| async move {
                if let Some(sel) = selector {
                    if sel.len() > 300 || sel.contains(['\n', '`']) {
                        return Err("Invalid selector.".into());
                    }
                    timeout(Duration::from_millis(ms.max(200)), page.find_element(sel))
                        .await
                        .map_err(|_| "Timed out waiting for selector.".to_string())?
                        .map_err(|err| format!("Wait failed: {err}"))?;
                } else {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                }
                snapshot(&page).await
            })
            .await
            .map(BrowserOutcome::from)
        }
        "browser_screenshot" => {
            let path = arg_str(args, "path");
            let (text, bytes) = with_page(session_id, move |page| async move {
                let bytes = timeout(
                    ACTION_TIMEOUT,
                    page.screenshot(
                        ScreenshotParams::builder()
                            .format(CaptureScreenshotFormat::Png)
                            .full_page(false)
                            .build(),
                    ),
                )
                .await
                .map_err(|_| "Screenshot timed out.".to_string())?
                .map_err(|err| format!("Screenshot failed: {err}"))?;
                let saved = save_screenshot(workspace_root, path.as_deref(), &bytes)?;
                let loc = page_location(&page).await.unwrap_or_default();
                Ok((
                    format!(
                        "Screenshot captured ({} bytes). It is shown in Live Activity. Saved to {saved}\n{loc}",
                        bytes.len()
                    ),
                    bytes,
                ))
            })
            .await?;
            Ok(png_outcome(text, bytes))
        }
        "browser_evaluate" => {
            let expression = arg_str(args, "expression").ok_or_else(|| {
                "browser_evaluate requires an \"expression\" string.".to_string()
            })?;
            if expression.chars().count() > MAX_EVAL_CHARS {
                return Err(format!(
                    "expression is too long (max {MAX_EVAL_CHARS} characters)."
                ));
            }
            let wrapped = format!("() => {{\nreturn ({expression});\n}}");
            with_page(session_id, move |page| async move {
                let value = timeout(ACTION_TIMEOUT, page.evaluate_function(wrapped))
                    .await
                    .map_err(|_| "Evaluate timed out.".to_string())?
                    .map_err(|err| format!("Evaluate failed: {err}"))?;
                let parsed: Value = value.into_value().unwrap_or(Value::Null);
                let text = serde_json::to_string_pretty(&parsed).unwrap_or_else(|_| parsed.to_string());
                let clipped: String = text.chars().take(12_000).collect();
                Ok(clipped)
            })
            .await
            .map(BrowserOutcome::from)
        }
        other => Err(format!("Unknown browser tool '{other}'.")),
    }
}

pub async fn close_session(session_id: &str) -> Result<String, String> {
    let key = session_key(session_id);
    let mut map = sessions().lock().await;
    if let Some(mut session) = map.remove(&key) {
        let _ = session.browser.close().await;
        Ok("Browser closed.".into())
    } else {
        Ok("No open browser session.".into())
    }
}

async fn with_page<F, Fut, T>(session_id: &str, f: F) -> Result<T, String>
where
    F: FnOnce(Page) -> Fut,
    Fut: std::future::Future<Output = Result<T, String>>,
{
    let page = ensure_session(session_id).await?;
    timeout(NAV_TIMEOUT, f(page))
        .await
        .map_err(|_| "Browser action timed out.".to_string())?
}

async fn ensure_session(session_id: &str) -> Result<Page, String> {
    let key = session_key(session_id);
    let mut map = sessions().lock().await;
    if let Some(session) = map.get(&key)
        && page_alive(&session.page).await
    {
        return Ok(session.page.clone());
    }
    map.remove(&key);
    let session = launch_session(&key).await?;
    let page = session.page.clone();
    map.insert(key, session);
    Ok(page)
}

async fn page_alive(page: &Page) -> bool {
    page.evaluate("1+1")
        .await
        .ok()
        .and_then(|v| v.into_value::<i32>().ok())
        == Some(2)
}

async fn launch_session(session_id: &str) -> Result<Session, String> {
    let mut builder = BrowserConfig::builder().with_head().window_size(1280, 800);
    if let Some(exe) = find_browser_executable() {
        builder = builder.chrome_executable(exe);
    }
    let profile = profile_dir(session_id);
    let _ = fs::create_dir_all(&profile);
    builder = builder.user_data_dir(profile);
    let config = builder
        .build()
        .map_err(|err| format!("Could not configure Chrome: {err}"))?;
    let (browser, mut handler) = timeout(LAUNCH_TIMEOUT, Browser::launch(config))
        .await
        .map_err(|_| "Timed out launching Chrome/Edge. Is a Chromium browser installed?".to_string())?
        .map_err(|err| {
            format!(
                "Could not launch Chrome/Edge ({err}). Install Google Chrome or Microsoft Edge, or set CHROME_PATH to the executable."
            )
        })?;
    tokio::spawn(async move {
        while let Some(next) = handler.next().await {
            if next.is_err() {
                break;
            }
        }
    });
    let page = browser
        .new_page("about:blank")
        .await
        .map_err(|err| format!("Could not open a tab: {err}"))?;
    Ok(Session { browser, page })
}

async fn snapshot(page: &Page) -> Result<String, String> {
    let value = timeout(
        ACTION_TIMEOUT,
        page.evaluate_function(SNAPSHOT_JS),
    )
    .await
    .map_err(|_| "Snapshot timed out.".to_string())?
    .map_err(|err| format!("Snapshot failed: {err}"))?;
    let data: Value = value
        .into_value()
        .map_err(|err| format!("Snapshot was not JSON: {err}"))?;
    Ok(format_snapshot(&data))
}

async fn page_location(page: &Page) -> Result<String, String> {
    let value = page
        .evaluate_function("() => ({ url: location.href, title: document.title })")
        .await
        .map_err(|err| format!("Could not read page URL: {err}"))?;
    let data: Value = value.into_value().unwrap_or(Value::Null);
    let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
    Ok(format!("title: {title}\nurl: {url}"))
}

async fn eval_json(page: &Page, js: &str) -> Result<Value, String> {
    let value = timeout(ACTION_TIMEOUT, page.evaluate_function(js.to_string()))
        .await
        .map_err(|_| "Page script timed out.".to_string())?
        .map_err(|err| format!("Page script failed: {err}"))?;
    value
        .into_value()
        .map_err(|err| format!("Page script did not return JSON: {err}"))
}

fn ensure_ok(value: &Value) -> Result<(), String> {
    if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
        Ok(())
    } else {
        Err(value
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Browser action failed.")
            .to_string())
    }
}

fn format_snapshot(data: &Value) -> String {
    let url = data.get("url").and_then(|v| v.as_str()).unwrap_or("");
    let title = data.get("title").and_then(|v| v.as_str()).unwrap_or("");
    let mut lines = vec![
        format!("title: {title}"),
        format!("url: {url}"),
        "interactive:".into(),
    ];
    let items = data.get("items").and_then(|v| v.as_array());
    if let Some(items) = items {
        for item in items.iter().take(MAX_SNAPSHOT_ITEMS) {
            let r = item.get("ref").and_then(|v| v.as_str()).unwrap_or("?");
            let tag = item.get("tag").and_then(|v| v.as_str()).unwrap_or("div");
            let typ = item
                .get("type")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let text = item
                .get("text")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let href = item
                .get("href")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let mut line = format!("  - [{r}] {tag}");
            if let Some(typ) = typ {
                line.push(' ');
                line.push_str(typ);
            }
            if let Some(name) = name {
                line.push_str(" name=");
                line.push_str(name);
            }
            if let Some(text) = text {
                line.push_str(" \"");
                line.push_str(text);
                line.push('"');
            }
            if let Some(href) = href {
                line.push_str(" href=");
                line.push_str(href);
            }
            lines.push(line);
        }
        if items.len() > MAX_SNAPSHOT_ITEMS {
            lines.push(format!(
                "  … {} more controls omitted",
                items.len() - MAX_SNAPSHOT_ITEMS
            ));
        }
    }
    if items.map(|i| i.is_empty()).unwrap_or(true) {
        lines.push("  (none)".into());
    }
    lines.join("\n")
}

fn png_outcome(text: String, bytes: Vec<u8>) -> BrowserOutcome {
    let image_data_url = if bytes.is_empty() || bytes.len() > MAX_EMBED_PNG_BYTES {
        None
    } else {
        Some(format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&bytes)
        ))
    };
    BrowserOutcome {
        text,
        image_data_url,
    }
}

fn save_screenshot(
    workspace_root: &str,
    requested: Option<&str>,
    bytes: &[u8],
) -> Result<String, String> {
    let rel = requested
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            format!("screenshots/browser-{ts}.png")
        });
    if let Ok(ws) = Workspace::open(workspace_root) {
        let abs = ws.resolve(&rel)?;
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).map_err(|err| format!("Could not create screenshot folder: {err}"))?;
        }
        fs::write(&abs, bytes).map_err(|err| format!("Could not write screenshot: {err}"))?;
        return Ok(ws.relative_display(&abs));
    }
    let dir = store::data_dir().join("browser-screenshots");
    fs::create_dir_all(&dir).map_err(|err| format!("Could not create screenshot folder: {err}"))?;
    let name = Path::new(&rel)
        .file_name()
        .and_then(|n| n.to_str())
        .filter(|n| n.ends_with(".png") || n.ends_with(".jpg"))
        .unwrap_or("browser.png");
    let abs = dir.join(name);
    fs::write(&abs, bytes).map_err(|err| format!("Could not write screenshot: {err}"))?;
    Ok(abs.display().to_string())
}

fn profile_dir(session_id: &str) -> PathBuf {
    store::data_dir().join("browser-profiles").join(session_key(session_id))
}

pub fn find_browser_executable() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("CHROME_PATH") {
        let path = PathBuf::from(path.trim());
        if path.is_file() {
            return Some(path);
        }
    }
    for candidate in browser_candidates() {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn browser_candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let local = PathBuf::from(local);
        out.push(local.join(r"Google\Chrome\Application\chrome.exe"));
        out.push(local.join(r"Microsoft\Edge\Application\msedge.exe"));
        out.push(local.join(r"Chromium\Application\chrome.exe"));
    }
    if let Some(pf) = std::env::var_os("PROGRAMFILES") {
        let pf = PathBuf::from(pf);
        out.push(pf.join(r"Google\Chrome\Application\chrome.exe"));
        out.push(pf.join(r"Microsoft\Edge\Application\msedge.exe"));
        out.push(pf.join(r"Chromium\Application\chrome.exe"));
    }
    if let Some(pf) = std::env::var_os("PROGRAMFILES(X86)") {
        let pf = PathBuf::from(pf);
        out.push(pf.join(r"Google\Chrome\Application\chrome.exe"));
        out.push(pf.join(r"Microsoft\Edge\Application\msedge.exe"));
    }
    out.push(PathBuf::from(
        r"/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ));
    out.push(PathBuf::from(
        r"/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    ));
    out.push(PathBuf::from(
        r"/Applications/Chromium.app/Contents/MacOS/Chromium",
    ));
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in [
                "google-chrome",
                "google-chrome-stable",
                "chromium",
                "chromium-browser",
                "microsoft-edge",
                "msedge",
                "chrome",
            ] {
                out.push(dir.join(name));
            }
        }
    }
    out
}

fn arg_str(args: &Value, key: &str) -> Option<String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn require_ref(args: &Value) -> Result<String, String> {
    let raw = arg_str(args, "ref").ok_or_else(|| {
        "This action needs \"ref\" from the latest browser_snapshot (e.g. e12).".to_string()
    })?;
    sanitize_ref(&raw)
}

fn sanitize_ref(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    if s.len() <= 24
        && s.starts_with('e')
        && s[1..].chars().all(|c| c.is_ascii_digit())
        && s.len() > 1
    {
        Ok(s.to_string())
    } else {
        Err("ref must look like e12 from browser_snapshot.".into())
    }
}

async fn timeout<T, E>(
    dur: Duration,
    fut: impl std::future::Future<Output = Result<T, E>>,
) -> Result<Result<T, E>, ()> {
    tokio::time::timeout(dur, fut).await.map_err(|_| ())
}

const SNAPSHOT_JS: &str = r#"() => {
  const sel = 'a[href], button, input, textarea, select, summary, [role="button"], [role="link"], [role="tab"], [role="menuitem"], [role="checkbox"], [role="textbox"], [contenteditable="true"]';
  const items = [];
  const seen = new Set();
  let i = 0;
  for (const el of document.querySelectorAll(sel)) {
    if (seen.has(el)) continue;
    const style = getComputedStyle(el);
    const r = el.getBoundingClientRect();
    if (style.display === 'none' || style.visibility === 'hidden' || el.disabled) continue;
    if (r.width < 1 && r.height < 1) continue;
    seen.add(el);
    i += 1;
    const ref = 'e' + i;
    el.setAttribute('data-tensor-ref', ref);
    const tag = el.tagName.toLowerCase();
    const type = (el.getAttribute('type') || '').toLowerCase();
    const name = (el.getAttribute('aria-label') || el.getAttribute('name') || el.getAttribute('placeholder') || '').trim().slice(0, 80);
    const text = ((el.innerText || el.value || el.getAttribute('alt') || '') + '').replace(/\s+/g, ' ').trim().slice(0, 120);
    const href = el.href || '';
    items.push({ ref, tag, type, name, text, href });
    if (items.length >= 180) break;
  }
  return { url: location.href, title: document.title, items };
}"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_http_urls() {
        assert!(ensure_http_url("file:///etc/passwd").is_err());
        assert!(ensure_http_url("javascript:alert(1)").is_err());
        assert!(ensure_http_url("https://example.com/x").is_ok());
        assert!(ensure_http_url("").is_err());
    }

    #[test]
    fn refs_must_be_snapshot_ids() {
        assert_eq!(sanitize_ref("e12").unwrap(), "e12");
        assert!(sanitize_ref("body").is_err());
        assert!(sanitize_ref("e12;alert(1)").is_err());
    }

    #[test]
    fn session_keys_are_safe_path_segments() {
        assert_eq!(session_key("abc/../x"), "abc____x");
        assert_eq!(session_key(""), "default");
    }

    #[test]
    fn tool_list_is_complete() {
        assert!(is_browser_tool("browser_navigate"));
        assert!(is_browser_tool("browser_snapshot"));
        assert!(!is_browser_tool("fetch_url"));
    }
}
