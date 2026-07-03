//! Launch-at-login support via a per-user LaunchAgent plist.
//!
//! When enabled, writes `~/Library/LaunchAgents/io.celestialtech.BelkaTunnel.plist`
//! pointing at the running bundle's binary. `RunAtLoad` makes launchd start the
//! daemon on user login; `KeepAlive.SuccessfulExit=false` makes it auto-respawn
//! on crashes but NOT on a clean Quit (so the user clicking Quit means Quit,
//! not "quit briefly then come back").
//!
//! Effect timing: writing the plist takes effect the next time launchd
//! scans the LaunchAgents directory — i.e. on the user's next login.
//! We deliberately don't call `launchctl bootstrap` to immediately load the
//! agent because the user is by definition running the daemon NOW; loading
//! the agent would spawn a second instance that would either collide on the
//! SOCKS port (and hot-loop under KeepAlive) or, if the running instance
//! exits, get respawned. Existing-instance detection in `main` handles
//! both shapes cleanly enough that we could enable immediate-load later,
//! but the simpler model is what shipped.
//!
//! Disable removes the file (and best-effort `launchctl bootout`s a loaded
//! agent so the next login behaves correctly even if the user toggles in
//! the same session as the previous login).

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

const LABEL: &str = "io.celestialtech.BelkaTunnel";

pub fn plist_path() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|home| PathBuf::from(home).join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

/// True iff the LaunchAgent plist file exists. The single source of truth —
/// we don't ask `launchctl list` because that would lie immediately after a
/// fresh enable (plist written but not loaded yet, by design).
pub fn is_enabled() -> bool {
    plist_path().map(|p| p.exists()).unwrap_or(false)
}

/// Write the plist so launchd starts the daemon at the next login.
/// `binary_path` should be the absolute path to the installed bundle's
/// MacOS binary — typically `/Applications/BelkaTunnel.app/Contents/MacOS/belka_tunnel`.
pub fn enable(binary_path: &Path) -> Result<()> {
    let path = plist_path().ok_or_else(|| anyhow!("HOME not set; cannot enable autolaunch"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let plist = build_plist(binary_path);
    atomic_write(&path, plist.as_bytes())?;
    Ok(())
}

/// Remove the plist. Best-effort `launchctl bootout` if the agent is
/// currently loaded so a re-enable in the same session can't end up with
/// the old binary path stuck in launchd's memory.
pub fn disable() -> Result<()> {
    let path = plist_path().ok_or_else(|| anyhow!("HOME not set"))?;
    if !path.exists() {
        return Ok(());
    }
    // Best-effort unload — if it isn't currently loaded, bootout returns
    // nonzero and we just don't care.
    let uid = unsafe { libc::getuid() };
    let _ = std::process::Command::new("/bin/launchctl")
        .args(["bootout", &format!("gui/{uid}/{LABEL}")])
        .output();
    std::fs::remove_file(&path)?;
    Ok(())
}

/// Walk up from `binary_or_exe` to the enclosing `.app` bundle's MacOS
/// binary path. If `binary_or_exe` is already such a path, returns it as-is.
/// If we're running outside a bundle (cargo run, plain target binary), returns
/// the path as-is so the plist still works for development.
pub fn current_bundle_binary() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    // Resolve symlinks so the plist points at the real file. macOS does NOT
    // automatically follow symlinks when launchd resolves ProgramArguments[0];
    // a plist pointing at a stale symlink would silently fail.
    let canonical = std::fs::canonicalize(&exe).unwrap_or(exe);
    Some(canonical)
}

fn build_plist(binary_path: &Path) -> String {
    // XML-escape the path so a bundle in a directory with `<` `>` `&` `"`
    // can't break the plist. Realistically none of those appear in a sane
    // installer path, but the cost of being correct is two replace calls.
    let raw = binary_path.display().to_string();
    let escaped = xml_escape(&raw);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{escaped}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>StandardOutPath</key>
    <string>/tmp/belka-tunnel.out</string>
    <key>StandardErrorPath</key>
    <string>/tmp/belka-tunnel.err</string>
</dict>
</plist>
"#
    )
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Same shape as config::atomic_write — write to sibling .tmp, fsync,
/// rename. We copy it here so the autolaunch module stays independent of
/// the config persistence path (different concerns, different lifetimes).
fn atomic_write(dest: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let tmp = parent.join(format!(
        ".{}.tmp.{}",
        dest.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "plist".to_string()),
        std::process::id()
    ));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    match std::fs::rename(&tmp, dest) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_plist_contains_required_keys_and_escapes() {
        let plist = build_plist(Path::new(
            "/Applications/BelkaTunnel & friends.app/Contents/MacOS/belka_tunnel",
        ));
        // Label + bundle id present (anchors automation that greps for this).
        assert!(plist.contains("<string>io.celestialtech.BelkaTunnel</string>"));
        // Path is XML-escaped — '&' must become '&amp;', the literal '&'
        // would otherwise produce a malformed plist that launchctl rejects.
        assert!(
            plist.contains("BelkaTunnel &amp; friends.app"),
            "ampersand not escaped: {plist}"
        );
        assert!(plist.contains("<key>RunAtLoad</key>"));
        // KeepAlive must be the dict form so SuccessfulExit=false lets a
        // clean Quit actually quit. A `<true/>` here would respawn on Quit.
        assert!(
            plist.contains("<key>KeepAlive</key>\n    <dict>"),
            "KeepAlive should be a dict, not a bare true",
        );
        assert!(plist.contains("<key>SuccessfulExit</key>"));
        assert!(plist.contains("<false/>"));
    }

    #[test]
    fn xml_escape_handles_each_metacharacter() {
        assert_eq!(xml_escape("a & b"), "a &amp; b");
        assert_eq!(xml_escape("<tag>"), "&lt;tag&gt;");
        assert_eq!(xml_escape("\""), "&quot;");
        // Apostrophe is not in our escape set because plist string values
        // we generate use double quotes — single quotes are valid bare.
        assert_eq!(xml_escape("don't"), "don't");
    }

    #[test]
    fn enable_creates_file_atomic_rename_leaves_no_temp() {
        // Redirect HOME at a tempdir so we don't touch the user's real
        // LaunchAgents directory. plist_path() reads HOME each call.
        let dir = tempfile::tempdir().unwrap();
        let prev_home = std::env::var_os("HOME");
        std::env::set_var("HOME", dir.path());

        let binary = Path::new("/Applications/BelkaTunnel.app/Contents/MacOS/belka_tunnel");
        enable(binary).unwrap();
        assert!(is_enabled(), "is_enabled should be true after enable");

        let plist = plist_path().unwrap();
        let body = std::fs::read_to_string(&plist).unwrap();
        assert!(body.contains("/Applications/BelkaTunnel.app"));

        // No orphan .tmp file in the LaunchAgents directory.
        let agents = plist.parent().unwrap();
        for entry in std::fs::read_dir(agents).unwrap() {
            let name = entry.unwrap().file_name();
            let name = name.to_string_lossy().into_owned();
            assert!(
                !name.starts_with(".") || name == ".",
                "orphan temp file left behind: {name}",
            );
        }

        // disable() removes the file.
        disable().unwrap();
        assert!(!is_enabled(), "is_enabled should be false after disable");

        if let Some(h) = prev_home {
            std::env::set_var("HOME", h);
        } else {
            std::env::remove_var("HOME");
        }
    }
}
