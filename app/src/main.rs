#![recursion_limit = "512"]

mod about;
mod config;
mod config_watcher;
mod firefox;
mod gui;
mod socks;
mod tunnel;
mod updater;

use anyhow::Result;
use std::sync::Arc;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

/// Cross-thread guard so double-clicking destructive actions doesn't fan
/// out into races (Firefox install, Homebrew bootstrap, relaunch).
static FIREFOX_INSTALL_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static HOMEBREW_INSTALL_BUSY: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
static RELAUNCH_BUSY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// RAII: clears an AtomicBool when dropped. Survives panics inside a worker
/// thread so a `panic!` mid-install can't permanently disable the menu item.
pub struct BusyFlagGuard(pub &'static std::sync::atomic::AtomicBool);

impl Drop for BusyFlagGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

const BT_GREEN_PNG: &[u8] = include_bytes!("../assets/bt-green.png");
const BT_ORANGE_PNG: &[u8] = include_bytes!("../assets/bt-orange.png");
const BT_RED_PNG: &[u8] = include_bytes!("../assets/bt-red.png");
const BT_GRAY_PNG: &[u8] = include_bytes!("../assets/bt-gray.png");

use crate::config::{ConfigFile, Profile};
use crate::tunnel::Status;

#[derive(Debug, Clone)]
enum UserEvent {
    StatusChanged(Status),
    UpdateAvailable(updater::UpdateInfo),
    /// `config.json` was rewritten on disk — self-restart to pick it up.
    ConfigChanged,
}

fn main() -> Result<()> {
    // Hold the WorkerGuard for the entire process lifetime so the non-blocking
    // log writer thread keeps draining.
    let _log_guard = init_tracing();

    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--gui") {
        return gui::run();
    }
    if args.iter().any(|a| a == "--about") {
        return about::run();
    }

    let file = ConfigFile::load_or_default()?;
    if let Some(p) = ConfigFile::default_path() {
        info!(path = %p.display(), "config path");
    }
    let profile = Arc::new(file.active_profile()?.clone());

    info!(
        active = %file.active,
        host = %profile.ssh.host,
        port = profile.ssh.port,
        user = %profile.ssh.user,
        socks = format!("{}:{}", profile.socks.listen_addr, profile.socks.listen_port),
        "starting БелкаТуннель"
    );

    // Single tokio runtime hosts BOTH the tunnel and the status-bridge task.
    // Previously the status bridge spun up a second current_thread runtime in
    // its own std::thread just to await a watch channel; that was an entire
    // executor + I/O driver for one .changed() call. The handle is cloned for
    // the bridge spawn before the rt itself moves into the tunnel thread.
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("belka-tokio")
        .build()
        .expect("build tokio runtime");
    let rt_handle = rt.handle().clone();

    let (status_tx, status_rx) = tokio::sync::watch::channel(Status::Connecting);
    let tunnel_ctl = tunnel::Control::new(true);

    // tao event loop must run on the main thread for macOS tray to work.
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Status bridge — spawned as a regular tokio task on the shared runtime
    // BEFORE we move rt into the tunnel thread. The runtime's workers are
    // already alive by this point, so the task starts polling immediately.
    rt_handle.spawn({
        let proxy = proxy.clone();
        async move {
            let mut rx = status_rx;
            let initial = rx.borrow().clone();
            let _ = proxy.send_event(UserEvent::StatusChanged(initial));
            while rx.changed().await.is_ok() {
                let s = rx.borrow().clone();
                let _ = proxy.send_event(UserEvent::StatusChanged(s));
            }
        }
    });

    // Update check — single-shot on startup, runs on a dedicated blocking
    // thread so the curl subprocess doesn't tie up a tokio worker. If a
    // newer release is found, the menu loop gets `UserEvent::UpdateAvailable`
    // and surfaces a notification.
    {
        let proxy = proxy.clone();
        let current = env!("CARGO_PKG_VERSION").to_string();
        rt_handle.spawn_blocking(move || match updater::check_for_update(&current) {
            Ok(Some(info)) => {
                info!(latest = %info.latest_version, "update available");
                let _ = proxy.send_event(UserEvent::UpdateAvailable(info));
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(error = %e, "update check failed (offline?)"),
        });
    }

    // Tunnel runs the runtime's block_on on a dedicated std::thread so it
    // owns the rt for its lifetime; when the tunnel exits, dropping rt aborts
    // any remaining spawned tasks (notably the status bridge).
    {
        let profile = profile.clone();
        let status_tx = status_tx.clone();
        let tunnel_ctl = tunnel_ctl.clone();
        std::thread::Builder::new()
            .name("belka-tunnel-rt".into())
            .spawn(move || {
                rt.block_on(tunnel::run_forever(profile, status_tx, tunnel_ctl));
            })?;
    }

    // Resolve paths used by menu items.
    let config_path = ConfigFile::default_path()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/proxy-tunnel-config.json"));

    // File-system watcher on config.json. When the GUI editor saves (or you
    // hand-edit), an event lands in the main event loop and the app
    // self-restarts to pick up the new settings. The Watcher must outlive
    // the event loop, so we leak it via Box::leak — the process exits when
    // the user picks Quit, so leaking is fine.
    let watcher_proxy = proxy.clone();
    if let Some(parent) = config_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match config_watcher::spawn(&config_path, move || {
        let _ = watcher_proxy.send_event(UserEvent::ConfigChanged);
    }) {
        Ok(w) => {
            Box::leak(Box::new(w));
            info!(path = %config_path.display(), "watching config for changes");
        }
        Err(e) => warn!(error = %e, "could not start config watcher"),
    }

    let log_path = directories::ProjectDirs::from("io", "celestialtech", "BelkaTunnel")
        .map(|d| d.data_dir().join("logs").join("proxy-tunnel.log"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/proxy-tunnel.log"));
    let data_dir = config_path
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| std::path::PathBuf::from("/"));

    // Build the tray menu.
    let menu = Menu::new();

    let header_item = MenuItem::new(
        format!(
            "Profile: {}  ({}@{}:{})",
            file.active, profile.ssh.user, profile.ssh.host, profile.ssh.port
        ),
        false,
        None,
    );
    // The status line is a CheckMenuItem so its checkmark aligns with the
    // system checkmark column — same horizontal position as the "Use neutral
    // status icon" and "Listen on all interfaces" check rows below. When
    // Connected, the native ✓ shows. Other states leave the slot empty and
    // describe themselves in the text. Clicking still toggles the tunnel.
    let status_item = CheckMenuItem::new("Connecting…", true, false, None);
    let socks_header = MenuItem::new("SOCKS5 endpoints (clients connect here):", false, None);
    let endpoints = collect_endpoints(&profile.socks.listen_addr, profile.socks.listen_port);
    let mut endpoint_items: Vec<MenuItem> = endpoints
        .iter()
        .map(|e| MenuItem::new(format!("  • {e}"), false, None))
        .collect();
    let copy_endpoints_item = MenuItem::new(
        "Copy primary endpoint to clipboard",
        !endpoints.is_empty(),
        None,
    );

    let listen_all = profile.socks.listen_addr == "0.0.0.0";
    let listen_all_item = CheckMenuItem::new("Listen on all interfaces", true, listen_all, None);

    // ---------- Firefox submenu (dynamic on Firefox state) ----------
    let firefox_info = firefox::detect();
    let firefox_submenu = Submenu::new("Browse via tunnel (Firefox)", true);

    let ff_status_label = match (&firefox_info.path, &firefox_info.version) {
        (Some(p), Some(v)) => format!("Firefox {} — {}", v, p.display()),
        (Some(p), None) => format!("Firefox installed at {}", p.display()),
        (None, _) => "Firefox is not installed".to_string(),
    };
    let firefox_status_item = MenuItem::new(ff_status_label, false, None);
    firefox_submenu.append(&firefox_status_item)?;
    firefox_submenu.append(&PredefinedMenuItem::separator())?;

    let open_firefox_item = MenuItem::new("Launch Firefox", firefox_info.installed(), None);
    // Firefox install is a self-contained DMG download — no Homebrew, no
    // browser. The label switches between fresh-install and update-in-place.
    let install_firefox_item = MenuItem::new(
        if firefox_info.installed() {
            "Reinstall / update Firefox (latest from mozilla.org)"
        } else {
            "Install Firefox (latest from mozilla.org)"
        },
        true,
        None,
    );
    // Homebrew install is shown only when brew is missing — it's an unrelated
    // utility that some users may also want.
    let install_homebrew_item = MenuItem::new(
        "Install Homebrew (opens Terminal)",
        firefox_info.brew.is_none(),
        None,
    );
    let uninstall_firefox_item =
        MenuItem::new("Uninstall Firefox…", firefox_info.installed(), None);

    firefox_submenu.append(&open_firefox_item)?;
    firefox_submenu.append(&PredefinedMenuItem::separator())?;
    firefox_submenu.append(&install_firefox_item)?;
    if firefox_info.brew.is_none() {
        firefox_submenu.append(&install_homebrew_item)?;
    }
    firefox_submenu.append(&uninstall_firefox_item)?;
    let edit_config_item = MenuItem::new("Edit Configuration…", true, None);
    let reveal_data_item = MenuItem::new("Reveal Data Folder in Finder", true, None);
    let open_logs_item = MenuItem::new("Open Log File", true, None);
    let about_item = MenuItem::new("О БелкаТуннеле", true, None);
    let quit_item = MenuItem::new("Quit", true, None);

    menu.append(&header_item)?;
    menu.append(&status_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&socks_header)?;
    for ep in &endpoint_items {
        menu.append(ep)?;
    }
    menu.append(&copy_endpoints_item)?;
    menu.append(&listen_all_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&firefox_submenu)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&edit_config_item)?;
    menu.append(&reveal_data_item)?;
    menu.append(&open_logs_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&about_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let icon_green = decode_icon(BT_GREEN_PNG)?;
    let icon_orange = decode_icon(BT_ORANGE_PNG)?;
    let icon_red = decode_icon(BT_RED_PNG)?;
    let icon_gray = decode_icon(BT_GRAY_PNG)?;

    // Menu bar: colored cyrillic "БТ" icon for status. Orange on startup.
    let mut tray = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(icon_orange.clone())
            .with_icon_as_template(false)
            .with_tooltip("БелкаТуннель")
            .build()?,
    );

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayIconEvent::receiver();
    let quit_id = quit_item.id().clone();
    let about_id = about_item.id().clone();
    let open_firefox_id = open_firefox_item.id().clone();
    let edit_config_id = edit_config_item.id().clone();
    let reveal_data_id = reveal_data_item.id().clone();
    let open_logs_id = open_logs_item.id().clone();
    let copy_endpoints_id = copy_endpoints_item.id().clone();
    let listen_all_id = listen_all_item.id().clone();
    let status_toggle_id = status_item.id().clone();
    let install_firefox_id = install_firefox_item.id().clone();
    let install_homebrew_id = install_homebrew_item.id().clone();
    let uninstall_firefox_id = uninstall_firefox_item.id().clone();
    let socks_host_for_firefox = if profile.socks.listen_addr == "0.0.0.0" {
        "127.0.0.1".to_string()
    } else {
        profile.socks.listen_addr.clone()
    };
    let socks_port_for_firefox = profile.socks.listen_port;
    let primary_endpoint = endpoints.first().cloned();
    let _endpoint_items_keep_alive = endpoint_items.split_off(0); // hold them so they stay in the menu
    drop(_endpoint_items_keep_alive); // we appended already; let muda own them via the menu

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        while let Ok(event) = menu_channel.try_recv() {
            if event.id == quit_id {
                info!("quit requested");
                tray.take();
                *control_flow = ControlFlow::Exit;
            } else if event.id == about_id {
                spawn_about();
            } else if event.id == status_toggle_id {
                let new_enabled = !tunnel_ctl.is_enabled();
                tunnel_ctl.set_enabled(new_enabled);
                info!(enabled = new_enabled, "tunnel toggle (via status click)");
            } else if event.id == open_firefox_id {
                if let Err(e) = firefox::launch_default() {
                    error!(error = %e, "Firefox launch failed");
                    macos_alert(
                        "Couldn't launch Firefox",
                        &format!("{e}\n\nUse the Firefox submenu → Install Firefox to fetch it."),
                    );
                }
            } else if event.id == install_firefox_id {
                use std::sync::atomic::Ordering;
                if FIREFOX_INSTALL_BUSY
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    notify_user(
                        "Firefox install already running",
                        "Wait for the current install to finish before starting another.",
                    );
                } else {
                    // Pass the RAII guard into the spawned install thread —
                    // the flag clears on Drop regardless of how the thread
                    // exits (success / error / panic).
                    let guard = BusyFlagGuard(&FIREFOX_INSTALL_BUSY);
                    if let Err(e) = firefox::install_or_update_async(
                        socks_host_for_firefox.clone(),
                        socks_port_for_firefox,
                        notify_user,
                        guard,
                    ) {
                        // guard already dropped on Err path before we get here;
                        // explicitly clear in case the function returned without
                        // moving the guard into a thread.
                        FIREFOX_INSTALL_BUSY.store(false, Ordering::Release);
                        macos_alert("Couldn't start Firefox install", &format!("{e}"));
                    }
                }
            } else if event.id == install_homebrew_id {
                use std::sync::atomic::Ordering;
                if HOMEBREW_INSTALL_BUSY
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    notify_user(
                        "Homebrew install already running",
                        "A poller is already watching for brew to appear.",
                    );
                } else {
                    firefox::install_homebrew_async(
                        notify_user,
                        BusyFlagGuard(&HOMEBREW_INSTALL_BUSY),
                    );
                }
            } else if event.id == uninstall_firefox_id {
                if macos_confirm(
                    "Uninstall Firefox?",
                    "This will remove Firefox.app from /Applications. \
                     Your tunnel-private Firefox profile (preferences, history, \
                     bookmarks for that profile) will stay in БелкаТуннель's \
                     data folder.",
                    "Uninstall",
                ) {
                    if let Err(e) = firefox::uninstall_async(notify_user) {
                        macos_alert("Couldn't uninstall Firefox", &format!("{e}"));
                    }
                }
            } else if event.id == edit_config_id {
                spawn_gui();
            } else if event.id == reveal_data_id {
                reveal_in_finder(&data_dir);
            } else if event.id == open_logs_id {
                ensure_file_exists(&log_path);
                open_path(&log_path);
            } else if event.id == copy_endpoints_id {
                if let Some(ep) = &primary_endpoint {
                    copy_to_clipboard(ep);
                }
            } else if event.id == listen_all_id {
                let new_addr = if listen_all_item.is_checked() {
                    "0.0.0.0"
                } else {
                    "127.0.0.1"
                };
                if let Err(e) = update_listen_addr(&config_path, new_addr) {
                    error!(error = %e, "could not update listen addr");
                } else {
                    info!(addr = new_addr, "listen address updated; restarting");
                    tray.take();
                    relaunch_self();
                    *control_flow = ControlFlow::Exit;
                }
            }
        }
        while let Ok(_e) = tray_channel.try_recv() {}

        // Single match — the two arms previously written as separate `if let`s
        // would have both tried to move out of `event`.
        let user_evt = match event {
            Event::UserEvent(u) => Some(u),
            _ => None,
        };
        if let Some(UserEvent::StatusChanged(status)) = &user_evt {
            // Status text — the native check column is rendered by macOS when
            // `set_checked(true)`. For other states the text describes itself.
            let (icon_for_status, body, checked) = match status {
                Status::Connecting => (
                    &icon_orange,
                    format!("Connecting…  {}", profile.ssh.host),
                    false,
                ),
                Status::Connected => (&icon_green, profile.ssh.host.clone(), true),
                Status::Disconnected(_) => (
                    &icon_red,
                    format!("Disconnected — {}", profile.ssh.host),
                    false,
                ),
                Status::Disabled => (&icon_gray, format!("Off — {}", profile.ssh.host), false),
            };
            status_item.set_checked(checked);
            let tooltip_action = match &status {
                Status::Disabled | Status::Disconnected(_) => "click to connect",
                _ => "click to disconnect",
            };
            let tooltip = match &status {
                Status::Disconnected(err) => format!(
                    "БелкаТуннель — {}\n{err}\n({tooltip_action})",
                    profile.ssh.host
                ),
                _ => format!("БелкаТуннель — {} ({tooltip_action})", profile.ssh.host),
            };
            if let Some(t) = tray.as_ref() {
                let _ = t.set_icon(Some(icon_for_status.clone()));
                let _ = t.set_tooltip(Some(tooltip));
            }
            status_item.set_text(&body);
        }

        if let Some(UserEvent::ConfigChanged) = &user_evt {
            // Coalesce: if the watcher fires more than once (e.g. during a
            // multi-step save), the relaunch guard above prevents a double
            // exit. Notification gives the user a heads-up.
            use std::sync::atomic::Ordering;
            if RELAUNCH_BUSY
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                info!("config changed on disk; self-restarting");
                notify_user("Config updated", "Applying the new БелкаТуннель settings…");
                tray.take();
                relaunch_self();
                *control_flow = ControlFlow::Exit;
            }
        }

        if let Some(UserEvent::UpdateAvailable(info)) = &user_evt {
            // One-shot user notification with the new version. Clicking it
            // doesn't open a URL by itself (Notification Center doesn't pipe
            // taps back to non-NSUserNotification apps), so we ALSO open the
            // release page directly — the user can choose to ignore it.
            let title = format!("БелкаТуннель {} is available", info.latest_version);
            let body = format!(
                "You're on {}. Click to view release notes / download.",
                env!("CARGO_PKG_VERSION")
            );
            notify_user(&title, &body);
            let url = info
                .asset_url
                .clone()
                .unwrap_or_else(|| info.release_url.clone());
            let _ = std::process::Command::new("/usr/bin/open")
                .arg(&url)
                .spawn();
        }
    });
}

/// Initialise tracing. Returns a `WorkerGuard` that must be held alive for
/// the lifetime of the process — dropping it flushes the non-blocking file
/// writer. Also installs daily rotation (kept files capped at 7) so a
/// long-running tunnel doesn't grow an unbounded log.
fn init_tracing() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = directories::ProjectDirs::from("io", "celestialtech", "BelkaTunnel")
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

    // Daily-rotated file writer; tracing-appender's non-blocking layer drains
    // into a background thread via a lock-free MPSC, so per-event tracing in
    // hot async code no longer contends on a Mutex<File> across the tokio
    // worker pool.
    let file_appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("proxy-tunnel")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&log_dir);
    let file_appender = match file_appender {
        Ok(a) => a,
        Err(e) => {
            registry.init();
            error!(error = %e, "could not set up rolling log file; stderr only");
            return None;
        }
    };
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(non_blocking)
        .with_ansi(false)
        .with_target(true);
    registry.with(file_layer).init();
    info!(dir = %log_dir.display(), "tracing initialized (rolling, non-blocking)");
    Some(guard)
}

/// Enumerate all sockets that match the configured listen address and produce
/// human-readable `host:port` strings clients can use to reach the proxy.
/// Skips noisy stuff: IPv6 link-local (fe80::), AWDL/utun/anpi pseudo-interfaces.
fn collect_endpoints(listen_addr: &str, port: u16) -> Vec<String> {
    let ifaces = local_ip_address::list_afinet_netifas().unwrap_or_default();
    collect_endpoints_from(listen_addr, port, ifaces)
}

/// Pure filtering logic — accepts an iterator of `(interface_name, IpAddr)`
/// so it can be unit-tested without depending on the actual system interface
/// list. See `tests::collect_endpoints_*`.
fn collect_endpoints_from(
    listen_addr: &str,
    port: u16,
    ifaces: impl IntoIterator<Item = (String, std::net::IpAddr)>,
) -> Vec<String> {
    if listen_addr != "0.0.0.0" && listen_addr != "::" {
        return vec![format!("{listen_addr}:{port}")];
    }
    let mut out: Vec<String> = vec![format!("127.0.0.1:{port}  (loopback)")];
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert("127.0.0.1".to_string());
    for (name, ip) in ifaces {
        if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
            continue;
        }
        let lower_name = name.to_lowercase();
        if lower_name.starts_with("utun")
            || lower_name.starts_with("awdl")
            || lower_name.starts_with("llw")
            || lower_name.starts_with("anpi")
            || lower_name.starts_with("ap")
            || lower_name.starts_with("bridge")
        {
            continue;
        }
        // IPv4 only — IPv6 endpoints are noise for this use case.
        if let std::net::IpAddr::V4(v) = ip {
            let key = v.to_string();
            if seen.insert(key) {
                out.push(format!("{v}:{port}  ({name})"));
            }
        }
    }
    out
}

fn open_path(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("/usr/bin/open")
        .arg(path.as_os_str())
        .spawn()
    {
        error!(path = %path.display(), error = %e, "open failed");
    }
}

fn reveal_in_finder(path: &std::path::Path) {
    if let Err(e) = std::process::Command::new("/usr/bin/open")
        .arg("-R")
        .arg(path.as_os_str())
        .spawn()
    {
        error!(path = %path.display(), error = %e, "reveal in finder failed");
    }
}

fn ensure_file_exists(path: &std::path::Path) {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path);
    }
}

fn copy_to_clipboard(s: &str) {
    use std::io::Write;
    if let Ok(mut child) = std::process::Command::new("/usr/bin/pbcopy")
        .stdin(std::process::Stdio::piped())
        .spawn()
    {
        if let Some(stdin) = child.stdin.as_mut() {
            let _ = stdin.write_all(s.as_bytes());
        }
        let _ = child.wait();
    }
}

fn update_listen_addr(config_path: &std::path::Path, new_addr: &str) -> Result<()> {
    let mut file = ConfigFile::load(config_path)?;
    if let Some(p) = file.profiles.get_mut(&file.active) {
        p.socks.listen_addr = new_addr.to_string();
    }
    file.save(config_path)?;
    Ok(())
}

/// Escape arbitrary text for safe inclusion inside an AppleScript string
/// literal. Handles every character that would otherwise break the parse:
/// backslash, double-quote, newline, carriage return.
pub fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

fn macos_alert(title: &str, body: &str) {
    let esc = applescript_escape;
    let script = format!(
        r#"display alert "{}" message "{}" as critical buttons {{"OK"}} default button "OK""#,
        esc(title),
        esc(body)
    );
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .spawn();
}

/// Modal Yes/No confirmation. Returns true if the user clicked `action_label`.
/// Default + cancel button is "Cancel".
fn macos_confirm(title: &str, body: &str, action_label: &str) -> bool {
    let esc = applescript_escape;
    let script = format!(
        r#"set r to display alert "{}" message "{}" buttons {{"Cancel", "{}"}} default button "Cancel" cancel button "Cancel" as critical
return button returned of r"#,
        esc(title),
        esc(body),
        esc(action_label)
    );
    let out = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .output();
    match out {
        Ok(o) => {
            let answer = String::from_utf8_lossy(&o.stdout).trim().to_string();
            answer == action_label
        }
        Err(_) => false,
    }
}

/// Non-modal user notification — appears in the Notification Center.
fn notify_user(title: &str, body: &str) {
    let esc = applescript_escape;
    let script = format!(
        r#"display notification "{}" with title "{}""#,
        esc(body),
        esc(title)
    );
    let _ = std::process::Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(&script)
        .spawn();
}

fn decode_icon(bytes: &[u8]) -> Result<Icon> {
    let img = image::load_from_memory(bytes)
        .map_err(|e| anyhow::anyhow!("decode tray icon: {e}"))?
        .to_rgba8();
    let (w, h) = img.dimensions();
    Icon::from_rgba(img.into_raw(), w, h).map_err(|e| anyhow::anyhow!("build tray icon: {e}"))
}

fn spawn_gui() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let _ = std::process::Command::new(exe).arg("--gui").spawn();
}

fn spawn_about() {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(_) => return,
    };
    let _ = std::process::Command::new(exe).arg("--about").spawn();
}

fn relaunch_self() {
    let exe = std::env::current_exe().ok();
    let bundle = exe.as_deref().and_then(|p| {
        let mut cur = Some(p);
        while let Some(parent) = cur.and_then(|p| p.parent()) {
            if parent
                .extension()
                .and_then(|e| e.to_str())
                .map(|s| s.eq_ignore_ascii_case("app"))
                .unwrap_or(false)
            {
                return Some(parent.to_path_buf());
            }
            cur = Some(parent);
        }
        None
    });
    let target = bundle.unwrap_or_else(|| exe.clone().unwrap_or_default());
    let _ = std::process::Command::new("/usr/bin/open")
        .arg("-n")
        .arg(target.as_os_str())
        .spawn();
}

// Quiet unused warnings on this struct field — we keep Profile available for
// callsites that depend on its construction visibility.
#[allow(dead_code)]
fn _profile_marker(_: &Profile) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv6Addr};

    fn v4(s: &str) -> IpAddr {
        IpAddr::V4(s.parse().unwrap())
    }
    fn v6(s: &str) -> IpAddr {
        IpAddr::V6(s.parse::<Ipv6Addr>().unwrap())
    }

    #[test]
    fn specific_addr_returns_single_entry_unchanged() {
        let r = collect_endpoints_from(
            "192.168.1.5",
            1080,
            vec![("en0".to_string(), v4("1.2.3.4"))],
        );
        assert_eq!(r, vec!["192.168.1.5:1080".to_string()]);
    }

    #[test]
    fn loopback_first_when_listening_on_all() {
        let r = collect_endpoints_from("0.0.0.0", 1081, std::iter::empty());
        assert_eq!(r, vec!["127.0.0.1:1081  (loopback)".to_string()]);
    }

    #[test]
    fn filters_pseudo_interfaces() {
        let r = collect_endpoints_from(
            "0.0.0.0",
            1080,
            vec![
                ("utun0".to_string(), v4("10.0.0.1")),
                ("awdl0".to_string(), v4("10.0.0.2")),
                ("anpi0".to_string(), v4("10.0.0.3")),
                ("ap1".to_string(), v4("10.0.0.4")),
                ("bridge100".to_string(), v4("10.0.0.5")),
                ("llw0".to_string(), v4("10.0.0.6")),
                ("en0".to_string(), v4("192.168.1.100")),
            ],
        );
        assert_eq!(
            r,
            vec![
                "127.0.0.1:1080  (loopback)".to_string(),
                "192.168.1.100:1080  (en0)".to_string(),
            ]
        );
    }

    #[test]
    fn drops_loopback_unspecified_multicast() {
        let r = collect_endpoints_from(
            "0.0.0.0",
            1080,
            vec![
                ("lo0".to_string(), v4("127.0.0.1")),
                ("en0".to_string(), v4("0.0.0.0")),
                ("en1".to_string(), v4("224.0.0.1")),
                ("en2".to_string(), v4("192.168.0.50")),
            ],
        );
        assert_eq!(
            r,
            vec![
                "127.0.0.1:1080  (loopback)".to_string(),
                "192.168.0.50:1080  (en2)".to_string(),
            ]
        );
    }

    #[test]
    fn ipv6_addresses_excluded() {
        let r = collect_endpoints_from(
            "0.0.0.0",
            1080,
            vec![
                ("en0".to_string(), v6("2001:db8::1")),
                ("en0".to_string(), v4("10.0.0.5")),
            ],
        );
        assert_eq!(
            r,
            vec![
                "127.0.0.1:1080  (loopback)".to_string(),
                "10.0.0.5:1080  (en0)".to_string(),
            ]
        );
    }

    #[test]
    fn deduplicates_repeated_ipv4() {
        let r = collect_endpoints_from(
            "0.0.0.0",
            1080,
            vec![
                ("en0".to_string(), v4("10.0.0.5")),
                ("en1".to_string(), v4("10.0.0.5")),
            ],
        );
        assert_eq!(
            r,
            vec![
                "127.0.0.1:1080  (loopback)".to_string(),
                "10.0.0.5:1080  (en0)".to_string(),
            ]
        );
    }
}
