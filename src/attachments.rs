use base64::Engine;
use serde::{Deserialize, Serialize};

const MAX_EXTRACT_BYTES: usize = 8 * 1024 * 1024;
const MAX_BASE64_BYTES: usize = MAX_EXTRACT_BYTES.div_ceil(3) * 4;
const MAX_METADATA_CHARS: usize = 255;
const MAX_EXTRACTED_TEXT_CHARS: usize = 500_000;

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
    if filename.chars().count() > MAX_METADATA_CHARS {
        return Err(format!(
            "Attachment filename is too long (max {MAX_METADATA_CHARS} characters)"
        ));
    }
    let mime_raw = req.mime.as_deref().unwrap_or("").trim();
    if mime_raw.chars().count() > MAX_METADATA_CHARS {
        return Err(format!(
            "Attachment MIME type is too long (max {MAX_METADATA_CHARS} characters)"
        ));
    }
    let mime = mime_raw.to_ascii_lowercase();
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
        .unwrap_or(trimmed)
        .trim();
    if payload.len() > MAX_BASE64_BYTES + 4 {
        return Err(format!(
            "Attachment is too large to extract (max {} MB)",
            MAX_EXTRACT_BYTES / (1024 * 1024)
        ));
    }
    base64::engine::general_purpose::STANDARD
        .decode(payload)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(payload))
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

fn push_chars_limited(output: &mut String, text: &str, remaining: &mut usize) -> bool {
    for ch in text.chars() {
        if *remaining == 0 {
            return false;
        }
        output.push(ch);
        *remaining -= 1;
    }
    true
}

fn push_normalized_line(
    output: &mut String,
    line: &str,
    blank_run: &mut usize,
    remaining: &mut usize,
) -> bool {
    let trimmed_end = line.trim_end();
    if trimmed_end.is_empty() {
        *blank_run += 1;
        return *blank_run > 2 || push_chars_limited(output, "\n", remaining);
    }
    *blank_run = 0;
    push_chars_limited(output, trimmed_end, remaining)
        && push_chars_limited(output, "\n", remaining)
}

fn normalize_extracted_text(text: &str) -> String {
    let mut output = String::with_capacity(text.len().min(MAX_EXTRACTED_TEXT_CHARS));
    let mut blank_run = 0usize;
    let mut remaining = MAX_EXTRACTED_TEXT_CHARS;
    let mut truncated = false;
    let bytes = text.as_bytes();
    let mut line_start = 0usize;
    let mut index = 0usize;

    while index < bytes.len() {
        if matches!(bytes[index], b'\n' | b'\r') {
            if !push_normalized_line(
                &mut output,
                &text[line_start..index],
                &mut blank_run,
                &mut remaining,
            ) {
                truncated = true;
                break;
            }
            if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
                index += 1;
            }
            line_start = index + 1;
        }
        index += 1;
    }
    if !truncated
        && line_start < text.len()
        && !push_normalized_line(
            &mut output,
            &text[line_start..],
            &mut blank_run,
            &mut remaining,
        )
    {
        truncated = true;
    }

    let trimmed = output.trim();
    if !truncated {
        return trimmed.to_string();
    }
    const SUFFIX: &str = "\n\n…[extracted text truncated by safety limit]";
    let keep = MAX_EXTRACTED_TEXT_CHARS.saturating_sub(SUFFIX.chars().count());
    let mut clipped: String = trimmed.chars().take(keep).collect();
    clipped.push_str(SUFFIX);
    clipped
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

    #[test]
    fn rejects_oversized_attachment_metadata() {
        let out = extract_attachment(ExtractRequest {
            filename: Some("x".repeat(MAX_METADATA_CHARS + 1)),
            mime: Some("text/plain".into()),
            content_base64: String::new(),
        });
        assert!(out.unwrap_err().contains("filename is too long"));
    }

    #[test]
    fn extracted_text_is_capped() {
        let normalized = normalize_extracted_text(&"x".repeat(MAX_EXTRACTED_TEXT_CHARS + 50));
        assert_eq!(normalized.chars().count(), MAX_EXTRACTED_TEXT_CHARS);
        assert!(normalized.ends_with("[extracted text truncated by safety limit]"));
    }
}
