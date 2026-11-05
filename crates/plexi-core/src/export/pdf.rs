//! Multi-page vector PDF: each page's SVG becomes a form XObject drawn full-page.

use std::collections::HashMap;

use pdf_writer::{Content, Finish, Name, Pdf, Rect, Ref, TextStr};

use super::{Assets, ExportError, RenderOptions, parse_svg, svg};
use crate::model::Page;

/// Page units are CSS pixels; PDF uses points.
const PT_PER_UNIT: f32 = 72.0 / 96.0;

pub fn pages_pdf(title: &str, pages: &[&Page], assets: &Assets, opts: &RenderOptions) -> Result<Vec<u8>, ExportError> {
    let mut alloc = Ref::new(1);
    let catalog_id = alloc.bump();
    let tree_id = alloc.bump();
    let info_id = alloc.bump();
    let mut pdf = Pdf::new();
    let mut kids = Vec::new();

    for page in pages {
        let tree = parse_svg(&svg::page_svg(page, assets, opts))?;
        let (chunk, svg_id) = svg2pdf::to_chunk(&tree, svg2pdf::ConversionOptions::default())
            .map_err(|e| ExportError::Pdf(e.to_string()))?;
        let mut map = HashMap::new();
        let chunk = chunk.renumber(|old| *map.entry(old).or_insert_with(|| alloc.bump()));
        let svg_id = *map.get(&svg_id).ok_or_else(|| ExportError::Pdf("lost the page object".into()))?;

        let (w, h) = (tree.size().width() * PT_PER_UNIT, tree.size().height() * PT_PER_UNIT);
        let page_id = alloc.bump();
        let content_id = alloc.bump();
        let name = Name(b"P");
        let mut p = pdf.page(page_id);
        p.media_box(Rect::new(0.0, 0.0, w, h));
        p.parent(tree_id);
        p.contents(content_id);
        p.resources().x_objects().pair(name, svg_id);
        p.finish();

        let mut content = Content::new();
        content.transform([w, 0.0, 0.0, h, 0.0, 0.0]).x_object(name);
        pdf.stream(content_id, &content.finish());
        pdf.extend(&chunk);
        kids.push(page_id);
    }

    pdf.catalog(catalog_id).pages(tree_id);
    let count = kids.len() as i32;
    pdf.pages(tree_id).kids(kids).count(count);
    pdf.document_info(info_id).title(TextStr(title)).producer(TextStr("Plexi"));
    Ok(pdf.finish())
}
