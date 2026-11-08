//! The handwriting window: write a sample of your own handwriting with the pen, and
//! save it as a font you can pick for text boxes, just like any other font.
//!
//! Each box captures its own pen strokes directly, independently of the main canvas —
//! but through the same [`plexi_core::ink::StrokeBuilder`] the canvas uses, so the line
//! looks and smooths the same way while you write. Saving turns the samples into a
//! TrueType font ([`plexi_core::handwriting::build_font`]) and hands the bytes to
//! `plexi_core::fonts`, then tells the app's [`FontBook`](crate::fonts::FontBook) to
//! forget what it knew about the family name, so the new glyphs show up without a
//! restart.

use std::collections::HashMap;

use egui::{Context, Painter, Pos2, Rect, Response, Sense, Ui, Vec2, pos2};
use plexi_core::handwriting::{self, BASELINE_Y, BOX_WIDTH, FAMILY_NAME, GRID_CHARS, Samples, XHEIGHT_Y};
use plexi_core::ink::StrokeBuilder;

use crate::app::PlexiApp;
use crate::config;

/// Everything the handwriting window needs, kept on `PlexiApp` the same way other
/// dialogs' state is.
pub struct HandwritingState {
    pub open: bool,
    pub samples: Samples,
    /// The glyph being written right now, if the pen (or mouse) is down in one of the
    /// boxes.
    drawing: Option<(char, StrokeBuilder)>,
    /// Where each box was painted last frame, in screen coordinates — the same trick
    /// `PlexiApp::canvas_origin` uses to remember where something was drawn so the next
    /// frame's input can be matched against it. Tests use this to find a box to write
    /// in without hard-coding the window's layout.
    pub boxes: HashMap<char, Rect>,
}

impl Default for HandwritingState {
    fn default() -> Self {
        Self {
            open: false,
            samples: handwriting::load(&config::handwriting_path()),
            drawing: None,
            boxes: HashMap::new(),
        }
    }
}

impl HandwritingState {
    /// Install the saved font, if any character was written before. Called once at
    /// startup so a handwriting font picked before survives a restart.
    pub fn install_saved_font(&self) {
        if !self.samples.is_empty() {
            plexi_core::fonts::set_handwriting(Some(handwriting::build_font(&self.samples)));
        }
    }

    fn save_to_disk(&self) {
        let _ = handwriting::save(&self.samples, &config::handwriting_path());
        let font = if self.samples.is_empty() { None } else { Some(handwriting::build_font(&self.samples)) };
        plexi_core::fonts::set_handwriting(font);
    }
}

/// A box's pixel position in a normalized (`x`, `y`) pen sample: both axes are divided
/// by the box's height, not its (narrower) width, so a circular pen tip stays circular
/// — see the `BOX_WIDTH` docs in `plexi_core::handwriting`.
fn normalize(rect: Rect, p: Pos2) -> [f32; 2] {
    let scale = rect.height().max(1.0);
    [(p.x - rect.left()) / scale, (p.y - rect.top()) / scale]
}

/// Paint one glyph box: its border, baseline/x-height guides, a faint target character
/// for anything not written yet, and the ink already saved for it.
fn paint_box(ui: &Ui, painter: &Painter, rect: Rect, c: char, strokes: &[plexi_core::Stroke]) {
    use egui::{Align2, FontId, Stroke as EguiStroke};
    let th = crate::ui::theme(ui);
    painter.rect_filled(rect, 3.0, th.canvas);
    painter.rect_stroke(rect, 3.0, EguiStroke::new(1.0, th.ink.gamma_multiply(0.35)), egui::StrokeKind::Inside);
    let scale = rect.height();
    let guide = |frac: f32| rect.top() + frac * scale;
    let baseline = EguiStroke::new(1.0, th.ink.gamma_multiply(0.18));
    let xheight = EguiStroke::new(1.0, th.ink.gamma_multiply(0.1));
    painter.hline(rect.left()..=rect.right(), guide(BASELINE_Y), baseline);
    painter.hline(rect.left()..=rect.right(), guide(XHEIGHT_Y), xheight);
    if strokes.is_empty() {
        painter.text(
            pos2(rect.left() + scale * 0.1, guide(BASELINE_Y)),
            Align2::LEFT_BOTTOM,
            c,
            FontId::proportional(scale * 0.62),
            th.ink.gamma_multiply(0.22),
        );
    }
    let to_screen = |p: [f32; 2]| pos2(rect.left() + p[0] * scale, rect.top() + p[1] * scale);
    for stroke in strokes {
        let width = (stroke.width * scale).max(1.0);
        let ink = EguiStroke::new(width, th.ink);
        for pair in stroke.points.windows(2) {
            painter.line_segment([to_screen(pair[0].pos()), to_screen(pair[1].pos())], ink);
        }
        // A single tap (one point, no drag) still shows up as a dot.
        if stroke.points.len() == 1 {
            painter.circle_filled(to_screen(stroke.points[0].pos()), width / 2.0, th.ink);
        }
    }
}

/// Feed the box's drag into the in-progress stroke for `c`, starting or finishing one
/// as the pointer goes down or up.
fn capture(app: &mut PlexiApp, ui: &Ui, response: &Response, rect: Rect, c: char) {
    let now = ui.input(|i| i.time);
    if response.drag_started() {
        let min_step = 0.5 / rect.height().max(1.0);
        let mut b = StrokeBuilder::new(Samples::new_stroke(), app.config.smoothing, min_step);
        if let Some(pos) = response.interact_pointer_pos() {
            b.push(now, normalize(rect, pos), 0.5);
        }
        app.handwriting.drawing = Some((c, b));
    } else if response.dragged()
        && let Some((dc, b)) = app.handwriting.drawing.as_mut()
        && *dc == c
        && let Some(pos) = response.interact_pointer_pos()
    {
        b.push(now, normalize(rect, pos), 0.5);
    } else if response.drag_stopped()
        && let Some((dc, _)) = &app.handwriting.drawing
        && *dc == c
        && let Some((_, b)) = app.handwriting.drawing.take()
    {
        app.handwriting.samples.push_stroke(c, b.finish());
    }
}

/// One character's box, plus a small button to clear just that glyph.
fn glyph_box(app: &mut PlexiApp, ui: &mut Ui, c: char) {
    let height = 64.0;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 2.0;
        let (rect, response) = ui.allocate_exact_size(Vec2::new(height * BOX_WIDTH, height), Sense::drag());
        app.handwriting.boxes.insert(c, rect);
        paint_box(ui, ui.painter(), rect, c, app.handwriting.samples.strokes(c));
        capture(app, ui, &response, rect, c);
        ui.horizontal(|ui| {
            ui.add_space((rect.width() - 18.0).max(0.0) / 2.0);
            if ui.small_button("×").on_hover_text(format!("Clear '{c}'")).clicked() {
                app.handwriting.samples.clear_glyph(c);
            }
        });
    });
}

pub fn window(app: &mut PlexiApp, ctx: &Context) {
    if !app.handwriting.open {
        return;
    }
    let mut open = true;
    let mut clear_all = false;
    let mut save_now = false;
    egui::Window::new("My handwriting").open(&mut open).default_width(620.0).vscroll(true).show(ctx, |ui| {
        ui.label(
            "Write each character below with the pen (or mouse). Skip any you like — they'll just \
             use the default font instead.",
        );
        ui.horizontal(|ui| {
            if ui.button("Clear all").clicked() {
                clear_all = true;
            }
            let has_ink = !app.handwriting.samples.is_empty();
            if ui.add_enabled(has_ink, egui::Button::new(egui::RichText::new("Save font").strong())).clicked() {
                save_now = true;
            }
        });
        ui.separator();
        egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                for c in GRID_CHARS.chars() {
                    glyph_box(app, ui, c);
                }
            });
        });
    });
    if clear_all {
        app.handwriting.samples = Samples::default();
    }
    if save_now {
        app.handwriting.save_to_disk();
        app.fonts.forget(FAMILY_NAME);
        app.toast(format!("Saved {FAMILY_NAME}"));
    }
    app.handwriting.open = open;
}
