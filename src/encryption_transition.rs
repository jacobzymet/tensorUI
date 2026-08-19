//! Authenticated, resumable encryption-mode transitions.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    agent::skills::UserSkill,
    crypto::{self, DiskKey, EncryptionMeta},
    secure_fs,
};

const TRANSITION_FILE: &str = "encryption-transition.json";
const TRANSITION_VERSION: u32 = 1;
const MAX_TRANSITION_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Enable,
    Disable,
}

impl Operation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Disable => "disable",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub chats: Value,
    pub preferences: Value,
    pub provider_tokens: Value,
    pub skills: Vec<UserSkill>,
}

#[derive(Debug, Deserialize)]
struct ProtectedSnapshot {
    transaction_id: String,
    operation: Operation,
    snapshot: Snapshot,
}

#[derive(Serialize)]
struct ProtectedSnapshotRef<'a> {
    transaction_id: &'a str,
    operation: Operation,
    snapshot: &'a Snapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Record {
    version: u32,
    pub transaction_id: String,
    pub operation: Operation,
    pub meta: EncryptionMeta,
    payload: Value,
}

pub fn path(root: &Path) -> PathBuf {
    root.join(TRANSITION_FILE)
}

pub fn exists(root: &Path) -> bool {
    std::fs::symlink_metadata(path(root)).is_ok()
}

pub fn load(root: &Path) -> Result<Option<Record>> {
    let path = path(root);
    if !exists(root) {
        return Ok(None);
    }
    let raw = secure_fs::read_limited_to_string(&path, MAX_TRANSITION_BYTES)?;
    let record: Record = serde_json::from_str(&raw)
        .with_context(|| format!("invalid encryption transition at {}", path.display()))?;
    if record.version != TRANSITION_VERSION || record.transaction_id.is_empty() {
        bail!("unsupported or invalid encryption transition");
    }
    Ok(Some(record))
}

pub fn begin(
    root: &Path,
    operation: Operation,
    meta: EncryptionMeta,
    key: &DiskKey,
    snapshot: &Snapshot,
) -> Result<Record> {
    if exists(root) {
        bail!("An encryption transition is already pending.");
    }
    crypto::verify_key(key, &meta)?;
    let mut id_bytes = [0u8; 16];
    getrandom::fill(&mut id_bytes).context("could not generate transaction id")?;
    let transaction_id = URL_SAFE_NO_PAD.encode(id_bytes);
    let protected = ProtectedSnapshotRef {
        transaction_id: &transaction_id,
        operation,
        snapshot,
    };
    let value = serde_json::to_value(&protected).context("could not serialize transition")?;
    let payload = crypto::encrypt_value(key, &value, &aad(&transaction_id, operation))?;
    let record = Record {
        version: TRANSITION_VERSION,
        transaction_id,
        operation,
        meta,
        payload,
    };
    let mut raw = serde_json::to_vec_pretty(&record).context("could not serialize transition")?;
    raw.push(b'\n');
    if raw.len() as u64 > MAX_TRANSITION_BYTES {
        bail!("Encryption transition is too large to stage safely.");
    }
    secure_fs::atomic_write(&path(root), &raw)?;
    Ok(record)
}

pub fn open(record: &Record, key: &DiskKey) -> Result<Snapshot> {
    crypto::verify_key(key, &record.meta)?;
    let value = crypto::decrypt_value(
        key,
        &record.payload,
        &aad(&record.transaction_id, record.operation),
    )?;
    let protected: ProtectedSnapshot =
        serde_json::from_value(value).context("invalid encryption transition payload")?;
    if protected.transaction_id != record.transaction_id || protected.operation != record.operation
    {
        bail!("encryption transition identity mismatch");
    }
    Ok(protected.snapshot)
}

pub fn clear(root: &Path) -> Result<()> {
    secure_fs::remove_file(&path(root))
}

fn aad(transaction_id: &str, operation: Operation) -> Vec<u8> {
    let mut value = crypto::AAD_ENCRYPTION_TRANSITION.to_vec();
    value.push(b':');
    value.extend_from_slice(operation.as_str().as_bytes());
    value.push(b':');
    value.extend_from_slice(transaction_id.as_bytes());
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot() -> Snapshot {
        Snapshot {
            chats: serde_json::json!({"conversations": []}),
            preferences: serde_json::json!({"theme": "dark"}),
            provider_tokens: serde_json::json!({"p1": "secret"}),
            skills: vec![],
        }
    }

    #[test]
    fn transition_roundtrip_is_authenticated() {
        let dir = tempfile::tempdir().unwrap();
        let salt = crypto::random_salt().unwrap();
        let key = crypto::derive_key("passphrase", &salt).unwrap();
        let meta = crypto::meta_for_key(&key, salt).unwrap();
        let snapshot = snapshot();
        let record = begin(dir.path(), Operation::Enable, meta, &key, &snapshot).unwrap();
        let raw = secure_fs::read_to_string(&path(dir.path())).unwrap();
        assert!(!raw.contains("secret"));
        assert!(!raw.contains("theme"));
        let opened = open(&record, &key).unwrap();
        assert_eq!(opened.provider_tokens["p1"], "secret");

        let mut tampered = load(dir.path()).unwrap().unwrap();
        tampered.operation = Operation::Disable;
        assert!(open(&tampered, &key).is_err());
    }
}
