mod about;
mod config;
mod firefox;
mod gui;
mod socks;
mod tunnel;

use anyhow::Result;
use std::sync::Arc;
use tracing::{error, info};
use tracing_subscriber::EnvFilter;

use muda::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tao::event::Event;
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tray_icon::{Icon, TrayIconBuilder, TrayIconEvent};

const BT_GREEN_PNG: &[u8] = include_bytes!("../assets/bt-green.png");
const BT_ORANGE_PNG: &[u8] = include_bytes!("../assets/bt-orange.png");
const BT_RED_PNG: &[u8] = include_bytes!("../assets/bt-red.png");
const BT_GRAY_PNG: &[u8] = include_bytes!("../assets/bt-gray.png");
const BT_HIDDEN_PNG: &[u8] = include_bytes!("../assets/bt-hidden.png");

use crate::config::{ConfigFile, Profile};
use crate::tunnel::Status;

#[derive(Debug, Clone)]
enum UserEvent {
    StatusChanged(Status),
}

fn main() -> Result<()> {
    init_tracing();

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

    // Tokio runtime owns the tunnel and the SOCKS listener.
    let (status_tx, status_rx) = tokio::sync::watch::channel(Status::Connecting);
    let tunnel_ctl = tunnel::Control::new(true);
    {
        let profile = profile.clone();
        let status_tx = status_tx.clone();
        let tunnel_ctl = tunnel_ctl.clone();
        std::thread::Builder::new()
            .name("tokio-tunnel".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .enable_all()
                    .build()
                    .expect("build tokio runtime");
                rt.block_on(tunnel::run_forever(profile, status_tx, tunnel_ctl));
            })?;
    }

    // tao event loop must run on the main thread for macOS tray to work.
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();

    // Bridge: watch tunnel status and forward as UserEvent to the main loop.
    {
        let proxy = proxy.clone();
        std::thread::Builder::new()
            .name("status-bridge".into())
            .spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build status bridge runtime");
                rt.block_on(async move {
                    let mut rx = status_rx;
                    let initial = rx.borrow().clone();
                    let _ = proxy.send_event(UserEvent::StatusChanged(initial));
                    while rx.changed().await.is_ok() {
                        let s = rx.borrow().clone();
                        let _ = proxy.send_event(UserEvent::StatusChanged(s));
                    }
                });
            })?;
    }

    // Resolve paths used by menu items.
    let config_path = ConfigFile::default_path()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp/proxy-tunnel-config.json"));
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
    // The status line doubles as the connect/disconnect toggle — click to flip.
    let status_item = MenuItem::new("Connecting…  (click to disconnect)", true, None);
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

    let hide_dot_item = CheckMenuItem::new(
        "Hide colored dot (compact mode)",
        true,
        file.hide_status_dot,
        None,
    );

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

    let open_firefox_item = MenuItem::new(
        "Launch Firefox",
        firefox_info.installed(),
        None,
    );
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
    let uninstall_firefox_item = MenuItem::new(
        "Uninstall Firefox…",
        firefox_info.installed(),
        None,
    );

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
    let restart_item = MenuItem::new("Restart (apply config changes)", true, None);
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
    menu.append(&hide_dot_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&firefox_submenu)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&edit_config_item)?;
    menu.append(&reveal_data_item)?;
    menu.append(&open_logs_item)?;
    menu.append(&restart_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&about_item)?;
    menu.append(&PredefinedMenuItem::separator())?;
    menu.append(&quit_item)?;

    let icon_green = decode_icon(BT_GREEN_PNG)?;
    let icon_orange = decode_icon(BT_ORANGE_PNG)?;
    let icon_red = decode_icon(BT_RED_PNG)?;
    let icon_gray = decode_icon(BT_GRAY_PNG)?;
    let icon_hidden = decode_icon(BT_HIDDEN_PNG)?;

    let initial_icon = if file.hide_status_dot {
        icon_hidden.clone()
    } else {
        icon_orange.clone()
    };

    // Menu bar: colored cyrillic "БТ" icon for status. Orange on startup.
    let mut tray = Some(
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_icon(initial_icon)
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
    let restart_id = restart_item.id().clone();
    let copy_endpoints_id = copy_endpoints_item.id().clone();
    let listen_all_id = listen_all_item.id().clone();
    let hide_dot_id = hide_dot_item.id().clone();
    let status_toggle_id = status_item.id().clone();
    let install_firefox_id = install_firefox_item.id().clone();
    let install_homebrew_id = install_homebrew_item.id().clone();
    let uninstall_firefox_id = uninstall_firefox_item.id().clone();
    let mut hide_status_dot = file.hide_status_dot;
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
            } else if event.id == hide_dot_id {
                hide_status_dot = hide_dot_item.is_checked();
                if let Err(e) = update_hide_dot(&config_path, hide_status_dot) {
                    error!(error = %e, "could not persist hide-dot setting");
                }
                if let Some(t) = tray.as_ref() {
                    let icon = if hide_status_dot {
                        icon_hidden.clone()
                    } else {
                        icon_orange.clone()
                    };
                    let _ = t.set_icon(Some(icon));
                }
            } else if event.id == open_firefox_id {
                if let Err(e) = firefox::launch_default() {
                    error!(error = %e, "Firefox launch failed");
                    macos_alert(
                        "Couldn't launch Firefox",
                        &format!(
                            "{e}\n\nUse the Firefox submenu → Install Firefox to fetch it."
                        ),
                    );
                }
            } else if event.id == install_firefox_id {
                if let Err(e) = firefox::install_or_update_async(
                    socks_host_for_firefox.clone(),
                    socks_port_for_firefox,
                    notify_user,
                ) {
                    macos_alert("Couldn't start Firefox install", &format!("{e}"));
                }
            } else if event.id == install_homebrew_id {
                firefox::install_homebrew_async(notify_user);
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
            } else if event.id == restart_id {
                info!("restart requested");
                tray.take();
                relaunch_self();
                *control_flow = ControlFlow::Exit;
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

        if let Event::UserEvent(UserEvent::StatusChanged(status)) = event {
            let (icon_for_status, body) = match &status {
                Status::Connecting => (
                    &icon_orange,
                    "Connecting…  (click to disconnect)".to_string(),
                ),
                Status::Connected => (
                    &icon_green,
                    format!("Connected to {}  (click to disconnect)", profile.ssh.host),
                ),
                Status::Disconnected(err) => (
                    &icon_red,
                    format!("Disconnected: {err}  (click to reconnect)"),
                ),
                Status::Disabled => (&icon_gray, "Disconnected  (click to connect)".to_string()),
            };
            let icon = if hide_status_dot {
                icon_hidden.clone()
            } else {
                icon_for_status.clone()
            };
            if let Some(t) = tray.as_ref() {
                let _ = t.set_icon(Some(icon));
                let _ = t.set_tooltip(Some(format!("БелкаТуннель — {body}")));
            }
            status_item.set_text(&body);
        }
    });
}

fn init_tracing() {
    let log_dir = directories::ProjectDirs::from("io", "celestialtech", "BelkaTunnel")
        .map(|d| d.data_dir().join("logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("proxy-tunnel.log");

    let stderr_layer = tracing_subscriber::fmt::layer()
        .with_writer(std::io::stderr)
        .with_target(true);

    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,russh=warn"));

    let registry = tracing_subscriber::registry().with(env).with(stderr_layer);

    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
    {
        Ok(file) => {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::sync::Mutex::new(file))
                .with_ansi(false)
                .with_target(true);
            registry.with(file_layer).init();
            info!(log_file = %log_path.display(), "tracing initialized");
        }
        Err(e) => {
            registry.init();
            error!(error = %e, "could not open log file; logging to stderr only");
        }
    }
}

/// Enumerate all sockets that match the configured listen address and produce
/// human-readable `host:port` strings clients can use to reach the proxy.
/// Skips noisy stuff: IPv6 link-local (fe80::), AWDL/utun/anpi pseudo-interfaces.
fn collect_endpoints(listen_addr: &str, port: u16) -> Vec<String> {
    if listen_addr != "0.0.0.0" && listen_addr != "::" {
        return vec![format!("{listen_addr}:{port}")];
    }
    let mut out: Vec<String> = vec![format!("127.0.0.1:{port}  (loopback)")];
    if let Ok(ifaces) = local_ip_address::list_afinet_netifas() {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        seen.insert("127.0.0.1".to_string());
        for (name, ip) in ifaces {
            if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
                continue;
            }
            // Filter out junk interfaces / link-local addresses.
            let lower_name = name.to_lowercase();
            if matches!(
                lower_name.as_str(),
                _ if lower_name.starts_with("utun")
                    || lower_name.starts_with("awdl")
                    || lower_name.starts_with("llw")
                    || lower_name.starts_with("anpi")
                    || lower_name.starts_with("ap")
                    || lower_name.starts_with("bridge")
            ) {
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

fn macos_alert(title: &str, body: &str) {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
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
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
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
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
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
    Icon::from_rgba(img.into_raw(), w, h)
        .map_err(|e| anyhow::anyhow!("build tray icon: {e}"))
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

fn update_hide_dot(config_path: &std::path::Path, hide: bool) -> Result<()> {
    let mut file = ConfigFile::load(config_path).or_else(|_| Ok::<_, anyhow::Error>(ConfigFile::default()))?;
    file.hide_status_dot = hide;
    file.save(config_path)?;
    Ok(())
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
