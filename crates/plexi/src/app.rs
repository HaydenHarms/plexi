//! Application state and the frame loop: file handling, mode switching between the
//! notebook window and the on-screen overlay, commands and shortcuts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Context, Key, Modifiers, ViewportCommand};
#[cfg(test)]
use plexi_core::Shape;
use plexi_core::export::{ExportFormat, RenderOptions};
use plexi_core::{Color, Document, Element, ImageEl, InkKind, Page, ShapeKind, TextBox, format, new_id};

use crate::canvas::{Interaction, MeshCache, View};
use crate::config::{self, Config};
use crate::history::{History, Snapshot, Target};
use crate::platform::{self, PxRect};
use crate::shell::{Shell, ShellEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
    Lasso,
    Text,
    Shape(ShapeKind),
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen => "Pen (P)",
            Tool::Highlighter => "Highlighter (H)",
            Tool::Eraser => "Eraser (E) — right-drag erases with any tool",
            Tool::Lasso => "Select (S) — lasso, drag to move, Del to delete",
            Tool::Text => "Text (T)",
            Tool::Shape(ShapeKind::Line) => "Line (L)",
            Tool::Shape(ShapeKind::Arrow) => "Arrow (A)",
            Tool::Shape(ShapeKind::Rect) => "Rectangle (R)",
            Tool::Shape(ShapeKind::Ellipse) => "Ellipse (O)",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotAction {
    /// Desktop plus ink onto the clipboard.
    Copy,
    /// Desktop as a new notebook page with the ink on top, still editable.
    ToPage,
    /// Desktop plus ink to an image file.
    SaveImage,
}

#[derive(Clone, Debug)]
pub struct Overlay {
    pub passthrough: bool,
    /// Physical rect the overlay covers.
    pub monitor: Option<PxRect>,
    /// Frames left before the window geometry is (re)applied.
    pub place_in: u8,
    /// Show the "click-through" hint until this time.
    pub hint_until: f64,
    pub snapshot: Option<(SnapshotAction, u8)>,
}

#[derive(Clone, Debug)]
pub enum Mode {
    Notebook,
    Overlay(Overlay),
}

/// Window state to restore when leaving the overlay.
#[derive(Clone, Copy, Debug)]
struct NotebookGeometry {
    rect: Option<PxRect>,
    maximized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportScope {
    CurrentPage,
    AllPages,
    ScreenInk,
}

pub struct ExportDialog {
    pub format: ExportFormat,
    pub scope: ExportScope,
    pub scale: f32,
    pub background: bool,
}

#[derive(Default)]
pub struct PaletteState {
    pub open: bool,
    pub query: String,
    pub selected: usize,
}

pub struct Toast {
    pub text: String,
    pub until: Instant,
    pub error: bool,
}

pub struct StartOptions {
    pub file: Option<PathBuf>,
    pub overlay: bool,
    /// Start invisible in click-through overlay mode, waiting for the hotkey.
    pub background: bool,
    /// Register global hotkeys and the tray icon (off in tests).
    pub shell: bool,
}

enum Job {
    Saved(PathBuf, Result<(), String>),
    Exported(Result<Vec<PathBuf>, String>),
}

pub struct PlexiApp {
    pub doc: Document,
    /// File the document lives in; `None` means the scratch file.
    pub path: Option<PathBuf>,
    pub dirty: bool,
    last_edit: Instant,
    saving: bool,
    pub history: History,
    pub page: usize,
    pub mode: Mode,
    pub tool: Tool,
    pub pen_color: Color,
    pub highlighter_color: Color,
    pub view: View,
    pub fit_pending: bool,
    pub interaction: Interaction,
    /// Snapshot taken when an interaction began; pushed to history only if it changed
    /// something.
    pub pending_snapshot: Option<Snapshot>,
    pub selection: HashSet<u64>,
    pub editing_text: Option<(Target, u64)>,
    pub text_buf: String,
    pub text_focus_pending: bool,
    pub pen_pressure: Option<f32>,
    pub last_sample_time: f64,
    pub cache: MeshCache,
    pub textures: HashMap<String, egui::TextureHandle>,
    pub config: Config,
    pub palette: PaletteState,
    pub export: Option<ExportDialog>,
    pub settings_open: bool,
    pub help_open: bool,
    pub toast: Option<Toast>,
    shell: Option<Shell>,
    hwnd: Option<isize>,
    notebook_geometry: Option<NotebookGeometry>,
    /// Physical rect to put the notebook window back to, after a few frames' delay.
    restore_rect: Option<(PxRect, u8)>,
    jobs_tx: Sender<Job>,
    jobs_rx: Receiver<Job>,
    last_title: String,
    start: Option<StartOptions>,
    pub quit_requested: bool,
}

impl PlexiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: StartOptions) -> Self {
        crate::ui::install_style(&cc.egui_ctx);
        let config = Config::load();
        let (jobs_tx, jobs_rx) = channel();
        let mut app = Self {
            doc: Document::new("Untitled"),
            path: None,
            dirty: false,
            last_edit: Instant::now(),
            saving: false,
            history: History::default(),
            page: 0,
            mode: Mode::Notebook,
            tool: Tool::Pen,
            pen_color: config.pen_colors.first().copied().unwrap_or(Color::BLACK),
            highlighter_color: config.highlighter_colors.first().copied().unwrap_or(Color::rgba(255, 224, 46, 110)),
            view: View::default(),
            fit_pending: true,
            interaction: Interaction::Idle,
            pending_snapshot: None,
            selection: HashSet::new(),
            editing_text: None,
            text_buf: String::new(),
            text_focus_pending: false,
            pen_pressure: None,
            last_sample_time: 0.0,
            cache: MeshCache::default(),
            textures: HashMap::new(),
            config,
            palette: PaletteState::default(),
            export: None,
            settings_open: false,
            help_open: false,
            toast: None,
            shell: None,
            hwnd: None,
            notebook_geometry: None,
            restore_rect: None,
            jobs_tx,
            jobs_rx,
            last_title: String::new(),
            start: None,
            quit_requested: false,
        };
        if start.shell {
            let shell = Shell::start(&cc.egui_ctx, &app.config);
            if !shell.failed_hotkeys.is_empty() {
                app.toast_error(format!("Hotkey already in use by another app: {}", shell.failed_hotkeys.join(", ")));
            }
            app.shell = Some(shell);
        }

        let file = start.file.clone().or_else(|| app.config.last_file.clone().filter(|p| p.exists()));
        match file {
            Some(p) => app.open_path(&p),
            None => {
                let scratch = config::scratch_path();
                if let Ok(doc) = format::load(&scratch) {
                    app.doc = doc;
                }
            }
        }
        app.start = Some(start);
        app
    }

    // ----- document access -------------------------------------------------------

    pub fn page_ref(&self, t: Target) -> &Page {
        match t {
            Target::Page(i) => &self.doc.pages[i.min(self.doc.pages.len() - 1)],
            Target::Overlay => &self.doc.overlay,
        }
    }

    pub fn page_mut(&mut self, t: Target) -> &mut Page {
        match t {
            Target::Page(i) => {
                let i = i.min(self.doc.pages.len() - 1);
                &mut self.doc.pages[i]
            }
            Target::Overlay => &mut self.doc.overlay,
        }
    }

    /// The element list the user is drawing on right now.
    pub fn target(&self) -> Target {
        match self.mode {
            Mode::Notebook => Target::Page(self.page),
            Mode::Overlay(_) => Target::Overlay,
        }
    }

    pub fn snapshot_of(&self, t: Target) -> Snapshot {
        Snapshot::Elements(t, self.page_ref(t).elements.clone())
    }

    /// Record the current state of `t` before changing it.
    pub fn checkpoint(&mut self, t: Target) {
        let s = self.snapshot_of(t);
        self.history.push(s);
        self.touch();
    }

    pub fn checkpoint_pages(&mut self) {
        self.history.push(Snapshot::Pages(self.doc.pages.clone(), self.page));
        self.touch();
    }

    /// Mark the document changed (autosave picks it up shortly after).
    pub fn touch(&mut self) {
        self.dirty = true;
        self.last_edit = Instant::now();
    }

    fn restore(&mut self, s: Snapshot) -> Snapshot {
        match s {
            Snapshot::Elements(t, els) => {
                let page = self.page_mut(t);
                let current = std::mem::replace(&mut page.elements, els);
                Snapshot::Elements(t, current)
            }
            Snapshot::Pages(pages, idx) => {
                let current = std::mem::replace(&mut self.doc.pages, pages);
                let cur_idx = self.page;
                self.page = idx.min(self.doc.pages.len() - 1);
                self.fit_pending = true;
                Snapshot::Pages(current, cur_idx)
            }
        }
    }

    pub fn undo(&mut self) {
        self.finish_text_edit();
        if let Some(s) = self.history.pop_undo() {
            let back = self.restore(s);
            self.history.push_redo(back);
            self.selection.clear();
            self.touch();
        }
    }

    pub fn redo(&mut self) {
        self.finish_text_edit();
        if let Some(s) = self.history.pop_redo() {
            let back = self.restore(s);
            self.history.push_keep_redo(back);
            self.selection.clear();
            self.touch();
        }
    }

    // ----- files ---------------------------------------------------------------

    pub fn display_name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Untitled".into())
    }

    fn reset_view_state(&mut self) {
        self.history.clear();
        self.selection.clear();
        self.editing_text = None;
        self.interaction = Interaction::Idle;
        self.page = 0;
        self.fit_pending = true;
        self.textures.clear();
        self.dirty = false;
    }

    pub fn open_path(&mut self, p: &Path) {
        match format::load(p) {
            Ok(doc) => {
                self.flush_save();
                self.doc = doc;
                self.path = Some(p.to_path_buf());
                self.doc.title = self.display_name();
                self.config.push_recent(p);
                self.config.last_file = Some(p.to_path_buf());
                self.config.save();
                self.reset_view_state();
            }
            Err(e) => self.toast_error(format!("Could not open {}: {e}", p.display())),
        }
    }

    pub fn open_dialog(&mut self) {
        if let Some(p) = rfd::FileDialog::new().add_filter("Plexi notebook", &[format::EXTENSION]).pick_file() {
            self.open_path(&p);
        }
    }

    pub fn new_document(&mut self) {
        self.flush_save();
        let untitled_with_content = self.path.is_none() && self.has_content();
        if untitled_with_content {
            let answer = rfd::MessageDialog::new()
                .set_title("Plexi")
                .set_description("Save the current notes before starting new ones?")
                .set_buttons(rfd::MessageButtons::YesNoCancel)
                .show();
            match answer {
                rfd::MessageDialogResult::Yes => {
                    if !self.save_as() {
                        return;
                    }
                }
                rfd::MessageDialogResult::No => {}
                _ => return,
            }
        }
        let overlay = std::mem::replace(&mut self.doc.overlay, Page::overlay(1920.0, 1080.0));
        self.doc = Document::new("Untitled");
        self.doc.overlay = overlay;
        let page = &mut self.doc.pages[0];
        page.size = self.config.new_page_size;
        page.background = self.config.new_page_background;
        self.path = None;
        self.config.last_file = None;
        self.config.save();
        let _ = std::fs::remove_file(config::scratch_path());
        self.reset_view_state();
    }

    fn has_content(&self) -> bool {
        self.doc.pages.iter().any(|p| !p.elements.is_empty()) || self.doc.pages.len() > 1
    }

    fn save_target(&self) -> PathBuf {
        self.path.clone().unwrap_or_else(config::scratch_path)
    }

    /// Save in the background (the UI never waits on disk).
    pub fn save(&mut self) {
        if self.path.is_none() {
            self.save_as();
            return;
        }
        self.spawn_save(true);
    }

    fn spawn_save(&mut self, announce: bool) {
        if self.saving {
            return;
        }
        self.finish_text_edit_keep();
        self.doc.prune_assets();
        let doc = self.doc.clone();
        let path = self.save_target();
        let tx = self.jobs_tx.clone();
        self.saving = true;
        self.dirty = false;
        std::thread::spawn(move || {
            let r = format::save(&doc, &path).map_err(|e| e.to_string());
            let _ = tx.send(Job::Saved(if announce { path } else { PathBuf::new() }, r));
        });
    }

    /// Save synchronously (on exit and before switching files).
    pub fn flush_save(&mut self) {
        if !self.dirty {
            return;
        }
        self.finish_text_edit_keep();
        self.doc.prune_assets();
        if let Err(e) = format::save(&self.doc, &self.save_target()) {
            log::error!("save failed: {e}");
        }
        self.dirty = false;
    }

    /// Returns false if the user cancelled.
    pub fn save_as(&mut self) -> bool {
        let name = format!("{}.{}", self.display_name(), format::EXTENSION);
        let Some(mut p) =
            rfd::FileDialog::new().add_filter("Plexi notebook", &[format::EXTENSION]).set_file_name(name).save_file()
        else {
            return false;
        };
        if p.extension().is_none() {
            p.set_extension(format::EXTENSION);
        }
        let was_scratch = self.path.is_none();
        self.path = Some(p.clone());
        self.doc.title = self.display_name();
        self.config.push_recent(&p);
        self.config.last_file = Some(p);
        self.config.save();
        self.dirty = true;
        self.saving = false;
        self.flush_save();
        if was_scratch {
            let _ = std::fs::remove_file(config::scratch_path());
        }
        self.toast(format!("Saved {}", self.display_name()));
        true
    }

    fn autosave(&mut self) {
        if self.config.autosave
            && self.dirty
            && self.last_edit.elapsed() > Duration::from_millis(1500)
            && matches!(self.interaction, Interaction::Idle)
        {
            self.spawn_save(false);
        }
    }

    fn poll_jobs(&mut self) {
        while let Ok(job) = self.jobs_rx.try_recv() {
            match job {
                Job::Saved(path, r) => {
                    self.saving = false;
                    match r {
                        Ok(()) if !path.as_os_str().is_empty() => {
                            self.toast(format!("Saved {}", path.file_name().unwrap_or_default().to_string_lossy()))
                        }
                        Ok(()) => {}
                        Err(e) => {
                            self.dirty = true;
                            self.toast_error(format!("Save failed: {e}"));
                        }
                    }
                }
                Job::Exported(Ok(files)) => {
                    let first = files.first().map(|f| f.display().to_string()).unwrap_or_default();
                    let more = if files.len() > 1 { format!(" (+{} more)", files.len() - 1) } else { String::new() };
                    self.toast(format!("Exported {first}{more}"));
                }
                Job::Exported(Err(e)) => self.toast_error(format!("Export failed: {e}")),
            }
        }
    }

    // ----- export ----------------------------------------------------------------

    pub fn run_export(&mut self) {
        let Some(dlg) = &self.export else { return };
        let (format, scope) = (dlg.format, dlg.scope);
        let opts = RenderOptions { scale: dlg.scale, background: dlg.background, text: true };
        let stem = match scope {
            ExportScope::CurrentPage if self.doc.pages.len() > 1 => {
                format!("{} - {}", self.display_name(), self.doc.pages[self.page].name)
            }
            ExportScope::ScreenInk => format!("{} - screen", self.display_name()),
            _ => self.display_name(),
        };
        let Some(mut path) = rfd::FileDialog::new()
            .add_filter(format.label(), &[format.extension()])
            .set_file_name(format!("{stem}.{}", format.extension()))
            .save_file()
        else {
            return;
        };
        if path.extension().is_none() {
            path.set_extension(format.extension());
        }
        self.export = None;
        self.finish_text_edit_keep();
        let doc = self.doc.clone();
        let page = self.page;
        let tx = self.jobs_tx.clone();
        self.toast("Exporting…");
        std::thread::spawn(move || {
            let pages: Vec<&Page> = match scope {
                ExportScope::CurrentPage => vec![&doc.pages[page]],
                ExportScope::AllPages => doc.pages.iter().collect(),
                ExportScope::ScreenInk => vec![&doc.overlay],
            };
            let r = plexi_core::export::export(&doc, &pages, format, &opts, &path).map_err(|e| e.to_string());
            let _ = tx.send(Job::Exported(r));
        });
    }

    // ----- pages -----------------------------------------------------------------

    pub fn add_page(&mut self) {
        self.finish_text_edit();
        self.checkpoint_pages();
        let mut p = Page::new(format!("Page {}", self.doc.pages.len() + 1), self.config.new_page_size);
        p.background = self.doc.pages[self.page].background;
        p.size = self.doc.pages[self.page].size;
        self.doc.pages.insert(self.page + 1, p);
        self.go_to_page(self.page + 1);
    }

    pub fn delete_page(&mut self) {
        self.checkpoint_pages();
        if self.doc.pages.len() == 1 {
            self.doc.pages[0].elements.clear();
        } else {
            self.doc.pages.remove(self.page);
            self.page = self.page.min(self.doc.pages.len() - 1);
        }
        self.selection.clear();
        self.fit_pending = true;
    }

    pub fn go_to_page(&mut self, i: usize) {
        self.finish_text_edit();
        let i = i.min(self.doc.pages.len() - 1);
        if i != self.page {
            self.page = i;
            self.selection.clear();
            self.interaction = Interaction::Idle;
            self.fit_pending = true;
        }
    }

    // ----- clipboard & drops -------------------------------------------------------

    /// Center of the visible canvas, in page units of the current target.
    pub fn view_center(&self, ctx: &Context) -> [f32; 2] {
        let r = ctx.content_rect();
        match self.mode {
            Mode::Notebook => self.view.to_page(r.min, r.center()),
            Mode::Overlay(_) => [r.center().x, r.center().y],
        }
    }

    pub fn insert_image(&mut self, bytes: Vec<u8>, at: Option<[f32; 2]>, ctx: &Context) {
        let Ok(img) = image::load_from_memory(&bytes) else {
            self.toast_error("That image format isn't supported");
            return;
        };
        let (w, h) = (img.width() as f32, img.height() as f32);
        // PNG and JPEG are kept as-is; anything else is converted to PNG.
        let stored = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) || bytes.starts_with(&[0xff, 0xd8]) {
            bytes
        } else {
            let mut out = std::io::Cursor::new(Vec::new());
            if img.write_to(&mut out, image::ImageFormat::Png).is_err() {
                return;
            }
            out.into_inner()
        };
        let ppp = ctx.pixels_per_point();
        let visible = ctx.content_rect().size() / self.zoom_for_target();
        let mut size = [w / ppp, h / ppp];
        let fit = (visible.x * 0.7 / size[0]).min(visible.y * 0.7 / size[1]).min(1.0);
        size = [size[0] * fit, size[1] * fit];
        let c = at.unwrap_or_else(|| self.view_center(ctx));
        let pos = [c[0] - size[0] / 2.0, c[1] - size[1] / 2.0];
        let t = self.target();
        self.checkpoint(t);
        let asset = self.doc.add_asset(stored);
        let id = new_id();
        self.page_mut(t).elements.push(Element::Image(ImageEl { id, asset, pos, size }));
        self.selection = [id].into();
        self.tool = Tool::Lasso;
    }

    pub fn insert_text(&mut self, text: String, ctx: &Context) {
        let c = self.view_center(ctx);
        let t = self.target();
        self.checkpoint(t);
        let size = 20.0;
        let color = self.pen_color_for_text();
        self.page_mut(t).elements.push(Element::Text(TextBox { id: new_id(), pos: c, text, size, color }));
    }

    pub fn pen_color_for_text(&self) -> Color {
        if self.pen_color.a() == 0 { Color::BLACK } else { self.pen_color.with_alpha(255) }
    }

    fn zoom_for_target(&self) -> f32 {
        match self.mode {
            Mode::Notebook => self.view.zoom,
            Mode::Overlay(_) => 1.0,
        }
    }

    fn handle_drops_and_paste(&mut self, ctx: &Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.clone());
        for f in dropped {
            let path = f.path().to_path_buf();
            if path.as_os_str().is_empty() {
                continue;
            }
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext == format::EXTENSION {
                self.open_path(&path);
                return;
            }
            if let Ok(bytes) = std::fs::read(&path) {
                let at = ctx.input(|i| i.pointer.hover_pos()).map(|p| self.screen_to_target(ctx, p));
                self.insert_image(bytes, at, ctx);
            }
        }
        if self.editing_text.is_some() || ctx.egui_wants_keyboard_input() {
            return;
        }
        let events = ctx.input(|i| i.events.clone());
        for ev in events {
            match ev {
                egui::Event::PasteImage(img) => {
                    let mut out = std::io::Cursor::new(Vec::new());
                    let rgba =
                        image::RgbaImage::from_raw(img.size[0] as u32, img.size[1] as u32, img.as_raw().to_vec());
                    if let Some(rgba) = rgba
                        && rgba.write_to(&mut out, image::ImageFormat::Png).is_ok()
                    {
                        self.insert_image(out.into_inner(), None, ctx);
                    }
                }
                egui::Event::Paste(text) if !text.trim().is_empty() => self.insert_text(text, ctx),
                _ => {}
            }
        }
    }

    pub fn screen_to_target(&self, ctx: &Context, p: egui::Pos2) -> [f32; 2] {
        match self.mode {
            Mode::Notebook => self.view.to_page(ctx.content_rect().min, p),
            Mode::Overlay(_) => [p.x, p.y],
        }
    }

    // ----- selection ---------------------------------------------------------------

    pub fn delete_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let t = self.target();
        self.checkpoint(t);
        let sel = std::mem::take(&mut self.selection);
        self.page_mut(t).elements.retain(|e| !sel.contains(&e.id()));
    }

    pub fn duplicate_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let t = self.target();
        self.checkpoint(t);
        let copies: Vec<Element> = self
            .page_ref(t)
            .elements
            .iter()
            .filter(|e| self.selection.contains(&e.id()))
            .cloned()
            .map(|mut e| {
                e.translate(24.0, 24.0);
                e
            })
            .collect();
        self.selection = copies.iter().map(Element::id).collect();
        self.page_mut(t).elements.extend(copies);
    }

    pub fn select_all(&mut self) {
        self.tool = Tool::Lasso;
        self.selection = self.page_ref(self.target()).elements.iter().map(Element::id).collect();
    }

    /// Clicking a swatch with a selection recolors it.
    pub fn pick_color(&mut self, c: Color, highlighter: bool) {
        if highlighter {
            self.highlighter_color = c;
            if !matches!(self.tool, Tool::Highlighter) {
                self.tool = Tool::Highlighter;
            }
        } else {
            self.pen_color = c;
            if matches!(self.tool, Tool::Eraser | Tool::Highlighter) {
                self.tool = Tool::Pen;
            }
        }
        if !self.selection.is_empty() {
            let t = self.target();
            self.checkpoint(t);
            let sel = self.selection.clone();
            let mut new_sel = HashSet::new();
            for e in &mut self.page_mut(t).elements {
                if sel.contains(&e.id()) {
                    e.set_color(c);
                    new_sel.insert(e.id());
                }
            }
            self.selection = new_sel;
        }
        if let Some((t, id)) = self.editing_text
            && let Some(Element::Text(tb)) = self.page_mut(t).elements.iter_mut().find(|e| e.id() == id)
        {
            tb.color = c.with_alpha(255);
        }
    }

    pub fn ink_width(&self, kind: InkKind) -> f32 {
        match kind {
            InkKind::Pen => self.config.pen_width,
            InkKind::Highlighter => self.config.highlighter_width,
        }
    }

    // ----- text editing --------------------------------------------------------------

    pub fn begin_text_edit(&mut self, t: Target, id: u64) {
        self.finish_text_edit();
        let Some(Element::Text(tb)) = self.page_ref(t).elements.iter().find(|e| e.id() == id) else { return };
        self.text_buf = tb.text.clone();
        self.pending_snapshot = Some(self.snapshot_of(t));
        self.editing_text = Some((t, id));
        self.text_focus_pending = true;
    }

    /// Commit the text being edited; drop the box if it ended up empty.
    pub fn finish_text_edit(&mut self) {
        let Some((t, id)) = self.editing_text.take() else { return };
        let text = std::mem::take(&mut self.text_buf);
        let page = self.page_mut(t);
        let mut changed = false;
        if let Some(i) = page.find(id)
            && let Element::Text(tb) = &mut page.elements[i]
        {
            if text.trim().is_empty() {
                page.elements.remove(i);
                changed = true;
            } else if tb.text != text {
                tb.text = text;
                changed = true;
            }
        }
        if let Some(s) = self.pending_snapshot.take() {
            // A brand-new box that stayed empty leaves no trace in history.
            let was_new = matches!(&s, Snapshot::Elements(_, els) if !els.iter().any(|e| e.id() == id));
            if changed && !(was_new && self.page_ref(t).find(id).is_none()) {
                self.history.push(s);
                self.touch();
            }
        }
    }

    /// Write the edit buffer into the document without ending the edit (before saving).
    fn finish_text_edit_keep(&mut self) {
        if let Some((t, id)) = self.editing_text {
            let text = self.text_buf.clone();
            if let Some(Element::Text(tb)) = self.page_mut(t).elements.iter_mut().find(|e| e.id() == id) {
                tb.text = text;
            }
        }
    }

    // ----- overlay -------------------------------------------------------------------

    pub fn is_overlay(&self) -> bool {
        matches!(self.mode, Mode::Overlay(_))
    }

    pub fn enter_overlay(&mut self, ctx: &Context, passthrough: bool) {
        self.finish_text_edit();
        self.selection.clear();
        self.interaction = Interaction::Idle;
        if let Mode::Overlay(o) = &mut self.mode {
            o.passthrough = passthrough;
            return;
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        self.notebook_geometry = Some(NotebookGeometry { rect: self.hwnd.and_then(platform::window_rect), maximized });
        if maximized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        }
        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Title("Plexi — drawing on screen".into()));
        if matches!(self.tool, Tool::Lasso | Tool::Text | Tool::Shape(ShapeKind::Line)) {
            self.tool = Tool::Pen;
        }
        self.mode = Mode::Overlay(Overlay {
            passthrough,
            monitor: platform::cursor_monitor(),
            place_in: 2,
            hint_until: 0.0,
            snapshot: None,
        });
        self.set_passthrough(ctx, passthrough);
    }

    pub fn set_passthrough(&mut self, ctx: &Context, on: bool) {
        let now = ctx.input(|i| i.time);
        if let Mode::Overlay(o) = &mut self.mode {
            o.passthrough = on;
            if on {
                o.hint_until = now + 2.5;
                self.interaction = Interaction::Idle;
            } else {
                // Drawing again: follow the mouse to whichever monitor it is on.
                let m = platform::cursor_monitor();
                if m.is_some() && m != o.monitor {
                    o.monitor = m;
                }
                o.place_in = 1;
            }
            ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(on));
            if !on {
                ctx.send_viewport_cmd(ViewportCommand::Focus);
            }
        }
    }

    pub fn exit_overlay(&mut self, ctx: &Context) {
        if !self.is_overlay() {
            return;
        }
        self.interaction = Interaction::Idle;
        self.mode = Mode::Notebook;
        if !self.config.keep_overlay_ink && !self.doc.overlay.elements.is_empty() {
            self.checkpoint(Target::Overlay);
            self.doc.overlay.elements.clear();
        }
        ctx.send_viewport_cmd(ViewportCommand::MousePassthrough(false));
        if let Some(h) = self.hwnd {
            platform::show_in_taskbar(h, true);
        }
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::Normal));
        ctx.send_viewport_cmd(ViewportCommand::Decorations(true));
        let geom = self.notebook_geometry.take();
        match geom.and_then(|g| g.rect) {
            Some(r) => self.restore_rect = Some((r, 3)),
            None => ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(1040.0, 760.0))),
        }
        if geom.is_some_and(|g| g.maximized) {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(true));
        }
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.fit_pending = true;
    }

    pub fn clear_overlay(&mut self) {
        if !self.doc.overlay.elements.is_empty() {
            self.checkpoint(Target::Overlay);
            self.doc.overlay.elements.clear();
            self.selection.clear();
        }
    }

    pub fn request_snapshot(&mut self, action: SnapshotAction) {
        if let (Mode::Overlay(o), Some(h)) = (&mut self.mode, self.hwnd) {
            platform::exclude_from_capture(h, true);
            o.snapshot = Some((action, 3));
        }
    }

    /// Advance multi-frame overlay work: window placement and screenshots.
    fn drive_overlay(&mut self, ctx: &Context) {
        let Some(h) = self.hwnd else { return };
        let Mode::Overlay(o) = &mut self.mode else {
            if let Some((r, n)) = self.restore_rect.take() {
                if n == 0 {
                    platform::place_normal(h, r);
                    self.fit_pending = true;
                } else {
                    self.restore_rect = Some((r, n - 1));
                    ctx.request_repaint();
                }
            }
            return;
        };
        if o.place_in > 0 {
            o.place_in -= 1;
            if o.place_in == 0 {
                platform::show_in_taskbar(h, false);
            }
            ctx.request_repaint();
        } else if let Some(m) = o.monitor {
            // Keep exactly covering the monitor; DPI changes can resize us behind our back.
            if platform::window_rect(h) != Some(m) {
                platform::place_topmost(h, m, !o.passthrough);
                if !o.passthrough {
                    platform::focus(h);
                }
                ctx.request_repaint();
            }
        }
        let size = ctx.content_rect().size();
        let ov = &mut self.doc.overlay;
        let want = plexi_core::PageSize::Custom([size.x, size.y]);
        if ov.size != want {
            ov.size = want;
        }

        if let Some((action, frames)) = o.snapshot {
            if frames > 0 {
                o.snapshot = Some((action, frames - 1));
                ctx.request_repaint();
                return;
            }
            o.snapshot = None;
            let monitor = o.monitor.or_else(|| platform::window_monitor(h));
            let shot = monitor.and_then(platform::capture);
            platform::exclude_from_capture(h, false);
            match shot {
                Some(img) => self.finish_snapshot(ctx, action, img),
                None => self.toast_error("Couldn't capture the screen"),
            }
        }
    }

    fn finish_snapshot(&mut self, ctx: &Context, action: SnapshotAction, shot: image::RgbaImage) {
        let ppp = ctx.pixels_per_point();
        let (w, h) = (shot.width() as f32 / ppp, shot.height() as f32 / ppp);
        let mut png = std::io::Cursor::new(Vec::new());
        if shot.write_to(&mut png, image::ImageFormat::Png).is_err() {
            return;
        }
        let mut page = Page::new(format!("Screen {}", self.doc.pages.len() + 1), plexi_core::PageSize::Custom([w, h]));
        let asset = self.doc.add_asset(png.into_inner());
        page.elements.push(Element::Image(ImageEl { id: new_id(), asset, pos: [0.0, 0.0], size: [w, h] }));
        page.elements.extend(self.doc.overlay.elements.iter().cloned());
        match action {
            SnapshotAction::ToPage => {
                self.checkpoint_pages();
                self.doc.pages.push(page);
                self.page = self.doc.pages.len() - 1;
                self.fit_pending = true;
                self.toast("Screen saved as a new notebook page");
            }
            SnapshotAction::Copy | SnapshotAction::SaveImage => {
                let opts = RenderOptions { scale: ppp, background: false, text: true };
                let img = match plexi_core::export::raster::page_rgba(&page, &self.doc.assets, &opts) {
                    Ok(i) => i,
                    Err(e) => {
                        self.toast_error(format!("Snapshot failed: {e}"));
                        return;
                    }
                };
                // The screenshot asset was only needed for this render.
                self.doc.prune_assets();
                if action == SnapshotAction::Copy {
                    let ci = egui::ColorImage::from_rgba_unmultiplied(
                        [img.width() as usize, img.height() as usize],
                        img.as_raw(),
                    );
                    ctx.copy_image(ci);
                    self.toast("Screenshot with ink copied to the clipboard");
                } else if let Some(p) = rfd::FileDialog::new()
                    .add_filter("PNG image", &["png"])
                    .set_file_name("Plexi screen.png")
                    .save_file()
                {
                    match img.save(&p) {
                        Ok(()) => self.toast(format!("Saved {}", p.display())),
                        Err(e) => self.toast_error(format!("Save failed: {e}")),
                    }
                }
            }
        }
    }

    fn handle_shell_events(&mut self, ctx: &Context) {
        let events: Vec<ShellEvent> = match &self.shell {
            Some(s) => s.events.try_iter().collect(),
            None => Vec::new(),
        };
        for ev in events {
            match ev {
                ShellEvent::ToggleDraw => match &self.mode {
                    Mode::Notebook => self.enter_overlay(ctx, false),
                    Mode::Overlay(o) => {
                        let on = !o.passthrough;
                        self.set_passthrough(ctx, on);
                    }
                },
                ShellEvent::ClearOverlay => self.clear_overlay(),
                ShellEvent::ShowNotebook => {
                    if self.is_overlay() {
                        self.exit_overlay(ctx);
                    } else {
                        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                        ctx.send_viewport_cmd(ViewportCommand::Focus);
                    }
                }
                ShellEvent::Quit => {
                    self.quit_requested = true;
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                }
            }
        }
    }

    // ----- toasts --------------------------------------------------------------------

    pub fn toast(&mut self, text: impl Into<String>) {
        self.toast =
            Some(Toast { text: text.into(), until: Instant::now() + Duration::from_millis(2600), error: false });
    }

    pub fn toast_error(&mut self, text: impl Into<String>) {
        self.toast = Some(Toast { text: text.into(), until: Instant::now() + Duration::from_secs(6), error: true });
    }

    // ----- shortcuts -----------------------------------------------------------------

    fn shortcuts(&mut self, ctx: &Context) {
        let typing = self.editing_text.is_some() || ctx.egui_wants_keyboard_input();
        let cmd = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND, k));
        let cmd_shift = |k: Key| ctx.input_mut(|i| i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, k));
        let plain = |k: Key| !typing && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, k));

        if cmd(Key::K) {
            self.palette.open = !self.palette.open;
            self.palette.query.clear();
            self.palette.selected = 0;
        }
        if typing {
            if ctx.input(|i| i.key_pressed(Key::Escape)) && self.editing_text.is_some() {
                self.finish_text_edit();
            }
            return;
        }
        if cmd_shift(Key::Z) || cmd(Key::Y) {
            self.redo();
        } else if cmd(Key::Z) {
            self.undo();
        }
        if cmd_shift(Key::S) {
            self.save_as();
        } else if cmd(Key::S) {
            self.save();
        }
        if cmd(Key::O) {
            self.open_dialog();
        }
        if cmd(Key::N) {
            self.new_document();
        }
        if cmd(Key::E) {
            self.open_export_dialog();
        }
        if cmd(Key::A) {
            self.select_all();
        }
        if cmd(Key::D) {
            self.duplicate_selection();
        }
        if cmd(Key::Comma) {
            self.settings_open = !self.settings_open;
        }
        if plain(Key::P) {
            self.tool = Tool::Pen;
        }
        if plain(Key::H) {
            self.tool = Tool::Highlighter;
        }
        if plain(Key::E) {
            self.tool = Tool::Eraser;
        }
        if plain(Key::S) || plain(Key::V) {
            self.tool = Tool::Lasso;
        }
        if plain(Key::T) {
            self.tool = Tool::Text;
        }
        if plain(Key::L) {
            self.tool = Tool::Shape(ShapeKind::Line);
        }
        if plain(Key::A) {
            self.tool = Tool::Shape(ShapeKind::Arrow);
        }
        if plain(Key::R) {
            self.tool = Tool::Shape(ShapeKind::Rect);
        }
        if plain(Key::O) {
            self.tool = Tool::Shape(ShapeKind::Ellipse);
        }
        if plain(Key::Delete) || plain(Key::Backspace) {
            if self.selection.is_empty() && self.is_overlay() {
                self.clear_overlay();
            } else {
                self.delete_selection();
            }
        }
        let digits =
            [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        for (i, k) in digits.into_iter().enumerate() {
            if plain(k) {
                if self.tool == Tool::Highlighter {
                    if let Some(c) = self.config.highlighter_colors.get(i).copied() {
                        self.pick_color(c, true);
                    }
                } else if let Some(c) = self.config.pen_colors.get(i).copied() {
                    self.pick_color(c, false);
                }
            }
        }
        if plain(Key::OpenBracket) {
            self.nudge_width(1.0 / 1.25);
        }
        if plain(Key::CloseBracket) {
            self.nudge_width(1.25);
        }
        if plain(Key::F1) {
            self.help_open = !self.help_open;
        }

        match self.mode.clone() {
            Mode::Notebook => {
                if plain(Key::PageDown) {
                    self.go_to_page(self.page + 1);
                }
                if plain(Key::PageUp) {
                    self.go_to_page(self.page.saturating_sub(1));
                }
                if cmd(Key::Enter) {
                    self.add_page();
                }
                if cmd(Key::Num0) {
                    self.fit_pending = true;
                }
                if cmd(Key::Equals) || cmd(Key::Plus) {
                    self.zoom_by(ctx, 1.25);
                }
                if cmd(Key::Minus) {
                    self.zoom_by(ctx, 0.8);
                }
                if plain(Key::Escape) {
                    self.selection.clear();
                }
            }
            Mode::Overlay(o) => {
                if plain(Key::Escape) {
                    if !self.selection.is_empty() {
                        self.selection.clear();
                    } else if !o.passthrough {
                        self.set_passthrough(ctx, true);
                    }
                }
                if plain(Key::C) {
                    self.clear_overlay();
                }
                if cmd_shift(Key::C) {
                    self.request_snapshot(SnapshotAction::Copy);
                }
                if plain(Key::N) {
                    self.exit_overlay(ctx);
                }
            }
        }
    }

    pub fn nudge_width(&mut self, f: f32) {
        match self.tool {
            Tool::Highlighter => self.config.highlighter_width = (self.config.highlighter_width * f).clamp(4.0, 80.0),
            Tool::Eraser => self.config.eraser_size = (self.config.eraser_size * f).clamp(4.0, 120.0),
            _ => self.config.pen_width = (self.config.pen_width * f).clamp(0.5, 40.0),
        }
    }

    pub fn zoom_by(&mut self, ctx: &Context, f: f32) {
        let r = ctx.content_rect();
        self.view.zoom_around(r.min, r.center(), f);
    }

    pub fn open_export_dialog(&mut self) {
        self.finish_text_edit();
        let scope = if self.is_overlay() { ExportScope::ScreenInk } else { ExportScope::CurrentPage };
        self.export =
            Some(ExportDialog { format: ExportFormat::Pdf, scope, scale: 2.0, background: !self.is_overlay() });
    }

    fn update_title(&mut self, ctx: &Context) {
        if self.is_overlay() {
            return;
        }
        let title = format!("{} — Plexi", self.display_name());
        if title != self.last_title {
            ctx.send_viewport_cmd(ViewportCommand::Title(title.clone()));
            self.last_title = title;
        }
    }
}

impl eframe::App for PlexiApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // The window is created transparent; the notebook paints its own opaque
        // background, the overlay paints only ink.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        if self.hwnd.is_none() {
            self.hwnd = platform::native_handle(frame);
        }
        if let Some(start) = self.start.take()
            && (start.overlay || start.background)
        {
            self.enter_overlay(&ctx, start.background);
        }
        self.poll_jobs();
        self.handle_shell_events(&ctx);
        self.drive_overlay(&ctx);
        self.handle_drops_and_paste(&ctx);

        let rect = ui.max_rect();
        match self.mode.clone() {
            Mode::Notebook => crate::ui::notebook(self, ui, rect),
            Mode::Overlay(o) => crate::ui::overlay(self, ui, rect, &o),
        }
        crate::ui::dialogs(self, &ctx);
        self.shortcuts(&ctx);
        self.autosave();
        self.update_title(&ctx);
        self.cache.end_frame();
        if self.dirty && self.config.autosave {
            // No input means no frames; make sure the autosave timer still fires.
            ctx.request_repaint_after(Duration::from_millis(1600));
        }
        if self.toast.is_some() || self.saving {
            ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.finish_text_edit();
        // Wait for a background save to land before writing again.
        let deadline = Instant::now() + Duration::from_secs(3);
        while self.saving && Instant::now() < deadline {
            self.poll_jobs();
            std::thread::sleep(Duration::from_millis(20));
        }
        self.flush_save();
        if self.path.is_some() {
            self.config.last_file = self.path.clone();
        }
        self.config.save();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, pos2};

    struct Harness {
        ctx: Context,
        app: PlexiApp,
        frame: eframe::Frame,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!("plexi-ui-test-{}-{}", std::process::id(), new_id()));
            // SAFETY: tests that touch the environment set the same variable to a fresh dir.
            unsafe { std::env::set_var("PLEXI_DATA_DIR", &dir) };
            let ctx = Context::default();
            let cc = eframe::CreationContext::_new_kittest(ctx.clone());
            let app = PlexiApp::new(&cc, StartOptions { file: None, overlay: false, background: false, shell: false });
            let mut h = Self { ctx, app, frame: eframe::Frame::_new_kittest(), time: 0.0 };
            h.step(vec![]);
            h
        }

        fn step(&mut self, events: Vec<Event>) {
            let raw = RawInput {
                screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, egui::vec2(1000.0, 800.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let (app, frame) = (&mut self.app, &mut self.frame);
            let mut out = self.ctx.run_ui(raw, |ui| eframe::App::ui(app, ui, frame));
            out.textures_delta.clear();
            self.time += 1.0 / 60.0;
        }

        fn drag(&mut self, button: PointerButton, pts: &[(f32, f32)]) {
            let p = |(x, y): (f32, f32)| pos2(x, y);
            let m = Default::default();
            self.step(vec![Event::PointerMoved(p(pts[0]))]);
            self.step(vec![Event::PointerButton { pos: p(pts[0]), button, pressed: true, modifiers: m }]);
            for chunk in pts[1..].chunks(3) {
                self.step(chunk.iter().map(|q| Event::PointerMoved(p(*q))).collect());
            }
            let last = p(*pts.last().unwrap());
            self.step(vec![Event::PointerButton { pos: last, button, pressed: false, modifiers: m }]);
            self.step(vec![]);
        }

        fn click(&mut self, x: f32, y: f32) {
            self.drag(PointerButton::Primary, &[(x, y)]);
        }

        fn elements(&self) -> &[Element] {
            &self.app.doc.pages[self.app.page].elements
        }
    }

    fn line(x0: f32, x1: f32, y: f32) -> Vec<(f32, f32)> {
        (0..=20).map(|i| (x0 + (x1 - x0) * i as f32 / 20.0, y)).collect()
    }

    #[test]
    fn pen_eraser_and_undo() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(200.0, 600.0, 300.0));
        assert_eq!(h.elements().len(), 1);
        let Element::Stroke(s) = &h.elements()[0] else { panic!("expected a stroke") };
        assert!(s.points.len() >= 2);
        // Right-drag erases with any tool.
        h.drag(PointerButton::Secondary, &[(400.0, 250.0), (400.0, 300.0), (400.0, 350.0)]);
        assert!(h.elements().is_empty());
        h.app.undo();
        assert_eq!(h.elements().len(), 1);
        h.app.redo();
        assert!(h.elements().is_empty());
    }

    #[test]
    fn typing_a_text_box() {
        let mut h = Harness::new();
        h.app.tool = Tool::Text;
        h.click(300.0, 400.0);
        assert!(h.app.editing_text.is_some(), "clicking with the text tool starts editing");
        h.step(vec![Event::Text("hello".into())]);
        h.step(vec![]);
        h.click(700.0, 650.0); // click away ends the edit
        h.step(vec![]);
        let texts: Vec<&str> = h
            .elements()
            .iter()
            .filter_map(|e| match e {
                Element::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["hello"]);
        h.app.undo();
        assert!(h.elements().is_empty(), "one undo removes the typed box");
    }

    #[test]
    fn lasso_select_move_delete() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        let before = h.elements()[0].bounds();
        h.app.tool = Tool::Lasso;
        h.drag(
            PointerButton::Primary,
            &[(250.0, 250.0), (550.0, 250.0), (550.0, 350.0), (250.0, 350.0), (250.0, 260.0)],
        );
        assert_eq!(h.app.selection.len(), 1);
        h.drag(PointerButton::Primary, &[(400.0, 300.0), (420.0, 320.0), (450.0, 350.0)]);
        let after = h.elements()[0].bounds();
        assert!(after.min[0] > before.min[0] + 10.0, "moved right: {before:?} -> {after:?}");
        assert_eq!(h.app.selection.len(), 1, "selection follows the moved element");
        h.app.delete_selection();
        assert!(h.elements().is_empty());
    }

    #[test]
    fn shapes_and_pages() {
        let mut h = Harness::new();
        h.app.tool = Tool::Shape(ShapeKind::Rect);
        h.drag(PointerButton::Primary, &[(200.0, 300.0), (300.0, 380.0), (400.0, 450.0)]);
        assert!(matches!(h.elements()[0], Element::Shape(Shape { kind: ShapeKind::Rect, .. })));
        h.app.add_page();
        assert_eq!(h.app.page, 1);
        assert!(h.elements().is_empty());
        h.app.undo();
        assert_eq!(h.app.doc.pages.len(), 1);
    }

    #[test]
    fn overlay_ink_is_separate_and_kept() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.enter_overlay(&ctx, false);
        h.step(vec![]);
        assert!(h.app.is_overlay());
        h.drag(PointerButton::Primary, &line(200.0, 600.0, 500.0));
        assert_eq!(h.app.doc.overlay.elements.len(), 1, "ink lands on the screen layer");
        assert!(h.app.doc.pages[0].elements.is_empty());
        h.app.set_passthrough(&ctx, true);
        h.step(vec![]);
        h.app.exit_overlay(&ctx);
        h.step(vec![]);
        assert!(!h.app.is_overlay());
        assert_eq!(h.app.doc.overlay.elements.len(), 1, "screen ink survives a trip to the notebook");
        h.app.enter_overlay(&ctx, false);
        h.app.clear_overlay();
        assert!(h.app.doc.overlay.elements.is_empty());
        h.app.undo();
        assert_eq!(h.app.doc.overlay.elements.len(), 1);
    }

    #[test]
    fn saving_round_trips() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(200.0, 600.0, 300.0));
        let path = crate::config::data_dir().join("saved.plexi");
        h.app.path = Some(path.clone());
        h.app.save();
        for _ in 0..200 {
            h.step(vec![]);
            if !h.app.saving {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let back = format::load(&path).unwrap();
        assert_eq!(back.pages[0].elements.len(), 1);
    }
}
