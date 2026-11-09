//! Application state and the frame loop: file handling, mode switching between the
//! notebook window and the on-screen overlay, commands and shortcuts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use egui::{Context, Key, Modifiers, ViewportCommand};
#[cfg(test)]
use plexi_core::Shape;
use plexi_core::export::layout::{self, Area, Layout};
use plexi_core::export::{ExportFormat, RenderOptions};
use plexi_core::{Color, Document, Element, ImageEl, InkKind, Page, ShapeKind, TextBox, format, new_id};

use crate::canvas::{AssetTexture, CropState, Interaction, MeshCache, View};
use crate::config::{self, Config};
use crate::fonts::FontBook;
use crate::handwriting::HandwritingState;
use crate::history::{History, Snapshot, Target};
use crate::platform::{self, PxRect};
use crate::shell::{Shell, ShellEvent};
use crate::updater::{self, InstallKind, Release};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Pen,
    Highlighter,
    Eraser,
    Lasso,
    RectSelect,
    Snapshot,
    Hand,
    Text,
    Shape(ShapeKind),
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pen => "Pen (P)",
            Tool::Highlighter => "Highlighter (H)",
            Tool::Eraser => "Eraser (E) — middle-drag erases with any tool",
            Tool::Lasso => "Select (S) — lasso, drag to move, Del to delete",
            Tool::RectSelect => "Rectangle select (M) — drag a box to select, Del to delete",
            Tool::Snapshot => "Snapshot — drag a box to copy that part of the page as a picture",
            Tool::Hand => "Hand (G) — drag to pan, or to grab and move an element",
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
    /// The area picked with [`Snip`]: onto the clipboard, and into the notebook when the
    /// snip started there.
    Snip,
}

/// Where to go once a snip is done (or cancelled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnipFrom {
    Notebook,
    ClickThrough,
    Drawing,
}

/// Picking an area of the screen to snip.
#[derive(Clone, Copy, Debug)]
pub struct Snip {
    pub from: SnipFrom,
    /// Where the drag started, once it has.
    pub start: Option<egui::Pos2>,
    pub end: egui::Pos2,
    /// The finished area, waiting for the screenshot.
    pub rect: Option<egui::Rect>,
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
    pub snip: Option<Snip>,
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
    /// Outer position and inner size in points, for platforms without `rect`.
    outer: Option<(egui::Pos2, egui::Vec2)>,
    maximized: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportScope {
    CurrentPage,
    AllPages,
    ScreenInk,
}

/// What sheet an export is laid out on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperChoice {
    /// Each output is exactly the page (or the drawing) size.
    Fit,
    /// One of `layout::PAPERS`.
    Preset(usize),
    Custom,
}

pub struct ExportDialog {
    pub format: ExportFormat,
    pub scope: ExportScope,
    pub scale: f32,
    pub background: bool,
    pub area: Area,
    pub paper: PaperChoice,
    pub landscape: bool,
    /// Custom sheet size in inches.
    pub custom_in: [f32; 2],
    /// Split across sheets at full size instead of shrinking onto one.
    pub tile: bool,
    /// Margin in inches.
    pub margin_in: f32,
    /// Export only the output pages listed in `page_list` (e.g. "1, 3, 5-7").
    pub pick_pages: bool,
    pub page_list: String,
    pub preview: crate::ui::Preview,
}

impl ExportDialog {
    pub fn sheet(&self) -> Option<[f32; 2]> {
        let [w, h] = match self.paper {
            PaperChoice::Fit => return None,
            PaperChoice::Preset(i) => layout::PAPERS[i.min(layout::PAPERS.len() - 1)].1,
            PaperChoice::Custom => [self.custom_in[0] * 96.0, self.custom_in[1] * 96.0],
        };
        let landscape = self.landscape && self.paper != PaperChoice::Custom;
        Some(if landscape { [h, w] } else { [w, h] })
    }

    pub fn layout(&self) -> Layout {
        Layout { area: self.area, sheet: self.sheet(), tile: self.tile, margin: self.margin_in * 96.0 }
    }

    pub fn render_options(&self) -> RenderOptions {
        RenderOptions { scale: self.scale, background: self.background, text: true, layout: self.layout(), pick: None }
    }

    /// The output pages picked, out of `total` (`None` for all of them).
    pub fn pick(&self, total: usize) -> Result<Option<Vec<usize>>, String> {
        if !self.pick_pages {
            return Ok(None);
        }
        layout::parse_page_list(&self.page_list, total).map(Some)
    }
}

/// The right-click menu on the canvas.
#[derive(Clone, Debug)]
pub struct ContextMenu {
    /// Screen position it opened at.
    pub pos: egui::Pos2,
    pub target: Target,
    /// Element under the pointer, if any (locked images included).
    pub hit: Option<u64>,
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
    Printed(Result<PathBuf, String>),
    UpdateChecked { manual: bool, result: Result<Option<Release>, String> },
    UpdateProgress(f32),
    UpdateDownloaded(Result<PathBuf, String>),
}

/// Where the update check and install stand.
#[derive(Clone, Debug, Default)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    /// A newer release; shown as a notice until acted on or dismissed.
    Available(Release),
    Downloading(Release, f32),
    Failed(Release, String),
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
    /// The last copied selection (Ctrl+C), pasted back with Ctrl+V. Elements keep their
    /// ids until paste gives the copies fresh ones, so repeated pastes don't collide.
    pub clip: Option<Vec<Element>>,
    pub editing_text: Option<(Target, u64)>,
    pub text_buf: String,
    pub text_focus_pending: bool,
    pub pen_pressure: Option<f32>,
    pub last_sample_time: f64,
    pub cache: MeshCache,
    pub textures: HashMap<String, AssetTexture>,
    pub fonts: FontBook,
    /// The text box being edited got a new font or style (a change even if the words
    /// stay the same).
    pub edit_style_changed: bool,
    pub crop: Option<CropState>,
    /// A snip waiting to go onto the page once the notebook window is back, with the
    /// frames left to wait.
    pending_image: Option<(Vec<u8>, u8)>,
    pub config: Config,
    pub palette: PaletteState,
    pub export: Option<ExportDialog>,
    pub settings_open: bool,
    pub help_open: bool,
    pub handwriting: HandwritingState,
    pub toast: Option<Toast>,
    pub context_menu: Option<ContextMenu>,
    pub update: UpdateState,
    /// The last scroll came from a trackpad (pans) rather than a mouse wheel (zooms).
    pub trackpad_scroll: bool,
    /// Top-left of the canvas on screen, as of the last frame.
    pub canvas_origin: egui::Pos2,
    /// The guide drawn around the text box being typed in last frame, by box id.
    pub edit_guide: Option<(u64, egui::Rect)>,
    /// An area the snapshot tool picked, to copy to the clipboard this frame.
    pub snapshot_request: Option<(Target, plexi_core::Rect)>,
    /// Bumped on every document change; caches keyed on it (sheet guides, export
    /// preview) know when to rebuild.
    pub revision: u64,
    pub sheet_cache: Option<(u64, usize, [f32; 2], Vec<plexi_core::Rect>)>,
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
    /// An update is installing; close the window on the next frame.
    update_quit: bool,
}

impl PlexiApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: StartOptions) -> Self {
        crate::ui::install_style(&cc.egui_ctx);
        let config = Config::load();
        FontBook::warm_up();
        let handwriting = HandwritingState::default();
        handwriting.install_saved_font();
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
            clip: None,
            editing_text: None,
            text_buf: String::new(),
            text_focus_pending: false,
            pen_pressure: None,
            last_sample_time: 0.0,
            cache: MeshCache::default(),
            textures: HashMap::new(),
            fonts: FontBook::default(),
            edit_style_changed: false,
            crop: None,
            pending_image: None,
            config,
            palette: PaletteState::default(),
            export: None,
            settings_open: false,
            help_open: false,
            handwriting,
            toast: None,
            context_menu: None,
            update: UpdateState::Idle,
            trackpad_scroll: false,
            canvas_origin: egui::Pos2::ZERO,
            edit_guide: None,
            snapshot_request: None,
            revision: 0,
            sheet_cache: None,
            shell: None,
            hwnd: None,
            notebook_geometry: None,
            restore_rect: None,
            jobs_tx,
            jobs_rx,
            last_title: String::new(),
            start: None,
            quit_requested: false,
            update_quit: false,
        };
        if start.shell {
            updater::clean_up_after_update();
            if app.config.check_updates {
                app.check_for_updates(&cc.egui_ctx, false);
            }
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
        self.revision += 1;
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
        self.crop = None;
        if let Some(s) = self.history.pop_undo() {
            let back = self.restore(s);
            self.history.push_redo(back);
            self.selection.clear();
            self.touch();
        }
    }

    pub fn redo(&mut self) {
        self.finish_text_edit();
        self.crop = None;
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
        self.crop = None;
        self.interaction = Interaction::Idle;
        self.page = 0;
        self.fit_pending = true;
        self.textures.clear();
        self.dirty = false;
        self.context_menu = None;
        self.revision += 1;
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
        page.paper = self.config.new_page_paper;
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
                Job::Printed(Ok(path)) => {
                    open_url(&path.to_string_lossy());
                    self.toast("Opened in your PDF viewer — print from there (Ctrl+P)");
                }
                Job::Printed(Err(e)) => self.toast_error(format!("Couldn't make the PDF to print: {e}")),
                Job::UpdateChecked { manual, result } => {
                    self.update = UpdateState::Idle;
                    match result {
                        Ok(Some(r)) if manual || self.config.skipped_version.as_deref() != Some(&r.version) => {
                            self.update = UpdateState::Available(r);
                        }
                        Ok(_) if manual => {
                            self.toast(format!("Plexi {} is the latest version", env!("CARGO_PKG_VERSION")))
                        }
                        Err(e) if manual => self.toast_error(format!("Couldn't check for updates: {e}")),
                        _ => {} // quiet on startup: offline is fine
                    }
                }
                Job::UpdateProgress(f) => {
                    if let UpdateState::Downloading(_, p) = &mut self.update {
                        *p = f;
                    }
                }
                Job::UpdateDownloaded(result) => {
                    let UpdateState::Downloading(release, _) = std::mem::take(&mut self.update) else { continue };
                    let applied = result.and_then(|path| {
                        // Everything is on disk before the installer or new exe takes over.
                        self.finish_text_edit();
                        self.flush_save();
                        if self.path.is_some() {
                            self.config.last_file = self.path.clone();
                        }
                        self.config.save();
                        updater::apply(updater::install_kind(), &path)
                    });
                    match applied {
                        Ok(()) => {
                            self.quit_requested = true;
                            self.update_quit = true;
                        }
                        Err(e) => self.update = UpdateState::Failed(release, e),
                    }
                }
            }
        }
    }

    // ----- updates ---------------------------------------------------------------

    /// Ask GitHub for a newer release in the background. A startup check (`manual` off)
    /// waits a moment so it never competes with opening the window, and stays quiet
    /// when offline or up to date.
    pub fn check_for_updates(&mut self, ctx: &Context, manual: bool) {
        if matches!(self.update, UpdateState::Checking | UpdateState::Downloading(..)) {
            return;
        }
        self.update = UpdateState::Checking;
        let (tx, ctx) = (self.jobs_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            if !manual {
                std::thread::sleep(Duration::from_secs(3));
            }
            let _ = tx.send(Job::UpdateChecked { manual, result: updater::check() });
            ctx.request_repaint();
        });
    }

    /// Download and install the offered release, or open its page when this copy can't
    /// update itself (MSI installs, macOS).
    pub fn install_update(&mut self, ctx: &Context) {
        let release = match &self.update {
            UpdateState::Available(r) | UpdateState::Failed(r, _) => r.clone(),
            _ => return,
        };
        let kind = updater::install_kind();
        if kind == InstallKind::Manual {
            open_url(&release.page);
            self.update = UpdateState::Idle;
            return;
        }
        self.update = UpdateState::Downloading(release.clone(), 0.0);
        let (tx, ctx) = (self.jobs_tx.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut last = 0.0;
            let result = updater::download(&release, kind, |f| {
                if f - last >= 0.01 {
                    last = f;
                    let _ = tx.send(Job::UpdateProgress(f));
                    ctx.request_repaint();
                }
            });
            let _ = tx.send(Job::UpdateDownloaded(result));
            ctx.request_repaint();
        });
    }

    pub fn skip_update(&mut self) {
        if let UpdateState::Available(r) = &self.update {
            self.config.skipped_version = Some(r.version.clone());
            self.config.save();
        }
        self.update = UpdateState::Idle;
    }

    // ----- export ----------------------------------------------------------------

    /// The pages an export of `scope` covers, as indices (`None` is the screen ink).
    pub fn export_pages(&self, scope: ExportScope) -> Vec<Option<usize>> {
        match scope {
            ExportScope::CurrentPage => vec![Some(self.page)],
            ExportScope::AllPages => (0..self.doc.pages.len()).map(Some).collect(),
            ExportScope::ScreenInk => vec![None],
        }
    }

    /// The export dialog's options, with the picked pages; `None` (after saying why) if
    /// the page list doesn't parse.
    fn export_options(&mut self) -> Option<RenderOptions> {
        let dlg = self.export.as_ref()?;
        let mut opts = dlg.render_options();
        match dlg.pick(dlg.preview.total()) {
            Ok(pick) => opts.pick = pick,
            Err(e) => {
                self.toast_error(format!("Pages: {e}"));
                return None;
            }
        }
        Some(opts)
    }

    /// The pages an export covers, for the worker thread.
    fn export_page_list(doc: &Document, scope: ExportScope, page: usize) -> Vec<&Page> {
        match scope {
            ExportScope::CurrentPage => vec![&doc.pages[page]],
            ExportScope::AllPages => doc.pages.iter().collect(),
            ExportScope::ScreenInk => vec![&doc.overlay],
        }
    }

    /// Write the export dialog's pages to a PDF in the temp folder and open it, to print
    /// from the system's PDF viewer.
    pub fn run_print(&mut self) {
        let Some(scope) = self.export.as_ref().map(|d| d.scope) else { return };
        let Some(mut opts) = self.export_options() else { return };
        opts.scale = 1.0;
        self.export = None;
        self.finish_text_edit_keep();
        let stem: String =
            self.display_name().chars().filter(|c| c.is_alphanumeric() || *c == ' ' || *c == '-').collect();
        let path = std::env::temp_dir().join(format!("Plexi print - {}.pdf", stem.trim()));
        let doc = self.doc.clone();
        let page = self.page;
        let tx = self.jobs_tx.clone();
        self.toast("Preparing to print…");
        std::thread::spawn(move || {
            let pages = Self::export_page_list(&doc, scope, page);
            let r = plexi_core::export::export(&doc, &pages, ExportFormat::Pdf, &opts, &path)
                .map(|_| path)
                .map_err(|e| e.to_string());
            let _ = tx.send(Job::Printed(r));
        });
    }

    pub fn run_export(&mut self) {
        let Some(dlg) = &self.export else { return };
        let (format, scope) = (dlg.format, dlg.scope);
        let Some(opts) = self.export_options() else { return };
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
            let pages = Self::export_page_list(&doc, scope, page);
            let r = plexi_core::export::export(&doc, &pages, format, &opts, &path).map_err(|e| e.to_string());
            let _ = tx.send(Job::Exported(r));
        });
    }

    // ----- pages -----------------------------------------------------------------

    pub fn add_page(&mut self) {
        self.finish_text_edit();
        self.checkpoint_pages();
        let mut p = Page::new(format!("Page {}", self.doc.pages.len() + 1), self.config.new_page_size);
        let cur = &self.doc.pages[self.page];
        (p.background, p.size, p.paper) = (cur.background, cur.size, cur.paper);
        self.doc.pages.insert(self.page + 1, p);
        self.go_to_page(self.page + 1);
    }

    pub fn delete_page(&mut self) {
        self.delete_page_at(self.page);
    }

    /// Delete page `i` (or clear it when it is the only one). Undo brings it back.
    pub fn delete_page_at(&mut self, i: usize) {
        if i >= self.doc.pages.len() {
            return;
        }
        self.finish_text_edit();
        self.crop = None;
        self.checkpoint_pages();
        let name = self.doc.pages[i].name.clone();
        if self.doc.pages.len() == 1 {
            self.doc.pages[0].elements.clear();
            self.toast("Page cleared — Ctrl+Z to undo");
        } else {
            self.doc.pages.remove(i);
            if i < self.page || self.page >= self.doc.pages.len() {
                self.page = self.page.saturating_sub(1);
            }
            self.toast(format!("Deleted “{name}” — Ctrl+Z to undo"));
        }
        self.selection.clear();
        self.interaction = Interaction::Idle;
        self.fit_pending = true;
    }

    pub fn go_to_page(&mut self, i: usize) {
        self.finish_text_edit();
        self.finish_crop(true);
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
        // PNG, JPEG and GIF are kept as-is (GIF keeps its original bytes so its other
        // frames survive save/load and can still be played back); anything else is
        // converted to PNG.
        let stored = if bytes.starts_with(&[0x89, b'P', b'N', b'G'])
            || bytes.starts_with(&[0xff, 0xd8])
            || bytes.starts_with(b"GIF8")
        {
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
        let img = ImageEl::new(asset, pos, size);
        let id = img.id;
        self.page_mut(t).elements.push(Element::Image(img));
        self.selection = [id].into();
        self.tool = Tool::Lasso;
    }

    pub fn insert_text(&mut self, text: String, ctx: &Context) {
        let c = self.view_center(ctx);
        let t = self.target();
        self.checkpoint(t);
        let mut tb = self.text_template(c);
        tb.text = text;
        self.page_mut(t).elements.push(Element::Text(tb));
    }

    /// An empty text box at `pos` in the style new text gets.
    pub fn text_template(&self, pos: [f32; 2]) -> TextBox {
        let c = &self.config;
        let mut tb = TextBox::new(pos, "", c.text_size, self.pen_color_for_text());
        tb.font = c.text_font.clone();
        (tb.bold, tb.italic, tb.underline) = (c.text_bold, c.text_italic, c.text_underline);
        tb
    }

    /// The text the style controls show and change: the box being edited, else the
    /// selected text boxes, else the style for new ones.
    pub fn text_style(&self) -> TextBox {
        let editing = self.editing_text.and_then(|(t, id)| self.page_ref(t).elements.iter().find(|e| e.id() == id));
        let selected = self
            .page_ref(self.target())
            .elements
            .iter()
            .find(|e| self.selection.contains(&e.id()) && matches!(e, Element::Text(_)));
        match editing.or(selected) {
            Some(Element::Text(tb)) => tb.clone(),
            _ => self.text_template([0.0, 0.0]),
        }
    }

    /// Whether any text box is selected.
    pub fn text_selected(&self) -> bool {
        self.page_ref(self.target())
            .elements
            .iter()
            .any(|e| self.selection.contains(&e.id()) && matches!(e, Element::Text(_)))
    }

    /// Change the font or style of the text being edited or selected, and of new text.
    pub fn apply_text_style(&mut self, f: impl Fn(&mut TextBox)) {
        let mut style = self.text_template([0.0, 0.0]);
        f(&mut style);
        let c = &mut self.config;
        c.text_font = style.font.clone();
        (c.text_size, c.text_bold, c.text_italic, c.text_underline) =
            (style.size, style.bold, style.italic, style.underline);
        if let Some((t, id)) = self.editing_text {
            if let Some(Element::Text(tb)) = self.page_mut(t).elements.iter_mut().find(|e| e.id() == id) {
                f(tb);
                self.edit_style_changed = true;
            }
            return;
        }
        if self.text_selected() {
            let t = self.target();
            self.checkpoint(t);
            let sel = std::mem::take(&mut self.selection);
            let mut new_sel = HashSet::new();
            for e in &mut self.page_mut(t).elements {
                if sel.contains(&e.id()) {
                    if let Element::Text(tb) = e {
                        f(tb);
                        e.renew_id();
                    }
                    new_sel.insert(e.id());
                }
            }
            self.selection = new_sel;
        }
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

    /// Copy the selection (works the same for the lasso and the rectangle select tool)
    /// onto Plexi's own clipboard, ready for [`PlexiApp::paste_selection`]. Also puts a
    /// PNG of the selection on the system clipboard, so it can be pasted into other
    /// apps too — best-effort, and never at the cost of the internal copy.
    pub fn copy_selection(&mut self) {
        if self.selection.is_empty() {
            return;
        }
        let t = self.target();
        let elements: Vec<Element> =
            self.page_ref(t).elements.iter().filter(|e| self.selection.contains(&e.id())).cloned().collect();
        if elements.is_empty() {
            return;
        }
        let n = elements.len();
        self.clip = Some(elements);
        self.toast(if n == 1 { "Copied".to_owned() } else { format!("Copied {n} elements") });
    }

    /// The snapshot tool: copy `area` of target `t` to the system clipboard as a
    /// picture, at the size it shows on screen (never below the screen's own pixels).
    pub fn copy_area(&mut self, ctx: &Context, t: Target, area: plexi_core::Rect) {
        let ppp = ctx.pixels_per_point();
        let scale = (ppp * self.view_for(t).zoom).max(ppp);
        let paper = matches!(t, Target::Page(_));
        let opts = RenderOptions { scale, background: paper, ..Default::default() };
        match plexi_core::export::raster::area_rgba(self.page_ref(t), &self.doc.assets, &opts, area) {
            Ok(img) => {
                let size = [img.width() as usize, img.height() as usize];
                ctx.copy_image(egui::ColorImage::from_rgba_unmultiplied(size, img.as_raw()));
                self.toast("Snapshot copied");
            }
            Err(e) => self.toast_error(format!("Couldn't take the snapshot: {e}")),
        }
    }

    /// Paste the last copied selection back into the current target, offset so it lands
    /// centered on the visible canvas, and select the pasted copies.
    pub fn paste_selection(&mut self, ctx: &Context) {
        let Some(clip) = self.clip.clone() else { return };
        let t = self.target();
        self.checkpoint(t);
        let bounds = clip.iter().map(Element::bounds).reduce(|a, b| a.union(b));
        let offset = match bounds {
            Some(b) => {
                let c = self.view_center(ctx);
                [c[0] - b.center()[0], c[1] - b.center()[1]]
            }
            None => [24.0, 24.0],
        };
        let mut pasted = clip;
        let mut new_sel = HashSet::new();
        for e in &mut pasted {
            e.translate(offset[0], offset[1]);
            e.renew_id();
            new_sel.insert(e.id());
        }
        self.page_mut(t).elements.extend(pasted);
        self.selection = new_sel;
    }

    pub fn select_all(&mut self) {
        self.tool = Tool::Lasso;
        self.selection =
            self.page_ref(self.target()).elements.iter().filter(|e| !e.locked()).map(Element::id).collect();
    }

    /// Lock or unlock images. Locked images stay put: no selecting, moving or resizing.
    pub fn set_locked(&mut self, t: Target, ids: &HashSet<u64>, locked: bool) {
        self.checkpoint(t);
        for e in &mut self.page_mut(t).elements {
            if let Element::Image(img) = e
                && ids.contains(&img.id)
            {
                img.locked = locked;
            }
        }
        if locked {
            self.selection.retain(|id| !ids.contains(id));
        }
    }

    /// Move the selection to the top (`front`) or bottom of the stacking order.
    pub fn restack_selection(&mut self, front: bool) {
        if self.selection.is_empty() {
            return;
        }
        let t = self.target();
        self.checkpoint(t);
        let sel = self.selection.clone();
        let els = std::mem::take(&mut self.page_mut(t).elements);
        let (picked, rest): (Vec<Element>, Vec<Element>) = els.into_iter().partition(|e| sel.contains(&e.id()));
        self.page_mut(t).elements = if front { [rest, picked].concat() } else { [picked, rest].concat() };
    }

    /// Give image `id` its natural size (one image pixel per screen pixel), keeping its center.
    pub fn reset_image_size(&mut self, t: Target, id: u64, ppp: f32) {
        let Some(Element::Image(img)) = self.page_ref(t).elements.iter().find(|e| e.id() == id) else { return };
        let Some(bytes) = self.doc.assets.get(&img.asset) else { return };
        let dims = image::ImageReader::new(std::io::Cursor::new(bytes.as_slice()))
            .with_guessed_format()
            .ok()
            .and_then(|r| r.into_dimensions().ok());
        let Some((w, h)) = dims else { return };
        self.checkpoint(t);
        let page = self.page_mut(t);
        if let Some(i) = page.find(id)
            && let Element::Image(img) = &mut page.elements[i]
        {
            let c = [img.pos[0] + img.size[0] / 2.0, img.pos[1] + img.size[1] / 2.0];
            let [l, top, r, b] = img.crop_or_full();
            img.size = [w as f32 / ppp * (r - l), h as f32 / ppp * (b - top)];
            img.pos = [c[0] - img.size[0] / 2.0, c[1] - img.size[1] / 2.0];
            page.elements[i].renew_id();
            let new = page.elements[i].id();
            if self.selection.remove(&id) {
                self.selection.insert(new);
            }
        }
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
        self.edit_style_changed = false;
        self.pending_snapshot = Some(self.snapshot_of(t));
        self.editing_text = Some((t, id));
        self.text_focus_pending = true;
    }

    /// Commit the text being edited; drop the box if it ended up empty.
    pub fn finish_text_edit(&mut self) {
        let Some((t, id)) = self.editing_text.take() else { return };
        let text = std::mem::take(&mut self.text_buf);
        let mut changed = std::mem::take(&mut self.edit_style_changed);
        let page = self.page_mut(t);
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
        self.finish_crop(true);
        self.selection.clear();
        self.interaction = Interaction::Idle;
        if let Mode::Overlay(o) = &mut self.mode {
            o.passthrough = passthrough;
            return;
        }
        let maximized = ctx.input(|i| i.viewport().maximized.unwrap_or(false));
        let outer = ctx.input(|i| {
            let v = i.viewport();
            v.outer_rect.zip(v.inner_rect).map(|(o, inner)| (o.min, inner.size()))
        });
        self.notebook_geometry =
            Some(NotebookGeometry { rect: self.hwnd.and_then(platform::window_rect), outer, maximized });
        if maximized {
            ctx.send_viewport_cmd(ViewportCommand::Maximized(false));
        }
        ctx.send_viewport_cmd(ViewportCommand::Decorations(false));
        ctx.send_viewport_cmd(ViewportCommand::WindowLevel(egui::WindowLevel::AlwaysOnTop));
        ctx.send_viewport_cmd(ViewportCommand::Title("Plexi — drawing on screen".into()));
        if matches!(
            self.tool,
            Tool::Lasso | Tool::RectSelect | Tool::Snapshot | Tool::Hand | Tool::Text | Tool::Shape(ShapeKind::Line)
        ) {
            self.tool = Tool::Pen;
        }
        self.mode = Mode::Overlay(Overlay {
            passthrough,
            monitor: platform::cursor_monitor(),
            place_in: 2,
            hint_until: 0.0,
            snapshot: None,
            snip: None,
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
        self.crop = None;
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
        match (geom.and_then(|g| g.rect), geom.and_then(|g| g.outer)) {
            (Some(r), _) => self.restore_rect = Some((r, 3)),
            (None, Some((pos, size))) => {
                ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                ctx.send_viewport_cmd(ViewportCommand::OuterPosition(pos));
            }
            (None, None) => ctx.send_viewport_cmd(ViewportCommand::InnerSize(egui::vec2(1040.0, 760.0))),
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

    /// Pick an area of the screen to snip: copied to the clipboard, and put on the page
    /// when this starts from the notebook.
    pub fn start_snip(&mut self, ctx: &Context) {
        let from = match &self.mode {
            Mode::Notebook => SnipFrom::Notebook,
            Mode::Overlay(o) if o.passthrough => SnipFrom::ClickThrough,
            Mode::Overlay(_) => SnipFrom::Drawing,
        };
        match from {
            SnipFrom::Notebook => self.enter_overlay(ctx, false),
            SnipFrom::ClickThrough => self.set_passthrough(ctx, false),
            SnipFrom::Drawing => {}
        }
        self.interaction = Interaction::Idle;
        self.selection.clear();
        if let Mode::Overlay(o) = &mut self.mode {
            o.hint_until = 0.0;
            o.snip = Some(Snip { from, start: None, end: egui::Pos2::ZERO, rect: None });
        }
    }

    /// Go back to wherever the snip started from.
    fn leave_snip(&mut self, ctx: &Context, from: SnipFrom) {
        match from {
            SnipFrom::Notebook => self.exit_overlay(ctx),
            SnipFrom::ClickThrough => self.set_passthrough(ctx, true),
            SnipFrom::Drawing => {}
        }
    }

    pub fn cancel_snip(&mut self, ctx: &Context) {
        let Mode::Overlay(o) = &mut self.mode else { return };
        let Some(snip) = o.snip.take() else { return };
        o.snapshot = None;
        if let Some(h) = self.hwnd {
            platform::exclude_from_capture(h, false);
        }
        self.leave_snip(ctx, snip.from);
    }

    /// Pointer input while picking the snip area.
    pub fn snip_input(&mut self, ctx: &Context) {
        let events = ctx.input(|i| i.events.clone());
        let mut cancel = false;
        let mut done = false;
        if let Mode::Overlay(o) = &mut self.mode
            && let Some(snip) = &mut o.snip
            && snip.rect.is_none()
        {
            for ev in events {
                match ev {
                    egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, .. } => {
                        snip.start = Some(pos);
                        snip.end = pos;
                    }
                    egui::Event::PointerButton { button: egui::PointerButton::Secondary, pressed: true, .. } => {
                        cancel = true;
                    }
                    egui::Event::PointerMoved(pos) if snip.start.is_some() => snip.end = pos,
                    egui::Event::PointerButton {
                        pos, button: egui::PointerButton::Primary, pressed: false, ..
                    } => {
                        if let Some(start) = snip.start.take() {
                            let r = egui::Rect::from_two_pos(start, pos);
                            if r.width() >= 4.0 && r.height() >= 4.0 {
                                snip.rect = Some(r);
                                done = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if cancel {
            self.cancel_snip(ctx);
        } else if done {
            self.request_snapshot(SnapshotAction::Snip);
        }
    }

    pub fn request_snapshot(&mut self, action: SnapshotAction) {
        if let Mode::Overlay(o) = &mut self.mode {
            // Windows can leave the overlay out of the capture; elsewhere the overlay
            // draws nothing for a few frames instead (see `hide_for_capture`).
            if let Some(h) = self.hwnd {
                platform::exclude_from_capture(h, true);
            }
            o.snapshot = Some((action, 3));
        }
    }

    /// The overlay must paint nothing this frame because a screenshot is coming up and
    /// the platform can't exclude our window from it.
    pub fn hide_for_capture(&self) -> bool {
        self.hwnd.is_none() && matches!(&self.mode, Mode::Overlay(o) if o.snapshot.is_some())
    }

    /// Advance multi-frame overlay work: window placement and screenshots.
    fn drive_overlay(&mut self, ctx: &Context) {
        let hwnd = self.hwnd;
        let Mode::Overlay(o) = &mut self.mode else {
            if let Some((r, n)) = self.restore_rect.take() {
                if n == 0 {
                    if let Some(h) = hwnd {
                        platform::place_normal(h, r);
                    }
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
                match hwnd {
                    Some(h) => platform::show_in_taskbar(h, false),
                    None => {
                        // No native handle (macOS): cover the main display with viewport commands.
                        if let Some(size) = ctx.input(|i| i.viewport().monitor_size) {
                            ctx.send_viewport_cmd(ViewportCommand::OuterPosition(egui::Pos2::ZERO));
                            ctx.send_viewport_cmd(ViewportCommand::InnerSize(size));
                        }
                    }
                }
            }
            ctx.request_repaint();
        } else if let (Some(h), Some(m)) = (hwnd, o.monitor) {
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
            let shot = match hwnd {
                Some(h) => {
                    let shot = o.monitor.or_else(|| platform::window_monitor(h)).and_then(platform::capture);
                    platform::exclude_from_capture(h, false);
                    shot
                }
                None => platform::capture_main_screen(),
            };
            match shot {
                Some(img) => self.finish_snapshot(ctx, action, img),
                None => self.toast_error("Couldn't capture the screen"),
            }
        }
    }

    fn finish_snapshot(&mut self, ctx: &Context, action: SnapshotAction, shot: image::RgbaImage) {
        let ppp = ctx.pixels_per_point();
        let snip = match &mut self.mode {
            Mode::Overlay(o) => o.snip.take(),
            Mode::Notebook => None,
        };
        let (w, h) = (shot.width() as f32 / ppp, shot.height() as f32 / ppp);
        let mut png = std::io::Cursor::new(Vec::new());
        if shot.write_to(&mut png, image::ImageFormat::Png).is_err() {
            return;
        }
        let mut page = Page::new(format!("Screen {}", self.doc.pages.len() + 1), plexi_core::PageSize::Custom([w, h]));
        let asset = self.doc.add_asset(png.into_inner());
        page.elements.push(Element::Image(ImageEl {
            id: new_id(),
            asset,
            pos: [0.0, 0.0],
            size: [w, h],
            locked: true,
            crop: None,
        }));
        if snip.is_none_or(|s| s.from != SnipFrom::Notebook) {
            page.elements.extend(self.doc.overlay.elements.iter().cloned());
        }
        match action {
            SnapshotAction::Snip => {
                let Some(snip) = snip else { return };
                let area = snip.rect.unwrap_or(egui::Rect::NOTHING).translate(-ctx.content_rect().min.to_vec2());
                let area = plexi_core::Rect::from_min_size([area.min.x, area.min.y], [area.width(), area.height()]);
                let opts = RenderOptions { scale: ppp, background: false, ..Default::default() };
                let img = plexi_core::export::raster::area_rgba(&page, &self.doc.assets, &opts, area);
                self.doc.prune_assets();
                let img = match img {
                    Ok(i) => i,
                    Err(e) => {
                        self.toast_error(format!("Snip failed: {e}"));
                        self.leave_snip(ctx, snip.from);
                        return;
                    }
                };
                let ci = egui::ColorImage::from_rgba_unmultiplied(
                    [img.width() as usize, img.height() as usize],
                    img.as_raw(),
                );
                ctx.copy_image(ci);
                self.leave_snip(ctx, snip.from);
                if snip.from == SnipFrom::Notebook {
                    let mut png = std::io::Cursor::new(Vec::new());
                    if img.write_to(&mut png, image::ImageFormat::Png).is_ok() {
                        self.pending_image = Some((png.into_inner(), 3));
                    }
                    self.toast("Snip added to the page and copied to the clipboard");
                } else {
                    self.toast("Snip copied to the clipboard");
                }
            }
            SnapshotAction::ToPage => {
                self.checkpoint_pages();
                self.doc.pages.push(page);
                self.page = self.doc.pages.len() - 1;
                self.fit_pending = true;
                self.toast("Screen saved as a new notebook page");
            }
            SnapshotAction::Copy | SnapshotAction::SaveImage => {
                let opts = RenderOptions { scale: ppp, background: false, ..Default::default() };
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
                ShellEvent::Snip => {
                    if !matches!(&self.mode, Mode::Overlay(o) if o.snip.is_some()) {
                        self.start_snip(ctx);
                    }
                }
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
        if matches!(&self.mode, Mode::Overlay(o) if o.snip.is_some()) {
            if plain(Key::Escape) {
                self.cancel_snip(ctx);
            }
            return;
        }
        if self.crop.is_some() {
            if plain(Key::Enter) {
                self.finish_crop(true);
            }
            if plain(Key::Escape) {
                self.finish_crop(false);
            }
        }
        // While the handwriting window is open, undo works on its samples instead.
        let hw = self.handwriting.open;
        if cmd_shift(Key::Z) || cmd(Key::Y) {
            if hw { self.handwriting.redo() } else { self.redo() }
        } else if cmd(Key::Z) {
            if hw { self.handwriting.undo() } else { self.undo() }
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
        if cmd(Key::P) {
            self.open_export_dialog();
            if let Some(d) = &mut self.export {
                d.format = ExportFormat::Pdf;
            }
        }
        if cmd(Key::A) {
            self.select_all();
        }
        if cmd(Key::D) {
            self.duplicate_selection();
        }
        if cmd(Key::C) {
            self.copy_selection();
        }
        if cmd(Key::V) {
            self.paste_selection(ctx);
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
        if plain(Key::M) {
            self.tool = Tool::RectSelect;
        }
        if plain(Key::G) {
            self.tool = Tool::Hand;
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
        // An infinite canvas prints on the sheets its guides show; a fixed page as itself.
        let infinite = !self.is_overlay() && self.doc.pages[self.page].size.dims().is_none();
        let paper = if infinite { PaperChoice::Preset(self.config.sheet_paper) } else { PaperChoice::Fit };
        self.export = Some(ExportDialog {
            format: ExportFormat::Pdf,
            scope,
            scale: 2.0,
            background: !self.is_overlay(),
            area: Area::Page,
            paper,
            landscape: self.config.sheet_landscape,
            custom_in: [8.5, 11.0],
            tile: infinite,
            margin_in: 0.0,
            pick_pages: false,
            page_list: String::new(),
            preview: Default::default(),
        });
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
            if let Some(h) = self.hwnd
                && self.start.as_ref().is_some_and(|s| !s.overlay && !s.background)
            {
                platform::fit_to_work_area(h);
            }
        }
        if let Some(start) = self.start.take()
            && (start.overlay || start.background)
        {
            self.enter_overlay(&ctx, start.background);
            if let Mode::Overlay(o) = &mut self.mode {
                o.hint_until = 0.0; // nothing to explain at login
            }
        }
        self.fonts.begin_frame();
        self.poll_jobs();
        if self.update_quit {
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        self.handle_shell_events(&ctx);
        self.drive_overlay(&ctx);
        self.handle_drops_and_paste(&ctx);
        if let Some((bytes, n)) = self.pending_image.take() {
            // Wait until the notebook window is back to its size, so the snip lands in view.
            let back = matches!(self.mode, Mode::Notebook) && self.restore_rect.is_none();
            if back && n == 0 {
                self.insert_image(bytes, None, &ctx);
            } else {
                self.pending_image = Some((bytes, if back { n - 1 } else { n }));
                ctx.request_repaint();
            }
        }

        let rect = ui.max_rect();
        let hidden = self.hide_for_capture();
        match self.mode.clone() {
            Mode::Notebook => crate::ui::notebook(self, ui, rect),
            Mode::Overlay(_) if hidden => ctx.request_repaint(),
            Mode::Overlay(o) => crate::ui::overlay(self, ui, rect, &o),
        }
        if !hidden {
            crate::ui::dialogs(self, &ctx);
        }
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

/// Open a web page in the default browser.
pub fn open_url(url: &str) {
    let opener = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let _ = std::process::Command::new(opener).arg(url).spawn();
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

        /// Hold (or release) Shift for every step from here on, the way a real press
        /// would, until this is called again. `egui`'s modifiers persist across frames
        /// until an `Event::ModifiersChanged`, so one of these before a `drag`/`click`
        /// is enough to simulate the key being held throughout it.
        fn set_shift(&mut self, held: bool) {
            let m = if held { Modifiers::SHIFT } else { Modifiers::NONE };
            self.step(vec![Event::ModifiersChanged(m)]);
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
        // Middle-drag erases with any tool.
        h.drag(PointerButton::Middle, &[(400.0, 250.0), (400.0, 300.0), (400.0, 350.0)]);
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
    fn shift_click_extends_and_toggles_the_selection() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.insert_image(png(40, 40), Some([150.0, 150.0]), &ctx);
        h.step(vec![]);
        h.app.insert_image(png(40, 40), Some([450.0, 150.0]), &ctx);
        h.step(vec![]);
        let (pos_a, pos_b, id_a, id_b) = {
            let imgs: Vec<&ImageEl> = h
                .elements()
                .iter()
                .filter_map(|e| match e {
                    Element::Image(i) => Some(i),
                    _ => None,
                })
                .collect();
            assert_eq!(imgs.len(), 2);
            (imgs[0].pos, imgs[1].pos, imgs[0].id, imgs[1].id)
        };

        h.app.tool = Tool::Lasso;
        let view = h.app.view;
        let origin = h.app.canvas_origin;
        let a = view.to_screen(origin, [pos_a[0] + 5.0, pos_a[1] + 5.0]);
        let b = view.to_screen(origin, [pos_b[0] + 5.0, pos_b[1] + 5.0]);

        h.click(a.x, a.y);
        assert_eq!(h.app.selection, HashSet::from([id_a]), "a plain click replaces the selection");

        h.set_shift(true);
        h.click(b.x, b.y);
        h.set_shift(false);
        assert_eq!(h.app.selection, HashSet::from([id_a, id_b]), "shift+click adds the second image to it");

        h.set_shift(true);
        h.click(b.x, b.y);
        h.set_shift(false);
        assert_eq!(
            h.app.selection,
            HashSet::from([id_a]),
            "shift+click on an already-selected image removes it instead"
        );

        h.set_shift(true);
        h.click(b.x, b.y);
        h.set_shift(false);
        assert_eq!(h.app.selection.len(), 2, "both are selected again");

        // Dragging from inside the combined bounds moves both images together.
        h.drag(PointerButton::Primary, &[(a.x, a.y), (a.x + 40.0, a.y + 20.0), (a.x + 80.0, a.y + 40.0)]);
        let moved: Vec<[f32; 2]> = h
            .elements()
            .iter()
            .filter_map(|e| match e {
                Element::Image(i) => Some(i.pos),
                _ => None,
            })
            .collect();
        assert!(
            moved[0][0] > pos_a[0] + 10.0 && moved[1][0] > pos_b[0] + 10.0,
            "both images moved: {pos_a:?}/{pos_b:?} -> {moved:?}"
        );
        assert_eq!(h.app.selection.len(), 2, "the moved pair stays selected");
    }

    #[test]
    fn shift_dragging_the_lasso_adds_to_the_selection_too() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 500.0));
        assert_eq!(h.elements().len(), 2);
        h.app.tool = Tool::Lasso;
        // Lasso around only the first stroke.
        h.drag(
            PointerButton::Primary,
            &[(250.0, 250.0), (550.0, 250.0), (550.0, 350.0), (250.0, 350.0), (250.0, 260.0)],
        );
        assert_eq!(h.app.selection.len(), 1);
        let first = *h.app.selection.iter().next().unwrap();
        // A plain second lasso elsewhere would replace the selection; a Shift one adds.
        h.set_shift(true);
        h.drag(
            PointerButton::Primary,
            &[(250.0, 450.0), (550.0, 450.0), (550.0, 550.0), (250.0, 550.0), (250.0, 460.0)],
        );
        h.set_shift(false);
        assert_eq!(h.app.selection.len(), 2, "the second lasso's catch joins the first");
        assert!(h.app.selection.contains(&first), "the first stroke is still selected");
    }

    #[test]
    fn rect_select_picks_whats_inside_the_box_and_shift_extends_it() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 500.0));
        assert_eq!(h.elements().len(), 2);
        h.app.tool = Tool::RectSelect;
        // A straight-sided box around only the first stroke.
        h.drag(PointerButton::Primary, &[(250.0, 250.0), (550.0, 350.0)]);
        assert_eq!(h.app.selection.len(), 1);
        let first = *h.app.selection.iter().next().unwrap();
        // Same Shift-extends behavior as the freeform lasso.
        h.set_shift(true);
        h.drag(PointerButton::Primary, &[(250.0, 450.0), (550.0, 550.0)]);
        h.set_shift(false);
        assert_eq!(h.app.selection.len(), 2, "the second box's catch joins the first");
        assert!(h.app.selection.contains(&first), "the first stroke is still selected");
    }

    #[test]
    fn copy_and_paste_a_selection_works_the_same_for_lasso_and_rect_select() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        assert_eq!(h.elements().len(), 1);

        // Lasso around the stroke, copy it, paste it: a second stroke appears, selected,
        // and the original is untouched.
        h.app.tool = Tool::Lasso;
        h.drag(
            PointerButton::Primary,
            &[(250.0, 250.0), (550.0, 250.0), (550.0, 350.0), (250.0, 350.0), (250.0, 260.0)],
        );
        assert_eq!(h.app.selection.len(), 1);
        h.app.copy_selection();
        h.app.paste_selection(&ctx);
        assert_eq!(h.elements().len(), 2, "paste added a copy");
        assert_eq!(h.app.selection.len(), 1, "only the pasted copy is selected");
        let pasted = *h.app.selection.iter().next().unwrap();
        assert!(h.elements().iter().any(|e| e.id() != pasted), "the original is still there");

        // Pasting again drops another copy, with yet another fresh id.
        h.app.paste_selection(&ctx);
        assert_eq!(h.elements().len(), 3);
        assert!(!h.app.selection.contains(&pasted), "the newest paste replaces the selection");

        // The rectangle select tool copies and pastes exactly the same way.
        h.app.tool = Tool::RectSelect;
        h.app.selection.clear();
        h.drag(PointerButton::Primary, &[(250.0, 250.0), (550.0, 350.0)]);
        assert_eq!(h.app.selection.len(), 1);
        h.app.copy_selection();
        let before = h.elements().len();
        h.app.paste_selection(&ctx);
        assert_eq!(h.elements().len(), before + 1, "rect-select copy/paste also adds a copy");

        // Paste is a no-op without anything copied.
        h.app.clip = None;
        let n = h.elements().len();
        h.app.paste_selection(&ctx);
        assert_eq!(h.elements().len(), n, "nothing to paste");
    }

    #[test]
    fn hand_tool_pans_empty_canvas_and_grabs_an_element() {
        let mut h = Harness::new();
        h.app.tool = Tool::Hand;
        // Empty canvas: panning, just like Space+drag or a right-drag.
        let before = h.app.view.offset;
        h.drag(PointerButton::Primary, &[(400.0, 300.0), (420.0, 340.0), (450.0, 380.0)]);
        assert_eq!(h.app.view.offset - before, egui::vec2(50.0, 80.0), "dragged empty canvas pans the view");
        assert!(h.elements().is_empty(), "the hand tool never draws");

        // On an element: grabs and moves it instead of panning. The view hasn't moved
        // since the stroke was drawn, so the same screen point still lands on it.
        h.app.tool = Tool::Pen;
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        assert_eq!(h.elements().len(), 1);
        h.app.tool = Tool::Hand;
        let before_offset = h.app.view.offset;
        h.drag(PointerButton::Primary, &[(400.0, 300.0), (430.0, 300.0), (460.0, 310.0)]);
        assert_eq!(h.app.selection.len(), 1, "clicking an element selects it");
        assert_eq!(h.app.view.offset, before_offset, "grabbing an element doesn't also pan");

        // In the screen overlay there's no view to pan; dragging empty space is a no-op.
        let ctx = h.ctx.clone();
        h.app.enter_overlay(&ctx, false);
        h.step(vec![]);
        h.app.tool = Tool::Hand;
        h.drag(PointerButton::Primary, &[(100.0, 100.0), (150.0, 150.0), (200.0, 200.0)]);
        assert!(h.app.doc.overlay.elements.is_empty(), "the hand tool still never draws, even on screen");
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
        let path = std::env::temp_dir().join(format!("plexi-save-test-{}.plexi", std::process::id()));
        h.app.path = Some(path.clone());
        h.app.save();
        for _ in 0..1000 {
            h.step(vec![]);
            if !h.app.saving {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let back = format::load(&path).unwrap();
        assert_eq!(back.pages[0].elements.len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// A tiny in-memory GIF with one solid-color frame per entry in `colors`, each
    /// held for `delay_ms` (kept at or above `plexi_core::gif::MIN_DELAY_MS` so the
    /// browser-style too-short-delay clamp doesn't change what the test expects).
    fn gif(colors: &[[u8; 4]], delay_ms: u32) -> Vec<u8> {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let mut bytes = Vec::new();
        let mut enc = GifEncoder::new(&mut bytes);
        for &c in colors {
            let img = image::RgbaImage::from_pixel(4, 4, image::Rgba(c));
            enc.encode_frame(Frame::from_parts(img, 0, 0, Delay::from_numer_denom_ms(delay_ms, 1))).unwrap();
        }
        drop(enc);
        bytes
    }

    #[test]
    fn gif_assets_keep_their_bytes_and_decode_every_frame() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        let bytes = gif(&[[255, 0, 0, 255], [0, 255, 0, 255], [0, 0, 255, 255]], 150);
        h.app.insert_image(bytes.clone(), None, &ctx);
        h.step(vec![]);
        let img = image_el(&h);
        assert_eq!(
            h.app.doc.assets.get(&img.asset).map(|b| (**b).clone()),
            Some(bytes),
            "a GIF's original bytes are kept as-is, so its other frames survive save/load"
        );
        // Painting the canvas (done as part of the step above) decodes and caches it.
        match h.app.textures.get(&img.asset) {
            Some(AssetTexture::Animated { frames, delays_ms, total_ms }) => {
                assert_eq!(frames.len(), 3);
                assert_eq!(delays_ms, &[150, 150, 150]);
                assert_eq!(*total_ms, 450);
            }
            other => panic!("expected a decoded animation, got {}", other.is_some()),
        }
    }

    #[test]
    fn animated_images_pick_their_frame_from_elapsed_time() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.insert_image(gif(&[[255, 0, 0, 255], [0, 255, 0, 255]], 100), None, &ctx);
        h.step(vec![]);
        let img = image_el(&h);
        let first_id = h.app.textures.get(&img.asset).unwrap().current(&ctx).id();
        // Advance well past one full loop of the 200ms-long animation.
        for _ in 0..30 {
            h.step(vec![]);
        }
        let later_id = h.app.textures.get(&img.asset).unwrap().current(&ctx).id();
        assert_ne!(first_id, later_id, "the picture shown changes as time passes");
    }

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbaImage::from_pixel(w, h, image::Rgba([200, 30, 30, 255]))
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    fn image_el(h: &Harness) -> ImageEl {
        h.elements()
            .iter()
            .find_map(|e| match e {
                Element::Image(i) => Some(i.clone()),
                _ => None,
            })
            .expect("an image")
    }

    #[test]
    fn images_resize_lock_and_unlock() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.insert_image(png(100, 50), None, &ctx);
        h.step(vec![]);
        let img = image_el(&h);
        assert_eq!(img.size, [100.0, 50.0]);
        let view = h.app.view;
        let corner = view.to_screen(h.app.canvas_origin, [img.pos[0] + 100.0, img.pos[1] + 50.0]);
        // Drag the bottom-right handle 100 points right: it resizes freely, so only the
        // width grows.
        h.drag(
            PointerButton::Primary,
            &[(corner.x, corner.y), (corner.x + 50.0, corner.y), (corner.x + 100.0, corner.y)],
        );
        let wide = image_el(&h);
        assert!((wide.size[0] - 200.0).abs() < 1.0 && (wide.size[1] - 50.0).abs() < 1.0, "{:?}", wide.size);
        assert_eq!(wide.pos, img.pos, "the opposite corner stays put");
        assert!(h.app.selection.contains(&wide.id), "the resized image stays selected");
        h.app.undo();
        assert_eq!(image_el(&h).size, [100.0, 50.0]);

        // The same drag with Shift held keeps the original proportions instead.
        // (Undo clears the selection, so the handle needs picking up again first.)
        h.app.selection = [image_el(&h).id].into();
        h.set_shift(true);
        h.drag(
            PointerButton::Primary,
            &[(corner.x, corner.y), (corner.x + 50.0, corner.y), (corner.x + 100.0, corner.y)],
        );
        h.set_shift(false);
        let big = image_el(&h);
        assert!((big.size[0] - 200.0).abs() < 1.0 && (big.size[1] - 100.0).abs() < 1.0, "{:?}", big.size);
        assert_eq!(big.pos, img.pos, "the opposite corner stays put");
        h.app.undo();
        assert_eq!(image_el(&h).size, [100.0, 50.0]);

        // Right-click opens the menu on the image instead of erasing.
        let mid = view.to_screen(h.app.canvas_origin, [img.pos[0] + 50.0, img.pos[1] + 25.0]);
        h.drag(PointerButton::Secondary, &[(mid.x, mid.y)]);
        let menu = h.app.context_menu.clone().expect("context menu");
        assert_eq!(menu.hit, Some(image_el(&h).id));
        h.app.context_menu = None;

        let id = image_el(&h).id;
        h.app.set_locked(Target::Page(0), &[id].into(), true);
        h.app.selection.clear();
        h.app.tool = Tool::Lasso;
        h.click(mid.x, mid.y);
        assert!(h.app.selection.is_empty(), "locked images can't be picked up");
        let before = image_el(&h).pos;
        h.drag(PointerButton::Primary, &[(mid.x, mid.y), (mid.x + 40.0, mid.y), (mid.x + 80.0, mid.y + 10.0)]);
        assert_eq!(image_el(&h).pos, before, "dragging over a locked image starts a lasso, not a move");
        h.app.select_all();
        assert!(h.app.selection.is_empty());
        h.app.set_locked(Target::Page(0), &[id].into(), false);
        h.click(mid.x, mid.y);
        assert!(h.app.selection.contains(&id), "unlocked images can be clicked again");
    }

    fn key(k: Key) -> Event {
        Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() }
    }

    #[test]
    fn cropping_an_image() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.insert_image(png(100, 50), None, &ctx);
        h.step(vec![]);
        let img = image_el(&h);
        h.app.start_crop(Target::Page(0), img.id);
        assert!(h.app.crop.is_some());
        let view = h.app.view;
        // Drag the left edge's handle 40 points to the right.
        let left = view.to_screen(h.app.canvas_origin, [img.pos[0], img.pos[1] + 25.0]);
        h.drag(PointerButton::Primary, &[(left.x, left.y), (left.x + 20.0, left.y), (left.x + 40.0, left.y)]);
        h.step(vec![key(Key::Enter)]);
        h.step(vec![]);
        assert!(h.app.crop.is_none(), "Enter applies the crop");
        let cropped = image_el(&h);
        let cut = 40.0 / view.zoom;
        assert!((cropped.size[0] - (100.0 - cut)).abs() < 0.5, "{:?}", cropped.size);
        assert!((cropped.pos[0] - (img.pos[0] + cut)).abs() < 0.5);
        let [l, _, r, _] = cropped.crop.expect("a crop");
        assert!((l - cut / 100.0).abs() < 0.01 && (r - 1.0).abs() < 1e-3, "{l} {r}");
        assert!(h.app.selection.contains(&cropped.id));

        // Esc leaves a crop alone; undo and "Remove crop" bring the whole picture back.
        h.app.start_crop(Target::Page(0), cropped.id);
        h.step(vec![key(Key::Escape)]);
        assert_eq!(image_el(&h).crop, cropped.crop);
        h.app.remove_crop(Target::Page(0), cropped.id);
        let whole = image_el(&h);
        assert_eq!(whole.crop, None);
        assert!((whole.size[0] - 100.0).abs() < 0.01 && (whole.pos[0] - img.pos[0]).abs() < 0.01);
        h.app.undo();
        h.app.undo();
        assert_eq!(image_el(&h).crop, None);
        assert_eq!(image_el(&h).size, [100.0, 50.0]);
    }

    #[test]
    fn styling_text() {
        let mut h = Harness::new();
        h.app.tool = Tool::Text;
        h.click(300.0, 400.0);
        h.step(vec![Event::Text("hi".into())]);
        // The style controls change the box being typed in, and the style of new boxes.
        h.app.apply_text_style(|t| {
            t.size = 32.0;
            t.underline = true;
        });
        h.step(vec![]);
        assert!(h.app.editing_text.is_some(), "still typing after a style change");
        h.click(700.0, 650.0);
        h.step(vec![]);
        let Some(Element::Text(t)) = h.elements().first().cloned() else { panic!("a text box") };
        assert_eq!((t.text.as_str(), t.size, t.underline), ("hi", 32.0, true));
        assert_eq!(h.app.config.text_size, 32.0);

        // Double-click it with the select tool to edit it again.
        h.app.tool = Tool::Lasso;
        let at = h.app.view.to_screen(h.app.canvas_origin, [t.pos[0] + 10.0, t.pos[1] + 10.0]);
        h.click(at.x, at.y);
        h.click(at.x, at.y);
        assert!(h.app.editing_text.is_some(), "double-click edits the text");
        h.step(vec![key(Key::Escape)]);
        assert!(h.app.editing_text.is_none());

        // A style change on a selected box is one undo step.
        h.app.select_all();
        h.app.apply_text_style(|t| t.bold = true);
        assert!(matches!(h.elements().first(), Some(Element::Text(t)) if t.bold));
        assert_eq!(h.app.selection.len(), 1, "the restyled box stays selected");
        h.app.undo();
        assert!(matches!(h.elements().first(), Some(Element::Text(t)) if !t.bold));
    }

    #[test]
    fn drag_resizing_a_text_box_sets_its_wrap_width() {
        let mut h = Harness::new();
        h.app.tool = Tool::Text;
        h.click(300.0, 300.0);
        h.step(vec![Event::Text("hello wide world".into())]);
        h.click(700.0, 650.0); // click elsewhere: commits the box
        h.step(vec![]);
        let Some(Element::Text(t)) = h.elements().first().cloned() else { panic!("a text box") };
        assert_eq!(t.wrap, None, "unwrapped until dragged");
        let bounds = t.approx_bounds();

        h.app.tool = Tool::Lasso;
        let view = h.app.view;
        let origin = h.app.canvas_origin;
        let at = view.to_screen(origin, [t.pos[0] + 10.0, t.pos[1] + 10.0]);
        h.click(at.x, at.y);
        assert_eq!(h.app.selection, HashSet::from([t.id]), "clicking the text selects it");

        // The handles sit on the dashed selection line, halfway down each side. Drag the
        // right one left, shrinking the box to ~60 page units wide.
        let (handle, target) = right_handle(&h, &bounds, 60.0);
        let mid = handle + (target - handle) / 2.0;
        h.drag(PointerButton::Primary, &[(handle.x, handle.y), (mid.x, mid.y), (target.x, target.y)]);

        let Some(Element::Text(resized)) = h.elements().first().cloned() else { panic!("a text box") };
        assert!(resized.wrap.is_some_and(|w| (w - 60.0).abs() < 2.0), "{:?}", resized.wrap);
        assert_eq!(resized.pos, t.pos, "dragging the right edge keeps the left edge in place");
        assert!(resized.wrapped_lines().len() > 1, "narrower than the text: it wraps to more lines");
        assert!(h.app.selection.contains(&resized.id), "the resized box stays selected");

        h.app.undo();
        assert_eq!(
            h.elements().first().and_then(|e| match e {
                Element::Text(t) => t.wrap,
                _ => None,
            }),
            None,
            "undo removes the wrap width again"
        );
    }

    /// The right-hand wrap handle of a selected text box with page bounds `b`, and the
    /// screen point to drag it to for a box `width` page units wide. The handle sits on
    /// the dashed selection outline, 4 points outside the text.
    fn right_handle(h: &Harness, b: &plexi_core::Rect, width: f32) -> (Pos2, Pos2) {
        let (view, origin) = (h.app.view, h.app.canvas_origin);
        let mid_y = (b.min[1] + b.max[1]) / 2.0;
        let handle = view.to_screen(origin, [b.max[0], mid_y]) + egui::vec2(4.0, 0.0);
        let target = view.to_screen(origin, [b.min[0] + width, mid_y]) + egui::vec2(4.0, 0.0);
        (handle, target)
    }

    fn first_text(h: &Harness) -> TextBox {
        h.elements()
            .iter()
            .find_map(|e| match e {
                Element::Text(t) => Some(t.clone()),
                _ => None,
            })
            .expect("a text box")
    }

    #[test]
    fn the_hand_and_text_tools_resize_text_boxes_too() {
        let mut h = Harness::new();
        h.app.tool = Tool::Text;
        h.click(300.0, 300.0);
        h.step(vec![Event::Text("hello wide world".into())]);
        h.click(700.0, 650.0);
        h.step(vec![]);
        let t = first_text(&h);

        // Hand: clicking the box selects it, and its side handle then resizes it.
        h.app.tool = Tool::Hand;
        let at = h.app.view.to_screen(h.app.canvas_origin, [t.pos[0] + 10.0, t.pos[1] + 10.0]);
        h.click(at.x, at.y);
        assert_eq!(h.app.selection, HashSet::from([t.id]));
        let (handle, target) = right_handle(&h, &t.approx_bounds(), 80.0);
        let offset = h.app.view.offset;
        h.drag(PointerButton::Primary, &[(handle.x, handle.y), (target.x, target.y)]);
        let resized = first_text(&h);
        assert!(resized.wrap.is_some_and(|w| (w - 80.0).abs() < 2.0), "{:?}", resized.wrap);
        assert_eq!(h.app.view.offset, offset, "dragging the handle doesn't pan");

        // Text: while typing in a box, its guide carries the same handles; dragging one
        // resizes the box and typing carries on afterwards.
        h.app.tool = Tool::Text;
        h.app.selection.clear();
        h.app.begin_text_edit(h.app.target(), resized.id);
        h.step(vec![]);
        h.step(vec![]);
        let (id, guide) = h.app.edit_guide.expect("the guide was drawn");
        assert_eq!(id, resized.id);
        let from = guide.right_center();
        let to = from - egui::vec2(20.0 * h.app.view.zoom, 0.0);
        h.drag(PointerButton::Primary, &[(from.x, from.y), (to.x, to.y)]);
        let narrower = first_text(&h);
        assert!(narrower.wrap.is_some_and(|w| (w - 60.0).abs() < 3.0), "{:?}", narrower.wrap);
        assert_eq!(h.app.editing_text.map(|(_, id)| id), Some(narrower.id), "still typing in it");
        assert_eq!(h.elements().len(), 1, "no new box was started");
    }

    #[test]
    fn clicking_empty_canvas_with_the_hand_deselects() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        h.app.tool = Tool::Hand;
        h.click(400.0, 300.0);
        assert_eq!(h.app.selection.len(), 1, "clicking the stroke selects it");
        h.click(400.0, 600.0);
        assert!(h.app.selection.is_empty(), "clicking off it lets go");
    }

    #[test]
    fn the_snapshot_tool_copies_an_area_without_touching_the_page() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0));
        h.app.tool = Tool::Snapshot;
        h.drag(PointerButton::Primary, &[(250.0, 250.0), (400.0, 300.0), (550.0, 350.0)]);
        assert_eq!(h.elements().len(), 1, "nothing drawn, nothing removed");
        assert!(h.app.selection.is_empty(), "nothing selected either");
        assert!(h.app.toast.as_ref().is_some_and(|t| t.text == "Snapshot copied" && !t.error));

        // Copying a selection no longer puts a picture on the clipboard: only the
        // snapshot tool does that.
        h.app.toast = None;
        h.app.tool = Tool::RectSelect;
        h.drag(PointerButton::Primary, &[(250.0, 250.0), (550.0, 350.0)]);
        h.app.copy_selection();
        assert!(h.app.clip.is_some());
    }

    #[test]
    fn ctrl_z_in_the_handwriting_window_takes_back_strokes() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(300.0, 500.0, 300.0)); // ink on the page
        h.app.handwriting.open = true;
        h.step(vec![]);
        let rect = *h.app.handwriting.boxes.get(&'A').expect("the 'A' box was painted");
        h.drag(PointerButton::Primary, &line(rect.left() + 4.0, rect.right() - 4.0, rect.center().y));
        h.drag(PointerButton::Primary, &line(rect.left() + 4.0, rect.right() - 4.0, rect.center().y + 10.0));
        assert_eq!(h.app.handwriting.samples.strokes('A').len(), 2);

        let ctrl_z =
            Event::Key { key: Key::Z, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
        h.step(vec![ctrl_z.clone()]);
        assert_eq!(h.app.handwriting.samples.strokes('A').len(), 1, "one stroke taken back");
        assert_eq!(h.elements().len(), 1, "the page's ink is left alone");
        let ctrl_y =
            Event::Key { key: Key::Y, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::COMMAND };
        h.step(vec![ctrl_y]);
        assert_eq!(h.app.handwriting.samples.strokes('A').len(), 2, "and redone");
        h.app.handwriting.undo();
        h.app.handwriting.undo();
        assert!(h.app.handwriting.samples.strokes('A').is_empty());

        h.app.handwriting.open = false;
        h.step(vec![ctrl_z]);
        assert!(h.elements().is_empty(), "with the window closed, Ctrl+Z undoes the page again");
    }

    #[test]
    fn snipping_from_the_notebook() {
        let mut h = Harness::new();
        let ctx = h.ctx.clone();
        h.app.start_snip(&ctx);
        h.step(vec![]);
        assert!(matches!(&h.app.mode, Mode::Overlay(o) if o.snip.is_some()));
        h.drag(PointerButton::Primary, &[(100.0, 100.0), (200.0, 180.0), (300.0, 250.0)]);
        assert!(h.app.doc.overlay.elements.is_empty(), "dragging picks an area instead of drawing");
        let Mode::Overlay(o) = &mut h.app.mode else { panic!("still on screen") };
        let rect = o.snip.and_then(|s| s.rect).expect("the picked area");
        assert_eq!(rect, egui::Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 250.0)));
        assert!(o.snapshot.is_some(), "a screenshot is on its way");
        // Stand in for the screenshot instead of capturing the real screen.
        o.snapshot = None;
        let shot = image::RgbaImage::from_pixel(1000, 800, image::Rgba([10, 120, 200, 255]));
        h.app.finish_snapshot(&ctx, SnapshotAction::Snip, shot);
        assert!(!h.app.is_overlay(), "back to the notebook");
        for _ in 0..8 {
            h.step(vec![]);
        }
        let img = image_el(&h);
        assert_eq!(img.size, [200.0, 150.0], "the snip lands on the page");
    }

    #[test]
    fn right_click_opens_the_menu_and_right_drag_still_erases() {
        let mut h = Harness::new();
        h.drag(PointerButton::Primary, &line(200.0, 600.0, 300.0));
        h.drag(PointerButton::Secondary, &[(400.0, 300.0)]);
        assert_eq!(h.elements().len(), 1, "a right click doesn't erase");
        assert!(h.app.context_menu.is_some());
        // The next press only dismisses the menu.
        h.click(100.0, 600.0);
        assert!(h.app.context_menu.is_none());
        assert_eq!(h.elements().len(), 1);
        // Right-drag pans instead of erasing.
        let before = h.app.view.offset;
        h.drag(PointerButton::Secondary, &[(400.0, 250.0), (420.0, 300.0), (440.0, 350.0)]);
        assert_eq!(h.elements().len(), 1);
        assert_eq!(h.app.view.offset - before, egui::vec2(40.0, 100.0));
        assert!(h.app.context_menu.is_none(), "a right-drag doesn't open the menu");
        let s = h.app.view.to_screen(h.app.canvas_origin, [300.0, 228.0]);
        h.drag(PointerButton::Middle, &[(s.x, s.y - 50.0), (s.x, s.y), (s.x, s.y + 50.0)]);
        assert!(h.elements().is_empty(), "middle-drag erases");
    }

    #[test]
    fn the_mouse_wheel_zooms_at_the_pointer() {
        let mut h = Harness::new();
        let at = pos2(500.0, 400.0);
        let origin = h.app.canvas_origin;
        let before = h.app.view.to_page(origin, at);
        let zoom = h.app.view.zoom;
        let wheel = Event::MouseWheel {
            unit: egui::MouseWheelUnit::Line,
            delta: egui::vec2(0.0, 3.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Default::default(),
        };
        h.step(vec![Event::PointerMoved(at)]);
        h.step(vec![wheel]);
        for _ in 0..30 {
            h.step(vec![]);
        }
        assert!(h.app.view.zoom > zoom * 1.2, "zoomed in: {} -> {}", zoom, h.app.view.zoom);
        let after = h.app.view.to_page(origin, at);
        let moved = (after[0] - before[0]).abs().max((after[1] - before[1]).abs());
        assert!(moved < 0.5, "the point under the pointer stays put ({moved})");
    }

    #[test]
    fn deleting_pages() {
        let mut h = Harness::new();
        h.app.add_page();
        h.app.add_page();
        assert_eq!(h.app.doc.pages.len(), 3);
        h.app.doc.pages[1].name = "Middle".into();
        h.app.go_to_page(2);
        h.app.delete_page_at(1);
        assert_eq!(h.app.doc.pages.len(), 2);
        assert_eq!(h.app.page, 1, "still on the same page after deleting one before it");
        assert!(h.app.doc.pages.iter().all(|p| p.name != "Middle"));
        h.app.undo();
        assert_eq!(h.app.doc.pages.len(), 3);
        h.app.delete_page();
        h.app.delete_page();
        h.app.delete_page();
        assert_eq!(h.app.doc.pages.len(), 1, "the last page is cleared, never removed");
    }

    #[test]
    fn new_notes_are_an_infinite_canvas_with_sheets() {
        let mut h = Harness::new();
        assert!(h.app.doc.pages[0].size.dims().is_none());
        h.drag(PointerButton::Primary, &line(200.0, 400.0, 300.0));
        h.drag(PointerButton::Primary, &line(880.0, 960.0, 300.0)); // past the first sheet's right edge
        h.step(vec![]);
        let sheets = h.app.sheet_cache.as_ref().map(|c| c.3.len());
        assert_eq!(sheets, Some(2), "a second sheet appears where the ink went");
        h.app.open_export_dialog();
        // The first render loads system fonts, which is slow on CI runners.
        for _ in 0..2000 {
            h.step(vec![]);
            let pv = &h.app.export.as_ref().unwrap().preview;
            if pv.ready() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let dlg = h.app.export.as_ref().unwrap();
        assert_eq!(dlg.preview.frame_count(), 2, "PDF export defaults to one sheet per outline");
        assert!(dlg.preview.ready(), "the preview rendered");
        // Pick just sheet 2.
        let dlg = h.app.export.as_mut().unwrap();
        dlg.pick_pages = true;
        dlg.page_list = "2".into();
        h.step(vec![]);
        let dlg = h.app.export.as_ref().unwrap();
        assert_eq!((dlg.preview.frame_count(), dlg.preview.total()), (1, 2));
        assert_eq!(dlg.pick(2), Ok(Some(vec![1])));
    }

    #[test]
    fn handwriting_window_captures_a_stroke_and_builds_a_font() {
        let mut h = Harness::new();
        h.app.handwriting.open = true;
        h.step(vec![]); // paint the window once so its boxes are recorded

        let rect = *h.app.handwriting.boxes.get(&'A').expect("the 'A' box was painted");
        h.drag(PointerButton::Primary, &line(rect.left() + 4.0, rect.right() - 4.0, rect.center().y));

        let strokes = h.app.handwriting.samples.strokes('A');
        assert_eq!(strokes.len(), 1, "one drag makes one stroke");
        assert!(strokes[0].points.len() > 1);
        // Points stay within (or very near) the box: normalized by its height, not its
        // narrower width, so a round pen tip isn't stretched into an ellipse.
        for p in &strokes[0].points {
            assert!((-0.2..1.2).contains(&p.x) && (-0.2..1.2).contains(&p.y));
        }
        assert!(h.app.handwriting.samples.strokes('B').is_empty(), "untouched boxes stay empty");

        let bytes = plexi_core::handwriting::build_font(&h.app.handwriting.samples);
        let face = ttf_parser::Face::parse(&bytes, 0).expect("a valid font came out of a real capture");
        assert!(face.glyph_index('A').is_some());
        assert!(face.glyph_index('B').is_none(), "characters never written fall back to the default font");
    }
}
