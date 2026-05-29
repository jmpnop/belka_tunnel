use crate::config::Profile as Config;
use crate::socks;
use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Handle};
use russh::keys::key;
use russh::ChannelMsg;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, error, info, warn};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    Disconnected(String),
    Disabled,
}

/// Tunnel on/off + change-notification rolled into one. Backed by a
/// `tokio::sync::watch::channel<bool>` because watch is edge-triggered AND
/// stores the latest value — so a flip from disabled→enabled that races
/// against the reconnect loop's `.changed().await` registration never
/// gets lost (which the previous `Notify` design could swallow).
#[derive(Clone)]
pub struct Control {
    tx: Arc<watch::Sender<bool>>,
    rx: watch::Receiver<bool>,
}

impl Control {
    pub fn new(enabled: bool) -> Self {
        let (tx, rx) = watch::channel(enabled);
        Self {
            tx: Arc::new(tx),
            rx,
        }
    }
    pub fn set_enabled(&self, on: bool) {
        // send_if_modified skips the version bump (and the wakeup of waiting
        // .changed() callers) when the value is unchanged. Matters for the
        // reconnect-backoff path: run_forever races sleep() against
        // ctl_rx.changed(), and a no-op set_enabled(current) would otherwise
        // cut the backoff short for no reason.
        self.tx.send_if_modified(|v| {
            if *v != on {
                *v = on;
                true
            } else {
                false
            }
        });
    }
    pub fn is_enabled(&self) -> bool {
        *self.rx.borrow()
    }
    /// Subscribe to changes. Callers hold the returned receiver across their
    /// loop iterations — that way the "seen version" advances correctly and
    /// no `set_enabled` ever falls between two `.changed().await` calls.
    /// Cloning a `watch::Receiver` resets the seen version to the latest
    /// value, which is exactly the lost-wakeup we want to avoid in hot loops.
    pub fn subscribe(&self) -> watch::Receiver<bool> {
        self.rx.clone()
    }
}

/// Marker prefix used by `run_one_session`'s mismatch error so the menu-bar
/// loop can recognise host-key failures and fire the once-per-session
/// notification without resorting to string-sniffing the wrapped chain.
pub const HOST_KEY_MISMATCH_PREFIX: &str = "host key mismatch";

pub async fn run_forever(
    config: Arc<Config>,
    active_profile_name: String,
    status: watch::Sender<Status>,
    control: Control,
) {
    let mut backoff = Duration::from_secs(config.reconnect.initial_backoff_secs);
    let max_backoff = Duration::from_secs(config.reconnect.max_backoff_secs);
    // Long-lived receiver — its "seen version" advances correctly across the
    // disabled-wait and the reconnect-sleep, so a set_enabled() that races
    // either of those is captured by the next .changed().await.
    let mut ctl_rx = control.subscribe();

    loop {
        if !control.is_enabled() {
            let _ = status.send(Status::Disabled);
            info!("tunnel disabled; waiting for re-enable");
            while !control.is_enabled() {
                let _ = ctl_rx.changed().await;
            }
            backoff = Duration::from_secs(config.reconnect.initial_backoff_secs);
            continue;
        }

        let _ = status.send(Status::Connecting);
        info!(
            host = %config.ssh.host,
            port = config.ssh.port,
            user = %config.ssh.user,
            "establishing SSH session"
        );

        match run_one_session(config.clone(), &active_profile_name, &status, &control).await {
            Ok(()) => {
                info!("SSH session ended cleanly");
                backoff = Duration::from_secs(config.reconnect.initial_backoff_secs);
                // Without this, status stays at Connected for the entire
                // reconnect-backoff sleep below — the menu line would lie
                // for up to `initial_backoff_secs` after every clean end.
                if control.is_enabled() {
                    let _ = status.send(Status::Disconnected("session ended".into()));
                }
            }
            Err(e) => {
                let msg = format!("{e:#}");
                warn!(error = %msg, "SSH session failed");
                if control.is_enabled() {
                    let _ = status.send(Status::Disconnected(msg));
                }
            }
        }

        if !control.is_enabled() {
            // User asked to disconnect — loop top will set Disabled status.
            continue;
        }

        info!(seconds = backoff.as_secs(), "sleeping before reconnect");
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            _ = ctl_rx.changed() => {}
        }
        backoff = std::cmp::min(
            max_backoff,
            Duration::from_secs_f64(backoff.as_secs_f64() * config.reconnect.backoff_multiplier),
        );
    }
}

/// Stable text form of a server host key's SHA-256 fingerprint, suitable for
/// persistence in config.json and for visual comparison with `ssh-keyscan`
/// output: `"SHA256:" + base64-no-padding(sha256(key))`.
pub fn fingerprint_of(pk: &key::PublicKey) -> String {
    // russh's PublicKey::fingerprint() already returns base64-no-padding of
    // the SHA-256 hash — we just brand it with the algorithm prefix so it
    // round-trips identically to OpenSSH's `ssh-keygen -lf` output.
    format!("SHA256:{}", pk.fingerprint())
}

pub struct ClientHandler {
    /// Fingerprint we expect the server to present, in `SHA256:…` form.
    /// `None` puts the connection in trust-on-first-use mode: we accept
    /// whatever the server offers and record what we saw via
    /// `observed_fingerprint`. `Some(_)` enforces equality and refuses
    /// the connection (returns Ok(false), which russh turns into a
    /// session-level error) on mismatch.
    expected_fingerprint: Option<String>,
    /// Filled in by check_server_key with whatever the server actually
    /// presented. Sized for cheap polling from the calling function after
    /// `client::connect` returns, so the TOFU writer can persist the
    /// observed value.
    observed_fingerprint: Arc<Mutex<Option<String>>>,
}

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        let observed = fingerprint_of(server_public_key);
        // Always record what we saw, even on rejection — useful for the
        // mismatch log and for the user to compare against ssh-keyscan.
        if let Ok(mut slot) = self.observed_fingerprint.lock() {
            *slot = Some(observed.clone());
        }
        match &self.expected_fingerprint {
            None => {
                // Trust-on-first-use: caller will read `observed_fingerprint`
                // after auth succeeds and persist it to config.json.
                info!(
                    fingerprint = %observed,
                    "trust-on-first-use accepting host key"
                );
                Ok(true)
            }
            Some(expected) if expected == &observed => {
                debug!(fingerprint = %observed, "host key matches recorded fingerprint");
                Ok(true)
            }
            Some(expected) => {
                error!(
                    expected = %expected,
                    observed = %observed,
                    "HOST KEY MISMATCH — refusing connection (possible MITM)"
                );
                Ok(false)
            }
        }
    }
}

async fn run_one_session(
    config: Arc<Config>,
    active_profile_name: &str,
    status: &watch::Sender<Status>,
    control: &Control,
) -> Result<()> {
    let ssh_cfg = client::Config {
        inactivity_timeout: Some(Duration::from_secs(config.ssh.keepalive_secs * 3)),
        keepalive_interval: Some(Duration::from_secs(config.ssh.keepalive_secs)),
        keepalive_max: 3,
        ..<_>::default()
    };
    let ssh_cfg = Arc::new(ssh_cfg);

    // Shared slot for the observed fingerprint — filled in by
    // check_server_key, read by the TOFU writer below after auth succeeds.
    let observed_fp: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let handler = ClientHandler {
        expected_fingerprint: config.ssh.host_key_fingerprint.clone(),
        observed_fingerprint: observed_fp.clone(),
    };

    let addr = (config.ssh.host.as_str(), config.ssh.port);
    let mut handle = match client::connect(ssh_cfg, addr, handler).await {
        Ok(h) => h,
        Err(e) => {
            // russh returns the same `russh::Error::Disconnect` for a
            // server-side disconnect AND for our `check_server_key` returning
            // Ok(false). Disambiguate by comparing the observed fingerprint
            // (which we wrote unconditionally) against what we expected; if
            // they differ, this was a host-key rejection by us, and we want
            // the caller to surface that to the user.
            let observed = observed_fp.lock().ok().and_then(|s| s.clone());
            if let (Some(expected), Some(observed)) = (&config.ssh.host_key_fingerprint, observed) {
                if expected != &observed {
                    bail!(
                        "{HOST_KEY_MISMATCH_PREFIX} — expected {expected}, server presented {observed}"
                    );
                }
            }
            return Err(e)
                .with_context(|| format!("connect {}:{}", config.ssh.host, config.ssh.port));
        }
    };

    let passphrase = config
        .ssh
        .key_passphrase
        .as_deref()
        .filter(|s| !s.is_empty());
    let key_pair = russh::keys::load_secret_key(&config.ssh.key_path, passphrase)
        .with_context(|| format!("loading key {}", config.ssh.key_path.display()))?;

    let authed = handle
        .authenticate_publickey(config.ssh.user.clone(), Arc::new(key_pair))
        .await
        .context("publickey authentication")?;

    if !authed {
        bail!("publickey authentication rejected");
    }

    // TOFU writeback: if config didn't have a fingerprint yet, persist what
    // we just saw. The file-system watcher will pick this up and self-restart
    // the daemon; the next iteration starts in fingerprint-locked mode.
    if config.ssh.host_key_fingerprint.is_none() {
        if let Some(fp) = observed_fp.lock().ok().and_then(|s| s.clone()) {
            if let Err(e) = persist_tofu_fingerprint(active_profile_name, &fp) {
                warn!(error = %e, "could not persist TOFU fingerprint (will retry next connect)");
            }
        }
    }

    let handle = Arc::new(handle);
    info!("SSH authenticated, starting SOCKS5 listener");
    let _ = status.send(Status::Connected);

    let dead = Arc::new(tokio::sync::Notify::new());
    let watchdog = {
        let handle = handle.clone();
        let dead = dead.clone();
        let control = control.clone();
        let mut ctl_rx = control.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {
                        if handle.is_closed() {
                            warn!("SSH session detected closed; signalling SOCKS to stop");
                            dead.notify_waiters();
                            break;
                        }
                        if !control.is_enabled() {
                            info!("tunnel disabled by user; signalling SOCKS to stop");
                            dead.notify_waiters();
                            break;
                        }
                    }
                    _ = ctl_rx.changed() => {
                        if !control.is_enabled() {
                            info!("tunnel disabled by user; signalling SOCKS to stop");
                            dead.notify_waiters();
                            break;
                        }
                    }
                }
            }
        })
    };

    let result = socks::serve(config.clone(), handle.clone(), dead).await;
    watchdog.abort();
    if !handle.is_closed() {
        let _ = handle
            .clone()
            .disconnect(russh::Disconnect::ByApplication, "reconnect", "en")
            .await;
    }
    result
}

/// Write a freshly-observed host-key fingerprint into the persisted
/// config.json for the given profile. Called once after the first successful
/// connection on a TOFU-mode profile; the file-system watcher then picks
/// the change up and self-restarts the daemon into fingerprint-locked mode.
///
/// Subtlety: we load the file fresh from disk (rather than mutating our
/// in-memory `Config` which is just one profile out of the whole file) so
/// that any concurrent edits the user has made to OTHER profiles aren't
/// stomped by our targeted save.
fn persist_tofu_fingerprint(profile_name: &str, fingerprint: &str) -> Result<()> {
    use crate::config::ConfigFile;
    let path = ConfigFile::default_path().ok_or_else(|| anyhow!("no config path available"))?;
    let mut file = ConfigFile::load(&path)
        .with_context(|| format!("re-reading {} for TOFU writeback", path.display()))?;
    let prof = file
        .profiles
        .get_mut(profile_name)
        .ok_or_else(|| anyhow!("profile {profile_name} vanished from config"))?;
    if prof.ssh.host_key_fingerprint.is_some() {
        // Someone else (the user, presumably) wrote a fingerprint between our
        // load and our save. Don't stomp their value.
        return Ok(());
    }
    prof.ssh.host_key_fingerprint = Some(fingerprint.to_string());
    file.save(&path)?;
    info!(
        path = %path.display(),
        profile = %profile_name,
        fingerprint = %fingerprint,
        "recorded TOFU host-key fingerprint",
    );
    Ok(())
}

/// Open a direct-tcpip channel through the SSH session.
pub async fn open_channel(
    handle: &Arc<Handle<ClientHandler>>,
    target_host: &str,
    target_port: u16,
) -> Result<russh::Channel<russh::client::Msg>> {
    let channel = handle
        .channel_open_direct_tcpip(
            target_host.to_string(),
            target_port as u32,
            "127.0.0.1".to_string(),
            0u32,
        )
        .await
        .map_err(|e| anyhow!("direct-tcpip open: {e}"))?;
    Ok(channel)
}

/// Bridge a TCP stream with an SSH `direct-tcpip` channel **with proper
/// half-close**. Either side can end its half independently; we keep
/// pumping the other direction until it also finishes, instead of dropping
/// in-flight bytes the moment the first end-of-stream arrives.
///
/// The 64 KiB buffer is a known sweet spot for tunneled-TCP throughput.
pub async fn bridge<S>(mut tcp: S, mut channel: russh::Channel<russh::client::Msg>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 64 * 1024];
    // tcp_done = no more bytes will move TCP→SSH ; chan_done = no more bytes will move SSH→TCP.
    let mut tcp_done = false;
    let mut chan_done = false;

    while !(tcp_done && chan_done) {
        tokio::select! {
            // TCP → SSH channel — only polled while TCP read half is alive.
            r = tcp.read(&mut buf), if !tcp_done => {
                match r {
                    Ok(0) => {
                        // Client closed its write half. Signal EOF to the SSH
                        // channel and stop reading TCP, but KEEP draining the
                        // channel — the server may still send a final HTTP
                        // response chunk after seeing the client's FIN.
                        let _ = channel.eof().await;
                        tcp_done = true;
                    }
                    Ok(n) => {
                        if let Err(e) = channel.data(&buf[..n]).await {
                            warn!(error = %e, "SSH channel write failed; tearing bridge down");
                            tcp_done = true;
                            chan_done = true;
                        }
                    }
                    Err(e) => {
                        debug!(error = %e, "TCP read error");
                        tcp_done = true;
                        let _ = channel.eof().await;
                    }
                }
            }
            // SSH channel → TCP — only polled while SSH side is alive.
            msg = channel.wait(), if !chan_done => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if let Err(e) = tcp.write_all(&data).await {
                            debug!(error = %e, "TCP write failed; tearing bridge down");
                            chan_done = true;
                            tcp_done = true;
                        }
                    }
                    Some(ChannelMsg::ExtendedData { .. }) => {
                        // ignore stderr-equivalent
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => {
                        // Server signalled end. Half-close the TCP write side
                        // but KEEP reading TCP — client may still send a final
                        // request body or RST that we want to observe.
                        let _ = tcp.shutdown().await;
                        chan_done = true;
                    }
                    Some(_) => {}
                }
            }
        }
    }

    let _ = tcp.shutdown().await;
    Ok(())
}

pub type ClientHandle = Arc<Handle<ClientHandler>>;

#[cfg(test)]
mod tests {
    use super::*;
    use russh::client::Handler;
    use russh::keys::key::KeyPair;

    /// Round-trip: load a fresh ed25519 keypair, ask russh-keys for its
    /// fingerprint, and assert our helper prefixes it as "SHA256:".
    /// This pins the wire format we persist to disk so it stays compatible
    /// with `ssh-keygen -l -E sha256 -f known_hosts` output.
    #[test]
    fn fingerprint_of_is_sha256_prefixed_and_stable() {
        let kp = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let pk = kp.clone_public_key().unwrap();
        let fp = fingerprint_of(&pk);
        assert!(fp.starts_with("SHA256:"), "missing prefix: {fp}");
        // Base64 body (after the prefix) is sha256(32 bytes) = 43 chars in
        // base64-no-padding.
        let body = &fp["SHA256:".len()..];
        assert_eq!(body.len(), 43, "unexpected fingerprint length: {fp}");
        // Calling twice on the same key must produce the same string.
        assert_eq!(fp, fingerprint_of(&pk), "fingerprint not deterministic");
    }

    /// Two distinct keypairs must hash to different fingerprints. (Trivial
    /// for SHA-256, but the test guards against a future refactor that
    /// accidentally hashes a constant or mis-encodes the key bytes.)
    #[test]
    fn fingerprint_of_differs_between_keys() {
        let a = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let b = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let fa = fingerprint_of(&a.clone_public_key().unwrap());
        let fb = fingerprint_of(&b.clone_public_key().unwrap());
        assert_ne!(fa, fb, "two random keys produced the same fingerprint");
    }

    /// TOFU mode: with no expected fingerprint, accept whatever the server
    /// presents and store the observed value where the caller can pick it up.
    #[tokio::test]
    async fn check_server_key_tofu_accepts_and_records() {
        let kp = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let pk = kp.clone_public_key().unwrap();
        let observed = Arc::new(Mutex::new(None));
        let mut h = ClientHandler {
            expected_fingerprint: None,
            observed_fingerprint: observed.clone(),
        };
        let ok = h.check_server_key(&pk).await.unwrap();
        assert!(ok, "TOFU must accept");
        let recorded = observed.lock().unwrap().clone();
        assert_eq!(recorded.as_deref(), Some(fingerprint_of(&pk).as_str()));
    }

    /// Locked mode matching: same key + same expected → accept.
    #[tokio::test]
    async fn check_server_key_locked_accepts_match() {
        let kp = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let pk = kp.clone_public_key().unwrap();
        let fp = fingerprint_of(&pk);
        let observed = Arc::new(Mutex::new(None));
        let mut h = ClientHandler {
            expected_fingerprint: Some(fp.clone()),
            observed_fingerprint: observed.clone(),
        };
        assert!(h.check_server_key(&pk).await.unwrap());
        // Even on accept we record the observed value for parity.
        assert_eq!(observed.lock().unwrap().as_deref(), Some(fp.as_str()));
    }

    /// Locked mode mismatch: different key than expected → refuse
    /// (Ok(false), which russh converts to a session error). The observed
    /// value MUST still be recorded so the caller can surface it.
    #[tokio::test]
    async fn check_server_key_locked_refuses_mismatch() {
        let real_kp = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let imposter_kp = KeyPair::generate_ed25519().expect("ed25519 keygen");
        let real_pk = real_kp.clone_public_key().unwrap();
        let imposter_pk = imposter_kp.clone_public_key().unwrap();
        let observed = Arc::new(Mutex::new(None));
        let mut h = ClientHandler {
            expected_fingerprint: Some(fingerprint_of(&real_pk)),
            observed_fingerprint: observed.clone(),
        };
        let ok = h.check_server_key(&imposter_pk).await.unwrap();
        assert!(!ok, "mismatch must refuse");
        // Caller depends on the observed value being recorded even on refuse —
        // otherwise the surfaced error message couldn't include 'server
        // presented X' and the user can't compare to ssh-keyscan.
        assert_eq!(
            observed.lock().unwrap().as_deref(),
            Some(fingerprint_of(&imposter_pk).as_str())
        );
    }

    /// End-to-end persistence: write a fresh config, simulate the TOFU
    /// writeback function picking up a new fingerprint, verify the file
    /// on disk now carries it.
    /// We can't easily exercise `persist_tofu_fingerprint` because it
    /// reads `ConfigFile::default_path()` (which is OS-wide); instead we
    /// reproduce its body inline against a tempdir path and assert the
    /// invariants that matter: (a) only the targeted profile's
    /// fingerprint changes, (b) other profiles' fields stay intact,
    /// (c) the on-disk file remains valid JSON parseable by `load`.
    #[test]
    fn tofu_writeback_only_modifies_target_profile_fingerprint() {
        use crate::config::{ConfigFile, Profile};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.json");
        let mut file = ConfigFile::default();
        // Two profiles so we can prove the writeback is targeted.
        file.profiles
            .insert("other".to_string(), Profile::default());
        // Customise "other" so we can detect if it got stomped.
        file.profiles.get_mut("other").unwrap().ssh.user = "marker".to_string();
        file.save(&path).unwrap();
        // Now simulate persist: load, mutate active profile only, save.
        let mut reloaded = ConfigFile::load(&path).unwrap();
        let prof = reloaded.profiles.get_mut("default").unwrap();
        assert!(prof.ssh.host_key_fingerprint.is_none(), "preconditions");
        prof.ssh.host_key_fingerprint = Some("SHA256:test-fingerprint".to_string());
        reloaded.save(&path).unwrap();
        // Verify on-disk file.
        let final_file = ConfigFile::load(&path).unwrap();
        assert_eq!(
            final_file.profiles["default"]
                .ssh
                .host_key_fingerprint
                .as_deref(),
            Some("SHA256:test-fingerprint")
        );
        // Other profile unchanged.
        assert!(
            final_file.profiles["other"]
                .ssh
                .host_key_fingerprint
                .is_none(),
            "non-target profile's fingerprint should not have been touched"
        );
        assert_eq!(
            final_file.profiles["other"].ssh.user, "marker",
            "non-target profile's fields should be preserved"
        );
    }

    #[test]
    fn control_initial_value() {
        assert!(Control::new(true).is_enabled());
        assert!(!Control::new(false).is_enabled());
    }

    #[tokio::test]
    async fn control_subscribe_observes_change() {
        let ctl = Control::new(false);
        let mut rx = ctl.subscribe();
        let waiter = tokio::spawn(async move {
            rx.changed().await.unwrap();
            *rx.borrow()
        });
        // Let waiter register.
        tokio::time::sleep(Duration::from_millis(20)).await;
        ctl.set_enabled(true);
        let v = tokio::time::timeout(Duration::from_secs(1), waiter)
            .await
            .expect("waiter timed out")
            .expect("waiter panicked");
        assert!(v);
        assert!(ctl.is_enabled());
    }

    #[tokio::test]
    async fn control_no_op_set_does_not_wake_subscribers() {
        // set_enabled(current) must NOT bump the watch version — otherwise
        // run_forever's tokio::select!(sleep, changed) would short-circuit
        // the backoff on a no-op menu flip.
        let ctl = Control::new(true);
        let mut rx = ctl.subscribe();
        ctl.set_enabled(true); // no-op
        ctl.set_enabled(true); // still no-op
        let r = tokio::time::timeout(Duration::from_millis(150), rx.changed()).await;
        assert!(r.is_err(), "no-op set_enabled spuriously woke a subscriber");
    }

    #[tokio::test]
    async fn control_no_lost_wakeup_on_rapid_flip() {
        // The bug we're guarding against: a subscriber that polled is_enabled()
        // and was about to await .changed() must still observe a flip that
        // happened in between. With a long-lived watch::Receiver from
        // subscribe(), the next .changed().await captures any subsequent change.
        let ctl = Control::new(true);
        let mut rx = ctl.subscribe();
        // Pretend we polled is_enabled() here.
        assert!(ctl.is_enabled());
        // Now flip BEFORE the subscriber awaits — historically a lost wakeup.
        ctl.set_enabled(false);
        // The receiver should observe the flip on its next call.
        let _ = tokio::time::timeout(Duration::from_secs(1), rx.changed())
            .await
            .expect("changed() lost the wakeup");
        assert!(!*rx.borrow());
    }
}
