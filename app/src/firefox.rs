use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tracing::{info, warn};

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

        // user.js is the canonical "force these settings on every start" file.
        let user_js = dir.join("user.js");
        let contents = format!(
            r#"// Managed by БелкаТуннель — overwritten on every "Open Firefox via Tunnel" click.
// SOCKS5 proxy
user_pref("network.proxy.type", 1);
user_pref("network.proxy.socks", "{host}");
user_pref("network.proxy.socks_port", {port});
user_pref("network.proxy.socks_version", 5);
user_pref("network.proxy.socks_remote_dns", true);
user_pref("network.proxy.no_proxies_on", "");

// Privacy: always start in Private Browsing
user_pref("browser.privatebrowsing.autostart", true);

// Disable WebRTC so the proxy isn't bypassed by STUN
user_pref("media.peerconnection.enabled", false);

// Don't prefetch DNS or links — would leak outside the proxy
user_pref("network.dns.disablePrefetch", true);
user_pref("network.prefetch-next", false);
user_pref("network.predictor.enabled", false);

// Home page makes it easy to confirm the tunnel works
user_pref("browser.startup.homepage", "https://ifconfig.me/");
user_pref("browser.startup.page", 1);

// Skip first-run dialogs
user_pref("browser.startup.firstrunSkipsHomepage", true);
user_pref("datareporting.policy.firstRunURL", "");
user_pref("trailhead.firstrun.didSeeAboutWelcome", true);

// Less noise
user_pref("browser.shell.checkDefaultBrowser", false);
user_pref("browser.aboutConfig.showWarning", false);
"#,
            host = socks_host,
            port = socks_port
        );
        std::fs::write(&user_js, contents)
            .with_context(|| format!("writing {}", user_js.display()))?;
        info!(dir = %dir.display(), "Firefox profile prepared");

        Ok(Self { dir })
    }

    pub fn launch(&self) -> Result<()> {
        let firefox = find_firefox_binary()
            .ok_or_else(|| anyhow!("Firefox.app not found in /Applications or ~/Applications"))?;
        info!(firefox = %firefox.display(), profile = %self.dir.display(), "launching Firefox with tunnel profile");
        std::process::Command::new(firefox)
            .arg("-profile")
            .arg(self.dir.as_os_str())
            .arg("-no-remote")
            .spawn()
            .context("spawn firefox")?;
        Ok(())
    }
}

fn find_firefox_binary() -> Option<PathBuf> {
    for app in [
        "/Applications/Firefox.app/Contents/MacOS/firefox-bin",
        "/Applications/Firefox.app/Contents/MacOS/firefox",
    ] {
        if Path::new(app).exists() {
            return Some(PathBuf::from(app));
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        for tail in [
            "Applications/Firefox.app/Contents/MacOS/firefox-bin",
            "Applications/Firefox.app/Contents/MacOS/firefox",
        ] {
            let p = PathBuf::from(&home).join(tail);
            if p.exists() {
                return Some(p);
            }
        }
    }
    warn!("could not find Firefox binary in standard locations");
    None
}
