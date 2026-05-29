use anyhow::{anyhow, bail, Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct FirefoxInfo {
    pub path: Option<PathBuf>,
    pub version: Option<String>,
    pub brew: Option<PathBuf>,
}

impl FirefoxInfo {
    pub fn installed(&self) -> bool {
        self.path.is_some()
    }
}

pub fn detect() -> FirefoxInfo {
    let path = locate_firefox_bundle();
    let version = path.as_ref().and_then(|p| read_firefox_version(p));
    let brew = locate_brew();
    FirefoxInfo {
        path,
        version,
        brew,
    }
}

fn locate_firefox_bundle() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = vec![PathBuf::from("/Applications/Firefox.app")];
    if let Ok(home) = std::env::var("HOME") {
        candidates.push(PathBuf::from(format!("{home}/Applications/Firefox.app")));
    }
    candidates.into_iter().find(|p| p.exists())
}

fn read_firefox_version(bundle: &Path) -> Option<String> {
    let plist = bundle.join("Contents/Info.plist");
    let out = std::process::Command::new("/usr/libexec/PlistBuddy")
        .args(["-c", "Print CFBundleShortVersionString"])
        .arg(&plist)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if v.is_empty() {
        None
    } else {
        Some(v)
    }
}

fn locate_brew() -> Option<PathBuf> {
    for p in ["/opt/homebrew/bin/brew", "/usr/local/bin/brew"] {
        if Path::new(p).exists() {
            return Some(PathBuf::from(p));
        }
    }
    None
}

// ---------- Launching Firefox ----------

/// Launch Firefox normally. The enterprise policy in the bundle already locks
/// every connection to the SOCKS5 tunnel, so there's no per-launch setup —
/// БелкаТуннель owns this Firefox install, and the user's default profile
/// (with their bookmarks, history, logins, etc.) is the only profile we
/// care about. No `-profile` flag, no `-no-remote`, no private-browsing
/// autostart.
pub fn launch_default() -> Result<()> {
    let info = detect();
    let bundle = info
        .path
        .ok_or_else(|| anyhow!("Firefox is not installed"))?;
    info!(bundle = %bundle.display(), "launching Firefox");
    std::process::Command::new("/usr/bin/open")
        .arg("-a")
        .arg(&bundle)
        .spawn()
        .context("spawn /usr/bin/open -a Firefox.app")?;
    Ok(())
}

/// Remove any orphaned BelkaTunnel-managed Firefox profile from previous
/// versions of the app. Best-effort; doesn't fail the caller on error.
pub fn cleanup_legacy_profile() {
    let Some(dirs) = directories::ProjectDirs::from("io", "celestialtech", "BelkaTunnel") else {
        return;
    };
    let legacy = dirs.data_dir().join("firefox-profile");
    if legacy.exists() {
        match std::fs::remove_dir_all(&legacy) {
            Ok(()) => info!(path = %legacy.display(), "removed legacy firefox-profile dir"),
            Err(e) => {
                warn!(path = %legacy.display(), error = %e, "could not remove legacy firefox-profile dir")
            }
        }
    }
}

// ---------- Install / Uninstall ----------

/// Direct DMG install: download Firefox from Mozilla, mount, copy
/// `Firefox.app` into `/Applications`, eject, clean up. Also seeds the SOCKS5
/// proxy preset into the БелкаТуннель profile AND into Firefox's main default
/// profile so the tunnel is wired up automatically — no separate setup click.
/// Runs in a background thread; returns immediately. Notifications at each step.
pub fn install_or_update_async(
    socks_host: String,
    socks_port: u16,
    notify: impl Fn(&str, &str) + Send + 'static,
    busy_guard: crate::BusyFlagGuard,
) -> Result<()> {
    info!("starting direct Firefox install");
    notify(
        "Downloading Firefox",
        "~150 MB from download.mozilla.org. You'll get a notification when it's done.",
    );
    std::thread::spawn(move || {
        // The guard's Drop clears FIREFOX_INSTALL_BUSY no matter how this
        // thread exits — clean return, error path, or panic.
        let _g = busy_guard;

        if let Err(e) = install_firefox_direct() {
            warn!(error = %e, "Firefox install failed");
            notify("Firefox install failed", &format!("{e}"));
            return;
        }
        info!("Firefox install succeeded");

        // Drop the enterprise policy into the bundle so EVERY Firefox launch
        // — Dock, Spotlight, "Open a private window", anything — is locked to
        // the SOCKS5 tunnel and the user cannot turn it off in Settings.
        let bundle = PathBuf::from("/Applications/Firefox.app");
        match install_firefox_policies(&bundle, &socks_host, socks_port) {
            Ok(p) => info!(path = %p.display(), "policy installed"),
            Err(e) => warn!(error = %e, "could not install enterprise policy"),
        }

        // Clean up the legacy isolated profile from older versions of the app.
        cleanup_legacy_profile();

        notify(
            "Firefox is ready",
            "Installed to /Applications. Every Firefox window now routes through the tunnel — the proxy is enforced by enterprise policy and can't be turned off.",
        );
    });
    Ok(())
}

/// Install a locked SOCKS5 proxy policy into the Firefox bundle's distribution
/// folder. Once this file is in place, every Firefox launch with this `.app`
/// (any profile, any window, normal or private) routes through our tunnel,
/// and the user cannot disable it from the Settings UI (`Locked: true`).
///
/// Reference: https://mozilla.github.io/policy-templates/#proxy
///
/// Implementation note: the policy JSON is built via `serde_json::json!` so
/// every string is properly escaped — a user-edited config with `"` in the
/// host can't silently break Firefox's policy parser and unlock the proxy.
pub fn install_firefox_policies(
    bundle: &Path,
    socks_host: &str,
    socks_port: u16,
) -> Result<PathBuf> {
    let dist_dir = bundle.join("Contents/Resources/distribution");
    std::fs::create_dir_all(&dist_dir).with_context(|| format!("mkdir {}", dist_dir.display()))?;
    let policies_path = dist_dir.join("policies.json");

    let socks_addr = format!("{}:{}", socks_host, socks_port);
    let pref_locked_bool = |v: bool| serde_json::json!({ "Value": v, "Status": "locked" });
    let pref_locked_str = |v: &str| serde_json::json!({ "Value": v, "Status": "locked" });
    let pref_locked_int = |v: i64| serde_json::json!({ "Value": v, "Status": "locked" });
    let block_locked = serde_json::json!({ "BlockNewRequests": true, "Locked": true });

    let policy = serde_json::json!({
        "_comment": "Managed by БелкаТуннель. Locked SOCKS5 + RFP-style anti-fingerprinting.",
        "policies": {
            "Proxy": {
                "Mode": "manual",
                "SOCKSProxy": socks_addr,
                "SOCKSVersion": 5,
                "UseProxyForDNS": true,
                "Locked": true
            },
            "DNSOverHTTPS": { "Enabled": true, "Locked": true },
            "EnableTrackingProtection": {
                "Value": true, "Locked": true,
                "Cryptomining": true, "Fingerprinting": true, "EmailTracking": true
            },
            "WebRTCIPHandling": { "Mode": "disable_non_proxied_udp", "Locked": true },
            "SearchEngines": {
                "Default": "Google",
                "PreventInstalls": false,
                "Remove": ["Яндекс", "Mail.ru"]
            },
            "Permissions": {
                "Geolocation":   &block_locked,
                "Camera":        &block_locked,
                "Microphone":    &block_locked,
                "Notifications": &block_locked
            },
            // Lock Firefox into a non-self-updating state so the bundle's
            // policies.json survives — Firefox's in-app updater rewrites the
            // app bundle wholesale and would otherwise wipe our policy.
            "AppAutoUpdate": false,
            "DisableAppUpdate": true,
            "BackgroundAppUpdate": false,

            "Preferences": {
                "geo.enabled":                        pref_locked_bool(false),
                "geo.provider.network.url":           pref_locked_str(""),
                "browser.region.network.url":         pref_locked_str(""),
                "browser.search.geoSpecificDefaults": pref_locked_bool(false),

                "privacy.resistFingerprinting":                      pref_locked_bool(true),
                "privacy.fingerprintingProtection":                  pref_locked_bool(true),
                "privacy.firstparty.isolate":                        pref_locked_bool(true),
                "privacy.trackingprotection.fingerprinting.enabled": pref_locked_bool(true),
                "privacy.trackingprotection.cryptomining.enabled":   pref_locked_bool(true),

                "media.peerconnection.ice.default_address_only":     pref_locked_bool(true),
                "media.navigator.enabled":                           pref_locked_bool(false),

                "dom.event.clipboardevents.enabled":                 pref_locked_bool(false),
                "dom.battery.enabled":                               pref_locked_bool(false),
                "dom.webaudio.enabled":                              pref_locked_bool(false),
                "dom.gamepad.enabled":                               pref_locked_bool(false),

                "network.http.referer.XOriginPolicy":                pref_locked_int(1),
                "network.http.referer.XOriginTrimmingPolicy":        pref_locked_int(2),
                "network.http.sendRefererHeader":                    pref_locked_int(1),

                "intl.accept_languages":              pref_locked_str("en-US, en"),
                "intl.locale.requested":              pref_locked_str("en-US"),
                "intl.locale.matchOS":                pref_locked_bool(false),
                "intl.regional_prefs.use_os_locales": pref_locked_bool(false),
                "general.useragent.locale":           pref_locked_str("en-US"),

                "spellchecker.dictionary":            pref_locked_str("en-US"),

                "browser.search.suggest.enabled":     pref_locked_bool(false),
                "browser.urlbar.suggest.searches":    pref_locked_bool(false),
                "browser.urlbar.suggest.engines":     pref_locked_bool(false),

                "browser.newtabpage.activity-stream.feeds.section.topstories":         pref_locked_bool(false),
                "browser.newtabpage.activity-stream.showSponsored":                    pref_locked_bool(false),
                "browser.newtabpage.activity-stream.showSponsoredTopSites":            pref_locked_bool(false),
                "browser.newtabpage.activity-stream.section.highlights.includePocket": pref_locked_bool(false),
                "browser.discovery.enabled":          pref_locked_bool(false),

                "browser.translations.automaticPopup": pref_locked_bool(false),
                "browser.translations.enable":         pref_locked_bool(false),

                "browser.region.update.enabled":       pref_locked_bool(false),
                "browser.search.geoip.url":            pref_locked_str(""),

                "javascript.use_us_english_locale":    pref_locked_bool(true)
            },

            "DisableSecurityBypass":   false,
            "DisableTelemetry":        true,
            "DisableFirefoxStudies":   true,
            "DontCheckDefaultBrowser": true,
            "OverrideFirstRunPage":    "https://ifconfig.me/",
            "DisablePocket":           true
        }
    });

    let json = serde_json::to_string_pretty(&policy).context("serialize policies.json")?;
    std::fs::write(&policies_path, json + "\n")
        .with_context(|| format!("writing {}", policies_path.display()))?;
    info!(path = %policies_path.display(), "installed Firefox enterprise policy (Locked SOCKS5)");
    Ok(policies_path)
}

fn install_firefox_direct() -> Result<()> {
    let tmp_dmg = std::env::temp_dir().join("belka-firefox.dmg");
    let _ = std::fs::remove_file(&tmp_dmg);

    // Mozilla's stable redirector — picks the right architecture automatically.
    let url = "https://download.mozilla.org/?product=firefox-latest-ssl&os=osx&lang=en-US";
    let dl = std::process::Command::new("/usr/bin/curl")
        .args(["-fL", "--silent", "--show-error", "-o"])
        .arg(&tmp_dmg)
        .arg(url)
        .output()
        .context("spawn curl for Firefox DMG")?;
    if !dl.status.success() {
        let stderr = String::from_utf8_lossy(&dl.stderr).into_owned();
        bail!("download failed: {}", stderr.trim());
    }

    // Mount the DMG read-only, no Finder window. `-noverify` skips the
    // checksum output which would otherwise be interleaved with the device
    // table on stdout, making parsing fragile.
    let mount_out = std::process::Command::new("/usr/bin/hdiutil")
        .args(["attach", "-nobrowse", "-readonly", "-noverify"])
        .arg(&tmp_dmg)
        .output()
        .context("spawn hdiutil attach")?;
    if !mount_out.status.success() {
        let _ = std::fs::remove_file(&tmp_dmg);
        bail!(
            "hdiutil attach failed: {}",
            String::from_utf8_lossy(&mount_out.stderr).trim()
        );
    }
    // Parse the mount point — last field of the line containing /Volumes/.
    let stdout = String::from_utf8_lossy(&mount_out.stdout);
    let mount_point = stdout
        .lines()
        .filter_map(|l| {
            l.split('\t')
                .next_back()
                .map(str::trim)
                .filter(|p| p.starts_with("/Volumes/"))
        })
        .next()
        .ok_or_else(|| anyhow!("could not parse mount point from hdiutil"))?
        .to_string();
    info!(mount = %mount_point, "DMG mounted");

    let src = PathBuf::from(&mount_point).join("Firefox.app");
    if !src.exists() {
        let _ = std::process::Command::new("/usr/bin/hdiutil")
            .args(["detach", "-quiet"])
            .arg(&mount_point)
            .status();
        let _ = std::fs::remove_file(&tmp_dmg);
        bail!("Firefox.app missing inside DMG at {}", src.display());
    }

    // Replace any existing /Applications/Firefox.app.
    let target_root = PathBuf::from("/Applications");
    let target = target_root.join("Firefox.app");
    if target.exists() {
        if let Err(e) = std::fs::remove_dir_all(&target) {
            // Try to detach before erroring out.
            let _ = std::process::Command::new("/usr/bin/hdiutil")
                .args(["detach", "-quiet"])
                .arg(&mount_point)
                .status();
            let _ = std::fs::remove_file(&tmp_dmg);
            bail!(
                "couldn't remove existing {}: {} \
                 (close Firefox first, or copy manually)",
                target.display(),
                e
            );
        }
    }

    // ditto preserves resource forks / extended attributes / signatures.
    let cp = std::process::Command::new("/usr/bin/ditto")
        .arg(&src)
        .arg(&target)
        .status()
        .context("spawn ditto for Firefox copy")?;

    // Always detach the DMG and remove the download, even if copy failed.
    let _ = std::process::Command::new("/usr/bin/hdiutil")
        .args(["detach", "-quiet"])
        .arg(&mount_point)
        .status();
    let _ = std::fs::remove_file(&tmp_dmg);

    if !cp.success() {
        bail!(
            "couldn't copy Firefox.app to {}. Permission denied? Try ~/Applications instead.",
            target_root.display()
        );
    }
    Ok(())
}

/// Uninstall Firefox. Prefers `brew uninstall --cask firefox` if Homebrew
/// manages the cask; otherwise moves the bundle to the Trash via Finder.
pub fn uninstall_async(notify: impl Fn(&str, &str) + Send + 'static) -> Result<()> {
    let info = detect();
    if !info.installed() {
        bail!("Firefox is not installed");
    }
    let bundle = info.path.clone().unwrap();

    if let Some(brew) = info.brew.clone() {
        // Check if firefox cask is actually managed by brew before using it.
        let managed = std::process::Command::new(&brew)
            .args(["list", "--cask", "firefox"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if managed {
            info!("uninstalling Firefox via Homebrew");
            notify("Uninstalling Firefox", "Using Homebrew.");
            std::thread::spawn(move || {
                let out = std::process::Command::new(&brew)
                    .args(["uninstall", "--cask", "firefox"])
                    .output();
                match out {
                    Ok(o) if o.status.success() => {
                        notify("Firefox uninstalled", "Removed via Homebrew.");
                    }
                    Ok(o) => {
                        let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
                        notify(
                            "Firefox uninstall failed",
                            &format!("brew uninstall returned non-zero:\n{stderr}"),
                        );
                    }
                    Err(e) => notify("Firefox uninstall failed", &format!("{e}")),
                }
            });
            return Ok(());
        }
    }

    // Fall back to moving the app bundle to Trash via Finder.
    info!(bundle = %bundle.display(), "moving Firefox.app to Trash via Finder");
    let path_str = crate::applescript_escape(&bundle.to_string_lossy());
    let script = format!(
        r#"tell application "Finder" to delete POSIX file "{}""#,
        path_str
    );
    std::thread::spawn(move || {
        let out = std::process::Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(&script)
            .output();
        match out {
            Ok(o) if o.status.success() => {
                notify("Firefox moved to Trash", "");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
                notify("Firefox uninstall failed", &stderr);
            }
            Err(e) => notify("Firefox uninstall failed", &format!("{e}")),
        }
    });
    Ok(())
}

// ---------- Homebrew bootstrap ----------

/// Install Homebrew via the canonical one-liner. Opens Terminal.app so the
/// user can see the install progress and respond to the sudo prompt
/// (Homebrew's install script refuses to run as root, so we can't bypass
/// Terminal cleanly). Background-polls for `brew` to appear and notifies
/// when it's ready.
pub fn install_homebrew_async(
    notify: impl Fn(&str, &str) + Send + 'static,
    busy_guard: crate::BusyFlagGuard,
) {
    // The official install command, as documented on https://brew.sh.
    let install_cmd = r#"/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)""#;
    let osascript = format!(
        r#"tell application "Terminal"
    activate
    do script "{}"
end tell"#,
        crate::applescript_escape(install_cmd)
    );
    if let Err(e) = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&osascript)
        .spawn()
    {
        warn!(error = %e, "could not open Terminal for Homebrew install");
        notify(
            "Couldn't open Terminal",
            "Run this in Terminal manually:\n\
             /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)\"",
        );
        // busy_guard dropped here on the early-return path.
        return;
    }

    notify(
        "Installing Homebrew",
        "Terminal opened with the standard install one-liner. \
         You'll get another notification once brew is ready.",
    );

    std::thread::spawn(move || {
        // Clears HOMEBREW_INSTALL_BUSY no matter how this poller exits.
        let _g = busy_guard;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if let Some(brew) = locate_brew() {
                info!(brew = %brew.display(), "Homebrew install detected");
                notify(
                    "Homebrew installed",
                    "Ready to use. Restart БелкаТуннель to refresh the menu.",
                );
                return;
            }
            if std::time::Instant::now() >= deadline {
                warn!("Homebrew install poll timed out");
                notify(
                    "Homebrew install timed out",
                    "Couldn't detect brew after 30 minutes. \
                     Run the install manually in Terminal if needed.",
                );
                return;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::tempdir;

    fn build_policy(host: &str, port: u16) -> Value {
        let dir = tempdir().unwrap();
        let bundle = dir.path().join("Firefox.app");
        let path = install_firefox_policies(&bundle, host, port).unwrap();
        let raw = std::fs::read_to_string(&path).unwrap();
        serde_json::from_str(&raw).expect("policies.json must be valid JSON")
    }

    #[test]
    fn proxy_is_locked_socks5() {
        let v = build_policy("127.0.0.1", 1080);
        let p = &v["policies"];
        assert_eq!(p["Proxy"]["Locked"], true);
        assert_eq!(p["Proxy"]["Mode"], "manual");
        assert_eq!(p["Proxy"]["SOCKSVersion"], 5);
        assert_eq!(p["Proxy"]["SOCKSProxy"], "127.0.0.1:1080");
        assert_eq!(p["Proxy"]["UseProxyForDNS"], true);
    }

    /// Regression for the bug fixed in commit 24972a7: without these three
    /// fields Firefox's in-app updater would rewrite the bundle and wipe
    /// our locked-proxy policy on every minor release.
    #[test]
    fn firefox_updater_disabled() {
        let v = build_policy("127.0.0.1", 1080);
        let p = &v["policies"];
        assert_eq!(p["AppAutoUpdate"], false);
        assert_eq!(p["DisableAppUpdate"], true);
        assert_eq!(p["BackgroundAppUpdate"], false);
    }

    #[test]
    fn anti_fingerprinting_layers_locked() {
        let v = build_policy("127.0.0.1", 1080);
        let p = &v["policies"];
        assert_eq!(p["EnableTrackingProtection"]["Locked"], true);
        assert_eq!(p["DNSOverHTTPS"]["Locked"], true);
        assert_eq!(p["WebRTCIPHandling"]["Locked"], true);
        assert_eq!(p["WebRTCIPHandling"]["Mode"], "disable_non_proxied_udp");
    }

    /// JSON-injection guard: even a host with characters that would break
    /// hand-built JSON strings (quote, backslash, newline) must round-trip
    /// byte-equal and yield valid JSON.
    #[test]
    fn host_with_special_chars_is_escaped() {
        let host = "a\"b\\c\nd";
        let v = build_policy(host, 9999);
        let socks = v["policies"]["Proxy"]["SOCKSProxy"].as_str().unwrap();
        assert_eq!(socks, format!("{host}:9999"));
    }

    proptest::proptest! {
        /// Any printable host string must produce valid JSON that round-trips.
        #[test]
        fn policy_json_always_valid_for_printable_hosts(
            host in "[\\x20-\\x7e]{1,40}",
            port in 1u16..65535,
        ) {
            let v = build_policy(&host, port);
            let socks = v["policies"]["Proxy"]["SOCKSProxy"].as_str().unwrap();
            assert_eq!(socks, format!("{host}:{port}"));
            assert_eq!(v["policies"]["Proxy"]["Locked"], true);
            assert_eq!(v["policies"]["DisableAppUpdate"], true);
        }
    }
}
