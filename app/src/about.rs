use anyhow::Result;
use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Frame, Layout, Margin, RichText, Rounding, Stroke,
    TextStyle,
};
use include_dir::{include_dir, Dir};

const ABOUT_BG_PNG: &[u8] = include_bytes!("../assets/about-bg.png");
static BT_FRAMES_DIR: Dir = include_dir!("$CARGO_MANIFEST_DIR/assets/bt-frames");
const FRAME_FPS: f32 = 12.0;

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
    unsafe {
        app.activateIgnoringOtherApps(true);
    }
}

pub fn run() -> Result<()> {
    #[cfg(target_os = "macos")]
    set_regular_activation_policy();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([460.0, 680.0])
            .with_min_inner_size([420.0, 620.0])
            .with_max_inner_size([520.0, 760.0])
            .with_title("О БелкаТуннеле")
            .with_resizable(false)
            .with_decorations(true)
            .with_visible(true)
            .with_active(true),
        centered: true,
        ..Default::default()
    };

    eframe::run_native(
        "О БелкаТуннеле",
        options,
        Box::new(|cc| {
            install_unicode_fonts(&cc.egui_ctx);
            apply_theme(&cc.egui_ctx);
            Ok(Box::new(AboutApp::new(&cc.egui_ctx)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))?;
    Ok(())
}

fn install_unicode_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    let candidates: &[&str] = &[
        "/System/Library/Fonts/SFNSRounded.ttf",
        "/System/Library/Fonts/SFNS.ttf",
        "/System/Library/Fonts/Helvetica.ttc",
        "/System/Library/Fonts/HelveticaNeue.ttc",
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    ];
    let mut data: Option<Vec<u8>> = None;
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            data = Some(bytes);
            break;
        }
    }
    let Some(bytes) = data else { return };
    let mut fonts = FontDefinitions::default();
    fonts
        .font_data
        .insert("system-unicode".to_string(), FontData::from_owned(bytes).into());
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
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.override_text_color = Some(Color32::from_rgb(232, 234, 240));
    style.text_styles.insert(
        TextStyle::Body,
        FontId::new(13.5, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Small,
        FontId::new(11.0, FontFamily::Proportional),
    );
    style.text_styles.insert(
        TextStyle::Monospace,
        FontId::new(11.5, FontFamily::Monospace),
    );
    style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    style.visuals.window_fill = Color32::from_rgb(15, 16, 20);
    style.visuals.panel_fill = Color32::from_rgb(15, 16, 20);
    ctx.set_style(style);
}

// Palette
const BG: Color32 = Color32::from_rgb(13, 14, 18);
const PANEL: Color32 = Color32::from_rgb(17, 19, 24);
const TITLE: Color32 = Color32::from_rgb(247, 247, 250);
const SUBTITLE: Color32 = Color32::from_rgb(220, 196, 140);
const BODY: Color32 = Color32::from_rgb(216, 219, 228);
const MUTED: Color32 = Color32::from_rgb(150, 156, 172);
const FAINT: Color32 = Color32::from_rgb(110, 116, 132);
const ACCENT: Color32 = Color32::from_rgb(99, 124, 247);

const PAD: f32 = 28.0;
const IMAGE_FRACTION: f32 = 0.46;

struct AboutApp {
    bg_handle: Option<egui::TextureHandle>,
    frames: Vec<egui::TextureHandle>,
    started: std::time::Instant,
    first_frame: bool,
    expanded_credits: bool,
}

impl AboutApp {
    fn new(ctx: &egui::Context) -> Self {
        let bg_handle = decode_image(ABOUT_BG_PNG)
            .map(|img| ctx.load_texture("about-bg", img, egui::TextureOptions::LINEAR));

        // Pre-decode all video frames into GPU textures (~63 × 360×360 RGBA).
        let mut frame_files: Vec<&include_dir::File> = BT_FRAMES_DIR
            .files()
            .filter(|f| {
                f.path()
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|s| s.eq_ignore_ascii_case("jpg") || s.eq_ignore_ascii_case("jpeg"))
                    .unwrap_or(false)
            })
            .collect();
        frame_files.sort_by_key(|f| f.path().to_string_lossy().to_string());
        let frames: Vec<egui::TextureHandle> = frame_files
            .into_iter()
            .enumerate()
            .filter_map(|(i, f)| {
                let img = image::load_from_memory(f.contents()).ok()?.to_rgba8();
                let (w, h) = img.dimensions();
                let color = egui::ColorImage::from_rgba_unmultiplied(
                    [w as usize, h as usize],
                    img.as_raw(),
                );
                Some(ctx.load_texture(
                    format!("bt-frame-{i:03}"),
                    color,
                    egui::TextureOptions::LINEAR,
                ))
            })
            .collect();
        tracing::info!(frames = frames.len(), "loaded animation frames");

        Self {
            bg_handle,
            frames,
            started: std::time::Instant::now(),
            first_frame: true,
            expanded_credits: false,
        }
    }
}

fn decode_image(bytes: &[u8]) -> Option<egui::ColorImage> {
    let img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        img.as_raw(),
    ))
}

impl eframe::App for AboutApp {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [
            BG.r() as f32 / 255.0,
            BG.g() as f32 / 255.0,
            BG.b() as f32 / 255.0,
            1.0,
        ]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            #[cfg(target_os = "macos")]
            set_regular_activation_policy();
        }

        egui::CentralPanel::default()
            .frame(Frame::none().fill(BG))
            .show(ctx, |ui| {
                let full = ui.max_rect();
                let painter = ui.painter().clone();

                // ---------- Image area (top portion) ----------
                let image_rect = egui::Rect::from_min_max(
                    full.left_top(),
                    egui::pos2(full.right(), full.top() + full.height() * IMAGE_FRACTION),
                );
                painter.rect_filled(image_rect, Rounding::ZERO, Color32::BLACK);

                // Prefer the animated video frame; fall back to the still BG if frames missing.
                let current_tex = if !self.frames.is_empty() {
                    let elapsed = self.started.elapsed().as_secs_f32();
                    let idx = (elapsed * FRAME_FPS) as usize % self.frames.len();
                    ctx.request_repaint_after(std::time::Duration::from_millis(
                        (1000.0 / FRAME_FPS) as u64,
                    ));
                    Some(&self.frames[idx])
                } else {
                    self.bg_handle.as_ref()
                };

                if let Some(tex) = current_tex {
                    let size = tex.size_vec2();
                    let scale =
                        (image_rect.width() / size.x).max(image_rect.height() / size.y);
                    let scaled = size * scale;
                    let center = image_rect.center();
                    let img_rect = egui::Rect::from_center_size(center, scaled);
                    painter.image(
                        tex.id(),
                        img_rect,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        Color32::WHITE,
                    );
                }

                // Smooth fade from image into the dark panel below
                let fade_height = 80.0;
                let fade_top = image_rect.bottom() - fade_height * 0.6;
                let steps = 60;
                for i in 0..steps {
                    let t = i as f32 / steps as f32;
                    let alpha = ((t * t * 0.95) * 255.0) as u8;
                    let stripe = egui::Rect::from_min_max(
                        egui::pos2(image_rect.left(), fade_top + t * fade_height),
                        egui::pos2(
                            image_rect.right(),
                            fade_top + ((i + 1) as f32 / steps as f32) * fade_height,
                        ),
                    );
                    painter.rect_filled(stripe, Rounding::ZERO, Color32::from_rgba_unmultiplied(
                        BG.r(), BG.g(), BG.b(), alpha,
                    ));
                }

                // ---------- Text panel (bottom portion) ----------
                let panel_rect = egui::Rect::from_min_max(
                    egui::pos2(full.left(), image_rect.bottom()),
                    full.right_bottom(),
                );
                painter.rect_filled(panel_rect, Rounding::ZERO, PANEL);

                // Inner content area with padding
                let content_rect = egui::Rect::from_min_max(
                    egui::pos2(panel_rect.left() + PAD, panel_rect.top() + PAD * 0.7),
                    egui::pos2(panel_rect.right() - PAD, panel_rect.bottom() - PAD * 0.6),
                );

                let mut content_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(content_rect)
                        .layout(Layout::top_down(Align::Min)),
                );

                // Title
                content_ui.scope(|ui| {
                    ui.style_mut().spacing.item_spacing.y = 2.0;
                    ui.add(egui::Label::new(
                        RichText::new("БелкаТуннель")
                            .size(34.0)
                            .strong()
                            .color(TITLE),
                    ));
                    ui.add(egui::Label::new(
                        RichText::new("Squirrel.Tunnel · v0.1.0")
                            .size(12.5)
                            .color(SUBTITLE),
                    ));
                });

                content_ui.add_space(14.0);

                // Tagline
                content_ui.add(egui::Label::new(
                    RichText::new("Зашифрованный канал через домашний роутер.")
                        .size(13.5)
                        .color(BODY),
                ));
                content_ui.add_space(2.0);
                content_ui.add(egui::Label::new(
                    RichText::new("Браузер → SSH → WAN → интернет.")
                        .size(12.0)
                        .color(MUTED),
                ));

                content_ui.add_space(16.0);

                // Tech chips, wrapped
                content_ui.horizontal_wrapped(|ui| {
                    chip(ui, "russh 0.45");
                    chip(ui, "tokio 1.x");
                    chip(ui, "egui 0.30");
                    chip(ui, "tao + muda");
                });

                content_ui.add_space(16.0);

                // Divider
                let divider_y = content_ui.cursor().top();
                painter.line_segment(
                    [
                        egui::pos2(content_rect.left(), divider_y),
                        egui::pos2(content_rect.right(), divider_y),
                    ],
                    Stroke::new(1.0, Color32::from_rgb(35, 38, 46)),
                );

                content_ui.add_space(10.0);

                // Credits toggle
                let arrow = if self.expanded_credits { "▾" } else { "▸" };
                let label_resp = content_ui.add(
                    egui::Label::new(
                        RichText::new(format!("{arrow}  Credits & notes"))
                            .color(MUTED)
                            .size(12.0),
                    )
                    .sense(egui::Sense::click()),
                );
                if label_resp.clicked() {
                    self.expanded_credits = !self.expanded_credits;
                }
                if self.expanded_credits {
                    content_ui.add_space(8.0);
                    credit_line(&mut content_ui, "Tunnel", "russh — pure-Rust SSH");
                    credit_line(&mut content_ui, "UI", "egui / eframe");
                    credit_line(&mut content_ui, "Menu bar", "tray-icon + muda");
                    credit_line(&mut content_ui, "Art", "voxel scene by datoora");
                }

                // ---------- Close hint, bottom-right of panel ----------
                let hint = "Esc — close";
                let hint_galley = painter.layout_no_wrap(
                    hint.to_string(),
                    FontId::new(10.5, FontFamily::Proportional),
                    FAINT,
                );
                let hint_pos = egui::pos2(
                    panel_rect.right() - PAD - hint_galley.size().x,
                    panel_rect.bottom() - PAD * 0.6 - hint_galley.size().y - 2.0,
                );
                painter.galley(hint_pos, hint_galley, FAINT);
            });

        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

fn chip(ui: &mut egui::Ui, text: &str) {
    Frame::none()
        .fill(Color32::from_rgb(24, 27, 33))
        .stroke(Stroke::new(1.0, Color32::from_rgb(46, 50, 60)))
        .rounding(Rounding::same(999.0))
        .inner_margin(Margin::symmetric(10.0, 4.0))
        .show(ui, |ui| {
            ui.label(
                RichText::new(text)
                    .color(BODY)
                    .size(10.5),
            );
        });
}

fn credit_line(ui: &mut egui::Ui, head: &str, body: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(
            RichText::new(head)
                .color(SUBTITLE)
                .size(11.0)
                .strong(),
        ));
        ui.add_space(6.0);
        ui.add(egui::Label::new(
            RichText::new(body)
                .color(BODY)
                .size(11.5),
        ));
    });
    ui.add_space(2.0);
    let _ = ACCENT; // keep import warm
}
