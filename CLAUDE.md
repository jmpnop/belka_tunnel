# БелкаТуннель

Setting up an SSH-based SOCKS5 proxy that routes Olga's traffic through Pasha's pfSense WAN IP. The native macOS app under `app/` is `БелкаТуннель`; the working directory is `~/RustRoverProjects/belka_tunnel/` (migrated from the original `~/PycharmProjects/ssh-proxy-wan/` once the project outgrew Python tooling).

**Repo:** https://github.com/jmpnop/belka_tunnel — current release is `v0.2.0` with `BelkaTunnel-0.2.0.dmg` attached (adds the HTTP/HTTPS forward proxy alongside SOCKS5; built + published locally via `./bt dmg` + `gh release create` since the tag-triggered `release.yml` workflow currently has a file issue). `v0.1.0` was the initial release. Earlier `jmpnop/belka-tunnel` was deleted and history rewritten to strip co-author trailers on 2026-05-29.

The repo grew a small companion tool — **pfUsers** under `pfusers/` — for managing the router-side tunnel user accounts (created when olga needed her `user-shell-access` priv tightened to `user-ssh-tunnel`). It's documented later in this file under *Native macOS app — pfUsers*, and shares the Zed-inspired theme via `crates/belka-ui/`. BelkaTunnel itself is the project; pfUsers is in service of it.

Architecture:

```
Olga's Mac (any network)  ──ssh -D 1080──▶  pfSense WAN  ──▶  internet
  Firefox → 127.0.0.1:1080         aurora.celestialtech.io
                                  (→ 173.77.254.243, kept current via
                                     Cloudflare DDNS) on port 22222
```

## Endpoints

| Role          | Address                          | Notes                                    |
|---------------|----------------------------------|------------------------------------------|
| pfSense WAN   | `173.77.254.243:22222`           | sshd moved from 22 → 22222 on 2026-05-28 |
| DDNS hostname | `aurora.celestialtech.io`        | Cloudflare-hosted A-record, TTL 60       |
| pfSense LAN   | `192.168.1.1:22222`              | LAN admin via anti-lockout rule          |
| LAN admin     | `192.168.1.131`                  | Pasha's Mac                              |
| Client user   | `olgatimoshevskaia`              | pfSense user (uid 2000), shell `/bin/tcsh` |
| Admin user    | `admin`                          | pfSense root                             |

Host key fingerprint (ed25519): `SHA256:iysgoGJhPPThFhj51DfJmGMmSC/Q4I7LAybLJ31fteI`

## pfSense state

- Version: pfSense 2.8.1-RELEASE on FreeBSD 15.0-CURRENT.
- Interfaces: `igb0` = WAN, `igb1` = LAN, `igb2` = OPT1 (172.31.255.1/24).
- `sshd_config`:
  - `Port 22222`
  - `PermitRootLogin yes`
  - `PasswordAuthentication no`
  - `UsePAM no`
  - → key-only auth via `authorized_keys`.

## Changes applied on 2026-05-28

### 1. WAN firewall rule (added via `pfSsh.php`)

```
pass in quick on igb0 inet proto tcp from any to 173.77.254.243 port = 22222
  label "USER_RULE: SSH (22222) from WAN - added via API"
```

Source = any (per user choice; consider tightening to Olga's public IP later).

### 2. Olga's user provisioning

- Authorized key (already present in pfSense User Manager):
  `ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMOH1Tl4At42ihChSJZBEGfBq1a9ngoB/c4UFzMCePMJ pfsense-tunnel`
- Privilege: **`user-ssh-tunnel`** (swapped from `user-shell-access` via pfUsers on 2026-05-29 — see [pfUsers section](#native-macos-app--pfusers-pfusers)). pfSense's `derive_shell` ladder maps this to `/usr/local/sbin/ssh_tunnel_shell`, so port forwarding (`-D`, `-L`, `-R`) works but interactive `exec` is blocked.
- System account provisioned via `local_user_set()`:
  - `/etc/passwd` entry, shell `/usr/local/sbin/ssh_tunnel_shell`
  - Home `/home/olgatimoshevskaia` with `.ssh/authorized_keys`
- No Web UI / OS password: pfUsers writes a random throwaway bcrypt-hash on creation and immediately drops the plaintext. pfSense's web UI login is mathematically impossible for tunnel users.

### 3. Cloudflare DDNS

- Zone: `celestialtech.io` (id `22e6b3f175e078c6aefc14c2d7d313c5`)
- Hostname: `aurora.celestialtech.io` (A-record id `36fee5206b9452a648a624288c94e121`, TTL 60, proxy off)
- Cloudflare API token: stored in `.env` (`CLOUDFLARE_API_TOKEN`), `Zone:DNS:Edit` scope, zone-restricted.
- pfSense DDNS client: provider `cloudflare`, monitors WAN interface, will update the A-record when WAN IP changes.
- Old NoIP entry (`pin3.hopto.org`) disabled in pfSense config but not deleted.

Client command now uses the hostname:

```
ssh -p 22222 -D 1080 -C -N olgatimoshevskaia@aurora.celestialtech.io
```

### 4. Config backup

Pre-change backup at:
`/cf/conf/backup/config-pre-ssh-wan-rule-20260528-144533.xml`

Rollback: restore via *Diagnostics → Backup & Restore → Config History* in the web UI, or `cp` it over `/cf/conf/config.xml` and run `/etc/rc.reload_all`.

## Olga's client

Command:

```
ssh -p 22222 -D 1080 -C -N olgatimoshevskaia@aurora.celestialtech.io
```

Flags: `-D 1080` = SOCKS5 on local 1080. `-C` = compression. `-N` = no remote command. Add `-v` for verbose, omit `-q` so host-key prompt is interactive on first connect.

The hostname `aurora.celestialtech.io` is kept pointed at the current WAN IP by Cloudflare DDNS (see *Changes applied → Cloudflare DDNS*). If you ever need to bypass DNS for testing, the bare IP at time of writing is `173.77.254.243`.

**Common typo:** `olgatimoshev**k**aia` (wrong) vs `olgatimoshev**sk**aia` (correct).

### Firefox setup (Settings → Network → Connection Settings)

- Manual proxy configuration
- SOCKS Host: `127.0.0.1` Port: `1080`
- SOCKS v5
- ✅ Proxy DNS when using SOCKS v5

Test: `https://ifconfig.me` should return the current WAN IP of pfSense (currently `173.77.254.243`; equivalent to `dig +short aurora.celestialtech.io @1.1.1.1`).

### Known issue: IPv6 port 1080 conflict

On Olga's Mac, `bind [::1]:1080: Address already in use` — some other process is bound to IPv6 localhost:1080. SSH falls back to IPv4 (`127.0.0.1:1080`), so the tunnel works. If Firefox prefers IPv6 and hits the rogue process instead, find it:

```
lsof -iTCP:1080 -sTCP:LISTEN
```

Kill it, re-run the SSH command.

## Diagnostics performed (path to resolution)

1. `nc -zv 173.77.254.243 22222` from LAN → succeeded (anti-lockout matched).
2. `ssh -vvv … olgatimoshevkaia@…` from Olga → hung at "Local version string …", then variously RST'd or timed out.
3. Root cause: no WAN firewall rule for port 22222; user had no shell-access privilege; (and username typo).

## Useful one-liners

```bash
# From LAN, on pfSense, list pf rules touching 22222
ssh -p 22222 admin@192.168.1.1 'pfctl -sr | grep 22222'

# List all pfSense users + their authorized keys
ssh -p 22222 admin@192.168.1.1 '/usr/local/sbin/pfSsh.php <<EOF
require_once("config.inc"); global \$config;
foreach (\$config["system"]["user"] as \$u) {
  echo \$u["name"]."\n";
  if (!empty(\$u["authorizedkeys"])) echo "  ".base64_decode(\$u["authorizedkeys"])."\n";
}
exec
exit
EOF'

# Verify tunnel from Olga's Mac (in another terminal while ssh -D 1080 runs)
curl --socks5-hostname localhost:1080 https://ifconfig.me

# Confirm DDNS hostname currently resolves to pfSense WAN
dig +short aurora.celestialtech.io @1.1.1.1

# Force pfSense to push current WAN IP to Cloudflare (run on pfSense)
ssh -p 22222 admin@192.168.1.1 'rm -f /var/db/dyndns_*.cache && /usr/local/bin/php-cgi -q /etc/rc.dyndns.update; tail -20 /var/log/system.log | grep -i dyndns'

# Inspect / rotate Cloudflare A-record via API (token from .env)
source ~/RustRoverProjects/belka_tunnel/.env
curl -s -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
  "https://api.cloudflare.com/client/v4/zones/$CLOUDFLARE_ZONE_ID/dns_records/$DDNS_RECORD_ID" | jq .result
```

## Native macOS app (`app/`)

A self-contained menu-bar app in Rust that maintains the tunnel without depending on the system `ssh` binary. Same external behaviour as the SSH command line, but persistent and visible. It exposes the tunnel to local clients two ways at once: a **SOCKS5** proxy and an **HTTP/HTTPS** forward proxy (see *Proxy front-ends* below).

### What it is

- Source: `app/` (Cargo project, edition 2021)
- Built artefact: `app/dist/BelkaTunnel.app` (menu-bar-only, `LSUIElement=true` — no Dock icon)
- Identifier: `io.celestialtech.BelkaTunnel`
- Config: `~/Library/Application Support/io.celestialtech.BelkaTunnel/config.json` (auto-created from defaults on first launch; daemon auto-restarts when this file changes on disk). Each profile carries an `ssh`, a `socks`, an `http`, and a `reconnect` block. The `http` block is `#[serde(default)]`, so configs written before the HTTP proxy existed upgrade in place to `{enabled: true, listen_addr: "0.0.0.0", listen_port: 8080}` on next load.
- Logs: `~/Library/Application Support/io.celestialtech.BelkaTunnel/logs/belka_tunnel.log`

### Architecture

- `russh` 0.45 for native SSH (no `ssh` subprocess; loads `~/.ssh/id_ed25519` directly).
- Two protocol-agnostic SSH-forwarding primitives in `src/tunnel.rs` do the actual work: `open_channel(host, port)` opens a `direct-tcpip` channel on the SSH session, and `bridge(tcp, channel)` pumps bytes both directions with proper half-close. Both proxy front-ends call these verbatim — only the per-connection request *parsing* differs.
- Hand-written SOCKS5 server in `src/socks.rs` — parses the SOCKS5 greeting + CONNECT request, then `open_channel` + `bridge`.
- Hand-written HTTP/HTTPS forward proxy in `src/http.rs` — see *Proxy front-ends* below.
- `tunnel::run_forever` is a reconnect loop with exponential backoff (1s → 2s → 4s … capped at `max_backoff_secs`); a watchdog task polls `Handle::is_closed()` every second and signals a shared `dead` `Notify` when the SSH session dies, tearing down both listeners so the loop trips fast on server-side disconnects.
- Tokio multi-thread runtime owns the tunnel + both proxy servers; main thread runs the `tao` event loop with `tray-icon` (required by macOS for menu-bar integration). A bridge thread forwards `Status` changes from a `watch` channel into `UserEvent` notifications so the menu-bar title can react to state.
- Menu-bar title: `●` Connected · `○` Connecting · `✕` Disconnected. Menu includes profile header, status row, SOCKS5 endpoints + copy, **HTTP-proxy-enabled toggle + HTTP endpoints + copy**, Listen-on-all-interfaces toggle (SOCKS listen address; the HTTP proxy has its own independent listen-address control in the GUI editor), Edit Configuration (opens GUI subprocess), Browse via tunnel (Firefox), Firefox install/uninstall, About, Quit. A file-system watcher on `config.json` self-restarts the daemon on save, so edits in the GUI editor apply without a manual click.

### Proxy front-ends (SOCKS5 + HTTP/HTTPS)

The SSH tunnel is the engine; SOCKS5 and HTTP are two thin, independent adapters onto it (not chained — HTTP does **not** convert to SOCKS5). Each parses the client's "where to" request and hands `(host, port)` to the same `tunnel::open_channel` + `tunnel::bridge`. The HTTP proxy exists for clients that only speak HTTP proxy and can't use SOCKS: anything honoring `http_proxy`/`https_proxy` (curl, git, npm, wget), and macOS's system *Web Proxy (HTTP)* / *Secure Web Proxy (HTTPS)* fields.

- **Separate ports.** SOCKS5 defaults to `0.0.0.0:1080`, HTTP to `0.0.0.0:8080`. Independent listeners; `validate()` rejects a same-addr+port collision at load.
- **No auth** on either proxy — open on the LAN, symmetric posture. The real perimeter is the LAN + the SSH tunnel, not a proxy password.
- **`src/http.rs` handler semantics** (mirrors go-gost's HTTP *handler*, the only piece of go-gost this borrows):
  - `CONNECT host:port` → `open_channel`, reply `200 Connection Established`, then `bridge` raw bytes. This is how **HTTPS** rides through: the client runs its own TLS end-to-end over the tunneled stream. **Not** a TLS-terminating/MITM proxy — we never see plaintext.
  - Absolute-form `GET http://host[:port]/path` (plain HTTP) → rewrite the request line to origin-form (`GET /path`), strip `Proxy-Connection`/`Proxy-Authorization`, force `Connection: close`, synthesize `Host` if absent, then `open_channel` + write head + `bridge`.
  - Errors: unparseable/origin-form-to-proxy → `400`; `open_channel` failure → `502`. Head read is bounded by a 10 s timeout + 64 KiB cap.
  - The pure `parse_request(&[u8]) -> Target` fn is the unit-testable core (same pattern as `socks.rs`'s `read_socks5_request`).
  - **Known limitation:** plain-HTTP keep-alive is disabled via the forced `Connection: close` (one request = one dial+bridge — avoids a full HTTP/1.1 message-framing parser). HTTPS via CONNECT is unaffected and is the common path.
- **Failure isolation:** the HTTP proxy runs as a sibling task off the shared `dead` signal; SOCKS5 stays the session-lifetime driver. If the HTTP listener can't bind (port occupied/misconfigured) it logs and parks instead of propagating — a bad HTTP port can never take SOCKS5 down.
- **Firefox** is still pointed at SOCKS5 (with proxy-DNS) by the menu's Firefox policy writer; the HTTP proxy is for non-browser/CLI/system-proxy clients.

Client usage examples (with the daemon connected):

```bash
# HTTPS through the HTTP proxy (CONNECT) — returns the pfSense WAN IP:
curl -x http://127.0.0.1:8080 https://ifconfig.me
https_proxy=http://127.0.0.1:8080 curl https://ifconfig.me
# plain HTTP forward:
curl -x http://127.0.0.1:8080 http://ifconfig.me
# SOCKS5 (unchanged):
curl --socks5-hostname 127.0.0.1:1080 https://ifconfig.me
```

### Build / test / harness — the `bt` CLI

All build/test/verify/bench operations live in a uv-managed Python CLI at
`tools/`. Run `./bt` (from the repo root) for the full list. Highlights:

```
./bt bootstrap          install git hooks + verify toolchain
./bt bundle             cargo build --release + assemble dist/BelkaTunnel.app
./bt dmg                build dist/BelkaTunnel-<version>.dmg (no bg image)
./bt verify bundle      Info.plist + codesign + arch + icon
./bt verify policies    policies.json schema + Locked/AppAutoUpdate invariants
./bt verify dmg         mount the latest DMG + check contents
./bt smoke-test         launch + verify menu + curl through tunnel
./bt bench              throughput / latency / connection-rate / concurrency
./bt ci                 full pipeline
```

Git hooks (`.githooks/`, installed by `bt bootstrap`):
- `pre-commit` → fmt-check + clippy + test
- `pre-push` → precommit + bundle + verify bundle/policies (caught the
  missing `DisableAppUpdate` policy field in commit `24972a7` before it shipped)

The low-level shell script `app/build-app.sh` still exists and is what
`bt bundle` calls under the hood. `Cargo.toml` declares `recursion_limit = 512`
because the policies.json generator uses a large `serde_json::json!{...}`.

### DMG installer

`./bt dmg` produces `app/dist/BelkaTunnel-<version>.dmg`:
- No background image. macOS Finder forces icon labels to render in **black**
  whenever a background image is set — confirmed empirically and via the
  `create-dmg` / `dmgbuild` community write-ups. Without one, label colour
  follows the user's system theme (white in Dark, black in Light), which is
  what we want. The `WindowAppearance=NSAppearanceNameDarkAqua` `.DS_Store`
  injection that lived here briefly is dead code per the same investigation
  and only kept for historical reference; deleting it is fine.
- `BelkaTunnel.app` at (200, 240); `/Applications` symlink at (600, 240).
- Window chrome hidden, volume name `БелкаТуннель`, compressed UDZO.
- ~7.5 MB output, built via `dmgbuild` (deterministic, no AppleScript).

### Icons

Two .icns files live under `app/assets/`:
- **`AppIcon.icns`** — the official БелкаТуннель design that ships inside the .app bundle. Hand-authored; treat as the source of truth and don't regenerate.
- **`DmgIcon.icns`** — papal-purple (#5D2D8B) rounded-square with white Cyrillic "БТ" centred, rendered in **NewCenturySchoolbookC-Bold** (`~/Library/Fonts/`). This is the icon Finder shows on the .dmg file itself, plus the pfUsers DMG (reuses the same brand mark).

The DMG icon is generated by `tools/make_app_icon.py`:
```
uv run --project tools python tools/make_app_icon.py
```
Falls back to system fonts (SF / Helvetica / Arial Unicode) if NewCenturySchoolbookC isn't installed, but the canonical look is the serif Cyrillic.

dmgbuild's `icon = …` setting silently no-ops in this environment (the resource fork stays empty); `tools/bt/dmg.py` calls `NSWorkspace.setIcon:forFile:options:` after the DMG is written to actually embed the icon. Confirmed via `xattr -l <dmg>` showing `com.apple.ResourceFork` present after the call.

### CI/CD

Two GitHub Actions workflows live under `.github/workflows/`:

- **`ci.yml`** — runs on every push to main + every PR. macos-14 runner. Caches `target/` + `~/.cargo/`. Steps: `bt precommit` (fmt-check + clippy + workspace tests), `bt bundle` + `bt verify bundle`, `bt bundle-pfusers` + `bt verify pfusers`. The bundle steps catch Info.plist / codesign / dmgbuild settings regressions that pure unit tests can't see.
- **`release.yml`** — triggered by `git push origin vX.Y.Z`. Builds the universal binary, bundles both apps, verifies both .app and both .dmg, optionally codesigns + notarizes if `secrets.CERTIFICATES_P12` + `SIGN_IDENTITY` + `APPLE_ID` / `APPLE_TEAM_ID` / `APPLE_APP_PASSWORD` are set, then uploads both DMGs to a GitHub release via `softprops/action-gh-release@v2`. Without the secrets the bundles stay ad-hoc-signed and the release ships with a Gatekeeper warning — still functional, just not notarized.

Local release flow without CI: `./bt bundle && ./bt dmg && ./bt bundle-pfusers && ./bt dmg-pfusers && gh release create vX.Y.Z app/dist/BelkaTunnel-X.Y.Z.dmg pfusers/dist/pfUsers-X.Y.Z.dmg`. The release workflow does the same steps; the CI workflow is what catches issues before tagging.

### Run

```
open app/dist/BelkaTunnel.app
```

Or for live logs to stderr:

```
RUST_LOG=info app/dist/BelkaTunnel.app/Contents/MacOS/belka_tunnel
```

There is **no `--config` flag** — the binary always loads `ConfigFile::default_path()` (the Application Support `config.json`). The only CLI args are `--gui` (launch the config editor subprocess) and `--about`. To run against a throwaway config during development, edit the real `config.json` (the file-system watcher will self-restart the daemon on save), or temporarily back it up and swap it. The `RUST_LOG=info …/Contents/MacOS/belka_tunnel` invocation above runs the same daemon in the foreground with logs on stderr.

### Defaults baked in

Match the working CLI command: `olgatimoshevskaia@aurora.celestialtech.io:22222`, key `~/.ssh/id_ed25519`, SOCKS5 at `0.0.0.0:1080`, **HTTP/HTTPS proxy at `0.0.0.0:8080`** (`enabled: true`), 30 s keepalive, exponential reconnect backoff. On first launch the daemon writes the active config to disk so it can be edited. (Listening on `0.0.0.0` makes both proxies reachable from the LAN; flip the menu's "Listen on all interfaces" off, or edit `listen_addr` to `127.0.0.1`, for loopback-only.)

### Known limitations / open items

- **Host key verification: trust-on-first-use** (commit `20b53ae`). First connect on a profile records the server's `SHA256:…` fingerprint into `host_key_fingerprint` in `config.json`; every subsequent connect requires an exact match. Mismatch → `Status::Disconnected("host key mismatch — …")` plus a one-shot macOS notification. The GUI editor's SSH section shows the recorded fingerprint as read-only with a "Forget" button (use after a legitimate server-key rotation; the daemon TOFUs the new key on the next connect).
- **No autolaunch on login.** Add via launchd plist or macOS *Login Items* (Settings → General → Login Items → +).

## Native macOS app — pfUsers (`pfusers/`)

A separate windowed admin app for CRUDing pfSense tunnel users on the router. Built on the same Zed-inspired dark theme as БелкаТуннель (shared via the `belka-ui` workspace crate). Lives at `pfusers/` in this repo (the repo is now a Cargo workspace; see *Repo layout* below).

### What it is

- Source: `pfusers/` (Cargo workspace member, edition 2021)
- Built artefact: `pfusers/dist/pfUsers.app` (windowed app, **`LSUIElement` absent** — pfUsers IS a Dock app, unlike BelkaTunnel which is menu-bar only)
- Identifier: `io.celestialtech.pfUsers`
- Config: `~/Library/Application Support/io.celestialtech.pfUsers/config.json` (SSH endpoint, recorded host-key fingerprint, window size). Atomic temp-rename save.
- Logs: `~/Library/Application Support/io.celestialtech.pfUsers/logs/pfusers.<date>.log` (daily rotated, 7-day retention)
- Defaults: `admin@192.168.1.1:22222`, key `~/.ssh/id_ed25519`. Settings modal exposes a Browse button to pick a different key + a Forget button to re-TOFU after a server key rotation.

### Repo layout (workspace)

```
belka_tunnel/
├── Cargo.toml                  # workspace root: members = [app, crates/belka-ui, pfusers]
├── app/                        # БелкаТуннель (menu-bar tunnel daemon)
├── crates/belka-ui/            # shared theme + form helpers (extracted from app/src/gui.rs)
├── pfusers/                    # pfUsers (windowed admin app, this section)
└── tools/                      # Python bt CLI — knows about both apps
```

`bt precommit`, `bt prepush`, `bt bundle` still target БелкаТуннель. `bt bundle-pfusers`, `bt verify pfusers` target pfUsers.

### Architecture

- **No PHP on the wire.** Reads `/cf/conf/config.xml` over SSH (`cat`), parses with `quick-xml`, mutates only the `<system>/<user>` and `<system>/<group>` subtrees, writes back atomically via `cat > tmp && mv`. Side effects (`pw useradd / usermod / userdel`, `mkdir`/`chmod`/`chown` of `/home/<user>/.ssh/authorized_keys`) go through SSH `exec`.
- Same TOFU host-key verification as БелкаТуннель: first connect records the `SHA256:…` fingerprint; mismatch refuses with rollback message; "Forget" button in Settings clears it for a controlled re-TOFU.
- Password hashing for the (mandatory) `<bcrypt-hash>` field: `bcrypt` crate with cost=10, matches pfSense's default. Tunnel users never log into the web UI so we generate the value from `/dev/urandom`, hash it, and discard the plaintext — see *No password story* below.

### Byte-precise mutation + four-layer safety wrapper

The single most important property: **bytes outside the changed `<user>` block survive verbatim**. We never re-serialise the full document. `apply_mutation` parses the XML to find byte offsets, splices new bytes only for the changed region, and writes:

```
result = original[..start] + new_bytes + original[end..]
```

Regions outside the splice are literally the same `&[u8]` as input — there's no Writer re-emission to disagree with pfSense's parser.

Every write is wrapped by `write_with_safety`:

1. `cp /cf/conf/config.xml /cf/conf/backup/config-<unix-ts>-pfusers.xml` — pfSense's Web UI Config History page enumerates that directory, so our snapshots show up alongside its own and the admin can revert through normal flows.
2. Atomic write of the mutated XML to `config.xml`.
3. Re-read, parse with our Rust parser, run the per-op assertion closure (new user found, descr round-tripped, count incremented, ...).
4. Invoke pfSense's own PHP parser once: `php -r 'require_once("config.inc"); parse_config(true); echo count($config["system"]["user"]);'` and compare to the expected post-mutation count. **This is the only PHP call in pfusers** and exists purely as a parser-agreement check.
5. On any failure in 2/3/4: `cp` the snapshot back. Router sees the exact bytes it had before the call.

Verified end-to-end against the live router (commit `46f8227`): olga's `user-shell-access` → `user-ssh-tunnel` swap landed cleanly through the full pipeline. The PHP-verify command must NOT pass `-d include_path=…` — pfSense's default `include_path` includes `/usr/local/share/pear` where `Net/IPv6.php` lives, and our earlier override stripped that, breaking `require_once`.

### What the GUI exposes (scope discipline)

The detail form for a user has, from top to bottom: **Identity** (username read-only, full name editable), **Privileges** (toggles from a curated 14-item subset of pfSense's ~251 priv strings), **Authorized SSH keys** (multiline textarea, one OpenSSH-format key per line), **Danger zone** (delete, with type-username confirm modal). Save Changes is greyed unless something differs from the server state (priv_list compared as a `HashSet`, so toggle order doesn't matter).

The Add User dialog asks for: username, full name (optional), initial SSH key (optional). That's it.

**Removed by design** (each closing a UX hole):
- Group memberships card — only `all` (implicit) and `admins` (root, page-all) exist on this router; non-admin users belong in neither beyond `all`. Engine still supports group edits via `Mutation::{AddGroupMember, RemoveGroupMembership}` if a future deployment introduces real custom groups.
- Password / Reset password card — pfSense uses `<bcrypt-hash>` only for Web UI login; Pasha's sshd is `PasswordAuthentication=no`, and tunnel users don't touch the web UI. `pfsense::set_password` remains in the engine.
- Add User priv toggles (`Shell access`, `Web UI (page-all)`) — every user pfUsers creates is by definition a tunnel user. `spawn_add_user` pins `priv_list` to `["user-ssh-tunnel"]`.
- Add User password field — tunnel users have no password story (next bullet).
- Admin row (uid 0) in the sidebar — touching uid 0 from here is footgun territory. Filter is GUI-side, not engine-side, so the PHP-verify count match still works.

### No password story

pfSense's `local_user_set_password` bails on an empty argument, so we can't simply skip the hash. `spawn_add_user` instead generates a 32-byte hex secret from `/dev/urandom`, passes it to `pfsense::add_user` (which bcrypts at cost=10), and `drop()`s the plaintext immediately. Nobody — including pfusers itself, including future maintainers — ever knows the value. The `<bcrypt-hash>` field in `config.xml` is effectively a random oracle: no input could plausibly hash to it, so any Web UI login attempt is mathematically guaranteed to fail.

`random_secret_hex` panics on `/dev/urandom` failure rather than falling back to a deterministic value — silently writing a predictable hash would be worse than crashing the spawn.

### Build / test / harness

```bash
./bt bundle-pfusers           # cargo build --release -p pfusers + assemble dist/pfUsers.app
./bt verify pfusers           # Info.plist keys, codesign, assert LSUIElement absent
cargo test --workspace        # 74 BelkaTunnel + 20 pfusers = 94 unit tests
cargo test --release -p pfusers swap_olga_priv_live -- --ignored --nocapture
                              # live-router integration test against admin@192.168.1.1
```

The live test (`#[ignore]` by default) connects to the real router, asserts olga's current priv, swaps it to `user-ssh-tunnel`, asserts the post-write state. Useful as the canonical "does the four-layer wrapper still work end-to-end?" check after non-trivial pfsense.rs edits.

### Run

```
open pfusers/dist/pfUsers.app
```

Or for live logs to stderr:

```
RUST_LOG=info pfusers/dist/pfUsers.app/Contents/MacOS/pfusers
```

### Known limitations / open items

- **No DMG packaging yet.** `bt dmg-pfusers` not implemented; only the .app exists in `pfusers/dist/`.
- **No notarization wiring** — `SIGN_IDENTITY` env var is honoured by `pfusers/build-app.sh` but there's no `bt notarize-pfusers` command.
- **No Add-or-Edit groups UI.** If a real `vpn-users` group ever materialises on the router, the GUI needs the membership card back (engine support remains).
- **Single-router scope.** pfusers connects to one endpoint at a time. Multi-router support would mean a profile concept in `config.json`, similar to БелкаТуннель.

## Olga's `~/.ssh/config` (recommended)

Saves typing and standardizes flags. Then she can just run `ssh wan-proxy`.

```
Host wan-proxy
    HostName aurora.celestialtech.io
    Port 22222
    User olgatimoshevskaia
    IdentityFile ~/.ssh/id_ed25519
    IdentitiesOnly yes
    DynamicForward 1080
    Compression yes
    SessionType none           # equivalent to -N
    ServerAliveInterval 30
    ServerAliveCountMax 3
    ExitOnForwardFailure yes
```

## Keeping the tunnel up automatically (Olga's Mac)

### Option 1 — `autossh` (simplest)

```
brew install autossh
autossh -M 0 -f -N -D 1080 -o "ServerAliveInterval 30" -o "ServerAliveCountMax 3" \
    -p 22222 olgatimoshevskaia@aurora.celestialtech.io
```

### Option 2 — launchd agent (survives reboots, no Homebrew)

`~/Library/LaunchAgents/com.olga.ssh-proxy.plist`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>com.olga.ssh-proxy</string>
    <key>ProgramArguments</key>
    <array>
        <string>/usr/bin/ssh</string>
        <string>-N</string>
        <string>-D</string><string>1080</string>
        <string>-o</string><string>ServerAliveInterval=30</string>
        <string>-o</string><string>ExitOnForwardFailure=yes</string>
        <string>wan-proxy</string>
    </array>
    <key>KeepAlive</key><true/>
    <key>RunAtLoad</key><true/>
    <key>StandardOutPath</key><string>/tmp/ssh-proxy.log</string>
    <key>StandardErrorPath</key><string>/tmp/ssh-proxy.err</string>
</dict>
</plist>
```

Load: `launchctl load ~/Library/LaunchAgents/com.olga.ssh-proxy.plist`
Unload: `launchctl unload ~/Library/LaunchAgents/com.olga.ssh-proxy.plist`
Logs: `tail -f /tmp/ssh-proxy.err`

## Hardening — tunnel-only access

**The original sshd `Match` block approach is superseded.** As of 2026-05-29 olga has the pfSense priv `user-ssh-tunnel` (set via pfUsers; see the pfUsers section below). That priv causes pfSense's `local_user_set` to assign `/usr/local/sbin/ssh_tunnel_shell` as her login shell, which permits TCP forwarding (so `-D 1080` SOCKS5 works) but refuses any `exec` request. No `/etc/ssh/sshd_config` edits required — and crucially no need to keep the sshd_config patch in sync across pfSense regenerations.

If you ever need to apply the same hardening to a new user, the canonical move is to set their priv to `user-ssh-tunnel` via pfUsers (or the pfSense Web UI under System → User Manager). The sshd_config Match block below is kept for historical reference only:

<details><summary>(historical) Match block approach</summary>

```
Match User olgatimoshevskaia
    PermitTTY no
    X11Forwarding no
    AllowAgentForwarding no
    AllowTcpForwarding yes
    PermitOpen any
    ForceCommand /sbin/nologin
```

Issue: pfSense regenerates `/etc/ssh/sshd_config` from `config.xml` on reload, so any direct edit gets lost. The priv-based approach embeds the same intent into config.xml itself.

</details>

## Health check (Olga's Mac)

```bash
#!/bin/bash
# ssh-proxy-check.sh — compares tunnel exit IP against current DDNS A-record
EXPECTED=$(dig +short aurora.celestialtech.io @1.1.1.1 | tail -1)
ACTUAL=$(curl -s --max-time 5 --socks5-hostname localhost:1080 https://ifconfig.me)
if [[ -n "$EXPECTED" && "$ACTUAL" == "$EXPECTED" ]]; then
    echo "OK: tunnel up, exit IP $ACTUAL == DDNS $EXPECTED"
    exit 0
else
    echo "FAIL: exit IP '$ACTUAL', DDNS '$EXPECTED'"
    exit 1
fi
```

## Troubleshooting matrix

| Symptom (client side)                                | Likely cause                                  | Fix                                                                |
|-------------------------------------------------------|-----------------------------------------------|--------------------------------------------------------------------|
| `Connection timed out during banner exchange`         | No WAN allow rule, or upstream drop           | Verify rule `pfctl -sr \| grep 22222`                              |
| `Connection reset by peer` during KEX                 | sshguard ban, or rate-limit                   | `pfctl -t sshguard -T show`; delete the IP if listed               |
| `Host key verification failed`                        | known_hosts entry for hostname/IP missing or mismatched | `ssh-keyscan -p 22222 -t ed25519 aurora.celestialtech.io >> ~/.ssh/known_hosts` |
| `Permission denied (publickey)`                       | Key not in pfSense user's authorized_keys, or user lacks `user-shell-access` privilege | Re-add key in User Manager; grant shell access |
| `bind [::1]:1080: Address already in use`             | Another local process owns IPv6 port 1080    | `lsof -iTCP:1080 -sTCP:LISTEN`, kill or use different port         |
| Firefox "proxy refusing connections"                  | SSH tunnel not actually running               | `lsof -iTCP:1080 -sTCP:LISTEN`; start ssh again                    |
| Browser works but `ifconfig.me` shows real IP         | Browser bypassing proxy, or DNS leak          | Confirm "Proxy DNS when using SOCKS v5" is checked in Firefox      |
| `Could not resolve hostname aurora.celestialtech.io`  | Cloudflare A-record gone / token revoked / pfSense DDNS broken | `dig +short aurora.celestialtech.io @1.1.1.1`; if empty, check Cloudflare DNS UI and pfSense *Services → Dynamic DNS* |
| DDNS works but exit IP differs from `aurora` record   | pfSense hasn't pushed update yet after WAN IP change | Force update with the one-liner in *Useful one-liners* section     |

## pfSense log/diagnostic locations

| What                          | Where                                                   |
|-------------------------------|---------------------------------------------------------|
| sshd auth attempts            | `/var/log/auth.log` (also `sshd.log`)                  |
| Filter blocks                 | `clog /var/log/filter.log`                              |
| Web UI logins                 | `/var/log/system.log` (php-fpm entries)                |
| sshguard activity             | grep `sshguard` in `/var/log/auth.log`                  |
| Current pf states             | `pfctl -ss \| grep 22222`                              |
| Config backups (auto + manual)| `/cf/conf/backup/`                                      |
| DDNS update activity          | `clog /var/log/system.log \| grep -i dyndns`            |
| DDNS update cache             | `/var/db/dyndns_*.cache` (delete to force a re-push)    |

## Privacy / leak notes

- **DNS leaks**: in Firefox enable "Proxy DNS when using SOCKS v5", or visible Mac-level DNS will bypass the tunnel.
- **WebRTC** can leak real LAN/public IP via STUN even with a SOCKS proxy. For Firefox: `about:config` → `media.peerconnection.enabled` → `false`.
- **System proxy mode** routes *all* Mac traffic (including Sparkle updates, Spotify, etc.) through the tunnel. Per-app via Firefox's built-in proxy or Chrome+FoxyProxy is cleaner.
- **Browser fingerprinting** is unaffected by SOCKS — IP is just one signal.

## Secrets

- Cloudflare API token (`CLOUDFLARE_API_TOKEN`) lives in `.env` (mode 600, `.gitignore`d). It was pasted in chat once; rotate it at https://dash.cloudflare.com/profile/api-tokens once you're confident the setup is stable, and overwrite the value in `.env`.
- The token's scope is `Zone:DNS:Edit` on `celestialtech.io` only — even leaked, the blast radius is bounded to DNS edits on this one zone.

## Open follow-ups

- Tighten WAN rule to Olga's specific source IP (currently `any`).
- ~~Apply the tunnel-only Match block above; verify it survives a pfSense config reload.~~ Superseded by olga's `user-ssh-tunnel` priv via pfUsers (commit `46f8227`).
- Resolve the local `::1:1080` conflict on Olga's Mac.
- Decide whether to enable `LogLevel VERBOSE` in sshd_config for better audit trail.
- Consider a second user / key for redundancy in case Olga's key is rotated.
- Rotate the Cloudflare API token now that the setup is verified (see *Secrets* above).
- Remove the disabled NoIP entry from pfSense config once Cloudflare DDNS has run successfully through a real WAN-IP change.
- **pfUsers**: build a release DMG (`bt dmg-pfusers` not yet implemented).
- **pfUsers**: wire `bt notarize-pfusers` for distribution. `SIGN_IDENTITY` env var is honoured by `pfusers/build-app.sh` but no notarize step exists.
