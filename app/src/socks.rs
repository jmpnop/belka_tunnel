use crate::config::Profile as Config;
use crate::tunnel::{self, ClientHandle};
use anyhow::{anyhow, bail, Context, Result};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

// SOCKS5 reply codes (RFC 1928)
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDR_TYPE_NOT_SUPPORTED: u8 = 0x08;

pub async fn serve(
    config: Arc<Config>,
    ssh: ClientHandle,
    dead: Arc<Notify>,
) -> Result<()> {
    let listen = format!("{}:{}", config.socks.listen_addr, config.socks.listen_port);
    let listener = TcpListener::bind(&listen)
        .await
        .with_context(|| format!("bind SOCKS5 listener at {listen}"))?;
    info!(addr = %listen, "SOCKS5 listening");

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (sock, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        warn!(error = %e, "accept failed");
                        continue;
                    }
                };
                let ssh = ssh.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_conn(sock, ssh).await {
                        debug!(peer = %peer, error = %e, "client connection ended");
                    }
                });
            }
            _ = dead.notified() => {
                info!("SOCKS5 listener shutting down (SSH session ended)");
                return Err(anyhow!("SSH session closed"));
            }
        }
    }
}

async fn handle_conn(mut sock: TcpStream, ssh: ClientHandle) -> Result<()> {
    // --- greeting ---
    let mut buf2 = [0u8; 2];
    sock.read_exact(&mut buf2).await.context("read greeting")?;
    if buf2[0] != 0x05 {
        bail!("not SOCKS5 (version byte {})", buf2[0]);
    }
    let n_methods = buf2[1] as usize;
    let mut methods = vec![0u8; n_methods];
    sock.read_exact(&mut methods).await.context("read methods")?;
    // We support only "no authentication required" (0x00).
    sock.write_all(&[0x05, 0x00]).await.context("write method")?;

    // --- request ---
    let mut req = [0u8; 4];
    sock.read_exact(&mut req).await.context("read request header")?;
    if req[0] != 0x05 {
        bail!("bad request version");
    }
    let cmd = req[1];
    if cmd != 0x01 {
        // only CONNECT
        reply(&mut sock, REP_COMMAND_NOT_SUPPORTED).await?;
        bail!("unsupported command {cmd}");
    }
    let atyp = req[3];
    let (host, port) = match atyp {
        0x01 => {
            let mut a = [0u8; 4];
            sock.read_exact(&mut a).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            (Ipv4Addr::from(a).to_string(), u16::from_be_bytes(p))
        }
        0x03 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l).await?;
            let mut name = vec![0u8; l[0] as usize];
            sock.read_exact(&mut name).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            let host = String::from_utf8(name).context("hostname not utf8")?;
            (host, u16::from_be_bytes(p))
        }
        0x04 => {
            let mut a = [0u8; 16];
            sock.read_exact(&mut a).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            (Ipv6Addr::from(a).to_string(), u16::from_be_bytes(p))
        }
        _ => {
            reply(&mut sock, REP_ADDR_TYPE_NOT_SUPPORTED).await?;
            bail!("unsupported atyp {atyp}");
        }
    };

    debug!(target_host = %host, target_port = port, "SOCKS5 CONNECT");

    // --- open SSH channel ---
    let channel = match tunnel::open_channel(&ssh, &host, port).await {
        Ok(c) => c,
        Err(e) => {
            reply(&mut sock, REP_HOST_UNREACHABLE).await?;
            return Err(anyhow!("open channel: {e}"));
        }
    };

    // --- reply success (we don't know the bound addr; report 0.0.0.0:0) ---
    let bound = SocketAddr::from(([0u8, 0, 0, 0], 0));
    let mut resp = vec![0x05, REP_SUCCESS, 0x00, 0x01];
    match bound.ip() {
        std::net::IpAddr::V4(v4) => resp.extend_from_slice(&v4.octets()),
        std::net::IpAddr::V6(_) => unreachable!(),
    }
    resp.extend_from_slice(&bound.port().to_be_bytes());
    sock.write_all(&resp).await.context("write success reply")?;

    // --- bridge ---
    tunnel::bridge(sock, channel).await
}

async fn reply(sock: &mut TcpStream, code: u8) -> Result<()> {
    let mut resp = vec![0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    if code == 0 {
        resp[1] = REP_GENERAL_FAILURE; // shouldn't happen
    }
    sock.write_all(&resp).await?;
    Ok(())
}
