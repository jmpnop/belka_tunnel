//! HTTP/HTTPS forward proxy — a second adapter onto the same SSH tunnel that
//! the SOCKS5 server (`socks.rs`) exposes, for clients that only speak HTTP
//! proxy (curl/git/npm honoring `http_proxy`/`https_proxy`, macOS "Web Proxy"
//! fields) and can't use SOCKS5.
//!
//! It is **not** a TLS-terminating / MITM proxy: HTTPS works because the client
//! issues `CONNECT host:443` and runs its own TLS end-to-end over the raw byte
//! stream we tunnel — we never see plaintext. Two request shapes are handled:
//!
//!   * `CONNECT host:port` — open a channel, reply `200`, bridge raw bytes.
//!   * absolute-form `GET http://host/path` (plain HTTP) — rewrite the request
//!     line to origin-form, strip proxy headers, force `Connection: close`,
//!     then forward + bridge.
//!
//! Everything after parsing reuses `tunnel::open_channel` + `tunnel::bridge`
//! verbatim — the SSH-forwarding primitives are protocol-agnostic.

use crate::config::Profile as Config;
use crate::tunnel::{self, ClientHandle};
use anyhow::{anyhow, bail, Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

/// Cap on time the client has to deliver the full request head (request-line +
/// headers) before we close. Protects against slow / dead clients pinning an FD.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Upper bound on the request head we'll buffer while looking for the blank
/// line. Bounds a slowloris-style client that streams headers forever.
const MAX_HEAD_BYTES: usize = 64 * 1024;

pub async fn serve(config: Arc<Config>, ssh: ClientHandle, dead: Arc<Notify>) -> Result<()> {
    // Disabled → bind nothing (SOCKS5 still runs); park until the session dies
    // so the caller's join/abort lifecycle is unchanged.
    if !config.http.enabled {
        info!("HTTP proxy disabled; not binding");
        dead.notified().await;
        return Ok(());
    }

    let listen = format!("{}:{}", config.http.listen_addr, config.http.listen_port);
    let listener = match TcpListener::bind(&listen).await {
        Ok(l) => l,
        Err(e) => {
            // Isolate bind failures: a propagated error would tear down the
            // whole session (and SOCKS5 with it). A misconfigured/occupied
            // HTTP port must NOT take SOCKS5 down — log, notify, and park.
            // Without the notification the menu still advertises the HTTP
            // endpoints while clients silently get connection-refused.
            warn!(addr = %listen, error = %e, "HTTP proxy bind failed; SOCKS5 unaffected");
            crate::notify_user(
                "БелкаТуннель — HTTP proxy unavailable",
                &format!("Couldn't listen on {listen}: {e}. Change the HTTP port in Edit Configuration (the SOCKS5 proxy is still running)."),
            );
            dead.notified().await;
            return Ok(());
        }
    };
    info!(addr = %listen, "HTTP proxy listening");

    loop {
        tokio::select! {
            res = listener.accept() => {
                let (sock, peer) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        // EMFILE/ENFILE etc. would hot-loop; back off briefly.
                        warn!(error = %e, "accept failed; backing off 100ms");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        continue;
                    }
                };
                let _ = sock.set_nodelay(true);
                let ssh = ssh.clone();
                let dead_for_conn = dead.clone();
                tokio::spawn(async move {
                    // Race the connection against session-dead so in-flight
                    // bridges don't hold sockets after the SSH side goes away.
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
                info!("HTTP proxy listener shutting down (SSH session ended)");
                return Ok(());
            }
        }
    }
}

async fn handle_conn(mut sock: TcpStream, ssh: ClientHandle) -> Result<()> {
    // Read the whole head (bounded by time + size). `buf` holds head + any
    // bytes the client pipelined past the blank line (a POST body's first
    // bytes, or — rarely — CONNECT payload sent before our 200).
    let (head_len, buf) = tokio::time::timeout(HANDSHAKE_TIMEOUT, read_head(&mut sock))
        .await
        .context("HTTP head read timeout")??;

    let target = match parse_request(&buf[..head_len]) {
        Ok(t) => t,
        Err(e) => {
            let _ = write_status(&mut sock, 400, "Bad Request").await;
            return Err(e);
        }
    };
    let leftover = &buf[head_len..];

    match target {
        Target::Connect { host, port } => {
            debug!(target_host = %host, target_port = port, "HTTP CONNECT");
            let channel = match tunnel::open_channel(&ssh, &host, port).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = write_status(&mut sock, 502, "Bad Gateway").await;
                    return Err(anyhow!("open channel: {e}"));
                }
            };
            sock.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                .await
                .context("write CONNECT 200")?;
            if !leftover.is_empty() {
                channel
                    .data(leftover)
                    .await
                    .map_err(|e| anyhow!("forward pipelined bytes: {e}"))?;
            }
            tunnel::bridge(sock, channel).await
        }
        Target::Forward {
            host,
            port,
            rewritten_head,
        } => {
            debug!(target_host = %host, target_port = port, "HTTP forward");
            let channel = match tunnel::open_channel(&ssh, &host, port).await {
                Ok(c) => c,
                Err(e) => {
                    let _ = write_status(&mut sock, 502, "Bad Gateway").await;
                    return Err(anyhow!("open channel: {e}"));
                }
            };
            channel
                .data(&rewritten_head[..])
                .await
                .map_err(|e| anyhow!("forward request head: {e}"))?;
            if !leftover.is_empty() {
                channel
                    .data(leftover)
                    .await
                    .map_err(|e| anyhow!("forward request body: {e}"))?;
            }
            tunnel::bridge(sock, channel).await
        }
    }
}

/// Read from `sock` until the `\r\n\r\n` header terminator, returning the byte
/// index just past it and the full buffer (head + any trailing pipelined
/// bytes). Generic so tests can drive it with `tokio::io::duplex`.
async fn read_head<S>(sock: &mut S) -> Result<(usize, Vec<u8>)>
where
    S: AsyncRead + Unpin,
{
    let mut buf: Vec<u8> = Vec::with_capacity(1024);
    let mut tmp = [0u8; 4096];
    let mut search_from = 0usize;
    loop {
        let n = sock.read(&mut tmp).await.context("read HTTP head")?;
        if n == 0 {
            bail!("client closed before completing request head");
        }
        buf.extend_from_slice(&tmp[..n]);
        if buf.len() > MAX_HEAD_BYTES {
            bail!("HTTP request head exceeds {MAX_HEAD_BYTES} bytes");
        }
        // Backtrack 3 bytes so a terminator straddling two reads is still found.
        let start = search_from.saturating_sub(3);
        if let Some(pos) = find_subslice(&buf[start..], b"\r\n\r\n") {
            return Ok((start + pos + 4, buf));
        }
        search_from = buf.len();
    }
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Minimal status reply with `Connection: close` and an empty body. Used for
/// the proxy's own error responses (400 / 502).
async fn write_status<S>(sock: &mut S, code: u16, reason: &str) -> Result<()>
where
    S: AsyncWrite + Unpin,
{
    let resp =
        format!("HTTP/1.1 {code} {reason}\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
    sock.write_all(resp.as_bytes()).await?;
    Ok(())
}

/// What the client asked us to reach.
#[derive(Debug, PartialEq, Eq)]
enum Target {
    /// `CONNECT host:port` — tunnel raw bytes (this is how HTTPS rides through).
    Connect { host: String, port: u16 },
    /// Plain-HTTP forward: dial `host:port`, write `rewritten_head` (origin-form
    /// request line + filtered headers + `Connection: close`), then bridge.
    Forward {
        host: String,
        port: u16,
        rewritten_head: Vec<u8>,
    },
}

/// Pure parser over the request head bytes (everything up to and including the
/// `\r\n\r\n`). This is the unit-testable core — `handle_conn` is just I/O glue
/// around it.
///
/// Operates on raw bytes rather than a UTF-8 `str`: HTTP request-targets and
/// header values are opaque octets (RFC 7230), not required to be UTF-8, so
/// requiring the whole head to decode would 400 otherwise-legal requests that
/// the SOCKS path accepts. Only the pieces we actually interpret — method,
/// version, and the authority (host:port) — need to be ASCII/UTF-8; the path
/// and header bytes are forwarded verbatim.
fn parse_request(head: &[u8]) -> Result<Target> {
    let lines = head_lines(head);
    let request_line = lines.first().copied().unwrap_or(&[]);

    let mut parts = request_line.splitn(3, |&b| b == b' ');
    let method = parts.next().unwrap_or(&[]);
    let target = parts
        .next()
        .ok_or_else(|| anyhow!("malformed request line"))?;
    let version = parts
        .next()
        .ok_or_else(|| anyhow!("malformed request line"))?;
    if method.is_empty() {
        bail!("empty method");
    }
    if !version.starts_with(b"HTTP/") {
        bail!("unsupported HTTP version");
    }

    // --- CONNECT: authority-form target, no default port (must be explicit) ---
    if method.eq_ignore_ascii_case(b"CONNECT") {
        let target = std::str::from_utf8(target).map_err(|_| anyhow!("CONNECT target not utf8"))?;
        let (host, port) = split_host_port(target, None)
            .ok_or_else(|| anyhow!("bad CONNECT target {target:?}"))?;
        return Ok(Target::Connect { host, port });
    }

    // --- plain HTTP forward: target must be absolute-form http://… ---
    // (An origin-form target like `GET /path` only makes sense to an origin
    // server, not a proxy — a proxy needs the absolute URI to know the host.)
    // Scheme match is case-insensitive per RFC 3986 §3.1 (`HTTP://` is valid).
    if target.len() < 7 || !target[..7].eq_ignore_ascii_case(b"http://") {
        bail!("non-CONNECT request without absolute http:// URI");
    }
    let rest = &target[7..];
    let (authority, path): (&[u8], &[u8]) = match rest.iter().position(|&b| b == b'/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, b"/"),
    };
    // The authority (host[:port]) must be text we can resolve; the path is opaque.
    let authority = std::str::from_utf8(authority).map_err(|_| anyhow!("authority not utf8"))?;
    // Drop any `userinfo@` prefix before host:port.
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let (host, port) = split_host_port(authority, Some(80))
        .ok_or_else(|| anyhow!("bad authority {authority:?}"))?;

    // Rebuild: origin-form request line, then headers minus proxy/connection
    // headers, then a forced `Connection: close` (one request = one dial+bridge;
    // keeps us from needing a full keep-alive/message-framing parser). Header
    // bytes are copied verbatim — only the ASCII header *name* is inspected.
    let mut out = Vec::with_capacity(head.len());
    out.extend_from_slice(method);
    out.push(b' ');
    out.extend_from_slice(path);
    out.extend_from_slice(b" HTTP/1.1\r\n");

    let mut have_host = false;
    for &line in lines.iter().skip(1) {
        if line.is_empty() {
            break; // blank line ends the headers
        }
        let name = header_name(line);
        // Hop-by-hop / proxy-specific headers the origin must not see.
        if name.eq_ignore_ascii_case(b"proxy-connection")
            || name.eq_ignore_ascii_case(b"proxy-authorization")
            || name.eq_ignore_ascii_case(b"connection")
        {
            continue;
        }
        if name.eq_ignore_ascii_case(b"host") {
            have_host = true;
        }
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    // HTTP/1.0 clients may omit Host; synthesize it from the absolute URI so the
    // origin (and any vhost) still routes correctly.
    if !have_host {
        out.extend_from_slice(b"Host: ");
        out.extend_from_slice(authority.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: close\r\n\r\n");

    Ok(Target::Forward {
        host,
        port,
        rewritten_head: out,
    })
}

/// Split a request head into CRLF-delimited lines at the byte level (tolerating
/// a lone `\n`), without requiring UTF-8. The trailing blank line(s) appear as
/// empty slices, which `parse_request` uses to stop at the end of the headers.
fn head_lines(head: &[u8]) -> Vec<&[u8]> {
    head.split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l))
        .collect()
}

/// The ASCII header name (bytes before the first `:`), trimmed of surrounding
/// ASCII whitespace. Returns the whole line if there's no colon.
fn header_name(line: &[u8]) -> &[u8] {
    let end = line.iter().position(|&b| b == b':').unwrap_or(line.len());
    trim_ascii(&line[..end])
}

fn trim_ascii(mut s: &[u8]) -> &[u8] {
    while let [first, rest @ ..] = s {
        if first.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    while let [rest @ .., last] = s {
        if last.is_ascii_whitespace() {
            s = rest;
        } else {
            break;
        }
    }
    s
}

/// Split an `authority` into `(host, port)`. Handles `host`, `host:port`, and
/// bracketed IPv6 `[::1]` / `[::1]:443`. Returns `None` for malformed input,
/// for an explicit port `0` (which would open a channel to a nonsense port —
/// the SOCKS and config paths reject 0 too), or when no port is present and no
/// `default_port` is supplied (CONNECT requires an explicit port).
fn split_host_port(s: &str, default_port: Option<u16>) -> Option<(String, u16)> {
    // Parse a port string, rejecting 0 (a valid u16 but never a real target).
    fn parse_port(s: &str) -> Option<u16> {
        match s.parse::<u16>().ok()? {
            0 => None,
            p => Some(p),
        }
    }
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    // Bracketed IPv6 literal.
    if let Some(rest) = s.strip_prefix('[') {
        let close = rest.find(']')?;
        let host = &rest[..close];
        if host.is_empty() {
            return None;
        }
        let after = &rest[close + 1..];
        let port = if let Some(p) = after.strip_prefix(':') {
            parse_port(p)?
        } else if after.is_empty() {
            default_port?
        } else {
            return None;
        };
        return Some((host.to_string(), port));
    }
    // host[:port]. rsplit so we split on the LAST colon; reject a bare
    // unbracketed IPv6 (host would still contain a ':').
    match s.rsplit_once(':') {
        Some((host, port)) if !host.is_empty() && !host.contains(':') => {
            Some((host.to_string(), parse_port(port)?))
        }
        Some(_) => None,
        None => Some((s.to_string(), default_port?)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    fn forward_head(target: Target) -> String {
        match target {
            Target::Forward { rewritten_head, .. } => String::from_utf8(rewritten_head).unwrap(),
            other => panic!("expected Forward, got {other:?}"),
        }
    }

    #[test]
    fn parses_connect() {
        let t = parse_request(b"CONNECT example.com:443 HTTP/1.1\r\nHost: example.com:443\r\n\r\n")
            .unwrap();
        assert_eq!(
            t,
            Target::Connect {
                host: "example.com".into(),
                port: 443
            }
        );
    }

    #[test]
    fn parses_connect_ipv6() {
        let t = parse_request(b"CONNECT [2001:db8::1]:8443 HTTP/1.1\r\n\r\n").unwrap();
        assert_eq!(
            t,
            Target::Connect {
                host: "2001:db8::1".into(),
                port: 8443
            }
        );
    }

    #[test]
    fn connect_without_port_is_error() {
        // CONNECT must carry an explicit port; no default applies.
        assert!(parse_request(b"CONNECT example.com HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn parses_forward_default_port_and_rewrites_to_origin_form() {
        let t = parse_request(
            b"GET http://example.com/path?q=1 HTTP/1.1\r\nHost: example.com\r\nUser-Agent: x\r\n\r\n",
        )
        .unwrap();
        let (host, port) = match &t {
            Target::Forward { host, port, .. } => (host.clone(), *port),
            _ => panic!("expected Forward"),
        };
        assert_eq!(host, "example.com");
        assert_eq!(port, 80);
        let head = forward_head(t);
        // Origin-form request line — the absolute URI is gone.
        assert!(
            head.starts_with("GET /path?q=1 HTTP/1.1\r\n"),
            "head: {head:?}"
        );
        assert!(head.contains("Host: example.com\r\n"));
        assert!(head.contains("User-Agent: x\r\n"));
        assert!(head.ends_with("Connection: close\r\n\r\n"));
    }

    #[test]
    fn forward_parses_explicit_port_and_empty_path() {
        let t = parse_request(
            b"GET http://example.com:8081 HTTP/1.1\r\nHost: example.com:8081\r\n\r\n",
        )
        .unwrap();
        let port = match &t {
            Target::Forward { port, .. } => *port,
            _ => panic!("expected Forward"),
        };
        assert_eq!(port, 8081);
        // No path in the URI → origin-form path defaults to "/".
        assert!(forward_head(t).starts_with("GET / HTTP/1.1\r\n"));
    }

    #[test]
    fn forward_strips_proxy_headers_and_replaces_connection() {
        let t = parse_request(
            b"GET http://h/ HTTP/1.1\r\nHost: h\r\nProxy-Connection: keep-alive\r\n\
              Proxy-Authorization: Basic abc\r\nConnection: keep-alive\r\n\r\n",
        )
        .unwrap();
        let head = forward_head(t);
        assert!(
            !head.to_lowercase().contains("proxy-connection"),
            "{head:?}"
        );
        assert!(
            !head.to_lowercase().contains("proxy-authorization"),
            "{head:?}"
        );
        // The only Connection header is our forced close.
        assert_eq!(head.matches("Connection: close").count(), 1, "{head:?}");
        assert!(!head.contains("keep-alive"), "{head:?}");
    }

    #[test]
    fn forward_synthesizes_host_when_absent() {
        // HTTP/1.0-style request with no Host header.
        let t = parse_request(b"GET http://example.org:8080/p HTTP/1.0\r\n\r\n").unwrap();
        let head = forward_head(t);
        assert!(head.contains("Host: example.org:8080\r\n"), "{head:?}");
    }

    #[test]
    fn origin_form_to_proxy_is_error() {
        // A proxy can't act on an origin-form target — it has no host.
        assert!(parse_request(b"GET /path HTTP/1.1\r\nHost: example.com\r\n\r\n").is_err());
    }

    #[test]
    fn malformed_request_line_is_error() {
        assert!(parse_request(b"GARBAGE\r\n\r\n").is_err());
        assert!(parse_request(b"GET\r\n\r\n").is_err());
    }

    #[test]
    fn scheme_match_is_case_insensitive() {
        // RFC 3986 §3.1: the scheme is case-insensitive. `HTTP://` must parse.
        let t = parse_request(b"GET HTTP://example.com/ HTTP/1.1\r\nHost: example.com\r\n\r\n")
            .unwrap();
        match t {
            Target::Forward { host, port, .. } => {
                assert_eq!(host, "example.com");
                assert_eq!(port, 80);
            }
            _ => panic!("expected Forward"),
        }
    }

    #[test]
    fn forward_preserves_non_utf8_path_bytes() {
        // A path with raw non-UTF-8 octets is legal (opaque per RFC 7230) and
        // must be forwarded verbatim, not rejected.
        let mut req = b"GET http://h/p".to_vec();
        req.extend_from_slice(&[0xff, 0xfe]); // non-UTF-8 in the path
        req.extend_from_slice(b" HTTP/1.1\r\nHost: h\r\n\r\n");
        let t = parse_request(&req).unwrap();
        match t {
            Target::Forward { rewritten_head, .. } => {
                assert!(
                    rewritten_head.starts_with(b"GET /p\xff\xfe HTTP/1.1\r\n"),
                    "head bytes: {rewritten_head:?}"
                );
            }
            _ => panic!("expected Forward"),
        }
    }

    #[test]
    fn forward_preserves_non_utf8_header_value() {
        let mut req = b"GET http://h/ HTTP/1.1\r\nHost: h\r\nX-Weird: ".to_vec();
        req.extend_from_slice(&[0xff]); // non-UTF-8 header value
        req.extend_from_slice(b"\r\n\r\n");
        let t = parse_request(&req).unwrap();
        match t {
            Target::Forward { rewritten_head, .. } => {
                // The weird header survives byte-for-byte.
                assert!(
                    rewritten_head.windows(11).any(|w| w == b"X-Weird: \xff\r"),
                    "head bytes: {rewritten_head:?}"
                );
            }
            _ => panic!("expected Forward"),
        }
    }

    #[test]
    fn connect_non_utf8_authority_is_error() {
        // The authority (host:port) is interpreted, so it must be valid text.
        assert!(parse_request(b"CONNECT \xff\xfe:443 HTTP/1.1\r\n\r\n").is_err());
    }

    #[test]
    fn port_zero_is_rejected() {
        assert!(parse_request(b"CONNECT h:0 HTTP/1.1\r\n\r\n").is_err());
        assert!(parse_request(b"GET http://h:0/ HTTP/1.1\r\nHost: h\r\n\r\n").is_err());
    }

    #[test]
    fn split_host_port_variants() {
        assert_eq!(split_host_port("h:80", None), Some(("h".into(), 80u16)));
        assert_eq!(split_host_port("h", Some(80)), Some(("h".into(), 80)));
        assert_eq!(split_host_port("h", None), None);
        assert_eq!(split_host_port("[::1]:5", None), Some(("::1".into(), 5)));
        assert_eq!(split_host_port("[::1]", Some(9)), Some(("::1".into(), 9)));
        assert_eq!(split_host_port("::1", None), None); // bare IPv6 must be bracketed
        assert_eq!(split_host_port("h:notaport", None), None);
        assert_eq!(split_host_port("h:0", None), None); // port 0 rejected
        assert_eq!(split_host_port("[::1]:0", None), None);
        assert_eq!(split_host_port("", Some(80)), None);
    }

    #[tokio::test]
    async fn read_head_splits_at_blank_line_and_keeps_body() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            a.write_all(b"GET http://x/ HTTP/1.1\r\nHost: x\r\n\r\nBODYBYTES")
                .await
                .unwrap();
        });
        let (len, buf) = read_head(&mut b).await.unwrap();
        assert!(buf[..len].ends_with(b"\r\n\r\n"));
        assert_eq!(&buf[len..], b"BODYBYTES");
    }

    #[tokio::test]
    async fn read_head_errors_on_premature_close() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        tokio::spawn(async move {
            a.write_all(b"GET http://x/ HTTP/1.1\r\n").await.unwrap();
            // drop without the blank line
        });
        assert!(read_head(&mut b).await.is_err());
    }
}
