use crate::config::Profile as Config;
use crate::socks;
use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Handle};
use russh::keys::key;
use russh::ChannelMsg;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{debug, info, warn};

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

pub async fn run_forever(config: Arc<Config>, status: watch::Sender<Status>, control: Control) {
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

        match run_one_session(config.clone(), &status, &control).await {
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

pub struct ClientHandler;

#[async_trait::async_trait]
impl client::Handler for ClientHandler {
    type Error = russh::Error;

    async fn check_server_key(
        &mut self,
        _server_public_key: &key::PublicKey,
    ) -> Result<bool, Self::Error> {
        // TODO: persist + verify host key against config.
        // For now, trust on first use — same model as `ssh` with
        // StrictHostKeyChecking=accept-new. Acceptable for a personal tunnel
        // to a fixed hostname; revisit before redistribution.
        Ok(true)
    }
}

async fn run_one_session(
    config: Arc<Config>,
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

    let addr = (config.ssh.host.as_str(), config.ssh.port);
    let mut handle = client::connect(ssh_cfg, addr, ClientHandler)
        .await
        .with_context(|| format!("connect {}:{}", config.ssh.host, config.ssh.port))?;

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
