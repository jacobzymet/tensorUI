use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{crypto, secure_fs};

pub const CHATS_FILE: &str = "chats.json";
pub const PREFERENCES_FILE: &str = "preferences.json";
pub const PROVIDER_TOKENS_FILE: &str = "provider-tokens.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum StorageMode {
    #[default]
    Disk,
    Browser,
}

impl StorageMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disk => "disk",
            Self::Browser => "browser",
        }
    }

    pub fn is_browser(self) -> bool {
        matches!(self, Self::Browser)
    }
}

#[derive(Debug)]
pub enum StoreError {
    Locked,
    Other(anyhow::Error),
}

impl StoreError {
    pub fn message(&self) -> String {
        match self {
            Self::Locked => {
                "Local data is encrypted. Unlock with your passphrase to continue.".into()
            }
            Self::Other(error) => format!("{error:#}"),
        }
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::Locked => "encrypted_locked",
            Self::Other(_) => "store_error",
        }
    }
}

impl From<anyhow::Error> for StoreError {
    fn from(value: anyhow::Error) -> Self {
        Self::Other(value)
    }
}

pub fn data_dir() -> PathBuf {
    // Keep the on-disk folder as `tensorUI` forever — product branding (TensorMI Harness)
    // must not move chats/config and orphan existing installs.
    ProjectDirs::from("", "", "tensorUI")
        .map(|dirs| dirs.config_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("tensorUI-data"))
}

pub fn chats_path(root: &Path) -> PathBuf {
    root.join(CHATS_FILE)
}

pub fn preferences_path(root: &Path) -> PathBuf {
    root.join(PREFERENCES_FILE)
}

pub fn provider_tokens_path(root: &Path) -> PathBuf {
    root.join(PROVIDER_TOKENS_FILE)
}

pub fn load_provider_tokens(root: &Path, key: &crypto::DiskKey) -> Result<Value, StoreError> {
    match read_json_file(&provider_tokens_path(root))? {
        Some(value) => crypto::decrypt_value(key, &value, crypto::AAD_PROVIDER_TOKENS)
            .map_err(StoreError::from),
        None if encryption_enabled(root) => Err(StoreError::Other(anyhow::anyhow!(
            "Encrypted provider credentials are missing. Restore the encrypted file from backup."
        ))),
        None => Ok(serde_json::json!({})),
    }
}

pub fn save_provider_tokens(
    root: &Path,
    value: &Value,
    key: &crypto::DiskKey,
) -> Result<(), StoreError> {
    ensure_data_dir(root)?;
    let encrypted = crypto::encrypt_value(key, value, crypto::AAD_PROVIDER_TOKENS)?;
    atomic_write_json(&provider_tokens_path(root), &encrypted)?;
    Ok(())
}

pub fn clear_provider_tokens(root: &Path) -> Result<()> {
    secure_fs::remove_file(&provider_tokens_path(root))
}

pub fn ensure_data_dir(root: &Path) -> Result<()> {
    secure_fs::ensure_private_dir(root)
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    secure_fs::atomic_write_json(path, value)
}

fn read_json_file(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = secure_fs::read_to_string(path)?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let value = serde_json::from_str(&raw)
        .with_context(|| format!("invalid JSON at {}", path.display()))?;
    Ok(Some(value))
}

fn empty_store() -> Value {
    serde_json::json!({
        "version": 2,
        "projects": [],
        "conversations": [],
        "bots": [],
    })
}

fn normalize_store(value: Value) -> Value {
    match value {
        Value::Array(conversations) => serde_json::json!({
            "version": 2,
            "projects": [],
            "conversations": conversations,
            "bots": [],
        }),
        Value::Object(mut map) => {
            if !map.contains_key("projects") {
                map.insert("projects".into(), Value::Array(vec![]));
            }
            if !map.contains_key("conversations") {
                map.insert("conversations".into(), Value::Array(vec![]));
            }
            if !map.contains_key("bots") {
                map.insert("bots".into(), Value::Array(vec![]));
            }
            if !map.contains_key("version") {
                map.insert("version".into(), Value::from(2));
            }
            Value::Object(map)
        }
        _ => empty_store(),
    }
}

fn decode_stored(
    value: Value,
    key: Option<&crypto::DiskKey>,
    aad: &[u8],
    encryption_on: bool,
) -> Result<Value, StoreError> {
    if crypto::is_envelope(&value) {
        let Some(key) = key else {
            return Err(StoreError::Locked);
        };
        Ok(crypto::decrypt_value(key, &value, aad)?)
    } else if encryption_on {
        // Meta says encrypted — refuse plaintext so a swapped/leftover file
        // cannot silently bypass the passphrase.
        Err(StoreError::Other(anyhow::anyhow!(
            "Encrypted local data is missing or was replaced with plaintext. Restore the encrypted file or turn encryption off with a backup."
        )))
    } else {
        Ok(value)
    }
}

fn encode_for_disk(value: &Value, key: Option<&crypto::DiskKey>, aad: &[u8]) -> Result<Value> {
    if let Some(key) = key {
        crypto::encrypt_value(key, value, aad)
    } else {
        Ok(value.clone())
    }
}

pub fn encryption_enabled(root: &Path) -> bool {
    // Presence is authoritative even when parsing fails: malformed security state
    // must lock the app and surface an error, never silently fall back to plaintext.
    std::fs::symlink_metadata(crypto::meta_path(root)).is_ok()
        || crate::encryption_transition::exists(root)
}

pub fn load_chats(root: &Path, key: Option<&crypto::DiskKey>) -> Result<Value, StoreError> {
    ensure_data_dir(root)?;
    let encryption_on = encryption_enabled(root);
    if encryption_on && key.is_none() {
        // Even if the file is missing, treat as locked so the UI prompts unlock.
        return Err(StoreError::Locked);
    }
    match read_json_file(&chats_path(root))? {
        Some(value) => Ok(normalize_store(decode_stored(
            value,
            key,
            crypto::AAD_CHATS,
            encryption_on,
        )?)),
        None if encryption_on => Err(StoreError::Other(anyhow::anyhow!(
            "Encrypted chat data is missing. Restore the encrypted file from backup."
        ))),
        None => Ok(empty_store()),
    }
}

pub fn save_chats(
    root: &Path,
    value: Value,
    key: Option<&crypto::DiskKey>,
) -> Result<(), StoreError> {
    ensure_data_dir(root)?;
    if encryption_enabled(root) && key.is_none() {
        return Err(StoreError::Locked);
    }
    let normalized = normalize_store(value);
    if !normalized
        .get("projects")
        .map(|v| v.is_array())
        .unwrap_or(false)
        || !normalized
            .get("conversations")
            .map(|v| v.is_array())
            .unwrap_or(false)
        || !normalized
            .get("bots")
            .map(|v| v.is_array())
            .unwrap_or(false)
    {
        return Err(StoreError::Other(anyhow::anyhow!(
            "store must include projects, conversations, and bots arrays"
        )));
    }
    let on_disk = encode_for_disk(&normalized, key, crypto::AAD_CHATS)?;
    atomic_write_json(&chats_path(root), &on_disk)?;
    Ok(())
}

pub fn load_preferences(root: &Path, key: Option<&crypto::DiskKey>) -> Result<Value, StoreError> {
    ensure_data_dir(root)?;
    let encryption_on = encryption_enabled(root);
    if encryption_on && key.is_none() {
        return Err(StoreError::Locked);
    }
    match read_json_file(&preferences_path(root))? {
        Some(value) => match decode_stored(value, key, crypto::AAD_PREFERENCES, encryption_on)? {
            Value::Object(map) => Ok(Value::Object(map)),
            _ => Ok(serde_json::json!({})),
        },
        None if encryption_on => Err(StoreError::Other(anyhow::anyhow!(
            "Encrypted preferences are missing. Restore the encrypted file from backup."
        ))),
        None => Ok(serde_json::json!({})),
    }
}

pub fn save_preferences(
    root: &Path,
    value: Value,
    key: Option<&crypto::DiskKey>,
) -> Result<(), StoreError> {
    ensure_data_dir(root)?;
    if encryption_enabled(root) && key.is_none() {
        return Err(StoreError::Locked);
    }
    let Value::Object(_) = &value else {
        return Err(StoreError::Other(anyhow::anyhow!(
            "preferences must be a JSON object"
        )));
    };
    let on_disk = encode_for_disk(&value, key, crypto::AAD_PREFERENCES)?;
    atomic_write_json(&preferences_path(root), &on_disk)?;
    Ok(())
}

/// Write plaintext while encryption meta still exists (disable path only).
pub fn save_chats_plaintext_for_disable(root: &Path, value: Value) -> Result<(), StoreError> {
    ensure_data_dir(root)?;
    let normalized = normalize_store(value);
    atomic_write_json(&chats_path(root), &normalized)?;
    Ok(())
}

/// Write plaintext while encryption meta still exists (disable path only).
pub fn save_preferences_plaintext_for_disable(root: &Path, value: Value) -> Result<(), StoreError> {
    ensure_data_dir(root)?;
    let Value::Object(_) = &value else {
        return Err(StoreError::Other(anyhow::anyhow!(
            "preferences must be a JSON object"
        )));
    };
    atomic_write_json(&preferences_path(root), &value)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn chats_roundtrip_and_legacy_array() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        save_chats(
            root,
            serde_json::json!([{ "id": "c1", "title": "Hi", "messages": [] }]),
            None,
        )
        .unwrap();
        let loaded = load_chats(root, None).unwrap();
        assert_eq!(loaded["version"], 2);
        assert!(loaded["projects"].as_array().unwrap().is_empty());
        assert_eq!(loaded["conversations"][0]["id"], "c1");
        assert!(loaded["bots"].as_array().unwrap().is_empty());
    }

    #[test]
    fn preferences_roundtrip() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        save_preferences(
            root,
            serde_json::json!({ "name": "Ada", "agentMode": true }),
            None,
        )
        .unwrap();
        let loaded = load_preferences(root, None).unwrap();
        assert_eq!(loaded["name"], "Ada");
        assert_eq!(loaded["agentMode"], true);
    }

    #[test]
    fn encrypted_roundtrip_requires_key() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let salt = crypto::random_salt().unwrap();
        let key = crypto::derive_key("test-passphrase", &salt).unwrap();
        let meta = crypto::meta_for_key(&key, salt).unwrap();
        // Encrypt files before meta so a crash cannot leave "enabled + plaintext".
        save_chats(
            root,
            serde_json::json!([{ "id": "c1", "title": "Secret", "messages": [] }]),
            Some(&key),
        )
        .unwrap();
        crypto::save_meta(root, &meta).unwrap();
        assert!(matches!(load_chats(root, None), Err(StoreError::Locked)));
        let loaded = load_chats(root, Some(&key)).unwrap();
        assert_eq!(loaded["conversations"][0]["title"], "Secret");
        let raw = read_json_file(&chats_path(root)).unwrap().unwrap();
        assert!(crypto::is_envelope(&raw));
    }

    #[test]
    fn encrypted_preferences_hide_model_picker_state() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let salt = crypto::random_salt().unwrap();
        let key = crypto::derive_key("test-passphrase", &salt).unwrap();
        let preferences = serde_json::json!({
            "selectedChatModel": "provider|secret-model",
            "pinnedModelIds": ["provider|secret-model"],
            "recentModelIds": ["provider|secret-model"]
        });
        save_preferences(root, preferences.clone(), Some(&key)).unwrap();
        crypto::save_meta(root, &crypto::meta_for_key(&key, salt).unwrap()).unwrap();

        let raw = read_json_file(&preferences_path(root)).unwrap().unwrap();
        let serialized = serde_json::to_string(&raw).unwrap();
        assert!(crypto::is_envelope(&raw));
        assert!(!serialized.contains("secret-model"));
        assert_eq!(load_preferences(root, Some(&key)).unwrap(), preferences);
    }

    #[test]
    fn encryption_rejects_plaintext_file() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let salt = crypto::random_salt().unwrap();
        let key = crypto::derive_key("test-passphrase", &salt).unwrap();
        crypto::save_meta(root, &crypto::meta_for_key(&key, salt).unwrap()).unwrap();
        // Attacker (or crash) left plaintext on disk while meta claims encryption.
        atomic_write_json(
            &chats_path(root),
            &serde_json::json!({
                "version": 2,
                "projects": [],
                "conversations": [{ "id": "c1", "title": "leaked", "messages": [] }],
            }),
        )
        .unwrap();
        assert!(load_chats(root, Some(&key)).is_err());
    }

    #[test]
    fn encryption_rejects_missing_protected_files() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let salt = crypto::random_salt().unwrap();
        let key = crypto::derive_key("test-passphrase", &salt).unwrap();
        crypto::save_meta(root, &crypto::meta_for_key(&key, salt).unwrap()).unwrap();
        assert!(load_chats(root, Some(&key)).is_err());
        assert!(load_preferences(root, Some(&key)).is_err());
        assert!(load_provider_tokens(root, &key).is_err());
    }

    #[test]
    fn provider_tokens_are_authenticated_and_encrypted() {
        let dir = tempdir().unwrap();
        let root = dir.path();
        let salt = crypto::random_salt().unwrap();
        let key = crypto::derive_key("a sufficiently long passphrase", &salt).unwrap();
        let tokens = serde_json::json!({ "provider-1": "secret-token" });
        save_provider_tokens(root, &tokens, &key).unwrap();

        let raw = read_json_file(&provider_tokens_path(root))
            .unwrap()
            .unwrap();
        assert!(crypto::is_envelope(&raw));
        assert!(
            !serde_json::to_string(&raw)
                .unwrap()
                .contains("secret-token")
        );
        assert_eq!(load_provider_tokens(root, &key).unwrap(), tokens);
    }
}
