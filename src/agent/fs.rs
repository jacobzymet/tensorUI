//! Workspace-scoped file tools. Paths are confined to `workspace_root`.

use std::{
    fs::{self, File},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

use serde_json::Value;

const MAX_READ_BYTES: u64 = 512_000;
const MAX_WRITE_BYTES: usize = 1_000_000;
const MAX_LIST_ENTRIES: usize = 500;
const MAX_GLOB_MATCHES: usize = 200;
const MAX_GREP_MATCHES: usize = 80;
const MAX_GREP_FILES: usize = 80;
const MAX_GREP_FILE_BYTES: u64 = 1_000_000;
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
        Ok(Self { root })
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
    let path = arg_path(args, "path")?;
    let abs = ws.resolve(&path)?;
    let meta = fs::symlink_metadata(&abs).map_err(|_| format!("File not found: {path}"))?;
    if meta.file_type().is_symlink() {
        return Err("Refusing to read a symlink.".into());
    }
    if !meta.is_file() {
        return Err(format!("{path} is not a file."));
    }
    if meta.len() > MAX_READ_BYTES {
        return Err(format!(
            "File is {} bytes; max read size is {MAX_READ_BYTES} bytes. Narrow with offset/limit or a smaller file.",
            meta.len()
        ));
    }
    let mut file = File::open(&abs).map_err(|err| format!("Could not read {path}: {err}"))?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)
        .map_err(|err| format!("Could not read {path}: {err}"))?;
    if buf.contains(&0) {
        return Err(
            "Refusing to read a binary file. Use the terminal for binary inspection.".into(),
        );
    }
    let text = String::from_utf8(buf).map_err(|_| "File is not valid UTF-8.".to_string())?;
    let offset = arg_usize(args, "offset").unwrap_or(0);
    let limit = arg_usize(args, "limit");
    Ok(slice_lines(
        &text,
        offset,
        limit,
        &ws.relative_display(&abs),
    ))
}

pub fn list_dir(ws: &Workspace, args: &Value) -> Result<String, String> {
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
    let pattern = args
        .get("pattern")
        .or_else(|| args.get("glob"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "glob requires a non-empty \"pattern\" string.".to_string())?;
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
    let query = args
        .get("query")
        .or_else(|| args.get("pattern"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| "grep requires a non-empty \"query\" string.".to_string())?;
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
    if let Ok(meta) = fs::symlink_metadata(&abs) {
        if meta.file_type().is_symlink() {
            return Err("Refusing to overwrite a symlink.".into());
        }
        if meta.is_dir() {
            return Err(format!("{path} is a directory."));
        }
    }
    if let Some(parent) = abs.parent() {
        ensure_parent_in_workspace(ws, parent)?;
        fs::create_dir_all(parent).map_err(|err| format!("Could not create directories: {err}"))?;
    }
    atomic_write(&abs, content.as_bytes())?;
    Ok(format!(
        "Wrote {} bytes to {}.",
        content.len(),
        ws.relative_display(&abs)
    ))
}

pub fn str_replace(ws: &Workspace, args: &Value) -> Result<String, String> {
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
    let text = fs::read_to_string(&abs).map_err(|err| format!("Could not read {path}: {err}"))?;
    let count = text.matches(old).count();
    if count == 0 {
        return Err(
            "old_string was not found. Re-read the file and copy the exact text to replace.".into(),
        );
    }
    if count > 1 && !replace_all {
        return Err(format!(
            "old_string matched {count} times. Pass a unique snippet, or set replace_all=true."
        ));
    }
    let next = if replace_all {
        text.replace(old, new)
    } else {
        text.replacen(old, new, 1)
    };
    atomic_write(&abs, next.as_bytes())?;
    let n = if replace_all { count } else { 1 };
    Ok(format!(
        "Updated {} ({} replacement{}).",
        ws.relative_display(&abs),
        n,
        if n == 1 { "" } else { "s" }
    ))
}

pub fn delete_file(ws: &Workspace, args: &Value) -> Result<String, String> {
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

fn slice_lines(text: &str, offset: usize, limit: Option<usize>, label: &str) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    if offset >= total {
        return format!("{label} has {total} lines; offset {offset} is past the end.");
    }
    let end = limit
        .map(|n| offset.saturating_add(n).min(total))
        .unwrap_or(total);
    let slice = lines[offset..end].join("\n");
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
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
        file.sync_all()
            .map_err(|err| format!("Could not write {}: {err}", path.display()))?;
        drop(file);
        replace_file(&tmp, path)
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
}
