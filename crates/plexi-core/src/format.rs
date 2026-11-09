//! The `.plexi` file: a zip holding `document.json`, the images under `assets/`, and a
//! `thumbnail.png` of the first page for file browsers.

use std::io::{Cursor, Read, Seek, Write};
use std::path::Path;
use std::sync::Arc;

use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

use crate::model::{Document, FORMAT_VERSION};

pub const EXTENSION: &str = "plexi";

#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("not a Plexi file: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("damaged document.json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("this file was made by a newer Plexi (format {0}); please update")]
    TooNew(u32),
}

pub fn to_bytes(doc: &Document) -> Result<Vec<u8>, FormatError> {
    let mut buf = Cursor::new(Vec::new());
    write_to(doc, &mut buf)?;
    Ok(buf.into_inner())
}

pub fn write_to<W: Write + Seek>(doc: &Document, w: W) -> Result<(), FormatError> {
    let mut zip = ZipWriter::new(w);
    let deflate = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);

    zip.start_file("document.json", deflate)?;
    let mut doc_json = serde_json::to_vec(doc)?;
    doc_json.push(b'\n');
    zip.write_all(&doc_json)?;

    for (key, bytes) in &doc.assets {
        // Images are already compressed.
        zip.start_file(format!("assets/{key}"), stored)?;
        zip.write_all(bytes)?;
    }

    if let Some(page) = doc.pages.first() {
        let opts =
            crate::export::RenderOptions { scale: 256.0 / page.export_bounds().width().max(1.0), ..Default::default() };
        if let Ok(png) = crate::export::raster::page_png(page, &doc.assets, &opts) {
            zip.start_file("thumbnail.png", stored)?;
            zip.write_all(&png)?;
        }
    }
    zip.finish()?;
    Ok(())
}

pub fn from_bytes(bytes: &[u8]) -> Result<Document, FormatError> {
    read_from(Cursor::new(bytes))
}

pub fn read_from<R: Read + Seek>(r: R) -> Result<Document, FormatError> {
    let mut zip = ZipArchive::new(r)?;
    let mut doc: Document = {
        let mut f = zip.by_name("document.json")?;
        let mut s = Vec::new();
        f.read_to_end(&mut s)?;
        // Check the version before the full parse so a newer file gets a clear message.
        #[derive(serde::Deserialize)]
        struct Probe {
            version: u32,
        }
        let probe: Probe = serde_json::from_slice(&s)?;
        if probe.version > FORMAT_VERSION {
            return Err(FormatError::TooNew(probe.version));
        }
        serde_json::from_slice(&s)?
    };
    for i in 0..zip.len() {
        let mut f = zip.by_index(i)?;
        let name = f.name()?.into_owned();
        let Some(key) = name.strip_prefix("assets/").map(str::to_owned) else { continue };
        if key.is_empty() {
            continue;
        }
        let mut bytes = Vec::with_capacity(f.size() as usize);
        f.read_to_end(&mut bytes)?;
        doc.assets.insert(key, Arc::new(bytes));
    }
    doc.version = FORMAT_VERSION;
    doc.renew_groups();
    Ok(doc)
}

/// Save atomically: write a sibling temp file, then rename it over the target, so a
/// crash mid-save never leaves a half-written notebook.
pub fn save(doc: &Document, path: &Path) -> Result<(), FormatError> {
    let bytes = to_bytes(doc)?;
    let tmp = path.with_extension("plexi.tmp");
    std::fs::write(&tmp, &bytes)?;
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

pub fn load(path: &Path) -> Result<Document, FormatError> {
    let f = std::fs::File::open(path)?;
    read_from(std::io::BufReader::new(f))
}

#[cfg(test)]
pub(crate) mod tests_support {
    use std::io::Cursor;

    pub(crate) fn tiny_png() -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::tiny_png;
    use super::*;
    use crate::model::*;

    fn sample() -> Document {
        let mut d = Document::new("Test");
        let asset = d.add_asset(tiny_png());
        let p = &mut d.pages[0];
        p.background = Background::Grid;
        p.elements.push(Element::Stroke(Stroke {
            id: new_id(),
            kind: InkKind::Pen,
            color: Color::rgb(10, 20, 30),
            width: 3.0,
            points: vec![InkPoint::new(10.0, 10.0, 0.4), InkPoint::new(50.0, 60.0, 0.9)],
            group: 0,
        }));
        p.elements.push(Element::Text(TextBox::new([100.0, 100.0], "hello\nworld", 20.0, Color::BLACK)));
        p.elements.push(Element::Image(ImageEl {
            id: new_id(),
            asset,
            pos: [0.0, 0.0],
            size: [10.0, 10.0],
            locked: true,
            crop: None,
            group: 0,
            rotation: 0.0,
            inner_rotation: 0.0,
        }));
        d.overlay.elements.push(Element::Shape(Shape {
            id: new_id(),
            kind: ShapeKind::Arrow,
            a: [0.0, 0.0],
            b: [100.0, 50.0],
            color: Color::rgb(255, 0, 0),
            width: 4.0,
            group: 0,
            rotation: 0.0,
        }));
        d
    }

    #[test]
    fn round_trip_preserves_content() {
        let d = sample();
        let bytes = to_bytes(&d).unwrap();
        let back = from_bytes(&bytes).unwrap();
        assert_eq!(back.title, "Test");
        assert_eq!(back.pages[0].background, Background::Grid);
        assert_eq!(back.pages[0].elements.len(), 3);
        assert_eq!(back.overlay.elements.len(), 1);
        assert_eq!(back.assets.len(), 1);
        match &back.pages[0].elements[1] {
            Element::Text(t) => assert_eq!(t.text, "hello\nworld"),
            e => panic!("unexpected {e:?}"),
        }
    }

    #[test]
    fn newer_files_are_rejected_clearly() {
        let mut d = sample();
        d.version = FORMAT_VERSION + 1;
        let bytes = to_bytes(&d).unwrap();
        assert!(matches!(from_bytes(&bytes), Err(FormatError::TooNew(_))));
    }

    #[test]
    fn save_and_load_from_disk() {
        let dir = std::env::temp_dir().join(format!("plexi-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("n.plexi");
        save(&sample(), &path).unwrap();
        assert_eq!(load(&path).unwrap().pages[0].elements.len(), 3);
        std::fs::remove_dir_all(dir).ok();
    }
}
