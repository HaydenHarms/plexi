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
        }
    }

    pub fn family(&self) -> &str {
        self.font.as_deref().unwrap_or(DEFAULT_FONT)
    }

    /// Vertical distance between lines. Shared by the editor and every exporter.
    pub fn line_height(&self) -> f32 {
        self.size * LINE_HEIGHT
    }

    /// A rough extent. Exact glyph widths depend on the font the renderer has, so this
    /// uses an average advance; it is only used for hit testing and export bounds.
    pub fn approx_bounds(&self) -> Rect {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let widest = lines.iter().map(|l| l.chars().count()).max().unwrap_or(0).max(1);
        let advance = if self.bold { 0.6 } else { 0.55 };
        let w = widest as f32 * self.size * advance;
        let h = lines.len().max(1) as f32 * self.line_height();
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
}

impl ImageEl {
    pub fn new(asset: String, pos: [f32; 2], size: [f32; 2]) -> Self {
        Self { id: new_id(), asset, pos, size, locked: false, crop: None }
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

    /// Show only `shown` (page units, inside [`Self::full_rect`]) of the picture.
    pub fn set_crop(&mut self, shown: Rect) {
        let full = self.full_rect();
        let fx = |x: f32| ((x - full.min[0]) / full.width().max(1e-4)).clamp(0.0, 1.0);
        let fy = |y: f32| ((y - full.min[1]) / full.height().max(1e-4)).clamp(0.0, 1.0);
        let c = [fx(shown.min[0]), fy(shown.min[1]), fx(shown.max[0]), fy(shown.max[1])];
        let whole = c[0] < 1e-3 && c[1] < 1e-3 && c[2] > 1.0 - 1e-3 && c[3] > 1.0 - 1e-3;
        self.crop = (!whole).then_some(c);
        self.pos = shown.min;
        self.size = [shown.width(), shown.height()];
    }

    /// Show the whole picture again, at the scale it is shown now.
    pub fn uncrop(&mut self) {
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
}

impl Shape {
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

    pub fn bounds(&self) -> Rect {
        match self {
            Element::Stroke(s) => {
                let pad = s.width * 1.6 * 0.5 + 1.0;
                Rect::from_points(s.points.iter().map(|p| p.pos())).expand(pad)
            }
            Element::Text(t) => t.approx_bounds(),
            Element::Image(i) => Rect::from_min_size(i.pos, i.size),
            Element::Shape(s) => Rect::from_points([s.a, s.b]).expand(s.width * 4.0 + 6.0),
        }
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
        });
        let json = serde_json::to_string(&s).unwrap();
        assert!(json.contains("\"points\":[1.0,2.0,0.5,3.25,4.0,1.0]"), "{json}");
        let back: Element = serde_json::from_str(&json).unwrap();
        match back {
            Element::Stroke(b) => assert_eq!(b.points.len(), 2),
            _ => panic!("wrong variant"),
        }
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
