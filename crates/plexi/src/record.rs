//! Recording to an MP4: the canvas while in the notebook, the whole monitor while
//! drawing on screen, and both in one file if you switch between them mid-recording.
//!
//! Each frame is what Plexi itself just painted, read back from the GPU
//! ([`egui::Context::request_screenshot`]). In the notebook that is the frame. On screen,
//! Plexi's window is left out of screen captures for as long as the recording runs, so
//! the monitor can be captured as it looks underneath and Plexi's own layer (the ink,
//! the toolbar) laid over it. Worker threads compose and encode the frames, one stage
//! each so they overlap; when they fall behind, frames are dropped rather than piling
//! up in memory.

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

/// Encode frames until the sender is dropped. Two stages, each on its own thread so
/// they overlap: this one captures the screen under the frame (when drawing on screen)
/// and composes the two straight into the encoder's pixel format; the other encodes.
fn encode(rx: mpsc::Receiver<Frame>, path: &Path) -> Result<(), String> {
    let (to_encoder, frames) = mpsc::sync_channel::<(f64, Vec<u8>, [u32; 2])>(2);
    let (recycle, spare) = mpsc::channel::<Vec<u8>>();
    let out = path.to_owned();
    let encoder = std::thread::spawn(move || -> Result<(), String> {
        let mut video: Option<(VideoWriter, [u32; 2])> = None;
        let mut first = None;
        let mut scaled = Vec::new();
        for (time, bgra, size) in frames {
            let video_size = match &video {
                Some((_, s)) => *s,
                None => {
                    let s = video_size(size);
                    video = Some((VideoWriter::create(&out, s[0], s[1], FPS)?, s));
                    s
                }
            };
            let data = if size == video_size {
                &bgra[..]
            } else {
                // The window changed size, or the recording moved between the notebook
                // and the screen: scale to fit the video.
                let mut rgba = bgra.clone();
                for px in rgba.as_chunks_mut::<4>().0 {
                    px.swap(0, 2);
                }
                let pic = image::RgbaImage::from_raw(size[0], size[1], rgba).ok_or("bad frame")?;
                fit_bgra(&pic, video_size, &mut scaled);
                &scaled[..]
            };
            let t0 = *first.get_or_insert(time);
            if let Some((v, _)) = &mut video {
                v.write(data, time - t0, 1.0 / FPS as f64)?;
            }
            let _ = recycle.send(bgra);
        }
        match video {
            Some((v, _)) => v.finish(),
            None => Err("nothing was recorded".into()),
        }
    });

    let mut grabber: Option<platform::ScreenGrabber> = None;
    for frame in rx {
        let [w, h] = frame.layer.size;
        let under = match frame.under {
            Some(rect) => {
                if grabber.as_ref().is_none_or(|g| g.rect() != rect) {
                    grabber = Some(platform::ScreenGrabber::new(rect));
                }
                grabber.as_mut().and_then(|g| g.grab()).filter(|b| b.len() == w * h * 4)
            }
            None => None,
        };
        let mut buf = spare.try_recv().unwrap_or_default();
        compose_bgra(&frame.layer, under, &mut buf);
        if to_encoder.send((frame.time, buf, [w as u32, h as u32])).is_err() {
            break; // the encoder stopped: its error says why
        }
    }
    drop(to_encoder);
    encoder.join().map_err(|_| "the encoder crashed".to_owned())?
}

/// The frame in the encoder's format (blue, green, red, 255): Plexi's layer over the
/// captured monitor when drawing on screen, or over black (the notebook paints its own
/// background anyway). `under` is the monitor in that same format, the layer's size.
/// Split across the CPU's cores: at 4K this is a lot of pixels.
fn compose_bgra(layer: &egui::ColorImage, under: Option<&[u8]>, out: &mut Vec<u8>) {
    let [w, h] = layer.size;
    out.clear();
    out.resize(w * h * 4, 0);
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(1, 8);
    let rows = h.div_ceil(threads).max(1);
    std::thread::scope(|scope| {
        for (i, chunk) in out.chunks_mut(rows * w * 4).enumerate() {
            let first = i * rows * w;
            scope.spawn(move || {
                for (k, px) in chunk.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let src = layer.pixels[first + k];
                    let d = match under {
                        Some(u) => {
                            let at = (first + k) * 4;
                            [u[at], u[at + 1], u[at + 2]]
                        }
                        None => [0, 0, 0],
                    };
                    // `src` is premultiplied: src + dst * (1 - alpha).
                    let keep = 255 - src.a() as u32;
                    let mix = |s: u8, d: u8| (s as u32 + (d as u32 * keep + 127) / 255).min(255) as u8;
                    *px = [mix(src.b(), d[0]), mix(src.g(), d[1]), mix(src.r(), d[2]), 255];
                }
            });
        }
    });
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

/// Where a new recording goes: the Downloads folder, as "Plexi recording N.mp4" with
/// the first N not taken.
pub fn next_path() -> PathBuf {
    let dirs = directories::UserDirs::new();
    let dir = dirs
        .as_ref()
        .and_then(|d| d.download_dir().or(d.video_dir()).map(Path::to_path_buf))
        .unwrap_or_else(std::env::temp_dir);
    (1..)
        .map(|n| dir.join(format!("Plexi recording {n}.mp4")))
        .find(|p| !p.exists())
        .unwrap_or_else(|| dir.join("Plexi recording.mp4"))
}

/// The mouse pointer's recent path, for the cursor trail painted while recording.
#[derive(Default)]
pub struct Trail {
    /// (time in seconds, position in points), oldest first.
    points: std::collections::VecDeque<(f64, egui::Pos2)>,
}

impl Trail {
    /// Add where the pointer is at `now`, and forget points older than `length` seconds.
    pub fn push(&mut self, now: f64, pos: egui::Pos2, length: f32) {
        if self.points.back().is_none_or(|(_, p)| p.distance(pos) > 0.5) {
            self.points.push_back((now, pos));
        } else if let Some(last) = self.points.back_mut() {
            last.0 = now; // still: the dot stays, the streak drains away
        }
        while self.points.front().is_some_and(|(t, _)| now - t > length as f64) && self.points.len() > 1 {
            self.points.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.points.clear();
    }

    /// The streak, tapering and fading toward its tail, with a dot on the pointer. The
    /// pointer is only sampled once a frame, so the path is a smooth curve through the
    /// samples, and it's painted as one ribbon (soft-edged, no marks where the samples
    /// were, no darker overlaps).
    pub fn paint(&self, painter: &egui::Painter, now: f64, color: egui::Color32, size: f32, length: f32) {
        let Some(&(_, head)) = self.points.back() else { return };
        let fade = |t: f64| (1.0 - ((now - t) / length.max(0.01) as f64) as f32).clamp(0.0, 1.0);
        let path = smooth_path(&self.points.iter().copied().collect::<Vec<_>>());
        if path.len() >= 2 {
            let feather = 1.0 / painter.pixels_per_point();
            let mut mesh = egui::epaint::Mesh::default();
            for (k, &(t, p)) in path.iter().enumerate() {
                let f = fade(t);
                // Narrower than the dot, down to nothing at the tail.
                let w = size * 0.36 * f.powf(0.7);
                let c = color.gamma_multiply(f.powf(1.3) * 0.85);
                let prev = path[k.saturating_sub(1)].1;
                let next = path[(k + 1).min(path.len() - 1)].1;
                let n = (next - prev).normalized().rot90();
                let n = if n.is_finite() { n } else { egui::Vec2::ZERO };
                // Across the ribbon: soft edge, edge, edge, soft edge.
                for (off, col) in [
                    (-(w + feather), egui::Color32::TRANSPARENT),
                    (-w, c),
                    (w, c),
                    (w + feather, egui::Color32::TRANSPARENT),
                ] {
                    mesh.colored_vertex(p + n * off, col);
                }
                if k > 0 {
                    let (a, b) = ((k as u32 - 1) * 4, k as u32 * 4);
                    for j in 0..3 {
                        mesh.add_triangle(a + j, a + j + 1, b + j);
                        mesh.add_triangle(a + j + 1, b + j + 1, b + j);
                    }
                }
            }
            painter.add(egui::Shape::mesh(mesh));
        }
        painter.circle_filled(head, size / 2.0, color);
        painter.circle_stroke(head, size / 2.0, egui::Stroke::new(1.5, egui::Color32::from_white_alpha(200)));
    }
}

/// A Catmull-Rom curve through `pts` (time, position), in steps of about 2 points,
/// with times interpolated along it.
fn smooth_path(pts: &[(f64, egui::Pos2)]) -> Vec<(f64, egui::Pos2)> {
    let n = pts.len();
    if n < 3 {
        return pts.to_vec();
    }
    let mut out = Vec::new();
    for i in 0..n - 1 {
        let p0 = pts[i.saturating_sub(1)].1;
        let (p1, p2) = (pts[i].1, pts[i + 1].1);
        let p3 = pts[(i + 2).min(n - 1)].1;
        let steps = ((p1.distance(p2) / 2.0).ceil() as usize).clamp(1, 40);
        for s in 0..steps {
            let t = s as f32 / steps as f32;
            let (t2, t3) = (t * t, t * t * t);
            let at = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            let p = egui::pos2(at(p0.x, p1.x, p2.x, p3.x), at(p0.y, p1.y, p2.y, p3.y));
            out.push((pts[i].0 + (pts[i + 1].0 - pts[i].0) * t as f64, p));
        }
    }
    out.push(pts[n - 1]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trail_curves_through_its_samples() {
        // Three samples making a right angle: the path passes through each of them, in
        // small steps, and rounds the corner instead of meeting it.
        let pts = [(0.0, egui::pos2(0.0, 0.0)), (0.1, egui::pos2(100.0, 0.0)), (0.2, egui::pos2(100.0, 100.0))];
        let path = smooth_path(&pts);
        assert!(path.len() > 50);
        for (_, p) in &pts {
            assert!(path.iter().any(|(_, q)| q.distance(*p) < 0.01), "passes through {p:?}");
        }
        assert!(path.windows(2).all(|w| w[0].1.distance(w[1].1) < 4.0), "small steps");
        assert!(path.windows(2).all(|w| w[1].0 >= w[0].0), "times run forward");
        // Just before the corner it's already turning: off the straight line y = 0.
        let near = path.iter().find(|(_, q)| q.x > 90.0 && q.x < 99.0).unwrap().1;
        assert!(near.y.abs() > 0.1 || near.x > 95.0, "{near:?}");
    }

    #[test]
    fn the_trail_keeps_only_its_recent_path() {
        let mut t = Trail::default();
        for i in 0..20 {
            t.push(i as f64 * 0.05, egui::pos2(i as f32 * 10.0, 0.0), 0.3);
        }
        // 0.3 s at 0.05 s apart: about 7 points left, the newest last.
        assert!((6..=8).contains(&t.points.len()), "{}", t.points.len());
        assert_eq!(t.points.back().unwrap().1.x, 190.0);
        // Holding still drains the streak down to just the dot.
        for i in 0..20 {
            t.push(1.0 + i as f64 * 0.05, egui::pos2(190.0, 0.0), 0.3);
        }
        assert_eq!(t.points.len(), 1);
    }

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

    /// How long each step of a frame takes, on synthetic frames (no screen capture).
    #[cfg(windows)]
    #[test]
    #[ignore = "timing only"]
    fn time_the_frame_steps() {
        for [w, h] in [[1920u32, 1080u32], [2560, 1440], [3840, 2160]] {
            let layer = egui::ColorImage::filled([w as usize, h as usize], egui::Color32::from_white_alpha(30));
            let screen = vec![90u8; (w * h * 4) as usize];
            let n = 20;
            let mut bgra = Vec::new();
            let t = Instant::now();
            for _ in 0..n {
                compose_bgra(&layer, Some(&screen), &mut bgra);
            }
            let compose_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            let path = std::env::temp_dir().join(format!("plexi-bench-{w}.mp4"));
            let mut v = VideoWriter::create(&path, w, h, FPS).unwrap();
            let t = Instant::now();
            for i in 0..n {
                // A changing picture, so the encoder has real work.
                for (k, px) in bgra.iter_mut().enumerate().step_by(97) {
                    *px = (k + i * 13) as u8;
                }
                v.write(&bgra, i as f64 / 30.0, 1.0 / 30.0).unwrap();
            }
            let enc_ms = t.elapsed().as_secs_f64() * 1000.0 / n as f64;
            v.finish().unwrap();
            let _ = std::fs::remove_file(&path);
            eprintln!("{w}x{h}: compose+convert {compose_ms:.1} ms, encode {enc_ms:.1} ms");
        }
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
        let mut out = Vec::new();
        compose_bgra(&layer, None, &mut out);
        assert!((out[0] as i32 - 128).abs() <= 1);
        assert_eq!(out[4..8], [0, 0, 0, 255]);
        // Over a captured screen (blue-green-red bytes): the transparent pixel shows it.
        compose_bgra(&layer, Some(&[10, 20, 30, 255, 10, 20, 30, 255]), &mut out);
        assert_eq!(out[4..8], [10, 20, 30, 255]);
        assert!(out[0] > 128, "half white over the screen is lighter than either alone");

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
