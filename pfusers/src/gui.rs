//! eframe app — split-pane: sidebar list + central detail form. Mirrors
//! the structure of BelkaTunnel's gui.rs profile editor.

use crate::config::AppConfig;
use crate::pfsh;
use crate::ssh;
use crate::users::{PfUser, CANONICAL_PRIVILEGES};
use anyhow::Result;
use belka_ui::{
    apply_zed_theme, card, danger_button, field, ghost_button, install_unicode_fonts, pill,
    primary_button, section_title, theme, toggle_widget,
};
use eframe::egui::{self, Align, Color32, Frame, Layout, Margin, RichText, Rounding, Stroke};
use std::sync::{Arc, Mutex};
use tokio::runtime::Runtime;
use tracing::warn;

#[cfg(target_os = "macos")]
fn set_regular_activation_policy() {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

pub fn run() -> Result<()> {
    #[cfg(target_os = "macos")]
    set_regular_activation_policy();

    let cfg = AppConfig::load_or_default()?;

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([cfg.window.width, cfg.window.height])
            .with_min_inner_size([900.0, 540.0])
            .with_title("pfUsers")
            .with_visible(true)
            .with_decorations(true)
            .with_resizable(true),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "pfUsers",
        options,
        Box::new(|cc| {
            install_unicode_fonts(&cc.egui_ctx);
            apply_zed_theme(&cc.egui_ctx);
            Ok(Box::new(App::new(cfg)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

// ---------- Connection state ----------

#[derive(Clone)]
enum ConnState {
    Idle,
    Connecting,
    Connected(ssh::ClientHandle),
    Failed(String),
}

#[derive(Clone, PartialEq, Eq)]
enum ToastKind {
    Success,
    Error,
}

// ---------- App ----------

struct App {
    cfg: AppConfig,
    rt: Arc<Runtime>,
    state: Arc<Mutex<SharedState>>,
    /// Per-render local state.
    selected: Option<String>,
    /// Edit-buffer of the currently selected user, so unsaved edits aren't
    /// clobbered by a background list refresh.
    edit_buffer: Option<EditBuffer>,
    show_add_dialog: bool,
    add_form: AddForm,
    confirm_delete: Option<String>,
    delete_confirm_text: String,
    show_settings: bool,
    settings_form: AppConfig,
}

/// Anything the async tasks update needs to live behind a Mutex so the egui
/// render loop can read it on each frame.
struct SharedState {
    conn: ConnState,
    users: Vec<PfUser>,
    toast: Option<(ToastKind, String)>,
    loading_users: bool,
    pending_op: Option<String>,
}

impl Default for SharedState {
    fn default() -> Self {
        Self {
            conn: ConnState::Idle,
            users: Vec::new(),
            toast: None,
            loading_users: false,
            pending_op: None,
        }
    }
}

struct EditBuffer {
    name: String,
    descr: String,
    authorized_keys: String,
    priv_list: Vec<String>,
    disabled: bool,
    groups: Vec<String>,
}

#[derive(Default)]
struct AddForm {
    name: String,
    descr: String,
    password: String,
    authorized_keys: String,
    shell_access: bool,
    web_access: bool,
}

impl App {
    fn new(cfg: AppConfig) -> Self {
        let rt = Arc::new(
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .thread_name("pfusers-tokio")
                .build()
                .expect("tokio runtime"),
        );
        let state = Arc::new(Mutex::new(SharedState::default()));
        let app = Self {
            cfg: cfg.clone(),
            rt,
            state,
            selected: None,
            edit_buffer: None,
            show_add_dialog: false,
            add_form: AddForm::default(),
            confirm_delete: None,
            delete_confirm_text: String::new(),
            show_settings: false,
            settings_form: cfg,
        };
        app.spawn_connect();
        app
    }

    fn spawn_connect(&self) {
        let cfg = self.cfg.clone();
        let state = self.state.clone();
        {
            let mut s = state.lock().unwrap();
            s.conn = ConnState::Connecting;
        }
        self.rt.spawn(async move {
            let r = ssh::connect(
                &cfg.ssh.host,
                cfg.ssh.port,
                &cfg.ssh.user,
                &cfg.ssh.key_path,
                cfg.ssh.host_key_fingerprint.clone(),
            )
            .await;
            let mut s = state.lock().unwrap();
            match r {
                Ok((handle, tofu)) => {
                    s.conn = ConnState::Connected(handle);
                    if let Some(fp) = tofu {
                        // Persist the observed fingerprint so subsequent
                        // launches start in locked mode. Save inline; we
                        // hold the SharedState lock briefly, no async
                        // boundary needed.
                        let mut new_cfg = cfg.clone();
                        new_cfg.ssh.host_key_fingerprint = Some(fp.clone());
                        if let Some(path) = AppConfig::default_path() {
                            if let Err(e) = new_cfg.save(&path) {
                                warn!(error = %e, "could not persist TOFU fingerprint");
                            }
                        }
                        s.toast = Some((
                            ToastKind::Success,
                            format!("Connected. Recorded host key {fp}"),
                        ));
                    } else {
                        s.toast = Some((ToastKind::Success, "Connected.".to_string()));
                    }
                }
                Err(e) => {
                    let msg = format!("{e:#}");
                    s.conn = ConnState::Failed(msg.clone());
                    s.toast = Some((ToastKind::Error, format!("Couldn't connect: {msg}")));
                }
            }
        });
    }

    fn spawn_refresh(&self) {
        let handle = match &self.state.lock().unwrap().conn {
            ConnState::Connected(h) => h.clone(),
            _ => return,
        };
        let state = self.state.clone();
        {
            let mut s = state.lock().unwrap();
            s.loading_users = true;
        }
        self.rt.spawn(async move {
            let r = pfsh::list_users(&handle).await;
            let mut s = state.lock().unwrap();
            s.loading_users = false;
            match r {
                Ok(users) => {
                    s.users = users;
                    s.toast = Some((ToastKind::Success, "Users refreshed.".to_string()));
                }
                Err(e) => {
                    s.toast = Some((ToastKind::Error, format!("Refresh failed: {e:#}")));
                }
            }
        });
    }

    fn spawn_save_user(&self, buf: EditBuffer) {
        let handle = match &self.state.lock().unwrap().conn {
            ConnState::Connected(h) => h.clone(),
            _ => return,
        };
        let state = self.state.clone();
        {
            let mut s = state.lock().unwrap();
            s.pending_op = Some(format!("Saving {}…", buf.name));
        }
        self.rt.spawn(async move {
            let r = pfsh::update_user(
                &handle,
                &buf.name,
                &buf.descr,
                &buf.priv_list,
                &buf.groups,
                &buf.authorized_keys,
                buf.disabled,
            )
            .await;
            // Refresh after, so the UI reflects the canonical server state.
            let refresh = pfsh::list_users(&handle).await;
            let mut s = state.lock().unwrap();
            s.pending_op = None;
            match r {
                Ok(()) => {
                    s.toast = Some((ToastKind::Success, format!("Saved {}.", buf.name)));
                    if let Ok(u) = refresh {
                        s.users = u;
                    }
                }
                Err(e) => {
                    s.toast = Some((ToastKind::Error, format!("Save failed: {e:#}")));
                }
            }
        });
    }

    fn spawn_add_user(&self, form: AddForm) {
        let handle = match &self.state.lock().unwrap().conn {
            ConnState::Connected(h) => h.clone(),
            _ => return,
        };
        let state = self.state.clone();
        let priv_list: Vec<String> = [
            ("user-shell-access", form.shell_access),
            ("page-all", form.web_access),
        ]
        .into_iter()
        .filter_map(|(p, on)| if on { Some(p.to_string()) } else { None })
        .collect();
        {
            let mut s = state.lock().unwrap();
            s.pending_op = Some(format!("Creating {}…", form.name));
        }
        let name = form.name.clone();
        self.rt.spawn(async move {
            let r = pfsh::add_user(
                &handle,
                &form.name,
                &form.descr,
                &form.password,
                &priv_list,
                &[],
                &form.authorized_keys,
            )
            .await;
            let refresh = pfsh::list_users(&handle).await;
            let mut s = state.lock().unwrap();
            s.pending_op = None;
            match r {
                Ok(()) => {
                    s.toast = Some((ToastKind::Success, format!("Created {name}.")));
                    if let Ok(u) = refresh {
                        s.users = u;
                    }
                }
                Err(e) => {
                    s.toast = Some((ToastKind::Error, format!("Create failed: {e:#}")));
                }
            }
        });
    }

    fn spawn_delete_user(&self, name: String) {
        let handle = match &self.state.lock().unwrap().conn {
            ConnState::Connected(h) => h.clone(),
            _ => return,
        };
        let state = self.state.clone();
        {
            let mut s = state.lock().unwrap();
            s.pending_op = Some(format!("Deleting {name}…"));
        }
        let name_for_msg = name.clone();
        self.rt.spawn(async move {
            let r = pfsh::delete_user(&handle, &name).await;
            let refresh = pfsh::list_users(&handle).await;
            let mut s = state.lock().unwrap();
            s.pending_op = None;
            match r {
                Ok(()) => {
                    s.toast = Some((ToastKind::Success, format!("Deleted {name_for_msg}.")));
                    if let Ok(u) = refresh {
                        s.users = u;
                    }
                }
                Err(e) => {
                    s.toast = Some((ToastKind::Error, format!("Delete failed: {e:#}")));
                }
            }
        });
    }
}

impl eframe::App for App {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        let [r, g, b, _] = theme::BG.to_array();
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Repaint regularly so background-task updates appear without the
        // user needing to wiggle the mouse.
        ctx.request_repaint_after(std::time::Duration::from_millis(250));

        self.draw_top_bar(ctx);
        self.draw_sidebar(ctx);
        self.draw_status_bar(ctx);
        self.draw_central(ctx);
        if self.show_add_dialog {
            self.draw_add_dialog(ctx);
        }
        if self.confirm_delete.is_some() {
            self.draw_delete_modal(ctx);
        }
        if self.show_settings {
            self.draw_settings_modal(ctx);
        }
    }
}

impl App {
    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .frame(
                Frame::none()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::symmetric(20.0, 14.0))
                    .stroke(Stroke::new(1.0, theme::BORDER)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("pfUsers")
                            .color(theme::TEXT_PRIMARY)
                            .size(15.0)
                            .strong(),
                    );
                    ui.label(RichText::new("·").color(theme::TEXT_FAINT).size(15.0));
                    ui.label(
                        RichText::new(format!(
                            "{}@{}:{}",
                            self.cfg.ssh.user, self.cfg.ssh.host, self.cfg.ssh.port
                        ))
                        .color(theme::TEXT_MUTED)
                        .size(13.0),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if primary_button(ui, "  + Add User  ").clicked() {
                            self.show_add_dialog = true;
                            self.add_form = AddForm::default();
                        }
                        ui.add_space(8.0);
                        if ghost_button(ui, "  ⟳ Refresh  ").clicked() {
                            self.spawn_refresh();
                        }
                        ui.add_space(8.0);
                        if ghost_button(ui, "  ⚙ Settings  ").clicked() {
                            self.show_settings = true;
                            self.settings_form = self.cfg.clone();
                        }
                        ui.add_space(12.0);
                        let s = self.state.lock().unwrap();
                        let (dot_color, label) = match &s.conn {
                            ConnState::Idle => (theme::TEXT_FAINT, "Idle".to_string()),
                            ConnState::Connecting => (theme::ACCENT, "Connecting…".to_string()),
                            ConnState::Connected(_) => (theme::SUCCESS, "Connected".to_string()),
                            ConnState::Failed(e) => {
                                (theme::DANGER, format!("Disconnected — {}", truncate(e, 40)))
                            }
                        };
                        ui.label(RichText::new("●").color(dot_color).size(13.0));
                        ui.label(RichText::new(label).color(theme::TEXT_MUTED).size(12.0));
                    });
                });
            });
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(320.0)
            .frame(
                Frame::none()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::NONE),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("USERS")
                            .color(theme::TEXT_FAINT)
                            .size(10.5)
                            .strong(),
                    );
                    let s = self.state.lock().unwrap();
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{}", s.users.len()))
                                .color(theme::TEXT_FAINT)
                                .size(10.5),
                        );
                    });
                });
                ui.add_space(8.0);

                let users = self.state.lock().unwrap().users.clone();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for u in &users {
                        let is_selected = self.selected.as_deref() == Some(u.name.as_str());
                        let bg = if is_selected {
                            theme::ACCENT.linear_multiply(0.22)
                        } else {
                            theme::CARD
                        };
                        let stroke = if is_selected {
                            Stroke::new(1.0, theme::ACCENT)
                        } else {
                            Stroke::new(1.0, theme::BORDER)
                        };
                        let resp = Frame::none()
                            .fill(bg)
                            .stroke(stroke)
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::symmetric(12.0, 10.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let dot = if u.has_shell_access() {
                                        theme::SUCCESS
                                    } else {
                                        theme::TEXT_FAINT
                                    };
                                    ui.label(RichText::new("●").color(dot).size(11.0));
                                    ui.label(
                                        RichText::new(&u.name)
                                            .color(theme::TEXT_PRIMARY)
                                            .size(13.0)
                                            .strong(),
                                    );
                                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                        if u.has_shell_access() {
                                            pill(ui, "SHELL", theme::ACCENT);
                                        }
                                    });
                                });
                                ui.add_space(2.0);
                                let line2 = if u.descr.is_empty() {
                                    format!("uid {} · {} keys", u.uid, u.key_count())
                                } else {
                                    format!("{} · uid {} · {} keys", u.descr, u.uid, u.key_count())
                                };
                                ui.label(RichText::new(line2).color(theme::TEXT_MUTED).size(11.0));
                            })
                            .response
                            .interact(egui::Sense::click());
                        if resp.clicked() {
                            self.selected = Some(u.name.clone());
                            self.edit_buffer = Some(EditBuffer {
                                name: u.name.clone(),
                                descr: u.descr.clone(),
                                authorized_keys: u.authorized_keys.clone(),
                                priv_list: u.priv_list.clone(),
                                disabled: u.disabled,
                                groups: u.groups.clone(),
                            });
                        }
                        ui.add_space(6.0);
                    }
                });
            });
    }

    fn draw_central(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(theme::BG)
                    .inner_margin(Margin::same(24.0)),
            )
            .show(ctx, |ui| {
                let Some(buf) = self.edit_buffer.as_mut() else {
                    ui.label(
                        RichText::new("Select a user to edit.")
                            .color(theme::TEXT_MUTED),
                    );
                    return;
                };
                let mut save_clicked = false;
                let mut delete_clicked = false;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&buf.name)
                            .color(theme::TEXT_PRIMARY)
                            .size(22.0)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if primary_button(ui, "  Save Changes  ").clicked() {
                            save_clicked = true;
                        }
                    });
                });
                ui.add_space(18.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    card(ui, |ui| {
                        section_title(ui, "Identity", Some("Username is the row key; only the description is editable."));
                        field(ui, "Username (read-only)", None, |ui| {
                            let mut name = buf.name.clone();
                            ui.add(
                                egui::TextEdit::singleline(&mut name)
                                    .interactive(false)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                        field(ui, "Full name", None, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut buf.descr)
                                    .desired_width(f32::INFINITY),
                            );
                        });
                        field(ui, "Account state", None, |ui| {
                            let mut enabled = !buf.disabled;
                            if ui.add(toggle_widget("Enabled", &mut enabled)).changed() {
                                buf.disabled = !enabled;
                            }
                        });
                    });
                    ui.add_space(14.0);

                    card(ui, |ui| {
                        section_title(ui, "Privileges", Some("Click to toggle. Shell access derives /bin/tcsh."));
                        for (name, descr) in CANONICAL_PRIVILEGES.iter() {
                            let mut on = buf.priv_list.iter().any(|p| p == name);
                            let was_on = on;
                            ui.horizontal(|ui| {
                                ui.add(toggle_widget(name, &mut on));
                                ui.label(RichText::new(*descr).color(theme::TEXT_MUTED).size(11.5));
                            });
                            if on != was_on {
                                if on {
                                    buf.priv_list.push(name.to_string());
                                } else {
                                    buf.priv_list.retain(|p| p != name);
                                }
                            }
                        }
                    });
                    ui.add_space(14.0);

                    card(ui, |ui| {
                        section_title(ui, "Authorized SSH keys", Some("One OpenSSH-format key per line."));
                        ui.add(
                            egui::TextEdit::multiline(&mut buf.authorized_keys)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(8)
                                .desired_width(f32::INFINITY),
                        );
                        ui.label(
                            RichText::new(format!("{} keys", count_keys(&buf.authorized_keys)))
                                .color(theme::TEXT_FAINT)
                                .size(11.0),
                        );
                    });
                    ui.add_space(14.0);

                    Frame::none()
                        .fill(theme::DANGER.linear_multiply(0.08))
                        .stroke(Stroke::new(1.0, theme::DANGER.linear_multiply(0.5)))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin::same(16.0))
                        .show(ui, |ui| {
                            ui.label(
                                RichText::new("Danger zone")
                                    .color(theme::DANGER)
                                    .size(13.5)
                                    .strong(),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(
                                    "Removing this user deletes the /etc/passwd entry, /home/<user>, and authorized_keys.",
                                )
                                .color(theme::TEXT_MUTED)
                                .size(11.5),
                            );
                            ui.add_space(8.0);
                            if danger_button(ui, "  Delete user…  ").clicked() {
                                delete_clicked = true;
                            }
                        });
                });
                if save_clicked {
                    if let Some(b) = self.edit_buffer.as_ref() {
                        let clone = EditBuffer {
                            name: b.name.clone(),
                            descr: b.descr.clone(),
                            authorized_keys: b.authorized_keys.clone(),
                            priv_list: b.priv_list.clone(),
                            disabled: b.disabled,
                            groups: b.groups.clone(),
                        };
                        self.spawn_save_user(clone);
                    }
                }
                if delete_clicked {
                    self.confirm_delete = self.selected.clone();
                    self.delete_confirm_text.clear();
                }
            });
    }

    fn draw_status_bar(&mut self, ctx: &egui::Context) {
        let toast = self.state.lock().unwrap().toast.clone();
        let pending = self.state.lock().unwrap().pending_op.clone();
        let Some((kind, msg)) = toast else {
            if let Some(p) = pending {
                egui::TopBottomPanel::bottom("status")
                    .frame(
                        Frame::none()
                            .fill(theme::ACCENT.linear_multiply(0.18))
                            .stroke(Stroke::new(1.0, theme::ACCENT.linear_multiply(0.4)))
                            .inner_margin(Margin::symmetric(18.0, 10.0)),
                    )
                    .show(ctx, |ui| {
                        ui.label(RichText::new(p).color(theme::TEXT_PRIMARY).size(12.0));
                    });
            }
            return;
        };
        let (fill, color) = match kind {
            ToastKind::Success => (theme::SUCCESS.linear_multiply(0.18), theme::SUCCESS),
            ToastKind::Error => (theme::DANGER.linear_multiply(0.18), theme::DANGER),
        };
        egui::TopBottomPanel::bottom("status")
            .frame(
                Frame::none()
                    .fill(fill)
                    .stroke(Stroke::new(1.0, color.linear_multiply(0.4)))
                    .inner_margin(Margin::symmetric(18.0, 10.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&msg).color(theme::TEXT_PRIMARY).size(12.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Dismiss").clicked() {
                            self.state.lock().unwrap().toast = None;
                        }
                    });
                });
            });
    }

    fn draw_add_dialog(&mut self, ctx: &egui::Context) {
        let mut do_create = false;
        let mut do_cancel = false;
        egui::Window::new("Add User")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                Frame::none()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(Margin::same(22.0)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(
                    RichText::new("New User")
                        .color(theme::TEXT_PRIMARY)
                        .size(16.0)
                        .strong(),
                );
                ui.add_space(12.0);
                field(ui, "Username", Some("lowercase, no spaces"), |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.add_form.name)
                            .desired_width(f32::INFINITY),
                    );
                });
                field(ui, "Full name", None, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.add_form.descr)
                            .desired_width(f32::INFINITY),
                    );
                });
                field(
                    ui,
                    "Password",
                    Some("Set on creation; can be reset later."),
                    |ui| {
                        ui.add(
                            egui::TextEdit::singleline(&mut self.add_form.password)
                                .password(true)
                                .desired_width(f32::INFINITY),
                        );
                    },
                );
                ui.horizontal(|ui| {
                    ui.add(toggle_widget(
                        "Shell access",
                        &mut self.add_form.shell_access,
                    ));
                    ui.add_space(20.0);
                    ui.add(toggle_widget(
                        "Web UI (page-all)",
                        &mut self.add_form.web_access,
                    ));
                });
                field(
                    ui,
                    "Initial SSH key",
                    Some("Optional. Paste one OpenSSH-format key per line."),
                    |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.add_form.authorized_keys)
                                .font(egui::TextStyle::Monospace)
                                .desired_rows(4)
                                .desired_width(f32::INFINITY),
                        );
                    },
                );
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ghost_button(ui, "  Cancel  ").clicked() {
                        do_cancel = true;
                    }
                    ui.add_space(8.0);
                    let can_create =
                        !self.add_form.name.trim().is_empty() && !self.add_form.password.is_empty();
                    if ui
                        .add_enabled(
                            can_create,
                            egui::Button::new(
                                RichText::new("  Create User  ")
                                    .color(Color32::WHITE)
                                    .strong(),
                            )
                            .fill(theme::ACCENT)
                            .rounding(Rounding::same(8.0)),
                        )
                        .clicked()
                    {
                        do_create = true;
                    }
                });
            });
        if do_cancel {
            self.show_add_dialog = false;
        }
        if do_create {
            let form = AddForm {
                name: self.add_form.name.trim().to_string(),
                descr: self.add_form.descr.clone(),
                password: self.add_form.password.clone(),
                authorized_keys: self.add_form.authorized_keys.clone(),
                shell_access: self.add_form.shell_access,
                web_access: self.add_form.web_access,
            };
            self.spawn_add_user(form);
            self.show_add_dialog = false;
        }
    }

    fn draw_delete_modal(&mut self, ctx: &egui::Context) {
        let Some(target) = self.confirm_delete.clone() else {
            return;
        };
        let mut cancel = false;
        let mut go = false;
        egui::Window::new("Delete User")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                Frame::none()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0, theme::DANGER.linear_multiply(0.5)))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(Margin::same(22.0)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(420.0);
                ui.label(
                    RichText::new(format!("Delete \"{target}\"?"))
                        .color(theme::TEXT_PRIMARY)
                        .size(15.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(
                        "This removes /etc/passwd, /home/<user>, and authorized_keys. Active SSH sessions stay open until they disconnect. This cannot be undone.",
                    )
                    .color(theme::TEXT_MUTED)
                    .size(11.5),
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("Type \"{target}\" to confirm:"))
                        .color(theme::TEXT_MUTED)
                        .size(11.5),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.delete_confirm_text)
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ghost_button(ui, "  Cancel  ").clicked() {
                        cancel = true;
                    }
                    ui.add_space(8.0);
                    let confirmed = self.delete_confirm_text == target;
                    if ui
                        .add_enabled(
                            confirmed,
                            egui::Button::new(RichText::new("  Delete User  ").color(theme::DANGER).strong())
                                .fill(theme::CARD)
                                .stroke(Stroke::new(1.0, theme::DANGER))
                                .rounding(Rounding::same(8.0)),
                        )
                        .clicked()
                    {
                        go = true;
                    }
                });
            });
        if cancel {
            self.confirm_delete = None;
            self.delete_confirm_text.clear();
        }
        if go {
            self.spawn_delete_user(target);
            self.confirm_delete = None;
            self.delete_confirm_text.clear();
            self.selected = None;
            self.edit_buffer = None;
        }
    }

    fn draw_settings_modal(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut save_and_reconnect = false;
        egui::Window::new("Settings")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .frame(
                Frame::none()
                    .fill(theme::PANEL)
                    .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
                    .rounding(Rounding::same(10.0))
                    .inner_margin(Margin::same(22.0)),
            )
            .show(ctx, |ui| {
                ui.set_min_width(460.0);
                ui.label(
                    RichText::new("pfSense endpoint")
                        .color(theme::TEXT_PRIMARY)
                        .size(15.0)
                        .strong(),
                );
                ui.add_space(8.0);
                field(ui, "Host", None, |ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.settings_form.ssh.host)
                            .desired_width(f32::INFINITY),
                    );
                });
                ui.horizontal(|ui| {
                    ui.vertical(|ui| {
                        field(ui, "Port", None, |ui| {
                            let mut port = self.settings_form.ssh.port as u32;
                            if ui
                                .add(egui::DragValue::new(&mut port).range(1..=65535))
                                .changed()
                            {
                                self.settings_form.ssh.port = port as u16;
                            }
                        });
                    });
                    ui.add_space(16.0);
                    ui.vertical(|ui| {
                        field(ui, "Admin username", None, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.settings_form.ssh.user)
                                    .desired_width(200.0),
                            );
                        });
                    });
                });
                field(
                    ui,
                    "Private key",
                    Some("Defaults to ~/.ssh/id_ed25519"),
                    |ui| {
                        let mut p = self
                            .settings_form
                            .ssh
                            .key_path
                            .to_string_lossy()
                            .into_owned();
                        if ui
                            .add(egui::TextEdit::singleline(&mut p).desired_width(f32::INFINITY))
                            .changed()
                        {
                            self.settings_form.ssh.key_path = std::path::PathBuf::from(p);
                        }
                    },
                );
                field(
                    ui,
                    "Host key fingerprint",
                    Some("Filled in automatically on first connect. Forget to re-TOFU."),
                    |ui| {
                        ui.horizontal(|ui| {
                            let mut show = self
                                .settings_form
                                .ssh
                                .host_key_fingerprint
                                .clone()
                                .unwrap_or_else(|| "(none — TOFU mode)".to_string());
                            ui.add(
                                egui::TextEdit::singleline(&mut show)
                                    .interactive(false)
                                    .desired_width(ui.available_width() - 90.0),
                            );
                            let can_forget = self.settings_form.ssh.host_key_fingerprint.is_some();
                            if ui
                                .add_enabled(can_forget, egui::Button::new("  Forget  "))
                                .clicked()
                            {
                                self.settings_form.ssh.host_key_fingerprint = None;
                            }
                        });
                    },
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ghost_button(ui, "  Cancel  ").clicked() {
                        close = true;
                    }
                    ui.add_space(8.0);
                    if primary_button(ui, "  Save & Reconnect  ").clicked() {
                        save_and_reconnect = true;
                    }
                });
            });
        if close {
            self.show_settings = false;
        }
        if save_and_reconnect {
            self.cfg = self.settings_form.clone();
            if let Some(path) = AppConfig::default_path() {
                let _ = self.cfg.save(&path);
            }
            self.show_settings = false;
            self.spawn_connect();
        }
    }
}

fn count_keys(s: &str) -> usize {
    s.lines().filter(|l| !l.trim().is_empty()).count()
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}
