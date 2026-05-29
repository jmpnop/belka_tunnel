//! SSH connection layer. Connects to `admin@host:port`, verifies the host
//! key against a stored fingerprint (TOFU on first connect), and exposes a
//! `Handle` that `pfsh::PfSshClient` uses to exec scripts.
//!
//! Same trust-on-first-use semantics as BelkaTunnel's tunnel.rs:
//! `host_key_fingerprint == None` → record the observed fingerprint and
//! accept; `Some(expected)` → require exact match.

use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Handle};
use russh::keys::key;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tracing::{error, info};

pub const HOST_KEY_MISMATCH_PREFIX: &str = "host key mismatch";

/// Same shape as BelkaTunnel's `tunnel::fingerprint_of`. Kept as a free
/// function here so the two apps' SSH layers don't depend on each other.
pub fn fingerprint_of(pk: &key::PublicKey) -> String {
    format!("SHA256:{}", pk.fingerprint())
}

pub struct HostKeyVerifier {
    pub expected: Option<String>,
    pub observed: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl client::Handler for HostKeyVerifier {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let observed = fingerprint_of(server_public_key);
        if let Ok(mut slot) = self.observed.lock() {
            *slot = Some(observed.clone());
        }
        match &self.expected {
            None => {
                info!(fingerprint = %observed, "TOFU accepting admin host key");
                Ok(true)
            }
            Some(expected) if expected == &observed => Ok(true),
            Some(expected) => {
                error!(
                    expected = %expected,
                    observed = %observed,
                    "ADMIN HOST KEY MISMATCH — refusing connect",
                );
                Ok(false)
            }
        }
    }
}

pub type ClientHandle = Arc<Handle<HostKeyVerifier>>;

/// Opens a session, authenticates with the user's private key, returns
/// `(handle, observed_fingerprint)` so the caller can persist the observed
/// fingerprint on a TOFU connection.
pub async fn connect(
    host: &str,
    port: u16,
    user: &str,
    key_path: &Path,
    expected_fingerprint: Option<String>,
) -> Result<(ClientHandle, Option<String>)> {
    let cfg = client::Config {
        inactivity_timeout: Some(Duration::from_secs(60)),
        ..<_>::default()
    };
    let cfg = Arc::new(cfg);

    let observed = Arc::new(Mutex::new(None::<String>));
    let handler = HostKeyVerifier {
        expected: expected_fingerprint.clone(),
        observed: observed.clone(),
    };

    let mut handle = match client::connect(cfg, (host, port), handler).await {
        Ok(h) => h,
        Err(e) => {
            // Disambiguate host-key rejection from generic connect failures
            // the same way BelkaTunnel does.
            let obs = observed.lock().ok().and_then(|s| s.clone());
            if let (Some(expected), Some(obs)) = (&expected_fingerprint, obs) {
                if expected != &obs {
                    bail!(
                        "{HOST_KEY_MISMATCH_PREFIX} — expected {expected}, server presented {obs}"
                    );
                }
            }
            return Err(e).with_context(|| format!("connect {host}:{port}"));
        }
    };

    let key_pair = russh::keys::load_secret_key(key_path, None)
        .with_context(|| format!("loading key {}", key_path.display()))?;

    let authed = handle
        .authenticate_publickey(user.to_string(), Arc::new(key_pair))
        .await
        .context("publickey authentication")?;
    if !authed {
        bail!("publickey authentication rejected by {host}");
    }

    let obs = observed.lock().ok().and_then(|s| s.clone());
    let tofu_value = match (&expected_fingerprint, &obs) {
        (None, Some(_)) => obs,
        _ => None,
    };
    Ok((Arc::new(handle), tofu_value))
}

/// Open a channel, execute `command` non-interactively, return stdout +
/// stderr + exit status. Used by pfsh::PfSshClient to run pfSsh.php
/// invocations.
pub async fn exec_command(handle: &ClientHandle, command: &str) -> Result<ExecResult> {
    use russh::ChannelMsg;
    let mut channel = handle
        .channel_open_session()
        .await
        .map_err(|e| anyhow!("open channel: {e}"))?;
    channel
        .exec(true, command)
        .await
        .map_err(|e| anyhow!("exec request: {e}"))?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut exit_status: Option<u32> = None;
    loop {
        let Some(msg) = channel.wait().await else {
            break;
        };
        match msg {
            ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
            ChannelMsg::ExtendedData { data, ext: 1 } => stderr.extend_from_slice(&data),
            ChannelMsg::ExtendedData { .. } => {}
            ChannelMsg::ExitStatus { exit_status: s } => exit_status = Some(s),
            ChannelMsg::Eof | ChannelMsg::Close => break,
            _ => {}
        }
    }
    Ok(ExecResult {
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
        exit_status,
    })
}

#[derive(Debug, Clone)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    // exit_status currently unused — pfSsh.php's exit code is unreliable
    // (it returns 0 even on PHP parse errors), so we ignore it and rely on
    // the OK/ERR sentinels in stdout/stderr. Kept on the struct so future
    // callers that exec regular shell commands can read it.
    #[allow(dead_code)]
    pub exit_status: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client::Handler;
    use russh::keys::key::KeyPair;

    #[tokio::test]
    async fn host_key_verifier_tofu_accepts_and_records() {
        let kp = KeyPair::generate_ed25519().expect("keygen");
        let pk = kp.clone_public_key().unwrap();
        let observed = Arc::new(Mutex::new(None));
        let mut h = HostKeyVerifier {
            expected: None,
            observed: observed.clone(),
        };
        assert!(h.check_server_key(&pk).await.unwrap());
        assert_eq!(
            observed.lock().unwrap().as_deref(),
            Some(fingerprint_of(&pk).as_str())
        );
    }

    #[tokio::test]
    async fn host_key_verifier_refuses_mismatch() {
        let real = KeyPair::generate_ed25519().expect("keygen");
        let imposter = KeyPair::generate_ed25519().expect("keygen");
        let mut h = HostKeyVerifier {
            expected: Some(fingerprint_of(&real.clone_public_key().unwrap())),
            observed: Arc::new(Mutex::new(None)),
        };
        assert!(!h
            .check_server_key(&imposter.clone_public_key().unwrap())
            .await
            .unwrap());
    }
}
