//! Exporters. Everything visual goes through one SVG description of the page
//! ([`svg::page_svg`]): rasters render that SVG with resvg and PDF converts it with
//! svg2pdf, so every format shows the same thing.

pub mod interop;
pub mod layout;
pub mod pdf;
pub mod raster;
pub mod svg;
pub mod text;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use resvg::usvg;

use crate::model::{Document, Page};

pub type Assets = BTreeMap<String, Arc<Vec<u8>>>;

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("could not build the page image: {0}")]
    Svg(String),
    #[error("could not encode the image: {0}")]
    Image(#[from] image::ImageError),
    #[error("could not write the PDF: {0}")]
    Pdf(String),
    #[error("{0}")]
    Other(String),
}

#[derive(Clone, Debug)]
pub struct RenderOptions {
    /// Output pixels per page unit (raster formats only).
    pub scale: f32,
    /// Paint the paper color and its lines/grid. Off gives a transparent background.
    pub background: bool,
    /// Include text boxes. Off is used when text is exported separately (draw.io).
    pub text: bool,
    /// Which part of the page goes on which sheets.
    pub layout: layout::Layout,
    /// Only these output pages (0-based, counted across every exported page). `None`
    /// writes them all.
    pub pick: Option<Vec<usize>>,
}

impl Default for RenderOptions {
    fn default() -> Self {
        Self { scale: 2.0, background: true, text: true, layout: layout::Layout::default(), pick: None }
    }
}

impl RenderOptions {
    /// The output pages `page` becomes under these options.
    pub fn frames(&self, page: &Page) -> Vec<layout::Frame> {
        layout::frames(page, &self.layout)
    }

    /// Every output page of an export of `pages`, in order, with the page each one
    /// shows; only the picked ones when [`Self::pick`] is set.
    pub fn outputs<'a>(&self, pages: &[&'a Page]) -> Vec<(&'a Page, layout::Frame)> {
        pages
            .iter()
            .flat_map(|p| self.frames(p).into_iter().map(move |f| (*p, f)))
            .enumerate()
            .filter(|(i, _)| self.pick.as_ref().is_none_or(|k| k.contains(i)))
            .map(|(_, o)| o)
            .collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    Png,
    Jpeg,
    Webp,
    Bmp,
    Tiff,
    Svg,
    Pdf,
    Html,
    Markdown,
    Text,
    Excalidraw,
    Drawio,
    InkMl,
    Json,
}

impl ExportFormat {
    pub const ALL: [ExportFormat; 14] = [
        ExportFormat::Pdf,
        ExportFormat::Png,
        ExportFormat::Jpeg,
        ExportFormat::Webp,
        ExportFormat::Bmp,
        ExportFormat::Tiff,
        ExportFormat::Svg,
        ExportFormat::Html,
        ExportFormat::Markdown,
        ExportFormat::Text,
        ExportFormat::Excalidraw,
        ExportFormat::Drawio,
        ExportFormat::InkMl,
        ExportFormat::Json,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Png => "PNG image",
            ExportFormat::Jpeg => "JPEG image",
            ExportFormat::Webp => "WebP image",
            ExportFormat::Bmp => "BMP image",
            ExportFormat::Tiff => "TIFF image",
            ExportFormat::Svg => "SVG vector",
            ExportFormat::Pdf => "PDF document",
            ExportFormat::Html => "HTML page",
            ExportFormat::Markdown => "Markdown + images",
            ExportFormat::Text => "Plain text",
            ExportFormat::Excalidraw => "Excalidraw",
            ExportFormat::Drawio => "draw.io diagram",
            ExportFormat::InkMl => "InkML (ink only)",
            ExportFormat::Json => "Plexi JSON",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Png => "png",
            ExportFormat::Jpeg => "jpg",
            ExportFormat::Webp => "webp",
            ExportFormat::Bmp => "bmp",
            ExportFormat::Tiff => "tiff",
            ExportFormat::Svg => "svg",
            ExportFormat::Pdf => "pdf",
            ExportFormat::Html => "html",
            ExportFormat::Markdown => "md",
            ExportFormat::Text => "txt",
            ExportFormat::Excalidraw => "excalidraw",
            ExportFormat::Drawio => "drawio",
            ExportFormat::InkMl => "inkml",
            ExportFormat::Json => "json",
        }
    }

    pub fn from_extension(ext: &str) -> Option<Self> {
        let ext = ext.to_ascii_lowercase();
        match ext.as_str() {
            "jpeg" => Some(ExportFormat::Jpeg),
            "tif" => Some(ExportFormat::Tiff),
            "markdown" => Some(ExportFormat::Markdown),
            "xml" => Some(ExportFormat::Drawio),
            _ => Self::ALL.into_iter().find(|f| f.extension() == ext),
        }
    }

    /// Raster formats and SVG hold one page per file.
    pub fn one_page_per_file(self) -> bool {
        matches!(
            self,
            ExportFormat::Png
                | ExportFormat::Jpeg
                | ExportFormat::Webp
                | ExportFormat::Bmp
                | ExportFormat::Tiff
                | ExportFormat::Svg
        )
    }

    pub fn uses_scale(self) -> bool {
        self.one_page_per_file() && self != ExportFormat::Svg
    }
}

/// Export `pages` of `doc` to `path`. Formats that hold one page per file write
/// `name-1.ext`, `name-2.ext`, … when given several pages. Returns the files written.
pub fn export(
    doc: &Document,
    pages: &[&Page],
    format: ExportFormat,
    opts: &RenderOptions,
    path: &Path,
) -> Result<Vec<PathBuf>, ExportError> {
    if pages.is_empty() {
        return Err(ExportError::Other("nothing to export".into()));
    }
    let assets = &doc.assets;
    if format.one_page_per_file() {
        let outputs = opts.outputs(pages);
        if outputs.is_empty() {
            return Err(ExportError::Other("no pages picked".into()));
        }
        let mut written = Vec::new();
        for (i, (page, frame)) in outputs.iter().enumerate() {
            let target = if outputs.len() == 1 { path.to_path_buf() } else { numbered(path, i + 1) };
            let bytes = match format {
                ExportFormat::Svg => svg::frame_svg(page, assets, opts, frame).into_bytes(),
                f => raster::frame_bytes(page, assets, opts, frame, f)?,
            };
            std::fs::write(&target, bytes)?;
            written.push(target);
        }
        return Ok(written);
    }
    let bytes = match format {
        ExportFormat::Pdf => pdf::pages_pdf(&doc.title, pages, assets, opts)?,
        ExportFormat::Html => text::html(&doc.title, pages, assets, opts).into_bytes(),
        ExportFormat::Markdown => return text::markdown(&doc.title, pages, assets, opts, path),
        ExportFormat::Text => text::plain(&doc.title, pages).into_bytes(),
        ExportFormat::Excalidraw => interop::excalidraw(pages, assets)?.into_bytes(),
        ExportFormat::Drawio => interop::drawio(pages, assets).into_bytes(),
        ExportFormat::InkMl => interop::inkml(pages).into_bytes(),
        ExportFormat::Json => interop::plexi_json(doc)?.into_bytes(),
        _ => unreachable!("handled above"),
    };
    std::fs::write(path, bytes)?;
    Ok(vec![path.to_path_buf()])
}

/// `notes.png` -> `notes-3.png`.
pub fn numbered(path: &Path, n: usize) -> PathBuf {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("page");
    let name = match path.extension().and_then(|e| e.to_str()) {
        Some(ext) => format!("{stem}-{n}.{ext}"),
        None => format!("{stem}-{n}"),
    };
    path.with_file_name(name)
}

pub(crate) fn parse_svg(svg: &str) -> Result<usvg::Tree, ExportError> {
    let opts = usvg::Options { fontdb: crate::fonts::database(), ..Default::default() };
    usvg::Tree::from_str(svg, &opts).map_err(|e| ExportError::Svg(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    fn doc() -> Document {
        let mut d = Document::new("Export test");
        let asset = d.add_asset(crate::format::tests_support::tiny_png());
        let p = &mut d.pages[0];
        p.background = Background::Lined;
        p.elements.push(Element::Stroke(Stroke {
            id: new_id(),
            kind: InkKind::Pen,
            color: Color::rgb(20, 40, 200),
            width: 4.0,
            points: (0..20)
                .map(|i| InkPoint::new(100.0 + i as f32 * 10.0, 200.0 + (i as f32).sin() * 20.0, 0.3 + i as f32 / 40.0))
                .collect(),
        }));
        p.elements.push(Element::Stroke(Stroke {
            id: new_id(),
            kind: InkKind::Highlighter,
            color: Color::rgba(255, 220, 0, 110),
            width: 18.0,
            points: vec![InkPoint::new(80.0, 300.0, 0.5), InkPoint::new(400.0, 300.0, 0.5)],
        }));
        p.elements.push(Element::Text(TextBox::new(
            [100.0, 400.0],
            "Hello <Plexi> & friends\nsecond line",
            24.0,
            Color::BLACK,
        )));
        p.elements.push(Element::Image(ImageEl {
            id: new_id(),
            asset,
            pos: [500.0, 500.0],
            size: [40.0, 40.0],
            locked: false,
            crop: None,
        }));
        for kind in [ShapeKind::Line, ShapeKind::Arrow, ShapeKind::Rect, ShapeKind::Ellipse] {
            p.elements.push(Element::Shape(Shape {
                id: new_id(),
                kind,
                a: [100.0, 600.0],
                b: [300.0, 700.0],
                color: Color::rgb(200, 0, 0),
                width: 3.0,
            }));
        }
        let mut p2 = Page::new("Infinite", PageSize::Infinite);
        p2.background = Background::Dots;
        p2.elements.push(Element::Text(TextBox::new([-500.0, -300.0], "far away", 16.0, Color::BLACK)));
        d.pages.push(p2);
        d
    }

    #[test]
    fn every_format_exports() {
        let d = doc();
        let pages: Vec<&Page> = d.pages.iter().collect();
        let dir = std::env::temp_dir().join(format!("plexi-export-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for f in ExportFormat::ALL {
            let path = dir.join(format!("out.{}", f.extension()));
            let opts = RenderOptions { scale: 0.5, ..Default::default() };
            let files = export(&d, &pages, f, &opts, &path).unwrap_or_else(|e| panic!("{f:?}: {e}"));
            assert!(!files.is_empty());
            for file in files {
                let len = std::fs::metadata(&file).unwrap().len();
                assert!(len > 20, "{f:?} wrote a tiny file {file:?}");
            }
        }
        let pdf = std::fs::read(dir.join("out.pdf")).unwrap();
        assert!(pdf.starts_with(b"%PDF"));
        assert_eq!(
            count(&pdf, b"/Type /Page\n") + count(&pdf, b"/Type /Page\r") + count(&pdf, b"/Type /Page/"),
            2,
            "two pages"
        );
        assert!(dir.join("out-2.png").exists());
        // Only the picked output pages are written.
        let opts = RenderOptions { scale: 0.25, pick: Some(vec![1]), ..Default::default() };
        let one = export(&d, &pages, ExportFormat::Pdf, &opts, &dir.join("pick.pdf")).unwrap();
        let pdf = std::fs::read(&one[0]).unwrap();
        let pages_in = count(&pdf, b"/Type /Page") - count(&pdf, b"/Type /Pages");
        assert_eq!(pages_in, 1);
        assert!(dir.join("out_files").join("page-1.png").exists());
        let html = std::fs::read_to_string(dir.join("out.html")).unwrap();
        assert!(html.contains("&lt;Plexi&gt; &amp; friends"));
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(dir.join("out.excalidraw")).unwrap()).unwrap();
        std::fs::remove_dir_all(dir).ok();
    }

    fn count(hay: &[u8], needle: &[u8]) -> usize {
        hay.windows(needle.len()).filter(|w| *w == needle).count()
    }

    #[test]
    fn extensions_map_back() {
        for f in ExportFormat::ALL {
            assert_eq!(ExportFormat::from_extension(f.extension()), Some(f));
        }
        assert_eq!(numbered(Path::new("a/notes.png"), 3), Path::new("a/notes-3.png"));
    }
}
