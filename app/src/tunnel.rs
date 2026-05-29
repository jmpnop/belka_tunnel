use crate::config::Profile as Config;
use crate::socks;
use anyhow::{anyhow, bail, Context, Result};
use russh::client::{self, Handle};
use russh::keys::key;
use russh::ChannelMsg;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;
use tracing::{error, info, warn};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Connecting,
    Connected,
    Disconnected(String),
    Disabled,
}

pub struct Tunnel {
    pub status_rx: watch::Receiver<Status>,
}

#[derive(Clone)]
pub struct Control {
    pub enabled: Arc<std::sync::atomic::AtomicBool>,
    pub wakeup: Arc<tokio::sync::Notify>,
}

impl Control {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled: Arc::new(std::sync::atomic::AtomicBool::new(enabled)),
            wakeup: Arc::new(tokio::sync::Notify::new()),
        }
    }
    pub fn set_enabled(&self, on: bool) {
        self.enabled
            .store(on, std::sync::atomic::Ordering::Relaxed);
        self.wakeup.notify_waiters();
    }
    pub fn is_enabled(&self) -> bool {
        self.enabled.load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[allow(dead_code)]
pub fn spawn(config: Arc<Config>) -> Tunnel {
    let (tx, rx) = watch::channel(Status::Connecting);
    tokio::spawn(run_forever(config, tx, Control::new(true)));
    Tunnel { status_rx: rx }
}

pub async fn run_forever(
    config: Arc<Config>,
    status: watch::Sender<Status>,
    control: Control,
) {
    let mut backoff = Duration::from_secs(config.reconnect.initial_backoff_secs);
    let max_backoff = Duration::from_secs(config.reconnect.max_backoff_secs);

    loop {
        // Wait while disabled.
        if !control.is_enabled() {
            let _ = status.send(Status::Disabled);
            info!("tunnel disabled; waiting for wakeup");
            control.wakeup.notified().await;
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
            _ = control.wakeup.notified() => {}
        }
        backoff = std::cmp::min(
            max_backoff,
            Duration::from_secs_f64(
                backoff.as_secs_f64() * config.reconnect.backoff_multiplier,
            ),
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
                    _ = control.wakeup.notified() => {
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

/// Bridge a TCP stream with an SSH channel until either side closes.
pub async fn bridge<S>(mut tcp: S, mut channel: russh::Channel<russh::client::Msg>) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 32 * 1024];
    loop {
        tokio::select! {
            // TCP -> SSH channel
            r = tcp.read(&mut buf) => {
                match r {
                    Ok(0) => {
                        // EOF on TCP side; signal EOF on channel
                        let _ = channel.eof().await;
                        break;
                    }
                    Ok(n) => {
                        if let Err(e) = channel.data(&buf[..n]).await {
                            return Err(anyhow!("channel data: {e}"));
                        }
                    }
                    Err(e) => return Err(anyhow!("tcp read: {e}")),
                }
            }
            // SSH channel -> TCP
            msg = channel.wait() => {
                match msg {
                    Some(ChannelMsg::Data { data }) => {
                        if let Err(e) = tcp.write_all(&data).await {
                            return Err(anyhow!("tcp write: {e}"));
                        }
                    }
                    Some(ChannelMsg::ExtendedData { .. }) => {
                        // ignore stderr-equivalent
                    }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) => break,
                    Some(_) => {}
                    None => break,
                }
            }
        }
    }
    let _ = tcp.shutdown().await;
    Ok(())
}

pub type ClientHandle = Arc<Handle<ClientHandler>>;

impl Drop for ClientHandler {
    fn drop(&mut self) {
        // nothing
    }
}

#[allow(dead_code)]
async fn shutdown(handle: Handle<ClientHandler>) {
    let _ = handle
        .disconnect(russh::Disconnect::ByApplication, "shutdown", "en")
        .await
        .map_err(|e| error!(error = %e, "disconnect"));
}
