# БелкаТуннель

Setting up an SSH-based SOCKS5 proxy that routes Olga's traffic through Pasha's pfSense WAN IP. The native macOS app under `app/` is `БелкаТуннель`; the working directory is still `~/PycharmProjects/ssh-proxy-wan/` (unrenamed to keep paths stable).

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
- Privilege added: `user-shell-access` (System: Shell account access).
- System account provisioned via `local_user_set()`:
  - `/etc/passwd` entry, shell `/bin/tcsh`
  - Home `/home/olgatimoshevskaia` with `.ssh/authorized_keys`

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
source ~/PycharmProjects/ssh-proxy-wan/.env
curl -s -H "Authorization: Bearer $CLOUDFLARE_API_TOKEN" \
  "https://api.cloudflare.com/client/v4/zones/$CLOUDFLARE_ZONE_ID/dns_records/$DDNS_RECORD_ID" | jq .result
```

## Native macOS app (`app/`)

A self-contained menu-bar app in Rust that maintains the tunnel without depending on the system `ssh` binary. Same external behaviour as the SSH command line, but persistent and visible.

### What it is

- Source: `app/` (Cargo project, edition 2021)
- Built artefact: `app/dist/BelkaTunnel.app` (menu-bar-only, `LSUIElement=true` — no Dock icon)
- Identifier: `io.celestialtech.BelkaTunnel`
- Config: `~/Library/Application Support/io.celestialtech.BelkaTunnel/config.toml` (auto-created from defaults on first launch)
- Logs: `~/Library/Application Support/io.celestialtech.BelkaTunnel/logs/proxy-tunnel.log`

### Architecture

- `russh` 0.45 for native SSH (no `ssh` subprocess; loads `~/.ssh/id_ed25519` directly).
- Hand-written SOCKS5 server in `src/socks.rs` — accepts CONNECT requests, opens a `direct-tcpip` channel on the SSH session, bridges TCP ↔ channel with `tokio::select!`.
- `tunnel::run_forever` is a reconnect loop with exponential backoff (1s → 2s → 4s … capped at `max_backoff_secs`); a watchdog task polls `Handle::is_closed()` every second and tears down the SOCKS listener when the SSH session dies, so the loop trips fast on server-side disconnects.
- Tokio multi-thread runtime owns the tunnel + SOCKS server; main thread runs the `tao` event loop with `tray-icon` (required by macOS for menu-bar integration). A bridge thread forwards `Status` changes from a `watch` channel into `UserEvent` notifications so the menu-bar title can react to state.
- Menu-bar title: `●` Connected · `○` Connecting · `✕` Disconnected. Menu shows host:port + status + Quit.

### Build

```
cd app
./build-app.sh
```

This runs `cargo build --release`, assembles `dist/BelkaTunnel.app`, ad-hoc signs it (`codesign --sign -`, required for recent macOS to launch unsigned bundles), and reregisters it with LaunchServices.

### Run

```
open app/dist/BelkaTunnel.app
```

Or for live logs to stderr:

```
RUST_LOG=info app/dist/BelkaTunnel.app/Contents/MacOS/proxy-tunnel
```

CLI override during development:

```
app/target/release/proxy-tunnel --config some-test.toml
```

### Defaults baked in

Match the working CLI command: `olgatimoshevskaia@aurora.celestialtech.io:22222`, key `~/.ssh/id_ed25519`, SOCKS at `127.0.0.1:1080`, 30 s keepalive, exponential reconnect backoff. On first launch the daemon writes the active config to disk so it can be edited.

### Known limitations / open items

- **Host key verification is permissive** — accepts whatever the server presents. Acceptable for first iteration (server identity is bound to the DDNS hostname we control); before redistribution, persist + verify against a known fingerprint.
- **No menu-bar interactivity** beyond status indicator + Quit. Future: "Reconnect now", "Open config", "Open logs", "About" with key fingerprint + version.
- **No autolaunch on login.** Add via launchd plist or macOS *Login Items* (Settings → General → Login Items → +).

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

## Hardening — tunnel-only Match block

To prevent Olga (or anyone with her key) from getting an interactive shell while keeping the SOCKS proxy working, append to pfSense's `/etc/ssh/sshd_config` (note: pfSense regenerates this file from `config.xml` on reload — see [pfSense ssh customization](#) caveat; you may need to use the *System → Advanced → Admin Access → Secure Shell* custom options field, or persist via a tunable):

```
Match User olgatimoshevskaia
    PermitTTY no
    X11Forwarding no
    AllowAgentForwarding no
    AllowTcpForwarding yes
    PermitOpen any
    ForceCommand /sbin/nologin
```

Trade-off: with `PermitTTY no` + `ForceCommand /sbin/nologin` she can't shell in but `-D 1080` still works (port forwarding doesn't need a TTY).

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
- Apply the tunnel-only Match block above; verify it survives a pfSense config reload.
- Resolve the local `::1:1080` conflict on Olga's Mac.
- Decide whether to enable `LogLevel VERBOSE` in sshd_config for better audit trail.
- Consider a second user / key for redundancy in case Olga's key is rotated.
- Rotate the Cloudflare API token now that the setup is verified (see *Secrets* above).
- Remove the disabled NoIP entry from pfSense config once Cloudflare DDNS has run successfully through a real WAN-IP change.
