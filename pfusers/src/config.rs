//! Persisted app config — SSH endpoint, key path, recorded admin host-key
//! fingerprint, window size.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AppConfig {
    pub ssh: SshConfig,
    #[serde(default)]
    pub window: WindowConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SshConfig {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub key_path: PathBuf,
    /// Same `SHA256:…` form BelkaTunnel uses. None → trust-on-first-use.
    #[serde(default)]
    pub host_key_fingerprint: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WindowConfig {
    pub width: f32,
    pub height: f32,
}

impl Default for WindowConfig {
    fn default() -> Self {
        Self {
            width: 1100.0,
            height: 720.0,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        // Pasha's setup, per CLAUDE.md.
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        Self {
            ssh: SshConfig {
                host: "192.168.1.1".into(),
                port: 22222,
                user: "admin".into(),
                key_path: PathBuf::from(format!("{home}/.ssh/id_ed25519")),
                host_key_fingerprint: None,
            },
            window: WindowConfig::default(),
        }
    }
}

impl AppConfig {
    pub fn default_path() -> Option<PathBuf> {
        ProjectDirs::from("io", "celestialtech", "pfUsers")
            .map(|d| d.config_dir().join("config.json"))
    }

    pub fn load_or_default() -> Result<Self> {
        let Some(path) = Self::default_path() else {
            return Ok(Self::default());
        };
        if !path.exists() {
            let cfg = Self::default();
            cfg.save(&path).ok();
            return Ok(cfg);
        }
        match Self::load(&path) {
            Ok(c) => Ok(c),
            Err(e) => {
                tracing::error!(path = %path.display(), error = %format!("{e:#}"),
                    "config failed to load; using defaults");
                Ok(Self::default())
            }
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        serde_json::from_str::<Self>(&s).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let s = serde_json::to_string_pretty(self)?;
        // Atomic rename so a crash mid-save doesn't truncate the file —
        // pfUsers' own version of the BelkaTunnel pattern.
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let tmp = parent.join(format!(
            ".{}.tmp.{}",
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "config".to_string()),
            std::process::id()
        ));
        std::fs::write(&tmp, s).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, path)
            .with_context(|| format!("renaming {} -> {}", tmp.display(), path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn defaults_match_pashas_setup() {
        let c = AppConfig::default();
        assert_eq!(c.ssh.host, "192.168.1.1");
        assert_eq!(c.ssh.port, 22222);
        assert_eq!(c.ssh.user, "admin");
    }

    #[test]
    fn roundtrip_save_load() {
        let dir = tempdir().unwrap();
        let p = dir.path().join("c.json");
        let mut c = AppConfig::default();
        c.window.width = 999.0;
        c.save(&p).unwrap();
        let back = AppConfig::load(&p).unwrap();
        assert_eq!(back.window.width, 999.0);
    }
}
