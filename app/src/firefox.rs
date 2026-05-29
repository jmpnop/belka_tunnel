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
    let version = path
        .as_ref()
        .and_then(|p| read_firefox_version(p));
    let brew = locate_brew();
    FirefoxInfo {
        path,
        version,
        brew,
    }
}

fn locate_firefox_bundle() -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> =
        vec![PathBuf::from("/Applications/Firefox.app")];
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

// ---------- Profile management (existing flow) ----------

pub struct FirefoxProfile {
    pub dir: PathBuf,
}

impl FirefoxProfile {
    pub fn default_dir() -> Option<PathBuf> {
        directories::ProjectDirs::from("io", "celestialtech", "BelkaTunnel")
            .map(|d| d.data_dir().join("firefox-profile"))
    }

    pub fn ensure(socks_host: &str, socks_port: u16) -> Result<Self> {
        let dir = Self::default_dir().ok_or_else(|| anyhow!("no data dir"))?;
        std::fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
        let user_js = dir.join("user.js");
        let contents = format!(
            r#"// Managed by БелкаТуннель — overwritten on every "Browse the web through tunnel" click.
user_pref("network.proxy.type", 1);
user_pref("network.proxy.socks", "{host}");
user_pref("network.proxy.socks_port", {port});
user_pref("network.proxy.socks_version", 5);
user_pref("network.proxy.socks_remote_dns", true);
user_pref("network.proxy.no_proxies_on", "");
user_pref("browser.privatebrowsing.autostart", true);
user_pref("media.peerconnection.enabled", false);
user_pref("network.dns.disablePrefetch", true);
user_pref("network.prefetch-next", false);
user_pref("network.predictor.enabled", false);
user_pref("browser.startup.homepage", "https://ifconfig.me/");
user_pref("browser.startup.page", 1);
user_pref("browser.startup.firstrunSkipsHomepage", true);
user_pref("datareporting.policy.firstRunURL", "");
user_pref("trailhead.firstrun.didSeeAboutWelcome", true);
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.aboutConfig.showWarning", false);
"#,
            host = socks_host,
            port = socks_port
        );
        std::fs::write(&user_js, contents)
            .with_context(|| format!("writing {}", user_js.display()))?;
        Ok(Self { dir })
    }

    pub fn launch(&self) -> Result<()> {
        let info = detect();
        let bundle = info
            .path
            .ok_or_else(|| anyhow!("Firefox is not installed"))?;
        let binary = find_firefox_binary(&bundle)
            .ok_or_else(|| anyhow!("Firefox binary missing inside {}", bundle.display()))?;
        info!(firefox = %binary.display(), profile = %self.dir.display(), "launching Firefox");
        std::process::Command::new(binary)
            .arg("-profile")
            .arg(self.dir.as_os_str())
            .arg("-no-remote")
            .spawn()
            .context("spawn firefox")?;
        Ok(())
    }
}

fn find_firefox_binary(bundle: &Path) -> Option<PathBuf> {
    for name in ["firefox", "firefox-bin"] {
        let p = bundle.join("Contents/MacOS").join(name);
        if p.exists() {
            return Some(p);
        }
    }
    None
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
) -> Result<()> {
    info!("starting direct Firefox install");
    notify(
        "Downloading Firefox",
        "~150 MB from download.mozilla.org. You'll get a notification when it's done.",
    );
    std::thread::spawn(move || {
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

        // Also seed the isolated БелкаТуннель profile so the menu item still
        // gets private-browsing autostart + ifconfig.me homepage as a clean test.
        if let Err(e) = FirefoxProfile::ensure(&socks_host, socks_port) {
            warn!(error = %e, "could not seed isolated profile");
        }

        notify(
            "Firefox is ready",
            "Installed to /Applications. The SOCKS5 proxy is enforced by Firefox enterprise policy — every window goes through the tunnel and the setting is locked.",
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
pub fn install_firefox_policies(bundle: &Path, socks_host: &str, socks_port: u16) -> Result<PathBuf> {
    let dist_dir = bundle.join("Contents/Resources/distribution");
    std::fs::create_dir_all(&dist_dir)
        .with_context(|| format!("mkdir {}", dist_dir.display()))?;
    let policies_path = dist_dir.join("policies.json");

    let socks_addr = format!("{}:{}", socks_host, socks_port);
    let json = format!(
        r#"{{
  "_comment": "Managed by БелкаТуннель. Forces all Firefox traffic through the SSH SOCKS5 tunnel.",
  "policies": {{
    "Proxy": {{
      "Mode": "manual",
      "SOCKSProxy": "{addr}",
      "SOCKSVersion": 5,
      "UseProxyForDNS": true,
      "Locked": true
    }},
    "DisableTelemetry": true,
    "DisableFirefoxStudies": true,
    "DontCheckDefaultBrowser": true,
    "OverrideFirstRunPage": "https://ifconfig.me/",
    "DisablePocket": true
  }}
}}
"#,
        addr = socks_addr
    );
    std::fs::write(&policies_path, json)
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
            l.split('\t').last().map(str::trim).filter(|p| {
                p.starts_with("/Volumes/")
            })
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
    let path_str = bundle.to_string_lossy().replace('"', "\\\"");
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

pub fn open_download_page() {
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("https://www.mozilla.org/firefox/new/")
        .spawn();
}

// ---------- Homebrew bootstrap ----------

/// Install Homebrew via the canonical one-liner. Opens Terminal.app so the
/// user can see the install progress and respond to the sudo prompt
/// (Homebrew's install script refuses to run as root, so we can't bypass
/// Terminal cleanly). Background-polls for `brew` to appear and notifies
/// when it's ready.
pub fn install_homebrew_async(notify: impl Fn(&str, &str) + Send + 'static) {
    // The official install command, as documented on https://brew.sh.
    let install_cmd = r#"/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)""#;
    let osascript = format!(
        r#"tell application "Terminal"
    activate
    do script "{}"
end tell"#,
        install_cmd.replace('"', "\\\"")
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
        return;
    }

    notify(
        "Installing Homebrew",
        "Terminal opened with the standard install one-liner. \
         You'll get another notification once brew is ready.",
    );

    std::thread::spawn(move || {
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
