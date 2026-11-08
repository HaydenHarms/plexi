//! Raster export: render the page SVG with resvg, then encode.

use std::io::Cursor;

use resvg::tiny_skia;

use super::layout::Frame;
use super::{Assets, ExportError, ExportFormat, RenderOptions, parse_svg, svg};
use crate::geom::Rect;
use crate::model::{Color, Page};

/// Largest output side, to keep a stray scale factor from allocating gigabytes.
pub const MAX_SIDE: u32 = 16384;

/// Render the first output page of `page` to a straight-alpha RGBA image.
pub fn page_rgba(page: &Page, assets: &Assets, opts: &RenderOptions) -> Result<image::RgbaImage, ExportError> {
    let frame = opts.frames(page).into_iter().next().unwrap_or_else(|| Frame::exact(page.export_bounds()));
    frame_rgba(page, assets, opts, &frame)
}

/// Render an explicit area of a page at 1:1 (times `opts.scale`).
pub fn area_rgba(
    page: &Page,
    assets: &Assets,
    opts: &RenderOptions,
    area: Rect,
) -> Result<image::RgbaImage, ExportError> {
    frame_rgba(page, assets, opts, &Frame::exact(area))
}

/// Render one output page. The image is `frame.size * opts.scale` pixels.
pub fn frame_rgba(
    page: &Page,
    assets: &Assets,
    opts: &RenderOptions,
    frame: &Frame,
) -> Result<image::RgbaImage, ExportError> {
    let doc = svg::frame_svg(page, assets, opts, frame);
    let tree = parse_svg(&doc)?;
    let size = tree.size();
    let mut scale = opts.scale.max(0.01);
    let longest = size.width().max(size.height()) * scale;
    if longest > MAX_SIDE as f32 {
        scale *= MAX_SIDE as f32 / longest;
    }
    let w = ((size.width() * scale).ceil() as u32).max(1);
    let h = ((size.height() * scale).ceil() as u32).max(1);
    let mut pixmap = tiny_skia::Pixmap::new(w, h).ok_or_else(|| ExportError::Svg("empty image".into()))?;
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut img = image::RgbaImage::new(w, h);
    for (dst, src) in img.pixels_mut().zip(pixmap.pixels()) {
        let c = src.demultiply();
        *dst = image::Rgba([c.red(), c.green(), c.blue(), c.alpha()]);
    }
    Ok(img)
}

pub fn page_png(page: &Page, assets: &Assets, opts: &RenderOptions) -> Result<Vec<u8>, ExportError> {
    page_bytes(page, assets, opts, ExportFormat::Png)
}

/// Composite onto an opaque color, for formats without alpha.
pub fn flatten(img: &image::RgbaImage, under: Color) -> image::RgbImage {
    let mut out = image::RgbImage::new(img.width(), img.height());
    for (dst, src) in out.pixels_mut().zip(img.pixels()) {
        let a = src[3] as u32;
        let mix = |c: u8, u: u8| ((c as u32 * a + u as u32 * (255 - a)) / 255) as u8;
        *dst = image::Rgb([mix(src[0], under.r()), mix(src[1], under.g()), mix(src[2], under.b())]);
    }
    out
}

pub fn encode(img: &image::RgbaImage, format: ExportFormat, under: Color) -> Result<Vec<u8>, ExportError> {
    let mut out = Cursor::new(Vec::new());
    match format {
        ExportFormat::Png => img.write_to(&mut out, image::ImageFormat::Png)?,
        ExportFormat::Webp => img.write_to(&mut out, image::ImageFormat::WebP)?,
        ExportFormat::Tiff => img.write_to(&mut out, image::ImageFormat::Tiff)?,
        ExportFormat::Jpeg => {
            let rgb = flatten(img, under);
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 92).encode_image(&rgb)?;
        }
        ExportFormat::Bmp => flatten(img, under).write_to(&mut out, image::ImageFormat::Bmp)?,
        f => return Err(ExportError::Other(format!("{f:?} is not a raster format"))),
    }
    Ok(out.into_inner())
}

pub fn page_bytes(
    page: &Page,
    assets: &Assets,
    opts: &RenderOptions,
    format: ExportFormat,
) -> Result<Vec<u8>, ExportError> {
    let frame = opts.frames(page).into_iter().next().unwrap_or_else(|| Frame::exact(page.export_bounds()));
    frame_bytes(page, assets, opts, &frame, format)
}

pub fn frame_bytes(
    page: &Page,
    assets: &Assets,
    opts: &RenderOptions,
    frame: &Frame,
    format: ExportFormat,
) -> Result<Vec<u8>, ExportError> {
    let img = frame_rgba(page, assets, opts, frame)?;
    let under = if page.paper.a() > 0 { page.paper.with_alpha(255) } else { Color::WHITE };
    encode(&img, format, under)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::*;

    #[test]
    fn ink_shows_up_in_pixels() {
        let mut page = Page::new("p", PageSize::Custom([100.0, 100.0]));
        page.elements.push(Element::Stroke(Stroke {
            id: 1,
            kind: InkKind::Pen,
            color: Color::rgb(255, 0, 0),
            width: 10.0,
            points: vec![InkPoint::new(10.0, 50.0, 0.5), InkPoint::new(90.0, 50.0, 0.5)],
        }));
        let img = page_rgba(&page, &Assets::new(), &RenderOptions { scale: 1.0, ..Default::default() }).unwrap();
        assert_eq!((img.width(), img.height()), (100, 100));
        assert_eq!(img.get_pixel(50, 50).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(50, 10).0, [255, 255, 255, 255]);
        // Transparent overlay page: no paper.
        page.paper = Color::TRANSPARENT;
        let img = page_rgba(&page, &Assets::new(), &RenderOptions { scale: 1.0, ..Default::default() }).unwrap();
        assert_eq!(img.get_pixel(50, 10).0[3], 0);
    }

    #[test]
    fn cropped_images_show_only_the_crop() {
        // Left half red, right half blue.
        let mut pic = image::RgbaImage::from_pixel(20, 10, image::Rgba([255, 0, 0, 255]));
        for x in 10..20 {
            for y in 0..10 {
                pic.put_pixel(x, y, image::Rgba([0, 0, 255, 255]));
            }
        }
        let mut png = Cursor::new(Vec::new());
        pic.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let mut assets = Assets::new();
        assets.insert("p.png".into(), std::sync::Arc::new(png.into_inner()));
        let mut page = Page::new("p", PageSize::Custom([100.0, 100.0]));
        let mut img = ImageEl::new("p.png".into(), [0.0, 0.0], [100.0, 50.0]);
        img.set_crop(Rect::from_min_size([50.0, 0.0], [50.0, 50.0])); // the blue half
        img.pos = [10.0, 10.0];
        page.elements.push(Element::Image(img));
        let img = page_rgba(&page, &assets, &RenderOptions { scale: 1.0, ..Default::default() }).unwrap();
        assert_eq!(img.get_pixel(35, 35).0, [0, 0, 255, 255], "inside the crop");
        assert_eq!(img.get_pixel(5, 35).0, [255, 255, 255, 255], "the red half is cut off");
        assert_eq!(img.get_pixel(70, 35).0, [255, 255, 255, 255], "nothing past the crop");
    }

    #[test]
    fn gif_assets_render_only_their_first_frame() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};

        // Red first frame, blue second — export must only ever show the red one.
        let mut bytes = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut bytes);
            let red = image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 0, 0, 255]));
            let blue = image::RgbaImage::from_pixel(10, 10, image::Rgba([0, 0, 255, 255]));
            enc.encode_frame(Frame::from_parts(red, 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
            enc.encode_frame(Frame::from_parts(blue, 0, 0, Delay::from_numer_denom_ms(100, 1))).unwrap();
        }
        let mut assets = Assets::new();
        assets.insert("a.gif".into(), std::sync::Arc::new(bytes));
        let mut page = Page::new("p", PageSize::Custom([10.0, 10.0]));
        page.elements.push(Element::Image(ImageEl::new("a.gif".into(), [0.0, 0.0], [10.0, 10.0])));
        let img = page_rgba(&page, &assets, &RenderOptions { scale: 1.0, ..Default::default() }).unwrap();
        assert_eq!(img.get_pixel(5, 5).0, [255, 0, 0, 255], "the still export shows the first frame, not the second");
    }

    #[test]
    fn text_styles_reach_the_svg() {
        let mut t = TextBox::new([0.0, 0.0], "hi", 20.0, Color::BLACK);
        t.font = Some("Georgia".into());
        t.bold = true;
        t.italic = true;
        t.underline = true;
        let mut page = Page::new("p", PageSize::Custom([100.0, 100.0]));
        page.elements.push(Element::Text(t));
        let svg = svg::page_svg(&page, &Assets::new(), &RenderOptions::default());
        assert!(svg.contains("font-family=\"'Georgia', Ubuntu"), "{svg}");
        assert!(svg.contains("font-weight=\"700\" font-style=\"italic\" text-decoration=\"underline\""), "{svg}");
        assert!(parse_svg(&svg).is_ok());
    }

    #[test]
    fn highlighter_is_even() {
        let mut page = Page::new("p", PageSize::Custom([100.0, 100.0]));
        page.paper = Color::TRANSPARENT;
        page.elements.push(Element::Stroke(Stroke {
            id: 1,
            kind: InkKind::Highlighter,
            color: Color::rgba(0, 0, 255, 128),
            width: 20.0,
            points: vec![
                InkPoint::new(10.0, 50.0, 0.5),
                InkPoint::new(50.0, 50.0, 0.5),
                InkPoint::new(90.0, 50.0, 0.5),
            ],
        }));
        let img = page_rgba(&page, &Assets::new(), &RenderOptions { scale: 1.0, ..Default::default() }).unwrap();
        let a = img.get_pixel(30, 50)[3];
        let b = img.get_pixel(50, 50)[3];
        assert!(a.abs_diff(b) <= 2, "{a} vs {b}");
    }
}
