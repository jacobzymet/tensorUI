use std::{
    fs::{self, OpenOptions},
    io::Write,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::{providers::ProvidersConfig, store::StorageMode};

pub const DEFAULT_UI_HOST: &str = "127.0.0.1";
pub const DEFAULT_UI_PORT: u16 = 3930;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Config {
    pub ui: UiConfig,
    pub data: DataConfig,
    #[serde(flatten)]
    pub providers: ProvidersConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct DataConfig {
    pub storage: StorageMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum UiTheme {
    #[default]
    Dark,
    Light,
    System,
}

impl UiTheme {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::System => "system",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dark" => Some(Self::Dark),
            "light" => Some(Self::Light),
            "system" | "auto" => Some(Self::System),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum UiFontScale {
    Compact,
    #[default]
    Default,
    Large,
}

impl UiFontScale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Default => "default",
            Self::Large => "large",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "compact" => Some(Self::Compact),
            "default" | "normal" | "medium" => Some(Self::Default),
            "large" => Some(Self::Large),
            _ => None,
        }
    }
}

pub const UI_FONT_BODY_IDS: &[&str] = &[
    "inter",
    "source-sans-3",
    "ibm-plex-sans",
    "atkinson-hyperlegible",
    "literata",
];
pub const UI_FONT_DISPLAY_IDS: &[&str] = &[
    "space-grotesk",
    "syne",
    "dm-sans",
    "fraunces",
    "instrument-sans",
];
pub const UI_FONT_MONO_IDS: &[&str] = &[
    "jetbrains-mono",
    "ibm-plex-mono",
    "source-code-pro",
    "fira-code",
];

pub const DEFAULT_UI_FONT_BODY: &str = "inter";
pub const DEFAULT_UI_FONT_DISPLAY: &str = "space-grotesk";
pub const DEFAULT_UI_FONT_MONO: &str = "jetbrains-mono";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct UiConfig {
    pub host: String,
    pub port: u16,
    pub theme: UiTheme,
    pub font_body: String,
    pub font_display: String,
    pub font_mono: String,
    pub font_scale: UiFontScale,
}

impl UiConfig {
    pub fn normalize_fonts(&mut self) {
        if !UI_FONT_BODY_IDS.contains(&self.font_body.as_str()) {
            self.font_body = DEFAULT_UI_FONT_BODY.into();
        }
        if !UI_FONT_DISPLAY_IDS.contains(&self.font_display.as_str()) {
            self.font_display = DEFAULT_UI_FONT_DISPLAY.into();
        }
        if !UI_FONT_MONO_IDS.contains(&self.font_mono.as_str()) {
            self.font_mono = DEFAULT_UI_FONT_MONO.into();
        }
    }

    pub fn reset_appearance(&mut self) {
        self.theme = UiTheme::Dark;
        self.font_body = DEFAULT_UI_FONT_BODY.into();
        self.font_display = DEFAULT_UI_FONT_DISPLAY.into();
        self.font_mono = DEFAULT_UI_FONT_MONO.into();
        self.font_scale = UiFontScale::Default;
    }
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            host: DEFAULT_UI_HOST.into(),
            port: DEFAULT_UI_PORT,
            theme: UiTheme::Dark,
            font_body: DEFAULT_UI_FONT_BODY.into(),
            font_display: DEFAULT_UI_FONT_DISPLAY.into(),
            font_mono: DEFAULT_UI_FONT_MONO.into(),
            font_scale: UiFontScale::Default,
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path)
            .with_context(|| format!("could not read {}", path.display()))?;
        toml::from_str(&raw).with_context(|| format!("invalid config at {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)
            .with_context(|| format!("could not create {}", parent.display()))?;
        let raw = toml::to_string_pretty(self).context("could not serialize configuration")?;
        let file_name = path
            .file_name()
            .context("configuration path has no file name")?
            .to_string_lossy();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let temporary = parent.join(format!(".{file_name}.{}.{}.tmp", std::process::id(), stamp));

        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(&temporary)
                .with_context(|| format!("could not create {}", temporary.display()))?;
            file.write_all(raw.as_bytes())
                .with_context(|| format!("could not write {}", temporary.display()))?;
            file.sync_all()
                .with_context(|| format!("could not flush {}", temporary.display()))?;
            drop(file);
            replace_file(&temporary, path)
        })();

        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    pub fn default_path() -> PathBuf {
        crate::store::data_dir().join("config.toml")
    }

    pub fn resolve_ui_bind(cli_bind: Option<SocketAddr>, config: &Self) -> Result<SocketAddr> {
        Self::resolve_ui_bind_with_env(cli_bind, std::env::var("TENSORUI_BIND").ok(), config)
    }

    fn resolve_ui_bind_with_env(
        cli_bind: Option<SocketAddr>,
        env_bind: Option<String>,
        config: &Self,
    ) -> Result<SocketAddr> {
        let addr = if let Some(addr) = cli_bind {
            addr
        } else if let Some(raw) = env_bind {
            let trimmed = raw.trim();
            if !trimmed.is_empty() {
                SocketAddr::from_str(trimmed)
                    .with_context(|| format!("invalid TENSORUI_BIND value: {raw}"))?
            } else {
                config.desired_ui_bind()?
            }
        } else {
            config.desired_ui_bind()?
        };
        require_loopback_bind(addr)
    }

    pub fn desired_ui_bind(&self) -> Result<SocketAddr> {
        require_loopback_bind(parse_ui_addr(&self.ui.host, self.ui.port)?)
    }

    pub fn keep_ui_private(&mut self) {
        let host = self.ui.host.trim();
        if host != DEFAULT_UI_HOST && host != "localhost" && host != "::1" {
            self.ui.host = DEFAULT_UI_HOST.into();
        }
    }
}

/// Reject non-loopback binds. tensorUI has no auth and must not be LAN/WAN-exposed.
pub fn require_loopback_bind(addr: SocketAddr) -> Result<SocketAddr> {
    if addr.ip().is_loopback() {
        return Ok(addr);
    }
    eprintln!();
    eprintln!("WARNING: Refusing to start — bind address {addr} is reachable on the network.");
    eprintln!(
        "WARNING: tensorUI is loopback-only. It has no authentication and can read/write local data."
    );
    eprintln!("WARNING: Use 127.0.0.1 or ::1 (config ui.host, --bind, or TENSORUI_BIND).");
    eprintln!();
    bail!("refusing to bind {addr}: not a loopback address")
}

fn replace_file(temporary: &Path, destination: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        match fs::rename(temporary, destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                fs::remove_file(destination)
                    .with_context(|| format!("could not replace {}", destination.display()))?;
                fs::rename(temporary, destination)
                    .with_context(|| format!("could not replace {}", destination.display()))
            }
            Err(error) => {
                Err(error).with_context(|| format!("could not replace {}", destination.display()))
            }
        }
    }
    #[cfg(not(windows))]
    {
        fs::rename(temporary, destination)
            .with_context(|| format!("could not replace {}", destination.display()))
    }
}

fn parse_ui_addr(host: &str, port: u16) -> Result<SocketAddr> {
    if port == 0 {
        bail!("ui port must be between 1 and 65535");
    }
    let host = strip_brackets(host.trim());
    if host.is_empty() {
        bail!("ui host cannot be empty");
    }
    let ip: IpAddr = host.parse().with_context(|| {
        format!("ui host must be an IP address such as 127.0.0.1 or ::1 (got {host})")
    })?;
    Ok(SocketAddr::from((ip, port)))
}

fn strip_brackets(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_loopback_binds() {
        assert!(require_loopback_bind("127.0.0.1:3930".parse().unwrap()).is_ok());
        assert!(require_loopback_bind("[::1]:3930".parse().unwrap()).is_ok());
        assert!(require_loopback_bind("127.0.0.42:1".parse().unwrap()).is_ok());
    }

    #[test]
    fn rejects_network_binds() {
        assert!(require_loopback_bind("0.0.0.0:3930".parse().unwrap()).is_err());
        assert!(require_loopback_bind("[::]:3930".parse().unwrap()).is_err());
        assert!(require_loopback_bind("192.168.1.10:3930".parse().unwrap()).is_err());
        assert!(require_loopback_bind("1.2.3.4:3930".parse().unwrap()).is_err());
    }

    #[test]
    fn resolve_refuses_env_network_bind() {
        let config = Config::default();
        let err = Config::resolve_ui_bind_with_env(None, Some("0.0.0.0:3930".into()), &config)
            .unwrap_err();
        assert!(err.to_string().contains("loopback"));
    }
}
