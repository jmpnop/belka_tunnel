# БелкаТуннель

> 🇷🇺 Русская версия — [README.ru.md](README.ru.md)

A native macOS menu-bar app that maintains a persistent SSH tunnel and exposes
it to local clients as **both** a SOCKS5 proxy **and** an HTTP/HTTPS forward
proxy. Traffic is routed out through a pfSense WAN IP, so the destination site
sees the router's address rather than the client's.

```
Client Mac (any network)  ──native SSH──▶  pfSense WAN  ──▶  internet
  Firefox → 127.0.0.1:1080 (SOCKS5)   aurora.celestialtech.io
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

## Install

1. Download `BelkaTunnel-<version>.dmg` from the
   [Releases](https://github.com/jmpnop/belka_tunnel/releases) page.
2. Open the DMG, drag **BelkaTunnel** to `/Applications`.
3. Launch it. A menu-bar item appears: `●` Connected · `○` Connecting ·
   `✕` Disconnected.

On first launch it writes a default config to
`~/Library/Application Support/io.celestialtech.BelkaTunnel/config.json`
(matching the working CLI: `olgatimoshevskaia@aurora.celestialtech.io:22222`,
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
`dig +short aurora.celestialtech.io @1.1.1.1`.

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
