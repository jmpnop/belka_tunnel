use crate::config::{ConfigFile, Profile};
use anyhow::Result;
use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Frame, Layout, Margin, RichText, Rounding, Stroke,
    TextStyle,
};
use std::path::PathBuf;

#[cfg(target_os = "macos")]
fn set_regular_activation_policy() {
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use objc2_foundation::MainThreadMarker;
    let mtm = match MainThreadMarker::new() {
        Some(m) => m,
        None => return,
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
}

pub fn run() -> Result<()> {
    #[cfg(target_os = "macos")]
    set_regular_activation_policy();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([900.0, 620.0])
            .with_min_inner_size([720.0, 480.0])
            .with_title("БелкаТуннель — Configuration")
            .with_visible(true)
            .with_active(true)
            .with_decorations(true)
            .with_resizable(true),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "БелкаТуннель — Configuration",
        options,
        Box::new(|cc| {
            install_unicode_fonts(&cc.egui_ctx);
            apply_zed_theme(&cc.egui_ctx);
            Ok(Box::new(ConfigApp::new()))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

/// Load a system font with broad Unicode coverage (Cyrillic, Greek, etc.) so
/// strings like "БелкаТуннель" render with real glyphs instead of fallback
/// squares.
fn install_unicode_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    let candidates: &[&str] = &[
        "/System/Library/Fonts/SFNSRounded.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/Library/Fonts/Arial Unicode.ttf",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ];
    let mut data: Option<(String, Vec<u8>)> = None;
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            data = Some((path.to_string(), bytes));
            break;
        }
    }
    let Some((path, bytes)) = data else { return };

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "system-unicode".to_string(),
        FontData::from_owned(bytes).into(),
    );
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "system-unicode".to_string());
    fonts
        .families
        .entry(FontFamily::Monospace)
        .or_default()
        .push("system-unicode".to_string());
    ctx.set_fonts(fonts);
    tracing::info!(font_path = %path, "loaded system Unicode font");
}

// ---------- Theme ----------

mod theme {
    use eframe::egui::Color32;
    pub const BG: Color32 = Color32::from_rgb(15, 17, 21);
    pub const PANEL: Color32 = Color32::from_rgb(20, 22, 27);
    pub const CARD: Color32 = Color32::from_rgb(26, 28, 34);
    pub const BORDER: Color32 = Color32::from_rgb(40, 43, 51);
    pub const BORDER_STRONG: Color32 = Color32::from_rgb(60, 64, 75);
    pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(220, 224, 232);
    pub const TEXT_MUTED: Color32 = Color32::from_rgb(132, 138, 152);
    pub const TEXT_FAINT: Color32 = Color32::from_rgb(94, 100, 114);
    pub const ACCENT: Color32 = Color32::from_rgb(99, 124, 247);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(120, 144, 255);
    pub const SUCCESS: Color32 = Color32::from_rgb(76, 192, 102);
    pub const DANGER: Color32 = Color32::from_rgb(228, 90, 105);
    pub const FIELD_BG: Color32 = Color32::from_rgb(13, 15, 19);
}

fn apply_zed_theme(ctx: &egui::Context) {
    use theme::*;
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.override_text_color = Some(TEXT_PRIMARY);
    style.visuals.window_fill = PANEL;
    style.visuals.panel_fill = BG;
    style.visuals.faint_bg_color = CARD;
    style.visuals.extreme_bg_color = FIELD_BG;
    style.visuals.code_bg_color = FIELD_BG;

    style.visuals.widgets.noninteractive.bg_fill = PANEL;
    style.visuals.widgets.noninteractive.weak_bg_fill = CARD;
    style.visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    style.visuals.widgets.noninteractive.rounding = Rounding::same(6.0);

    style.visuals.widgets.inactive.bg_fill = CARD;
    style.visuals.widgets.inactive.weak_bg_fill = FIELD_BG;
    style.visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, BORDER);
    style.visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    style.visuals.widgets.inactive.rounding = Rounding::same(6.0);

    style.visuals.widgets.hovered.bg_fill = CARD;
    style.visuals.widgets.hovered.weak_bg_fill = FIELD_BG;
    style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    style.visuals.widgets.hovered.fg_stroke = Stroke::new(1.5, TEXT_PRIMARY);
    style.visuals.widgets.hovered.rounding = Rounding::same(6.0);

    style.visuals.widgets.active.bg_fill = ACCENT;
    style.visuals.widgets.active.weak_bg_fill = ACCENT;
    style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT_HOVER);
    style.visuals.widgets.active.fg_stroke = Stroke::new(1.5, Color32::WHITE);
    style.visuals.widgets.active.rounding = Rounding::same(6.0);

    style.visuals.widgets.open.bg_fill = CARD;
    style.visuals.widgets.open.weak_bg_fill = FIELD_BG;
    style.visuals.widgets.open.bg_stroke = Stroke::new(1.0, BORDER_STRONG);
    style.visuals.widgets.open.fg_stroke = Stroke::new(1.0, TEXT_PRIMARY);
    style.visuals.widgets.open.rounding = Rounding::same(6.0);

    style.visuals.selection.bg_fill = ACCENT.linear_multiply(0.4);
    style.visuals.selection.stroke = Stroke::new(1.0, ACCENT);
    style.visuals.window_stroke = Stroke::new(1.0, BORDER);
    style.visuals.window_rounding = Rounding::same(10.0);
    style.visuals.menu_rounding = Rounding::same(8.0);
    style.visuals.hyperlink_color = ACCENT;

    style.spacing.item_spacing = egui::vec2(10.0, 10.0);
    style.spacing.button_padding = egui::vec2(14.0, 8.0);
    style.spacing.window_margin = Margin::same(0.0);
    style.spacing.indent = 18.0;
    style.spacing.interact_size = egui::vec2(40.0, 28.0);

    // Custom text styles with consistent sizes.
    use egui::FontFamily::Proportional;
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::new(20.0, Proportional));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.5, Proportional));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::new(13.0, Proportional));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::new(11.5, Proportional));
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(12.5, FontFamily::Monospace),
    );

    ctx.set_style(style);
}

// ---------- Helpers ----------

fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::none()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::same(18.0))
        .show(ui, add_contents)
        .inner
}

fn section_title(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>) {
    ui.vertical(|ui| {
        ui.add_space(2.0);
        ui.label(
            RichText::new(title)
                .color(theme::TEXT_PRIMARY)
                .size(14.5)
                .strong(),
        );
        if let Some(sub) = subtitle {
            ui.label(RichText::new(sub).color(theme::TEXT_MUTED).size(11.5));
        }
        ui.add_space(10.0);
    });
}

fn field(ui: &mut egui::Ui, label: &str, hint: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label)
                .color(theme::TEXT_MUTED)
                .size(11.5)
                .strong(),
        );
        if let Some(h) = hint {
            ui.label(RichText::new(h).color(theme::TEXT_FAINT).size(10.5));
        }
        ui.add_space(4.0);
        add(ui);
        ui.add_space(8.0);
    });
}

fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::none()
        .fill(color.linear_multiply(0.18))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.4)))
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(10.5).strong());
        });
}

fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    let resp = ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(theme::ACCENT)
            .stroke(Stroke::new(1.0, theme::ACCENT_HOVER))
            .rounding(Rounding::same(8.0)),
    );
    resp
}

fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme::TEXT_PRIMARY))
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .rounding(Rounding::same(8.0)),
    )
}

fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme::DANGER))
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::DANGER.linear_multiply(0.4)))
            .rounding(Rounding::same(8.0)),
    )
}

// ---------- App state ----------

struct ConfigApp {
    file: ConfigFile,
    selected: String,
    new_profile_name: String,
    last_status: Option<(StatusKind, String)>,
    confirm_delete: Option<String>,
    first_frame: bool,
}

#[derive(Clone)]
enum StatusKind {
    Success,
    Error,
}

impl ConfigApp {
    fn new() -> Self {
        let file = ConfigFile::load_or_default().unwrap_or_else(|_| ConfigFile::default());
        let selected = file.active.clone();
        Self {
            file,
            selected,
            new_profile_name: String::new(),
            last_status: None,
            confirm_delete: None,
            first_frame: true,
        }
    }

    fn save(&mut self) {
        // Pre-flight: catch invariant violations before writing the file so the
        // user sees the actual reason instead of the daemon silently rejecting
        // the file on its next auto-reload.
        if let Err(e) = self.file.validate() {
            self.last_status = Some((StatusKind::Error, format!("Invalid config: {e}")));
            return;
        }
        match ConfigFile::default_path() {
            Some(path) => match self.file.save(&path) {
                Ok(()) => {
                    self.last_status = Some((
                        StatusKind::Success,
                        format!(
                            "Saved to {}. The tunnel will reload automatically.",
                            path.display()
                        ),
                    ));
                }
                Err(e) => {
                    self.last_status = Some((StatusKind::Error, format!("Save failed: {e}")));
                }
            },
            None => {
                self.last_status =
                    Some((StatusKind::Error, "No config path available".to_string()));
            }
        }
    }
}

impl eframe::App for ConfigApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let [r, g, b, _] = theme::BG.to_array();
        [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            #[cfg(target_os = "macos")]
            set_regular_activation_policy();
        }
        self.draw_top_bar(ctx);
        self.draw_sidebar(ctx);
        self.draw_status_bar(ctx);
        self.draw_main(ctx);
        self.draw_delete_modal(ctx);
    }
}

impl ConfigApp {
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
                        RichText::new("БелкаТуннель")
                            .color(theme::TEXT_PRIMARY)
                            .size(15.0)
                            .strong(),
                    );
                    ui.label(RichText::new("·").color(theme::TEXT_FAINT).size(15.0));
                    ui.label(
                        RichText::new("Configuration")
                            .color(theme::TEXT_MUTED)
                            .size(13.0),
                    );

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if primary_button(ui, "  Save Changes  ").clicked() {
                            self.save();
                        }
                        ui.add_space(8.0);
                        if ghost_button(ui, "  Discard  ").clicked() {
                            self.file = ConfigFile::load_or_default()
                                .unwrap_or_else(|_| ConfigFile::default());
                            self.selected = self.file.active.clone();
                            self.last_status =
                                Some((StatusKind::Success, "Reloaded from disk.".to_string()));
                        }
                    });
                });
            });
    }

    fn draw_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .resizable(false)
            .exact_width(300.0)
            .frame(
                Frame::none()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::same(16.0))
                    .stroke(Stroke::NONE),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("PROFILES")
                            .color(theme::TEXT_FAINT)
                            .size(10.5)
                            .strong(),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("{}", self.file.profiles.len()))
                                .color(theme::TEXT_FAINT)
                                .size(10.5),
                        );
                    });
                });
                ui.add_space(8.0);

                let names: Vec<String> = self.file.profiles.keys().cloned().collect();
                for name in &names {
                    let is_active = *name == self.file.active;
                    let is_selected = *name == self.selected;

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
                                let dot_color = if is_active {
                                    theme::SUCCESS
                                } else {
                                    theme::TEXT_FAINT
                                };
                                ui.label(RichText::new("●").color(dot_color).size(11.0));
                                ui.label(
                                    RichText::new(name)
                                        .color(theme::TEXT_PRIMARY)
                                        .size(13.0)
                                        .strong(),
                                );
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    if is_active {
                                        pill(ui, "ACTIVE", theme::SUCCESS);
                                    }
                                });
                            });
                            if let Some(p) = self.file.profiles.get(name) {
                                ui.add_space(2.0);
                                ui.label(
                                    RichText::new(format!(
                                        "{}@{}:{}",
                                        p.ssh.user, p.ssh.host, p.ssh.port
                                    ))
                                    .color(theme::TEXT_MUTED)
                                    .size(11.0),
                                );
                            }
                        })
                        .response
                        .interact(egui::Sense::click());
                    if resp.clicked() {
                        self.selected = name.clone();
                    }
                    ui.add_space(6.0);
                }

                ui.add_space(8.0);
                Frame::none()
                    .fill(theme::CARD)
                    .stroke(Stroke::new(1.0, theme::BORDER))
                    .rounding(Rounding::same(8.0))
                    .inner_margin(Margin::same(10.0))
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new("CREATE NEW")
                                .color(theme::TEXT_FAINT)
                                .size(10.0)
                                .strong(),
                        );
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut self.new_profile_name)
                                    .hint_text("profile name…")
                                    .desired_width(ui.available_width() - 60.0),
                            );
                            let can_add = !self.new_profile_name.trim().is_empty()
                                && !self
                                    .file
                                    .profiles
                                    .contains_key(self.new_profile_name.trim());
                            if ui
                                .add_enabled(
                                    can_add,
                                    egui::Button::new(RichText::new("Add").color(Color32::WHITE))
                                        .fill(theme::ACCENT)
                                        .rounding(Rounding::same(6.0)),
                                )
                                .clicked()
                            {
                                let name = self.new_profile_name.trim().to_string();
                                self.file.profiles.insert(name.clone(), Profile::default());
                                self.selected = name;
                                self.new_profile_name.clear();
                            }
                        });
                    });

                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let exists = self.file.profiles.contains_key(&self.selected);
                    if ui
                        .add_enabled(exists, egui::Button::new("Duplicate"))
                        .clicked()
                    {
                        if let Some(src) = self.file.profiles.get(&self.selected).cloned() {
                            let mut i = 1;
                            let mut copy_name = format!("{}-copy", self.selected);
                            while self.file.profiles.contains_key(&copy_name) {
                                i += 1;
                                copy_name = format!("{}-copy{i}", self.selected);
                            }
                            self.file.profiles.insert(copy_name.clone(), src);
                            self.selected = copy_name;
                        }
                    }
                    let can_delete = self.file.profiles.len() > 1
                        && self.file.profiles.contains_key(&self.selected);
                    if ui
                        .add_enabled(can_delete, egui::Button::new("Delete"))
                        .clicked()
                    {
                        self.confirm_delete = Some(self.selected.clone());
                    }
                });

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(8.0);

                let activate_enabled = self.file.profiles.contains_key(&self.selected)
                    && self.selected != self.file.active;
                if ui
                    .add_enabled(
                        activate_enabled,
                        egui::Button::new(
                            RichText::new(format!("Set “{}” as Active", self.selected))
                                .color(theme::TEXT_PRIMARY),
                        )
                        .fill(theme::CARD)
                        .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
                        .rounding(Rounding::same(8.0)),
                    )
                    .clicked()
                {
                    self.file.active = self.selected.clone();
                }
            });
    }

    fn draw_main(&mut self, ctx: &egui::Context) {
        egui::CentralPanel::default()
            .frame(
                Frame::none()
                    .fill(theme::BG)
                    .inner_margin(Margin::same(24.0)),
            )
            .show(ctx, |ui| {
                if !self.file.profiles.contains_key(&self.selected) {
                    ui.label(
                        RichText::new("No profile selected.")
                            .color(theme::TEXT_MUTED),
                    );
                    return;
                }
                let selected_name = self.selected.clone();
                let active_name = self.file.active.clone();
                let is_active = selected_name == active_name;
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&selected_name)
                            .color(theme::TEXT_PRIMARY)
                            .size(22.0)
                            .strong(),
                    );
                    if is_active {
                        ui.add_space(8.0);
                        pill(ui, "ACTIVE PROFILE", theme::SUCCESS);
                    }
                });
                ui.label(
                    RichText::new("Edit this profile's SSH endpoint, SOCKS5 listener, and reconnect behavior.")
                        .color(theme::TEXT_MUTED)
                        .size(12.0),
                );
                ui.add_space(18.0);

                egui::ScrollArea::vertical().show(ui, |ui| {
                    let profile = match self.file.profiles.get_mut(&selected_name) {
                        Some(p) => p,
                        None => return,
                    };

                    card(ui, |ui| {
                        section_title(
                            ui,
                            "SSH endpoint",
                            Some("Where the tunnel connects and how it authenticates."),
                        );

                        field(ui, "Host", None, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut profile.ssh.host)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("aurora.celestialtech.io"),
                            );
                        });

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                field(ui, "Port", None, |ui| {
                                    let mut port = profile.ssh.port as u32;
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut port)
                                                .range(1..=65535)
                                                .speed(1.0),
                                        )
                                        .changed()
                                    {
                                        profile.ssh.port = port as u16;
                                    }
                                });
                            });
                            ui.add_space(20.0);
                            ui.vertical(|ui| {
                                field(ui, "Keepalive (seconds)", None, |ui| {
                                    let mut ka = profile.ssh.keepalive_secs;
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut ka)
                                                .range(5u64..=600)
                                                .speed(1.0),
                                        )
                                        .changed()
                                    {
                                        profile.ssh.keepalive_secs = ka;
                                    }
                                });
                            });
                        });

                        field(ui, "User", None, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(&mut profile.ssh.user)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("olgatimoshevskaia"),
                            );
                        });

                        field(
                            ui,
                            "Private key",
                            Some("OpenSSH-format private key (ed25519 recommended)."),
                            |ui| {
                                ui.horizontal(|ui| {
                                    let mut path = profile.ssh.key_path.to_string_lossy().into_owned();
                                    let resp = ui.add(
                                        egui::TextEdit::singleline(&mut path)
                                            .desired_width(ui.available_width() - 100.0)
                                            .hint_text("~/.ssh/id_ed25519"),
                                    );
                                    if resp.changed() {
                                        profile.ssh.key_path = PathBuf::from(path);
                                    }
                                    if ghost_button(ui, "  Browse…  ").clicked() {
                                        let start = profile
                                            .ssh
                                            .key_path
                                            .parent()
                                            .map(|p| p.to_path_buf())
                                            .unwrap_or_else(|| {
                                                PathBuf::from(
                                                    std::env::var("HOME").unwrap_or_default(),
                                                )
                                                .join(".ssh")
                                            });
                                        if let Some(picked) = rfd::FileDialog::new()
                                            .set_directory(start)
                                            .pick_file()
                                        {
                                            profile.ssh.key_path = picked;
                                        }
                                    }
                                });
                            },
                        );

                        field(ui, "Key passphrase", Some("Leave empty if the key is unencrypted."), |ui| {
                            let mut pass = profile.ssh.key_passphrase.clone().unwrap_or_default();
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut pass)
                                    .password(true)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("(none)"),
                            );
                            if resp.changed() {
                                profile.ssh.key_passphrase =
                                    if pass.is_empty() { None } else { Some(pass) };
                            }
                        });

                        field(
                            ui,
                            "Recorded host-key fingerprint",
                            Some(
                                "Filled in automatically the first time the daemon \
                                 connects. The daemon refuses to connect if the server \
                                 later presents a different key (possible MITM). \
                                 Forget the value to re-trust on the next connect — \
                                 use only if you intentionally rotated the server key.",
                            ),
                            |ui| {
                                ui.horizontal(|ui| {
                                    let display = profile
                                        .ssh
                                        .host_key_fingerprint
                                        .clone()
                                        .unwrap_or_else(|| {
                                            "(none recorded — trust-on-first-use mode)".to_string()
                                        });
                                    // Read-only so a typo can't silently break
                                    // connections; use a TextEdit with the buffer
                                    // disabled so the value is still selectable
                                    // for copy / compare against ssh-keyscan.
                                    let mut shown = display;
                                    ui.add(
                                        egui::TextEdit::singleline(&mut shown)
                                            .interactive(false)
                                            .desired_width(ui.available_width() - 90.0),
                                    );
                                    let can_clear =
                                        profile.ssh.host_key_fingerprint.is_some();
                                    if ui
                                        .add_enabled(can_clear, egui::Button::new("  Forget  "))
                                        .clicked()
                                    {
                                        profile.ssh.host_key_fingerprint = None;
                                    }
                                });
                            },
                        );
                    });

                    ui.add_space(18.0);

                    card(ui, |ui| {
                        section_title(
                            ui,
                            "SOCKS5 listener",
                            Some("Where local programs connect to use the tunnel."),
                        );

                        ui.horizontal(|ui| {
                            let listen_all = profile.socks.listen_addr == "0.0.0.0";
                            let mut new_listen_all = listen_all;
                            if ui
                                .add(toggle_widget("Listen on all interfaces", &mut new_listen_all))
                                .changed()
                            {
                                profile.socks.listen_addr = if new_listen_all {
                                    "0.0.0.0".to_string()
                                } else {
                                    "127.0.0.1".to_string()
                                };
                            }
                            ui.label(
                                RichText::new(if new_listen_all {
                                    "Reachable from the LAN."
                                } else {
                                    "Loopback only (this Mac)."
                                })
                                .color(theme::TEXT_MUTED)
                                .size(11.5),
                            );
                        });
                        ui.add_space(6.0);

                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                field(ui, "Listen address", None, |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut profile.socks.listen_addr)
                                            .desired_width(200.0),
                                    );
                                });
                            });
                            ui.add_space(20.0);
                            ui.vertical(|ui| {
                                field(ui, "Listen port", None, |ui| {
                                    let mut p = profile.socks.listen_port as u32;
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut p)
                                                .range(1..=65535)
                                                .speed(1.0),
                                        )
                                        .changed()
                                    {
                                        profile.socks.listen_port = p as u16;
                                    }
                                });
                            });
                        });
                    });

                    ui.add_space(18.0);

                    card(ui, |ui| {
                        section_title(
                            ui,
                            "Reconnect behavior",
                            Some("Exponential backoff between failed reconnect attempts."),
                        );
                        ui.horizontal(|ui| {
                            ui.vertical(|ui| {
                                field(ui, "Initial backoff (s)", None, |ui| {
                                    let mut v = profile.reconnect.initial_backoff_secs;
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut v)
                                                .range(1u64..=60),
                                        )
                                        .changed()
                                    {
                                        profile.reconnect.initial_backoff_secs = v;
                                    }
                                });
                            });
                            ui.add_space(20.0);
                            ui.vertical(|ui| {
                                field(ui, "Max backoff (s)", None, |ui| {
                                    let mut v = profile.reconnect.max_backoff_secs;
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut v)
                                                .range(1u64..=3600),
                                        )
                                        .changed()
                                    {
                                        profile.reconnect.max_backoff_secs = v;
                                    }
                                });
                            });
                            ui.add_space(20.0);
                            ui.vertical(|ui| {
                                field(ui, "Multiplier", None, |ui| {
                                    let mut v = profile.reconnect.backoff_multiplier as f32;
                                    if ui
                                        .add(
                                            egui::DragValue::new(&mut v)
                                                .range(1.0..=10.0)
                                                .speed(0.1),
                                        )
                                        .changed()
                                    {
                                        profile.reconnect.backoff_multiplier = v as f64;
                                    }
                                });
                            });
                        });
                    });

                    ui.add_space(18.0);

                    if is_active {
                        Frame::none()
                            .fill(theme::ACCENT.linear_multiply(0.10))
                            .stroke(Stroke::new(1.0, theme::ACCENT.linear_multiply(0.4)))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::same(14.0))
                            .show(ui, |ui| {
                                ui.label(
                                    RichText::new("This is the active profile. Changes apply automatically when you save — the tunnel reloads in place.")
                                        .color(theme::TEXT_PRIMARY)
                                        .size(12.0),
                                );
                            });
                    } else {
                        Frame::none()
                            .fill(theme::CARD)
                            .stroke(Stroke::new(1.0, theme::BORDER))
                            .rounding(Rounding::same(8.0))
                            .inner_margin(Margin::same(14.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.label(
                                        RichText::new("Inactive profile.")
                                            .color(theme::TEXT_MUTED)
                                            .size(12.0),
                                    );
                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("Set Active").color(Color32::WHITE),
                                            )
                                            .fill(theme::ACCENT)
                                            .rounding(Rounding::same(6.0)),
                                        )
                                        .clicked()
                                    {
                                        self.file.active = selected_name.clone();
                                    }
                                });
                            });
                    }
                });
            });
    }

    fn draw_status_bar(&mut self, ctx: &egui::Context) {
        if let Some((kind, msg)) = self.last_status.clone() {
            let (fill, color) = match kind {
                StatusKind::Success => (theme::SUCCESS.linear_multiply(0.18), theme::SUCCESS),
                StatusKind::Error => (theme::DANGER.linear_multiply(0.18), theme::DANGER),
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
                                self.last_status = None;
                            }
                        });
                    });
                });
        }
    }

    fn draw_delete_modal(&mut self, ctx: &egui::Context) {
        if let Some(target) = self.confirm_delete.clone() {
            egui::Window::new("Delete Profile")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(
                    Frame::none()
                        .fill(theme::PANEL)
                        .stroke(Stroke::new(1.0, theme::BORDER_STRONG))
                        .rounding(Rounding::same(10.0))
                        .inner_margin(Margin::same(20.0)),
                )
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new(format!("Delete “{target}”?"))
                            .color(theme::TEXT_PRIMARY)
                            .size(15.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new("This cannot be undone.")
                            .color(theme::TEXT_MUTED)
                            .size(11.5),
                    );
                    ui.add_space(14.0);
                    ui.horizontal(|ui| {
                        if danger_button(ui, "  Delete  ").clicked() {
                            self.file.profiles.remove(&target);
                            if self.file.active == target {
                                self.file.active = self
                                    .file
                                    .profiles
                                    .keys()
                                    .next()
                                    .cloned()
                                    .unwrap_or_default();
                            }
                            self.selected = self.file.active.clone();
                            self.confirm_delete = None;
                        }
                        ui.add_space(6.0);
                        if ghost_button(ui, "  Cancel  ").clicked() {
                            self.confirm_delete = None;
                        }
                    });
                });
        }
    }
}

/// Custom toggle switch widget — Zed-like pill-shape toggle.
fn toggle_widget<'a>(label: &'a str, on: &'a mut bool) -> impl egui::Widget + 'a {
    move |ui: &mut egui::Ui| {
        let desired_size = egui::vec2(36.0, 20.0);
        let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
        if response.clicked() {
            *on = !*on;
            response.mark_changed();
        }
        if ui.is_rect_visible(rect) {
            let how_on = ui.ctx().animate_bool(response.id, *on);
            let visuals = ui.style().interact_selectable(&response, *on);
            let bg = theme::FIELD_BG.lerp_to_gamma(theme::ACCENT, how_on);
            ui.painter().rect(
                rect,
                Rounding::same(10.0),
                bg,
                Stroke::new(1.0, visuals.bg_stroke.color),
            );
            let knob_radius = 7.5;
            let knob_x = egui::lerp(
                (rect.left() + knob_radius + 2.0)..=(rect.right() - knob_radius - 2.0),
                how_on,
            );
            ui.painter().circle(
                egui::pos2(knob_x, rect.center().y),
                knob_radius,
                Color32::WHITE,
                Stroke::NONE,
            );
        }
        let _label_id = ui.label(RichText::new(label).color(theme::TEXT_PRIMARY).size(12.5));
        response
    }
}
