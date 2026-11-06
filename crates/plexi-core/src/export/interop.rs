//! Formats other tools open: Excalidraw, draw.io, InkML, and Plexi's own JSON.

use std::fmt::Write;

use base64::Engine;
use serde_json::{Value, json};

use super::{Assets, ExportError, RenderOptions, svg};
use crate::geom::Rect;
use crate::model::*;

/// Vertical gap between pages when several are laid out on one canvas.
const PAGE_GAP: f32 = 80.0;

/// Page offsets that stack pages top to bottom, each page's export area starting at x = 0.
fn stacked(pages: &[&Page]) -> Vec<(Rect, [f32; 2])> {
    let mut y = 0.0;
    pages
        .iter()
        .map(|p| {
            let b = p.export_bounds();
            let off = [-b.min[0], y - b.min[1]];
            y += b.height() + PAGE_GAP;
            (b, off)
        })
        .collect()
}

/// Excalidraw's image crop, in the picture's own pixels.
fn excalidraw_crop(img: &ImageEl, bytes: &[u8]) -> Value {
    let Some([l, t, r, b]) = img.crop else { return Value::Null };
    let dims = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()
        .and_then(|r| r.into_dimensions().ok());
    let Some((w, h)) = dims else { return Value::Null };
    let (w, h) = (w as f32, h as f32);
    json!({
        "x": l * w, "y": t * h, "width": (r - l) * w, "height": (b - t) * h,
        "naturalWidth": w, "naturalHeight": h,
    })
}

pub fn excalidraw(pages: &[&Page], assets: &Assets) -> Result<String, ExportError> {
    let mut elements = Vec::new();
    let mut files = serde_json::Map::new();
    let mut seq = 0u64;
    let mut next = || {
        seq += 1;
        seq
    };
    let base = |id: String, ty: &str, x: f32, y: f32, w: f32, h: f32, color: Color, stroke_width: f32, seed: u64| {
        json!({
            "id": id, "type": ty, "x": x, "y": y, "width": w, "height": h, "angle": 0,
            "strokeColor": color.hex_rgb(), "backgroundColor": "transparent", "fillStyle": "solid",
            "strokeWidth": stroke_width, "strokeStyle": "solid", "roughness": 0,
            "opacity": (color.alpha_f32() * 100.0).round(), "groupIds": [], "frameId": null,
            "roundness": null, "seed": seed, "version": 1, "versionNonce": seed,
            "isDeleted": false, "boundElements": null, "updated": 1, "link": null, "locked": false,
        })
    };
    let merge = |mut v: Value, extra: Value| {
        if let (Some(o), Some(e)) = (v.as_object_mut(), extra.as_object()) {
            for (k, val) in e {
                o.insert(k.clone(), val.clone());
            }
        }
        v
    };

    for (page, (_, off)) in pages.iter().zip(stacked(pages)) {
        if let Some([w, h]) = page.size.dims() {
            let n = next();
            // A light outline so the page edges survive on the infinite canvas.
            elements.push(merge(
                base(
                    format!("plexi-page-{n}"),
                    "rectangle",
                    off[0],
                    off[1],
                    w,
                    h,
                    Color::rgba(160, 170, 190, 255),
                    1.0,
                    n,
                ),
                json!({ "strokeStyle": "dashed" }),
            ));
        }
        for el in &page.elements {
            let n = next();
            let id = format!("plexi-{n}");
            match el {
                Element::Stroke(s) if !s.points.is_empty() => {
                    let r = Rect::from_points(s.points.iter().map(|p| p.pos()));
                    let (x0, y0) = (s.points[0].x, s.points[0].y);
                    let pts: Vec<[f32; 2]> = s.points.iter().map(|p| [p.x - x0, p.y - y0]).collect();
                    let pressures: Vec<f32> = s.points.iter().map(|p| p.p).collect();
                    let sw = (s.width / 4.0).max(0.5);
                    elements.push(merge(
                        base(id, "freedraw", x0 + off[0], y0 + off[1], r.width(), r.height(), s.color, sw, n),
                        json!({
                            "points": pts,
                            "pressures": if s.kind == InkKind::Pen { pressures } else { vec![] },
                            "simulatePressure": false,
                            "lastCommittedPoint": null,
                        }),
                    ));
                }
                Element::Stroke(_) => {}
                Element::Text(t) => {
                    let b = t.approx_bounds();
                    elements.push(merge(
                        base(id, "text", t.pos[0] + off[0], t.pos[1] + off[1], b.width(), b.height(), t.color, 1.0, n),
                        json!({
                            "text": t.text, "originalText": t.text, "fontSize": t.size, "fontFamily": 2,
                            "textAlign": "left", "verticalAlign": "top", "containerId": null,
                            "lineHeight": LINE_HEIGHT, "autoResize": true,
                        }),
                    ));
                }
                Element::Image(img) => {
                    let Some(bytes) = assets.get(&img.asset) else { continue };
                    let file_id = img.asset.replace('.', "-");
                    files.entry(file_id.clone()).or_insert_with(|| {
                        json!({ "mimeType": svg::mime_of(bytes), "id": file_id, "dataURL": svg::data_uri(bytes), "created": 1 })
                    });
                    elements.push(merge(
                        base(
                            id,
                            "image",
                            img.pos[0] + off[0],
                            img.pos[1] + off[1],
                            img.size[0],
                            img.size[1],
                            Color::BLACK,
                            1.0,
                            n,
                        ),
                        json!({
                            "fileId": file_id, "status": "saved", "scale": [1, 1], "locked": img.locked,
                            "crop": excalidraw_crop(img, bytes),
                        }),
                    ));
                }
                Element::Shape(s) => {
                    let r = Rect::from_points([s.a, s.b]);
                    let sw = s.width.max(0.5);
                    let v = match s.kind {
                        ShapeKind::Rect | ShapeKind::Ellipse => base(
                            id,
                            if s.kind == ShapeKind::Rect { "rectangle" } else { "ellipse" },
                            r.min[0] + off[0],
                            r.min[1] + off[1],
                            r.width(),
                            r.height(),
                            s.color,
                            sw,
                            n,
                        ),
                        ShapeKind::Line | ShapeKind::Arrow => merge(
                            base(
                                id,
                                if s.kind == ShapeKind::Arrow { "arrow" } else { "line" },
                                s.a[0] + off[0],
                                s.a[1] + off[1],
                                (s.b[0] - s.a[0]).abs(),
                                (s.b[1] - s.a[1]).abs(),
                                s.color,
                                sw,
                                n,
                            ),
                            json!({
                                "points": [[0.0, 0.0], [s.b[0] - s.a[0], s.b[1] - s.a[1]]],
                                "startBinding": null, "endBinding": null, "lastCommittedPoint": null,
                                "startArrowhead": null,
                                "endArrowhead": if s.kind == ShapeKind::Arrow { json!("arrow") } else { Value::Null },
                            }),
                        ),
                    };
                    elements.push(v);
                }
            }
        }
    }
    let doc = json!({
        "type": "excalidraw",
        "version": 2,
        "source": "https://github.com/HaydenHarms/plexi",
        "elements": elements,
        "appState": { "viewBackgroundColor": "#ffffff", "gridSize": null },
        "files": files,
    });
    serde_json::to_string_pretty(&doc).map_err(|e| ExportError::Other(e.to_string()))
}

/// One draw.io page per Plexi page. Ink, shapes and images go into a single SVG image
/// cell; text boxes become real, editable text cells on top.
pub fn drawio(pages: &[&Page], assets: &Assets) -> String {
    let mut out = String::from("<mxfile host=\"Plexi\" type=\"device\">\n");
    let opts = RenderOptions { text: false, ..Default::default() };
    for (i, page) in pages.iter().enumerate() {
        let b = page.export_bounds();
        let art = svg::page_svg(page, assets, &opts);
        let art64 = base64::engine::general_purpose::STANDARD.encode(art.as_bytes());
        let _ = write!(
            out,
            "<diagram id=\"plexi-{i}\" name=\"{}\">\n<mxGraphModel dx=\"0\" dy=\"0\" grid=\"0\" gridSize=\"10\" guides=\"1\" tooltips=\"1\" connect=\"0\" arrows=\"0\" fold=\"1\" page=\"1\" pageScale=\"1\" pageWidth=\"{}\" pageHeight=\"{}\" math=\"0\" shadow=\"0\">\n<root>\n<mxCell id=\"0\"/>\n<mxCell id=\"1\" parent=\"0\"/>\n",
            svg::esc(&page.name),
            svg::n(b.width()),
            svg::n(b.height())
        );
        // draw.io style strings use ';' as a separator, so its data URIs drop ";base64".
        let _ = writeln!(
            out,
            "<mxCell id=\"ink\" value=\"\" style=\"shape=image;verticalLabelPosition=bottom;verticalAlign=top;imageAspect=0;aspect=fixed;image=data:image/svg+xml,{art64};\" vertex=\"1\" parent=\"1\"><mxGeometry x=\"0\" y=\"0\" width=\"{}\" height=\"{}\" as=\"geometry\"/></mxCell>",
            svg::n(b.width()),
            svg::n(b.height())
        );
        for (k, el) in page.elements.iter().enumerate() {
            let Element::Text(t) = el else { continue };
            if t.text.trim().is_empty() {
                continue;
            }
            let r = t.approx_bounds();
            let _ = writeln!(
                out,
                "<mxCell id=\"text-{k}\" value=\"{}\" style=\"text;html=0;whiteSpace=wrap;align=left;verticalAlign=top;fontSize={};fontColor={};fontFamily={};fontStyle={};spacing=0;\" vertex=\"1\" parent=\"1\"><mxGeometry x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" as=\"geometry\"/></mxCell>",
                svg::esc(&t.text).replace('\n', "&#10;"),
                svg::n(t.size),
                t.color.hex_rgb(),
                svg::esc(t.family()).replace(';', ""),
                t.bold as u8 | (t.italic as u8) << 1 | (t.underline as u8) << 2,
                svg::n(t.pos[0] - b.min[0]),
                svg::n(t.pos[1] - b.min[1]),
                svg::n(r.width() * 1.2),
                svg::n(r.height())
            );
        }
        out.push_str("</root>\n</mxGraphModel>\n</diagram>\n");
    }
    out.push_str("</mxfile>\n");
    out
}

/// W3C InkML: every stroke as a trace with X, Y and force channels.
pub fn inkml(pages: &[&Page]) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ink xmlns=\"http://www.w3.org/2003/InkML\">\n<definitions>\n\
         <context xml:id=\"ctx\"><inkSource xml:id=\"src\"><traceFormat>\
         <channel name=\"X\" type=\"decimal\" units=\"px\"/><channel name=\"Y\" type=\"decimal\" units=\"px\"/>\
         <channel name=\"F\" type=\"decimal\" min=\"0\" max=\"1\"/></traceFormat></inkSource></context>\n",
    );
    let mut brushes: Vec<(Color, u32, InkKind)> = Vec::new();
    let mut body = String::new();
    for page in pages {
        let _ = writeln!(body, "<traceGroup>\n<annotation type=\"page\">{}</annotation>", svg::esc(&page.name));
        for el in &page.elements {
            let Element::Stroke(s) = el else { continue };
            let key = (s.color, (s.width * 100.0) as u32, s.kind);
            let bi = brushes.iter().position(|b| *b == key).unwrap_or_else(|| {
                brushes.push(key);
                brushes.len() - 1
            });
            let pts: Vec<String> =
                s.points.iter().map(|p| format!("{} {} {}", svg::n(p.x), svg::n(p.y), svg::n(p.p))).collect();
            let _ = writeln!(body, "<trace contextRef=\"#ctx\" brushRef=\"#b{bi}\">{}</trace>", pts.join(", "));
        }
        body.push_str("</traceGroup>\n");
    }
    for (i, (c, w, kind)) in brushes.iter().enumerate() {
        let _ = writeln!(
            out,
            "<brush xml:id=\"b{i}\"><brushProperty name=\"color\" value=\"{}\"/><brushProperty name=\"width\" value=\"{}\" units=\"px\"/><brushProperty name=\"transparency\" value=\"{}\"/><brushProperty name=\"tip\" value=\"{}\"/></brush>",
            c.hex_rgb(),
            svg::n(*w as f32 / 100.0),
            255 - c.a(),
            if *kind == InkKind::Highlighter { "rectangle" } else { "ellipse" }
        );
    }
    out.push_str("</definitions>\n");
    out.push_str(&body);
    out.push_str("</ink>\n");
    out
}

/// The document as readable JSON with its images inlined as base64.
pub fn plexi_json(doc: &Document) -> Result<String, ExportError> {
    let mut v = serde_json::to_value(doc).map_err(|e| ExportError::Other(e.to_string()))?;
    let assets: serde_json::Map<String, Value> = doc
        .assets
        .iter()
        .map(|(k, b)| (k.clone(), Value::String(base64::engine::general_purpose::STANDARD.encode(b.as_slice()))))
        .collect();
    if let Some(o) = v.as_object_mut() {
        o.insert("assets".into(), Value::Object(assets));
    }
    serde_json::to_string_pretty(&v).map_err(|e| ExportError::Other(e.to_string()))
}
