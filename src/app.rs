use std::{net::SocketAddr, path::PathBuf};

use crate::{
    config::Config,
    crypto,
    local_llm::LocalLlmManager,
    providers::{
        ApiStyle, CatalogCache, HealthCache, ProviderHealth, ProviderPublic, RemoteModelOption,
        mask_token, normalize_provider_base, probe_provider_catalog, probe_provider_health,
        split_openai_base,
    },
    store::{self, StorageMode, StoreError},
};

type WarmTarget = (ApiStyle, String, String);

#[derive(Debug)]
pub struct App {
    pub config: Config,
    pub config_path: PathBuf,
    pub listen_addr: Option<SocketAddr>,
    pub remote_health: HealthCache,
    pub remote_catalog: CatalogCache,
    /// Session key for disk encryption at rest. Cleared on lock / process exit.
    disk_key: Option<crypto::DiskKey>,
    pub local_llm: LocalLlmManager,
}

impl App {
    pub fn new(mut config: Config, config_path: PathBuf) -> Self {
        let before = config.clone();
        config.providers.migrate();
        config.ui.normalize_fonts();
        config.keep_ui_private();
        if config != before {
            let _ = config.save(&config_path);
        }
        Self {
            config,
            config_path,
            listen_addr: None,
            remote_health: HealthCache::default(),
            remote_catalog: CatalogCache::default(),
            disk_key: None,
            local_llm: LocalLlmManager::default(),
        }
    }

    pub fn set_listen_addr(&mut self, addr: SocketAddr) {
        self.listen_addr = Some(addr);
    }

    pub fn shutdown(&mut self) {
        let _ = self.local_llm.stop();
        self.lock_disk_encryption();
    }

    pub fn persist_config(&mut self) {
        let _ = self.config.save(&self.config_path);
    }

    pub fn skill_store(&self) -> crate::skills::SkillStore {
        crate::skills::SkillStore::new(&self.config_path)
    }

    pub fn list_user_skills(&self) -> Result<Vec<crate::skills::UserSkill>, String> {
        self.skill_store().list().map_err(|error| error.to_string())
    }

    pub fn enabled_user_skills(&self) -> Vec<crate::skills::UserSkill> {
        self.skill_store().enabled_skills().unwrap_or_default()
    }

    pub fn create_user_skill(
        &self,
        upsert: crate::skills::SkillUpsert,
    ) -> Result<crate::skills::UserSkill, String> {
        self.skill_store()
            .create(upsert)
            .map_err(|error| error.to_string())
    }

    pub fn import_user_skill(
        &self,
        filename: Option<&str>,
        content: &str,
    ) -> Result<crate::skills::UserSkill, String> {
        self.skill_store()
            .import_markdown(filename, content)
            .map_err(|error| error.to_string())
    }

    pub fn update_user_skill(
        &self,
        id: &str,
        upsert: crate::skills::SkillUpsert,
    ) -> Result<crate::skills::UserSkill, String> {
        self.skill_store()
            .update(id, upsert)
            .map_err(|error| error.to_string())
    }

    pub fn delete_user_skill(&self, id: &str) -> Result<(), String> {
        self.skill_store()
            .delete(id)
            .map_err(|error| error.to_string())
    }

    pub fn remote_health_cached(&self) -> Option<ProviderHealth> {
        let remote = self.config.providers.active()?;
        let base = remote.base.trim();
        if base.is_empty() {
            return None;
        }
        self.remote_health
            .peek(remote.api_style, base, remote.token.trim())
    }

    pub fn remote_health_for_cached(
        &self,
        style: ApiStyle,
        base: &str,
        token: &str,
    ) -> Option<ProviderHealth> {
        self.remote_health.peek(style, base.trim(), token.trim())
    }

    pub fn store_remote_health(
        &self,
        style: ApiStyle,
        base: &str,
        token: &str,
        health: ProviderHealth,
    ) {
        self.remote_health
            .put(style, base.trim(), token.trim(), health);
    }

    pub fn store_remote_catalog(
        &self,
        style: ApiStyle,
        base: &str,
        token: &str,
        catalog: Vec<RemoteModelOption>,
    ) {
        self.remote_catalog
            .put(style, base.trim(), token.trim(), catalog);
    }

    pub fn remote_caches_need_warm(&self) -> bool {
        for remote in &self.config.providers.items {
            let base = remote.base.trim();
            if base.is_empty() {
                continue;
            }
            if !self
                .remote_health
                .is_fresh(remote.api_style, base, remote.token.trim())
            {
                return true;
            }
        }
        for remote in &self.config.providers.items {
            let base = remote.base.trim();
            if base.is_empty() {
                continue;
            }
            if !self
                .remote_catalog
                .is_fresh(remote.api_style, base, remote.token.trim())
            {
                return true;
            }
        }
        false
    }

    pub fn provider_warm_targets(&self) -> (Vec<WarmTarget>, Vec<WarmTarget>) {
        let health_targets = self
            .config
            .providers
            .items
            .iter()
            .filter(|remote| {
                let base = remote.base.trim();
                !base.is_empty()
                    && !self
                        .remote_health
                        .is_fresh(remote.api_style, base, remote.token.trim())
            })
            .map(|remote| {
                (
                    remote.api_style,
                    remote.base.trim().to_string(),
                    remote.token.trim().to_string(),
                )
            })
            .collect();
        let catalog_targets = self
            .config
            .providers
            .items
            .iter()
            .filter(|remote| {
                let base = remote.base.trim();
                !base.is_empty()
                    && !self
                        .remote_catalog
                        .is_fresh(remote.api_style, base, remote.token.trim())
            })
            .map(|remote| {
                (
                    remote.api_style,
                    remote.base.trim().to_string(),
                    remote.token.trim().to_string(),
                )
            })
            .collect();
        (health_targets, catalog_targets)
    }

    pub fn remote_model_catalog_cached(&self) -> Vec<RemoteModelOption> {
        self.remote_model_catalog_peek().unwrap_or_default()
    }

    /// Merged model catalogs from every saved provider, stamped with provider badges.
    /// `None` means no provider has been probed yet.
    pub fn remote_model_catalog_peek(&self) -> Option<Vec<RemoteModelOption>> {
        let mut merged = Vec::new();
        let mut any_known = false;
        for provider in &self.config.providers.items {
            let base = provider.base.trim();
            if base.is_empty() {
                continue;
            }
            let Some(catalog) =
                self.remote_catalog
                    .peek(provider.api_style, base, provider.token.trim())
            else {
                continue;
            };
            any_known = true;
            for mut opt in catalog {
                opt.provider_id = provider.id.clone();
                opt.provider_name = provider.name.clone();
                // Include provider id so the same model on two providers stays distinct.
                opt.id = format!("remote|{}|{}|{}", provider.id, opt.base, opt.model);
                merged.push(opt);
            }
        }
        if !any_known {
            return None;
        }
        merged.sort_by(|a, b| {
            a.provider_name
                .cmp(&b.provider_name)
                .then(a.model.cmp(&b.model))
                .then(a.base.cmp(&b.base))
        });
        Some(merged)
    }

    pub fn public_providers(&self) -> Vec<ProviderPublic> {
        let active_id = self
            .config
            .providers
            .active()
            .map(|p| p.id.clone())
            .unwrap_or_default();
        self.config
            .providers
            .items
            .iter()
            .map(|remote| ProviderPublic {
                id: remote.id.clone(),
                name: remote.name.clone(),
                base: remote.base.clone(),
                api_style: remote.api_style.as_str(),
                token_set: !remote.token.trim().is_empty(),
                token_masked: mask_token(&remote.token),
                active: remote.id == active_id,
                health: self.remote_health_for_cached(
                    remote.api_style,
                    &remote.base,
                    &remote.token,
                ),
            })
            .collect()
    }

    pub fn create_provider(
        &mut self,
        name: &str,
        base: &str,
        token: &str,
        api_style: ApiStyle,
        activate: bool,
    ) -> Result<(), String> {
        self.config
            .providers
            .upsert(None, name, base, Some(token), api_style, activate)?;
        self.persist_providers("provider added")
    }

    pub fn update_provider(
        &mut self,
        id: &str,
        name: Option<&str>,
        base: Option<&str>,
        token: Option<&str>,
        api_style: Option<ApiStyle>,
    ) -> Result<(), String> {
        self.config.providers.migrate();
        let current = self
            .config
            .providers
            .items
            .iter()
            .find(|p| p.id == id)
            .cloned()
            .ok_or_else(|| "Provider not found.".to_string())?;
        self.config.providers.upsert(
            Some(id),
            name.unwrap_or(&current.name),
            base.unwrap_or(&current.base),
            token,
            api_style.unwrap_or(current.api_style),
            false,
        )?;
        self.persist_providers("provider updated")
    }

    pub fn delete_provider(&mut self, id: &str) -> Result<(), String> {
        self.config.providers.delete(id)?;
        self.persist_providers("provider removed")
    }

    pub fn activate_provider(&mut self, id: &str) -> Result<(), String> {
        self.config.providers.set_active(id)?;
        self.persist_providers("provider activated")
    }

    /// Register or refresh the managed llama-server provider and make it default.
    pub fn ensure_local_llama_provider(&mut self, base_url: &str) -> Result<(), String> {
        let id = crate::local_llm::provider_id();
        let name = crate::local_llm::provider_name();
        let Some(normalized) = normalize_provider_base(base_url, ApiStyle::Openai) else {
            return Err("Invalid local llama-server base URL.".into());
        };
        if let Some(provider) = self.config.providers.items.iter_mut().find(|p| p.id == id) {
            provider.name = name.to_string();
            provider.base = normalized;
            provider.api_style = ApiStyle::Openai;
            provider.token.clear();
            self.config.providers.active_provider_id = id.to_string();
        } else {
            if self.config.providers.items.len() >= 32 {
                return Err("Maximum of 32 providers reached.".into());
            }
            let mut provider =
                crate::providers::Provider::new(name, normalized, "", ApiStyle::Openai);
            provider.id = id.to_string();
            self.config.providers.active_provider_id = id.to_string();
            self.config.providers.items.push(provider);
        }
        self.persist_providers("local llama provider")
    }

    fn persist_providers(&mut self, _action: &str) -> Result<(), String> {
        self.config.providers.migrate();
        self.config.keep_ui_private();
        self.config
            .save(&self.config_path)
            .map_err(|error| format!("could not save config: {error:#}"))?;
        Ok(())
    }

    pub fn set_ui_theme(&mut self, theme: crate::config::UiTheme) {
        if self.config.ui.theme == theme {
            return;
        }
        self.config.ui.theme = theme;
        self.persist_config();
    }

    pub fn set_ui_appearance(
        &mut self,
        theme: Option<crate::config::UiTheme>,
        font_body: Option<String>,
        font_display: Option<String>,
        font_mono: Option<String>,
        font_scale: Option<crate::config::UiFontScale>,
    ) -> Result<(), String> {
        let mut changed = false;
        if let Some(theme) = theme
            && self.config.ui.theme != theme
        {
            self.config.ui.theme = theme;
            changed = true;
        }
        if let Some(font_body) = font_body {
            let id = font_body.trim().to_ascii_lowercase();
            if !crate::config::UI_FONT_BODY_IDS.contains(&id.as_str()) {
                return Err(format!(
                    "font_body must be one of: {}",
                    crate::config::UI_FONT_BODY_IDS.join(", ")
                ));
            }
            if self.config.ui.font_body != id {
                self.config.ui.font_body = id;
                changed = true;
            }
        }
        if let Some(font_display) = font_display {
            let id = font_display.trim().to_ascii_lowercase();
            if !crate::config::UI_FONT_DISPLAY_IDS.contains(&id.as_str()) {
                return Err(format!(
                    "font_display must be one of: {}",
                    crate::config::UI_FONT_DISPLAY_IDS.join(", ")
                ));
            }
            if self.config.ui.font_display != id {
                self.config.ui.font_display = id;
                changed = true;
            }
        }
        if let Some(font_mono) = font_mono {
            let id = font_mono.trim().to_ascii_lowercase();
            if !crate::config::UI_FONT_MONO_IDS.contains(&id.as_str()) {
                return Err(format!(
                    "font_mono must be one of: {}",
                    crate::config::UI_FONT_MONO_IDS.join(", ")
                ));
            }
            if self.config.ui.font_mono != id {
                self.config.ui.font_mono = id;
                changed = true;
            }
        }
        if let Some(font_scale) = font_scale
            && self.config.ui.font_scale != font_scale
        {
            self.config.ui.font_scale = font_scale;
            changed = true;
        }
        if changed {
            self.persist_config();
        }
        Ok(())
    }

    pub fn reset_ui_appearance(&mut self) {
        let before = self.config.ui.clone();
        self.config.ui.reset_appearance();
        if self.config.ui != before {
            self.persist_config();
        }
    }

    pub fn app_version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    pub fn data_dir(&self) -> std::path::PathBuf {
        self.config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf())
            .unwrap_or_else(store::data_dir)
    }

    pub fn storage_mode(&self) -> StorageMode {
        self.config.data.storage
    }

    pub fn set_storage_mode(&mut self, mode: StorageMode) {
        if self.config.data.storage != mode {
            self.config.data.storage = mode;
            self.persist_config();
        }
    }

    pub fn encryption_enabled(&self) -> bool {
        store::encryption_enabled(&self.data_dir())
    }

    pub fn encryption_unlocked(&self) -> bool {
        self.disk_key.is_some()
    }

    fn disk_key(&self) -> Option<&crypto::DiskKey> {
        self.disk_key.as_ref()
    }

    fn map_store_err(error: StoreError) -> String {
        error.message()
    }

    pub fn load_chat_store(&self) -> Result<serde_json::Value, String> {
        store::load_chats(&self.data_dir(), self.disk_key()).map_err(Self::map_store_err)
    }

    pub fn save_chat_store(&self, value: serde_json::Value) -> Result<(), String> {
        store::save_chats(&self.data_dir(), value, self.disk_key()).map_err(Self::map_store_err)
    }

    pub fn load_chat_preferences(&self) -> Result<serde_json::Value, String> {
        store::load_preferences(&self.data_dir(), self.disk_key()).map_err(Self::map_store_err)
    }

    pub fn save_chat_preferences(&self, value: serde_json::Value) -> Result<(), String> {
        store::save_preferences(&self.data_dir(), value, self.disk_key())
            .map_err(Self::map_store_err)
    }

    fn derive_and_verify_key(
        passphrase: &str,
        meta: &crypto::EncryptionMeta,
        root: &std::path::Path,
    ) -> Result<crypto::DiskKey, String> {
        let key =
            crypto::derive_key_from_meta(passphrase, meta).map_err(|error| format!("{error:#}"))?;
        if meta.verifier.is_some() {
            crypto::verify_key(&key, meta).map_err(|error| format!("{error:#}"))?;
        } else {
            // Legacy meta (pre-verifier): prove the key by opening an encrypted file.
            // Reject if both files are plaintext — that used to accept any passphrase.
            let chats_path = store::chats_path(root);
            let prefs_path = store::preferences_path(root);
            let chats_raw = std::fs::read_to_string(&chats_path).ok();
            let prefs_raw = std::fs::read_to_string(&prefs_path).ok();
            let chats_env = chats_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .filter(crypto::is_envelope);
            let prefs_env = prefs_raw
                .as_deref()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(raw).ok())
                .filter(crypto::is_envelope);
            match (chats_env, prefs_env) {
                (Some(value), _) => {
                    crypto::decrypt_value(&key, &value, crypto::AAD_CHATS)
                        .map_err(|error| format!("{error:#}"))?;
                }
                (_, Some(value)) => {
                    crypto::decrypt_value(&key, &value, crypto::AAD_PREFERENCES)
                        .map_err(|error| format!("{error:#}"))?;
                }
                (None, None) => {
                    return Err(
                        "Encrypted data is missing a key verifier and no encrypted files were found. Re-enable encryption after restoring a backup."
                            .into(),
                    );
                }
            }
        }
        Ok(key)
    }

    pub fn unlock_disk_encryption(&mut self, passphrase: &str) -> Result<(), String> {
        let root = self.data_dir();
        let mut meta = crypto::load_meta(&root)
            .map_err(|error| format!("{error:#}"))?
            .ok_or_else(|| "Disk encryption is not enabled.".to_string())?;
        let key = Self::derive_and_verify_key(passphrase, &meta, &root)?;

        // Confirm payloads decrypt (also rejects plaintext leftovers).
        let _ = store::load_chats(&root, Some(&key)).map_err(Self::map_store_err)?;
        let _ = store::load_preferences(&root, Some(&key)).map_err(Self::map_store_err)?;

        // Migrate legacy meta to include a key verifier (and bump version).
        if meta.verifier.is_none()
            && let Ok(verifier) = crypto::make_verifier(&key)
        {
            meta.version = 2;
            meta.verifier = Some(verifier);
            let _ = crypto::save_meta(&root, &meta);
        }

        self.disk_key = Some(key);
        Ok(())
    }

    pub fn lock_disk_encryption(&mut self) {
        self.disk_key = None; // DiskKey zeroizes on drop
    }

    pub fn enable_disk_encryption(
        &mut self,
        passphrase: &str,
        passphrase_confirm: &str,
    ) -> Result<(), String> {
        if self.storage_mode().is_browser() {
            return Err(
                "Switch off browser localStorage first — encryption applies to on-disk data."
                    .into(),
            );
        }
        if passphrase != passphrase_confirm {
            return Err("Passphrases do not match.".into());
        }
        crypto::validate_passphrase(passphrase).map_err(|error| format!("{error:#}"))?;
        let root = self.data_dir();
        if store::encryption_enabled(&root) {
            return Err("Disk encryption is already enabled.".into());
        }

        // Read current plaintext while encryption is still off.
        let chats = store::load_chats(&root, None).map_err(Self::map_store_err)?;
        let prefs = store::load_preferences(&root, None).map_err(Self::map_store_err)?;

        let salt = crypto::random_salt().map_err(|error| format!("{error:#}"))?;
        let key = crypto::derive_key(passphrase, &salt).map_err(|error| format!("{error:#}"))?;
        let meta = crypto::meta_for_key(&key, salt).map_err(|error| format!("{error:#}"))?;

        // Encrypt payloads first, then write meta last. A crash mid-way must not
        // leave encryption.json claiming "on" while files are still plaintext.
        store::save_chats(&root, chats, Some(&key)).map_err(Self::map_store_err)?;
        store::save_preferences(&root, prefs, Some(&key)).map_err(Self::map_store_err)?;
        crypto::save_meta(&root, &meta).map_err(|error| format!("{error:#}"))?;
        self.disk_key = Some(key);
        Ok(())
    }

    pub fn disable_disk_encryption(&mut self, passphrase: &str) -> Result<(), String> {
        let root = self.data_dir();
        let meta = crypto::load_meta(&root)
            .map_err(|error| format!("{error:#}"))?
            .ok_or_else(|| "Disk encryption is not enabled.".to_string())?;
        let key = Self::derive_and_verify_key(passphrase, &meta, &root)?;

        let chats = store::load_chats(&root, Some(&key)).map_err(Self::map_store_err)?;
        let prefs = store::load_preferences(&root, Some(&key)).map_err(Self::map_store_err)?;

        // Write plaintext first while meta still exists (dedicated helpers), then
        // remove meta. Avoids a window of encrypted files with no meta.
        store::save_chats_plaintext_for_disable(&root, chats).map_err(Self::map_store_err)?;
        store::save_preferences_plaintext_for_disable(&root, prefs).map_err(Self::map_store_err)?;
        crypto::clear_meta(&root).map_err(|error| format!("{error:#}"))?;
        self.disk_key = None;
        Ok(())
    }

    pub fn thinking_supported(&self) -> bool {
        self.remote_model_catalog_cached()
            .iter()
            .any(|m| m.thinking_supported)
    }

    /// Primary ports owned by every configured provider *other* than `base`.
    /// Feeds `probe_provider_catalog` so providers can't absorb each other.
    pub fn other_provider_ports(&self, base: &str) -> Vec<u16> {
        let own = normalize_provider_base(base, ApiStyle::Openai);
        self.config
            .providers
            .items
            .iter()
            .filter(|provider| {
                let candidate = provider.base.trim();
                !candidate.is_empty()
                    && normalize_provider_base(candidate, provider.api_style) != own
            })
            .filter_map(|provider| split_openai_base(provider.base.trim()).map(|(_, _, port)| port))
            .collect()
    }

    pub fn warm_provider_caches(&self) {
        let (health_targets, catalog_targets) = self.provider_warm_targets();
        for (style, base, token) in health_targets {
            let health = probe_provider_health(&base, &token, style);
            self.store_remote_health(style, &base, &token, health);
        }
        for (style, base, token) in catalog_targets {
            let others = self.other_provider_ports(&base);
            let catalog = probe_provider_catalog(&base, &token, style, &[], &others);
            self.store_remote_catalog(style, &base, &token, catalog);
        }
    }
}
