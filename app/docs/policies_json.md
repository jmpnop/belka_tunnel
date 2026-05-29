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
    "DNSOverHTTPS": { "Enabled": true, "Locked": true },
    "Permissions": {
      "Geolocation": { "BlockNewRequests": true, "Locked": true },
      "Camera":      { "BlockNewRequests": true, "Locked": true },
      "Microphone":  { "BlockNewRequests": true, "Locked": true }
    },
    "Preferences": {
      "geo.enabled":                       { "Value": false, "Status": "locked" },
      "geo.provider.network.url":          { "Value": "",    "Status": "locked" },
      "browser.region.network.url":        { "Value": "",    "Status": "locked" },
      "browser.search.geoSpecificDefaults":{ "Value": false, "Status": "locked" }
    },
    "DisableSecurityBypass":   false,
    "DisableTelemetry":        true,
    "DisableFirefoxStudies":   true,
    "DontCheckDefaultBrowser": true,
    "OverrideFirstRunPage":    "https://ifconfig.me/",
    "DisablePocket":           true
  }
}
```

| Key                                | Why we set it                                                                                                                                  |
|------------------------------------|------------------------------------------------------------------------------------------------------------------------------------------------|
| `Proxy` + `Locked: true`           | Force every connection through the tunnel; user can't change it in Settings.                                                                   |
| `DNSOverHTTPS` (locked on)         | DNS goes via HTTPS to Cloudflare (default) **inside** the tunnel — Cloudflare sees queries, the SSH server doesn't.                            |
| `Permissions.Geolocation` (block)  | Sites can't even ask for location. See [Location services](#location-services-deep-dive) below.                                                |
| `Permissions.Camera/Microphone`    | Same idea for the other physical-identity sensors that often co-leak with location ("Allow this site to use camera/mic for video calls").     |
| `geo.enabled = false` (locked)     | Belt-and-suspenders: disables the `navigator.geolocation` API entirely so sites don't even get a `prompt` to call.                             |
| `geo.provider.network.url = ""`    | Even if `geo.enabled` is somehow re-enabled, the network-location lookup URL is empty so Wi-Fi-based geolocation can't function.               |
| `browser.region.network.url = ""`  | Stops Firefox from periodically calling out to the Mozilla "region" service to figure out your country (used for default search engine, etc.). |
| `browser.search.geoSpecificDefaults: false` | Disables locale-based default-search-engine swap (Yandex in RU, Baidu in CN, etc.) — keeps search consistent regardless of exit IP. |
| `DisableSecurityBypass: false`     | Default behaviour, set explicitly: users **can** click through cert warnings and `about:blocked` interstitials.                                |
| `DisableTelemetry`                 | Stop the Firefox health-report / data-reporting pings; they'd leak through the tunnel anyway.                                                  |
| `DisableFirefoxStudies`            | No Normandy/SHIELD experiments.                                                                                                                |
| `DontCheckDefaultBrowser`          | No "Make Firefox default?" prompt on every launch.                                                                                             |
| `OverrideFirstRunPage`             | First launch goes to ifconfig.me so the user can immediately see the tunnel IP.                                                                |
| `DisablePocket`                    | Less UI clutter; Pocket pings are a known tracker signal.                                                                                      |

---

## Russian-locale scrubbing (deep dive)

A US-IP tunnel + UTC timezone is undone by an `Accept-Language: ru` header.
Sites and bots fingerprint heavily on locale signals because they're stable
across IP changes. The policy now scrubs every channel that could leak a
Russian system:

### 1. HTTP `Accept-Language` header
Set by Firefox to whatever `intl.accept_languages` says. We lock it to
`en-US, en` — same as a fresh US install.
```json
"intl.accept_languages": { "Value": "en-US, en", "Status": "locked" }
```

### 2. `navigator.language` / `navigator.languages` JS APIs
Driven by `intl.locale.requested` and the OS-locale fallback. We lock
`intl.locale.requested = en-US`, plus disable the fallbacks:
```json
"intl.locale.requested":              { "Value": "en-US", "Status": "locked" },
"intl.locale.matchOS":                { "Value": false,   "Status": "locked" },
"intl.regional_prefs.use_os_locales": { "Value": false,   "Status": "locked" },
"general.useragent.locale":           { "Value": "en-US", "Status": "locked" }
```
The last one is legacy but still read by some Firefox code paths and add-ons.

### 3. `Date()` / `Intl.DateTimeFormat()` localization
By default Firefox uses the OS locale for number/date/currency formatting,
so a Russian system formats dates as `28.05.2026`, decimals as `1,5`, and
currency as `₽1 000,50`. `resistFingerprinting` normalizes these to en-US,
and we add a belt:
```json
"javascript.use_us_english_locale": { "Value": true, "Status": "locked" }
```
This forces `Date.toLocaleString()` etc. to en-US even when RFP is
somehow disabled.

### 4. Default search engine (Yandex / Mail.ru)
Firefox auto-swaps the default search engine to Yandex in RU/UA/BY at first
launch, even with no user interaction — single biggest "this user is Russian"
signal. We override + lock with the `SearchEngines` policy:
```json
"SearchEngines": {
  "Default": "Google",
  "PreventInstalls": false,
  "Remove": ["Яндекс", "Mail.ru"]
}
```
Google is what a generic en-US user has. DuckDuckGo would be more private
but also a "privacy user" signal — Google is the most generic choice.

### 5. Search & URL-bar suggestions
Suggestions are routed through the default search engine, locale-aware
endpoints, and quietly echo the user's locale in every query. Disabled:
```json
"browser.search.suggest.enabled":  { "Value": false, "Status": "locked" },
"browser.urlbar.suggest.searches": { "Value": false, "Status": "locked" },
"browser.urlbar.suggest.engines":  { "Value": false, "Status": "locked" }
```

### 6. New tab page sponsored / topstories content
Pocket-driven, locale-aware feed of "stories" on the new tab — by default
shows Russian content if your locale says Russian. We turn the whole feed
off:
```json
"browser.newtabpage.activity-stream.feeds.section.topstories":         { "Value": false },
"browser.newtabpage.activity-stream.showSponsored":                    { "Value": false },
"browser.newtabpage.activity-stream.showSponsoredTopSites":            { "Value": false },
"browser.newtabpage.activity-stream.section.highlights.includePocket": { "Value": false },
"browser.discovery.enabled":                                           { "Value": false }
```

### 7. Translation popup
The "Translate this page?" banner appears when the page language ≠ user's
locale. Even if the user dismisses it, the popup itself proves the browser
considers the page foreign — which is informative if RFP wasn't perfect.
Disabled:
```json
"browser.translations.automaticPopup": { "Value": false, "Status": "locked" },
"browser.translations.enable":         { "Value": false, "Status": "locked" }
```

### 8. Mozilla "region update" service
Firefox periodically re-detects your country and updates the regional
default search engine. Even if we set Google as default today, the region
update could swap it to Yandex tomorrow when the tunnel IP changes or
maintenance lets the lookup leak. Disabled:
```json
"browser.region.update.enabled": { "Value": false, "Status": "locked" },
"browser.search.geoip.url":      { "Value": "",    "Status": "locked" }
```

### 9. Spellchecker dictionary
Firefox auto-selects a Russian dictionary based on OS locale; the
`spellchecker.dictionary` pref's value is queryable from extensions and
some sites. We lock it to `en-US`:
```json
"spellchecker.dictionary": { "Value": "en-US", "Status": "locked" }
```

### 10. What we deliberately don't scrub
- **Bookmarks** — your Russian bookmarks are yours; we don't touch them.
- **History / passwords** — same.
- **Installed dictionaries** — we lock the *active* dictionary, not the
  installed ones. Russian dictionary remains available if you ever need it.
- **The user's actual typing** — if you type Cyrillic into the URL bar
  or search box, that obviously goes through the tunnel as Cyrillic. No
  policy can hide that.

### Verifying the scrub
Open Firefox after install/reinstall, visit:
- <https://browserleaks.com/ip> → IP = 173.77.254.243, headers should show `Accept-Language: en-US,en;q=0.5`, timezone UTC, language en-US.
- <https://www.whatismybrowser.com/detect/what-is-my-language> → reports en-US.
- `about:config?filter=intl.` → `intl.accept_languages` and `intl.locale.requested` shown as locked.

---

## Fingerprinting resistance (deep dive)

A clean SOCKS5 tunnel hides your IP. It does **not** hide your browser. Sites
identify "you" across visits via dozens of small signals — screen resolution,
timezone, font list, canvas-rendering quirks, hardware concurrency. With a
fresh tunnel IP but the same browser fingerprint, you're still trivially
re-identified.

What we now turn on (all locked):

### `privacy.resistFingerprinting` — the big switch
Tor Browser's RFP, ported into Firefox. Normalizes / spoofs:
- **User agent** → generic recent Firefox.
- **Timezone** → UTC, regardless of macOS setting.
- **Screen / window size** → rounded to "letterbox" buckets so window dimensions don't uniquely ID you.
- **Fonts** → restricted to a small allowlist; site-installed fonts ignored.
- **Canvas / WebGL** → permission-prompted (instead of silently returning unique pixel data).
- **`navigator.platform`, `oscpu`, `appCodeName`** → fixed values.
- **`Intl.DateTimeFormat`** → UTC, en-US.
- **Pointer/touch capabilities** → spoofed.
- **`performance.now()`** → reduced precision (2ms) to defeat timing attacks.

Cost: some sites break — common ones are anything that does layout based on
real screen size, video conferencing apps that need precise timers, or
sites that *check* the user agent for compatibility shims.

### `privacy.fingerprintingProtection`
Newer (Firefox 102+) parallel system that uses Mozilla's "known fingerprinters"
list (ETP) to neutralize specific techniques per-script instead of broadly.
Complementary to RFP, less site-breaking.

### `EnableTrackingProtection` with `Fingerprinting: true`
ETP's strict-mode list of fingerprinting-tracker domains gets blocked at the
network layer. The script never runs.

### `privacy.firstparty.isolate`
First-Party Isolation. Caches, cookies, storage, DOM storage, IndexedDB,
broadcast channels, etc. are partitioned by the URL bar's origin. A tracker
loaded on `news.com` and on `shop.com` gets two separate identities.

### `WebRTCIPHandling: disable_non_proxied_udp`
WebRTC's STUN traffic uses UDP, which SOCKS5 doesn't tunnel. Without this
policy, a malicious page can grab your real LAN+public IP via WebRTC even
through the tunnel. With this policy, only proxy-able candidates are used —
no STUN leak.

### `media.navigator.enabled: false`
Kills the `navigator.mediaDevices` API entirely — sites can't enumerate
cameras/mics (those device IDs are themselves unique fingerprints).

### Locked locale: `en-US`
`navigator.language` and the `Accept-Language` header are hard-set so a
Russian user behind the tunnel doesn't broadcast Russian preferences. This
makes you indistinguishable from the rest of the en-US Firefox population.

### What's deliberately NOT enabled
- **`webgl.disabled: true`** — too site-breaky; WebGL gets canvas-style
  permission prompt under RFP, which is the right tradeoff.
- **`media.peerconnection.enabled: false`** — would break Discord, video
  calls, etc. `WebRTCIPHandling` is a finer tool.
- **Spoofing back to a fake "user agent string"** — `general.useragent.override`
  is a footgun because it doesn't update navigator object fields consistently;
  RFP already handles UA correctly.

### Verifying the disguise
Visit one of these to see what sites see:
- <https://browserleaks.com/firefox> — proxy/WebRTC/canvas/fonts in one page.
- <https://coveryourtracks.eff.org> — EFF's Panopticlick successor.
- <https://amiunique.org> — overall uniqueness percentile.

Behind БелкаТуннель + this policy you should see:
- IP: tunnel exit (173.77.254.243)
- Timezone: UTC
- Language: en-US
- WebRTC: no candidates
- Canvas: prompted instead of leaked
- Fonts: ~10 standardized
- Geolocation: blocked

---

## Location services (deep dive)

Firefox can leak your physical location through several distinct channels — IP
isn't the only one. Even with all traffic in the tunnel, Firefox itself can
still tell a site where you are. What we block:

### 1. `navigator.geolocation` API (the JS one)
The browser API a page calls to get coordinates. Firefox sources these from
Google's geolocation service by default (yes — Google, even on Firefox), using
nearby Wi-Fi BSSIDs and cell-tower IDs harvested by macOS. **Even routed
through the SOCKS5 tunnel the response is your real lat/long**, because the
Wi-Fi MAC addresses going into the request are physical-radio facts.

We block this at three levels:
- `Permissions.Geolocation.BlockNewRequests: true, Locked: true` — no permission prompt; sites get `PermissionDeniedError`.
- `geo.enabled = false, locked` — the API itself is disabled; `navigator.geolocation` becomes `undefined`.
- `geo.provider.network.url = "", locked` — even if `geo.enabled` flips back, there's no URL to call so the network provider can't return a location.

### 2. Mozilla "region" service
Firefox periodically fetches `https://location.services.mozilla.com/v1/country?key=…`
to determine the user's country. Used for things like swapping the default
search engine to Yandex in Russia. We disable it with
`browser.region.network.url = "", locked` and
`browser.search.geoSpecificDefaults: false`.

### 3. WebRTC IP discovery
WebRTC's STUN/ICE machinery can reveal your real LAN IP and your real public
IP even with a SOCKS5 proxy, because STUN uses UDP which SOCKS5 doesn't tunnel.
We don't set `WebRTCIPHandling` in the current policy — recommend adding it:
```json
"WebRTCIPHandling": { "Mode": "disable_non_proxied_udp", "Locked": true }
```
Effect: WebRTC will only use ICE candidates that go through the proxy. STUN
queries that would hit the public internet are dropped.

### 4. Timezone / system locale fingerprint
The `Date()` object reveals the macOS system timezone, and `navigator.language`
exposes locale. Both are reachable from any JS context and can hint at country.
To neutralize, set the resist-fingerprinting pref:
```json
"Preferences": {
  "privacy.resistFingerprinting": { "Value": true, "Status": "locked" }
}
```
Caveat: this normalizes many things (timezone forced to UTC, screen resolution
rounded, fonts spoofed) which **does** break some sites. Worth turning on for
maximum anonymity, leave off for everyday browsing.

### 5. What we deliberately don't try to fake
We block leaks, but we don't *fake* a location to make sites think you're in
the tunnel exit's country. Faking requires either:
- A WebExtension intercepting the geolocation API (would need Mozilla signing
  for release Firefox).
- An external geolocation database keyed on the tunnel exit IP (and the page
  would have to actually use IP-based geolocation, which most modern sites
  don't — they ask the JS API first).

Conclusion: with the policy block + WebRTC fix + resistFingerprinting, what
sites see is "no location, no fingerprint, IP says 173.77.254.243". That's
the tunnel exit; everything else is silent.

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
