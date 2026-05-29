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
    // `--max-time` caps the *total* request (connect + transfer + TLS)
    // so a hung GitHub response doesn't pin our blocking thread for the
    // rest of the process lifetime. 30s is generous for a few-KB JSON
    // payload on any connection we'd realistically run on.
    let out = std::process::Command::new("/usr/bin/curl")
        .args([
            "--silent",
            "--show-error",
            "--fail",
            "--location",
            "--max-time",
            "30",
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
    //! Tests use `file://` URLs against a tempdir to exercise the full curl
    //! → parse path without hitting the network. curl accepts file:// natively.
    //! NOTE: these tests touch a process-wide env var (`BELKA_UPDATE_URL`),
    //! so they MUST NOT run concurrently with each other. A static mutex
    //! enforces serialization.
    use super::*;
    use std::sync::Mutex;
    use tempfile::tempdir;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn write_release(path: &std::path::Path, tag: &str, draft: bool, prerelease: bool) -> String {
        let body = format!(
            r#"{{
                "tag_name": "{tag}",
                "html_url": "https://example.test/release/{tag}",
                "draft": {draft},
                "prerelease": {prerelease},
                "assets": [
                    {{
                        "name": "BelkaTunnel-{tag}.dmg",
                        "browser_download_url": "https://example.test/dl/BelkaTunnel-{tag}.dmg"
                    }},
                    {{
                        "name": "other-asset.txt",
                        "browser_download_url": "https://example.test/dl/other-asset.txt"
                    }}
                ]
            }}"#
        );
        std::fs::write(path, &body).unwrap();
        format!("file://{}", path.display())
    }

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

    #[test]
    fn check_returns_update_when_remote_is_newer() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("release.json");
        let url = write_release(&path, "v0.2.0", false, false);
        std::env::set_var("BELKA_UPDATE_URL", &url);
        let info = check_for_update("0.1.0").unwrap();
        std::env::remove_var("BELKA_UPDATE_URL");
        let info = info.expect("expected Some(UpdateInfo)");
        assert_eq!(info.latest_version, "v0.2.0");
        assert_eq!(info.release_url, "https://example.test/release/v0.2.0");
        // Asset filter picks the .dmg that contains 'BelkaTunnel' — not the
        // .txt sibling.
        assert_eq!(
            info.asset_url.as_deref(),
            Some("https://example.test/dl/BelkaTunnel-v0.2.0.dmg")
        );
    }

    #[test]
    fn check_returns_none_when_remote_is_equal_or_older() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("release.json");
        let url = write_release(&path, "v0.1.0", false, false);
        std::env::set_var("BELKA_UPDATE_URL", &url);
        let same = check_for_update("0.1.0").unwrap();
        assert!(same.is_none(), "equal versions should not advertise update");
        let url2 = write_release(&path, "v0.0.1", false, false);
        std::env::set_var("BELKA_UPDATE_URL", &url2);
        let older = check_for_update("0.1.0").unwrap();
        std::env::remove_var("BELKA_UPDATE_URL");
        assert!(older.is_none(), "older remote should not advertise update");
    }

    #[test]
    fn check_skips_drafts_and_prereleases() {
        let _lock = ENV_LOCK.lock().unwrap();
        let dir = tempdir().unwrap();
        let path = dir.path().join("release.json");

        let url = write_release(&path, "v0.2.0", true, false); // draft
        std::env::set_var("BELKA_UPDATE_URL", &url);
        assert!(
            check_for_update("0.1.0").unwrap().is_none(),
            "draft release leaked through"
        );

        let url = write_release(&path, "v0.2.0", false, true); // prerelease
        std::env::set_var("BELKA_UPDATE_URL", &url);
        assert!(
            check_for_update("0.1.0").unwrap().is_none(),
            "prerelease leaked through"
        );
        std::env::remove_var("BELKA_UPDATE_URL");
    }

    #[test]
    fn check_surfaces_clear_error_on_unreachable_url() {
        let _lock = ENV_LOCK.lock().unwrap();
        // file:// path that doesn't exist — curl returns nonzero, http_get bails.
        std::env::set_var("BELKA_UPDATE_URL", "file:///dev/null/does-not-exist");
        let r = check_for_update("0.1.0");
        std::env::remove_var("BELKA_UPDATE_URL");
        assert!(r.is_err(), "expected error on unreachable URL");
    }
}
