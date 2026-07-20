# БелкаТуннель

> 🇷🇺 Русская версия — [README.ru.md](README.ru.md)

A native macOS menu-bar app that maintains a persistent SSH tunnel and exposes
it to local clients as **both** a SOCKS5 proxy **and** an HTTP/HTTPS forward
proxy. Traffic is routed out through a pfSense WAN IP, so the destination site
sees the router's address rather than the client's.

```
Client Mac (any network)  ──native SSH──▶  pfSense WAN  ──▶  internet
  Firefox → 127.0.0.1:1080 (SOCKS5)   proxy.example.com
  curl    → 127.0.0.1:8080 (HTTP)    (→ current WAN IP, kept fresh
                                        by Cloudflare DDNS) on port 22222
```

**Latest release:** [`v0.2.0`](https://github.com/jmpnop/belka_tunnel/releases/tag/v0.2.0)
— adds the HTTP/HTTPS forward proxy alongside SOCKS5. Download
`BelkaTunnel-0.2.0.dmg` from the release page.

## What it is

- A self-contained menu-bar app (`LSUIElement` — no Dock icon) written in Rust.
- Speaks SSH natively via [`russh`](https://crates.io/crates/russh) — it does
  **not** shell out to the system `ssh` binary. Loads `~/.ssh/id_ed25519`
  directly.
- Exposes the one SSH tunnel two ways at once:
  - **SOCKS5** proxy — default `0.0.0.0:1080` (what Firefox uses).
  - **HTTP/HTTPS forward proxy** — default `0.0.0.0:8080`, for clients that only
    speak HTTP proxy: anything honoring `http_proxy`/`https_proxy` (curl, git,
    npm, wget) and macOS's system *Web Proxy* / *Secure Web Proxy* fields.
- Bundle id `io.celestialtech.BelkaTunnel`; config + logs live under
  `~/Library/Application Support/io.celestialtech.BelkaTunnel/`.

The two proxy front-ends are independent, thin adapters onto the same tunnel —
HTTP is **not** converted to SOCKS5. Each parses the client's "where to" request
and hands `(host, port)` to the shared SSH-forwarding primitives. The HTTP proxy
handles `CONNECT` (how HTTPS rides through — end-to-end TLS, never MITM) and
absolute-URI plain-HTTP forwarding. Neither proxy authenticates: the perimeter is
the LAN plus the SSH tunnel, not a proxy password.

Because both proxies listen on `0.0.0.0` by default, the Mac running the app is a
**proxy gateway for the whole LAN** — a phone or another laptop can route through
the same tunnel by pointing at the Mac's LAN IP (see
[Share the tunnel with a phone](#share-the-tunnel-with-a-phone--other-devices)).

## Features

- **Two proxies, one tunnel** — SOCKS5 (`1080`) and HTTP/HTTPS (`8080`) at once,
  each toggleable and independently addressable.
- **Menu-bar control** — click the status row to connect/disconnect; copy either
  proxy endpoint to the clipboard; toggle "Listen on all interfaces".
- **LAN gateway** — the endpoint rows list every reachable address
  (`127.0.0.1:1080`, `192.168.1.100:1080 (en0)`, …) so other devices can join.
- **Auto-reconnect** with exponential backoff + a watchdog that tears down fast
  on server-side disconnects.
- **Trust-on-first-use host-key pinning** — records the server fingerprint on
  first connect; refuses + notifies on a later mismatch (MITM guard).
- **Launch at Login** — one toggle writes a per-user LaunchAgent.
- **Managed Firefox** — install/update Firefox straight from mozilla.org and
  point it at the SOCKS5 proxy with a locked, anti-leak policy (DNS-over-SOCKS,
  WebRTC/DoH disabled).
- **Config GUI** editor; a file-system watcher self-restarts the daemon on save.
- **Reveal Data Folder / Open Log File** from the menu; daily-rotated logs.

## Install

1. Download `BelkaTunnel-<version>.dmg` from the
   [Releases](https://github.com/jmpnop/belka_tunnel/releases) page.
2. Open the DMG, drag **BelkaTunnel** to `/Applications`.
3. Launch it. A menu-bar item appears: `●` Connected · `○` Connecting ·
   `✕` Disconnected.

On first launch it writes a default config to
`~/Library/Application Support/io.celestialtech.BelkaTunnel/config.json`
(matching the working CLI: `tunneluser@proxy.example.com:22222`,
key `~/.ssh/id_ed25519`, SOCKS5 on `0.0.0.0:1080`, HTTP on `0.0.0.0:8080`). Edit
via the menu's **Edit Configuration** (a GUI editor); saving self-restarts the
daemon.

## Usage

With the app connected:

```bash
# HTTPS through the HTTP proxy (CONNECT) — returns the pfSense WAN IP:
curl -x http://127.0.0.1:8080 https://ifconfig.me
https_proxy=http://127.0.0.1:8080 curl https://ifconfig.me

# plain HTTP forward:
curl -x http://127.0.0.1:8080 http://ifconfig.me

# SOCKS5:
curl --socks5-hostname 127.0.0.1:1080 https://ifconfig.me
```

**Firefox** (*Settings → Network → Connection Settings*): Manual proxy, SOCKS
Host `127.0.0.1` port `1080`, SOCKS v5, ✅ *Proxy DNS when using SOCKS v5*.

All of the above should return the current pfSense WAN IP — compare against
`dig +short proxy.example.com @1.1.1.1`.

### Share the tunnel with a phone / other devices

The Mac running БелкаТуннель is a proxy gateway for the whole LAN — no app or SSH
client needed on the other device, just its OS proxy fields.

1. On the host Mac, open the menu — the endpoint rows show the LAN address to
   use, e.g. `192.168.1.100:8080 (en0)` for HTTP or `192.168.1.100:1080 (en0)`
   for SOCKS5. (Only see `127.0.0.1 (loopback)`? Turn on "Listen on all
   interfaces".)
2. **iPhone/iPad:** *Settings → Wi-Fi → ⓘ → Configure Proxy → Manual* → Server
   `192.168.1.100`, Port `8080`. (iOS Wi-Fi proxy is HTTP-only; HTTPS still works
   via CONNECT.)
3. **Another Mac:** *System Settings → Network → … → Details → Proxies* → enable
   *Web Proxy (HTTP)* + *Secure Web Proxy (HTTPS)* → `192.168.1.100:8080`.
4. **Android:** *Wi-Fi → network → Advanced → Proxy → Manual* → same host/port.

On the phone, load `https://ifconfig.me` — it should show the pfSense WAN IP,
confirming traffic exits through the tunnel.

> ⚠️ On `0.0.0.0` with no proxy auth, anyone who can reach the Mac on ports
> 1080/8080 can use the tunnel. That's the intended posture on a trusted home
> LAN; on an untrusted network turn "Listen on all interfaces" **off**.

## Build from source

Build/test/package operations live in a uv-managed Python CLI at `tools/`. Run
`./bt` from the repo root for the full list.

```bash
./bt bootstrap        # install git hooks + verify toolchain
./bt bundle           # cargo build --release + assemble dist/BelkaTunnel.app
./bt dmg              # build dist/BelkaTunnel-<version>.dmg
./bt verify bundle    # Info.plist + codesign + arch + icon checks
./bt precommit        # fmt-check + clippy + workspace tests
./bt ci               # full pipeline
```

Run the daemon in the foreground with logs on stderr:

```bash
RUST_LOG=info app/dist/BelkaTunnel.app/Contents/MacOS/belka_tunnel
```

## Repo layout

This repo is a Cargo workspace:

```
belka_tunnel/
├── app/                 # БелкаТуннель — the menu-bar tunnel daemon
├── crates/belka-ui/     # shared Zed-inspired theme + form helpers
├── pfusers/             # pfUsers — a companion app for CRUDing pfSense
│                        #   tunnel-user accounts on the router
└── tools/               # the `bt` Python CLI (builds both apps)
```

**pfUsers** is a separate windowed admin app for managing the router-side tunnel
user accounts (e.g. tightening a user's privilege to `user-ssh-tunnel`). It
shares the theme via `crates/belka-ui/`.

## Documentation

The full operational reference — pfSense state, firewall rules, Cloudflare DDNS,
the proxy front-end internals, DMG/icon build details, CI/CD, and a
troubleshooting matrix — lives in [`CLAUDE.md`](CLAUDE.md).
