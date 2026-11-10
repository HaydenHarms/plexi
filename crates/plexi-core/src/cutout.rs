//! Taking the plain background out of a picture: the color that runs in from its edges
//! (a white page behind a logo, a flat backdrop behind a product shot) becomes
//! transparent, with a soft edge where it meets the subject.
//!
//! It fills inward from the border, so the same color inside the subject (a white shirt
//! on a white backdrop, say) stays put as long as an outline separates it from the
//! background.

use std::collections::VecDeque;
use std::io::Cursor;

/// How different (RGB distance, 0..=441) a pixel may be from the background color and
/// still count as background.
const TOLERANCE: f32 = 52.0;
/// Pixels next to the background and up to this much further off it fade out partly,
/// so the cut edge isn't jagged.
const FEATHER: f32 = 40.0;

fn distance(a: [u8; 4], b: [f32; 3]) -> f32 {
    let d = |i: usize| a[i] as f32 - b[i];
    (d(0) * d(0) + d(1) * d(1) + d(2) * d(2)).sqrt()
}

/// The color most of the border shares, if it shares one.
fn border_color(img: &image::RgbaImage) -> Option<[f32; 3]> {
    let (w, h) = img.dimensions();
    let mut border = Vec::new();
    for x in 0..w {
        border.push(img.get_pixel(x, 0).0);
        border.push(img.get_pixel(x, h - 1).0);
    }
    for y in 1..h - 1 {
        border.push(img.get_pixel(0, y).0);
        border.push(img.get_pixel(w - 1, y).0);
    }
    let opaque: Vec<[u8; 4]> = border.iter().copied().filter(|p| p[3] > 128).collect();
    if opaque.len() * 4 < border.len() {
        return None; // the edges are already see-through
    }
    // The most common color, in coarse buckets so noise and JPEG fuzz agree.
    let mut counts = std::collections::HashMap::<[u8; 3], usize>::new();
    for p in &opaque {
        *counts.entry([p[0] / 24, p[1] / 24, p[2] / 24]).or_default() += 1;
    }
    let (bucket, n) = counts.into_iter().max_by_key(|(_, n)| *n)?;
    if n * 3 < opaque.len() {
        return None; // no one color around the edge: a photo, not a backdrop
    }
    let members: Vec<&[u8; 4]> = opaque.iter().filter(|p| [p[0] / 24, p[1] / 24, p[2] / 24] == bucket).collect();
    let avg = |i: usize| members.iter().map(|p| p[i] as f32).sum::<f32>() / members.len() as f32;
    Some([avg(0), avg(1), avg(2)])
}

/// The picture in `bytes` with its background taken out, as a PNG. `None` if it has no
/// plain background to take out (or isn't a picture).
pub fn remove_background(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut img = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = img.dimensions();
    if w < 3 || h < 3 {
        return None;
    }
    let bg = border_color(&img)?;
    let (wu, hu) = (w as usize, h as usize);
    let idx = |x: usize, y: usize| y * wu + x;
    let px = |img: &image::RgbaImage, i: usize| img.get_pixel((i % wu) as u32, (i / wu) as u32).0;
    let is_bg = |p: [u8; 4]| p[3] == 0 || distance(p, bg) <= TOLERANCE;

    // Fill in from every background-colored pixel on the border.
    let mut removed = vec![false; wu * hu];
    let mut queue = VecDeque::new();
    let seed = |x: usize, y: usize, removed: &mut Vec<bool>, queue: &mut VecDeque<usize>| {
        let i = idx(x, y);
        if !removed[i] && is_bg(px(&img, i)) {
            removed[i] = true;
            queue.push_back(i);
        }
    };
    for x in 0..wu {
        seed(x, 0, &mut removed, &mut queue);
        seed(x, hu - 1, &mut removed, &mut queue);
    }
    for y in 0..hu {
        seed(0, y, &mut removed, &mut queue);
        seed(wu - 1, y, &mut removed, &mut queue);
    }
    while let Some(i) = queue.pop_front() {
        let (x, y) = (i % wu, i / wu);
        let mut visit = |j: usize| {
            if !removed[j] && is_bg(px(&img, j)) {
                removed[j] = true;
                queue.push_back(j);
            }
        };
        if x > 0 {
            visit(i - 1);
        }
        if x + 1 < wu {
            visit(i + 1);
        }
        if y > 0 {
            visit(i - wu);
        }
        if y + 1 < hu {
            visit(i + wu);
        }
    }
    let count = removed.iter().filter(|r| **r).count();
    if count == 0 || count * 100 > wu * hu * 99 {
        return None; // nothing to take out, or nothing would be left
    }

    // Soften the edge: pixels touching the background that are close to its color
    // fade part of the way out.
    let mut alpha: Vec<u8> = (0..wu * hu).map(|i| if removed[i] { 0 } else { px(&img, i)[3] }).collect();
    for y in 0..hu {
        for x in 0..wu {
            let i = idx(x, y);
            if removed[i] {
                continue;
            }
            let touches = (x > 0 && removed[i - 1])
                || (x + 1 < wu && removed[i + 1])
                || (y > 0 && removed[i - wu])
                || (y + 1 < hu && removed[i + wu]);
            if touches {
                let d = distance(px(&img, i), bg);
                let keep = ((d - TOLERANCE) / FEATHER).clamp(0.0, 1.0);
                alpha[i] = (alpha[i] as f32 * keep.max(0.15)).round() as u8;
            }
        }
    }
    for (i, p) in img.pixels_mut().enumerate() {
        p.0[3] = alpha[i];
    }
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// Where a picture can be seen: a coarse map of how opaque it is, so clicks on its
/// see-through parts go to whatever is under it instead.
#[derive(Clone, Debug)]
pub struct AlphaMask {
    w: usize,
    h: usize,
    /// The most opaque pixel in each cell, rows top to bottom.
    cells: Vec<u8>,
}

/// Cells across the longer side of an [`AlphaMask`].
const MASK_SIDE: u32 = 256;
/// How opaque a pixel must be to count as something you can see and click.
const VISIBLE: u8 = 24;

impl AlphaMask {
    /// The mask of a picture's (first frame's) bytes. `None` when it can't be read or
    /// shows all over, so it needs no mask.
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        let img = image::load_from_memory(bytes).ok()?.to_rgba8();
        let (iw, ih) = img.dimensions();
        if iw == 0 || ih == 0 || img.pixels().all(|p| p[3] >= VISIBLE) {
            return None;
        }
        let k = (iw.max(ih) as f32 / MASK_SIDE as f32).max(1.0);
        let (w, h) = (((iw as f32 / k).ceil() as usize).max(1), ((ih as f32 / k).ceil() as usize).max(1));
        let mut cells = vec![0u8; w * h];
        for (x, y, p) in img.enumerate_pixels() {
            let c = ((y as f32 / k) as usize).min(h - 1) * w + ((x as f32 / k) as usize).min(w - 1);
            cells[c] = cells[c].max(p[3]);
        }
        Some(Self { w, h, cells })
    }

    /// Whether anything shows at `(u, v)` (fractions across and down the picture) or
    /// within `slack` of it (fractions too).
    pub fn shows(&self, u: f32, v: f32, slack: [f32; 2]) -> bool {
        let cell = |f: f32, n: usize| (f * n as f32).floor() as i64;
        let (x0, x1) = (cell(u - slack[0], self.w), cell(u + slack[0], self.w));
        let (y0, y1) = (cell(v - slack[1], self.h), cell(v + slack[1], self.h));
        (y0.max(0)..=y1.min(self.h as i64 - 1)).any(|y| {
            (x0.max(0)..=x1.min(self.w as i64 - 1)).any(|x| self.cells[y as usize * self.w + x as usize] >= VISIBLE)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mask_knows_where_a_picture_shows() {
        let mut img = image::RgbaImage::new(400, 200);
        for y in 20..40 {
            for x in 300..320 {
                img.put_pixel(x, y, image::Rgba([200, 30, 30, 255]));
            }
        }
        let mut png = Cursor::new(Vec::new());
        img.write_to(&mut png, image::ImageFormat::Png).unwrap();
        let m = AlphaMask::from_bytes(png.get_ref()).expect("mostly see-through");
        assert!(m.shows(310.0 / 400.0, 30.0 / 200.0, [0.0, 0.0]));
        assert!(!m.shows(0.5, 0.5, [0.0, 0.0]), "the empty middle");
        assert!(m.shows(0.74, 0.15, [0.01, 0.01]), "near the dot, within the slack");
        // An opaque picture needs no mask.
        let full = image::RgbaImage::from_pixel(10, 10, image::Rgba([1, 2, 3, 255]));
        let mut png = Cursor::new(Vec::new());
        full.write_to(&mut png, image::ImageFormat::Png).unwrap();
        assert!(AlphaMask::from_bytes(png.get_ref()).is_none());
    }

    fn png(img: &image::RgbaImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn a_plain_backdrop_goes_and_the_subject_stays() {
        // A white picture with a black ring, and white again inside the ring.
        let mut img = image::RgbaImage::from_pixel(40, 40, image::Rgba([250, 250, 250, 255]));
        for y in 0..40 {
            for x in 0..40 {
                let d = ((x as f32 - 20.0).powi(2) + (y as f32 - 20.0).powi(2)).sqrt();
                if (8.0..12.0).contains(&d) {
                    img.put_pixel(x, y, image::Rgba([10, 10, 10, 255]));
                }
            }
        }
        let out = image::load_from_memory(&remove_background(&png(&img)).unwrap()).unwrap().to_rgba8();
        assert_eq!(out.get_pixel(1, 1).0[3], 0, "the backdrop is gone");
        assert_eq!(out.get_pixel(20, 10).0[3], 255, "the ring stays");
        assert_eq!(out.get_pixel(20, 20).0[3], 255, "white inside the ring isn't backdrop");
    }

    #[test]
    fn a_busy_or_blank_picture_is_left_alone() {
        let blank = image::RgbaImage::from_pixel(10, 10, image::Rgba([255, 255, 255, 255]));
        assert!(remove_background(&png(&blank)).is_none(), "nothing would be left");
        let noisy = image::RgbaImage::from_fn(30, 30, |x, y| {
            image::Rgba([(x * 37 % 256) as u8, (y * 91 % 256) as u8, ((x * y) % 256) as u8, 255])
        });
        assert!(remove_background(&png(&noisy)).is_none(), "no one color around the edge");
        assert!(remove_background(b"not a picture").is_none());
    }
}
