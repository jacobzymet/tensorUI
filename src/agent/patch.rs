//! Context-based, multi-file patches with preflight validation and rollback.

use super::fs::{Workspace, atomic_write_with_permissions};
use serde_json::Value;
use std::{collections::HashSet, fs, io::Read, path::PathBuf};

const MAX_PATCH_BYTES: usize = 1_000_000;
const MAX_FILE_BYTES: u64 = 8_000_000;
const MAX_FILES: usize = 64;

#[derive(Debug)]
enum Operation {
    Add {
        path: String,
        lines: Vec<String>,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        destination: Option<String>,
        hunks: Vec<Hunk>,
    },
}

#[derive(Debug)]
struct Hunk {
    anchor: Option<String>,
    old: Vec<String>,
    new: Vec<String>,
    eof: bool,
}

struct Change {
    path: PathBuf,
    before: Option<Vec<u8>>,
    after: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
}

pub fn apply(ws: &Workspace, args: &Value) -> Result<String, String> {
    let input = args
        .get("patch")
        .and_then(Value::as_str)
        .ok_or("apply_patch requires a patch string")?;
    let operations = parse(input)?;
    let _access = ws
        .access
        .write()
        .map_err(|_| "Workspace access lock poisoned")?;
    let mut changes = Vec::new();
    let mut seen = HashSet::new();
    for operation in operations {
        match operation {
            Operation::Add { path, lines } => {
                let path = checked_path(ws, &path, &mut seen)?;
                if path.exists() {
                    return Err(format!(
                        "{} already exists; use Update File. No changes made.",
                        path.display()
                    ));
                }
                let content = if lines.is_empty() {
                    String::new()
                } else {
                    lines.join("\n") + "\n"
                };
                changes.push(Change {
                    path,
                    before: None,
                    after: Some(content.into_bytes()),
                    permissions: None,
                });
            }
            Operation::Delete { path } => {
                let path = checked_path(ws, &path, &mut seen)?;
                let before = read_existing(&path)?;
                let permissions = Some(
                    fs::metadata(&path)
                        .map_err(|err| err.to_string())?
                        .permissions(),
                );
                changes.push(Change {
                    path,
                    before: Some(before),
                    after: None,
                    permissions,
                });
            }
            Operation::Update {
                path,
                destination,
                hunks,
            } => {
                let path = checked_path(ws, &path, &mut seen)?;
                let before = read_existing(&path)?;
                let permissions = Some(
                    fs::metadata(&path)
                        .map_err(|err| err.to_string())?
                        .permissions(),
                );
                let text = std::str::from_utf8(&before).map_err(|_| "Patch target is not UTF-8")?;
                let after = apply_hunks(text, &hunks)?.into_bytes();
                if after.len() as u64 > MAX_FILE_BYTES {
                    return Err("Patched file exceeds the 8 MB limit; no changes made.".into());
                }
                if let Some(destination) = destination {
                    let destination = checked_path(ws, &destination, &mut seen)?;
                    if destination.exists() {
                        return Err("Move destination exists; no changes made.".into());
                    }
                    changes.push(Change {
                        path: destination,
                        before: None,
                        after: Some(after),
                        permissions: permissions.clone(),
                    });
                    changes.push(Change {
                        path,
                        before: Some(before),
                        after: None,
                        permissions,
                    });
                } else {
                    changes.push(Change {
                        path,
                        before: Some(before),
                        after: Some(after),
                        permissions,
                    });
                }
            }
        }
        if changes
            .iter()
            .map(|change| {
                change.before.as_ref().map_or(0, Vec::len)
                    + change.after.as_ref().map_or(0, Vec::len)
            })
            .sum::<usize>()
            > 64 * 1024 * 1024
        {
            return Err("Patch snapshots exceed the 64 MB transaction limit; split the patch. No changes made.".into());
        }
    }
    // Validate every hunk and path before changing any file. Recheck snapshots
    // before publishing to avoid overwriting edits made during preflight.
    for change in &changes {
        if current_bytes(&change.path)? != change.before {
            return Err(format!(
                "{} changed while preparing the patch; re-read it. No changes made.",
                change.path.display()
            ));
        }
    }
    commit_changes(&changes, publish)?;
    Ok(format!(
        "Applied patch:\n{}",
        changes
            .iter()
            .map(|change| {
                let kind = if change.before.is_none() {
                    "A"
                } else if change.after.is_none() {
                    "D"
                } else {
                    "M"
                };
                format!("{kind} {}", ws.relative_display(&change.path))
            })
            .collect::<Vec<_>>()
            .join("\n")
    ))
}

fn commit_changes(
    changes: &[Change],
    mut publish: impl FnMut(&Change, &mut Vec<PathBuf>) -> Result<(), String>,
) -> Result<(), String> {
    let mut created_dirs = Vec::new();
    for (index, change) in changes.iter().enumerate() {
        if let Err(err) = publish(change, &mut created_dirs) {
            let mut rollback_errors = Vec::new();
            for applied in changes[..index].iter().rev() {
                if current_bytes(&applied.path).ok().as_ref() != Some(&applied.after) {
                    rollback_errors.push(format!(
                        "{} changed externally; left untouched",
                        applied.path.display()
                    ));
                    continue;
                }
                let result = match &applied.before {
                    Some(bytes) => atomic_write_with_permissions(
                        &applied.path,
                        bytes,
                        true,
                        applied.permissions.as_ref(),
                    ),
                    None => fs::remove_file(&applied.path).map_err(|err| err.to_string()),
                };
                if let Err(err) = result {
                    rollback_errors.push(err);
                }
            }
            for dir in created_dirs.iter().rev() {
                let _ = fs::remove_dir(dir);
            }
            return Err(if rollback_errors.is_empty() {
                format!(
                    "Patch could not be committed: {err}. Earlier file changes were rolled back."
                )
            } else {
                format!(
                    "Patch failed: {err}. Rollback needs attention: {}",
                    rollback_errors.join("; ")
                )
            });
        }
    }
    Ok(())
}

fn checked_path(ws: &Workspace, raw: &str, seen: &mut HashSet<PathBuf>) -> Result<PathBuf, String> {
    if raw.trim().is_empty() {
        return Err("Patch path is empty".into());
    }
    let path = ws.resolve(raw)?;
    if path == ws.resolve(".")? {
        return Err("Cannot patch the workspace root".into());
    }
    if !seen.insert(path.clone()) {
        return Err(
            "Multiple operations target the same path; combine its hunks in one Update File."
                .into(),
        );
    }
    Ok(path)
}

fn read_existing(path: &PathBuf) -> Result<Vec<u8>, String> {
    let meta = fs::symlink_metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
    if !meta.is_file() || meta.file_type().is_symlink() {
        return Err("Patch target must be a regular file".into());
    }
    if meta.len() > MAX_FILE_BYTES {
        return Err("Patch target exceeds the 8 MB limit".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path)
        .map_err(|err| err.to_string())?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err("Patch target exceeds the 8 MB limit".into());
    }
    if bytes.contains(&0) {
        return Err("Cannot patch binary files".into());
    }
    Ok(bytes)
}

fn current_bytes(path: &PathBuf) -> Result<Option<Vec<u8>>, String> {
    match fs::symlink_metadata(path) {
        Ok(_) => read_existing(path).map(Some),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.to_string()),
    }
}

fn publish(change: &Change, created_dirs: &mut Vec<PathBuf>) -> Result<(), String> {
    if current_bytes(&change.path)? != change.before {
        return Err("Target changed since preflight".into());
    }
    match &change.after {
        Some(bytes) => {
            let mut missing = Vec::new();
            let mut parent = change.path.parent();
            while let Some(dir) = parent {
                if dir.exists() {
                    break;
                }
                missing.push(dir.to_owned());
                parent = dir.parent();
            }
            for dir in missing.into_iter().rev() {
                fs::create_dir(&dir).map_err(|err| err.to_string())?;
                created_dirs.push(dir);
            }
            atomic_write_with_permissions(
                &change.path,
                bytes,
                change.before.is_some(),
                change.permissions.as_ref(),
            )
        }
        None => fs::remove_file(&change.path).map_err(|err| err.to_string()),
    }
}

fn parse(input: &str) -> Result<Vec<Operation>, String> {
    if input.len() > MAX_PATCH_BYTES {
        return Err("Patch exceeds the 1 MB limit".into());
    }
    let normalized = input.replace("\r\n", "\n");
    let lines: Vec<&str> = normalized.trim_end_matches('\n').split('\n').collect();
    if lines.first() != Some(&"*** Begin Patch") || lines.last() != Some(&"*** End Patch") {
        return Err("Patch must start with *** Begin Patch and end with *** End Patch".into());
    }
    let mut operations = Vec::new();
    let mut at = 1;
    while at + 1 < lines.len() {
        let header = lines[at];
        at += 1;
        if let Some(path) = header.strip_prefix("*** Add File: ") {
            let mut added = Vec::new();
            while at + 1 < lines.len() && !lines[at].starts_with("*** ") {
                added.push(
                    lines[at]
                        .strip_prefix('+')
                        .ok_or("Added file lines must start with +")?
                        .to_owned(),
                );
                at += 1;
            }
            operations.push(Operation::Add {
                path: path.into(),
                lines: added,
            });
        } else if let Some(path) = header.strip_prefix("*** Delete File: ") {
            operations.push(Operation::Delete { path: path.into() });
        } else if let Some(path) = header.strip_prefix("*** Update File: ") {
            let destination = lines
                .get(at)
                .and_then(|line| line.strip_prefix("*** Move to: "))
                .map(str::to_owned);
            if destination.is_some() {
                at += 1;
            }
            let mut hunks = Vec::new();
            while at + 1 < lines.len() && lines[at].starts_with("@@") {
                let anchor = if lines[at] == "@@" {
                    None
                } else {
                    Some(
                        lines[at]
                            .strip_prefix("@@ ")
                            .ok_or("Invalid hunk header")?
                            .to_owned(),
                    )
                };
                at += 1;
                let mut hunk = Hunk {
                    anchor,
                    old: Vec::new(),
                    new: Vec::new(),
                    eof: false,
                };
                while at + 1 < lines.len()
                    && !lines[at].starts_with("@@")
                    && !lines[at].starts_with("*** ")
                {
                    let line = lines[at];
                    match line.as_bytes().first() {
                        Some(b' ') => {
                            hunk.old.push(line[1..].into());
                            hunk.new.push(line[1..].into());
                        }
                        Some(b'-') => hunk.old.push(line[1..].into()),
                        Some(b'+') => hunk.new.push(line[1..].into()),
                        _ => return Err(
                            "Hunk lines must start with a space, +, or - (including blank lines)"
                                .into(),
                        ),
                    }
                    at += 1;
                }
                if lines.get(at) == Some(&"*** End of File") {
                    hunk.eof = true;
                    at += 1;
                }
                if hunk.old.is_empty() && hunk.new.is_empty() {
                    return Err("Empty patch hunk".into());
                }
                hunks.push(hunk);
            }
            if hunks.is_empty() && destination.is_none() {
                return Err("Update File needs at least one @@ hunk".into());
            }
            operations.push(Operation::Update {
                path: path.into(),
                destination,
                hunks,
            });
        } else {
            return Err(format!("Invalid patch header: {header}"));
        }
        if operations.len() > MAX_FILES {
            return Err("Patch exceeds the 64-file limit".into());
        }
    }
    if operations.is_empty() {
        return Err("Patch contains no file operations".into());
    }
    Ok(operations)
}

#[derive(Clone)]
struct Line {
    text: String,
    ending: String,
}

fn apply_hunks(text: &str, hunks: &[Hunk]) -> Result<String, String> {
    let mut lines: Vec<Line> = text
        .split_inclusive('\n')
        .map(|line| {
            let ending = if line.ends_with("\r\n") {
                "\r\n"
            } else if line.ends_with('\n') {
                "\n"
            } else {
                ""
            };
            Line {
                text: line[..line.len() - ending.len()].into(),
                ending: ending.into(),
            }
        })
        .collect();
    let default_ending = lines
        .iter()
        .find(|line| !line.ending.is_empty())
        .map(|line| line.ending.clone())
        .unwrap_or_else(|| "\n".into());
    let mut cursor = 0;
    for hunk in hunks {
        if let Some(anchor) = &hunk.anchor {
            let anchors: Vec<_> = (cursor..lines.len())
                .filter(|&i| lines[i].text == *anchor)
                .collect();
            if anchors.len() != 1 {
                return Err("Hunk anchor must match one exact line; re-read the file".into());
            }
            cursor = anchors[0] + 1;
        }
        let start = if hunk.old.is_empty() {
            if !hunk.eof && !lines.is_empty() {
                return Err("Insertion needs context or *** End of File".into());
            }
            lines.len()
        } else {
            let matches: Vec<_> = (cursor..=lines.len())
                .filter(|&i| {
                    i + hunk.old.len() <= lines.len()
                        && (!hunk.eof || i + hunk.old.len() == lines.len())
                        && lines[i..i + hunk.old.len()]
                            .iter()
                            .zip(&hunk.old)
                            .all(|(actual, old)| actual.text == *old)
                })
                .collect();
            if matches.len() != 1 {
                return Err(format!(
                    "Hunk matched {} places; provide unique exact context. No changes made.",
                    matches.len()
                ));
            }
            matches[0]
        };
        let end = start + hunk.old.len();
        let at_end = end == lines.len();
        let final_ending = lines
            .last()
            .map(|line| line.ending.clone())
            .unwrap_or_else(|| default_ending.clone());
        let ending = lines[start..end]
            .iter()
            .find(|line| !line.ending.is_empty())
            .map(|line| line.ending.clone())
            .unwrap_or_else(|| default_ending.clone());
        let mut new: Vec<Line> = hunk
            .new
            .iter()
            .map(|text| Line {
                text: text.clone(),
                ending: ending.clone(),
            })
            .collect();
        // Preserve unchanged context lines, including mixed newline styles.
        let mut old_index = 0;
        for line in &mut new {
            if let Some(found) = lines[start + old_index..end]
                .iter()
                .position(|old| old.text == line.text)
            {
                old_index += found;
                line.ending = lines[start + old_index].ending.clone();
                old_index += 1;
            }
        }
        let new_len = new.len();
        for (i, line) in new.iter_mut().enumerate() {
            if (!at_end || i + 1 < new_len) && line.ending.is_empty() {
                line.ending = ending.clone();
            }
        }
        if at_end && let Some(last) = new.last_mut() {
            last.ending = final_ending;
        }
        if !new.is_empty() && start > 0 && lines[start - 1].ending.is_empty() {
            lines[start - 1].ending = ending;
        }
        cursor = start + new.len();
        lines.splice(start..end, new);
    }
    Ok(lines
        .into_iter()
        .map(|line| line.text + &line.ending)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn multi_file_multi_hunk_patch_preserves_crlf_and_unrelated_edits() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        fs::write(dir.path().join("a.txt"), "user edit\r\none\r\nkeep\r\ntwo").unwrap();
        apply(&ws, &json!({"patch": "*** Begin Patch\n*** Update File: a.txt\n@@\n-one\n+ONE\n@@\n-two\n+TWO\n*** Add File: nested/b.txt\n+new\n*** End Patch"})).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "user edit\r\nONE\r\nkeep\r\nTWO"
        );
        assert_eq!(
            fs::read_to_string(dir.path().join("nested/b.txt")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn invalid_later_hunk_leaves_every_file_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        fs::write(dir.path().join("a.txt"), "original\n").unwrap();
        assert!(apply(&ws, &json!({"patch": "*** Begin Patch\n*** Add File: b.txt\n+new\n*** Update File: a.txt\n@@\n-missing\n+bad\n*** End Patch"})).is_err());
        assert!(!dir.path().join("b.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "original\n"
        );
    }

    #[test]
    fn rejects_ambiguous_context_and_workspace_escape() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        fs::write(dir.path().join("a.txt"), "same\nsame\n").unwrap();
        for patch in [
            "*** Begin Patch\n*** Update File: a.txt\n@@\n-same\n+bad\n*** End Patch",
            "*** Begin Patch\n*** Add File: ../escape.txt\n+bad\n*** End Patch",
        ] {
            assert!(apply(&ws, &json!({"patch": patch})).is_err());
        }
        assert_eq!(
            fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "same\nsame\n"
        );
    }

    #[test]
    fn supports_move_delete_and_explicit_eof_insertion() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        fs::write(dir.path().join("a.txt"), "first").unwrap();
        fs::write(dir.path().join("remove.txt"), "remove").unwrap();
        apply(&ws, &json!({"patch": "*** Begin Patch\n*** Update File: a.txt\n*** Move to: b.txt\n@@\n+second\n*** End of File\n*** Delete File: remove.txt\n*** End Patch"})).unwrap();
        assert!(!dir.path().join("a.txt").exists());
        assert!(!dir.path().join("remove.txt").exists());
        assert_eq!(
            fs::read_to_string(dir.path().join("b.txt")).unwrap(),
            "first\nsecond"
        );
    }

    #[test]
    fn adding_after_unterminated_context_inserts_a_line_break() {
        let hunks = vec![Hunk {
            anchor: None,
            old: vec!["end".into()],
            new: vec!["end".into(), "addition".into()],
            eof: true,
        }];
        assert_eq!(
            apply_hunks("start\r\nend", &hunks).unwrap(),
            "start\r\nend\r\naddition"
        );
    }

    #[test]
    fn commit_failure_rolls_back_prior_writes_without_clobbering_external_edits() {
        for external_edit in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("a.txt");
            fs::write(&path, "original").unwrap();
            let changes = vec![
                Change {
                    path: path.clone(),
                    before: Some(b"original".to_vec()),
                    after: Some(b"edited".to_vec()),
                    permissions: None,
                },
                Change {
                    path: dir.path().join("b.txt"),
                    before: None,
                    after: Some(b"new".to_vec()),
                    permissions: None,
                },
            ];
            let error = commit_changes(&changes, |change, dirs| {
                if change.path == path {
                    publish(change, dirs)
                } else {
                    if external_edit {
                        fs::write(&path, "external edit").unwrap();
                    }
                    Err("simulated disk failure".into())
                }
            })
            .unwrap_err();
            assert_eq!(
                fs::read_to_string(&path).unwrap(),
                if external_edit {
                    "external edit"
                } else {
                    "original"
                }
            );
            assert!(!dir.path().join("b.txt").exists());
            assert!(error.contains(if external_edit {
                "left untouched"
            } else {
                "rolled back"
            }));
        }
    }

    #[cfg(unix)]
    #[test]
    fn moving_an_executable_preserves_its_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::open(dir.path().to_str().unwrap()).unwrap();
        fs::write(dir.path().join("a.sh"), "echo hi\n").unwrap();
        fs::set_permissions(dir.path().join("a.sh"), fs::Permissions::from_mode(0o750)).unwrap();
        apply(&ws, &json!({"patch":"*** Begin Patch\n*** Update File: a.sh\n*** Move to: b.sh\n*** End Patch"})).unwrap();
        assert_eq!(
            fs::metadata(dir.path().join("b.sh"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }
}
