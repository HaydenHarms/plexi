//! The SVG description of a page, which every visual exporter builds on.

use std::fmt::Write;

use base64::Engine;

use super::{Assets, RenderOptions};
use crate::geom::Rect;
use crate::ink::{PenPiece, pen_pieces};
use crate::model::*;

/// Font stack for text. "Ubuntu" is the editor's font and is bundled for rendering.
pub const FONT_FAMILY: &str = "Ubuntu, 'Segoe UI', Helvetica, Arial, sans-serif";

/// One mark of a page background, in page units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    HLine { y: f32, x0: f32, x1: f32 },
    VLine { x: f32, y0: f32, y1: f32 },
    Dot { x: f32, y: f32 },
}

/// Background marks covering `area`, aligned to the page origin so they line up across
/// renders. The editor calls this with the visible area, exporters with the whole page.
pub fn background_marks(bg: Background, area: Rect) -> Vec<Mark> {
    let s = RULE_SPACING;
    let first = |v: f32| (v / s).ceil() as i64;
    let last = |v: f32| (v / s).floor() as i64;
    let (ys, xs) = (first(area.min[1])..=last(area.max[1]), first(area.min[0])..=last(area.max[0]));
    // Refuse absurd counts (e.g. a zoomed-out infinite page) instead of stalling.
    if ys.clone().count() * xs.clone().count().max(1) > 400_000 {
        return Vec::new();
    }
    let mut out = Vec::new();
    match bg {
        Background::Blank => {}
        Background::Lined => {
            for k in ys {
                out.push(Mark::HLine { y: k as f32 * s, x0: area.min[0], x1: area.max[0] });
            }
        }
        Background::Grid => {
            for k in ys {
                out.push(Mark::HLine { y: k as f32 * s, x0: area.min[0], x1: area.max[0] });
            }
            for k in xs {
                out.push(Mark::VLine { x: k as f32 * s, y0: area.min[1], y1: area.max[1] });
            }
        }
        Background::Dots => {
            for ky in ys {
                for kx in xs.clone() {
                    out.push(Mark::Dot { x: kx as f32 * s, y: ky as f32 * s });
                }
            }
        }
    }
    out
}

/// Compact number formatting: two decimals, trailing zeros dropped.
pub(crate) fn n(v: f32) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

pub(crate) fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&apos;"),
            c if (c as u32) < 0x20 && c != '\n' && c != '\t' => {}
            c => o.push(c),
        }
    }
    o
}

fn opacity_attr(name: &str, c: Color) -> String {
    if c.a() == 255 { String::new() } else { format!(" {name}=\"{}\"", n(c.alpha_f32())) }
}

pub(crate) fn mime_of(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xff, 0xd8]) { "image/jpeg" } else { "image/png" }
}

pub(crate) fn data_uri(bytes: &[u8]) -> String {
    format!("data:{};base64,{}", mime_of(bytes), base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// Path data for the filled outline of a pen stroke (a union of discs and hulls, all
/// wound the same way so the default nonzero fill merges them).
pub fn pen_path_data(stroke: &Stroke) -> String {
    let mut d = String::new();
    for piece in pen_pieces(stroke) {
        match piece {
            PenPiece::Disc { c, r } => {
                let _ = write!(
                    d,
                    "M{} {}A{r} {r} 0 1 1 {} {}A{r} {r} 0 1 1 {} {}Z",
                    n(c[0] - r),
                    n(c[1]),
                    n(c[0] + r),
                    n(c[1]),
                    n(c[0] - r),
                    n(c[1]),
                    r = n(r)
                );
            }
            PenPiece::Hull(q) => {
                let _ = write!(
                    d,
                    "M{} {}L{} {}L{} {}L{} {}Z",
                    n(q[0][0]),
                    n(q[0][1]),
                    n(q[1][0]),
                    n(q[1][1]),
                    n(q[2][0]),
                    n(q[2][1]),
                    n(q[3][0]),
                    n(q[3][1])
                );
            }
        }
    }
    d
}

fn polyline_data(points: &[InkPoint]) -> String {
    let mut d = String::new();
    for (i, p) in points.iter().enumerate() {
        let _ = write!(d, "{}{} {}", if i == 0 { 'M' } else { 'L' }, n(p.x), n(p.y));
    }
    if points.len() == 1 {
        d.push_str("l0.01 0");
    }
    d
}

pub(crate) fn element_svg(out: &mut String, el: &Element, assets: &Assets, opts: &RenderOptions) {
    match el {
        Element::Stroke(s) if s.points.is_empty() => {}
        Element::Stroke(s) => match s.kind {
            InkKind::Pen => {
                let _ = writeln!(
                    out,
                    "<path fill=\"{}\"{} d=\"{}\"/>",
                    s.color.hex_rgb(),
                    opacity_attr("fill-opacity", s.color),
                    pen_path_data(s)
                );
            }
            InkKind::Highlighter => {
                let _ = writeln!(
                    out,
                    "<path fill=\"none\" stroke=\"{}\"{} stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\" d=\"{}\"/>",
                    s.color.hex_rgb(),
                    opacity_attr("stroke-opacity", s.color),
                    n(s.width),
                    polyline_data(&s.points)
                );
            }
        },
        Element::Text(t) if !opts.text || t.text.trim().is_empty() => {}
        Element::Text(t) => {
            let _ = write!(
                out,
                "<text font-family=\"{FONT_FAMILY}\" font-weight=\"300\" font-size=\"{}\" fill=\"{}\"{} xml:space=\"preserve\">",
                n(t.size),
                t.color.hex_rgb(),
                opacity_attr("fill-opacity", t.color),
            );
            for (i, line) in t.text.split('\n').enumerate() {
                let y = t.pos[1] + t.size * BASELINE + i as f32 * t.line_height();
                let _ = write!(out, "<tspan x=\"{}\" y=\"{}\">{}</tspan>", n(t.pos[0]), n(y), esc(line));
            }
            out.push_str("</text>\n");
        }
        Element::Image(img) => {
            if let Some(bytes) = assets.get(&img.asset) {
                let _ = writeln!(
                    out,
                    "<image x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" preserveAspectRatio=\"none\" xlink:href=\"{}\"/>",
                    n(img.pos[0]),
                    n(img.pos[1]),
                    n(img.size[0]),
                    n(img.size[1]),
                    data_uri(bytes)
                );
            }
        }
        Element::Shape(s) => {
            let stroke = format!(
                "fill=\"none\" stroke=\"{}\"{} stroke-width=\"{}\" stroke-linecap=\"round\" stroke-linejoin=\"round\"",
                s.color.hex_rgb(),
                opacity_attr("stroke-opacity", s.color),
                n(s.width)
            );
            let r = Rect::from_points([s.a, s.b]);
            match s.kind {
                ShapeKind::Line => {
                    let _ = writeln!(
                        out,
                        "<line x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\" {stroke}/>",
                        n(s.a[0]),
                        n(s.a[1]),
                        n(s.b[0]),
                        n(s.b[1])
                    );
                }
                ShapeKind::Arrow => {
                    let mut d = format!("M{} {}L{} {}", n(s.a[0]), n(s.a[1]), n(s.b[0]), n(s.b[1]));
                    if let Some([h1, h2]) = s.arrow_head() {
                        let _ = write!(
                            d,
                            "M{} {}L{} {}L{} {}",
                            n(h1[0]),
                            n(h1[1]),
                            n(s.b[0]),
                            n(s.b[1]),
                            n(h2[0]),
                            n(h2[1])
                        );
                    }
                    let _ = writeln!(out, "<path d=\"{d}\" {stroke}/>");
                }
                ShapeKind::Rect => {
                    let _ = writeln!(
                        out,
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" {stroke}/>",
                        n(r.min[0]),
                        n(r.min[1]),
                        n(r.width()),
                        n(r.height())
                    );
                }
                ShapeKind::Ellipse => {
                    let c = r.center();
                    let _ = writeln!(
                        out,
                        "<ellipse cx=\"{}\" cy=\"{}\" rx=\"{}\" ry=\"{}\" {stroke}/>",
                        n(c[0]),
                        n(c[1]),
                        n(r.width() / 2.0),
                        n(r.height() / 2.0)
                    );
                }
            }
        }
    }
}

pub(crate) fn background_svg(out: &mut String, page: &Page, b: Rect) {
    if page.paper.a() > 0 {
        let _ = writeln!(
            out,
            "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"{}/>",
            n(b.min[0]),
            n(b.min[1]),
            n(b.width()),
            n(b.height()),
            page.paper.hex_rgb(),
            opacity_attr("fill-opacity", page.paper)
        );
    }
    let marks = background_marks(page.background, b);
    if marks.is_empty() {
        return;
    }
    let c = RULE_COLOR;
    let mut lines = String::new();
    let mut dots = String::new();
    for m in marks {
        match m {
            Mark::HLine { y, x0, x1 } => {
                let _ = write!(lines, "M{} {}H{}", n(x0), n(y), n(x1));
            }
            Mark::VLine { x, y0, y1 } => {
                let _ = write!(lines, "M{} {}V{}", n(x), n(y0), n(y1));
            }
            Mark::Dot { x, y } => {
                let _ = write!(dots, "M{} {}a1.5 1.5 0 1 0 3 0a1.5 1.5 0 1 0 -3 0Z", n(x - 1.5), n(y));
            }
        }
    }
    if !lines.is_empty() {
        let _ = writeln!(
            out,
            "<path fill=\"none\" stroke=\"{}\" stroke-opacity=\"{}\" stroke-width=\"1\" d=\"{lines}\"/>",
            c.hex_rgb(),
            n(c.alpha_f32())
        );
    }
    if !dots.is_empty() {
        let _ = writeln!(out, "<path fill=\"{}\" fill-opacity=\"{}\" d=\"{dots}\"/>", c.hex_rgb(), n(c.alpha_f32()));
    }
}

/// A standalone SVG document for one page, sized in CSS pixels (= page units).
pub fn page_svg(page: &Page, assets: &Assets, opts: &RenderOptions) -> String {
    page_svg_in(page, assets, opts, page.export_bounds())
}

/// Like [`page_svg`] but covering an explicit area of the page.
pub fn page_svg_in(page: &Page, assets: &Assets, opts: &RenderOptions, b: Rect) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" xmlns:xlink=\"http://www.w3.org/1999/xlink\" width=\"{w}\" height=\"{h}\" viewBox=\"{} {} {w} {h}\">",
        n(b.min[0]),
        n(b.min[1]),
        w = n(b.width()),
        h = n(b.height()),
    );
    let _ = writeln!(out, "<title>{}</title>", esc(&page.name));
    if opts.background {
        background_svg(&mut out, page, b);
    }
    for el in &page.elements {
        element_svg(&mut out, el, assets, opts);
    }
    out.push_str("</svg>\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_compact() {
        assert_eq!(n(1.0), "1");
        assert_eq!(n(1.5), "1.5");
        assert_eq!(n(-0.001), "0");
        assert_eq!(n(2.345), "2.35");
    }

    #[test]
    fn lined_background_aligns_to_origin() {
        let marks = background_marks(Background::Lined, Rect::from_min_size([0.0, 10.0], [100.0, 60.0]));
        assert_eq!(marks.len(), 2); // y = 32 and 64
        assert!(matches!(marks[0], Mark::HLine { y, .. } if y == 32.0));
    }

    #[test]
    fn svg_parses() {
        let mut page = Page::new("p", PageSize::Letter);
        page.background = Background::Grid;
        page.elements.push(Element::Stroke(Stroke {
            id: 1,
            kind: InkKind::Pen,
            color: Color::BLACK,
            width: 3.0,
            points: vec![InkPoint::new(1.0, 1.0, 0.5), InkPoint::new(30.0, 40.0, 1.0)],
        }));
        let svg = page_svg(&page, &Assets::new(), &RenderOptions::default());
        assert!(crate::export::parse_svg(&svg).is_ok());
    }
}
