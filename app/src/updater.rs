//! Lightweight GitHub-Releases-based update check.
//!
//! On startup we ask the GitHub API for the latest release of the
//! belka-tunnel repo, compare its tag against our compiled-in version, and if
//! something newer is published, surface a single tray-bound `UpdateInfo` to
//! the menu-bar code so it can notify the user. No background download, no
//! self-replace — we just open the release page in the user's browser when
//! they click. Keeps the install path identical to a fresh download.
//!
//! HTTP is shelled out to `curl` (already present on macOS and already used
//! by `firefox::install_firefox_direct`) to avoid pulling a heavy HTTP
//! client into the binary.

use anyhow::{anyhow, Context, Result};
use semver::Version;
use serde::Deserialize;
use tracing::{debug, info, warn};

/// Default upstream — can be overridden at runtime via the `BELKA_UPDATE_URL`
/// env var (used by tests).
pub const RELEASES_URL: &str = "https://api.github.com/repos/jmpnop/belka-tunnel/releases/latest";

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub latest_version: String,
    pub release_url: String,
    pub asset_url: Option<String>,
}

#[derive(Deserialize)]
struct ReleaseResponse {
    tag_name: String,
    html_url: String,
    #[serde(default)]
    assets: Vec<Asset>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

/// Fetch the latest GitHub release and compare to `current_version`.
/// Returns `Ok(Some(info))` only when a stable release strictly newer than
/// the current build is available.
pub fn check_for_update(current_version: &str) -> Result<Option<UpdateInfo>> {
    let url = std::env::var("BELKA_UPDATE_URL").unwrap_or_else(|_| RELEASES_URL.to_string());
    let body = http_get(&url)?;
    let release: ReleaseResponse = serde_json::from_str(&body)
        .with_context(|| format!("parse GitHub release JSON from {url}"))?;

    if release.draft || release.prerelease {
        debug!(tag = %release.tag_name, "ignoring draft/prerelease");
        return Ok(None);
    }

    let latest = parse_version(&release.tag_name)
        .with_context(|| format!("parse tag {}", release.tag_name))?;
    let current = parse_version(current_version)
        .with_context(|| format!("parse current version {current_version}"))?;
    if latest <= current {
        info!(latest = %latest, current = %current, "no update available");
        return Ok(None);
    }

    let asset_url = release
        .assets
        .into_iter()
        .find(|a| a.name.ends_with(".dmg") && a.name.contains("BelkaTunnel"))
        .map(|a| a.browser_download_url);

    Ok(Some(UpdateInfo {
        latest_version: release.tag_name,
        release_url: release.html_url,
        asset_url,
    }))
}

/// `vX.Y.Z` and `X.Y.Z` are both accepted.
pub fn parse_version(s: &str) -> Result<Version> {
    let trimmed = s.trim().trim_start_matches('v');
    Version::parse(trimmed).map_err(|e| anyhow!("not a semver: {trimmed} ({e})"))
}

fn http_get(url: &str) -> Result<String> {
    // `-A` matters: GitHub rejects requests with no UA. `--fail` makes
    // non-2xx codes exit non-zero so we don't try to parse error HTML.
    let out = std::process::Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "-A",
            "belka-tunnel-updater/0.1",
            "-H",
            "Accept: application/vnd.github+json",
            url,
        ])
        .output()
        .context("spawn curl")?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        warn!(url, code = ?out.status.code(), stderr = %stderr, "curl failed");
        return Err(anyhow!("curl returned {}", out.status));
    }
    String::from_utf8(out.stdout).context("response not utf8")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_version_with_or_without_v() {
        assert_eq!(parse_version("0.1.2").unwrap(), Version::new(0, 1, 2));
        assert_eq!(parse_version("v0.1.2").unwrap(), Version::new(0, 1, 2));
        assert_eq!(
            parse_version("v0.1.2-beta.1").unwrap().to_string(),
            "0.1.2-beta.1"
        );
        assert!(parse_version("not a version").is_err());
    }
}
