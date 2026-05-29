//! Shared dark-theme egui widgets used by both BelkaTunnel's config GUI and
//! pfUsers. Extracted verbatim from BelkaTunnel's `gui.rs` so the visual
//! grammar stays identical across the two apps without copy-paste drift.
//!
//! Public surface:
//!   * `theme::*` — semantic colour constants (BG, PANEL, CARD, ACCENT, …).
//!   * `apply_zed_theme(&ctx)` — installs visuals, spacing, text styles.
//!   * `install_unicode_fonts(&ctx)` — loads a system font with Cyrillic
//!     coverage so headings like "БелкаТуннель" render with glyphs, not
//!     fallback boxes.
//!   * Form helpers: `card`, `section_title`, `field`, `pill`,
//!     `primary_button`, `ghost_button`, `danger_button`, `toggle_widget`.

use eframe::egui::{
    self, Color32, FontFamily, FontId, Frame, Margin, RichText, Rounding, Stroke, TextStyle,
};

pub mod theme {
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

/// Load a system font with broad Unicode coverage (Cyrillic, Greek, etc.) so
/// strings like "БелкаТуннель" render with real glyphs instead of fallback
/// squares. macOS-specific candidate list — the first readable file wins.
pub fn install_unicode_fonts(ctx: &egui::Context) {
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

pub fn apply_zed_theme(ctx: &egui::Context) {
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

pub fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    Frame::none()
        .fill(theme::CARD)
        .stroke(Stroke::new(1.0, theme::BORDER))
        .rounding(Rounding::same(10.0))
        .inner_margin(Margin::same(18.0))
        .show(ui, add_contents)
        .inner
}

pub fn section_title(ui: &mut egui::Ui, title: &str, subtitle: Option<&str>) {
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

pub fn field(ui: &mut egui::Ui, label: &str, hint: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
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

pub fn pill(ui: &mut egui::Ui, text: &str, color: Color32) {
    Frame::none()
        .fill(color.linear_multiply(0.18))
        .stroke(Stroke::new(1.0, color.linear_multiply(0.4)))
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(8.0, 2.0))
        .show(ui, |ui| {
            ui.label(RichText::new(text).color(color).size(10.5).strong());
        });
}

pub fn primary_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(Color32::WHITE).strong())
            .fill(theme::ACCENT)
            .stroke(Stroke::new(1.0, theme::ACCENT_HOVER))
            .rounding(Rounding::same(8.0)),
    )
}

pub fn ghost_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme::TEXT_PRIMARY))
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::BORDER))
            .rounding(Rounding::same(8.0)),
    )
}

pub fn danger_button(ui: &mut egui::Ui, text: &str) -> egui::Response {
    ui.add(
        egui::Button::new(RichText::new(text).color(theme::DANGER))
            .fill(theme::CARD)
            .stroke(Stroke::new(1.0, theme::DANGER.linear_multiply(0.4)))
            .rounding(Rounding::same(8.0)),
    )
}

/// Custom toggle switch widget — Zed-like pill-shape toggle.
pub fn toggle_widget<'a>(label: &'a str, on: &'a mut bool) -> impl egui::Widget + 'a {
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
