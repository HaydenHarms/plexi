//! Text-first exports: HTML (inline SVG pages), Markdown (text plus page images) and
//! plain text.

use std::fmt::Write;
use std::path::{Path, PathBuf};

use super::{Assets, ExportError, ExportFormat, RenderOptions, raster, svg};
use crate::model::{Element, Page};

/// Text boxes of a page in reading order (top to bottom, then left to right).
pub fn page_texts(page: &Page) -> Vec<&str> {
    let mut boxes: Vec<_> = page
        .elements
        .iter()
        .filter_map(|e| match e {
            Element::Text(t) if !t.text.trim().is_empty() => Some(t),
            _ => None,
        })
        .collect();
    boxes.sort_by(|a, b| {
        let row = |t: &crate::model::TextBox| (t.pos[1] / 16.0).round() as i64;
        row(a).cmp(&row(b)).then(a.pos[0].total_cmp(&b.pos[0]))
    });
    boxes.into_iter().map(|t| t.text.as_str()).collect()
}

pub fn plain(title: &str, pages: &[&Page]) -> String {
    let mut out = format!("{title}\n{}\n", "=".repeat(title.chars().count().max(3)));
    for page in pages {
        let _ = write!(out, "\n{}\n{}\n\n", page.name, "-".repeat(page.name.chars().count().max(3)));
        for t in page_texts(page) {
            out.push_str(t.trim_end());
            out.push_str("\n\n");
        }
    }
    out
}

pub fn html(title: &str, pages: &[&Page], assets: &Assets, opts: &RenderOptions) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n<title>{}</title>\n<style>\n\
         body {{ margin: 0; padding: 24px 16px; background: #e9ebef; font-family: system-ui, sans-serif; color: #1d2330; }}\n\
         h1 {{ max-width: 900px; margin: 0 auto 16px; font-weight: 600; }}\n\
         section {{ max-width: 900px; margin: 0 auto 32px; }}\n\
         h2 {{ font-size: 14px; font-weight: 600; color: #5b6475; margin: 0 0 8px; }}\n\
         section svg {{ display: block; width: 100%; height: auto; background: transparent; box-shadow: 0 1px 4px rgba(0,0,0,.15); }}\n\
         @media (prefers-color-scheme: dark) {{ body {{ background: #15171c; color: #e6e8ec; }} h2 {{ color: #9aa3b5; }} }}\n\
         </style>\n</head>\n<body>\n<h1>{}</h1>\n",
        svg::esc(title),
        svg::esc(title)
    );
    for page in pages {
        let _ = writeln!(out, "<section>\n<h2>{}</h2>", svg::esc(&page.name));
        for frame in opts.frames(page) {
            out.push_str(&svg::frame_svg(page, assets, opts, &frame));
        }
        out.push_str("</section>\n");
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// Writes `notes.md` plus `notes_files/page-N.png`.
pub fn markdown(
    title: &str,
    pages: &[&Page],
    assets: &Assets,
    opts: &RenderOptions,
    path: &Path,
) -> Result<Vec<PathBuf>, ExportError> {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("notes").to_owned();
    let dir_name = format!("{stem}_files");
    let dir = path.with_file_name(&dir_name);
    std::fs::create_dir_all(&dir)?;
    let mut written = Vec::new();
    let mut md = format!("# {title}\n");
    let mut k = 0;
    for page in pages {
        let _ = write!(md, "\n## {}\n", page.name);
        for frame in opts.frames(page) {
            k += 1;
            let file = format!("page-{k}.png");
            let img = dir.join(&file);
            std::fs::write(&img, raster::frame_bytes(page, assets, opts, &frame, ExportFormat::Png)?)?;
            written.push(img);
            let _ = write!(md, "\n![{}]({}/{})\n", page.name, dir_name.replace(' ', "%20"), file);
        }
        for t in page_texts(page) {
            md.push('\n');
            md.push_str(t.trim_end());
            md.push('\n');
        }
    }
    std::fs::write(path, md)?;
    written.insert(0, path.to_path_buf());
    Ok(written)
}
