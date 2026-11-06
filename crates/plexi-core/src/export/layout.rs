//! Where a page lands on the output: which part of the page, on what size of sheet,
//! and whether it is shrunk onto one sheet or split across several.
//!
//! Every visual exporter turns a page into a list of [`Frame`]s, one per output page or
//! file, and renders each frame the same way, so what the export dialog previews is
//! exactly what gets written.

use crate::geom::Rect;
use crate::model::Page;

/// Standard sheet sizes in page units (1/96 inch), portrait.
pub const PAPERS: [(&str, [f32; 2]); 7] = [
    ("Letter", [816.0, 1056.0]),
    ("A4", [793.7, 1122.5]),
    ("Legal", [816.0, 1344.0]),
    ("Tabloid", [1056.0, 1632.0]),
    ("A5", [559.4, 793.7]),
    ("A3", [1122.5, 1587.4]),
    ("Slide 16:9", [720.0, 1280.0]),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Area {
    /// The sheet of a fixed-size page; everything drawn on an infinite one.
    #[default]
    Page,
    /// Only what is drawn, trimmed tight (plus the margin).
    Content,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    pub area: Area,
    /// Output sheet size in page units. `None` makes each output exactly the area's size.
    pub sheet: Option<[f32; 2]>,
    /// Split the area across as many sheets as it needs at full size, instead of
    /// shrinking it onto one. Sheets line up with the page origin, like the sheet guides
    /// on an infinite canvas, and empty ones are skipped.
    pub tile: bool,
    /// Blank space around the content, in page units.
    pub margin: f32,
}

impl Default for Layout {
    fn default() -> Self {
        Self { area: Area::Page, sheet: None, tile: false, margin: 0.0 }
    }
}

/// One output page: the part of the page it shows (`view`, in page units) and the size
/// it is written at (`size`, in page units, so 1:1 means the same physical size).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Frame {
    pub view: Rect,
    pub size: [f32; 2],
}

impl Frame {
    pub fn exact(view: Rect) -> Self {
        Self { view, size: [view.width().max(1.0), view.height().max(1.0)] }
    }

    /// Output units per page unit.
    pub fn scale(&self) -> f32 {
        self.size[0] / self.view.width().max(1e-3)
    }
}

/// The part of `page` an export covers before it is placed on sheets.
pub fn area_rect(page: &Page, area: Area) -> Rect {
    match area {
        Area::Page => page.export_bounds(),
        Area::Content => page.content_bounds().filter(|b| !b.is_empty()).unwrap_or_else(|| page.export_bounds()),
    }
}

/// Sheets of `tile` size, aligned to the page origin, that hold something drawn on
/// `page`, in reading order (rows top to bottom, then left to right).
pub fn used_tiles(page: &Page, tile: [f32; 2]) -> Vec<Rect> {
    let (tw, th) = (tile[0].max(16.0), tile[1].max(16.0));
    let mut cells = std::collections::BTreeSet::new();
    for el in &page.elements {
        let b = el.bounds();
        if b.is_empty() {
            continue;
        }
        // Ignore the soft padding around ink so a stroke that just touches an edge
        // doesn't claim the neighboring sheet.
        let b = b.expand(-0.5_f32.min(b.width().min(b.height()) / 2.0));
        let (x0, x1) = ((b.min[0] / tw).floor() as i64, (b.max[0] / tw).floor() as i64);
        let (y0, y1) = ((b.min[1] / th).floor() as i64, (b.max[1] / th).floor() as i64);
        if (x1 - x0 + 1) * (y1 - y0 + 1) > 10_000 {
            continue; // absurdly large element; don't stall
        }
        for y in y0..=y1 {
            for x in x0..=x1 {
                cells.insert((y, x));
            }
        }
    }
    cells.into_iter().map(|(y, x)| Rect::from_min_size([x as f32 * tw, y as f32 * th], [tw, th])).collect()
}

/// The output pages for `page` under `layout`.
pub fn frames(page: &Page, layout: &Layout) -> Vec<Frame> {
    let area = area_rect(page, layout.area);
    let m = layout.margin.max(0.0);
    let Some([w, h]) = layout.sheet else {
        return vec![Frame::exact(area.expand(m))];
    };
    if layout.tile {
        let tiles: Vec<Rect> = used_tiles(page, [w, h]).into_iter().filter(|t| t.intersects(&area)).collect();
        if tiles.is_empty() {
            return vec![Frame::exact(Rect::from_min_size(area.min, [w, h]))];
        }
        return tiles.into_iter().map(Frame::exact).collect();
    }
    // Shrink to fit one sheet (never enlarge), centered across and starting at the top.
    let (iw, ih) = ((w - 2.0 * m).max(1.0), (h - 2.0 * m).max(1.0));
    let s = (iw / area.width().max(1.0)).min(ih / area.height().max(1.0)).min(1.0);
    let (vw, vh) = (w / s, h / s);
    let c = area.center();
    let top = if area.height() * s < ih { area.min[1] - m / s } else { c[1] - vh / 2.0 };
    vec![Frame { view: Rect::from_min_size([c[0] - vw / 2.0, top], [vw, vh]), size: [w, h] }]
}

/// Parse a page list like `1, 3, 5-7` (1-based, as people count pages) into sorted
/// 0-based indices below `count`.
pub fn parse_page_list(text: &str, count: usize) -> Result<Vec<usize>, String> {
    let mut out = std::collections::BTreeSet::new();
    for part in text.split([',', ';', ' ']).map(str::trim).filter(|p| !p.is_empty()) {
        let num = |s: &str| -> Result<usize, String> {
            s.trim().parse::<usize>().map_err(|_| format!("“{part}” isn't a page number"))
        };
        let (a, b) = match part.split_once(['-', '–']) {
            Some((a, b)) if a.trim().is_empty() => (1, num(b)?),
            Some((a, b)) if b.trim().is_empty() => (num(a)?, count),
            Some((a, b)) => (num(a)?, num(b)?),
            None => (num(part)?, num(part)?),
        };
        if a == 0 || b == 0 {
            return Err("Pages start at 1".into());
        }
        if a.max(b) > count {
            return Err(format!("There {} only {count}", if count == 1 { "is" } else { "are" }));
        }
        out.extend(a.min(b) - 1..a.max(b));
    }
    if out.is_empty() {
        return Err("Type page numbers, e.g. 1, 3, 5-7".into());
    }
    Ok(out.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn dot(x: f32, y: f32) -> Element {
        Element::Shape(Shape {
            id: new_id(),
            kind: ShapeKind::Rect,
            a: [x, y],
            b: [x + 10.0, y + 10.0],
            color: Color::BLACK,
            width: 0.0,
        })
    }

    #[test]
    fn page_lists_parse() {
        assert_eq!(parse_page_list("1,3,5", 6), Ok(vec![0, 2, 4]));
        assert_eq!(parse_page_list(" 2-4, 1 ", 6), Ok(vec![0, 1, 2, 3]));
        assert_eq!(parse_page_list("5-", 6), Ok(vec![4, 5]));
        assert_eq!(parse_page_list("4-2", 6), Ok(vec![1, 2, 3]));
        assert!(parse_page_list("7", 6).is_err());
        assert!(parse_page_list("0", 6).is_err());
        assert!(parse_page_list("two", 6).is_err());
        assert!(parse_page_list("", 6).is_err());
    }

    #[test]
    fn default_layout_is_the_page() {
        let page = Page::new("p", PageSize::Letter);
        assert_eq!(
            frames(&page, &Layout::default()),
            vec![Frame::exact(Rect::from_min_size([0.0, 0.0], [816.0, 1056.0]))]
        );
    }

    #[test]
    fn tiles_follow_the_content() {
        let mut page = Page::new("p", PageSize::Infinite);
        page.elements.push(dot(100.0, 100.0));
        page.elements.push(dot(900.0, 100.0)); // the sheet to the right
        page.elements.push(dot(100.0, -500.0)); // the sheet above
        let tiles = used_tiles(&page, [816.0, 1056.0]);
        assert_eq!(tiles.len(), 3);
        assert_eq!(tiles[0].min, [0.0, -1056.0]); // reading order: the row above first
        assert_eq!(tiles[2].min, [816.0, 0.0]);
        let layout = Layout { sheet: Some([816.0, 1056.0]), tile: true, ..Default::default() };
        assert_eq!(frames(&page, &layout).len(), 3);
    }

    #[test]
    fn fit_shrinks_wide_content_onto_one_sheet() {
        let mut page = Page::new("p", PageSize::Infinite);
        page.elements.push(dot(0.0, 0.0));
        page.elements.push(dot(3000.0, 0.0));
        let layout = Layout { area: Area::Content, sheet: Some([816.0, 1056.0]), tile: false, margin: 48.0 };
        let f = frames(&page, &layout);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].size, [816.0, 1056.0]);
        assert!(f[0].scale() < 0.3);
        let area = area_rect(&page, Area::Content);
        assert!(f[0].view.min[0] < area.min[0] && f[0].view.max[0] > area.max[0]);
    }

    #[test]
    fn fit_never_enlarges() {
        let mut page = Page::new("p", PageSize::Infinite);
        page.elements.push(dot(0.0, 0.0));
        let layout = Layout { area: Area::Content, sheet: Some([816.0, 1056.0]), ..Default::default() };
        assert_eq!(frames(&page, &layout)[0].scale(), 1.0);
    }
}
