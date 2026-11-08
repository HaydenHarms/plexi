//! The drawing surface: view transform, pointer and pen input for every tool, and
//! rendering a page (paper, background, elements, live previews).
//!
//! Input is read from the raw event list rather than per-frame pointer state, so every
//! pen sample between two frames lands in the stroke, not just one per frame.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::epaint::{Vertex, WHITE_UV};
use egui::text::{LayoutJob, TextFormat};
use egui::{
    Align2, Color32, CursorIcon, Event, FontId, Galley, Id, Key, Margin, Order, PointerButton, Pos2, Rect, Sense,
    Shape as EShape, Stroke as EStroke, StrokeKind, TextEdit, Ui, Vec2, pos2, vec2,
};
use plexi_core::export::svg::{Mark, background_marks};
use plexi_core::geom::{dist, dist_to_segment, point_in_polygon};
use plexi_core::ink::{self, StrokeBuilder};
use plexi_core::{
    BASELINE, Color, Element, InkKind, LINE_HEIGHT, Page, RULE_SPACING, Shape, ShapeKind, Stroke, TextBox, new_id,
    rule_color,
};

use crate::app::{ContextMenu, Mode, PlexiApp, Tool};
use crate::fonts::FontBook;
use crate::history::Target;

/// Maps page units to screen points: `screen = origin + offset + page * zoom`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct View {
    pub offset: Vec2,
    pub zoom: f32,
}

impl Default for View {
    fn default() -> Self {
        Self { offset: Vec2::ZERO, zoom: 1.0 }
    }
}

impl View {
    pub fn to_screen(self, origin: Pos2, p: [f32; 2]) -> Pos2 {
        origin + self.offset + vec2(p[0], p[1]) * self.zoom
    }

    pub fn to_page(self, origin: Pos2, s: Pos2) -> [f32; 2] {
        let v = (s - origin - self.offset) / self.zoom;
        [v.x, v.y]
    }

    pub fn rect_to_screen(self, origin: Pos2, r: plexi_core::Rect) -> Rect {
        Rect::from_min_max(self.to_screen(origin, r.min), self.to_screen(origin, r.max))
    }

    pub fn zoom_around(&mut self, origin: Pos2, at: Pos2, factor: f32) {
        let new = (self.zoom * factor).clamp(0.1, 12.0);
        let f = new / self.zoom;
        let rel = at - origin;
        self.offset = rel - (rel - self.offset) * f;
        self.zoom = new;
    }
}

pub enum Interaction {
    Idle,
    Ink(Box<StrokeBuilder>),
    Erase {
        last: [f32; 2],
        changed: bool,
    },
    /// A lasso or rectangle-select drag: `pts` is the polygon hit-tested against the
    /// page on release (the lasso's own trace, or a rectangle's four corners when
    /// `rect` is set). `extend` is `true` when Shift was held as it started: its catch
    /// is added to the selection on release instead of replacing it.
    Lasso {
        pts: Vec<[f32; 2]>,
        extend: bool,
        rect: bool,
    },
    Move {
        start: [f32; 2],
        delta: [f32; 2],
    },
    Shape {
        kind: ShapeKind,
        start: [f32; 2],
        end: [f32; 2],
    },
    Pan {
        last: Pos2,
    },
    /// Text tool pressed here; the box opens on release.
    TextAt([f32; 2]),
    /// Right button down: it becomes an erase once it moves, the context menu if not.
    RightPress {
        start: Pos2,
    },
    /// Dragging a corner of the selected image. `anchor` is the opposite corner, `dir`
    /// the direction from it to the dragged one, `orig` the size when the drag began.
    Resize {
        id: u64,
        anchor: [f32; 2],
        dir: [f32; 2],
        orig: [f32; 2],
        changed: bool,
    },
    /// Dragging a corner of the selected text box to change its wrap width. `anchor_x`
    /// is the x of the opposite edge (which stays put), `dir_x` is +1 when the dragged
    /// corner is on the right, -1 on the left.
    TextResize {
        id: u64,
        anchor_x: f32,
        dir_x: f32,
        changed: bool,
    },
}

/// A grabbed image corner: (image id, opposite corner, direction to the grabbed corner,
/// image size).
type Handle = (u64, [f32; 2], [f32; 2], [f32; 2]);

/// Screen distance within which a resize handle catches the pointer.
const HANDLE_REACH: f32 = 10.0;

/// An image being cropped: the whole picture (`full`) and the part that will show.
#[derive(Clone, Debug)]
pub struct CropState {
    pub target: Target,
    pub id: u64,
    pub full: plexi_core::Rect,
    pub shown: plexi_core::Rect,
    pub drag: Option<CropDrag>,
}

/// A crop edge being dragged. `edge` is -1/0/1 per axis (left/none/right, top/none/
/// bottom); `[0, 0]` moves the crop window over the picture.
#[derive(Clone, Copy, Debug)]
pub struct CropDrag {
    pub edge: [i8; 2],
    pub start: [f32; 2],
    pub orig: plexi_core::Rect,
}

/// The eight crop handles: corners and edge middles.
const CROP_EDGES: [[i8; 2]; 8] = [[-1, -1], [0, -1], [1, -1], [1, 0], [1, 1], [0, 1], [-1, 1], [-1, 0]];

fn edge_point(r: Rect, e: [i8; 2]) -> Pos2 {
    let pick = |lo: f32, hi: f32, k: i8| match k {
        -1 => lo,
        1 => hi,
        _ => (lo + hi) / 2.0,
    };
    pos2(pick(r.left(), r.right(), e[0]), pick(r.top(), r.bottom(), e[1]))
}

/// Tessellated strokes, keyed by element id. Meshes are rebuilt when the zoom moves to
/// a different half-octave so curves stay smooth without re-tessellating every frame.
#[derive(Default)]
pub struct MeshCache {
    map: HashMap<u64, (i32, Arc<ink::Mesh>)>,
    used: HashSet<u64>,
}

impl MeshCache {
    pub fn get(&mut self, s: &Stroke, px_per_unit: f32) -> Arc<ink::Mesh> {
        let bucket = (px_per_unit.max(0.01).log2() * 2.0).round() as i32;
        self.used.insert(s.id);
        if let Some((b, m)) = self.map.get(&s.id)
            && *b == bucket
        {
            return m.clone();
        }
        let m = Arc::new(ink::stroke_mesh(s, 2f32.powf(bucket as f32 / 2.0)));
        self.map.insert(s.id, (bucket, m.clone()));
        m
    }

    pub fn end_frame(&mut self) {
        if self.map.len() > self.used.len() + 512 {
            let used = &self.used;
            self.map.retain(|k, _| used.contains(k));
        }
        self.used.clear();
    }
}

pub fn color32(c: Color) -> Color32 {
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a())
}

fn to_egui_mesh(m: &ink::Mesh, view: View, origin: Pos2, delta: [f32; 2], color: Color32) -> egui::Mesh {
    let base = origin + view.offset;
    let vertices = m
        .vertices
        .iter()
        .map(|v| Vertex { pos: base + vec2(v[0] + delta[0], v[1] + delta[1]) * view.zoom, uv: WHITE_UV, color })
        .collect();
    egui::Mesh { indices: m.indices.clone(), vertices, texture_id: egui::TextureId::default() }
}

/// Snap with Shift: lines to 15° steps, boxes to squares.
pub fn constrain(kind: ShapeKind, a: [f32; 2], b: [f32; 2], shift: bool) -> [f32; 2] {
    if !shift {
        return b;
    }
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    match kind {
        ShapeKind::Line | ShapeKind::Arrow => {
            let len = (dx * dx + dy * dy).sqrt();
            let step = std::f32::consts::PI / 12.0;
            let ang = (dy.atan2(dx) / step).round() * step;
            [a[0] + len * ang.cos(), a[1] + len * ang.sin()]
        }
        ShapeKind::Rect | ShapeKind::Ellipse => {
            let s = dx.abs().max(dy.abs());
            [a[0] + s * dx.signum(), a[1] + s * dy.signum()]
        }
    }
}

fn shape_hit(s: &Shape, p: [f32; 2], r: f32) -> bool {
    let tol = r + s.width * 0.5;
    match s.kind {
        ShapeKind::Line | ShapeKind::Arrow => {
            dist_to_segment(p, s.a, s.b) <= tol
                || s.arrow_head()
                    .is_some_and(|[h1, h2]| dist_to_segment(p, h1, s.b) <= tol || dist_to_segment(p, h2, s.b) <= tol)
        }
        ShapeKind::Rect => {
            let r = plexi_core::Rect::from_points([s.a, s.b]);
            let c = [[r.min[0], r.min[1]], [r.max[0], r.min[1]], [r.max[0], r.max[1]], [r.min[0], r.max[1]]];
            (0..4).any(|i| dist_to_segment(p, c[i], c[(i + 1) % 4]) <= tol)
        }
        ShapeKind::Ellipse => {
            let r = plexi_core::Rect::from_points([s.a, s.b]);
            let c = r.center();
            let (rx, ry) = (r.width() / 2.0, r.height() / 2.0);
            let pts: Vec<[f32; 2]> = (0..48)
                .map(|k| {
                    let a = k as f32 / 48.0 * std::f32::consts::TAU;
                    [c[0] + rx * a.cos(), c[1] + ry * a.sin()]
                })
                .collect();
            (0..48).any(|i| dist_to_segment(p, pts[i], pts[(i + 1) % 48]) <= tol)
        }
    }
}

pub struct PaintInput<'a> {
    pub page: &'a Page,
    pub assets: &'a plexi_core::export::Assets,
    pub view: View,
    pub origin: Pos2,
    pub clip: Rect,
    /// Draw the paper and its lines (off for the overlay).
    pub paper: bool,
    pub skip: Option<u64>,
    pub moving: Option<(&'a HashSet<u64>, [f32; 2])>,
    pub canvas_bg: Color32,
    /// Sheets in use on an infinite page, and whether to outline and number them.
    pub sheets: &'a [plexi_core::Rect],
    pub sheet_lines: bool,
    pub sheet_numbers: bool,
}

pub fn paint_shape(painter: &egui::Painter, s: &Shape, view: View, origin: Pos2, delta: [f32; 2]) {
    let p = |v: [f32; 2]| view.to_screen(origin, [v[0] + delta[0], v[1] + delta[1]]);
    let stroke = EStroke::new((s.width * view.zoom).max(1.0), color32(s.color));
    match s.kind {
        ShapeKind::Line => {
            painter.line_segment([p(s.a), p(s.b)], stroke);
        }
        ShapeKind::Arrow => {
            painter.line_segment([p(s.a), p(s.b)], stroke);
            if let Some([h1, h2]) = s.arrow_head() {
                painter.add(EShape::line(vec![p(h1), p(s.b), p(h2)], stroke));
            }
        }
        ShapeKind::Rect => {
            painter.rect_stroke(Rect::from_two_pos(p(s.a), p(s.b)), 0.0, stroke, StrokeKind::Middle);
        }
        ShapeKind::Ellipse => {
            let r = Rect::from_two_pos(p(s.a), p(s.b));
            painter.add(EShape::ellipse_stroke(r.center(), r.size() / 2.0, stroke));
        }
    }
    // Round the corners of thick lines like the exporters do.
    if matches!(s.kind, ShapeKind::Line | ShapeKind::Arrow) && stroke.width > 2.0 {
        for q in [s.a, s.b] {
            painter.circle_filled(p(q), stroke.width / 2.0, stroke.color);
        }
    }
}

/// The format text box `t` is drawn with at `zoom`.
fn text_format(fonts: &mut FontBook, ctx: &egui::Context, t: &TextBox, zoom: f32) -> TextFormat {
    let size = (t.size * zoom).max(1.0);
    let color = color32(t.color);
    TextFormat {
        font_id: FontId::new(size, fonts.family(ctx, t)),
        color,
        line_height: Some(t.size * LINE_HEIGHT * zoom),
        underline: if t.underline { EStroke::new((size / 14.0).max(1.0), color) } else { EStroke::NONE },
        ..Default::default()
    }
}

/// `wrap_width` (screen px) is the live, pixel-accurate wrap while typing or just
/// displaying; it is `egui`'s own wrapping, so it can land a word differently than the
/// estimate [`TextBox::wrap_line`] uses for export, but the box width always matches.
fn layout(ui: &Ui, text: &str, format: &TextFormat, wrap_width: Option<f32>) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), format.clone());
    if let Some(w) = wrap_width {
        job.wrap = egui::text::TextWrapping::wrap_at_width(w);
    }
    ui.fonts_mut(|f| f.layout_job(job))
}

/// Text box `t` (showing `text`) laid out at `zoom`, and where the galley's top-left
/// goes relative to the box position: lines sit `LINE_HEIGHT` apart with the first
/// baseline at `BASELINE`, exactly where the exporters put them, whatever the font.
pub fn text_layout(ui: &Ui, fonts: &mut FontBook, t: &TextBox, text: &str, zoom: f32) -> (Arc<Galley>, Vec2) {
    let format = text_format(fonts, ui.ctx(), t, zoom);
    let probe = layout(ui, "H", &format, None);
    let baseline = probe.rows.first().and_then(|r| r.row.glyphs.first().map(|g| r.pos.y + g.pos.y));
    let shift = t.size * BASELINE * zoom - baseline.unwrap_or(t.size * BASELINE * zoom);
    (layout(ui, text, &format, t.wrap.map(|w| w * zoom)), vec2(0.0, shift))
}

pub fn paint_page(
    ui: &Ui,
    input: &PaintInput,
    cache: &mut MeshCache,
    textures: &mut HashMap<String, egui::TextureHandle>,
    fonts: &mut FontBook,
) {
    let painter = ui.painter_at(input.clip);
    let ctx = ui.ctx();
    let PaintInput { page, view, origin, clip, .. } = *input;
    let ppp = ctx.pixels_per_point();

    if input.paper {
        painter.rect_filled(clip, 0.0, input.canvas_bg);
        let sheet = page
            .size
            .dims()
            .map(|[w, h]| view.rect_to_screen(origin, plexi_core::Rect::from_min_size([0.0, 0.0], [w, h])));
        let paper_area = match sheet {
            Some(s) => {
                painter.rect_filled(s.translate(vec2(0.0, 2.0)).expand(1.5), 3.0, Color32::from_black_alpha(28));
                painter.rect_filled(s, 0.0, color32(page.paper));
                s.intersect(clip)
            }
            None => {
                painter.rect_filled(clip, 0.0, color32(page.paper));
                clip
            }
        };
        if paper_area.is_positive() && RULE_SPACING * view.zoom >= 7.0 {
            let a = view.to_page(origin, paper_area.min);
            let b = view.to_page(origin, paper_area.max);
            let area = plexi_core::Rect { min: a, max: b };
            let rule = EStroke::new(1.0, color32(rule_color(page.paper)));
            for m in background_marks(page.background, area) {
                match m {
                    Mark::HLine { y, x0, x1 } => {
                        painter.line_segment([view.to_screen(origin, [x0, y]), view.to_screen(origin, [x1, y])], rule);
                    }
                    Mark::VLine { x, y0, y1 } => {
                        painter.line_segment([view.to_screen(origin, [x, y0]), view.to_screen(origin, [x, y1])], rule);
                    }
                    Mark::Dot { x, y } => {
                        painter.circle_filled(view.to_screen(origin, [x, y]), (1.5 * view.zoom).max(0.8), rule.color);
                    }
                }
            }
        }
    }

    if !input.sheets.is_empty() {
        let c = rule_color(page.paper);
        let edge = EStroke::new(1.0, color32(c.with_alpha((c.a() as u32 * 2).min(255) as u8)));
        let label = color32(c.with_alpha((c.a() as u32 * 3).min(255) as u8));
        for (i, s) in input.sheets.iter().enumerate() {
            let r = view.rect_to_screen(origin, *s);
            if !r.intersects(clip) {
                continue;
            }
            if input.sheet_lines {
                let c = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
                painter.extend(EShape::dashed_line(&c, edge, 7.0, 5.0));
            }
            if input.sheet_numbers && r.width() > 120.0 {
                painter.text(
                    r.right_top() + vec2(-8.0, 6.0),
                    Align2::RIGHT_TOP,
                    format!("{}", i + 1),
                    FontId::proportional(11.0),
                    label,
                );
            }
        }
    }

    for el in &page.elements {
        let id = el.id();
        if input.skip == Some(id) {
            continue;
        }
        let delta = match input.moving {
            Some((sel, d)) if sel.contains(&id) => d,
            _ => [0.0, 0.0],
        };
        let mut b = el.bounds();
        b.min = [b.min[0] + delta[0], b.min[1] + delta[1]];
        b.max = [b.max[0] + delta[0], b.max[1] + delta[1]];
        if !view.rect_to_screen(origin, b).intersects(clip) {
            continue;
        }
        match el {
            Element::Stroke(s) => {
                if s.points.is_empty() {
                    continue;
                }
                let mesh = cache.get(s, view.zoom * ppp);
                painter.add(EShape::mesh(to_egui_mesh(&mesh, view, origin, delta, color32(s.color))));
            }
            Element::Text(t) => {
                let pos = view.to_screen(origin, [t.pos[0] + delta[0], t.pos[1] + delta[1]]);
                let (galley, shift) = text_layout(ui, fonts, t, &t.text, view.zoom);
                painter.galley(pos + shift, galley, color32(t.color));
            }
            Element::Image(img) => {
                let tex = textures.entry(img.asset.clone()).or_insert_with(|| {
                    let decoded = input
                        .assets
                        .get(&img.asset)
                        .and_then(|b| image::load_from_memory(b).ok())
                        .map(|i| i.to_rgba8());
                    let ci = match decoded {
                        Some(i) => egui::ColorImage::from_rgba_unmultiplied(
                            [i.width() as usize, i.height() as usize],
                            i.as_raw(),
                        ),
                        None => egui::ColorImage::new([1, 1], vec![Color32::from_gray(200)]),
                    };
                    ctx.load_texture(&img.asset, ci, egui::TextureOptions::LINEAR)
                });
                let r = view.rect_to_screen(
                    origin,
                    plexi_core::Rect::from_min_size([img.pos[0] + delta[0], img.pos[1] + delta[1]], img.size),
                );
                let [u0, v0, u1, v1] = img.crop_or_full();
                painter.image(tex.id(), r, Rect::from_min_max(pos2(u0, v0), pos2(u1, v1)), Color32::WHITE);
            }
            Element::Shape(s) => paint_shape(&painter, s, view, origin, delta),
        }
    }
}

impl PlexiApp {
    pub fn view_for(&self, t: Target) -> View {
        match t {
            Target::Overlay => View::default(),
            Target::Page(_) => self.view,
        }
    }

    fn fit_if_pending(&mut self, rect: Rect) {
        if !self.fit_pending {
            return;
        }
        self.fit_pending = false;
        let page = &self.doc.pages[self.page];
        match page.size.dims() {
            Some([w, _]) => {
                let zoom = ((rect.width() - 48.0) / w).clamp(0.1, 1.25);
                self.view = View { zoom, offset: vec2((rect.width() - w * zoom) / 2.0, 72.0) };
            }
            None => {
                let c = page.content_bounds().filter(|b| !b.is_empty());
                self.view = match c {
                    Some(b) => {
                        // Fit the drawing's width (never enlarging); long notes scroll down.
                        let zoom = ((rect.width() - 96.0) / b.width().max(1.0)).clamp(0.25, 1.0);
                        let x = rect.width() / 2.0 - b.center()[0] * zoom;
                        View { zoom, offset: vec2(x, 96.0 - b.min[1] * zoom) }
                    }
                    None => View { zoom: 1.0, offset: vec2(48.0, 72.0) },
                };
            }
        }
    }

    /// Pan and zoom from wheel, trackpad and two-finger touch.
    fn view_input(&mut self, ui: &Ui, rect: Rect, resp: &egui::Response) {
        let (scroll, zoom, hover, multi, shift) = ui.input(|i| {
            (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos(), i.multi_touch(), i.modifiers.shift)
        });
        // A mouse wheel zooms; trackpad two-finger scrolling pans. Remember which one
        // was used last, since smoothed scrolling keeps arriving after the events stop.
        ui.input(|i| {
            for e in &i.events {
                if let Event::MouseWheel { source, .. } = e {
                    self.trackpad_scroll =
                        matches!(source, egui::MouseWheelSource::Trackpad | egui::MouseWheelSource::Momentum);
                }
            }
        });
        if let Some(mt) = multi
            && mt.num_touches >= 2
        {
            if matches!(self.interaction, Interaction::Ink(_)) {
                self.interaction = Interaction::Idle; // the first finger was a pan, not ink
            }
            self.view.offset += mt.translation_delta;
            if mt.zoom_delta != 1.0 {
                self.view.zoom_around(rect.min, mt.center_pos, mt.zoom_delta);
            }
            return;
        }
        if !resp.contains_pointer() {
            return;
        }
        if zoom != 1.0 {
            self.view.zoom_around(rect.min, hover.unwrap_or(rect.center()), zoom);
        }
        if scroll != Vec2::ZERO {
            if self.trackpad_scroll || shift {
                // Shift+wheel pans, for mice without a comfortable right-drag.
                self.view.offset += scroll;
            } else {
                let at = hover.unwrap_or(rect.center());
                self.view.zoom_around(rect.min, at, ((scroll.y + scroll.x) / 300.0).exp());
            }
        }
    }

    fn pressure(&self) -> f32 {
        self.pen_pressure.unwrap_or(0.5)
    }

    pub fn selection_bounds(&self, t: Target) -> Option<plexi_core::Rect> {
        self.page_ref(t)
            .elements
            .iter()
            .filter(|e| self.selection.contains(&e.id()))
            .map(Element::bounds)
            .reduce(|a, b| a.union(b))
    }

    fn hit_text(&self, t: Target, p: [f32; 2]) -> Option<u64> {
        self.page_ref(t).elements.iter().rev().find_map(|e| match e {
            Element::Text(tb) if tb.approx_bounds().expand(4.0).contains(p) => Some(tb.id),
            _ => None,
        })
    }

    /// Topmost element under `p` (within `r` page units of ink and outlines).
    pub fn hit_element(&self, t: Target, p: [f32; 2], r: f32, include_locked: bool) -> Option<u64> {
        self.page_ref(t).elements.iter().rev().find_map(|e| {
            let hit = match e {
                Element::Stroke(s) => ink::stroke_hit(s, p, r),
                Element::Shape(s) => shape_hit(s, p, r),
                Element::Text(tb) => tb.approx_bounds().expand(4.0).contains(p),
                Element::Image(i) => {
                    plexi_core::Rect::from_min_size(i.pos, i.size).contains(p) && (include_locked || !i.locked)
                }
            };
            hit.then(|| e.id())
        })
    }

    /// The single selected, unlocked image, if that is what is selected.
    pub fn selected_image(&self, t: Target) -> Option<plexi_core::ImageEl> {
        if self.selection.len() != 1 {
            return None;
        }
        self.page_ref(t).elements.iter().find_map(|e| match e {
            Element::Image(i) if self.selection.contains(&i.id) && !i.locked => Some(i.clone()),
            _ => None,
        })
    }

    /// The single selected text box, if that is what is selected.
    pub fn selected_text(&self, t: Target) -> Option<TextBox> {
        if self.selection.len() != 1 {
            return None;
        }
        self.page_ref(t).elements.iter().find_map(|e| match e {
            Element::Text(tb) if self.selection.contains(&tb.id) => Some(tb.clone()),
            _ => None,
        })
    }

    /// The resize handle of the selected image under screen point `pos`.
    fn handle_at(&self, t: Target, view: View, origin: Pos2, pos: Pos2) -> Option<Handle> {
        let img = self.selected_image(t)?;
        let [x0, y0] = img.pos;
        let [x1, y1] = [x0 + img.size[0], y0 + img.size[1]];
        [
            ([x0, y0], [x1, y1], [-1.0, -1.0]),
            ([x1, y0], [x0, y1], [1.0, -1.0]),
            ([x1, y1], [x0, y0], [1.0, 1.0]),
            ([x0, y1], [x1, y0], [-1.0, 1.0]),
        ]
        .into_iter()
        .find(|(corner, _, _)| view.to_screen(origin, *corner).distance(pos) <= HANDLE_REACH)
        .map(|(_, anchor, dir)| (img.id, anchor, dir, img.size))
    }

    /// The wrap-width handle of the selected text box under screen point `pos`: one of
    /// its four corners, left side or right. There is no height field to drag, so only
    /// the x side of the corner matters.
    fn text_handle_at(&self, t: Target, view: View, origin: Pos2, pos: Pos2) -> Option<(u64, f32, f32)> {
        let tb = self.selected_text(t)?;
        let b = tb.approx_bounds();
        let (x0, x1) = (b.min[0], b.max[0]);
        [[x0, b.min[1]], [x1, b.min[1]], [x1, b.max[1]], [x0, b.max[1]]]
            .into_iter()
            .find(|corner| view.to_screen(origin, *corner).distance(pos) <= HANDLE_REACH)
            .map(|corner| if corner[0] == x0 { (tb.id, x1, -1.0) } else { (tb.id, x0, 1.0) })
    }

    // ----- cropping ------------------------------------------------------------------

    /// Start cropping image `id`: its edges become handles over the whole picture.
    pub fn start_crop(&mut self, t: Target, id: u64) {
        self.finish_text_edit();
        let Some(Element::Image(img)) = self.page_ref(t).elements.iter().find(|e| e.id() == id) else { return };
        if img.locked {
            return;
        }
        let shown = plexi_core::Rect::from_min_size(img.pos, img.size);
        self.crop = Some(CropState { target: t, id, full: img.full_rect(), shown, drag: None });
        self.selection.clear();
        self.interaction = Interaction::Idle;
        self.toast("Drag the edges to crop — Enter to apply, Esc to cancel");
    }

    /// End cropping, keeping the new crop if `apply`.
    pub fn finish_crop(&mut self, apply: bool) {
        let Some(c) = self.crop.take() else { return };
        if self.toast.as_ref().is_some_and(|t| t.text.starts_with("Drag the edges")) {
            self.toast = None;
        }
        let Some(i) = self.page_ref(c.target).find(c.id) else { return };
        if apply && let Element::Image(img) = &self.page_ref(c.target).elements[i] {
            let now = plexi_core::Rect::from_min_size(img.pos, img.size);
            let moved =
                (0..2).any(|k| (now.min[k] - c.shown.min[k]).abs() + (now.max[k] - c.shown.max[k]).abs() > 0.01);
            if moved {
                self.checkpoint(c.target);
                let page = self.page_mut(c.target);
                if let Element::Image(img) = &mut page.elements[i] {
                    img.set_crop(c.shown);
                }
                page.elements[i].renew_id();
            }
        }
        self.selection = [self.page_ref(c.target).elements[i].id()].into();
    }

    /// Show the whole of a cropped image again.
    pub fn remove_crop(&mut self, t: Target, id: u64) {
        self.checkpoint(t);
        let page = self.page_mut(t);
        let Some(i) = page.find(id) else { return };
        if let Element::Image(img) = &mut page.elements[i] {
            img.uncrop();
        }
        page.elements[i].renew_id();
        self.selection = [page.elements[i].id()].into();
    }

    /// The crop handle under screen point `pos`: an edge, or `[0, 0]` inside the crop.
    fn crop_handle_at(&self, view: View, origin: Pos2, pos: Pos2) -> Option<[i8; 2]> {
        let c = self.crop.as_ref()?;
        let r = view.rect_to_screen(origin, c.shown);
        CROP_EDGES
            .into_iter()
            .find(|e| edge_point(r, *e).distance(pos) <= HANDLE_REACH)
            .or_else(|| r.contains(pos).then_some([0, 0]))
    }

    fn drag_crop(&mut self, p: [f32; 2], zoom: f32) {
        let Some(c) = &mut self.crop else { return };
        let Some(d) = c.drag else { return };
        let (dx, dy) = (p[0] - d.start[0], p[1] - d.start[1]);
        let (full, mut r) = (c.full, d.orig);
        let min = 8.0 / zoom;
        if d.edge == [0, 0] {
            let (w, h) = (r.width(), r.height());
            let x = (r.min[0] + dx).clamp(full.min[0], (full.max[0] - w).max(full.min[0]));
            let y = (r.min[1] + dy).clamp(full.min[1], (full.max[1] - h).max(full.min[1]));
            r = plexi_core::Rect::from_min_size([x, y], [w, h]);
        } else {
            match d.edge[0] {
                -1 => r.min[0] = (r.min[0] + dx).clamp(full.min[0], r.max[0] - min),
                1 => r.max[0] = (r.max[0] + dx).clamp(r.min[0] + min, full.max[0]),
                _ => {}
            }
            match d.edge[1] {
                -1 => r.min[1] = (r.min[1] + dy).clamp(full.min[1], r.max[1] - min),
                1 => r.max[1] = (r.max[1] + dy).clamp(r.min[1] + min, full.max[1]),
                _ => {}
            }
        }
        c.shown = r;
    }

    /// Erase along the segment `from`-`to`; returns whether anything changed.
    fn erase_along(&mut self, t: Target, from: [f32; 2], to: [f32; 2], r: f32) -> bool {
        let steps = (dist(from, to) / (r * 0.5).max(0.5)).ceil().max(1.0) as usize;
        let mut changed = false;
        for k in 1..=steps {
            let f = k as f32 / steps as f32;
            let q = [from[0] + (to[0] - from[0]) * f, from[1] + (to[1] - from[1]) * f];
            changed |= self.erase_at(t, q, r);
        }
        changed
    }

    /// Open the right-click menu for whatever is under screen point `pos`.
    fn open_context_menu(&mut self, t: Target, view: View, origin: Pos2, pos: Pos2) {
        let p = view.to_page(origin, pos);
        let hit = self.hit_element(t, p, 6.0 / view.zoom, true);
        if let Some(id) = hit
            && !self.selection.contains(&id)
        {
            let locked = self.page_ref(t).elements.iter().any(|e| e.id() == id && e.locked());
            self.selection = if locked { HashSet::new() } else { [id].into() };
        }
        self.context_menu = Some(ContextMenu { pos, target: t, hit });
    }

    /// Erase around `p`; returns whether anything changed.
    fn erase_at(&mut self, t: Target, p: [f32; 2], r: f32) -> bool {
        let partial = self.config.eraser_partial;
        let page = self.page_mut(t);
        let mut changed = false;
        let old = std::mem::take(&mut page.elements);
        let mut out = Vec::with_capacity(old.len());
        for el in old {
            match &el {
                Element::Stroke(s) if partial => match ink::erase_partial(s, p, r) {
                    None => out.push(el),
                    Some(pieces) => {
                        changed = true;
                        out.extend(pieces.into_iter().map(Element::Stroke));
                    }
                },
                Element::Stroke(s) if ink::stroke_hit(s, p, r) => changed = true,
                Element::Shape(s) if shape_hit(s, p, r) => changed = true,
                _ => out.push(el),
            }
        }
        page.elements = out;
        changed
    }

    #[allow(clippy::too_many_arguments)]
    fn begin(
        &mut self,
        t: Target,
        view: View,
        origin: Pos2,
        pos: Pos2,
        button: PointerButton,
        space: bool,
        now: f64,
        shift: bool,
    ) {
        if self.editing_text.is_some() {
            self.finish_text_edit();
            if self.tool == Tool::Text {
                return;
            }
        }
        let p = view.to_page(origin, pos);
        if self.crop.is_some() {
            // While cropping, the left button works the crop; a press elsewhere applies it.
            if button == PointerButton::Primary {
                match self.crop_handle_at(view, origin, pos) {
                    Some(edge) => {
                        if let Some(c) = &mut self.crop {
                            c.drag = Some(CropDrag { edge, start: p, orig: c.shown });
                        }
                    }
                    None => self.finish_crop(true),
                }
            }
            return;
        }
        let notebook = matches!(self.mode, Mode::Notebook);
        if notebook && button == PointerButton::Primary && space {
            self.interaction = Interaction::Pan { last: pos };
            return;
        }
        let tool = match button {
            PointerButton::Primary => self.tool,
            // Middle-drag erases with any tool, and so does a pen's barrel button.
            PointerButton::Middle => Tool::Eraser,
            PointerButton::Secondary if self.pen_pressure.is_some() => Tool::Eraser,
            // Right: drag to pan, click for the menu.
            PointerButton::Secondary => {
                self.interaction = Interaction::RightPress { start: pos };
                return;
            }
            _ => return,
        };
        self.last_sample_time = now;
        match tool {
            Tool::Pen | Tool::Highlighter => {
                let kind = if tool == Tool::Pen { InkKind::Pen } else { InkKind::Highlighter };
                let color = if kind == InkKind::Pen { self.pen_color } else { self.highlighter_color };
                let stroke = Stroke { id: new_id(), kind, color, width: self.ink_width(kind), points: Vec::new() };
                let ppu = view.zoom * 1.5; // ~half a physical pixel on typical displays
                let mut b = StrokeBuilder::new(stroke, self.config.smoothing, 0.5 / ppu);
                b.push(now, p, self.pressure());
                self.selection.clear();
                self.interaction = Interaction::Ink(Box::new(b));
            }
            Tool::Eraser => {
                self.pending_snapshot = Some(self.snapshot_of(t));
                let r = self.config.eraser_size / 2.0 / view.zoom;
                let changed = self.erase_at(t, p, r);
                self.interaction = Interaction::Erase { last: p, changed };
            }
            // The rectangle select tool shares every bit of this with the freeform
            // lasso — handles, shift+click, picking up what's already selected — and
            // differs only in how the drag below turns into a hit-tested polygon.
            Tool::Lasso | Tool::RectSelect => {
                let rect = tool == Tool::RectSelect;
                if let Some((id, anchor, dir, orig)) = self.handle_at(t, view, origin, pos) {
                    self.pending_snapshot = Some(self.snapshot_of(t));
                    self.interaction = Interaction::Resize { id, anchor, dir, orig, changed: false };
                    return;
                }
                if let Some((id, anchor_x, dir_x)) = self.text_handle_at(t, view, origin, pos) {
                    self.pending_snapshot = Some(self.snapshot_of(t));
                    self.interaction = Interaction::TextResize { id, anchor_x, dir_x, changed: false };
                    return;
                }
                if shift {
                    // Shift+click adds an unselected element to the selection, or
                    // removes one that's already in it — it never just replaces the
                    // selection, so it skips the "drag what's already selected" shortcut
                    // below even when pressed inside the current selection's bounds.
                    match self.hit_element(t, p, 4.0 / view.zoom, false) {
                        Some(id) if self.selection.remove(&id) => self.interaction = Interaction::Idle,
                        Some(id) => {
                            self.selection.insert(id);
                            self.interaction = Interaction::Move { start: p, delta: [0.0, 0.0] };
                        }
                        // Empty space: a lasso/rectangle that adds its catch to the selection.
                        None => self.interaction = Interaction::Lasso { pts: vec![p], extend: true, rect },
                    }
                    return;
                }
                let inside = self.selection_bounds(t).is_some_and(|b| b.expand(6.0 / view.zoom).contains(p));
                if inside {
                    self.interaction = Interaction::Move { start: p, delta: [0.0, 0.0] };
                } else if let Some(id) = self.hit_element(t, p, 4.0 / view.zoom, false) {
                    // Press on something unselected: pick it up and drag it right away.
                    self.selection = [id].into();
                    self.interaction = Interaction::Move { start: p, delta: [0.0, 0.0] };
                } else {
                    self.selection.clear();
                    self.interaction = Interaction::Lasso { pts: vec![p], extend: false, rect };
                }
            }
            Tool::Hand => {
                // A neutral pointer: grab and move whatever's under it, or pan empty
                // canvas. It never draws, and it never starts a lasso.
                let inside = self.selection_bounds(t).is_some_and(|b| b.expand(6.0 / view.zoom).contains(p));
                if inside {
                    self.interaction = Interaction::Move { start: p, delta: [0.0, 0.0] };
                } else if let Some(id) = self.hit_element(t, p, 4.0 / view.zoom, false) {
                    self.selection = [id].into();
                    self.interaction = Interaction::Move { start: p, delta: [0.0, 0.0] };
                } else if matches!(t, Target::Page(_)) {
                    // Empty canvas: pan. The screen overlay has no view to pan (it
                    // always paints at `View::default()`), so it just stays idle.
                    self.interaction = Interaction::Pan { last: pos };
                }
            }
            // Text boxes open on release: egui drops keyboard focus on the click that
            // ends a press, so a box opened on press would close immediately.
            Tool::Text => self.interaction = Interaction::TextAt(p),
            Tool::Shape(kind) => self.interaction = Interaction::Shape { kind, start: p, end: p },
        }
    }

    /// Edit the text box under `p`, or start a new one there.
    fn open_text_at(&mut self, t: Target, p: [f32; 2]) {
        if let Some(id) = self.hit_text(t, p) {
            self.begin_text_edit(t, id);
        } else {
            let snapshot = self.snapshot_of(t);
            let mut tb = self.text_template(p);
            tb.pos[1] -= tb.size * 0.6;
            let id = tb.id;
            self.page_mut(t).elements.push(Element::Text(tb));
            self.text_buf.clear();
            self.pending_snapshot = Some(snapshot);
            self.editing_text = Some((t, id));
            self.text_focus_pending = true;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn drag(&mut self, t: Target, view: View, origin: Pos2, pos: Pos2, time: f64, shift: bool) {
        let p = view.to_page(origin, pos);
        if self.crop.as_ref().is_some_and(|c| c.drag.is_some()) {
            self.drag_crop(p, view.zoom);
            return;
        }
        let mut inter = std::mem::replace(&mut self.interaction, Interaction::Idle);
        match &mut inter {
            Interaction::Idle => {}
            Interaction::Ink(b) => b.push(time, p, self.pressure()),
            Interaction::RightPress { start } => {
                if start.distance(pos) > 4.0 && matches!(self.mode, Mode::Notebook) {
                    // It's a drag: pan, counting from where the button went down.
                    self.view.offset += pos - *start;
                    inter = Interaction::Pan { last: pos };
                } else if start.distance(pos) > 4.0 {
                    // The screen overlay can't pan; a right-drag there erases.
                    self.pending_snapshot = Some(self.snapshot_of(t));
                    let r = self.config.eraser_size / 2.0 / view.zoom;
                    let from = view.to_page(origin, *start);
                    let changed = self.erase_at(t, from, r) | self.erase_along(t, from, p, r);
                    inter = Interaction::Erase { last: p, changed };
                }
            }
            Interaction::Erase { last, changed } => {
                let r = self.config.eraser_size / 2.0 / view.zoom;
                *changed |= self.erase_along(t, *last, p, r);
                *last = p;
            }
            Interaction::Resize { id, anchor, dir, orig, changed } => {
                let min = 8.0 / view.zoom;
                let w = ((p[0] - anchor[0]) * dir[0]).max(min);
                let h = ((p[1] - anchor[1]) * dir[1]).max(min);
                // Resize freely; Shift keeps the picture's original proportions.
                let (w, h) = if shift {
                    let s = (w / orig[0].max(1e-3)).max(h / orig[1].max(1e-3));
                    (orig[0] * s, orig[1] * s)
                } else {
                    (w, h)
                };
                let pos = [
                    if dir[0] > 0.0 { anchor[0] } else { anchor[0] - w },
                    if dir[1] > 0.0 { anchor[1] } else { anchor[1] - h },
                ];
                let id = *id;
                if let Some(Element::Image(img)) = self.page_mut(t).elements.iter_mut().find(|e| e.id() == id) {
                    img.pos = pos;
                    img.size = [w, h];
                    *changed = true;
                }
            }
            Interaction::TextResize { id, anchor_x, dir_x, changed } => {
                let tb = self.selected_text(t);
                let min = tb.as_ref().map_or(8.0, |tb| tb.min_wrap()).max(8.0 / view.zoom);
                let w = ((p[0] - *anchor_x) * *dir_x).max(min);
                let id = *id;
                if let Some(Element::Text(tb)) = self.page_mut(t).elements.iter_mut().find(|e| e.id() == id) {
                    if *dir_x < 0.0 {
                        tb.pos[0] = *anchor_x - w;
                    }
                    tb.wrap = Some(w);
                    *changed = true;
                }
            }
            Interaction::Lasso { pts, rect, .. } => {
                if *rect {
                    // Keep just the rectangle's four corners, start to current point.
                    let start = pts[0];
                    *pts = vec![start, [p[0], start[1]], p, [start[0], p[1]]];
                } else if pts.last().is_none_or(|l| dist(*l, p) > 2.0 / view.zoom) {
                    pts.push(p);
                }
            }
            Interaction::Move { start, delta } => *delta = [p[0] - start[0], p[1] - start[1]],
            Interaction::Shape { end, .. } => *end = p,
            Interaction::Pan { last } => {
                self.view.offset += pos - *last;
                *last = pos;
            }
            Interaction::TextAt(_) => {}
        }
        self.interaction = inter;
    }

    /// Shared by [`Interaction::Resize`] and [`Interaction::TextResize`]: commit the
    /// drag to history if it actually changed anything, and leave the element selected
    /// under its fresh id.
    fn finish_resize(&mut self, t: Target, id: u64, changed: bool) {
        let snapshot = self.pending_snapshot.take();
        if changed && let Some(s) = snapshot {
            self.history.push(s);
            self.touch();
            let page = self.page_mut(t);
            if let Some(i) = page.find(id) {
                page.elements[i].renew_id();
                self.selection = [page.elements[i].id()].into();
            }
        }
    }

    fn end(&mut self, t: Target, view: View, shift: bool) {
        if let Some(c) = &mut self.crop {
            c.drag = None;
        }
        match std::mem::replace(&mut self.interaction, Interaction::Idle) {
            Interaction::Idle | Interaction::Pan { .. } => {}
            Interaction::TextAt(p) => self.open_text_at(t, p),
            Interaction::RightPress { start } => self.open_context_menu(t, view, self.canvas_origin, start),
            Interaction::Resize { id, changed, .. } => self.finish_resize(t, id, changed),
            Interaction::TextResize { id, changed, .. } => self.finish_resize(t, id, changed),
            Interaction::Ink(b) => {
                let s = b.finish();
                if !s.points.is_empty() {
                    self.checkpoint(t);
                    self.page_mut(t).elements.push(Element::Stroke(s));
                }
            }
            Interaction::Erase { changed, .. } => {
                if let Some(s) = self.pending_snapshot.take()
                    && changed
                {
                    self.history.push(s);
                    self.touch();
                }
            }
            Interaction::Lasso { pts, extend, .. } => {
                if pts.len() < 3 {
                    // A click on empty space, or on a locked image.
                    if let Some(p) = pts.first()
                        && self.hit_element(t, *p, 4.0 / view.zoom, true).is_some()
                    {
                        self.toast("That image is locked — right-click it to unlock");
                    }
                    return;
                }
                let mut sel: HashSet<u64> = self
                    .page_ref(t)
                    .elements
                    .iter()
                    .filter(|e| !e.locked())
                    .filter(|e| match e {
                        Element::Stroke(s) => {
                            let inside = s.points.iter().filter(|q| point_in_polygon(q.pos(), &pts)).count();
                            inside * 2 >= s.points.len().max(1)
                        }
                        other => point_in_polygon(other.bounds().center(), &pts),
                    })
                    .map(Element::id)
                    .collect();
                // Shift started this lasso: add its catch to the selection instead of
                // replacing it.
                if extend {
                    sel.extend(self.selection.iter().copied());
                }
                self.selection = sel;
            }
            Interaction::Move { delta, .. } => {
                if delta[0].abs() + delta[1].abs() > 0.01 {
                    self.checkpoint(t);
                    let sel = std::mem::take(&mut self.selection);
                    let mut new_sel = HashSet::new();
                    for e in &mut self.page_mut(t).elements {
                        if sel.contains(&e.id()) {
                            e.translate(delta[0], delta[1]);
                            new_sel.insert(e.id());
                        }
                    }
                    self.selection = new_sel;
                }
            }
            Interaction::Shape { kind, start, end } => {
                let end = constrain(kind, start, end, shift);
                if dist(start, end) > 3.0 / view.zoom {
                    self.checkpoint(t);
                    let shape = Shape {
                        id: new_id(),
                        kind,
                        a: start,
                        b: end,
                        color: self.pen_color.with_alpha(255),
                        width: self.config.pen_width,
                    };
                    self.page_mut(t).elements.push(Element::Shape(shape));
                }
            }
        }
    }

    fn tool_input(&mut self, ui: &Ui, rect: Rect, t: Target) {
        if matches!(&self.mode, Mode::Overlay(o) if o.snip.is_some()) {
            return;
        }
        let ctx = ui.ctx().clone();
        let layer = ui.layer_id();
        let view = self.view_for(t);
        let origin = rect.min;
        self.canvas_origin = origin;
        let on_canvas = |pos: Pos2| rect.contains(pos) && ctx.layer_id_at(pos).is_none_or(|l| l == layer);
        let (events, now, mods, space) = ui.input(|i| (i.events.clone(), i.time, i.modifiers, i.key_down(Key::Space)));
        let moves = events.iter().filter(|e| matches!(e, Event::PointerMoved(_))).count().max(1);
        let t0 = self.last_sample_time.clamp(now - 0.05, now);
        let mut k = 0;
        for ev in events {
            match ev {
                Event::Touch { force: Some(f), phase, .. } => {
                    if matches!(phase, egui::TouchPhase::End | egui::TouchPhase::Cancel) {
                        self.pen_pressure = None;
                    } else {
                        self.pen_pressure = Some(f);
                    }
                }
                Event::PointerButton { pos, button, pressed: true, .. } => {
                    if self.context_menu.is_some() {
                        // The press that dismisses the menu doesn't draw.
                        if on_canvas(pos) {
                            self.context_menu = None;
                        }
                    } else if matches!(self.interaction, Interaction::Idle) && on_canvas(pos) {
                        self.begin(t, view, origin, pos, button, space, now, mods.shift);
                    }
                }
                Event::PointerMoved(pos) => {
                    k += 1;
                    let time = t0 + (now - t0) * k as f64 / moves as f64;
                    self.drag(t, view, origin, pos, time, mods.shift);
                }
                Event::PointerButton { pressed: false, .. } => {
                    self.end(t, view, mods.shift);
                    self.pen_pressure = None;
                }
                _ => {}
            }
        }
        self.last_sample_time = now;
        if !matches!(self.interaction, Interaction::Idle) {
            ctx.request_repaint();
        }
    }

    fn cursor(&self, ui: &Ui, rect: Rect, t: Target) {
        if matches!(&self.mode, Mode::Overlay(o) if o.snip.is_some()) {
            return;
        }
        let Some(pos) = ui.input(|i| i.pointer.hover_pos()) else { return };
        if !rect.contains(pos) || ui.ctx().layer_id_at(pos).is_some_and(|l| l != ui.layer_id()) {
            return;
        }
        let view = self.view_for(t);
        if self.crop.as_ref().is_some_and(|c| c.target == t) {
            let edge = self.crop.as_ref().and_then(|c| c.drag.map(|d| d.edge));
            let icon = match edge.or_else(|| self.crop_handle_at(view, rect.min, pos)) {
                Some([0, 0]) => CursorIcon::Move,
                Some([0, _]) => CursorIcon::ResizeVertical,
                Some([_, 0]) => CursorIcon::ResizeHorizontal,
                Some([x, y]) if x == y => CursorIcon::ResizeNwSe,
                Some(_) => CursorIcon::ResizeNeSw,
                None => CursorIcon::Default,
            };
            ui.ctx().set_cursor_icon(icon);
            return;
        }
        let painter = ui.painter_at(rect);
        let erasing = self.tool == Tool::Eraser || matches!(self.interaction, Interaction::Erase { .. });
        let ring = |r: f32, c: Color32| {
            painter.circle_stroke(pos, r + 1.0, EStroke::new(1.0, Color32::from_white_alpha(200)));
            painter.circle_stroke(pos, r, EStroke::new(1.0, c));
        };
        match self.tool {
            _ if matches!(self.interaction, Interaction::Pan { .. }) => ui.ctx().set_cursor_icon(CursorIcon::Grabbing),
            _ if erasing => {
                ui.ctx().set_cursor_icon(CursorIcon::None);
                ring(self.config.eraser_size / 2.0, Color32::from_gray(60));
            }
            Tool::Pen | Tool::Highlighter => {
                ui.ctx().set_cursor_icon(CursorIcon::None);
                let (c, w) = if self.tool == Tool::Pen {
                    (self.pen_color, self.config.pen_width)
                } else {
                    (self.highlighter_color, self.config.highlighter_width)
                };
                let r = (w * 0.5 * view.zoom).max(1.5);
                painter.circle_filled(pos, r, color32(c.with_alpha(c.a().max(160))));
                ring(r, Color32::from_black_alpha(120));
            }
            Tool::Text => ui.ctx().set_cursor_icon(CursorIcon::Text),
            Tool::Lasso | Tool::RectSelect => {
                let p = view.to_page(rect.min, pos);
                let handle = match &self.interaction {
                    Interaction::Resize { dir, .. } => Some(*dir),
                    _ => self.handle_at(t, view, rect.min, pos).map(|h| h.2),
                };
                let text_handle = matches!(self.interaction, Interaction::TextResize { .. })
                    || self.text_handle_at(t, view, rect.min, pos).is_some();
                let icon = if let Some(d) = handle {
                    if d[0] * d[1] > 0.0 { CursorIcon::ResizeNwSe } else { CursorIcon::ResizeNeSw }
                } else if text_handle {
                    CursorIcon::ResizeHorizontal
                } else if self.selection_bounds(t).is_some_and(|b| b.expand(6.0 / view.zoom).contains(p))
                    || self.hit_element(t, p, 4.0 / view.zoom, false).is_some()
                {
                    CursorIcon::Move
                } else {
                    CursorIcon::Crosshair
                };
                ui.ctx().set_cursor_icon(icon);
            }
            Tool::Hand => {
                let icon = if matches!(self.interaction, Interaction::Move { .. }) {
                    CursorIcon::Grabbing
                } else {
                    CursorIcon::Grab
                };
                ui.ctx().set_cursor_icon(icon);
            }
            Tool::Shape(_) => ui.ctx().set_cursor_icon(CursorIcon::Crosshair),
            Tool::Eraser => {}
        }
    }

    /// The whole canvas for target `t` inside `rect`.
    pub fn canvas(&mut self, ui: &mut Ui, rect: Rect, t: Target, canvas_bg: Color32) {
        let resp = ui.interact(rect, Id::unique("plexi-canvas"), Sense::click_and_drag());
        if matches!(t, Target::Page(_)) {
            self.fit_if_pending(rect);
            self.view_input(ui, rect, &resp);
        }
        self.tool_input(ui, rect, t);
        let double = ui.input(|i| {
            i.pointer.button_double_clicked(PointerButton::Primary)
                || i.pointer.button_triple_clicked(PointerButton::Primary)
        });
        if double && matches!(self.tool, Tool::Lasso | Tool::RectSelect) && self.crop.is_none() {
            // Double-click a text box with the select tool to edit it.
            let on_canvas = |q: &Pos2| rect.contains(*q) && ui.ctx().layer_id_at(*q).is_none_or(|l| l == ui.layer_id());
            let p =
                ui.input(|i| i.pointer.interact_pos()).filter(on_canvas).map(|q| self.view_for(t).to_page(rect.min, q));
            if let Some(id) = p.and_then(|p| self.hit_text(t, p)) {
                self.selection.clear();
                self.begin_text_edit(t, id);
            }
        }

        let view = self.view_for(t);
        let origin = rect.min;
        let moving = match &self.interaction {
            Interaction::Move { delta, .. } => Some(*delta),
            _ => None,
        };
        let skip = self.editing_text.filter(|(et, _)| *et == t).map(|(_, id)| id).or(self
            .crop
            .as_ref()
            .filter(|c| c.target == t)
            .map(|c| c.id));
        let sheets = self.sheets_for(t);
        {
            let input = PaintInput {
                page: match t {
                    Target::Page(i) => &self.doc.pages[i.min(self.doc.pages.len() - 1)],
                    Target::Overlay => &self.doc.overlay,
                },
                assets: &self.doc.assets,
                view,
                origin,
                clip: rect,
                paper: matches!(t, Target::Page(_)),
                skip,
                moving: moving.map(|d| (&self.selection, d)),
                canvas_bg,
                sheets: &sheets,
                sheet_lines: self.config.sheet_guides,
                sheet_numbers: self.config.sheet_numbers,
            };
            paint_page(ui, &input, &mut self.cache, &mut self.textures, &mut self.fonts);
        }
        self.paint_live(ui, rect, t);
        self.cursor(ui, rect, t);
        self.text_editor(ui, t);
    }

    /// Outlines of the sheets in use on an infinite page, cached per document revision.
    fn sheets_for(&mut self, t: Target) -> Vec<plexi_core::Rect> {
        let Target::Page(i) = t else { return Vec::new() };
        let i = i.min(self.doc.pages.len() - 1);
        let shown = self.config.sheet_guides || self.config.sheet_numbers;
        if !shown || self.doc.pages[i].size.dims().is_some() {
            return Vec::new();
        }
        let size = self.config.sheet_size();
        if let Some((rev, page, s, tiles)) = &self.sheet_cache
            && *rev == self.revision
            && *page == i
            && *s == size
        {
            return tiles.clone();
        }
        let tiles = plexi_core::export::layout::used_tiles(&self.doc.pages[i], size);
        self.sheet_cache = Some((self.revision, i, size, tiles.clone()));
        tiles
    }

    fn paint_live(&mut self, ui: &Ui, rect: Rect, t: Target) {
        let view = self.view_for(t);
        let origin = rect.min;
        let painter = ui.painter_at(rect);
        let accent = crate::ui::ACCENT;
        let shift = ui.input(|i| i.modifiers.shift);
        match &self.interaction {
            Interaction::Ink(b) if !b.stroke.points.is_empty() => {
                let m = ink::stroke_mesh(&b.stroke, view.zoom * ui.ctx().pixels_per_point());
                painter.add(EShape::mesh(to_egui_mesh(&m, view, origin, [0.0, 0.0], color32(b.stroke.color))));
            }
            Interaction::Shape { kind, start, end } => {
                let s = Shape {
                    id: 0,
                    kind: *kind,
                    a: *start,
                    b: constrain(*kind, *start, *end, shift),
                    color: self.pen_color.with_alpha(255),
                    width: self.config.pen_width,
                };
                paint_shape(&painter, &s, view, origin, [0.0, 0.0]);
            }
            Interaction::Lasso { pts, .. } if pts.len() > 1 => {
                let screen: Vec<Pos2> = pts.iter().map(|p| view.to_screen(origin, *p)).collect();
                let mut closed = screen.clone();
                closed.push(screen[0]);
                painter.extend(EShape::dashed_line(&closed, EStroke::new(1.5, accent), 6.0, 4.0));
            }
            _ => {}
        }
        if let Some(c) = self.crop.as_ref().filter(|c| c.target == t) {
            let asset = self.page_ref(t).elements.iter().find_map(|e| match e {
                Element::Image(i) if i.id == c.id => Some(i.asset.clone()),
                _ => None,
            });
            let full = view.rect_to_screen(origin, c.full);
            let shown = view.rect_to_screen(origin, c.shown);
            if let Some(tex) = asset.and_then(|a| self.textures.get(&a)) {
                let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
                painter.image(tex.id(), full, whole, Color32::from_white_alpha(110));
                let uv = |q: Pos2| pos2((q.x - full.left()) / full.width(), (q.y - full.top()) / full.height());
                painter.image(tex.id(), shown, Rect::from_min_max(uv(shown.min), uv(shown.max)), Color32::WHITE);
            }
            painter.rect_stroke(full, 0.0, EStroke::new(1.0, accent.gamma_multiply(0.5)), StrokeKind::Outside);
            painter.rect_stroke(shown, 0.0, EStroke::new(1.5, accent), StrokeKind::Outside);
            for e in CROP_EDGES {
                let size = if e[0] != 0 && e[1] != 0 { vec2(10.0, 10.0) } else { vec2(8.0, 8.0) };
                let h = Rect::from_center_size(edge_point(shown, e), size);
                painter.rect_filled(h, 2.0, Color32::WHITE);
                painter.rect_stroke(h, 2.0, EStroke::new(1.5, accent), StrokeKind::Inside);
            }
        }
        if let Some(b) = self.selection_bounds(t) {
            let d = match &self.interaction {
                Interaction::Move { delta, .. } => *delta,
                _ => [0.0, 0.0],
            };
            let r = view.rect_to_screen(origin, b).translate(vec2(d[0], d[1]) * view.zoom).expand(4.0);
            painter.rect_filled(r, 4.0, accent.gamma_multiply(0.06));
            let c = [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom(), r.left_top()];
            painter.extend(EShape::dashed_line(&c, EStroke::new(1.5, accent), 6.0, 4.0));
        }
        // Corner handles for resizing a single selected image.
        if let Some(img) = self.selected_image(t) {
            let d = match &self.interaction {
                Interaction::Move { delta, .. } => *delta,
                _ => [0.0, 0.0],
            };
            let r = view.rect_to_screen(
                origin,
                plexi_core::Rect::from_min_size([img.pos[0] + d[0], img.pos[1] + d[1]], img.size),
            );
            painter.rect_stroke(r, 0.0, EStroke::new(1.0, accent), StrokeKind::Outside);
            for c in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                let h = Rect::from_center_size(c, vec2(9.0, 9.0));
                painter.rect_filled(h, 2.0, Color32::WHITE);
                painter.rect_stroke(h, 2.0, EStroke::new(1.5, accent), StrokeKind::Inside);
            }
        }
        // Corner handles for dragging a selected text box's wrap width. Only their x
        // side matters (there is no height field), but they sit at the corners like an
        // image's so the two tools feel the same.
        if let Some(tb) = self.selected_text(t) {
            let d = match &self.interaction {
                Interaction::Move { delta, .. } => *delta,
                _ => [0.0, 0.0],
            };
            let b = tb.approx_bounds();
            let r = view.rect_to_screen(
                origin,
                plexi_core::Rect::from_min_size([b.min[0] + d[0], b.min[1] + d[1]], [b.width(), b.height()]),
            );
            for c in [r.left_top(), r.right_top(), r.right_bottom(), r.left_bottom()] {
                let h = Rect::from_center_size(c, vec2(9.0, 9.0));
                painter.rect_filled(h, 2.0, Color32::WHITE);
                painter.rect_stroke(h, 2.0, EStroke::new(1.5, accent), StrokeKind::Inside);
            }
        }
    }

    fn text_editor(&mut self, ui: &Ui, t: Target) {
        let Some((et, id)) = self.editing_text else { return };
        if et != t {
            return;
        }
        let Some(Element::Text(tb)) = self.page_ref(t).elements.iter().find(|e| e.id() == id).cloned() else {
            self.editing_text = None;
            return;
        };
        let view = self.view_for(t);
        let origin = ui.max_rect().min;
        let pos = view.to_screen(origin, tb.pos);
        let format = text_format(&mut self.fonts, ui.ctx(), &tb, view.zoom);
        let (_, shift) = text_layout(ui, &mut self.fonts, &tb, "", view.zoom);
        let color = color32(tb.color);
        let wrap_width = tb.wrap.map(|w| w * view.zoom);
        let measure = if self.text_buf.is_empty() { "Type…".to_owned() } else { format!("{}W", self.text_buf) };
        // A wrapped box keeps its own width instead of growing to fit what's typed.
        let width = wrap_width.unwrap_or_else(|| layout(ui, &measure, &format, None).size().x + 4.0);
        let ctx = ui.ctx().clone();
        let mut layouter =
            move |ui: &Ui, buf: &dyn egui::TextBuffer, _wrap: f32| layout(ui, buf.as_str(), &format, wrap_width);
        let area = egui::Area::new(Id::unique(("plexi-text", id))).fixed_pos(pos + shift).order(Order::Foreground);
        area.show(&ctx, |ui| {
            let edit = TextEdit::multiline(&mut self.text_buf)
                .layouter(&mut layouter)
                .text_color(color)
                .frame(egui::Frame::NONE)
                .margin(Margin::ZERO)
                .desired_rows(1)
                .desired_width(width)
                .hint_text("Type…");
            let r = ui.add(edit);
            // Keep typing into the box after a click on the toolbar (to change the font,
            // say). Pressing on the canvas or Esc ends the edit; another text field
            // (the font search) may take the keyboard meanwhile.
            let elsewhere = ui.ctx().memory(|m| m.focused().is_some_and(|f| f != r.id));
            if self.text_focus_pending || (!r.has_focus() && !elsewhere) {
                r.request_focus();
                self.text_focus_pending = false;
            }
            // A thin caret-colored guide so the box is findable on busy screens.
            let g = r.rect.expand(3.0);
            ui.painter().rect_stroke(
                g,
                3.0,
                EStroke::new(1.0, crate::ui::ACCENT.gamma_multiply(0.6)),
                StrokeKind::Outside,
            );
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exporters place text with `LINE_HEIGHT` and `BASELINE`; they must match how
    /// egui lays out the editor's font or exported text drifts from what was typed.
    #[test]
    fn text_metrics_match_the_editor_font() {
        let ctx = egui::Context::default();
        let mut row_h = 0.0;
        let mut baseline = 0.0;
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            let g = ui.painter().layout_no_wrap("Hg\nHg".into(), FontId::proportional(100.0), Color32::BLACK);
            row_h = g.rows[1].pos.y - g.rows[0].pos.y;
            baseline = g.rows[0].row.glyphs[0].pos.y;
        });
        out.textures_delta.clear();
        println!("row height {row_h}, baseline {baseline}");
        assert!((row_h / 100.0 - plexi_core::LINE_HEIGHT).abs() < 0.02, "row height {row_h}");
        assert!((baseline / 100.0 - plexi_core::BASELINE).abs() < 0.03, "baseline {baseline}");
    }
}
