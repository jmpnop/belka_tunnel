use anyhow::Result;
use eframe::egui::{
    self, Align, Color32, FontFamily, FontId, Frame, Layout, Margin, RichText, Rounding, Stroke,
    TextStyle,
};
use include_dir::{include_dir, Dir};

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
    app.activateIgnoringOtherApps(true);
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
}

fn apply_theme(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    style.visuals.dark_mode = true;
    style.visuals.override_text_color = Some(Color32::from_rgb(232, 234, 240));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::new(13.5, FontFamily::Proportional));
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
/// Upper bound on how much of the window height the animation band may take,
/// so an unexpectedly tall frame can't crowd out the text panel. The band's
/// natural height is `width / frame_aspect` (see `frame_aspect`); this only
/// clamps the pathological case.
const MAX_IMAGE_FRACTION: f32 = 0.55;
/// Fallback aspect (16:9) used only if the first frame's dimensions can't be
/// read — every shipped frame is a normal raster JPEG, so this never triggers
/// in practice.
const FALLBACK_FRAME_ASPECT: f32 = 16.0 / 9.0;

/// Width/height of a frame, read from the first JPEG's header (no full decode).
/// Sizing the image band to this means the frame fills the band edge-to-edge —
/// no letterbox bars, no crop, no distortion.
fn read_frame_aspect(bytes: &[u8]) -> f32 {
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    match dims {
        Some((w, h)) if h > 0 => w as f32 / h as f32,
        _ => FALLBACK_FRAME_ASPECT,
    }
}

/// Lower bound on cache capacity. The actual cap is `max(this, frame count)`
/// up to 256 — see `frame_cache_cap` below. We keep this constant only so
/// the floor is centralized and clear; a directory with fewer frames than
/// this still works because the cap clamps from both sides.
///
/// The previous design fixed the cap at 12 frames (~1 s at 12 fps). With
/// the current 63 baked-in frames the animation cycles every 5.25 s, which
/// meant every frame after the first second was a cache miss — we
/// re-decoded the JPEG and re-uploaded a GPU texture 12 times a second for
/// frames we had already decoded seconds earlier. Total throwaway work.
/// Now we hold all frames once we've seen them; the eager allocation that
/// the original Vec<TextureHandle> did at startup is still avoided because
/// decoding is lazy — when the About window is never opened we pay nothing.
const FRAME_CACHE_MIN_CAP: usize = 12;
/// Hard ceiling so dropping 10 000 frames in the assets directory doesn't
/// pin gigabytes of GPU memory. Tuned for the kind of looping animations
/// we'd actually ship.
const FRAME_CACHE_MAX_CAP: usize = 256;

struct AboutApp {
    /// Raw JPEG bytes per frame, indexed; never mutated.
    frame_bytes: Vec<&'static [u8]>,
    /// Decoded textures (subset, LRU-evicted to keep memory bounded).
    frame_cache: std::collections::HashMap<usize, egui::TextureHandle>,
    /// Indices in MRU order — oldest at the front, freshest at the back.
    frame_lru: std::collections::VecDeque<usize>,
    /// Per-instance eviction threshold: `frame_bytes.len()` clamped into
    /// `[FRAME_CACHE_MIN_CAP, FRAME_CACHE_MAX_CAP]`. For the typical case
    /// where it ≥ frame count, eviction never runs after the first cycle
    /// and the animation is hit-only.
    cache_cap: usize,
    /// Aspect ratio (w/h) of the frames, read once from the first frame's
    /// header. The image band is sized to this so frames fill it without bars.
    frame_aspect: f32,
    started: std::time::Instant,
    first_frame: bool,
    expanded_credits: bool,
}

impl AboutApp {
    fn new(_ctx: &egui::Context) -> Self {
        // Enumerate the JPEG sources but DON'T decode them — that happens on
        // demand in `frame_texture()`.
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
        let frame_bytes: Vec<&'static [u8]> =
            frame_files.into_iter().map(|f| f.contents()).collect();
        let cache_cap = frame_bytes
            .len()
            .clamp(FRAME_CACHE_MIN_CAP, FRAME_CACHE_MAX_CAP);
        let frame_aspect = frame_bytes
            .first()
            .map(|b| read_frame_aspect(b))
            .unwrap_or(FALLBACK_FRAME_ASPECT);
        tracing::info!(
            count = frame_bytes.len(),
            cache_cap,
            frame_aspect,
            "indexed animation frames (lazy decode)"
        );

        Self {
            frame_bytes,
            cache_cap,
            frame_aspect,
            frame_cache: std::collections::HashMap::new(),
            frame_lru: std::collections::VecDeque::new(),
            started: std::time::Instant::now(),
            first_frame: true,
            expanded_credits: false,
        }
    }

    /// Return the GPU texture for `idx`, decoding + uploading if it isn't in
    /// the cache. Promotes `idx` to most-recently-used and evicts the oldest
    /// entry if we're at capacity.
    fn frame_texture(&mut self, ctx: &egui::Context, idx: usize) -> Option<egui::TextureHandle> {
        if self.frame_cache.contains_key(&idx) {
            // Refresh LRU order.
            self.frame_lru.retain(|&i| i != idx);
            self.frame_lru.push_back(idx);
            return self.frame_cache.get(&idx).cloned();
        }
        let bytes = *self.frame_bytes.get(idx)?;
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (w, h) = img.dimensions();
        let color =
            egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], img.as_raw());
        let handle = ctx.load_texture(
            format!("bt-frame-{idx:03}"),
            color,
            egui::TextureOptions::LINEAR,
        );
        self.frame_cache.insert(idx, handle.clone());
        self.frame_lru.push_back(idx);
        while self.frame_cache.len() > self.cache_cap {
            if let Some(oldest) = self.frame_lru.pop_front() {
                self.frame_cache.remove(&oldest);
            }
        }
        Some(handle)
    }
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
                // Height is derived from the frame's aspect ratio so the frame
                // fills the band edge-to-edge — no letterbox. Clamped so a very
                // tall frame can't swallow the text panel.
                let band_h =
                    (full.width() / self.frame_aspect).min(full.height() * MAX_IMAGE_FRACTION);
                let image_rect = egui::Rect::from_min_max(
                    full.left_top(),
                    egui::pos2(full.right(), full.top() + band_h),
                );
                painter.rect_filled(image_rect, Rounding::ZERO, Color32::BLACK);

                // Animation runs only while the window is focused/visible —
                // a dropped-from-view About window otherwise burns 12fps of GPU
                // for nothing.
                let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
                let current_tex = if self.frame_bytes.is_empty() {
                    None
                } else {
                    let elapsed = self.started.elapsed().as_secs_f32();
                    let idx = (elapsed * FRAME_FPS) as usize % self.frame_bytes.len();
                    if focused {
                        ctx.request_repaint_after(std::time::Duration::from_millis(
                            (1000.0 / FRAME_FPS) as u64,
                        ));
                    }
                    self.frame_texture(ctx, idx)
                };

                if let Some(tex) = current_tex {
                    let size = tex.size_vec2();
                    // Contain (preserve aspect, no crop). Letterboxes against
                    // the BLACK fill drawn just above.
                    let scale = (image_rect.width() / size.x).min(image_rect.height() / size.y);
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
                    painter.rect_filled(
                        stripe,
                        Rounding::ZERO,
                        Color32::from_rgba_unmultiplied(BG.r(), BG.g(), BG.b(), alpha),
                    );
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
            ui.label(RichText::new(text).color(BODY).size(10.5));
        });
}

fn credit_line(ui: &mut egui::Ui, head: &str, body: &str) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(
            RichText::new(head).color(SUBTITLE).size(11.0).strong(),
        ));
        ui.add_space(6.0);
        ui.add(egui::Label::new(RichText::new(body).color(BODY).size(11.5)));
    });
    ui.add_space(2.0);
    let _ = ACCENT; // keep import warm
}
