//! Bringing a PDF onto the canvas: each page becomes a picture, at the size it prints.

use hayro::hayro_interpret::InterpreterSettings;
use hayro::hayro_syntax::Pdf;
use hayro::vello_cpu::color::palette::css::WHITE;
use hayro::{PixmapSettings, RenderCache, RenderSettings, render};

/// One page of a PDF as a picture.
pub struct PdfPage {
    /// The page as a PNG.
    pub png: Vec<u8>,
    /// How big the page prints, in page units (1/96 inch).
    pub size: [f32; 2],
}

/// Page units (1/96 inch) per PDF point (1/72 inch).
const UNITS_PER_POINT: f32 = 96.0 / 72.0;
/// Pixels per page unit the pages are drawn at, so they stay sharp zoomed in a little.
const SHARPNESS: f32 = 2.0;
/// The longest side a page's picture may have, in pixels.
const MAX_SIDE: f32 = 4096.0;

/// Whether `bytes` look like a PDF file.
pub fn is_pdf(bytes: &[u8]) -> bool {
    bytes.get(..1024).unwrap_or(bytes).windows(5).any(|w| w == b"%PDF-")
}

/// The first `max_pages` pages of the PDF in `bytes`, drawn on white, and how many
/// pages it has in all.
pub fn pages(bytes: Vec<u8>, max_pages: usize) -> Result<(Vec<PdfPage>, usize), String> {
    let pdf = Pdf::new(bytes).map_err(|e| match format!("{e:?}") {
        s if s.contains("ncrypt") || s.contains("assword") => "That PDF is password-protected".to_owned(),
        _ => "That PDF couldn't be read".to_owned(),
    })?;
    let all = pdf.pages();
    let total = all.len();
    if total == 0 {
        return Err("That PDF has no pages".into());
    }
    let cache = RenderCache::new();
    let interpreter = InterpreterSettings::default();
    let settings = RenderSettings::default();
    let mut out = Vec::new();
    for page in all.iter().take(max_pages) {
        let (w, h) = page.render_dimensions();
        let size = [w * UNITS_PER_POINT, h * UNITS_PER_POINT];
        let scale = (UNITS_PER_POINT * SHARPNESS).min(MAX_SIDE / w.max(h).max(1.0));
        let pixmap = PixmapSettings { x_scale: scale, y_scale: scale, bg_color: WHITE };
        let pix = render(page, &cache, &interpreter, &settings, &pixmap);
        let (pw, ph) = (pix.width() as u32, pix.height() as u32);
        let img = image::RgbaImage::from_raw(pw, ph, pix.data_as_u8_slice().to_vec())
            .ok_or_else(|| "A page of that PDF couldn't be drawn".to_owned())?;
        let mut png = std::io::Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
        out.push(PdfPage { png: png.into_inner(), size });
    }
    Ok((out, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::RenderOptions;
    use crate::export::pdf::pages_pdf;
    use crate::model::*;

    fn pdf_again(page: &Page) -> Vec<u8> {
        pages_pdf("t", &[page, page], &Default::default(), &RenderOptions::default()).unwrap()
    }

    #[test]
    fn a_pdf_comes_back_as_pictures_of_its_pages() {
        // Make a one-page Letter PDF with a red box near its top-left corner.
        let mut page = Page::new("p", PageSize::Letter);
        let mut s = Shape::new(ShapeKind::Rect, [50.0, 50.0], [250.0, 150.0], Color::rgb(220, 0, 0), 30.0);
        s.width = 40.0;
        page.elements.push(Element::Shape(s));
        let doc = Document::new("d");
        let pdf = pages_pdf("t", &[&page, &page], &doc.assets, &RenderOptions::default()).unwrap();
        assert!(is_pdf(&pdf));
        let (drawn, total) = pages(pdf, 10).unwrap();
        assert_eq!((drawn.len(), total), (2, 2));
        assert_eq!(pages(pdf_again(&page), 1).unwrap().0.len(), 1, "only as many pages as asked for");
        let p = &drawn[0];
        assert!((p.size[0] - 816.0).abs() < 1.0 && (p.size[1] - 1056.0).abs() < 1.0, "{:?}", p.size);
        let img = image::load_from_memory(&p.png).unwrap().to_rgba8();
        assert!(img.width() > 1000, "drawn sharp: {}", img.width());
        let k = img.width() as f32 / 816.0;
        let px = |x: f32, y: f32| img.get_pixel((x * k) as u32, (y * k) as u32).0;
        assert_eq!(px(600.0, 900.0), [255, 255, 255, 255], "white paper");
        let red = px(50.0, 100.0);
        assert!(red[0] > 180 && red[1] < 80, "the box's edge: {red:?}");
        assert!(!is_pdf(b"\x89PNG not a pdf"));
        assert!(pages(b"%PDF-1.7 garbage".to_vec(), 10).is_err());
    }
}
