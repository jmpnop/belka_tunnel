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

/// Kick off `brew install --cask --force firefox` (upgrades if already
/// installed). Runs in a background thread; returns immediately. Emits macOS
/// notifications on start, success, and failure.
pub fn install_or_update_async(notify: impl Fn(&str, &str) + Send + 'static) -> Result<()> {
    let brew = locate_brew().ok_or_else(|| {
        anyhow!(
            "Homebrew not found. Install it from https://brew.sh, \
             or download Firefox manually from https://www.mozilla.org/firefox/"
        )
    })?;
    info!(brew = %brew.display(), "starting Firefox install/upgrade");
    notify(
        "Installing Firefox via Homebrew",
        "This may take a couple of minutes. You'll get a notification when it's done.",
    );
    std::thread::spawn(move || {
        let out = std::process::Command::new(&brew)
            .args(["install", "--cask", "--force", "firefox"])
            .output();
        match out {
            Ok(o) if o.status.success() => {
                info!("Firefox install/upgrade succeeded");
                notify("Firefox is ready", "Installed via Homebrew.");
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr).into_owned();
                warn!(stderr = %stderr, "brew install failed");
                notify(
                    "Firefox install failed",
                    &format!(
                        "brew install --cask firefox returned non-zero. \
                         Tail of stderr:\n{}",
                        stderr.lines().rev().take(6).collect::<Vec<_>>().join("\n")
                    ),
                );
            }
            Err(e) => {
                warn!(error = %e, "brew install could not start");
                notify("Firefox install failed", &format!("{e}"));
            }
        }
    });
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

/// Open Terminal.app with the official Homebrew install command, then poll in
/// the background until the `brew` binary appears (or the user cancels). When
/// `then_install_firefox` is true, immediately runs Firefox install once brew
/// is detected. Notifications keep the user informed.
pub fn install_homebrew_async(
    then_install_firefox: bool,
    notify: impl Fn(&str, &str) + Send + Clone + 'static,
) {
    // The official install script. Run inside a fresh Terminal window so the
    // user can see progress and enter their sudo password.
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
            "Open Terminal manually and run the install command from https://brew.sh",
        );
        return;
    }

    notify(
        "Installing Homebrew",
        "Terminal is open — enter your password when prompted. \
         You'll get another notification when Homebrew is ready.",
    );

    let notify_for_thread = notify.clone();
    std::thread::spawn(move || {
        // Poll every 5s for up to 30 minutes for the brew binary to appear.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30 * 60);
        loop {
            std::thread::sleep(std::time::Duration::from_secs(5));
            if let Some(brew) = locate_brew() {
                info!(brew = %brew.display(), "Homebrew install detected");
                notify_for_thread(
                    "Homebrew installed",
                    if then_install_firefox {
                        "Now installing Firefox via Homebrew…"
                    } else {
                        "Ready to use. Restart БелкаТуннель to refresh the menu."
                    },
                );
                if then_install_firefox {
                    let out = std::process::Command::new(&brew)
                        .args(["install", "--cask", "--force", "firefox"])
                        .output();
                    match out {
                        Ok(o) if o.status.success() => notify_for_thread(
                            "Firefox is ready",
                            "Installed via Homebrew. Restart БелкаТуннель to enable the 'Open a private window' item.",
                        ),
                        Ok(o) => notify_for_thread(
                            "Firefox install failed",
                            &format!(
                                "brew install returned non-zero:\n{}",
                                String::from_utf8_lossy(&o.stderr)
                                    .lines()
                                    .rev()
                                    .take(6)
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            ),
                        ),
                        Err(e) => notify_for_thread(
                            "Firefox install failed",
                            &format!("{e}"),
                        ),
                    }
                }
                return;
            }
            if std::time::Instant::now() >= deadline {
                warn!("Homebrew install poll timed out after 30 min");
                notify_for_thread(
                    "Homebrew install timed out",
                    "Couldn't detect brew after 30 minutes. \
                     If you completed the install, restart БелкаТуннель and try again.",
                );
                return;
            }
        }
    });
}
