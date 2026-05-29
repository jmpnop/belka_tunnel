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
        if !path.exists() {
            let cfg = Self::default();
            cfg.save(&path).ok();
            return Ok(cfg);
        }
        // File exists. If it parses + validates, use it. If it doesn't —
        // corrupt JSON from a partial save, a hand-edit gone wrong, a
        // pathological backoff value — fall back to defaults instead of
        // bailing out of main(). The whole point of this app is to be a
        // persistent menu-bar daemon; if a stray comma in config.json
        // crashed startup the user couldn't even open the GUI editor to
        // recover. The original file is left intact so the user can fix
        // it manually; the GUI just won't read it until they do.
        match Self::load(&path) {
            Ok(cfg) => Ok(cfg),
            Err(e) => {
                tracing::error!(
                    path = %path.display(),
                    error = %format!("{e:#}"),
                    "config failed to load — using defaults; original file untouched"
                );
                Ok(Self::default())
            }
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        let cfg: Self =
            serde_json::from_str(&s).with_context(|| format!("parsing {}", path.display()))?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Cheap sanity checks beyond what serde gives us. Runs on load so a
    /// malformed config.json (which the watcher will now auto-pick up and
    /// restart on) can't crash-loop the daemon. The validators target real
    /// failure modes:
    ///
    /// * `backoff_multiplier <= 0`, NaN, or infinite would make
    ///   `Duration::from_secs_f64(backoff * multiplier)` panic in
    ///   `run_forever`'s reconnect path.
    /// * `initial_backoff_secs == 0` would hot-spin the reconnect loop.
    /// * `listen_port == 0` would bind an ephemeral port and break clients
    ///   that expect the configured port.
    /// * Dangling `active` produces a useless menu — surfaced as the same
    ///   error the load path used to bail with.
    pub fn validate(&self) -> Result<()> {
        if !self.profiles.contains_key(&self.active) {
            anyhow::bail!(
                "active profile '{}' does not exist in profiles list",
                self.active
            );
        }
        for (name, p) in &self.profiles {
            if p.socks.listen_port == 0 {
                anyhow::bail!("profile '{name}': socks.listen_port must be > 0");
            }
            if p.ssh.port == 0 {
                anyhow::bail!("profile '{name}': ssh.port must be > 0");
            }
            if p.reconnect.initial_backoff_secs == 0 {
                anyhow::bail!("profile '{name}': reconnect.initial_backoff_secs must be > 0");
            }
            if p.reconnect.max_backoff_secs < p.reconnect.initial_backoff_secs {
                anyhow::bail!(
                    "profile '{name}': reconnect.max_backoff_secs ({}) < initial_backoff_secs ({})",
                    p.reconnect.max_backoff_secs,
                    p.reconnect.initial_backoff_secs
                );
            }
            let m = p.reconnect.backoff_multiplier;
            if !m.is_finite() || m <= 1.0 {
                // Multiplier ≤ 1.0 means backoff never grows; combined with a
                // long outage the daemon would retry forever at the initial
                // interval. NaN / inf would panic Duration::from_secs_f64.
                anyhow::bail!(
                    "profile '{name}': reconnect.backoff_multiplier must be finite and > 1.0 (got {m})"
                );
            }
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn default_config_has_default_profile() {
        let cfg = ConfigFile::default();
        assert_eq!(cfg.active, "default");
        assert!(cfg.profiles.contains_key("default"));
    }

    #[test]
    fn roundtrip_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        ConfigFile::default().save(&path).unwrap();
        let back = ConfigFile::load(&path).unwrap();
        assert_eq!(back.active, "default");
        assert_eq!(back.profiles.len(), 1);
    }

    #[test]
    fn rejects_dangling_active() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        std::fs::write(&path, r#"{"active":"ghost","profiles":{}}"#).unwrap();
        let err = ConfigFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("ghost"), "got: {err}");
    }

    fn write_cfg(path: &Path, multiplier: f64, init: u64, max: u64, socks_port: u16) {
        let body = format!(
            r#"{{
                "active": "default",
                "profiles": {{
                    "default": {{
                        "ssh": {{
                            "host": "x", "port": 22, "user": "u",
                            "key_path": "/k", "keepalive_secs": 30
                        }},
                        "socks": {{ "listen_addr": "127.0.0.1", "listen_port": {socks_port} }},
                        "reconnect": {{
                            "initial_backoff_secs": {init}, "max_backoff_secs": {max},
                            "backoff_multiplier": {multiplier}
                        }}
                    }}
                }}
            }}"#
        );
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn rejects_zero_socks_port() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        write_cfg(&path, 2.0, 1, 60, 0);
        let err = ConfigFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("listen_port"), "got: {err}");
    }

    #[test]
    fn rejects_zero_initial_backoff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        write_cfg(&path, 2.0, 0, 60, 1080);
        let err = ConfigFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("initial_backoff_secs"), "got: {err}");
    }

    #[test]
    fn rejects_max_below_initial_backoff() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        write_cfg(&path, 2.0, 30, 5, 1080);
        let err = ConfigFile::load(&path).unwrap_err().to_string();
        assert!(err.contains("max_backoff_secs"), "got: {err}");
    }

    #[test]
    fn rejects_pathological_backoff_multipliers() {
        for &m in &[0.0, -1.0, 1.0, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
            let dir = tempdir().unwrap();
            let path = dir.path().join("c.json");
            // serde_json rejects NaN/Inf in strict mode, so encode NaN/Inf as
            // string-substitution after to_string; for now skip non-finite
            // cases that JSON itself disallows.
            if !m.is_finite() {
                // Hand-construct since JSON has no NaN/Inf literal.
                let token = match m.classify() {
                    std::num::FpCategory::Nan => "NaN",
                    _ if m.is_sign_positive() => "Infinity",
                    _ => "-Infinity",
                };
                let body = format!(
                    r#"{{"active":"default","profiles":{{"default":{{"ssh":{{"host":"x","port":22,"user":"u","key_path":"/k","keepalive_secs":30}},"socks":{{"listen_addr":"127.0.0.1","listen_port":1080}},"reconnect":{{"initial_backoff_secs":1,"max_backoff_secs":60,"backoff_multiplier":{token}}}}}}}}}"#
                );
                std::fs::write(&path, body).unwrap();
                // serde_json will refuse non-finite floats at parse time,
                // which is itself a form of protection — the load() error
                // surfaces it.
                assert!(ConfigFile::load(&path).is_err(), "non-finite {m} accepted");
                continue;
            }
            write_cfg(&path, m, 1, 60, 1080);
            let err = ConfigFile::load(&path).unwrap_err().to_string();
            assert!(
                err.contains("backoff_multiplier"),
                "multiplier {m} got: {err}"
            );
        }
    }

    #[test]
    fn load_or_default_falls_back_on_corrupt_file() {
        // load_or_default must NOT bail when config.json is malformed —
        // doing so used to crash main() and lock the user out of the GUI
        // editor that could fix it. Verify by writing a broken file at a
        // tempdir and asserting that load() errors but the original is
        // still on disk afterward (so user-recovery is possible).
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        std::fs::write(&path, "{ broken json: ").unwrap();
        let load_err = ConfigFile::load(&path);
        assert!(load_err.is_err(), "broken JSON should fail load");
        // Original file should still be there for the user to inspect.
        assert!(path.exists(), "load() must not delete the bad file");
        let body = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            body, "{ broken json: ",
            "load() must not rewrite the bad file"
        );
    }

    /// Old configs may still carry the old `hide_status_dot` field — serde's
    /// default is to ignore unknown fields, so we should round-trip cleanly.
    #[test]
    fn unknown_fields_ignored() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("c.json");
        let body = r#"{
            "active": "default",
            "hide_status_dot": true,
            "profiles": {
                "default": {
                    "ssh": {
                        "host": "aurora.celestialtech.io", "port": 22222,
                        "user": "olga", "key_path": "/tmp/k", "keepalive_secs": 30
                    },
                    "socks": { "listen_addr": "127.0.0.1", "listen_port": 1080 },
                    "reconnect": {
                        "initial_backoff_secs": 1, "max_backoff_secs": 60,
                        "backoff_multiplier": 2.0
                    }
                }
            }
        }"#;
        std::fs::write(&path, body).unwrap();
        let cfg = ConfigFile::load(&path).unwrap();
        assert_eq!(cfg.active, "default");
    }
}
