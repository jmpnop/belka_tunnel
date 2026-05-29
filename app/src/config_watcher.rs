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
