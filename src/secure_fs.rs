//! Durable, private filesystem operations for security-sensitive local state.

use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::Serialize;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

#[derive(Debug)]
pub struct DataLock {
    _file: File,
}

impl DataLock {
    pub fn acquire(root: &Path) -> Result<Self> {
        ensure_private_dir(root)?;
        let path = root.join(".tensorui.lock");
        reject_unsafe_destination(&path)?;
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        options.mode(0o600);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            options.share_mode(0);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("could not open data lock {}", path.display()))?;

        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            const LOCK_EX: i32 = 2;
            const LOCK_NB: i32 = 4;
            unsafe extern "C" {
                fn flock(fd: i32, operation: i32) -> i32;
            }
            let opened = file
                .metadata()
                .context("could not inspect open data lock")?;
            let linked = fs::symlink_metadata(&path)
                .with_context(|| format!("could not inspect data lock {}", path.display()))?;
            if linked.file_type().is_symlink()
                || opened.dev() != linked.dev()
                || opened.ino() != linked.ino()
                || opened.nlink() != 1
            {
                bail!("refusing unsafe data lock {}", path.display());
            }
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .with_context(|| format!("could not secure data lock {}", path.display()))?;
            let result = unsafe { flock(file.as_raw_fd(), LOCK_EX | LOCK_NB) };
            if result != 0 {
                return Err(std::io::Error::last_os_error()).context(
                    "This data folder is already in use by another TensorMI Harness process.",
                );
            }
        }

        Ok(Self { _file: file })
    }
}

pub fn ensure_private_dir(path: &Path) -> Result<()> {
    let existed = path.exists();
    fs::create_dir_all(path).with_context(|| format!("could not create {}", path.display()))?;
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing unsafe data directory {}", path.display());
    }
    #[cfg(unix)]
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not secure {}", path.display()))?;
    sync_directory(path)?;
    if !existed
        && let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        sync_directory(parent)?;
    }
    Ok(())
}

fn ensure_write_dir(path: &Path) -> Result<()> {
    if !path.exists() {
        return ensure_private_dir(path);
    }
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("refusing unsafe destination directory {}", path.display());
    }
    Ok(())
}

fn reject_unsafe_destination(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            bail!("refusing unsafe destination {}", path.display())
        }
        Ok(metadata) => {
            #[cfg(unix)]
            if metadata.nlink() != 1 {
                bail!("refusing hard-linked destination {}", path.display());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

fn temporary_path(path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .context("path has no file name")?
        .to_string_lossy();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    Ok(parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), stamp)))
}

fn open_private_new(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .with_context(|| format!("could not create {}", path.display()))
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_write_dir(parent)?;
    reject_unsafe_destination(path)?;
    let temporary = temporary_path(path)?;

    let result = (|| -> Result<()> {
        let mut file = open_private_new(&temporary)?;
        file.write_all(bytes)
            .with_context(|| format!("could not write {}", temporary.display()))?;
        file.sync_all()
            .with_context(|| format!("could not flush {}", temporary.display()))?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_directory(parent)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn atomic_write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut raw = serde_json::to_vec_pretty(value).context("could not serialize JSON")?;
    raw.push(b'\n');
    atomic_write(path, &raw)
}

pub fn read(path: &Path) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        bail!("refusing unsafe source {}", path.display());
    }
    #[cfg(unix)]
    {
        if metadata.nlink() != 1 {
            bail!("refusing hard-linked source {}", path.display());
        }
        if metadata.permissions().mode() & 0o077 != 0 {
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))
                .with_context(|| format!("could not secure {}", path.display()))?;
        }
    }
    fs::read(path).with_context(|| format!("could not read {}", path.display()))
}

pub fn read_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let metadata = fs::symlink_metadata(path)
        .with_context(|| format!("could not inspect {}", path.display()))?;
    if metadata.len() > max_bytes {
        bail!("refusing oversized security file {}", path.display());
    }
    let bytes = read(path)?;
    if bytes.len() as u64 > max_bytes {
        bail!("refusing oversized security file {}", path.display());
    }
    Ok(bytes)
}

pub fn read_limited_to_string(path: &Path, max_bytes: u64) -> Result<String> {
    String::from_utf8(read_limited(path, max_bytes)?)
        .with_context(|| format!("{} is not UTF-8", path.display()))
}

pub fn read_to_string(path: &Path) -> Result<String> {
    String::from_utf8(read(path)?).with_context(|| format!("{} is not UTF-8", path.display()))
}

pub fn remove_file(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                bail!("refusing unsafe removal target {}", path.display());
            }
            #[cfg(unix)]
            if metadata.nlink() != 1 {
                bail!("refusing hard-linked removal target {}", path.display());
            }
            fs::remove_file(path)
                .with_context(|| format!("could not remove {}", path.display()))?;
            if let Some(parent) = path.parent() {
                sync_directory(parent)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("could not inspect {}", path.display())),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .with_context(|| format!("could not open directory {}", path.display()))?
        .sync_all()
        .with_context(|| format!("could not flush directory {}", path.display()))
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> Result<()> {
    // MOVEFILE_WRITE_THROUGH below flushes the replacement before returning.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(not(windows))]
fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    fs::rename(temporary, destination)
        .with_context(|| format!("could not replace {}", destination.display()))
}

#[cfg(windows)]
fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
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
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("could not replace {}", destination.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_replaces_complete_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        atomic_write(&path, b"old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
    }

    #[cfg(unix)]
    #[test]
    fn private_permissions_are_applied() {
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        ensure_private_dir(&private).unwrap();
        let path = private.join("secret");
        atomic_write(&path, b"secret").unwrap();
        assert_eq!(
            fs::metadata(&private).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlink_and_hardlink_targets() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let original = dir.path().join("original");
        atomic_write(&original, b"secret").unwrap();

        let symlink_path = dir.path().join("symlink");
        symlink(&original, &symlink_path).unwrap();
        assert!(read(&symlink_path).is_err());
        assert!(atomic_write(&symlink_path, b"replacement").is_err());

        let hardlink_path = dir.path().join("hardlink");
        fs::hard_link(&original, &hardlink_path).unwrap();
        assert!(read(&original).is_err());
        assert!(atomic_write(&original, b"replacement").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn data_lock_is_exclusive_and_released_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let first = DataLock::acquire(dir.path()).unwrap();
        assert!(DataLock::acquire(dir.path()).is_err());
        drop(first);
        DataLock::acquire(dir.path()).unwrap();
    }
}
