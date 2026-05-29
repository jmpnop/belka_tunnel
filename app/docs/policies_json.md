# Firefox enterprise policies (`policies.json`)

БелкаТуннель drops a `policies.json` file into Firefox's distribution folder
during install so the SOCKS5 proxy is enforced on every launch, lock-icon and all:

```
/Applications/Firefox.app/Contents/Resources/distribution/policies.json
```

Reference: <https://mozilla.github.io/policy-templates/>

This doc lists every policy field that's potentially useful for a tunneled-browser
deployment — what we already set, what we could add, and which ones to avoid.

---

## What we currently set

```json
{
  "policies": {
    "Proxy": {
      "Mode": "manual",
      "SOCKSProxy": "127.0.0.1:1081",
      "SOCKSVersion": 5,
      "UseProxyForDNS": true,
      "Locked": true
    },
    "DisableTelemetry": true,
    "DisableFirefoxStudies": true,
    "DontCheckDefaultBrowser": true,
    "OverrideFirstRunPage": "https://ifconfig.me/",
    "DisablePocket": true
  }
}
```

| Key                       | Why we set it                                                                                  |
|---------------------------|------------------------------------------------------------------------------------------------|
| `Proxy` + `Locked: true`  | Force every connection through the tunnel; user can't change it in Settings.                   |
| `DisableTelemetry`        | Stop the Firefox health-report / data-reporting pings; they'd leak through the tunnel anyway.  |
| `DisableFirefoxStudies`   | No Normandy/SHIELD experiments.                                                                |
| `DontCheckDefaultBrowser` | No "Make Firefox default?" prompt on every launch.                                             |
| `OverrideFirstRunPage`    | First launch goes to ifconfig.me so the user can immediately see the tunnel IP.                |
| `DisablePocket`           | Less UI clutter; Pocket pings are a known tracker signal.                                      |

---

## Other proxy-related policies

### `Proxy.Passthrough`
Comma-separated list of hosts the proxy is bypassed for. Useful if you want
loopback/intranet hosts to skip the tunnel:
```json
"Passthrough": "<local>,127.0.0.1,localhost,192.168.1.0/24"
```

### `Proxy.AutoConfigURL`
Use a PAC file instead of fixed host/port. Lets you route some sites direct
and others through the tunnel. Mutually exclusive with `Mode: "manual"`.

### `DNSOverHTTPS`
Force DoH on or off. Currently we leave it default; if you want DNS to stay
inside the tunnel and you're using `UseProxyForDNS: true`, you probably want
DoH **off** here:
```json
"DNSOverHTTPS": { "Enabled": false, "Locked": true }
```

---

## Privacy hardening (recommended additions)

| Policy                                     | Effect                                                                                   |
|--------------------------------------------|------------------------------------------------------------------------------------------|
| `DisableFirefoxAccounts: true`             | No Mozilla account sign-in / Sync.                                                       |
| `DisableSecurityBypass: true`              | Users can't click through cert warnings or `about:blocked` interstitials.                |
| `EnableTrackingProtection: { "Value": true, "Locked": true }` | Force ETP on.                                                       |
| `DisablePasswordReveal: true`              | Hides the "show password" eye icon on input fields.                                      |
| `OfferToSaveLogins: false`                 | No password save prompts (no creds at rest on this Mac).                                 |
| `Cookies: { "Default": true, "RejectTracker": true }` | Block third-party trackers automatically.                                     |
| `WebRTCIPHandling: { "Mode": "disable_non_proxied_udp", "Locked": true }` | Plug WebRTC IP leaks — critical when using a proxy. |
| `HardwareAcceleration: false`              | Avoid GPU fingerprinting.                                                                |

### Example
```json
"WebRTCIPHandling": {
  "Mode": "disable_non_proxied_udp",
  "Locked": true
},
"EnableTrackingProtection": {
  "Value": true,
  "Locked": true,
  "Cryptomining": true,
  "Fingerprinting": true
}
```

`WebRTCIPHandling: "disable_non_proxied_udp"` is the policy-equivalent of the
`media.peerconnection.ice.default_address_only` pref we set in `user.js` — it
stops WebRTC from leaking your real IP via STUN even with the tunnel up.

---

## Update / channel control

| Policy                                          | Effect                                                          |
|-------------------------------------------------|-----------------------------------------------------------------|
| `AppAutoUpdate: false`                          | Stop background updater; we control updates via БелкаТуннель.   |
| `DisableAppUpdate: true`                        | Disable the entire update mechanism.                            |
| `ManualAppUpdateOnly: true`                     | Allow updates only via *Help → About Firefox*.                  |
| `BackgroundAppUpdate: false`                    | Updater service won't run when Firefox is closed.               |

Caveat: if you disable updates entirely, **you take on the responsibility of
shipping security patches**. Recommend leaving updates on (`AppAutoUpdate: true`).

---

## UI lockdown

| Policy                          | Effect                                                                  |
|---------------------------------|-------------------------------------------------------------------------|
| `BlockAboutConfig: true`        | Users can't open `about:config` (matches our intent of policy-only prefs). |
| `BlockAboutProfiles: true`      | Can't view/manage profiles from the URL bar.                            |
| `BlockAboutAddons: true`        | Can't go to the add-ons manager.                                        |
| `DisableDeveloperTools: true`   | Hides Web Inspector, Console, etc.                                      |
| `DisableMasterPasswordCreation: true` | No primary-password prompt.                                       |
| `DisableProfileImport: true`    | No import-from-other-browser wizard.                                    |
| `DisableProfileRefresh: true`   | No "Refresh Firefox" button.                                            |
| `DisableSetDesktopBackground: true` | Removes "Set as wallpaper" context-menu item.                        |

For our use case `BlockAboutConfig` makes sense (we don't want the user to
hand-roll prefs that bypass policies); the rest is taste.

---

## Bookmarks / homepage / first run

| Policy                          | Example                                                                  |
|---------------------------------|--------------------------------------------------------------------------|
| `Homepage`                      | `{ "URL": "https://ifconfig.me/", "Locked": true, "StartPage": "homepage" }` |
| `NewTabPage: false`             | Disable the activity-stream new-tab page.                                |
| `OverridePostUpdatePage`        | Where to land after Firefox auto-updates. `""` for none.                 |
| `Bookmarks`                     | Inject a custom bookmark list (array of `{Title, URL, Folder}`).         |
| `ManagedBookmarks`              | Same idea but read-only; appears in a separate "Managed bookmarks" menu. |

---

## Add-ons control

| Policy                                  | Effect                                                                |
|-----------------------------------------|-----------------------------------------------------------------------|
| `ExtensionSettings`                     | Per-add-on rules: install-source allowlist, blocked install, force-install, etc. |
| `ExtensionUpdate: false`                | Lock add-on versions.                                                 |
| `BlockedInstallMessage`                 | Custom message when a blocked add-on tries to install.                |

Example: force-install a custom add-on signed via your own AMO account:
```json
"ExtensionSettings": {
  "*": { "installation_mode": "blocked" },
  "belkatunnel@celestialtech.io": {
    "installation_mode": "force_installed",
    "install_url": "https://celestialtech.io/firefox/belkatunnel.xpi"
  }
}
```

Useful if you ever want to ship an extension for richer control (status indicator,
per-tab proxy toggle, etc.) — but it requires Mozilla-signed XPIs for Release
Firefox.

---

## Networking / privacy

| Policy                                                 | Effect                                            |
|--------------------------------------------------------|---------------------------------------------------|
| `Preferences`                                          | Override any pref by key. Per-pref `Status` can be `default | locked | user`. Use sparingly — `Proxy` policy is preferred. |
| `BlockedSitesPolicy`                                   | URL allow/block lists for the parental-controls layer. |
| `Cookies`                                              | Default behaviour + per-host allow/block.         |
| `LegacyProfiles: false`                                | One profile per install — avoids profile churn.   |
| `NetworkPrediction: false`                             | Disables DNS prefetching, link prefetch.          |
| `SearchEngines`                                        | Restrict / pin search engines.                    |
| `SSLVersionMin`                                        | Set a TLS floor.                                  |
| `OfferToSaveLoginsDefault: false` + `OfferToSaveLogins: false` | No password manager prompts.               |

The `Preferences` policy is the escape hatch for anything that doesn't have a
named policy — for example:
```json
"Preferences": {
  "media.peerconnection.ice.default_address_only": { "Value": true, "Status": "locked" },
  "privacy.resistFingerprinting": { "Value": true, "Status": "locked" }
}
```

---

## What NOT to set

- **`Proxy.SOCKSProxy` without `Locked: true`** — users could change it and unknowingly leak. Always lock the proxy policy.
- **Anything that requires admin / sudo to take effect** — our policies.json sits inside the bundle, so we only get bundle-level scope (per-Firefox-installation). That's what we want.
- **`DisableAppUpdate: true`** unless you have a story for security patches.

---

## Testing the policy is active

Open Firefox after install and visit:

```
about:policies
```

You should see "Active" policies listing `Proxy` (with `Locked`), `DisableTelemetry`, etc.
`about:policies?show=errors` shows parse failures.

You can also check:

```
about:preferences#general
```
→ scroll to "Network Settings". The "Settings…" button should be greyed out
with a lock icon and a "managed by your organization" hint.

---

## Re-applying on update

Firefox.app gets overwritten when БелкаТуннель reinstalls/updates Firefox via
the DMG installer. The policy file inside `Contents/Resources/distribution/`
is also overwritten by the fresh bundle. **That's why
`install_firefox_policies()` is called at the end of every install run.**

If a user manually drags Firefox to the trash and reinstalls from mozilla.org
without going through БелкаТуннель, the policy disappears. To re-apply, click
*Browse via tunnel (Firefox) → Reinstall / update Firefox*.
