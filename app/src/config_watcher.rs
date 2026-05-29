//! Watches the persisted `config.json` for changes. When the file is
//! rewritten (e.g. by the GUI editor's "Save" or a hand-edit), an event is
//! posted to the menu-bar event loop so the daemon can self-restart and
//! pick up the new settings — no manual click on "Restart" required.
//!
//! Uses `notify`'s FSEvents backend on macOS, which is what Finder + every
//! native app uses. Coalesces multiple events fired within a short window
//! (atomic-renames from a GUI editor fire CREATE + REMOVE in quick
//! succession) into a single restart trigger.

use anyhow::Result;
use notify::{event::ModifyKind, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::Path;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

/// Spawns a dedicated thread that watches `config_path` and calls `on_change`
/// (once per coalesced edit) whenever the file is modified. Returns the
/// `Watcher` handle — the caller must keep it alive (the watcher stops when
/// dropped).
pub fn spawn(
    config_path: &Path,
    on_change: impl Fn() + Send + 'static,
) -> Result<RecommendedWatcher> {
    let (tx, rx) = channel::<notify::Result<Event>>();
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    )?;

    // Watch the parent directory rather than the file itself — atomic
    // renames (the common save pattern: write to .tmp, rename onto target)
    // would otherwise leave us listening on a vanished inode.
    let parent = config_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("config path has no parent"))?;
    let target_filename = config_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("config path has no filename"))?
        .to_os_string();
    watcher.watch(parent, RecursiveMode::NonRecursive)?;

    std::thread::Builder::new()
        .name("config-watcher".into())
        .spawn(move || {
            // Coalesce rapid event bursts into a single fire — atomic-rename
            // editors emit several events back-to-back.
            let mut last_fired: Option<Instant> = None;
            let coalesce = Duration::from_millis(400);
            while let Ok(res) = rx.recv() {
                let Ok(event) = res else { continue };
                // Only care about edits to our target file.
                if !event
                    .paths
                    .iter()
                    .any(|p| p.file_name() == Some(&target_filename))
                {
                    continue;
                }
                let interesting = matches!(
                    event.kind,
                    EventKind::Modify(ModifyKind::Data(_))
                        | EventKind::Modify(ModifyKind::Any)
                        | EventKind::Create(_)
                        | EventKind::Modify(ModifyKind::Name(_))
                );
                if !interesting {
                    continue;
                }
                let now = Instant::now();
                if let Some(prev) = last_fired {
                    if now.duration_since(prev) < coalesce {
                        continue;
                    }
                }
                last_fired = Some(now);
                on_change();
            }
        })?;

    Ok(watcher)
}

#[cfg(test)]
mod tests {
    //! FSEvents-based tests are inherently timing-dependent: the kernel
    //! batches and delivers events asynchronously. We use generous
    //! per-event waits (~2s) so the suite stays green on a loaded CI
    //! runner, and rely on an AtomicUsize callback counter rather than
    //! a single Notified so we can distinguish "fired once" from
    //! "fired N times" for the coalescing test.
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::time::Duration;
    use tempfile::tempdir;

    /// Spin-wait up to `max` for `pred()` to be true. Returns the final
    /// value of `pred()`.
    fn wait_until(max: Duration, pred: impl Fn() -> bool) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed() < max {
            if pred() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        pred()
    }

    #[test]
    fn write_triggers_callback() {
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let count_w = count.clone();
        let _watcher = spawn(&cfg, move || {
            count_w.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();

        // Give FSEvents a moment to start observing before we touch the file —
        // otherwise the write can race the watch and we see no event.
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&cfg, r#"{"k":1}"#).unwrap();

        assert!(
            wait_until(Duration::from_secs(2), || count.load(Ordering::SeqCst) >= 1),
            "callback never fired",
        );
    }

    #[test]
    fn coalesces_burst_of_writes_into_single_fire() {
        // The watcher should de-dupe a burst inside its 400 ms window so
        // a single Save click in the GUI editor doesn't trigger multiple
        // self-restarts.
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let count_w = count.clone();
        let _watcher = spawn(&cfg, move || {
            count_w.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));

        // Five rapid writes within 100 ms — well inside the 400 ms coalesce.
        for i in 0..5 {
            std::fs::write(&cfg, format!(r#"{{"i":{i}}}"#)).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }

        // Wait past the coalesce window + FSEvents latency.
        std::thread::sleep(Duration::from_millis(900));
        let fired = count.load(Ordering::SeqCst);
        assert!(
            fired >= 1,
            "coalesced burst should fire at least once (fired={fired})"
        );
        // The hard ceiling we care about: the burst above isn't 5× the
        // user-visible restart; it's one save action.
        assert!(
            fired <= 2,
            "burst within coalesce window fired too many times (fired={fired})"
        );
    }

    #[test]
    fn unrelated_file_changes_are_ignored() {
        // Files in the watched parent dir that aren't the target should not
        // fire the callback — otherwise an editor saving a sibling .swp or
        // backup file would self-restart the daemon for nothing.
        let dir = tempdir().unwrap();
        let cfg = dir.path().join("config.json");
        std::fs::write(&cfg, "{}").unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let count_w = count.clone();
        let _watcher = spawn(&cfg, move || {
            count_w.fetch_add(1, Ordering::SeqCst);
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(200));

        // Write a sibling file the daemon doesn't care about.
        std::fs::write(dir.path().join("other.json"), "{}").unwrap();
        std::fs::write(dir.path().join("config.json.swp"), b"vim swap").unwrap();

        std::thread::sleep(Duration::from_millis(700));
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "callback fired for unrelated sibling files"
        );
    }
}
