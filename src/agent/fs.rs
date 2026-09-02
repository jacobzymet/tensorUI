//! Workspace-scoped file tools. Paths are confined to `workspace_root`.

use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufRead, BufReader, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, RwLock, Weak},
};

use serde_json::Value;

const MAX_READ_BYTES: usize = 32_000;
const DEFAULT_READ_LINES: usize = 200;
const MAX_WRITE_BYTES: usize = 1_000_000;
const MAX_EDIT_BYTES: usize = 16 * 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 500;
const MAX_GLOB_MATCHES: usize = 200;
const MAX_GLOB_PATTERN_CHARS: usize = 512;
const MAX_GREP_MATCHES: usize = 80;
const MAX_GREP_FILES: usize = 80;
const MAX_GREP_FILE_BYTES: u64 = 1_000_000;
const MAX_GREP_QUERY_CHARS: usize = 4_096;
const SKIP_DIR_NAMES: &[&str] = &[
    ".git",
    "node_modules",
    "target",
    "dist",
    "build",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
    pub(super) access: Arc<RwLock<()>>,
}

impl Workspace {
    pub fn open(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Err(
                "No workspace folder is set for this chat. Choose a folder from the composer + menu or the terminal."
                    .into(),
            );
        }
        let path = PathBuf::from(trimmed);
        if !path.is_absolute() {
            return Err("Workspace folder must be an absolute path.".into());
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| {
            format!(
                "Workspace folder does not exist or is not accessible: {}",
                path.display()
            )
        })?;
        if meta.file_type().is_symlink() {
            return Err("Workspace folder cannot be a symlink.".into());
        }
        if !meta.is_dir() {
            return Err("Workspace path is not a directory.".into());
        }
        let root = canonicalize_dir(&path)?;
        static LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<RwLock<()>>>>> = OnceLock::new();
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        locks.retain(|_, lock| lock.strong_count() > 0);
        let access = locks.get(&root).and_then(Weak::upgrade).unwrap_or_else(|| {
            let lock = Arc::new(RwLock::new(()));
            locks.insert(root.clone(), Arc::downgrade(&lock));
            lock
        });
        Ok(Self { root, access })
    }

    pub fn root_display(&self) -> String {
        display_path(&self.root)
    }

    pub fn resolve(&self, raw: &str) -> Result<PathBuf, String> {
        resolve_under(&self.root, raw)
    }

    pub fn relative_display(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .map(|rel| {
                if rel.as_os_str().is_empty() {
                    ".".into()
                } else {
                    rel.to_string_lossy().replace('\\', "/")
                }
            })
            .unwrap_or_else(|_| display_path(path))
    }
}

pub fn read_file(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .read()
        .map_err(|_| "Workspace access lock poisoned")?;
    let path = arg_path(args, "path")?;
    let abs = ws.resolve(&path)?;
    let meta = fs::symlink_metadata(&abs).map_err(|_| format!("File not found: {path}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to read a symlink.".into());
    }
    if !meta.is_file() {
        return Err(format!("{path} is not a file."));
    }
    let offset = arg_usize(args, "offset").unwrap_or(0);
    let limit = arg_usize(args, "limit")
        .unwrap_or(DEFAULT_READ_LINES)
        .clamp(1, 2000);
    let max_bytes = arg_usize(args, "max_bytes")
        .unwrap_or(MAX_READ_BYTES)
        .clamp(256, MAX_READ_BYTES);
    let byte_offset = args.get("byte_offset").and_then(Value::as_u64).unwrap_or(0);
    if byte_offset > 0 && offset > 0 {
        return Err("Use either offset (lines) or byte_offset, not both.".into());
    }
    if byte_offset > meta.len() {
        return Err("byte_offset is past the end of the file.".into());
    }
    let file = File::open(&abs).map_err(|err| format!("Could not read {path}: {err}"))?;
    let mut reader = BufReader::new(file);
    reader
        .seek(SeekFrom::Start(byte_offset))
        .map_err(|err| err.to_string())?;
    let page = read_page(&mut reader, offset, limit, max_bytes)
        .map_err(|err| format!("Could not read {path}: {err}"))?;
    let next = byte_offset + page.consumed;
    let label = ws.relative_display(&abs);
    let continuation = if page.more {
        format!(
            "More content available: call read_file with path={path:?}, byte_offset={next}. Do not combine it with offset.\n"
        )
    } else {
        "End of file.\n".into()
    };
    Ok(format!(
        "{label} ({} bytes; page starts at line offset {offset}, byte offset {byte_offset})\n{continuation}--- file content ---\n{}",
        meta.len(),
        page.text
    ))
}

struct ReadPage {
    text: String,
    consumed: u64,
    more: bool,
}

// Only buffer the requested page. Even an enormous single line is bounded;
// the byte cursor lets the next call resume without rescanning earlier pages.
fn read_page(
    reader: &mut impl BufRead,
    offset: usize,
    limit: usize,
    max_bytes: usize,
) -> Result<ReadPage, String> {
    let mut consumed = 0u64;
    let mut skipped = 0usize;
    while skipped < offset {
        let buf = reader.fill_buf().map_err(|err| err.to_string())?;
        if buf.is_empty() {
            break;
        }
        let take = buf
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(buf.len());
        if buf[take - 1] == b'\n' {
            skipped += 1;
        }
        reader.consume(take);
        consumed += take as u64;
    }
    let mut bytes = Vec::new();
    let mut lines = 0;
    while lines < limit && bytes.len() < max_bytes {
        let buf = reader.fill_buf().map_err(|err| err.to_string())?;
        if buf.is_empty() {
            break;
        }
        let available = buf.len().min(max_bytes - bytes.len());
        let take = buf[..available]
            .iter()
            .position(|&b| b == b'\n')
            .map(|i| i + 1)
            .unwrap_or(available);
        if buf[take - 1] == b'\n' {
            lines += 1;
        }
        bytes.extend_from_slice(&buf[..take]);
        reader.consume(take);
    }
    let more = !reader.fill_buf().map_err(|err| err.to_string())?.is_empty();
    if bytes.contains(&0) {
        return Err("Refusing to read binary content.".into());
    }
    // A byte-limited page can end inside UTF-8. Leave that partial character
    // for the next byte-offset request rather than returning corrupt text.
    let mut valid = match std::str::from_utf8(&bytes) {
        Ok(_) => bytes.len(),
        Err(err) if err.error_len().is_none() && more => err.valid_up_to(),
        Err(_) => {
            return Err("Content is not valid UTF-8, or byte_offset is inside a character.".into());
        }
    };
    if more && valid > 0 && bytes[valid - 1] == b'\r' {
        valid -= 1;
    }
    let more = more || valid < bytes.len();
    bytes.truncate(valid);
    consumed += valid as u64;
    let text = String::from_utf8(bytes)
        .map_err(|err| err.to_string())?
        .replace("\r\n", "\n");
    Ok(ReadPage {
        text,
        consumed,
        more,
    })
}

pub fn list_dir(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .read()
        .map_err(|_| "Workspace access lock poisoned")?;
    let path = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let abs = ws.resolve(path)?;
    let meta = fs::symlink_metadata(&abs).map_err(|_| format!("Directory not found: {path}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to list a symlink.".into());
    }
    if !meta.is_dir() {
        return Err(format!("{path} is not a directory."));
    }
    let mut entries: Vec<(bool, String, u64)> = Vec::new();
    for entry in fs::read_dir(&abs).map_err(|err| format!("Could not list {path}: {err}"))? {
        let entry = entry.map_err(|err| format!("Could not list {path}: {err}"))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == "." || name == ".." {
            continue;
        }
        let ft = entry.file_type().ok();
        let is_dir = ft.map(|t| t.is_dir()).unwrap_or(false);
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        entries.push((is_dir, name, size));
        if entries.len() >= MAX_LIST_ENTRIES {
            break;
        }
    }
    entries.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(a.1.to_lowercase().cmp(&b.1.to_lowercase()))
    });
    let mut lines = vec![format!(
        "Listing {} ({} entries):",
        ws.relative_display(&abs),
        entries.len()
    )];
    for (is_dir, name, size) in entries {
        if is_dir {
            lines.push(format!("  {name}/"));
        } else {
            lines.push(format!("  {name}  ({size} bytes)"));
        }
    }
    Ok(lines.join("\n"))
}

pub fn glob_files(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .read()
        .map_err(|_| "Workspace access lock poisoned")?;
    let pattern = args
        .get("pattern")
        .or_else(|| args.get("glob"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "glob requires a non-empty \"pattern\" string.".to_string())?;
    if pattern.chars().count() > MAX_GLOB_PATTERN_CHARS {
        return Err(format!(
            "glob pattern is too long (max {MAX_GLOB_PATTERN_CHARS} characters)."
        ));
    }
    let mut matches = Vec::new();
    walk_files(&ws.root, &ws.root, &mut |rel, _abs, is_dir| {
        if is_dir {
            return matches.len() < MAX_GLOB_MATCHES;
        }
        if glob_match(pattern, &rel) {
            matches.push(rel);
        }
        matches.len() < MAX_GLOB_MATCHES
    })?;
    matches.sort();
    if matches.is_empty() {
        return Ok(format!("No files matched `{pattern}` under the workspace."));
    }
    let mut out = format!("{} files matching `{pattern}`:\n", matches.len());
    out.push_str(&matches.join("\n"));
    Ok(out)
}

pub fn grep_files(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .read()
        .map_err(|_| "Workspace access lock poisoned")?;
    let query = args
        .get("query")
        .or_else(|| args.get("pattern"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "grep requires a non-empty \"query\" string.".to_string())?;
    if query.chars().count() > MAX_GREP_QUERY_CHARS {
        return Err(format!(
            "grep query is too long (max {MAX_GREP_QUERY_CHARS} characters)."
        ));
    }
    let glob = args
        .get("glob")
        .or_else(|| args.get("include"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let case_insensitive = args
        .get("case_insensitive")
        .or_else(|| args.get("i"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let needle = if case_insensitive {
        query.to_ascii_lowercase()
    } else {
        query.to_string()
    };

    let mut hits: Vec<String> = Vec::new();
    let mut files_hit = 0usize;
    walk_files(&ws.root, &ws.root, &mut |rel, abs, is_dir| {
        if is_dir || hits.len() >= MAX_GREP_MATCHES || files_hit >= MAX_GREP_FILES {
            return hits.len() < MAX_GREP_MATCHES && files_hit < MAX_GREP_FILES;
        }
        if let Some(glob) = glob
            && !glob_match(glob, &rel)
        {
            return true;
        }
        let Ok(meta) = fs::symlink_metadata(abs) else {
            return true;
        };
        if meta.file_type().is_symlink() || !meta.is_file() || meta.len() > MAX_GREP_FILE_BYTES {
            return true;
        }
        let Ok(bytes) = fs::read(abs) else {
            return true;
        };
        if bytes.contains(&0) {
            return true;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            return true;
        };
        let mut file_hits = 0usize;
        for (idx, line) in text.lines().enumerate() {
            let hay = if case_insensitive {
                line.to_ascii_lowercase()
            } else {
                line.to_string()
            };
            if hay.contains(&needle) {
                hits.push(format!("{rel}:{}:{line}", idx + 1));
                file_hits += 1;
                if hits.len() >= MAX_GREP_MATCHES {
                    break;
                }
            }
        }
        if file_hits > 0 {
            files_hit += 1;
        }
        hits.len() < MAX_GREP_MATCHES && files_hit < MAX_GREP_FILES
    })?;

    if hits.is_empty() {
        return Ok(format!("No matches for `{query}`."));
    }
    Ok(format!("{} matches:\n{}", hits.len(), hits.join("\n")))
}

pub fn write_file(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .write()
        .map_err(|_| "Workspace access lock poisoned")?;
    let path = arg_path(args, "path")?;
    let content = args
        .get("content")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "write_file requires a \"content\" string.".to_string())?;
    if content.len() > MAX_WRITE_BYTES {
        return Err(format!(
            "Content is {} bytes; max write size is {MAX_WRITE_BYTES} bytes.",
            content.len()
        ));
    }
    let abs = ws.resolve(&path)?;
    let overwrite = args
        .get("overwrite")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if let Ok(meta) = fs::symlink_metadata(&abs) {
        if meta.file_type().is_symlink() {
            return Err("Refusing to overwrite a symlink.".into());
        }
        if meta.is_dir() {
            return Err(format!("{path} is a directory."));
        }
        if !overwrite {
            return Err(format!(
                "{path} already exists; no changes were made. Use str_replace for edits or additions. Only use overwrite=true with the complete file content after reading the entire file."
            ));
        }
    }
    if let Some(parent) = abs.parent() {
        ensure_parent_in_workspace(ws, parent)?;
        fs::create_dir_all(parent).map_err(|err| format!("Could not create directories: {err}"))?;
    }
    atomic_write(&abs, content.as_bytes(), overwrite)?;
    Ok(format!(
        "Wrote {} bytes to {}.",
        content.len(),
        ws.relative_display(&abs)
    ))
}

pub fn str_replace(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .write()
        .map_err(|_| "Workspace access lock poisoned")?;
    let path = arg_path(args, "path")?;
    let old = args
        .get("old_string")
        .or_else(|| args.get("old"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "str_replace requires a non-empty \"old_string\".".to_string())?;
    let new = args
        .get("new_string")
        .or_else(|| args.get("new"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "str_replace requires \"new_string\".".to_string())?;
    let replace_all = args
        .get("replace_all")
        .or_else(|| args.get("allow_multiple"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let abs = ws.resolve(&path)?;
    let meta = fs::symlink_metadata(&abs).map_err(|_| format!("File not found: {path}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to edit a symlink.".into());
    }
    if !meta.is_file() {
        return Err(format!("{path} is not a file."));
    }
    if meta.len() > MAX_EDIT_BYTES as u64 {
        return Err(format!(
            "{path} is too large to edit safely (max {MAX_EDIT_BYTES} bytes)."
        ));
    }
    let text = fs::read_to_string(&abs).map_err(|err| format!("Could not read {path}: {err}"))?;
    // read_file presents LF line breaks. Match that representation without
    // rewriting the line endings (or any other bytes) outside the edited span.
    let normalized = text.replace("\r\n", "\n");
    let old = old.replace("\r\n", "\n");
    let count = normalized.match_indices(&old).count();
    if count == 0 {
        return Err(
            "old_string was not found; no changes were made. LF and CRLF line endings are already treated as equivalent. Re-read the relevant lines and copy a unique snippet exactly, including indentation; do not include the read_file header or invent a trailing newline.".into(),
        );
    }
    if count > 1 && !replace_all {
        return Err(format!(
            "old_string matched {count} times. Pass a unique snippet, or set replace_all=true."
        ));
    }
    let removed_cr: Vec<usize> = text
        .match_indices("\r\n")
        .enumerate()
        .map(|(removed, (at, _))| at - removed)
        .collect();
    let original_offset = |at| at + removed_cr.partition_point(|&cr| cr < at);
    let new = new.replace("\r\n", "\n");
    if old.len() > MAX_EDIT_BYTES || new.len() > MAX_EDIT_BYTES {
        return Err(format!(
            "Replacement text is too large (max {MAX_EDIT_BYTES} bytes)."
        ));
    }
    let default_crlf = text
        .find('\n')
        .is_some_and(|i| i > 0 && text.as_bytes()[i - 1] == b'\r');
    let new_crlf_extra = new.bytes().filter(|byte| *byte == b'\n').count();
    let mut output_len = text.len();
    for (at, _) in normalized.match_indices(&old) {
        let start = original_offset(at);
        let end = original_offset(at + old.len());
        let matched = &text[start..end];
        let crlf = matched
            .find('\n')
            .map(|i| i > 0 && matched.as_bytes()[i - 1] == b'\r')
            .unwrap_or(default_crlf);
        let inserted = new
            .len()
            .checked_add(if crlf { new_crlf_extra } else { 0 })
            .ok_or_else(|| "Replacement would be too large.".to_string())?;
        output_len = output_len
            .checked_sub(end - start)
            .and_then(|len| len.checked_add(inserted))
            .ok_or_else(|| "Replacement would be too large.".to_string())?;
        if output_len > MAX_EDIT_BYTES {
            return Err(format!(
                "Replacement would produce {output_len} bytes; max edit size is {MAX_EDIT_BYTES} bytes."
            ));
        }
    }
    let mut next = String::with_capacity(output_len);
    let mut cursor = 0;
    for (at, _) in normalized.match_indices(&old) {
        let start = original_offset(at);
        let end = original_offset(at + old.len());
        let matched = &text[start..end];
        let crlf = matched
            .find('\n')
            .map(|i| i > 0 && matched.as_bytes()[i - 1] == b'\r')
            .unwrap_or(default_crlf);
        next.push_str(&text[cursor..start]);
        if crlf {
            next.push_str(&new.replace('\n', "\r\n"));
        } else {
            next.push_str(&new);
        }
        cursor = end;
    }
    next.push_str(&text[cursor..]);
    atomic_write(&abs, next.as_bytes(), true)?;
    let n = if replace_all { count } else { 1 };
    Ok(format!(
        "Updated {} ({} replacement{}).",
        ws.relative_display(&abs),
        n,
        if n == 1 { "" } else { "s" }
    ))
}

pub fn delete_file(ws: &Workspace, args: &Value) -> Result<String, String> {
    let _access = ws
        .access
        .write()
        .map_err(|_| "Workspace access lock poisoned")?;
    let path = arg_path(args, "path")?;
    let abs = ws.resolve(&path)?;
    if abs == ws.root {
        return Err("Refusing to delete the workspace root.".into());
    }
    let meta = fs::symlink_metadata(&abs).map_err(|_| format!("Path not found: {path}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to delete a symlink.".into());
    }
    if meta.is_dir() {
        return Err("delete_file only removes files, not directories.".into());
    }
    fs::remove_file(&abs).map_err(|err| format!("Could not delete {path}: {err}"))?;
    Ok(format!("Deleted {}.", ws.relative_display(&abs)))
}

pub fn tool_summary(name: &str, args: &Value) -> String {
    match name {
        "read_file" | "write_file" | "str_replace" | "delete_file" | "list_dir" => args
            .get("path")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string(),
        "glob" => args
            .get("pattern")
            .or_else(|| args.get("glob"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        "grep" => args
            .get("query")
            .or_else(|| args.get("pattern"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string(),
        _ => String::new(),
    }
}

fn arg_path(args: &Value, key: &str) -> Result<String, String> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| format!("{key} is required."))
}

fn arg_usize(args: &Value, key: &str) -> Option<usize> {
    args.get(key).and_then(|v| {
        v.as_u64()
            .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            .and_then(|n| usize::try_from(n).ok())
    })
}

#[cfg(test)]
fn slice_lines(text: &str, offset: usize, limit: Option<usize>, label: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if offset >= total {
        return format!("{label} has {total} lines; offset {offset} is past the end.");
    }
    let end = limit
        .map(|n| offset.saturating_add(n).min(total))
        .unwrap_or(total);
    let mut slice = lines[offset..end].join("\n");
    if end > offset && (end < total || text.ends_with('\n')) {
        slice.push('\n');
    }
    if offset == 0 && end == total {
        format!("{label} ({total} lines)\n{slice}")
    } else {
        format!("{label} lines {}–{} of {total}\n{slice}", offset + 1, end)
    }
}

fn walk_files(
    root: &Path,
    dir: &Path,
    visit: &mut dyn FnMut(String, &Path, bool) -> bool,
) -> Result<(), String> {
    let entries = fs::read_dir(dir).map_err(|err| format!("Could not walk workspace: {err}"))?;
    for entry in entries {
        let entry = entry.map_err(|err| format!("Could not walk workspace: {err}"))?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') && name_str != ".gitignore" && name_str != ".env.example" {
            // Still skip .git and other dotted dirs; allow listing files that aren't hidden dirs.
            if SKIP_DIR_NAMES.contains(&name_str.as_ref()) {
                continue;
            }
        }
        if SKIP_DIR_NAMES.contains(&name_str.as_ref()) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.file_type().is_symlink() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        if meta.is_dir() {
            if !visit(rel, &path, true) {
                return Ok(());
            }
            walk_files(root, &path, visit)?;
        } else if meta.is_file() && !visit(rel, &path, false) {
            return Ok(());
        }
    }
    Ok(())
}

fn ensure_parent_in_workspace(ws: &Workspace, parent: &Path) -> Result<(), String> {
    if parent == ws.root {
        return Ok(());
    }
    if !parent.starts_with(&ws.root) {
        return Err("Cannot create directories outside the workspace.".into());
    }
    Ok(())
}

pub(super) fn atomic_write(path: &Path, bytes: &[u8], overwrite: bool) -> Result<(), String> {
    atomic_write_with_permissions(path, bytes, overwrite, None)
}

pub(super) fn atomic_write_with_permissions(
    path: &Path,
    bytes: &[u8],
    overwrite: bool,
    permissions: Option<&fs::Permissions>,
) -> Result<(), String> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| format!("Could not write {}: path has no file name", path.display()))?
        .to_string_lossy();
    let mut random = [0u8; 8];
    getrandom::fill(&mut random)
        .map_err(|err| format!("Could not prepare write for {}: {err}", path.display()))?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let tmp = parent.join(format!(
        ".{file_name}.{}.{}.tensorui-tmp",
        std::process::id(),
        suffix
    ));
    let result = (|| -> Result<(), String> {
        let mut file = File::options()
            .create_new(true)
            .write(true)
            .open(&tmp)
            .map_err(|err| format!("Could not write {}: {err}", path.display()))?;
        file.write_all(bytes)
            .map_err(|err| format!("Could not write {}: {err}", path.display()))?;
        let permissions = permissions.cloned().or_else(|| {
            overwrite
                .then(|| fs::metadata(path).ok().map(|meta| meta.permissions()))
                .flatten()
        });
        if let Some(permissions) = permissions {
            file.set_permissions(permissions)
                .map_err(|err| format!("Could not preserve permissions: {err}"))?;
        }
        file.sync_all()
            .map_err(|err| format!("Could not write {}: {err}", path.display()))?;
        drop(file);
        if overwrite {
            replace_file(&tmp, path)
        } else {
            // Publishing by hard link is atomic and cannot clobber a file that
            // appeared after the existence check above. Fail closed if unsupported.
            fs::hard_link(&tmp, path).map_err(|err| {
                format!(
                    "Could not create {} without overwriting: {err}",
                    path.display()
                )
            })?;
            let _ = fs::remove_file(&tmp);
            Ok(())
        }
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    fs::rename(temporary, destination)
        .map_err(|err| format!("Could not replace {}: {err}", destination.display()))
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> Result<(), String> {
    use std::{iter, os::windows::ffi::OsStrExt};

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, replacement: *const u16, flags: u32) -> i32;
    }

    let existing: Vec<u16> = temporary
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let replacement: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(iter::once(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            existing.as_ptr(),
            replacement.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        return Err(format!(
            "Could not replace {}: {}",
            destination.display(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

fn resolve_under(root: &Path, raw: &str) -> Result<PathBuf, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(root.to_path_buf());
    }
    if trimmed.contains('\0') {
        return Err("Invalid path.".into());
    }
    let candidate = PathBuf::from(trimmed);
    let joined = if candidate.is_absolute() {
        candidate
    } else {
        root.join(&candidate)
    };
    let normalized = normalize_components(&joined)?;
    let prefix = if normalized.is_absolute() {
        normalized
    } else {
        root.join(normalized)
    };
    if let Ok(canon) = canonicalize_existing_prefix(&prefix) {
        if !is_within(root, &canon) {
            return Err("Path is outside the workspace.".into());
        }
        return Ok(canon);
    }
    if !is_within(root, &prefix) {
        return Err("Path is outside the workspace.".into());
    }
    Ok(prefix)
}

fn normalize_components(path: &Path) -> Result<PathBuf, String> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return Err("Path is outside the workspace.".into());
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Ok(out)
}

fn canonicalize_dir(path: &Path) -> Result<PathBuf, String> {
    fs::canonicalize(path).map_err(|err| format!("Could not resolve workspace: {err}"))
}

fn canonicalize_existing_prefix(path: &Path) -> Result<PathBuf, String> {
    if path.exists() {
        return fs::canonicalize(path).map_err(|err| format!("Could not resolve path: {err}"));
    }
    let mut current = path.to_path_buf();
    let mut missing = Vec::new();
    while !current.exists() {
        if let Some(name) = current.file_name() {
            missing.push(name.to_os_string());
        }
        if !current.pop() {
            break;
        }
    }
    if !current.exists() {
        return Err("Path is outside the workspace.".into());
    }
    let mut canon =
        fs::canonicalize(&current).map_err(|err| format!("Could not resolve path: {err}"))?;
    for part in missing.into_iter().rev() {
        canon.push(part);
    }
    Ok(canon)
}

fn is_within(root: &Path, candidate: &Path) -> bool {
    #[cfg(not(windows))]
    {
        candidate == root || candidate.starts_with(root)
    }

    #[cfg(windows)]
    {
        let root_s = display_path(root).to_ascii_lowercase();
        let cand_s = display_path(candidate).to_ascii_lowercase();
        cand_s == root_s
            || cand_s.starts_with(&(root_s.clone() + std::path::MAIN_SEPARATOR_STR))
            || cand_s.starts_with(&(root_s + "/"))
    }
}

fn display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_string()
}

pub fn glob_match(pattern: &str, path: &str) -> bool {
    if pattern.chars().count() > MAX_GLOB_PATTERN_CHARS {
        return false;
    }
    let pattern = pattern.replace('\\', "/");
    let path = path.replace('\\', "/");
    glob_rec(pattern.as_bytes(), path.as_bytes())
}

fn glob_rec(pat: &[u8], text: &[u8]) -> bool {
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat.starts_with(b"**/") {
        return glob_rec(&pat[3..], text)
            || (!text.is_empty() && {
                if let Some(slash) = text.iter().position(|&c| c == b'/') {
                    glob_rec(pat, &text[slash + 1..])
                } else {
                    glob_rec(&pat[3..], text) || glob_rec(&pat[3..], b"")
                }
            });
    }
    if pat == b"**" {
        return true;
    }
    if pat[0] == b'*' {
        let rest = &pat[1..];
        if rest.starts_with(b"*") {
            return glob_rec(&pat[1..], text);
        }
        if text.is_empty() {
            return glob_rec(rest, text);
        }
        let mut i = 0;
        while i <= text.len() {
            if text.get(i) == Some(&b'/') {
                return glob_rec(rest, &text[i..]);
            }
            if glob_rec(rest, &text[i..]) {
                return true;
            }
            i += 1;
        }
        return false;
    }
    if pat[0] == b'?' {
        return !text.is_empty() && text[0] != b'/' && glob_rec(&pat[1..], &text[1..]);
    }
    !text.is_empty() && pat[0] == text[0] && glob_rec(&pat[1..], &text[1..])
}

pub fn workspace_prompt_line(root: &str) -> String {
    let trimmed = root.trim();
    if trimmed.is_empty() {
        "No workspace folder is set for this chat; file tools will fail until the user chooses one."
            .into()
    } else {
        format!(
            "Workspace root: {trimmed}. Stay inside it. Prefer these tools over shell for files."
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn temp_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        (dir, ws)
    }

    #[test]
    fn large_file_pages_resume_without_losing_unicode_or_crlf_boundaries() {
        let (dir, ws) = temp_ws();
        let original = format!("{}\r\n{}\r\nend", "x".repeat(254), "é🙂".repeat(100_000));
        fs::write(dir.path().join("large.txt"), &original).unwrap();
        let mut offset = 0;
        let mut restored = String::new();
        loop {
            let page = read_file(
                &ws,
                &json!({"path":"large.txt", "byte_offset":offset, "max_bytes":256}),
            )
            .unwrap();
            let (header, body) = page.split_once("--- file content ---\n").unwrap();
            assert!(body.len() <= 256);
            restored.push_str(body);
            if header.contains("End of file.") {
                break;
            }
            let next: u64 = header
                .split("byte_offset=")
                .nth(1)
                .unwrap()
                .split('.')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            assert!(next > offset);
            offset = next;
        }
        assert_eq!(restored, original.replace("\r\n", "\n"));
    }

    #[test]
    fn defaults_to_200_lines_and_rejects_invalid_byte_cursors() {
        let (dir, ws) = temp_ws();
        fs::write(dir.path().join("lines.txt"), "é\r\n".repeat(1000)).unwrap();
        let page = read_file(&ws, &json!({"path":"lines.txt"})).unwrap();
        assert_eq!(
            page.split_once("--- file content ---\n")
                .unwrap()
                .1
                .lines()
                .count(),
            200
        );
        assert!(page.contains("byte_offset=800"));
        let last = read_file(&ws, &json!({"path":"lines.txt", "offset":999})).unwrap();
        assert!(last.ends_with("é\n"));
        assert!(last.contains("End of file."));
        for args in [
            json!({"byte_offset":1}),
            json!({"byte_offset":4001}),
            json!({"byte_offset":4,"offset":1}),
        ] {
            let mut args = args;
            args["path"] = json!("lines.txt");
            assert!(read_file(&ws, &args).is_err());
        }
    }

    #[test]
    fn glob_star_and_double_star() {
        assert!(glob_match("*.rs", "main.rs"));
        assert!(!glob_match("*.rs", "src/main.rs"));
        assert!(glob_match("**/*.rs", "src/main.rs"));
        assert!(glob_match("src/**/*.js", "src/a/b.js"));
        assert!(!glob_match("src/**/*.js", "lib/a.js"));
        assert!(glob_match("src/foo.rs", "src/foo.rs"));
    }

    #[test]
    fn rejects_parent_escape() {
        let (_dir, ws) = temp_ws();
        assert!(ws.resolve("../secret.txt").is_err());
        assert!(ws.resolve("..").is_err());
    }

    #[cfg(not(windows))]
    #[test]
    fn case_distinct_sibling_is_outside_workspace() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Workspace");
        fs::create_dir(&root).unwrap();
        let ws = Workspace::open(root.to_str().unwrap()).unwrap();

        assert!(ws.resolve("../workspace/escape.txt").is_err());
    }

    #[test]
    fn write_read_replace_roundtrip() {
        let (_dir, ws) = temp_ws();
        write_file(
            &ws,
            &json!({ "path": "hello.txt", "content": "hello world" }),
        )
        .unwrap();
        let read = read_file(&ws, &json!({ "path": "hello.txt" })).unwrap();
        assert!(read.contains("hello world"));
        str_replace(
            &ws,
            &json!({
                "path": "hello.txt",
                "old_string": "world",
                "new_string": "tensor"
            }),
        )
        .unwrap();
        let read = read_file(&ws, &json!({ "path": "hello.txt" })).unwrap();
        assert!(read.contains("hello tensor"));
        delete_file(&ws, &json!({ "path": "hello.txt" })).unwrap();
        assert!(read_file(&ws, &json!({ "path": "hello.txt" })).is_err());
    }

    #[test]
    fn str_replace_requires_unique_match() {
        let (_dir, ws) = temp_ws();
        write_file(&ws, &json!({ "path": "a.txt", "content": "aa aa" })).unwrap();
        let err = str_replace(
            &ws,
            &json!({ "path": "a.txt", "old_string": "aa", "new_string": "b" }),
        )
        .unwrap_err();
        assert!(err.contains("2 times"));
    }

    #[test]
    fn str_replace_rejects_oversized_inputs_and_expansion() {
        let (_dir, ws) = temp_ws();
        let oversized = ws.root.join("oversized.txt");
        File::create(&oversized)
            .unwrap()
            .set_len(MAX_EDIT_BYTES as u64 + 1)
            .unwrap();
        let err = str_replace(
            &ws,
            &json!({ "path": "oversized.txt", "old_string": "a", "new_string": "b" }),
        )
        .unwrap_err();
        assert!(err.contains("too large to edit safely"));

        let expanding = ws.root.join("expanding.txt");
        let original = "a".repeat(MAX_EDIT_BYTES / 2 + 1);
        fs::write(&expanding, &original).unwrap();
        let err = str_replace(
            &ws,
            &json!({
                "path": "expanding.txt",
                "old_string": "a",
                "new_string": "aa",
                "replace_all": true
            }),
        )
        .unwrap_err();
        assert!(err.contains("max edit size"));
        assert_eq!(
            fs::metadata(expanding).unwrap().len(),
            original.len() as u64
        );
    }

    #[test]
    fn write_requires_explicit_overwrite_and_preserves_existing_content() {
        let (_dir, ws) = temp_ws();
        let path = ws.root.join("styles.css");
        let original = "/* uncommitted styles */\r\nbody { color: white; }\r\n";
        fs::write(&path, original).unwrap();
        let err = write_file(
            &ws,
            &json!({
                "path": "styles.css", "content": "select option { color: white; }"
            }),
        )
        .unwrap_err();
        assert!(err.contains("already exists"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        write_file(
            &ws,
            &json!({
                "path": "styles.css", "content": "intentional full rewrite", "overwrite": true
            }),
        )
        .unwrap();
        assert_eq!(
            fs::read_to_string(path).unwrap(),
            "intentional full rewrite"
        );
    }

    #[test]
    fn atomic_creation_cannot_clobber_a_file_created_after_preflight() {
        let (_dir, ws) = temp_ws();
        let path = ws.root.join("raced.txt");
        fs::write(&path, "another writer").unwrap();
        assert!(atomic_write(&path, b"replacement", false).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "another writer");
        assert_eq!(fs::read_dir(&ws.root).unwrap().count(), 1);
    }

    #[test]
    fn replacements_handle_line_endings_and_preserve_unrelated_bytes() {
        let (_dir, ws) = temp_ws();
        let path = ws.root.join("a.txt");
        for (original, old, new, expected) in [
            (
                "head\r\na\r\nb\r\ntail\r\n",
                "a\nb",
                "x\ny",
                "head\r\nx\r\ny\r\ntail\r\n",
            ),
            (
                "head\na\nb\ntail\n",
                "a\r\nb",
                "x\r\ny",
                "head\nx\ny\ntail\n",
            ),
            (
                "head\r\n100% {\r\n  x\r\n}\r\n",
                "100% {\n  x\n}",
                "100% {\n  x\n}\n\nselect option {}",
                "head\r\n100% {\r\n  x\r\n}\r\n\r\nselect option {}\r\n",
            ),
            ("é\r\na\r\nb\n末尾", "a\nb", "α\nβ", "é\r\nα\r\nβ\n末尾"),
            (
                "head\r\nend",
                "end",
                "end\naddition",
                "head\r\nend\r\naddition",
            ),
            ("a\r\nb", "\nb", "\nc", "a\r\nc"),
            ("a\r\nb", "a\n", "", "b"),
        ] {
            fs::write(&path, original).unwrap();
            str_replace(
                &ws,
                &json!({"path": "a.txt", "old_string": old, "new_string": new}),
            )
            .unwrap();
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                expected,
                "original: {original:?}"
            );
        }
    }

    #[test]
    fn normalized_matches_still_require_uniqueness_and_exact_indentation() {
        let (_dir, ws) = temp_ws();
        let path = ws.root.join("a.txt");
        let original = "a\r\nb\r\na\nb\n";
        fs::write(&path, original).unwrap();
        let err = str_replace(
            &ws,
            &json!({
                "path": "a.txt", "old_string": "a\nb", "new_string": "x\ny"
            }),
        )
        .unwrap_err();
        assert!(err.contains("2 times"));
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        assert!(
            str_replace(
                &ws,
                &json!({
                    "path": "a.txt", "old_string": "  a\nb", "new_string": "x"
                })
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
        str_replace(
            &ws,
            &json!({
                "path": "a.txt", "old_string": "a\nb", "new_string": "x\ny", "replace_all": true
            }),
        )
        .unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "x\r\ny\r\nx\ny\n");
    }

    #[test]
    fn read_excerpts_preserve_whether_the_final_line_has_a_newline() {
        assert_eq!(
            slice_lines("a\r\nb\r\n", 1, None, "f"),
            "f lines 2–2 of 2\nb\n"
        );
        assert_eq!(slice_lines("a\r\nb", 1, None, "f"), "f lines 2–2 of 2\nb");
        assert_eq!(
            slice_lines("a\r\nb", 0, Some(1), "f"),
            "f lines 1–1 of 2\na\n"
        );
    }

    #[test]
    fn list_and_glob_see_created_files() {
        let (_dir, ws) = temp_ws();
        fs::create_dir_all(ws.root.join("src")).unwrap();
        write_file(
            &ws,
            &json!({ "path": "src/lib.rs", "content": "fn x() {}" }),
        )
        .unwrap();
        let listing = list_dir(&ws, &json!({ "path": "src" })).unwrap();
        assert!(listing.contains("lib.rs"));
        let globbed = glob_files(&ws, &json!({ "pattern": "**/*.rs" })).unwrap();
        assert!(globbed.contains("src/lib.rs"));
        let grepped = grep_files(&ws, &json!({ "query": "fn x" })).unwrap();
        assert!(grepped.contains("src/lib.rs"));
    }

    #[test]
    fn search_patterns_have_bounded_complexity() {
        let (_dir, ws) = temp_ws();
        let glob = "*".repeat(MAX_GLOB_PATTERN_CHARS + 1);
        assert!(glob_files(&ws, &json!({ "pattern": glob })).is_err());
        let query = "x".repeat(MAX_GREP_QUERY_CHARS + 1);
        assert!(grep_files(&ws, &json!({ "query": query })).is_err());
    }
}
