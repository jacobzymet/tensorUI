//! Load images the agent can put in a user-visible reply (`show_image`).

use std::time::Duration;

use base64::Engine;
use futures_util::StreamExt;
use serde_json::Value;
use tokio::time::timeout;

use super::fs::Workspace;
use crate::http;

const MAX_IMAGE_BYTES: usize = 1_500_000;
const FETCH_TIMEOUT: Duration = Duration::from_secs(20);

pub async fn load_image(args: &Value, workspace_root: &str) -> Result<(Vec<u8>, &'static str), String> {
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let url = args
        .get("url")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    match (path, url) {
        (Some(path), None) => load_workspace_image(workspace_root, path),
        (None, Some(url)) => load_remote_image(url).await,
        (Some(_), Some(_)) => Err("Pass either \"path\" or \"url\", not both.".into()),
        (None, None) => Err("show_image requires a workspace \"path\" or an http(s) \"url\".".into()),
    }
}

pub fn tool_summary(args: &Value) -> String {
    args.get("path")
        .and_then(|v| v.as_str())
        .or_else(|| args.get("url").and_then(|v| v.as_str()))
        .unwrap_or("")
        .trim()
        .to_string()
}

fn load_workspace_image(workspace_root: &str, path: &str) -> Result<(Vec<u8>, &'static str), String> {
    let ws = Workspace::open(workspace_root)?;
    let abs = ws.resolve(path)?;
    let meta = std::fs::symlink_metadata(&abs).map_err(|_| format!("File not found: {path}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to read a symlink.".into());
    }
    if !meta.is_file() {
        return Err(format!("{path} is not a file."));
    }
    if meta.len() > MAX_IMAGE_BYTES as u64 {
        return Err(format!(
            "Image is {} bytes; max is {MAX_IMAGE_BYTES} bytes.",
            meta.len()
        ));
    }
    let bytes = std::fs::read(&abs).map_err(|err| format!("Could not read {path}: {err}"))?;
    let mime = sniff_image_mime(&bytes).ok_or_else(|| {
        "That file is not a PNG, JPEG, GIF, or WebP image.".to_string()
    })?;
    Ok((bytes, mime))
}

async fn load_remote_image(url: &str) -> Result<(Vec<u8>, &'static str), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| {
        format!("Invalid URL '{url}'. Use an absolute http(s) address.")
    })?;
    match parsed.scheme() {
        "http" | "https" => {}
        other => {
            return Err(format!("Refusing {other} URL. show_image only fetches http(s)."));
        }
    }
    let response = timeout(FETCH_TIMEOUT, http::safe_public_get(parsed.as_str(), true))
        .await
        .map_err(|_| "Image fetch timed out.".to_string())?
        .map_err(|err| format!("Image fetch failed: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("Image fetch returned HTTP {}.", response.status()));
    }
    let bytes = timeout(FETCH_TIMEOUT, async move {
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|err| format!("Image fetch failed: {err}"))?;
            if bytes.len().saturating_add(chunk.len()) > MAX_IMAGE_BYTES {
                return Err(format!("Image exceeds the {MAX_IMAGE_BYTES}-byte limit."));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<_, String>(bytes)
    })
    .await
    .map_err(|_| "Image fetch timed out.".to_string())??;
    let mime = sniff_image_mime(&bytes).ok_or_else(|| {
        "That URL is not a PNG, JPEG, GIF, or WebP image.".to_string()
    })?;
    Ok((bytes, mime))
}

pub fn to_data_url(bytes: &[u8], mime: &str) -> String {
    format!(
        "data:{mime};base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes)
    )
}

pub fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G']) {
        return Some("image/png");
    }
    if bytes.len() >= 3 && bytes[0] == 0xFF && bytes[1] == 0xD8 && bytes[2] == 0xFF {
        return Some("image/jpeg");
    }
    if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_common_headers() {
        let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        png.extend_from_slice(&[0; 8]);
        assert_eq!(sniff_image_mime(&png), Some("image/png"));
        assert_eq!(sniff_image_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_image_mime(b"GIF89a...."), Some("image/gif"));
        let mut webp = b"RIFF".to_vec();
        webp.extend_from_slice(&[16, 0, 0, 0]);
        webp.extend_from_slice(b"WEBP");
        assert_eq!(sniff_image_mime(&webp), Some("image/webp"));
        assert_eq!(sniff_image_mime(b"not an image"), None);
    }
}
