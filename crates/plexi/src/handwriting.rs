//! The handwriting window: write a sample of your own handwriting with the pen, and
//! save it as a font you can pick for text boxes, just like any other font. There can be
//! several, each with its own name, and each can be saved out as a `.ttf` file to use
//! in other programs.
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
use plexi_core::handwriting::{
    self, BASELINE_Y, BOX_WIDTH, FAMILY_NAME, GRID_CHARS, HandwritingFont, Library, Samples, XHEIGHT_Y,
};
use plexi_core::ink::StrokeBuilder;

use crate::app::PlexiApp;
use crate::config;

/// Everything the handwriting window needs, kept on `PlexiApp` the same way other
/// dialogs' state is.
pub struct HandwritingState {
    pub open: bool,
    /// Every font, as last saved.
    pub library: Library,
    /// Which of them the window is working on.
    pub current: usize,
    /// The name being typed for it, and the samples being written for it: saved into
    /// `library` (and installed) by Save font.
    pub name: String,
    pub samples: Samples,
    /// The glyph being written right now, if the pen (or mouse) is down in one of the
    /// boxes.
    drawing: Option<(char, StrokeBuilder)>,
    /// Where each box was painted last frame, in screen coordinates — the same trick
    /// `PlexiApp::canvas_origin` uses to remember where something was drawn so the next
    /// frame's input can be matched against it. Tests use this to find a box to write
    /// in without hard-coding the window's layout.
    pub boxes: HashMap<char, Rect>,
    /// Where the Save font button was painted last frame, for the same reason.
    pub save_button: Option<Rect>,
    /// Earlier versions of `samples`, for Ctrl+Z while the window is open.
    undo: Vec<Samples>,
    redo: Vec<Samples>,
}

impl Default for HandwritingState {
    fn default() -> Self {
        let mut library = handwriting::load(&config::handwriting_path());
        if library.fonts.is_empty() {
            library.fonts.push(HandwritingFont { name: FAMILY_NAME.into(), samples: Samples::default() });
        }
        let first = library.fonts[0].clone();
        Self {
            open: false,
            library,
            current: 0,
            name: first.name,
            samples: first.samples,
            drawing: None,
            boxes: HashMap::new(),
            save_button: None,
            undo: Vec::new(),
            redo: Vec::new(),
        }
    }
}

impl HandwritingState {
    /// Install every saved font that has something written in it. Called once at
    /// startup so handwriting fonts picked before survive a restart.
    pub fn install_saved_font(&self) {
        for f in &self.library.fonts {
            if !f.samples.is_empty() {
                plexi_core::fonts::set_handwriting(&f.name, Some(handwriting::build_font(&f.samples, &f.name)));
            }
        }
    }

    /// Whether the name or samples differ from what was last saved.
    pub fn unsaved(&self) -> bool {
        self.library.fonts.get(self.current).is_none_or(|f| f.name != self.name || f.samples != self.samples)
    }

    /// Work on font `i` instead.
    pub fn switch_to(&mut self, i: usize) {
        let Some(f) = self.library.fonts.get(i).cloned() else { return };
        self.current = i;
        self.name = f.name;
        self.samples = f.samples;
        self.drawing = None;
        self.undo.clear();
        self.redo.clear();
    }

    /// Start a new, empty font with a name no other font has.
    pub fn new_font(&mut self) {
        let taken = |n: &str| self.library.fonts.iter().any(|f| f.name == n);
        let name = (2..).map(|k| format!("Handwriting {k}")).find(|n| !taken(n)).unwrap_or_default();
        self.library.fonts.push(HandwritingFont { name, samples: Samples::default() });
        self.switch_to(self.library.fonts.len() - 1);
        let _ = handwriting::save(&self.library, &config::handwriting_path());
    }

    /// Delete the font being worked on. There's always at least one (empty) font left.
    pub fn delete_font(&mut self) {
        if self.current >= self.library.fonts.len() {
            return;
        }
        let gone = self.library.fonts.remove(self.current);
        plexi_core::fonts::set_handwriting(&gone.name, None);
        if self.library.fonts.is_empty() {
            self.library.fonts.push(HandwritingFont { name: FAMILY_NAME.into(), samples: Samples::default() });
        }
        self.switch_to(0);
        let _ = handwriting::save(&self.library, &config::handwriting_path());
    }

    /// Why the typed name can't be saved, if it can't.
    pub fn name_problem(&self) -> Option<&'static str> {
        let name = self.name.trim();
        if name.is_empty() {
            return Some("Give the font a name");
        }
        let clash = self.library.fonts.iter().enumerate().any(|(i, f)| i != self.current && f.name == name);
        clash.then_some("Another of your fonts has that name")
    }

    /// Remember the samples as they are now, before changing them.
    fn checkpoint(&mut self) {
        self.undo.push(self.samples.clone());
        self.redo.clear();
    }

    /// Ctrl+Z inside the window: take back the last stroke, clear or clear-all.
    pub fn undo(&mut self) {
        if let Some(prev) = self.undo.pop() {
            self.redo.push(std::mem::replace(&mut self.samples, prev));
        }
    }

    pub fn redo(&mut self) {
        if let Some(next) = self.redo.pop() {
            self.undo.push(std::mem::replace(&mut self.samples, next));
        }
    }

    /// Save the name and samples, and install the font under its (maybe new) name.
    /// Returns the name it had before, if it was renamed.
    fn save_to_disk(&mut self) -> Option<String> {
        let name = self.name.trim().to_owned();
        self.name = name.clone();
        let entry = HandwritingFont { name: name.clone(), samples: self.samples.clone() };
        let old = match self.library.fonts.get_mut(self.current) {
            Some(f) => Some(std::mem::replace(f, entry).name),
            None => {
                self.library.fonts.push(entry);
                self.current = self.library.fonts.len() - 1;
                None
            }
        };
        let _ = handwriting::save(&self.library, &config::handwriting_path());
        let renamed = old.filter(|o| *o != name);
        if let Some(o) = &renamed {
            plexi_core::fonts::set_handwriting(o, None);
        }
        let font = if self.samples.is_empty() { None } else { Some(handwriting::build_font(&self.samples, &name)) };
        plexi_core::fonts::set_handwriting(&name, font);
        renamed
    }
}

/// A pointer position as pixels from the box's top-left corner. Strokes are recorded in
/// these units while the pen is down, because the stroke builder's filtering and
/// simplification are tuned for page-sized units: in the box's 0..1 units they'd flatten
/// every curve into a few straight lines.
fn box_px(rect: Rect, p: Pos2) -> [f32; 2] {
    [p.x - rect.left(), p.y - rect.top()]
}

/// A finished stroke recorded in [`box_px`] units, normalized to the box: both axes are
/// divided by the box's height, not its (narrower) width, so a circular pen tip stays
/// circular — see the `BOX_WIDTH` docs in `plexi_core::handwriting`.
fn normalize(rect: Rect, mut stroke: plexi_core::Stroke) -> plexi_core::Stroke {
    let scale = rect.height().max(1.0);
    for p in &mut stroke.points {
        p.x /= scale;
        p.y /= scale;
    }
    stroke
}

/// Paint `points` (normalized box units) as one ink line `width` pixels wide.
fn paint_line(painter: &Painter, rect: Rect, points: &[plexi_core::InkPoint], width: f32, color: egui::Color32) {
    let scale = rect.height();
    let pts: Vec<Pos2> = points.iter().map(|p| pos2(rect.left() + p.x * scale, rect.top() + p.y * scale)).collect();
    match pts.as_slice() {
        [] => {}
        // A single tap (one point, no drag) still shows up as a dot.
        [one] => {
            painter.circle_filled(*one, width / 2.0, color);
        }
        _ => {
            painter.add(egui::Shape::line(pts.clone(), egui::Stroke::new(width, color)));
            // Round the ends and the joins between segments.
            for p in pts {
                painter.circle_filled(p, width / 2.0, color);
            }
        }
    }
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
    for stroke in strokes {
        paint_line(painter, rect, &stroke.points, (stroke.width * scale).max(1.0), th.ink);
    }
}

/// Feed the box's drag into the in-progress stroke for `c`, starting or finishing one
/// as the pointer goes down or up.
fn capture(app: &mut PlexiApp, ui: &Ui, response: &Response, rect: Rect, c: char) {
    let now = ui.input(|i| i.time);
    if response.drag_started() {
        let mut b = StrokeBuilder::new(Samples::new_stroke(), app.config.smoothing, 0.5);
        if let Some(pos) = response.interact_pointer_pos() {
            b.push(now, box_px(rect, pos), 0.5);
        }
        app.handwriting.drawing = Some((c, b));
    } else if response.dragged()
        && let Some((dc, b)) = app.handwriting.drawing.as_mut()
        && *dc == c
        && let Some(pos) = response.interact_pointer_pos()
    {
        b.push(now, box_px(rect, pos), 0.5);
    } else if response.drag_stopped()
        && let Some((dc, _)) = &app.handwriting.drawing
        && *dc == c
        && let Some((_, b)) = app.handwriting.drawing.take()
    {
        app.handwriting.checkpoint();
        app.handwriting.samples.push_stroke(c, normalize(rect, b.finish()));
    }
}

/// The stroke being written in `c`'s box right now, in normalized box units, so it
/// shows while the pen is still down.
fn live_stroke(app: &PlexiApp, rect: Rect, c: char) -> Option<plexi_core::Stroke> {
    let (dc, b) = app.handwriting.drawing.as_ref()?;
    (*dc == c).then(|| normalize(rect, b.stroke.clone()))
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
        if let Some(live) = live_stroke(app, rect, c) {
            let th = crate::ui::theme(ui);
            paint_line(ui.painter(), rect, &live.points, (live.width * rect.height()).max(1.0), th.ink);
        }
        ui.horizontal(|ui| {
            ui.add_space((rect.width() - 18.0).max(0.0) / 2.0);
            let has_ink = !app.handwriting.samples.strokes(c).is_empty();
            if ui.small_button("×").on_hover_text(format!("Clear '{c}'")).clicked() && has_ink {
                app.handwriting.checkpoint();
                app.handwriting.samples.clear_glyph(c);
            }
        });
    });
}

/// Glyph boxes per row in the handwriting window.
const GRID_COLUMNS: usize = 11;

pub fn window(app: &mut PlexiApp, ctx: &Context) {
    if !app.handwriting.open {
        return;
    }
    let mut open = true;
    let mut clear_all = false;
    let mut save_now = false;
    let mut export = false;
    let mut switch = None;
    let mut new_font = false;
    let mut delete = false;
    let shown = egui::Window::new("My handwriting")
        .open(&mut open)
        .default_width(620.0)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .vscroll(true)
        .show(ctx, |ui| {
            ui.label(
                "Write each character below with the pen (or mouse). Skip any you like — they'll just \
             use the default font instead. Ctrl+Z takes back the last stroke.",
            );
            let hw = &mut app.handwriting;
            ui.horizontal(|ui| {
                ui.label("Font");
                let current = hw.library.fonts.get(hw.current).map(|f| f.name.clone()).unwrap_or_default();
                egui::ComboBox::from_id_salt("handwriting-font").selected_text(current).show_ui(ui, |ui| {
                    for (i, f) in hw.library.fonts.iter().enumerate() {
                        if ui.selectable_label(i == hw.current, &f.name).clicked() && i != hw.current {
                            switch = Some(i);
                        }
                    }
                });
                if ui.button("New").on_hover_text("Start another handwriting font").clicked() {
                    new_font = true;
                }
                if ui.button("Delete").on_hover_text("Delete this font").clicked() {
                    delete = true;
                }
            });
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.add(egui::TextEdit::singleline(&mut hw.name).desired_width(200.0));
                if let Some(problem) = hw.name_problem() {
                    ui.colored_label(egui::Color32::from_rgb(0xd6, 0x3a, 0x3a), problem);
                }
            });
            ui.horizontal(|ui| {
                if ui.button("Clear all").clicked() {
                    clear_all = true;
                }
                let has_ink = !hw.samples.is_empty();
                let ok = hw.name_problem().is_none();
                let save = ui.add_enabled(ok, egui::Button::new(egui::RichText::new("Save font").strong()));
                hw.save_button = Some(save.rect);
                if save.clicked() {
                    save_now = true;
                }
                let tip = "Save it as a TrueType font file, to install or use in other programs";
                if ui.add_enabled(has_ink && ok, egui::Button::new("Export .ttf…")).on_hover_text(tip).clicked() {
                    export = true;
                }
            });
            ui.separator();
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                // Fixed rows: a wrapped layout inside an auto-sized window just grows the
                // window until every box fits on one row.
                ui.spacing_mut().item_spacing = Vec2::new(6.0, 6.0);
                let chars: Vec<char> = GRID_CHARS.chars().collect();
                for row in chars.chunks(GRID_COLUMNS) {
                    ui.horizontal(|ui| {
                        for &c in row {
                            glyph_box(app, ui, c);
                        }
                    });
                }
            });
        });
    crate::ui::bigger_close(ctx, "handwriting", shown.map(|r| r.response.rect), &mut open);
    if clear_all && !app.handwriting.samples.is_empty() {
        app.handwriting.checkpoint();
        app.handwriting.samples = Samples::default();
    }
    if (save_now || export || switch.is_some() || new_font) && app.handwriting.name_problem().is_none() {
        // Switching away (or starting another) keeps what was written: it's saved first.
        if save_now || app.handwriting.unsaved() {
            save_font(app);
        }
    }
    if export {
        export_font(app);
    }
    if let Some(i) = switch {
        app.handwriting.switch_to(i);
    }
    if new_font {
        app.handwriting.new_font();
    }
    if delete {
        let name = app.handwriting.name.clone();
        app.handwriting.delete_font();
        app.fonts.forget(&name);
        app.toast(format!("Deleted {name}"));
    }
    app.handwriting.open = open;
}

/// Save the font being worked on and show it everywhere: the font picker, and text
/// already set in it (which follows it to its new name if it was renamed).
fn save_font(app: &mut PlexiApp) {
    let renamed = app.handwriting.save_to_disk();
    let name = app.handwriting.name.clone();
    app.fonts.forget(&name);
    if let Some(old) = renamed {
        app.fonts.forget(&old);
        let mut changed = false;
        for page in app.doc.pages.iter_mut().chain(std::iter::once(&mut app.doc.overlay)) {
            for e in &mut page.elements {
                if let plexi_core::Element::Text(t) = e
                    && t.font.as_deref() == Some(old.as_str())
                {
                    t.font = Some(name.clone());
                    e.renew_id();
                    changed = true;
                }
            }
        }
        if changed {
            app.touch();
        }
        if app.config.text_font.as_deref() == Some(old.as_str()) {
            app.config.text_font = Some(name.clone());
        }
    }
    app.toast(format!("Saved {name}"));
}

/// Write the font being worked on to a `.ttf` file the user picks.
fn export_font(app: &mut PlexiApp) {
    let hw = &app.handwriting;
    let name = hw.name.trim().to_owned();
    let file: String =
        name.chars().map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' { c } else { '_' }).collect();
    let Some(path) =
        rfd::FileDialog::new().add_filter("TrueType font", &["ttf"]).set_file_name(format!("{file}.ttf")).save_file()
    else {
        return;
    };
    let path = if path.extension().is_none() { path.with_extension("ttf") } else { path };
    match std::fs::write(&path, handwriting::build_font(&hw.samples, &name)) {
        Ok(()) => app.toast(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy())),
        Err(e) => app.toast_error(format!("Couldn't save the font: {e}")),
    }
}
