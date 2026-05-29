//! pfUsers — manage pfSense router users from a native macOS app.
//!
//! Talks to the router over SSH, drives `/usr/local/sbin/pfSsh.php` for
//! CRUD on `$config['system']['user']`, and uses the same Zed-inspired
//! visual grammar as BelkaTunnel via the shared `belka_ui` crate.

#![recursion_limit = "256"]

mod config;
mod gui;
mod pfsense;
mod ssh;
mod users;

use anyhow::Result;
use tracing_subscriber::EnvFilter;

fn main() -> Result<()> {
    let _guard = init_tracing();
    gui::run()
}

/// Rolling per-day log file in our ProjectDirs data dir, mirroring
/// BelkaTunnel's setup so the on-disk story is consistent.
fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = directories::ProjectDirs::from("io", "celestialtech", "pfUsers")
        .map(|d| d.data_dir().join("logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let _ = std::fs::create_dir_all(&log_dir);

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,russh=warn"));
    let registry = tracing_subscriber::registry().with(env).with(stderr_layer);

    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("pfusers")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&log_dir);
    let file_appender = match file_appender {
        Ok(a) => a,
        Err(e) => {
            registry.init();
            tracing::error!(error = %e, "could not set up rolling log file; stderr only");
            return None;
        }
    };
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);
    registry.with(file_layer).init();
    tracing::info!(dir = %log_dir.display(), "tracing initialized");
    Some(guard)
}
