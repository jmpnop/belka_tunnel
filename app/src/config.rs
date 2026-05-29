use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    pub ssh: SshConfig,
    pub socks: SocksConfig,
    pub reconnect: ReconnectConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: PathBuf,
    #[serde(default)]
    pub key_passphrase: Option<String>,
    pub keepalive_secs: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SocksConfig {
    pub listen_addr: String,
    pub listen_port: u16,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconnectConfig {
    pub initial_backoff_secs: u64,
    pub max_backoff_secs: u64,
    pub backoff_multiplier: f64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConfigFile {
    /// Name of the profile that the tunnel daemon should run as.
    pub active: String,
    pub profiles: BTreeMap<String, Profile>,
    /// When true, the menu bar shows just a dot "·" instead of the colored
    /// status emoji. The menu is still accessible by clicking on it.
    #[serde(default)]
    pub hide_status_dot: bool,
}

// ---- Defaults ----

impl Default for Profile {
    fn default() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        Self {
            ssh: SshConfig {
                host: "aurora.celestialtech.io".into(),
                port: 22222,
                user: "olgatimoshevskaia".into(),
                key_path: PathBuf::from(format!("{home}/.ssh/id_ed25519")),
                key_passphrase: None,
                keepalive_secs: 30,
            },
            socks: SocksConfig {
                // 0.0.0.0 → listen on all IPv4 interfaces so other devices on
                // the LAN can use this Mac as a SOCKS5 proxy. Change to
                // 127.0.0.1 for loopback-only.
                listen_addr: "0.0.0.0".into(),
                listen_port: 1080,
            },
            reconnect: ReconnectConfig {
                initial_backoff_secs: 1,
                max_backoff_secs: 60,
                backoff_multiplier: 2.0,
            },
        }
    }
}

impl Default for ConfigFile {
    fn default() -> Self {
        let mut profiles = BTreeMap::new();
        profiles.insert("default".to_string(), Profile::default());
        Self {
            active: "default".to_string(),
            profiles,
            hide_status_dot: false,
        }
    }
}

// ---- I/O ----

impl ConfigFile {
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("io", "celestialtech", "BelkaTunnel")
            .map(|d| d.config_dir().join("config.json"))
    }

    pub fn load_or_default() -> Result<Self> {
        let path = match Self::default_path() {
            Some(p) => p,
            None => return Ok(Self::default()),
        };
        if path.exists() {
            Self::load(&path)
        } else {
            let cfg = Self::default();
            cfg.save(&path).ok();
            Ok(cfg)
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self = serde_json::from_str(&s)
            .with_context(|| format!("parsing {}", path.display()))?;
        if !cfg.profiles.contains_key(&cfg.active) {
            anyhow::bail!(
                "active profile '{}' does not exist in profiles list",
                cfg.active
            );
        }
        Ok(cfg)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let s = serde_json::to_string_pretty(self)?;
        std::fs::write(path, s).with_context(|| format!("writing {}", path.display()))?;
        Ok(())
    }

    pub fn active_profile(&self) -> Result<&Profile> {
        self.profiles
            .get(&self.active)
            .ok_or_else(|| anyhow::anyhow!("active profile '{}' missing", self.active))
    }
}
