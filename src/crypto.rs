//! Passphrase-based encryption at rest for local chat data (Argon2id + AES-256-GCM).
//!
//! Threat model: protects chats, preferences, provider credentials, and user skills
//! against offline reading (stolen laptop backup, casual filesystem access). The
//! passphrase is never stored. The derived key lives only in process memory until
//! lock / exit.
//!
//! Not covered: pre-existing copies/backups, filenames and timestamps, OS memory
//! dumps, malware in the same unlocked user session, or a forgotten passphrase.

use std::path::{Path, PathBuf};

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use anyhow::{Context, Result, bail};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD as B64};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::secure_fs;

pub const META_FILE: &str = "encryption.json";
const SALT_LEN: usize = 32;
const MIN_SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;
const KEY_LEN: usize = 32;
/// Envelope without associated data (legacy installs).
const ENVELOPE_V1: u32 = 1;
/// Envelope authenticated with a purpose string (AAD).
const ENVELOPE_V2: u32 = 2;
const META_VERSION: u32 = 2;
const MAX_META_BYTES: u64 = 64 * 1024;

/// Deliberately above the OWASP interactive Argon2id minimum.
const KDF_MEMORY_KIB: u32 = 65_536;
const KDF_ITERATIONS: u32 = 3;
const KDF_PARALLELISM: u32 = 1;
const LEGACY_KDF_MEMORY_KIB: u32 = 19_456;
const LEGACY_KDF_ITERATIONS: u32 = 2;

pub const AAD_CHATS: &[u8] = b"tensorui:v1:chats";
pub const AAD_PREFERENCES: &[u8] = b"tensorui:v1:preferences";
pub const AAD_PROVIDER_TOKENS: &[u8] = b"tensorui:v1:provider-tokens";
pub const AAD_SKILL_INDEX: &[u8] = b"tensorui:v1:skill-index";
pub const AAD_SKILL_CONTENT: &[u8] = b"tensorui:v1:skill-content";
pub const AAD_SKILLS: &[u8] = b"tensorui:v1:skills";
pub const AAD_ENCRYPTION_TRANSITION: &[u8] = b"tensorui:v1:encryption-transition";
const AAD_VERIFIER: &[u8] = b"tensorui:v1:verifier";
const VERIFIER_PLAINTEXT: &[u8] = b"tensorui-ok";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EncryptionMeta {
    pub version: u32,
    pub kdf: String,
    #[serde(default = "default_kdf_memory")]
    pub kdf_memory_kib: u32,
    #[serde(default = "default_kdf_iterations")]
    pub kdf_iterations: u32,
    #[serde(default = "default_kdf_parallelism")]
    pub kdf_parallelism: u32,
    pub salt: String,
    /// AES-GCM envelope that only the correct key can open. Prevents "any
    /// passphrase unlocks" when data files are still plaintext.
    #[serde(default)]
    pub verifier: Option<Value>,
}

fn default_kdf_memory() -> u32 {
    LEGACY_KDF_MEMORY_KIB
}
fn default_kdf_iterations() -> u32 {
    LEGACY_KDF_ITERATIONS
}
fn default_kdf_parallelism() -> u32 {
    KDF_PARALLELISM
}

impl EncryptionMeta {
    pub fn new(salt: [u8; SALT_LEN], verifier: Value) -> Self {
        Self {
            version: META_VERSION,
            kdf: "argon2id".into(),
            kdf_memory_kib: KDF_MEMORY_KIB,
            kdf_iterations: KDF_ITERATIONS,
            kdf_parallelism: KDF_PARALLELISM,
            salt: B64.encode(salt),
            verifier: Some(verifier),
        }
    }

    pub fn salt_bytes(&self) -> Result<Vec<u8>> {
        let raw = B64
            .decode(self.salt.trim())
            .context("invalid encryption salt")?;
        if !(MIN_SALT_LEN..=64).contains(&raw.len()) {
            bail!("encryption salt has an invalid length");
        }
        Ok(raw)
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    tensorui_crypto: u32,
    nonce: String,
    ciphertext: String,
}

/// Session key that is wiped from memory on drop.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct DiskKey([u8; KEY_LEN]);

impl DiskKey {
    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for DiskKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiskKey([redacted])")
    }
}

pub fn meta_path(root: &Path) -> PathBuf {
    root.join(META_FILE)
}

pub fn load_meta(root: &Path) -> Result<Option<EncryptionMeta>> {
    let path = meta_path(root);
    if !path.exists() {
        return Ok(None);
    }
    let raw = secure_fs::read_limited_to_string(&path, MAX_META_BYTES)?;
    if raw.trim().is_empty() {
        return Ok(None);
    }
    let meta: EncryptionMeta = serde_json::from_str(&raw)
        .with_context(|| format!("invalid encryption metadata at {}", path.display()))?;
    if !matches!(meta.version, 1 | META_VERSION) {
        bail!("unsupported encryption metadata version {}", meta.version);
    }
    if meta.kdf != "argon2id" {
        bail!("unsupported key-derivation algorithm {:?}", meta.kdf);
    }
    Ok(Some(meta))
}

pub fn save_meta(root: &Path, meta: &EncryptionMeta) -> Result<()> {
    secure_fs::ensure_private_dir(root)?;
    let path = meta_path(root);
    let value = serde_json::to_value(meta).context("could not serialize encryption metadata")?;
    atomic_write_json(&path, &value)
}

pub fn clear_meta(root: &Path) -> Result<()> {
    secure_fs::remove_file(&meta_path(root))
}

fn atomic_write_json(path: &Path, value: &Value) -> Result<()> {
    secure_fs::atomic_write_json(path, value)
}

pub fn random_salt() -> Result<[u8; SALT_LEN]> {
    let mut salt = [0u8; SALT_LEN];
    getrandom::fill(&mut salt).context("could not generate salt")?;
    Ok(salt)
}

pub fn derive_key(passphrase: &str, salt: &[u8]) -> Result<DiskKey> {
    derive_key_with_params(
        passphrase,
        salt,
        KDF_MEMORY_KIB,
        KDF_ITERATIONS,
        KDF_PARALLELISM,
    )
}

pub fn derive_key_from_meta(passphrase: &str, meta: &EncryptionMeta) -> Result<DiskKey> {
    let salt = meta.salt_bytes()?;
    derive_key_with_params(
        passphrase,
        &salt,
        meta.kdf_memory_kib,
        meta.kdf_iterations,
        meta.kdf_parallelism,
    )
}

fn derive_key_with_params(
    passphrase: &str,
    salt: &[u8],
    memory_kib: u32,
    iterations: u32,
    parallelism: u32,
) -> Result<DiskKey> {
    if salt.len() < MIN_SALT_LEN {
        bail!("encryption salt is too short");
    }
    // Only accept profiles emitted by this application. Broad attacker-controlled
    // ranges let a tampered metadata file turn unlock into a CPU/RAM denial of service.
    let current =
        (memory_kib, iterations, parallelism) == (KDF_MEMORY_KIB, KDF_ITERATIONS, KDF_PARALLELISM);
    let legacy = (memory_kib, iterations, parallelism)
        == (
            LEGACY_KDF_MEMORY_KIB,
            LEGACY_KDF_ITERATIONS,
            KDF_PARALLELISM,
        );
    if !current && !legacy {
        bail!("encryption metadata has unsupported Argon2 parameters");
    }
    let params = Params::new(memory_kib, iterations, parallelism, Some(KEY_LEN))
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut key = [0u8; KEY_LEN];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|error| anyhow::anyhow!("could not derive key: {error}"))?;
    Ok(DiskKey(key))
}

pub fn validate_passphrase(passphrase: &str) -> Result<()> {
    if passphrase.is_empty() {
        bail!("Passphrase must not be empty.");
    }
    if passphrase.contains('\0') {
        bail!("Passphrase must not contain null characters.");
    }
    Ok(())
}

pub fn is_envelope(value: &Value) -> bool {
    matches!(
        value.get("tensorui_crypto").and_then(|v| v.as_u64()),
        Some(1 | 2)
    ) && value.get("ciphertext").and_then(|v| v.as_str()).is_some()
        && value.get("nonce").and_then(|v| v.as_str()).is_some()
}

pub fn encrypt_value(key: &DiskKey, value: &Value, aad: &[u8]) -> Result<Value> {
    let mut plaintext =
        serde_json::to_vec(value).context("could not serialize JSON for encryption")?;
    let envelope = encrypt_bytes(key, &plaintext, aad)?;
    plaintext.zeroize();
    Ok(envelope)
}

fn encrypt_bytes(key: &DiskKey, plaintext: &[u8], aad: &[u8]) -> Result<Value> {
    let mut nonce_bytes = [0u8; NONCE_LEN];
    getrandom::fill(&mut nonce_bytes).context("could not generate nonce")?;
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid encryption key"))?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|error| anyhow::anyhow!("encryption failed: {error}"))?;
    Ok(serde_json::to_value(Envelope {
        tensorui_crypto: ENVELOPE_V2,
        nonce: B64.encode(nonce_bytes),
        ciphertext: B64.encode(ciphertext),
    })?)
}

pub fn decrypt_value(key: &DiskKey, value: &Value, aad: &[u8]) -> Result<Value> {
    let mut plaintext = decrypt_bytes(key, value, aad)?;
    let parsed = serde_json::from_slice(&plaintext).context("decrypted data is not valid JSON");
    plaintext.zeroize();
    parsed
}

fn decrypt_bytes(key: &DiskKey, value: &Value, aad: &[u8]) -> Result<Vec<u8>> {
    let envelope: Envelope =
        serde_json::from_value(value.clone()).context("invalid encrypted envelope")?;
    let nonce_raw = B64
        .decode(envelope.nonce.trim())
        .context("invalid encryption nonce")?;
    if nonce_raw.len() != NONCE_LEN {
        bail!("unexpected nonce length");
    }
    let ciphertext = B64
        .decode(envelope.ciphertext.trim())
        .context("invalid ciphertext")?;
    let cipher = Aes256Gcm::new_from_slice(key.as_bytes())
        .map_err(|_| anyhow::anyhow!("invalid encryption key"))?;
    let nonce = Nonce::from_slice(&nonce_raw);

    let plaintext = match envelope.tensorui_crypto {
        ENVELOPE_V2 => cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext.as_ref(),
                    aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("Incorrect passphrase or corrupted encrypted data."))?,
        ENVELOPE_V1 => cipher
            .decrypt(nonce, ciphertext.as_ref())
            .map_err(|_| anyhow::anyhow!("Incorrect passphrase or corrupted encrypted data."))?,
        other => bail!("unsupported encryption envelope version {other}"),
    };
    Ok(plaintext)
}

pub fn make_verifier(key: &DiskKey) -> Result<Value> {
    encrypt_bytes(key, VERIFIER_PLAINTEXT, AAD_VERIFIER)
}

pub fn verify_key(key: &DiskKey, meta: &EncryptionMeta) -> Result<()> {
    let Some(verifier) = meta.verifier.as_ref() else {
        bail!("encryption metadata is missing a key verifier");
    };
    let mut plaintext = decrypt_bytes(key, verifier, AAD_VERIFIER)?;
    let ok = plaintext == VERIFIER_PLAINTEXT;
    plaintext.zeroize();
    if !ok {
        bail!("Incorrect passphrase or corrupted encrypted data.");
    }
    Ok(())
}

/// Build meta + verifier for a freshly derived key.
pub fn meta_for_key(key: &DiskKey, salt: [u8; SALT_LEN]) -> Result<EncryptionMeta> {
    Ok(EncryptionMeta::new(salt, make_verifier(key)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let salt = random_salt().unwrap();
        let key = derive_key("correct horse battery", &salt).unwrap();
        let value = serde_json::json!({ "hello": "world", "n": 3 });
        let envelope = encrypt_value(&key, &value, AAD_CHATS).unwrap();
        assert!(is_envelope(&envelope));
        let decoded = decrypt_value(&key, &envelope, AAD_CHATS).unwrap();
        assert_eq!(decoded, value);
    }

    #[test]
    fn aad_mismatch_fails() {
        let salt = random_salt().unwrap();
        let key = derive_key("correct horse battery", &salt).unwrap();
        let envelope = encrypt_value(&key, &serde_json::json!({ "a": 1 }), AAD_CHATS).unwrap();
        assert!(decrypt_value(&key, &envelope, AAD_PREFERENCES).is_err());
    }

    #[test]
    fn wrong_passphrase_fails() {
        let salt = random_salt().unwrap();
        let key = derive_key("passphrase-one", &salt).unwrap();
        let other = derive_key("passphrase-two", &salt).unwrap();
        let envelope = encrypt_value(&key, &serde_json::json!({ "a": 1 }), AAD_CHATS).unwrap();
        assert!(decrypt_value(&other, &envelope, AAD_CHATS).is_err());
    }

    #[test]
    fn verifier_rejects_wrong_key() {
        let salt = random_salt().unwrap();
        let key = derive_key("passphrase-one", &salt).unwrap();
        let other = derive_key("passphrase-two", &salt).unwrap();
        let meta = meta_for_key(&key, salt).unwrap();
        verify_key(&key, &meta).unwrap();
        assert!(verify_key(&other, &meta).is_err());
    }

    #[test]
    fn meta_roundtrip() {
        let dir = tempdir().unwrap();
        let salt = random_salt().unwrap();
        let key = derive_key("test-passphrase", &salt).unwrap();
        let meta = meta_for_key(&key, salt).unwrap();
        save_meta(dir.path(), &meta).unwrap();
        let loaded = load_meta(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.salt_bytes().unwrap(), salt);
        verify_key(&key, &loaded).unwrap();
    }

    #[test]
    fn legacy_v1_envelope_still_decrypts() {
        let salt = random_salt().unwrap();
        let key = derive_key("legacy-pass", &salt).unwrap();
        let plaintext = br#"{"ok":true}"#;
        let mut nonce_bytes = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce_bytes).unwrap();
        let cipher = Aes256Gcm::new_from_slice(key.as_bytes()).unwrap();
        let nonce = Nonce::from_slice(&nonce_bytes);
        let ciphertext = cipher.encrypt(nonce, plaintext.as_ref()).unwrap();
        let envelope = serde_json::json!({
            "tensorui_crypto": 1,
            "nonce": B64.encode(nonce_bytes),
            "ciphertext": B64.encode(ciphertext),
        });
        let decoded = decrypt_value(&key, &envelope, AAD_CHATS).unwrap();
        assert_eq!(decoded["ok"], true);
    }

    #[test]
    fn passphrase_validation() {
        assert!(validate_passphrase("").is_err());
        assert!(validate_passphrase("short").is_ok());
        assert!(validate_passphrase("a long enough passphrase").is_ok());
        assert!(validate_passphrase(&"x".repeat(2048)).is_ok());
        assert!(validate_passphrase("contains\0null").is_err());
    }

    #[test]
    fn rejects_attacker_controlled_kdf_costs() {
        let salt = random_salt().unwrap();
        assert!(derive_key_with_params("passphrase", &salt, 1_048_576, 10, 4).is_err());
        assert!(derive_key_with_params("passphrase", &salt, KDF_MEMORY_KIB, 10, 1).is_err());
    }
}
