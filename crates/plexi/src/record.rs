//! Recording to an MP4: the canvas while in the notebook, the whole monitor while
//! drawing on screen, and both in one file if you switch between them mid-recording.
//!
//! Each frame is what Plexi itself just painted, read back from the GPU
//! ([`egui::Context::request_screenshot`]). In the notebook that is the frame. On screen,
//! Plexi's window is left out of screen captures for as long as the recording runs, so
//! the monitor can be captured as it looks underneath and Plexi's own layer (the ink,
//! the toolbar) laid over it. A worker thread composes and encodes the frames; when it
//! falls behind, frames are dropped rather than piling up in memory.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use crate::platform::{self, PxRect, VideoWriter};

/// Frames a second asked for. The video plays at whatever rate frames actually came.
pub const FPS: u32 = 30;
/// The largest video, in pixels: bigger frames are scaled down to fit (the encoder
/// tops out around 4K).
const MAX_SIZE: [u32; 2] = [3840, 2160];

/// A recording in progress.
pub struct Recording {
    started: Instant,
    /// Seconds after `started` the next frame is due.
    next_frame: f64,
    tx: mpsc::SyncSender<Frame>,
    worker: std::thread::JoinHandle<()>,
}

/// One frame on its way to the worker.
struct Frame {
    /// Seconds since the recording started.
    time: f64,
    /// What Plexi painted: premultiplied RGBA, the window's size in physical pixels.
    layer: Arc<egui::ColorImage>,
    /// The monitor area under the window when drawing on screen, captured on the worker
    /// and shown beneath `layer`; `None` in the notebook.
    under: Option<PxRect>,
}

impl Recording {
    /// Start recording to `path`. `done` is called on the worker thread once the file is
    /// finished (or failed).
    pub fn start(path: PathBuf, done: impl FnOnce(Result<PathBuf, String>) + Send + 'static) -> Self {
        let (tx, rx) = mpsc::sync_channel(3);
        let worker = std::thread::spawn(move || done(encode(rx, &path).map(|()| path)));
        Self { started: Instant::now(), next_frame: 0.0, tx, worker }
    }

    pub fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// Called once a frame: when the next video frame is due, asks for this frame's
    /// pixels, and keeps frames coming while nothing else changes. `under` is the
    /// monitor area to capture beneath Plexi's window, when drawing on screen.
    pub fn tick(&mut self, ctx: &egui::Context, under: Option<PxRect>) {
        let now = self.started.elapsed().as_secs_f64();
        if now >= self.next_frame {
            // Keep to the frame rate's grid, without trying to catch up on missed frames.
            self.next_frame = (self.next_frame + 1.0 / FPS as f64).max(now);
            let tx = self.tx.clone();
            ctx.request_screenshot(move |layer| {
                let _ = tx.try_send(Frame { time: now, layer, under });
            });
        }
        ctx.request_repaint_after(Duration::from_secs_f64((self.next_frame - now).max(0.0)));
    }

    /// Stop and let the worker write out the file. The handle is for waiting on it at
    /// exit; `done` reports the result either way.
    pub fn stop(self) -> std::thread::JoinHandle<()> {
        drop(self.tx);
        self.worker
    }
}

/// Encode frames until the sender is dropped.
fn encode(rx: mpsc::Receiver<Frame>, path: &Path) -> Result<(), String> {
    let mut video: Option<(VideoWriter, [u32; 2])> = None;
    let mut bgra = Vec::new();
    let mut first = None;
    for frame in rx {
        let picture = compose(&frame);
        let size = match &video {
            Some((_, size)) => *size,
            None => {
                let size = video_size([picture.width(), picture.height()]);
                video = Some((VideoWriter::create(path, size[0], size[1], FPS)?, size));
                size
            }
        };
        fit_bgra(&picture, size, &mut bgra);
        let t0 = *first.get_or_insert(frame.time);
        if let Some((v, _)) = &mut video {
            v.write(&bgra, frame.time - t0, 1.0 / FPS as f64)?;
        }
    }
    match video {
        Some((v, _)) => v.finish(),
        None => Err("nothing was recorded".into()),
    }
}

/// The frame as an opaque picture: Plexi's layer over the captured monitor when
/// drawing on screen, or over black (the notebook paints its own background anyway).
fn compose(frame: &Frame) -> image::RgbaImage {
    let [w, h] = frame.layer.size;
    let mut under = frame
        .under
        .and_then(platform::capture)
        .filter(|u| (u.width() as usize, u.height() as usize) == (w, h))
        .unwrap_or_else(|| image::RgbaImage::from_pixel(w as u32, h as u32, image::Rgba([0, 0, 0, 255])));
    for (dst, src) in under.pixels_mut().zip(frame.layer.pixels.iter()) {
        // `src` is premultiplied: src + dst * (1 - alpha).
        let keep = 255 - src.a() as u32;
        let mix = |s: u8, d: u8| (s as u32 + (d as u32 * keep + 127) / 255).min(255) as u8;
        *dst = image::Rgba([mix(src.r(), dst[0]), mix(src.g(), dst[1]), mix(src.b(), dst[2]), 255]);
    }
    under
}

/// The video size for frames of `size`: scaled down to [`MAX_SIZE`] if bigger, and
/// even on both sides, as H.264 needs.
fn video_size([w, h]: [u32; 2]) -> [u32; 2] {
    let s = (MAX_SIZE[0] as f64 / w as f64).min(MAX_SIZE[1] as f64 / h as f64).min(1.0);
    let even = |v: u32| ((v as f64 * s) as u32 & !1).max(2);
    [even(w), even(h)]
}

/// Put `picture` into a `size` frame as blue-green-red-unused bytes, scaled down to
/// fit (keeping its proportions, centered on black) if it isn't already that size.
fn fit_bgra(picture: &image::RgbaImage, size: [u32; 2], out: &mut Vec<u8>) {
    let [w, h] = size;
    out.clear();
    out.resize((w * h * 4) as usize, 0);
    let (pw, ph) = picture.dimensions();
    let scaled;
    let (src, x0, y0) = if pw > w || ph > h {
        let s = (w as f64 / pw as f64).min(h as f64 / ph as f64);
        let (sw, sh) = (((pw as f64 * s) as u32).clamp(1, w), ((ph as f64 * s) as u32).clamp(1, h));
        scaled = image::imageops::resize(picture, sw, sh, image::imageops::FilterType::Triangle);
        (&scaled, (w - sw) / 2, (h - sh) / 2)
    } else {
        // Same size, or smaller: centered as it is.
        (picture, (w - pw) / 2, (h - ph) / 2)
    };
    let row = (w * 4) as usize;
    for (y, line) in src.rows().enumerate() {
        let start = (y0 as usize + y) * row + x0 as usize * 4;
        for (x, px) in line.enumerate() {
            let at = start + x * 4;
            out[at..at + 4].copy_from_slice(&[px[2], px[1], px[0], 255]);
        }
    }
}

/// Where a new recording goes: the Videos folder, as "Plexi recording N.mp4" with the
/// first N not taken.
pub fn next_path() -> PathBuf {
    let dirs = directories::UserDirs::new();
    let dir = dirs
        .as_ref()
        .and_then(|d| d.video_dir().or(d.document_dir()).map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    (1..)
        .map(|n| dir.join(format!("Plexi recording {n}.mp4")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join("Plexi recording.mp4"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run a recording the way the app does, answering each frame's screenshot request
    /// with a picture of our own (there's no GPU in tests), and check a real video comes
    /// out the other end.
    #[cfg(windows)]
    #[test]
    fn a_recording_ends_up_as_a_video_file() {
        let dir = std::env::temp_dir().join(format!("plexi-record-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        if let Err(e) = VideoWriter::create(&dir.join("probe.mp4"), 64, 48, FPS) {
            eprintln!("no video encoder here ({e}); skipping");
            return;
        }
        let path = dir.join("rec.mp4");
        let (done_tx, done_rx) = mpsc::channel();
        let mut rec = Recording::start(path.clone(), move |r| done_tx.send(r).unwrap());
        let ctx = egui::Context::default();
        let mut answered = 0;
        for i in 0..12 {
            let raw = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(67.0, 51.0))),
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| rec.tick(ui.ctx(), None));
            out.textures_delta.clear();
            for cmd in out.viewport_output[&egui::ViewportId::ROOT].commands.clone() {
                if let egui::ViewportCommand::Screenshot(cb) = cmd {
                    // An odd-sized window: the video rounds it down to even.
                    let shade = (i * 20) as u8;
                    cb.complete(Arc::new(egui::ColorImage::filled([67, 51], egui::Color32::from_rgb(shade, 90, 200))));
                    answered += 1;
                }
            }
            std::thread::sleep(Duration::from_millis(40));
        }
        assert!(answered >= 8, "a frame was asked for about every 33ms: {answered}");
        rec.stop().join().unwrap();
        let saved = done_rx.recv().unwrap().expect("the recording was saved");
        assert_eq!(saved, path);
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(len > 500, "{len} bytes");

        let probe = std::process::Command::new("ffprobe")
            .args(["-v", "error", "-select_streams", "v:0", "-count_frames"])
            .args(["-show_entries", "stream=width,height,nb_read_frames", "-of", "csv=p=0"])
            .arg(&path)
            .output();
        if let Ok(out) = probe
            && out.status.success()
        {
            let info = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            let parts: Vec<&str> = info.split(',').collect();
            assert_eq!(parts[..2], ["66", "50"], "{info}");
            assert!(parts[2].parse::<u32>().unwrap_or(0) >= 6, "frames: {info}");
        } else {
            eprintln!("no ffprobe; only checked the file was written");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn video_sizes_are_even_and_capped() {
        assert_eq!(video_size([1001, 701]), [1000, 700]);
        assert_eq!(video_size([7680, 4320]), [3840, 2160]);
        assert_eq!(video_size([1920, 1080]), [1920, 1080]);
    }

    #[test]
    fn the_layer_goes_over_the_monitor_and_frames_fit_the_video() {
        // Half-transparent white over black is mid gray; fully transparent shows black.
        let layer =
            egui::ColorImage::new([2, 1], vec![egui::Color32::from_white_alpha(128), egui::Color32::TRANSPARENT]);
        let frame = Frame { time: 0.0, layer: Arc::new(layer), under: None };
        let pic = compose(&frame);
        assert!((pic.get_pixel(0, 0)[0] as i32 - 128).abs() <= 1);
        assert_eq!(pic.get_pixel(1, 0).0, [0, 0, 0, 255]);

        // A smaller picture is centered as it is; a bigger one is scaled down to fit.
        let red = image::RgbaImage::from_pixel(2, 2, image::Rgba([255, 0, 0, 255]));
        let mut out = Vec::new();
        fit_bgra(&red, [4, 4], &mut out);
        let px = |out: &[u8], x: usize, y: usize| out[(y * 4 + x) * 4..(y * 4 + x) * 4 + 4].to_vec();
        assert_eq!(px(&out, 0, 0), [0, 0, 0, 0], "black around it");
        assert_eq!(px(&out, 1, 1), [0, 0, 255, 255], "red, in blue-green-red order");
        let big = image::RgbaImage::from_pixel(8, 4, image::Rgba([0, 255, 0, 255]));
        fit_bgra(&big, [4, 4], &mut out);
        assert_eq!(px(&out, 0, 0), [0, 0, 0, 0], "letterboxed above");
        assert_eq!(px(&out, 0, 1)[1], 255, "the wide picture fills the middle rows");
    }
}
