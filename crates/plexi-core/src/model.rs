//! The document model: a notebook of pages, each holding ink, text, images and shapes.
//!
//! Coordinates are in page units, which are CSS pixels (1/96 inch). A fixed-size page
//! such as Letter is 816 x 1056 units; an infinite page has no size.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use crate::geom::Rect;

/// Bumped whenever the on-disk layout changes in a way older builds cannot read.
pub const FORMAT_VERSION: u32 = 1;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Process-unique element id. Ids are not saved; every load assigns fresh ones. Anything
/// that changes an element's geometry gives it a new id so render caches stay valid.
pub fn new_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

fn is_zero_f32(v: &f32) -> bool {
    *v == 0.0
}

/// `p` turned by `angle` radians (clockwise on screen, where y points down) around `c`.
pub fn rotate_point(p: [f32; 2], c: [f32; 2], angle: f32) -> [f32; 2] {
    if angle == 0.0 {
        return p;
    }
    let (s, k) = angle.sin_cos();
    let (dx, dy) = (p[0] - c[0], p[1] - c[1]);
    [c[0] + dx * k - dy * s, c[1] + dx * s + dy * k]
}

/// Straight (not premultiplied) RGBA. Serialized as `#rrggbbaa`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(into = "String", try_from = "String")]
pub struct Color(pub [u8; 4]);

impl Color {
    pub const BLACK: Color = Color([0, 0, 0, 255]);
    pub const WHITE: Color = Color([255, 255, 255, 255]);
    pub const TRANSPARENT: Color = Color([0, 0, 0, 0]);

    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Color([r, g, b, 255])
    }

    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Color([r, g, b, a])
    }

    pub fn with_alpha(self, a: u8) -> Self {
        let [r, g, b, _] = self.0;
        Color([r, g, b, a])
    }

    pub fn r(self) -> u8 {
        self.0[0]
    }
    pub fn g(self) -> u8 {
        self.0[1]
    }
    pub fn b(self) -> u8 {
        self.0[2]
    }
    pub fn a(self) -> u8 {
        self.0[3]
    }

    /// Perceived brightness in 0..=1, ignoring alpha.
    pub fn luminance(self) -> f32 {
        (0.299 * self.0[0] as f32 + 0.587 * self.0[1] as f32 + 0.114 * self.0[2] as f32) / 255.0
    }

    pub fn alpha_f32(self) -> f32 {
        self.0[3] as f32 / 255.0
    }

    /// `#rrggbb`, ignoring alpha (SVG wants opacity separately).
    pub fn hex_rgb(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0[0], self.0[1], self.0[2])
    }

    pub fn hex_rgba(self) -> String {
        format!("#{:02x}{:02x}{:02x}{:02x}", self.0[0], self.0[1], self.0[2], self.0[3])
    }

    pub fn parse(s: &str) -> Option<Self> {
        let h = s.strip_prefix('#')?;
        let byte = |i: usize| u8::from_str_radix(h.get(i..i + 2)?, 16).ok();
        match h.len() {
            6 => Some(Color([byte(0)?, byte(2)?, byte(4)?, 255])),
            8 => Some(Color([byte(0)?, byte(2)?, byte(4)?, byte(6)?])),
            _ => None,
        }
    }
}

impl From<Color> for String {
    fn from(c: Color) -> String {
        if c.a() == 255 { c.hex_rgb() } else { c.hex_rgba() }
    }
}

impl TryFrom<String> for Color {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        Color::parse(&s).ok_or_else(|| format!("bad color {s:?}"))
    }
}

/// One sampled pen position. `p` is pressure in 0..=1 (0.5 for devices without pressure).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InkPoint {
    pub x: f32,
    pub y: f32,
    pub p: f32,
}

impl InkPoint {
    pub fn new(x: f32, y: f32, p: f32) -> Self {
        Self { x, y, p }
    }
    pub fn pos(self) -> [f32; 2] {
        [self.x, self.y]
    }
}

/// Points are stored as one flat `[x, y, p, x, y, p, ...]` array to keep files small.
mod flat_points {
    use super::InkPoint;
    use serde::{Deserialize, Deserializer, Serializer, ser::SerializeSeq};

    fn round(v: f32) -> f32 {
        (v * 100.0).round() / 100.0
    }

    pub fn serialize<S: Serializer>(pts: &[InkPoint], s: S) -> Result<S::Ok, S::Error> {
        let mut seq = s.serialize_seq(Some(pts.len() * 3))?;
        for p in pts {
            seq.serialize_element(&round(p.x))?;
            seq.serialize_element(&round(p.y))?;
            seq.serialize_element(&((p.p * 1000.0).round() / 1000.0))?;
        }
        seq.end()
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<InkPoint>, D::Error> {
        let flat = Vec::<f32>::deserialize(d)?;
        if flat.len() % 3 != 0 {
            return Err(serde::de::Error::custom("point array length is not a multiple of 3"));
        }
        Ok(flat.as_chunks::<3>().0.iter().map(|c| InkPoint::new(c[0], c[1], c[2])).collect())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum InkKind {
    /// Opaque, pressure-sensitive ink.
    #[default]
    Pen,
    /// Translucent, constant-width ink drawn as one flat layer.
    Highlighter,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Stroke {
    #[serde(skip, default = "new_id")]
    pub id: u64,
    #[serde(default)]
    pub kind: InkKind,
    pub color: Color,
    /// Nominal width in page units; pressure scales the pen around it.
    pub width: f32,
    #[serde(with = "flat_points")]
    pub points: Vec<InkPoint>,
    /// Elements sharing a nonzero group are selected and moved together.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub group: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TextBox {
    #[serde(skip, default = "new_id")]
    pub id: u64,
    /// Top-left corner of the first line.
    pub pos: [f32; 2],
    pub text: String,
    pub size: f32,
    pub color: Color,
    /// Font family; `None` is the editor's default font ([`DEFAULT_FONT`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub font: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub underline: bool,
    /// Width in page units the text wraps to, set by dragging a corner handle.
    /// `None` is the old behavior: one line per `\n`, sized to the longest of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<f32>,
    /// Turned this many radians (clockwise) around `pos`, its top-left corner.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub rotation: f32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub group: u64,
}

/// The font text boxes use unless another is picked. It is bundled, so it looks the same
/// everywhere; it has no bold or italic faces.
pub const DEFAULT_FONT: &str = "Ubuntu";

impl TextBox {
    /// A plain text box in the default font.
    pub fn new(pos: [f32; 2], text: impl Into<String>, size: f32, color: Color) -> Self {
        Self {
            id: new_id(),
            pos,
            text: text.into(),
            size,
            color,
            font: None,
            bold: false,
            italic: false,
            underline: false,
            wrap: None,
            rotation: 0.0,
            group: 0,
        }
    }

    pub fn family(&self) -> &str {
        self.font.as_deref().unwrap_or(DEFAULT_FONT)
    }

    /// Vertical distance between lines. Shared by the editor and every exporter.
    pub fn line_height(&self) -> f32 {
        self.size * LINE_HEIGHT
    }

    /// Average glyph advance as a fraction of the font size, used to estimate wrapping
    /// and bounds without a real font (exact widths depend on the font the renderer has).
    fn advance(&self) -> f32 {
        if self.bold { 0.6 } else { 0.55 }
    }

    /// The narrowest a dragged wrap width is allowed to shrink to: room for a couple
    /// of characters, so a box can never be squeezed down to nothing.
    pub fn min_wrap(&self) -> f32 {
        self.size * self.advance() * 2.0
    }

    /// Word-wrap one `\n`-separated line to [`Self::wrap`], estimating glyph width the
    /// same way [`Self::approx_bounds`] does. Shared by the editor's layout and every
    /// exporter (SVG has no native text wrapping) so a box wraps the same everywhere.
    /// Breaks on whitespace where it can; a single word wider than the wrap width is
    /// hard-broken at a character boundary instead, so no visual line ever overflows.
    pub fn wrap_line<'a>(&self, line: &'a str) -> Vec<&'a str> {
        let Some(w) = self.wrap else { return vec![line] };
        let max_chars = ((w / (self.size * self.advance()).max(1.0)).floor() as usize).max(1);
        if line.chars().count() <= max_chars {
            return vec![line];
        }
        let mut out = Vec::new();
        let mut start = 0usize; // byte offset where the current visual line begins
        let mut col = 0usize; // characters placed on it so far
        let mut last_space = None; // byte offset right after the most recent space on it
        for (i, c) in line.char_indices() {
            if col == max_chars {
                // This line is full before `c` joins it: break after the last space on
                // it, or hard-break right before `c` if there was none (one very long
                // word, broken by character instead of overflowing).
                let cut = last_space.filter(|&b| b > start).unwrap_or(i);
                out.push(line[start..cut].trim());
                start = cut;
                col = line[start..i].chars().count();
                last_space = None;
            }
            if c.is_whitespace() {
                last_space = Some(i + c.len_utf8());
            }
            col += 1;
        }
        out.push(line[start..].trim());
        out
    }

    /// Every visual line of the box: each `\n`-separated line, word-wrapped.
    pub fn wrapped_lines(&self) -> Vec<&str> {
        self.text.split('\n').flat_map(|l| self.wrap_line(l)).collect()
    }

    /// A rough extent. Exact glyph widths depend on the font the renderer has, so this
    /// uses an average advance; it is only used for hit testing and export bounds.
    pub fn approx_bounds(&self) -> Rect {
        let lines = self.wrapped_lines();
        let h = lines.len().max(1) as f32 * self.line_height();
        let w = match self.wrap {
            Some(w) => w,
            None => {
                let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1);
                widest as f32 * self.size * self.advance()
            }
        };
        Rect::from_min_size(self.pos, [w, h])
    }
}

/// Line height as a multiple of the font size.
pub const LINE_HEIGHT: f32 = 1.15;
/// Distance from the top of a line box to the text baseline, as a multiple of the font size.
pub const BASELINE: f32 = 0.93;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ImageEl {
    #[serde(skip, default = "new_id")]
    pub id: u64,
    /// Key into [`Document::assets`].
    pub asset: String,
    pub pos: [f32; 2],
    pub size: [f32; 2],
    /// Locked images can't be selected, moved or resized until unlocked (right-click).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// The part of the picture that shows, as fractions of its width and height:
    /// `[left, top, right, bottom]`. `None` shows all of it. `pos`/`size` are always the
    /// shown part.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<[f32; 4]>,
    /// Turned this many radians (clockwise) around its center.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub rotation: f32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub group: u64,
    /// The picture turned inside its frame (radians, clockwise, around the frame's
    /// center), while the frame itself stays put: straightening a cropped photo. On top
    /// of this the whole thing turns by `rotation`.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub inner_rotation: f32,
}

impl ImageEl {
    pub fn new(asset: String, pos: [f32; 2], size: [f32; 2]) -> Self {
        Self { id: new_id(), asset, pos, size, locked: false, crop: None, rotation: 0.0, group: 0, inner_rotation: 0.0 }
    }

    /// The crop as `[left, top, right, bottom]` fractions, the whole picture if uncropped.
    pub fn crop_or_full(&self) -> [f32; 4] {
        self.crop.unwrap_or([0.0, 0.0, 1.0, 1.0])
    }

    /// Where the whole, uncropped picture would be, in page units.
    pub fn full_rect(&self) -> Rect {
        let [l, t, r, b] = self.crop_or_full();
        let fw = self.size[0] / (r - l).max(1e-4);
        let fh = self.size[1] / (b - t).max(1e-4);
        Rect::from_min_size([self.pos[0] - l * fw, self.pos[1] - t * fh], [fw, fh])
    }

    /// The middle of the frame: what the picture turns around inside it.
    pub fn frame_center(&self) -> [f32; 2] {
        [self.pos[0] + self.size[0] / 2.0, self.pos[1] + self.size[1] / 2.0]
    }

    /// The corners of the whole picture, turned inside the frame by
    /// [`Self::inner_rotation`] (in the element's own, unturned frame).
    pub fn picture_quad(&self) -> [[f32; 2]; 4] {
        let f = self.full_rect();
        let c = self.frame_center();
        [f.min, [f.max[0], f.min[1]], f.max, [f.min[0], f.max[1]]].map(|p| rotate_point(p, c, self.inner_rotation))
    }

    /// What shows of the picture: the turned picture cut to the frame, as a polygon of
    /// (position, texture coordinate) pairs in the element's own, unturned frame.
    pub fn visible_polygon(&self) -> Vec<([f32; 2], [f32; 2])> {
        let frame = Rect::from_min_size(self.pos, self.size);
        picture_polygon(self.full_rect(), self.frame_center(), self.inner_rotation, frame)
    }

    /// Show only `shown` (page units) of the picture. Without a turn inside the frame it
    /// must be inside [`Self::full_rect`]; with one, the picture stays exactly where it
    /// is while the frame moves over it (corners past the picture show nothing).
    pub fn set_crop(&mut self, shown: Rect) {
        let mut full = self.full_rect();
        let turned = self.inner_rotation != 0.0;
        if turned {
            // The picture turns around the frame's center, which moves with the frame:
            // shift the unturned picture so the turned one doesn't move.
            let c = self.frame_center();
            let c2 = shown.center();
            let d = [c2[0] - c[0], c2[1] - c[1]];
            let back = rotate_point(d, [0.0, 0.0], -self.inner_rotation);
            let t = [d[0] - back[0], d[1] - back[1]];
            full =
                Rect { min: [full.min[0] + t[0], full.min[1] + t[1]], max: [full.max[0] + t[0], full.max[1] + t[1]] };
        }
        let keep = |v: f32| if turned { v } else { v.clamp(0.0, 1.0) };
        let fx = |x: f32| keep((x - full.min[0]) / full.width().max(1e-4));
        let fy = |y: f32| keep((y - full.min[1]) / full.height().max(1e-4));
        let c = [fx(shown.min[0]), fy(shown.min[1]), fx(shown.max[0]), fy(shown.max[1])];
        let whole = c[0].abs() < 1e-3 && c[1].abs() < 1e-3 && (c[2] - 1.0).abs() < 1e-3 && (c[3] - 1.0).abs() < 1e-3;
        self.crop = (!whole).then_some(c);
        self.pos = shown.min;
        self.size = [shown.width(), shown.height()];
    }

    /// Show the whole picture again, at the scale it is shown now. A picture turned
    /// inside its frame gets a frame around all of it.
    pub fn uncrop(&mut self) {
        if self.inner_rotation != 0.0 {
            self.set_crop(Rect::from_points(self.picture_quad()));
            return;
        }
        let full = self.full_rect();
        self.crop = None;
        self.pos = full.min;
        self.size = [full.width(), full.height()];
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ShapeKind {
    Line,
    Arrow,
    Rect,
    Ellipse,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Shape {
    #[serde(skip, default = "new_id")]
    pub id: u64,
    pub kind: ShapeKind,
    /// Start point (line/arrow) or one corner (rect/ellipse).
    pub a: [f32; 2],
    /// End point (line/arrow) or the opposite corner.
    pub b: [f32; 2],
    pub color: Color,
    pub width: f32,
    /// Rectangles and ellipses: turned this many radians (clockwise) around their
    /// center. Lines and arrows turn by moving their ends instead, so it stays 0.
    #[serde(default, skip_serializing_if = "is_zero_f32")]
    pub rotation: f32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub group: u64,
}

impl Shape {
    /// A shape with no rotation and no group.
    pub fn new(kind: ShapeKind, a: [f32; 2], b: [f32; 2], color: Color, width: f32) -> Self {
        Self { id: new_id(), kind, a, b, color, width, rotation: 0.0, group: 0 }
    }

    /// The two short strokes of an arrow head at `b`, in page units.
    pub fn arrow_head(&self) -> Option<[[f32; 2]; 2]> {
        let dx = self.b[0] - self.a[0];
        let dy = self.b[1] - self.a[1];
        let len = (dx * dx + dy * dy).sqrt();
        if len < 1e-3 {
            return None;
        }
        let (ux, uy) = (dx / len, dy / len);
        let head = (self.width * 4.0 + 6.0).min(len * 0.5);
        let (c, s) = (0.5f32.cos(), 0.5f32.sin()); // ~29 degrees each side
        let back = |sx: f32| {
            let rx = -ux * c - sx * -uy * s;
            let ry = -uy * c - sx * ux * s;
            [self.b[0] + rx * head, self.b[1] + ry * head]
        };
        Some([back(1.0), back(-1.0)])
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Element {
    Stroke(Stroke),
    Text(TextBox),
    Image(ImageEl),
    Shape(Shape),
}

impl Element {
    pub fn id(&self) -> u64 {
        match self {
            Element::Stroke(s) => s.id,
            Element::Text(t) => t.id,
            Element::Image(i) => i.id,
            Element::Shape(s) => s.id,
        }
    }

    pub fn locked(&self) -> bool {
        matches!(self, Element::Image(i) if i.locked)
    }

    /// Give the element a fresh id (after its geometry changed).
    pub fn renew_id(&mut self) {
        let id = new_id();
        match self {
            Element::Stroke(s) => s.id = id,
            Element::Text(t) => t.id = id,
            Element::Image(i) => i.id = id,
            Element::Shape(s) => s.id = id,
        }
    }

    /// The extent before any rotation: the element as if it weren't turned.
    pub fn local_bounds(&self) -> Rect {
        match self {
            Element::Stroke(s) => {
                let pad = s.width * 1.6 * 0.5 + 1.0;
                Rect::from_points(s.points.iter().map(|p| p.pos())).expand(pad)
            }
            Element::Text(t) => t.approx_bounds(),
            Element::Image(i) => Rect::from_min_size(i.pos, i.size),
            Element::Shape(s) => {
                let pad = match s.kind {
                    ShapeKind::Line | ShapeKind::Arrow => s.width * 4.0 + 6.0,
                    ShapeKind::Rect | ShapeKind::Ellipse => s.width * 0.5 + 1.0,
                };
                Rect::from_points([s.a, s.b]).expand(pad)
            }
        }
    }

    /// How far the element is turned (radians, clockwise), and the point it turns
    /// around. Ink and lines are turned by moving their points, so theirs is 0.
    pub fn rotation(&self) -> (f32, [f32; 2]) {
        match self {
            Element::Stroke(_) => (0.0, [0.0, 0.0]),
            Element::Text(t) => (t.rotation, t.pos),
            Element::Image(i) => (i.rotation, [i.pos[0] + i.size[0] / 2.0, i.pos[1] + i.size[1] / 2.0]),
            Element::Shape(s) => (s.rotation, Rect::from_points([s.a, s.b]).center()),
        }
    }

    /// The corners of [`Self::local_bounds`], turned with the element: its outline.
    pub fn outline(&self) -> [[f32; 2]; 4] {
        let b = self.local_bounds();
        let (a, c) = self.rotation();
        [b.min, [b.max[0], b.min[1]], b.max, [b.min[0], b.max[1]]].map(|p| rotate_point(p, c, a))
    }

    /// `p` (page units) in the element's own unturned frame: hit test it against the
    /// element as if it weren't rotated.
    pub fn to_local(&self, p: [f32; 2]) -> [f32; 2] {
        let (a, c) = self.rotation();
        rotate_point(p, c, -a)
    }

    pub fn bounds(&self) -> Rect {
        if self.rotation().0 == 0.0 {
            return self.local_bounds();
        }
        Rect::from_points(self.outline())
    }

    pub fn group(&self) -> u64 {
        match self {
            Element::Stroke(s) => s.group,
            Element::Text(t) => t.group,
            Element::Image(i) => i.group,
            Element::Shape(s) => s.group,
        }
    }

    pub fn set_group(&mut self, g: u64) {
        match self {
            Element::Stroke(s) => s.group = g,
            Element::Text(t) => t.group = g,
            Element::Image(i) => i.group = g,
            Element::Shape(s) => s.group = g,
        }
    }

    /// Turn the element by `angle` radians around `c`.
    pub fn rotate_about(&mut self, c: [f32; 2], angle: f32) {
        let turn = |p: &mut [f32; 2]| *p = rotate_point(*p, c, angle);
        match self {
            Element::Stroke(s) => {
                for p in &mut s.points {
                    let q = rotate_point([p.x, p.y], c, angle);
                    (p.x, p.y) = (q[0], q[1]);
                }
            }
            Element::Shape(s) if matches!(s.kind, ShapeKind::Line | ShapeKind::Arrow) => {
                turn(&mut s.a);
                turn(&mut s.b);
            }
            Element::Shape(s) => {
                let m = Rect::from_points([s.a, s.b]).center();
                let n = rotate_point(m, c, angle);
                for p in [&mut s.a, &mut s.b] {
                    p[0] += n[0] - m[0];
                    p[1] += n[1] - m[1];
                }
                s.rotation = tidy_angle(s.rotation + angle);
            }
            Element::Text(t) => {
                turn(&mut t.pos);
                t.rotation = tidy_angle(t.rotation + angle);
            }
            Element::Image(i) => {
                let m = [i.pos[0] + i.size[0] / 2.0, i.pos[1] + i.size[1] / 2.0];
                let n = rotate_point(m, c, angle);
                i.pos = [n[0] - i.size[0] / 2.0, n[1] - i.size[1] / 2.0];
                i.rotation = tidy_angle(i.rotation + angle);
            }
        }
        self.renew_id();
    }

    pub fn translate(&mut self, dx: f32, dy: f32) {
        match self {
            Element::Stroke(s) => {
                for p in &mut s.points {
                    p.x += dx;
                    p.y += dy;
                }
            }
            Element::Text(t) => {
                t.pos[0] += dx;
                t.pos[1] += dy;
            }
            Element::Image(i) => {
                i.pos[0] += dx;
                i.pos[1] += dy;
            }
            Element::Shape(s) => {
                s.a[0] += dx;
                s.a[1] += dy;
                s.b[0] += dx;
                s.b[1] += dy;
            }
        }
        self.renew_id();
    }

    /// Recolor ink, text and shapes; images are left alone.
    pub fn set_color(&mut self, color: Color) {
        match self {
            Element::Stroke(s) => {
                let a = if s.kind == InkKind::Highlighter { s.color.a() } else { 255 };
                s.color = color.with_alpha(a);
            }
            Element::Text(t) => t.color = color,
            Element::Shape(s) => s.color = color,
            Element::Image(_) => return,
        }
        self.renew_id();
    }
}

/// The picture `full` turned by `angle` around `pivot`, cut to `clip`: the polygon left,
/// as (position, texture coordinate) pairs. Texture coordinates run 0..1 across `full`.
pub fn picture_polygon(full: Rect, pivot: [f32; 2], angle: f32, clip: Rect) -> Vec<([f32; 2], [f32; 2])> {
    let mut poly: Vec<[f32; 2]> = [full.min, [full.max[0], full.min[1]], full.max, [full.min[0], full.max[1]]]
        .iter()
        .map(|p| rotate_point(*p, pivot, angle))
        .collect();
    // Sutherland–Hodgman against each side of the clip rectangle.
    let sides: [(usize, f32, bool); 4] =
        [(0, clip.min[0], true), (0, clip.max[0], false), (1, clip.min[1], true), (1, clip.max[1], false)];
    for (axis, edge, keep_above) in sides {
        let inside = |p: &[f32; 2]| if keep_above { p[axis] >= edge } else { p[axis] <= edge };
        let mut out = Vec::with_capacity(poly.len() + 2);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (ia, ib) = (inside(&a), inside(&b));
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (edge - a[axis]) / (b[axis] - a[axis]);
                out.push([a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t]);
            }
        }
        poly = out;
        if poly.is_empty() {
            break;
        }
    }
    let (w, h) = (full.width().max(1e-4), full.height().max(1e-4));
    poly.into_iter()
        .map(|p| {
            let q = rotate_point(p, pivot, -angle);
            (p, [(q[0] - full.min[0]) / w, (q[1] - full.min[1]) / h])
        })
        .collect()
}

/// An angle brought into -π..=π, and snapped to exactly 0 when it's within a hair of it
/// (so turning something back to straight leaves no rotation behind).
pub fn tidy_angle(a: f32) -> f32 {
    use std::f32::consts::{PI, TAU};
    let a = (a + PI).rem_euclid(TAU) - PI;
    if a.abs() < 1e-4 { 0.0 } else { a }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Background {
    #[default]
    Blank,
    Lined,
    Grid,
    Dots,
}

impl Background {
    pub const ALL: [Background; 4] = [Background::Blank, Background::Lined, Background::Grid, Background::Dots];
    pub fn label(self) -> &'static str {
        match self {
            Background::Blank => "Blank",
            Background::Lined => "Lined",
            Background::Grid => "Grid",
            Background::Dots => "Dots",
        }
    }
}

/// Spacing of background rules and grid, in page units.
pub const RULE_SPACING: f32 = 32.0;
/// Color of background rules, grid and dots on light paper.
pub const RULE_COLOR: Color = Color::rgba(120, 140, 170, 90);

/// Rule color that stays visible on `paper`: the usual blue-gray on light paper, a pale
/// one on dark paper.
pub fn rule_color(paper: Color) -> Color {
    if paper.a() > 0 && paper.luminance() < 0.45 { Color::rgba(205, 215, 235, 56) } else { RULE_COLOR }
}

/// Paper colors offered in the page menu.
pub const PAPER_COLORS: [(&str, Color); 8] = [
    ("White", Color::WHITE),
    ("Cream", Color::rgb(0xfb, 0xf5, 0xe6)),
    ("Gray", Color::rgb(0xee, 0xf0, 0xf3)),
    ("Mint", Color::rgb(0xe8, 0xf6, 0xee)),
    ("Sky", Color::rgb(0xe7, 0xf0, 0xfb)),
    ("Blush", Color::rgb(0xfb, 0xec, 0xee)),
    ("Slate", Color::rgb(0x2a, 0x2f, 0x38)),
    ("Black", Color::rgb(0x12, 0x13, 0x16)),
];

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum PageSize {
    Letter,
    A4,
    /// Fixed custom size in page units.
    Custom([f32; 2]),
    /// Grows with the content.
    Infinite,
}

impl PageSize {
    pub fn dims(self) -> Option<[f32; 2]> {
        match self {
            PageSize::Letter => Some([816.0, 1056.0]),
            PageSize::A4 => Some([793.7, 1122.5]),
            PageSize::Custom(d) => Some(d),
            PageSize::Infinite => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Page {
    pub name: String,
    pub size: PageSize,
    #[serde(default)]
    pub background: Background,
    /// Paper color. Fully transparent for overlay pages.
    pub paper: Color,
    #[serde(default)]
    pub elements: Vec<Element>,
}

impl Page {
    pub fn new(name: impl Into<String>, size: PageSize) -> Self {
        Self { name: name.into(), size, background: Background::Blank, paper: Color::WHITE, elements: Vec::new() }
    }

    /// The transparent page used by overlay mode, sized to the screen it covers.
    pub fn overlay(width: f32, height: f32) -> Self {
        Self {
            name: "Overlay".into(),
            size: PageSize::Custom([width, height]),
            background: Background::Blank,
            paper: Color::TRANSPARENT,
            elements: Vec::new(),
        }
    }

    pub fn content_bounds(&self) -> Option<Rect> {
        self.elements.iter().map(Element::bounds).reduce(|a, b| a.union(b))
    }

    /// The area an exporter should cover: the sheet for fixed pages, the content (plus a
    /// margin) for infinite ones.
    pub fn export_bounds(&self) -> Rect {
        match self.size.dims() {
            Some([w, h]) => Rect::from_min_size([0.0, 0.0], [w, h]),
            None => self
                .content_bounds()
                .map(|r| r.expand(24.0))
                .unwrap_or_else(|| Rect::from_min_size([0.0, 0.0], [816.0, 1056.0])),
        }
    }

    pub fn find(&self, id: u64) -> Option<usize> {
        self.elements.iter().position(|e| e.id() == id)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Document {
    pub version: u32,
    pub title: String,
    pub pages: Vec<Page>,
    /// Ink drawn in overlay mode, on top of the desktop.
    pub overlay: Page,
    /// Image bytes (PNG or JPEG) keyed by asset id. Stored as separate zip entries.
    #[serde(skip)]
    pub assets: BTreeMap<String, Arc<Vec<u8>>>,
}

impl Default for Document {
    fn default() -> Self {
        Self::new("Untitled")
    }
}

impl Document {
    pub fn new(title: impl Into<String>) -> Self {
        Self {
            version: FORMAT_VERSION,
            title: title.into(),
            pages: vec![Page::new("Page 1", PageSize::Infinite)],
            overlay: Page::overlay(1920.0, 1080.0),
            assets: BTreeMap::new(),
        }
    }

    /// Store image bytes and return their asset id. Identical bytes share one asset.
    pub fn add_asset(&mut self, bytes: Vec<u8>) -> String {
        let id = format!("{:016x}", fnv1a(&bytes));
        let ext = if bytes.starts_with(&[0xff, 0xd8]) { "jpg" } else { "png" };
        let key = format!("{id}.{ext}");
        self.assets.entry(key.clone()).or_insert_with(|| Arc::new(bytes));
        key
    }

    /// Give every group a fresh id, so groups read from a file can't share an id with
    /// one made in this session. Elements that were grouped together stay together.
    pub fn renew_groups(&mut self) {
        let mut map = std::collections::HashMap::new();
        for page in self.pages.iter_mut().chain(std::iter::once(&mut self.overlay)) {
            renew_groups(&mut page.elements, &mut map);
        }
    }

    /// Drop assets no element references any more.
    pub fn prune_assets(&mut self) {
        let used: std::collections::HashSet<&str> = self
            .pages
            .iter()
            .chain(std::iter::once(&self.overlay))
            .flat_map(|p| &p.elements)
            .filter_map(|e| match e {
                Element::Image(i) => Some(i.asset.as_str()),
                _ => None,
            })
            .collect();
        let used: std::collections::HashSet<String> = used.into_iter().map(str::to_owned).collect();
        self.assets.retain(|k, _| used.contains(k));
    }
}

/// Give the groups in `elements` fresh ids (the same fresh id for every member of a
/// group), reusing `map` across calls. Used on load and for pasted or duplicated copies,
/// which must not join the group they were copied from.
pub fn renew_groups(elements: &mut [Element], map: &mut std::collections::HashMap<u64, u64>) {
    for e in elements {
        let g = e.group();
        if g != 0 {
            e.set_group(*map.entry(g).or_insert_with(new_id));
        }
    }
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in bytes {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_round_trips_through_hex() {
        assert_eq!(Color::parse("#ff8000"), Some(Color::rgb(255, 128, 0)));
        assert_eq!(Color::parse("#ff800080"), Some(Color::rgba(255, 128, 0, 128)));
        assert_eq!(String::from(Color::rgba(1, 2, 3, 4)), "#01020304");
        assert_eq!(Color::parse("nope"), None);
    }

    #[test]
    fn stroke_points_serialize_flat() {
        let s = Element::Stroke(Stroke {
            id: 0,
            kind: InkKind::Pen,
            color: Color::BLACK,
            width: 3.0,
            points: vec![InkPoint::new(1.0, 2.0, 0.5), InkPoint::new(3.25, 4.0, 1.0)],
            group: 0,
        });
        assert!(!serde_json::to_string(&s).unwrap().contains("group"), "no group, nothing saved");
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"points\":[1.0,2.0,0.5,3.25,4.0,1.0]"), "{json}");
        let back: Element = serde_json::from_str(&json).unwrap();
        match back {
            Element::Stroke(b) => assert_eq!(b.points.len(), 2),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn wrapping_breaks_on_spaces_and_bounds_follow_it() {
        let mut t = TextBox::new([10.0, 20.0], "one two three four", 20.0, Color::BLACK);
        // Unwrapped: a single line, as wide as the whole text.
        assert_eq!(t.wrapped_lines(), vec!["one two three four"]);
        let natural = t.approx_bounds();
        assert_eq!(natural.height(), t.line_height());

        // Narrow it to a few words per line.
        t.wrap = Some(60.0);
        let lines = t.wrapped_lines();
        assert!(lines.len() > 1, "{lines:?}");
        assert!(lines.iter().all(|l| !l.is_empty() && !l.starts_with(' ') && !l.ends_with(' ')), "{lines:?}");
        assert_eq!(lines.join(" "), "one two three four", "wrapping only inserts breaks, never drops words");
        let wrapped = t.approx_bounds();
        assert_eq!(wrapped.width(), 60.0, "the box keeps the wrap width even if the text doesn't fill it");
        assert!(wrapped.height() > natural.height(), "more lines take more height");

        // A word with no spaces to break on is hard-broken by character instead of
        // overflowing the box.
        t.text = "a-very-long-unbreakable-word".into();
        let lines = t.wrap_line(&t.text);
        let max_chars = (t.wrap.unwrap() / (t.size * t.advance())).floor() as usize;
        assert!(lines.len() > 1, "{lines:?}");
        assert!(lines.iter().all(|l| l.chars().count() <= max_chars), "{lines:?}");
        assert_eq!(lines.concat(), t.text, "hard-breaking never drops characters");

        // `\n` splits still apply before wrapping each piece.
        t.wrap = None;
        t.text = "short\nline".into();
        assert_eq!(t.wrapped_lines(), vec!["short", "line"]);
    }

    #[test]
    fn cropping_keeps_the_picture_in_place() {
        let mut img = ImageEl::new("a.png".into(), [100.0, 100.0], [200.0, 100.0]);
        img.set_crop(Rect::from_min_size([150.0, 120.0], [100.0, 50.0]));
        assert_eq!(img.crop, Some([0.25, 0.2, 0.75, 0.7]));
        assert_eq!((img.pos, img.size), ([150.0, 120.0], [100.0, 50.0]));
        let full = img.full_rect();
        assert!((full.min[0] - 100.0).abs() < 1e-3 && (full.width() - 200.0).abs() < 1e-3, "{full:?}");
        // Cropping again works from the whole picture, not the current crop.
        img.set_crop(full);
        assert_eq!(img.crop, None);
        img.set_crop(Rect::from_min_size([100.0, 100.0], [100.0, 100.0]));
        img.uncrop();
        assert_eq!((img.pos, img.size, img.crop), ([100.0, 100.0], [200.0, 100.0], None));
    }

    #[test]
    fn rotating_turns_every_kind_of_element_around_a_point() {
        use std::f32::consts::FRAC_PI_2;
        let c = [0.0, 0.0];
        // A quarter turn clockwise (y down): +x goes to +y.
        let q = rotate_point([10.0, 0.0], c, FRAC_PI_2);
        assert!((q[0]).abs() < 1e-4 && (q[1] - 10.0).abs() < 1e-4, "{q:?}");

        let mut img = Element::Image(ImageEl::new("a.png".into(), [10.0, -5.0], [20.0, 10.0]));
        img.rotate_about(c, FRAC_PI_2);
        let Element::Image(i) = &img else { unreachable!() };
        assert!((i.rotation - FRAC_PI_2).abs() < 1e-5);
        // Its center (20, 0) went to (0, 20), size unchanged.
        assert!((i.pos[0] + 10.0).abs() < 1e-3 && (i.pos[1] - 15.0).abs() < 1e-3, "{:?}", i.pos);
        // Turned on its side, its bounds are 10 wide and 20 tall.
        let b = img.bounds();
        assert!((b.width() - 10.0).abs() < 1e-3 && (b.height() - 20.0).abs() < 1e-3, "{b:?}");
        // A point on the turned image is inside it in its own frame.
        let local = img.to_local([0.0, 28.0]);
        assert!(img.local_bounds().contains(local), "{local:?}");

        let mut line = Element::Shape(Shape::new(ShapeKind::Line, [0.0, 0.0], [10.0, 0.0], Color::BLACK, 2.0));
        line.rotate_about(c, FRAC_PI_2);
        let Element::Shape(s) = &line else { unreachable!() };
        assert_eq!(s.rotation, 0.0, "lines turn by moving their ends");
        assert!((s.b[1] - 10.0).abs() < 1e-4);

        // Four quarter turns come back to no rotation at all.
        for _ in 0..3 {
            img.rotate_about(c, FRAC_PI_2);
        }
        let Element::Image(i) = &img else { unreachable!() };
        assert_eq!(i.rotation, 0.0);
    }

    #[test]
    fn a_picture_turns_inside_its_frame_and_stays_put_when_recropped() {
        let mut img = ImageEl::new("a.png".into(), [0.0, 0.0], [200.0, 100.0]);
        img.set_crop(Rect::from_min_size([50.0, 25.0], [100.0, 50.0]));
        img.inner_rotation = 0.3;
        let before = img.picture_quad();
        // The frame stays the frame; what shows is cut to it.
        let poly = img.visible_polygon();
        assert!(poly.len() >= 4);
        assert!(poly.iter().all(|(p, _)| p[0] >= 49.99 && p[0] <= 150.01 && p[1] >= 24.99 && p[1] <= 75.01));
        assert!(poly.iter().all(|(_, uv)| (-1e-3..=1.001).contains(&uv[0]) && (-1e-3..=1.001).contains(&uv[1])));
        // Moving the frame doesn't move the turned picture under it.
        img.set_crop(Rect::from_min_size([70.0, 20.0], [60.0, 40.0]));
        let after = img.picture_quad();
        for (a, b) in before.iter().zip(after) {
            assert!((a[0] - b[0]).abs() < 1e-2 && (a[1] - b[1]).abs() < 1e-2, "{a:?} vs {b:?}");
        }
        // Uncropping frames all of the turned picture.
        img.uncrop();
        let quad = Rect::from_points(img.picture_quad());
        assert!(
            (quad.width() - img.size[0]).abs() < 0.1 && (quad.min[0] - img.pos[0]).abs() < 0.1,
            "{quad:?} {:?} {:?} {:?}",
            img.pos,
            img.size,
            img.crop
        );
    }

    #[test]
    fn groups_survive_saving_but_get_fresh_ids() {
        let mut d = Document::new("t");
        for x in [0.0, 50.0, 100.0] {
            let mut s = Shape::new(ShapeKind::Rect, [x, 0.0], [x + 10.0, 10.0], Color::BLACK, 1.0);
            s.group = if x < 90.0 { 7 } else { 0 };
            d.pages[0].elements.push(Element::Shape(s));
        }
        d.renew_groups();
        let g: Vec<u64> = d.pages[0].elements.iter().map(Element::group).collect();
        assert!(g[0] != 0 && g[0] != 7 && g[0] == g[1] && g[2] == 0, "{g:?}");
    }

    #[test]
    fn identical_assets_are_shared() {
        let mut d = Document::new("t");
        let a = d.add_asset(vec![1, 2, 3]);
        let b = d.add_asset(vec![1, 2, 3]);
        assert_eq!(a, b);
        assert_eq!(d.assets.len(), 1);
        d.prune_assets();
        assert!(d.assets.is_empty());
    }
}
