use crate::config::Profile as Config;
use crate::tunnel::{self, ClientHandle};
use anyhow::{anyhow, bail, Context, Result};
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Cap on time the client has to complete the SOCKS5 handshake (greeting +
/// request) before we close the connection. Protects against slow / dead
/// clients pinning a task and an FD forever.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

// SOCKS5 reply codes (RFC 1928)
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_COMMAND_NOT_SUPPORTED: u8 = 0x07;
const REP_ADDR_TYPE_NOT_SUPPORTED: u8 = 0x08;

pub async fn serve(config: Arc<Config>, ssh: ClientHandle, dead: Arc<Notify>) -> Result<()> {
    let listen = format!("{}:{}", config.socks.listen_addr, config.socks.listen_port);
    let listener = match TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            // Isolate bind failures symmetrically with the HTTP proxy: don't
            // propagate (that would end the session and tear down a working
            // HTTP proxy, then reconnect-loop rebuilding SSH just to re-hit the
            // same bind error). Notify + park until the session dies instead.
            warn!(addr = %listen, error = %e, "SOCKS5 bind failed; HTTP proxy unaffected");
            crate::notify_user(
                "БелкаТуннель — SOCKS5 proxy unavailable",
                &format!("Couldn't listen on {listen}: {e}. Change the SOCKS port in Edit Configuration."),
            );
            dead.notified().await;
            return Ok(());
        }
    };
    info!(addr = %listen, "SOCKS5 listening");

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (sock, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        // EMFILE/ENFILE etc. would otherwise hot-loop and burn
                        // CPU; back off briefly before retrying.
                        warn!(error = %e, "accept failed; backing off 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                // SOCKS handshake is 3 small writes; Nagle would add ~40ms.
                let _ = sock.set_nodelay(true);
                let ssh = ssh.clone();
                let dead_for_conn = dead.clone();
                tokio::spawn(async move {
                    // Race the connection against the session-dead signal so
                    // in-flight bridges don't hold sockets after the SSH side
                    // goes away.
                    tokio::select! {
                        r = handle_conn(sock, ssh) => {
                            if let Err(e) = r {
                                debug!(peer = %peer, error = %e, "client connection ended");
                            }
                        }
                        _ = dead_for_conn.notified() => {
                            debug!(peer = %peer, "session ended while client active");
                        }
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
    // Whole SOCKS5 handshake (greeting + method negotiation + CONNECT request)
    // is bounded — anyone slower than HANDSHAKE_TIMEOUT gets the FIN.
    let (host, port) = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_socks5_request(&mut sock))
        .await
        .context("SOCKS5 handshake timeout")??;

    debug!(target_host = %host, target_port = port, "SOCKS5 CONNECT");

    // --- open SSH channel ---
    let channel = match tunnel::open_channel(&ssh, &host, port).await {
        Ok(c) => c,
        Err(e) => {
            reply(&mut sock, REP_HOST_UNREACHABLE).await?;
            return Err(anyhow!("open channel: {e}"));
        }
    };

    // --- reply success — we don't know the SSH-side bound addr, so report
    //     0.0.0.0:0. Stack-allocated (no heap touch per accept).
    let resp: [u8; 10] = [0x05, REP_SUCCESS, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    sock.write_all(&resp).await.context("write success reply")?;

    // --- bridge ---
    tunnel::bridge(sock, channel).await
}

/// Parses the SOCKS5 greeting + CONNECT request bytes from `sock` and returns
/// the requested `(host, port)`. Writes the method-selection reply (and any
/// error reply) inline. Generic over `AsyncRead + AsyncWrite` so unit tests
/// can drive it with `tokio::io::duplex` without standing up a real socket.
async fn read_socks5_request<S>(sock: &mut S) -> Result<(String, u16)>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    // --- greeting ---
    let mut buf2 = [0u8; 2];
    sock.read_exact(&mut buf2).await.context("read greeting")?;
    if buf2[0] != 0x05 {
        bail!("not SOCKS5 (version byte {})", buf2[0]);
    }
    let n_methods = buf2[1] as usize;
    // Method count is bounded ≤ 255; stack-allocate.
    let mut methods = [0u8; 255];
    sock.read_exact(&mut methods[..n_methods])
        .await
        .context("read methods")?;
    if !methods[..n_methods].contains(&0x00) {
        let _ = sock.write_all(&[0x05, 0xFF]).await;
        bail!("client offered no acceptable auth method");
    }
    sock.write_all(&[0x05, 0x00])
        .await
        .context("write method")?;

    // --- request header ---
    let mut req = [0u8; 4];
    sock.read_exact(&mut req)
        .await
        .context("read request header")?;
    if req[0] != 0x05 {
        bail!("bad request version");
    }
    let cmd = req[1];
    if cmd != 0x01 {
        reply(sock, REP_COMMAND_NOT_SUPPORTED).await?;
        bail!("unsupported command {cmd}");
    }
    let atyp = req[3];
    match atyp {
        0x01 => {
            let mut a = [0u8; 4];
            sock.read_exact(&mut a).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            Ok((Ipv4Addr::from(a).to_string(), u16::from_be_bytes(p)))
        }
        0x03 => {
            let mut l = [0u8; 1];
            sock.read_exact(&mut l).await?;
            let len = l[0] as usize;
            if len == 0 {
                reply(sock, REP_GENERAL_FAILURE).await?;
                bail!("empty hostname");
            }
            // Hostname ≤ 255; stack-allocate.
            let mut name = [0u8; 255];
            sock.read_exact(&mut name[..len]).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            let host = std::str::from_utf8(&name[..len])
                .context("hostname not utf8")?
                .to_string();
            Ok((host, u16::from_be_bytes(p)))
        }
        0x04 => {
            let mut a = [0u8; 16];
            sock.read_exact(&mut a).await?;
            let mut p = [0u8; 2];
            sock.read_exact(&mut p).await?;
            Ok((Ipv6Addr::from(a).to_string(), u16::from_be_bytes(p)))
        }
        _ => {
            reply(sock, REP_ADDR_TYPE_NOT_SUPPORTED).await?;
            bail!("unsupported atyp {atyp}");
        }
    }
}

async fn reply<S>(sock: &mut S, code: u8) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    // Stack-allocated 10-byte SOCKS5 error reply (avoids per-error-path heap).
    let code = if code == 0 { REP_GENERAL_FAILURE } else { code };
    let resp = [0x05, code, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    sock.write_all(&resp).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Drive `read_socks5_request` over an in-memory duplex pair. Each test
    //! plays a byte-level "client" against the parser running in a spawned
    //! task and asserts both the parser's return and the bytes it emitted on
    //! the wire.
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Spawn the parser on one half of a duplex; the test drives the other
    /// half as the "client". Returns the parser's task join handle and a
    /// handle to the client side.
    fn spawn_parser() -> (
        tokio::task::JoinHandle<Result<(String, u16)>>,
        tokio::io::DuplexStream,
    ) {
        let (server, client) = tokio::io::duplex(4096);
        let handle = tokio::spawn(async move {
            let mut server = server;
            read_socks5_request(&mut server).await
        });
        (handle, client)
    }

    #[tokio::test]
    async fn parses_ipv4_connect() {
        let (h, mut client) = spawn_parser();
        // greeting: ver=5, nmethods=1, methods=[0x00]
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        // server should reply [0x05, 0x00]
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [0x05, 0x00]);
        // request: ver=5, cmd=CONNECT, rsv=0, atyp=IPv4, addr=1.2.3.4, port=443
        client
            .write_all(&[0x05, 0x01, 0x00, 0x01, 1, 2, 3, 4, 0x01, 0xBB])
            .await
            .unwrap();
        let (host, port) = h.await.unwrap().unwrap();
        assert_eq!(host, "1.2.3.4");
        assert_eq!(port, 443);
    }

    #[tokio::test]
    async fn parses_domain_connect() {
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        // request: ver=5, cmd=CONNECT, rsv=0, atyp=DOMAIN, len=11, "example.com", port=80
        let name = b"example.com";
        let mut req = vec![0x05, 0x01, 0x00, 0x03, name.len() as u8];
        req.extend_from_slice(name);
        req.extend_from_slice(&80u16.to_be_bytes());
        client.write_all(&req).await.unwrap();
        let (host, port) = h.await.unwrap().unwrap();
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);
    }

    #[tokio::test]
    async fn parses_ipv6_connect() {
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        let addr = Ipv6Addr::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1);
        let mut req = vec![0x05, 0x01, 0x00, 0x04];
        req.extend_from_slice(&addr.octets());
        req.extend_from_slice(&8080u16.to_be_bytes());
        client.write_all(&req).await.unwrap();
        let (host, port) = h.await.unwrap().unwrap();
        assert_eq!(host, addr.to_string());
        assert_eq!(port, 8080);
    }

    #[tokio::test]
    async fn rejects_wrong_socks_version() {
        let (h, mut client) = spawn_parser();
        // SOCKS4 byte; we only speak 5.
        client.write_all(&[0x04, 0x01, 0x00]).await.unwrap();
        let err = h.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("not SOCKS5"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_no_acceptable_auth_method() {
        let (h, mut client) = spawn_parser();
        // ver=5, nmethods=1, methods=[0x02] (username/password — we don't support it)
        client.write_all(&[0x05, 0x01, 0x02]).await.unwrap();
        // Server should send [0x05, 0xFF] (no acceptable methods).
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        assert_eq!(reply, [0x05, 0xFF]);
        let err = h.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("acceptable auth method"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_bind_command() {
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        // cmd=0x02 (BIND) — we only do CONNECT.
        client
            .write_all(&[0x05, 0x02, 0x00, 0x01, 0, 0, 0, 0, 0, 80])
            .await
            .unwrap();
        // Should get a 0x07 (command not supported) reply on the wire.
        let mut resp = [0u8; 10];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp[0], 0x05);
        assert_eq!(resp[1], REP_COMMAND_NOT_SUPPORTED);
        let err = h.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("unsupported command"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_unknown_atyp() {
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        // atyp=0x09 (not a real type).
        client.write_all(&[0x05, 0x01, 0x00, 0x09]).await.unwrap();
        let mut resp = [0u8; 10];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp[1], REP_ADDR_TYPE_NOT_SUPPORTED);
        let err = h.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("unsupported atyp"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_zero_length_domain() {
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        // atyp=DOMAIN, len=0 — pathological input, must be rejected (else
        // we'd try to open an SSH channel to "" which would surface a confusing
        // server-side error).
        client
            .write_all(&[0x05, 0x01, 0x00, 0x03, 0x00])
            .await
            .unwrap();
        let mut resp = [0u8; 10];
        client.read_exact(&mut resp).await.unwrap();
        assert_eq!(resp[0], 0x05);
        assert_eq!(resp[1], REP_GENERAL_FAILURE);
        let err = h.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("empty hostname"), "got: {err}");
    }

    #[tokio::test]
    async fn rejects_non_utf8_domain() {
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        // domain bytes contain a stray 0xFF — invalid UTF-8.
        let mut req = vec![0x05, 0x01, 0x00, 0x03, 0x03, b'a', 0xFF, b'b'];
        req.extend_from_slice(&80u16.to_be_bytes());
        client.write_all(&req).await.unwrap();
        let err = h.await.unwrap().unwrap_err().to_string();
        assert!(err.contains("hostname"), "got: {err}");
    }

    #[tokio::test]
    async fn truncated_request_is_an_error_not_a_panic() {
        // Greeting fine, but then we send only the header without the address.
        let (h, mut client) = spawn_parser();
        client.write_all(&[0x05, 0x01, 0x00]).await.unwrap();
        let mut reply = [0u8; 2];
        client.read_exact(&mut reply).await.unwrap();
        // 4-byte header announces IPv4 but we never write the 4 address bytes —
        // then drop the connection.
        client.write_all(&[0x05, 0x01, 0x00, 0x01]).await.unwrap();
        drop(client);
        let r = h.await.unwrap();
        assert!(r.is_err(), "expected error on truncation, got {r:?}");
    }
}
