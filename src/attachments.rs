use base64::Engine;
use serde::{Deserialize, Serialize};

const MAX_EXTRACT_BYTES: usize = 12 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct ExtractRequest {
    pub filename: Option<String>,
    pub mime: Option<String>,
    pub content_base64: String,
}

#[derive(Debug, Serialize)]
pub struct ExtractResponse {
    pub text: String,
    pub kind: &'static str,
    pub filename: String,
}

pub fn extract_attachment(req: ExtractRequest) -> Result<ExtractResponse, String> {
    let filename = req
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("attachment")
        .to_string();
    let mime = req.mime.as_deref().unwrap_or("").to_ascii_lowercase();
    let bytes = decode_base64(&req.content_base64)?;
    if bytes.len() > MAX_EXTRACT_BYTES {
        return Err(format!(
            "Attachment is too large to extract (max {} MB)",
            MAX_EXTRACT_BYTES / (1024 * 1024)
        ));
    }

    let lower_name = filename.to_ascii_lowercase();
    if mime.contains("pdf") || lower_name.ends_with(".pdf") {
        let text = extract_pdf_text(&bytes)?;
        return Ok(ExtractResponse {
            text: normalize_extracted_text(&text),
            kind: "pdf",
            filename,
        });
    }

    if mime.starts_with("text/")
        || mime == "application/json"
        || mime == "application/xml"
        || mime == "application/javascript"
        || looks_like_text_filename(&lower_name)
    {
        let text =
            String::from_utf8(bytes).map_err(|_| "File is not valid UTF-8 text".to_string())?;
        return Ok(ExtractResponse {
            text: normalize_extracted_text(&text),
            kind: "text",
            filename,
        });
    }

    Err("Unsupported file type for text extraction. Enable OCR for images, or attach a PDF/text file.".into())
}

fn decode_base64(raw: &str) -> Result<Vec<u8>, String> {
    let trimmed = raw.trim();
    let payload = trimmed
        .strip_prefix("data:")
        .and_then(|rest| rest.split_once(',').map(|(_, data)| data))
        .unwrap_or(trimmed);
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload.trim()))
        .map_err(|error| format!("Invalid base64 attachment payload: {error}"))
}

fn extract_pdf_text(bytes: &[u8]) -> Result<String, String> {
    pdf_extract::extract_text_from_mem(bytes)
        .map_err(|error| format!("PDF extract failed: {error}"))
}

fn looks_like_text_filename(name: &str) -> bool {
    const EXTS: &[&str] = &[
        ".txt",
        ".md",
        ".markdown",
        ".csv",
        ".tsv",
        ".json",
        ".jsonl",
        ".xml",
        ".html",
        ".htm",
        ".css",
        ".js",
        ".ts",
        ".tsx",
        ".jsx",
        ".py",
        ".rs",
        ".go",
        ".java",
        ".c",
        ".cpp",
        ".h",
        ".hpp",
        ".yml",
        ".yaml",
        ".toml",
        ".ini",
        ".log",
        ".sql",
        ".sh",
        ".bat",
        ".ps1",
    ];
    EXTS.iter().any(|ext| name.ends_with(ext))
}

fn normalize_extracted_text(text: &str) -> String {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut out = String::with_capacity(normalized.len());
    let mut blank_run = 0usize;
    for line in normalized.lines() {
        let trimmed_end = line.trim_end();
        if trimmed_end.is_empty() {
            blank_run += 1;
            if blank_run <= 2 {
                out.push('\n');
            }
            continue;
        }
        blank_run = 0;
        out.push_str(trimmed_end);
        out.push('\n');
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_text_payload() {
        let encoded = base64::engine::general_purpose::STANDARD.encode(b"hello attachment");
        let out = extract_attachment(ExtractRequest {
            filename: Some("note.txt".into()),
            mime: Some("text/plain".into()),
            content_base64: encoded,
        })
        .unwrap();
        assert_eq!(out.kind, "text");
        assert_eq!(out.text, "hello attachment");
    }
}
