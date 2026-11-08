//! Decoding animated GIF assets, and the pure timing math that picks a frame.
//!
//! Textures are the UI crate's problem (it owns the GPU handles), but decoding the
//! frames and working out which one is showing at a given time don't need a UI at
//! all, so they live here where they can be tested headless.

use image::AnimationDecoder;
use image::codecs::gif::GifDecoder;

/// A delay shorter than this is rounded up, the way browsers treat a GIF's 0 (or
/// otherwise too-short) frame delays as ~100ms instead of flickering as fast as the
/// decoder allows.
pub const MIN_DELAY_MS: u32 = 100;

/// One decoded GIF frame: straight (non-premultiplied) RGBA8 pixels, and how long it
/// stays on screen before the next one.
pub struct GifFrame {
    pub size: [u32; 2],
    pub rgba: Vec<u8>,
    pub delay_ms: u32,
}

/// Frame data past this many raw bytes isn't worth holding in GPU memory for an
/// animation; such a GIF still shows (its first frame, via the ordinary image path).
pub const MAX_ANIMATION_BYTES: usize = 64 * 1024 * 1024;

/// Decode every frame of a GIF asset, or `None` if it isn't worth animating: not a
/// GIF, failed to decode, only one frame (nothing to animate), or large enough that
/// holding every frame as a texture would be wasteful.
pub fn decode_frames(bytes: &[u8]) -> Option<Vec<GifFrame>> {
    if !bytes.starts_with(b"GIF8") {
        return None;
    }
    let decoder = GifDecoder::new(std::io::Cursor::new(bytes)).ok()?;
    let mut out = Vec::new();
    let mut total_bytes: usize = 0;
    for frame in decoder.into_frames() {
        let frame = frame.ok()?;
        let (num, den) = frame.delay().numer_denom_ms();
        let delay_ms = num.checked_div(den).unwrap_or(0);
        let delay_ms = if delay_ms < MIN_DELAY_MS { MIN_DELAY_MS } else { delay_ms };
        let buf = frame.into_buffer();
        let (w, h) = (buf.width(), buf.height());
        total_bytes += w as usize * h as usize * 4;
        if total_bytes > MAX_ANIMATION_BYTES {
            return None;
        }
        out.push(GifFrame { size: [w, h], rgba: buf.into_raw(), delay_ms });
    }
    (out.len() > 1).then_some(out)
}

/// Which of `delays_ms` is showing `elapsed_ms` into an infinitely looping animation
/// whose frames total `total_ms`, and how many ms until the next one starts (handy
/// for scheduling a repaint). `delays_ms` must be non-empty and sum to `total_ms`.
pub fn frame_at(delays_ms: &[u32], total_ms: u32, elapsed_ms: f64) -> (usize, f64) {
    if delays_ms.is_empty() || total_ms == 0 {
        return (0, f64::INFINITY);
    }
    let t = elapsed_ms.rem_euclid(total_ms as f64);
    let mut acc = 0.0;
    for (i, &d) in delays_ms.iter().enumerate() {
        let next = acc + d as f64;
        if t < next {
            return (i, next - t);
        }
        acc = next;
    }
    (delays_ms.len() - 1, 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::codecs::gif::GifEncoder;
    use image::{Delay, Frame, RgbaImage};

    /// A tiny in-memory GIF with `n` frames, each a different solid color, holding
    /// for `delay_ms` (0 for the first frame, to exercise the too-short-delay clamp).
    fn make_gif(n: u32, delay_ms: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut bytes);
            for i in 0..n {
                let color = [i as u8 * 50, 0, 255 - i as u8 * 50, 255];
                let img = RgbaImage::from_pixel(4, 4, image::Rgba(color));
                let delay =
                    if i == 0 { Delay::from_numer_denom_ms(0, 1) } else { Delay::from_numer_denom_ms(delay_ms, 1) };
                enc.encode_frame(Frame::from_parts(img, 0, 0, delay)).unwrap();
            }
        }
        bytes
    }

    #[test]
    fn decodes_every_frame_with_its_delay() {
        let bytes = make_gif(3, 150);
        let frames = decode_frames(&bytes).expect("a multi-frame gif decodes");
        assert_eq!(frames.len(), 3);
        assert_eq!(frames[0].size, [4, 4]);
        // The first frame's 0ms delay is clamped up like a browser would.
        assert_eq!(frames[0].delay_ms, MIN_DELAY_MS);
        assert_eq!(frames[1].delay_ms, 150);
        assert_eq!(frames[2].delay_ms, 150);
        // The colors round-trip (first pixel of each frame).
        assert_eq!(&frames[1].rgba[0..4], &[50, 0, 205, 255]);
    }

    #[test]
    fn a_single_frame_gif_is_not_treated_as_animated() {
        let bytes = make_gif(1, 100);
        assert!(decode_frames(&bytes).is_none());
    }

    #[test]
    fn non_gif_bytes_decode_to_nothing() {
        assert!(decode_frames(b"not a gif").is_none());
    }

    #[test]
    fn frame_at_time_picks_the_right_frame_and_countdown() {
        let delays = [100u32, 200, 50];
        let total = 350;
        assert_eq!(frame_at(&delays, total, 0.0), (0, 100.0));
        assert_eq!(frame_at(&delays, total, 99.0), (0, 1.0));
        assert_eq!(frame_at(&delays, total, 100.0), (1, 200.0));
        assert_eq!(frame_at(&delays, total, 250.0), (1, 50.0));
        assert_eq!(frame_at(&delays, total, 300.0), (2, 50.0));
        // Loops back around past the total.
        assert_eq!(frame_at(&delays, total, 350.0), (0, 100.0));
        assert_eq!(frame_at(&delays, total, 450.0), (1, 200.0));
    }
}
