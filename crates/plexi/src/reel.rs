//! The animated tour in the README (`docs/tour.gif`), recorded headlessly: the real app
//! runs in the test harness with scripted input, a small software rasterizer paints each
//! egui frame, and ffmpeg turns the frames into a GIF.
//!
//! `cargo test -p plexi --release -- --ignored record_tour`

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use egui::epaint::{ClippedPrimitive, Primitive, Vertex};
use egui::{Color32, Context, Event, Modifiers, PointerButton, Pos2, RawInput, TextureId, pos2, vec2};

use crate::app::{PlexiApp, StartOptions};

/// Window size in points, and how many pixels each point gets before downscaling.
const SIZE: [f32; 2] = [960.0, 600.0];
const PPP: f32 = 2.0;
const FPS: f64 = 30.0;

/// RGBA texture, premultiplied, as egui hands it over.
#[derive(Clone)]
struct Texture {
    size: [usize; 2],
    pixels: Vec<Color32>,
    linear: bool,
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>, // premultiplied, 0..1, gamma space like egui_glow
}

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![[0.0, 0.0, 0.0, 1.0]; w * h] }
    }

    fn paint(&mut self, prims: &[ClippedPrimitive], textures: &HashMap<TextureId, Arc<Texture>>) {
        for p in prims {
            let Primitive::Mesh(mesh) = &p.primitive else { continue };
            let Some(tex) = textures.get(&mesh.texture_id) else { continue };
            let clip = p.clip_rect;
            let clip = [
                (clip.min.x * PPP).max(0.0) as usize,
                (clip.min.y * PPP).max(0.0) as usize,
                ((clip.max.x * PPP).ceil() as usize).min(self.w),
                ((clip.max.y * PPP).ceil() as usize).min(self.h),
            ];
            for t in mesh.indices.as_chunks::<3>().0 {
                let v = [0, 1, 2].map(|i| &mesh.vertices[t[i] as usize]);
                self.triangle(v, tex, clip);
            }
        }
    }

    fn triangle(&mut self, v: [&Vertex; 3], tex: &Texture, clip: [usize; 4]) {
        let p = v.map(|v| [v.pos.x * PPP, v.pos.y * PPP]);
        let area = (p[1][0] - p[0][0]) * (p[2][1] - p[0][1]) - (p[2][0] - p[0][0]) * (p[1][1] - p[0][1]);
        if area.abs() < 1e-6 {
            return;
        }
        let min_x = (p.iter().map(|q| q[0]).fold(f32::MAX, f32::min).floor().max(0.0) as usize).max(clip[0]);
        let min_y = (p.iter().map(|q| q[1]).fold(f32::MAX, f32::min).floor().max(0.0) as usize).max(clip[1]);
        let max_x = (p.iter().map(|q| q[0]).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(clip[2]);
        let max_y = (p.iter().map(|q| q[1]).fold(f32::MIN, f32::max).ceil().max(0.0) as usize).min(clip[3]);
        let col = v.map(|v| v.color.to_array().map(|c| c as f32 / 255.0));
        for y in min_y..max_y {
            for x in min_x..max_x {
                let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
                let edge = |a: [f32; 2], b: [f32; 2]| (b[0] - a[0]) * (cy - a[1]) - (cx - a[0]) * (b[1] - a[1]);
                let w0 = edge(p[1], p[2]) / area;
                let w1 = edge(p[2], p[0]) / area;
                let w2 = 1.0 - w0 - w1;
                if w0 < -1e-4 || w1 < -1e-4 || w2 < -1e-4 {
                    continue;
                }
                let u = w0 * v[0].uv.x + w1 * v[1].uv.x + w2 * v[2].uv.x;
                let t = w0 * v[0].uv.y + w1 * v[1].uv.y + w2 * v[2].uv.y;
                let s = tex.sample(u, t);
                let mut src = [0.0; 4];
                for c in 0..4 {
                    src[c] = (w0 * col[0][c] + w1 * col[1][c] + w2 * col[2][c]) * s[c];
                }
                let d = &mut self.px[y * self.w + x];
                for c in 0..4 {
                    d[c] = src[c] + d[c] * (1.0 - src[3]);
                }
            }
        }
    }

    /// Downscale by `PPP` (box filter) into straight RGB.
    fn to_image(&self) -> image::RgbImage {
        let k = PPP as usize;
        let (w, h) = (self.w / k, self.h / k);
        image::RgbImage::from_fn(w as u32, h as u32, |x, y| {
            let mut acc = [0.0; 3];
            for dy in 0..k {
                for dx in 0..k {
                    let p = self.px[(y as usize * k + dy) * self.w + x as usize * k + dx];
                    for c in 0..3 {
                        acc[c] += p[c];
                    }
                }
            }
            image::Rgb(acc.map(|a| (a / (k * k) as f32 * 255.0).round().clamp(0.0, 255.0) as u8))
        })
    }
}

impl Texture {
    fn sample(&self, u: f32, v: f32) -> [f32; 4] {
        let [w, h] = self.size;
        let fetch = |x: isize, y: isize| {
            let x = x.clamp(0, w as isize - 1) as usize;
            let y = y.clamp(0, h as isize - 1) as usize;
            self.pixels[y * w + x].to_array().map(|c| c as f32 / 255.0)
        };
        let (x, y) = (u * w as f32 - 0.5, v * h as f32 - 0.5);
        if !self.linear {
            return fetch(x.round() as isize, y.round() as isize);
        }
        let (x0, y0) = (x.floor(), y.floor());
        let (fx, fy) = (x - x0, y - y0);
        let (x0, y0) = (x0 as isize, y0 as isize);
        let [a, b, c, d] = [fetch(x0, y0), fetch(x0 + 1, y0), fetch(x0, y0 + 1), fetch(x0 + 1, y0 + 1)];
        let mut out = [0.0; 4];
        for i in 0..4 {
            out[i] = (a[i] * (1.0 - fx) + b[i] * fx) * (1.0 - fy) + (c[i] * (1.0 - fx) + d[i] * fx) * fy;
        }
        out
    }
}

/// Apply a frame's texture uploads and patches.
fn upload(textures: &mut HashMap<TextureId, Arc<Texture>>, delta: &mut egui::TexturesDelta) {
    for (id, deltas) in std::mem::take(&mut delta.set) {
        for d in deltas {
            let egui::ImageData::Color(img) = &d.image;
            let linear = d.options.magnification == egui::TextureFilter::Linear;
            match d.pos {
                None => {
                    textures.insert(id, Arc::new(Texture { size: img.size, pixels: img.pixels.clone(), linear }));
                }
                Some([x0, y0]) => {
                    let t = Arc::make_mut(textures.get_mut(&id).expect("patch for a known texture"));
                    for y in 0..img.size[1] {
                        for x in 0..img.size[0] {
                            t.pixels[(y0 + y) * t.size[0] + x0 + x] = img.pixels[y * img.size[0] + x];
                        }
                    }
                }
            }
        }
    }
}

/// The app plus everything needed to feed it input and keep its frames.
struct Recorder {
    /// What shows through the window while it is transparent (draw-on-screen mode):
    /// stands in for the desktop, which a test must never capture.
    backdrop: Option<Arc<Vec<[f32; 4]>>>,
    ctx: Context,
    app: PlexiApp,
    frame: eframe::Frame,
    textures: HashMap<TextureId, Arc<Texture>>,
    /// Frames being rasterized and written on other threads.
    pending: Vec<JoinHandle<()>>,
    time: f64,
    pointer: Pos2,
    caption: Option<(String, f32)>,
    out: PathBuf,
    n: usize,
}

impl Recorder {
    fn new(out: &Path) -> Self {
        let dir = std::env::temp_dir().join(format!("plexi-reel-{}", std::process::id()));
        // SAFETY: set before anything reads it; this is the only test in its process that records.
        unsafe { std::env::set_var("PLEXI_DATA_DIR", &dir) };
        let _ = std::fs::remove_dir_all(out);
        std::fs::create_dir_all(out).unwrap();
        let ctx = Context::default();
        ctx.set_pixels_per_point(PPP);
        let cc = eframe::CreationContext::_new_kittest(ctx.clone());
        let app = PlexiApp::new(&cc, StartOptions { file: None, overlay: false, background: false, shell: false });
        let mut r = Self {
            backdrop: None,
            ctx,
            app,
            frame: eframe::Frame::_new_kittest(),
            textures: HashMap::new(),
            pending: Vec::new(),
            time: 0.0,
            pointer: pos2(SIZE[0] * 0.6, SIZE[1] * 0.6),
            caption: None,
            out: out.to_owned(),
            n: 0,
        };
        for _ in 0..3 {
            r.run(vec![], false);
        }
        r
    }

    /// One app frame; `keep` writes it out as a GIF frame.
    fn run(&mut self, events: Vec<Event>, keep: bool) {
        let mut raw = RawInput {
            screen_rect: Some(egui::Rect::from_min_size(Pos2::ZERO, vec2(SIZE[0], SIZE[1]))),
            time: Some(self.time),
            events,
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(PPP);
        let (app, frame) = (&mut self.app, &mut self.frame);
        let (caption, pointer) = (self.caption.clone(), self.pointer);
        let mut out = self.ctx.run_ui(raw, |ui| {
            eframe::App::ui(app, ui, frame);
            overlay(ui.ctx(), caption.as_ref().map(|(t, y)| (t.as_str(), *y)), pointer);
        });
        upload(&mut self.textures, &mut out.textures_delta);
        if keep {
            let prims = self.ctx.tessellate(out.shapes, out.pixels_per_point);
            let textures = self.textures.clone();
            let path = self.out.join(format!("{:05}.png", self.n));
            if self.pending.len() >= 12 {
                self.pending.remove(0).join().unwrap();
            }
            let backdrop = self.backdrop.clone();
            self.pending.push(std::thread::spawn(move || {
                let mut canvas = Canvas::new((SIZE[0] * PPP) as usize, (SIZE[1] * PPP) as usize);
                if let Some(b) = backdrop {
                    canvas.px.clone_from(&b);
                }
                canvas.paint(&prims, &textures);
                canvas.to_image().save(path).unwrap();
            }));
            self.n += 1;
        }
        for id in std::mem::take(&mut out.textures_delta.free) {
            self.textures.remove(&id);
        }
        self.time += 1.0 / FPS;
    }

    /// Hold still for `secs`.
    fn wait(&mut self, secs: f64) {
        for _ in 0..(secs * FPS).round() as usize {
            self.run(vec![], true);
        }
    }

    fn say(&mut self, caption: &str) {
        self.caption = Some((caption.to_owned(), SIZE[1] - 92.0));
    }

    /// Glide the pointer to `to` over `secs`, easing in and out.
    fn move_to(&mut self, to: (f32, f32), secs: f64) {
        let from = self.pointer;
        let to = pos2(to.0, to.1);
        let n = ((secs * FPS).round() as usize).max(1);
        for i in 1..=n {
            let t = i as f32 / n as f32;
            let e = t * t * (3.0 - 2.0 * t);
            self.pointer = from + (to - from) * e;
            self.run(vec![Event::PointerMoved(self.pointer)], true);
        }
    }

    fn button(&mut self, button: PointerButton, pressed: bool) {
        let pos = self.pointer;
        self.run(vec![Event::PointerButton { pos, button, pressed, modifiers: Modifiers::default() }], true);
    }

    fn click(&mut self, at: (f32, f32)) {
        self.move_to(at, 0.35);
        self.button(PointerButton::Primary, true);
        self.button(PointerButton::Primary, false);
        self.wait(0.15);
    }

    /// Press `button`, follow `path` one point per app frame, keeping every `keep`th frame.
    fn drag(&mut self, button: PointerButton, path: &[(f32, f32)], keep: usize) {
        let d = self.pointer.distance(pos2(path[0].0, path[0].1));
        self.move_to(path[0], (d as f64 / 900.0).clamp(0.12, 0.4));
        self.button(button, true);
        for (i, &(x, y)) in path[1..].iter().enumerate() {
            self.pointer = pos2(x, y);
            self.run(vec![Event::PointerMoved(self.pointer)], (i + 1) % keep.max(1) == 0);
        }
        self.button(button, false);
    }

    fn wheel(&mut self, at: (f32, f32), notches: i32) {
        self.move_to(at, 0.3);
        for _ in 0..notches.abs() {
            let delta = vec2(0.0, if notches > 0 { 1.0 } else { -1.0 });
            let e = Event::MouseWheel {
                unit: egui::MouseWheelUnit::Line,
                delta,
                modifiers: Modifiers::default(),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
            };
            self.run(vec![e], true);
            self.run(vec![], true);
        }
        self.wait(0.4);
    }

    fn type_text(&mut self, text: &str) {
        for ch in text.chars() {
            self.run(vec![Event::Text(ch.to_string())], true);
            self.run(vec![], true);
        }
    }

    fn finish(&mut self) {
        for h in self.pending.drain(..) {
            h.join().unwrap();
        }
    }

    fn snapshot(&mut self, name: &str) {
        self.run(vec![], true);
        self.finish();
        let last = self.out.join(format!("{:05}.png", self.n - 1));
        std::fs::copy(last, self.out.join(format!("{name}.png"))).unwrap();
    }
}

/// The pointer and the caption, drawn above the app.
fn overlay(ctx: &Context, caption: Option<(&str, f32)>, pointer: Pos2) {
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, egui::Id::unique("reel")));
    if let Some((text, y)) = caption {
        let font = egui::FontId::proportional(17.0);
        let galley = painter.layout_no_wrap(text.to_owned(), font, Color32::WHITE);
        let size = galley.size() + vec2(28.0, 24.0);
        let rect = egui::Rect::from_center_size(pos2(SIZE[0] / 2.0, y), size);
        painter.rect_filled(rect, 10.0, Color32::from_black_alpha(200));
        painter.galley(rect.min + vec2(14.0, 12.0), galley, Color32::WHITE);
    }
    // A classic arrow cursor.
    let p = pointer;
    let pts = [(0.0, 0.0), (0.0, 17.0), (4.5, 13.0), (7.5, 19.5), (10.0, 18.5), (7.0, 12.0), (12.5, 12.0)];
    let pts: Vec<Pos2> = pts.iter().map(|&(x, y)| p + vec2(x, y)).collect();
    painter.add(egui::Shape::convex_polygon(pts.clone(), Color32::BLACK, egui::Stroke::NONE));
    painter.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.2, Color32::WHITE)));
}

/// Cursive-looking loops (a prolate cycloid), one stroke per word, like a line of
/// handwriting.
fn handwriting(x0: f32, y0: f32, words: &[usize]) -> Vec<Vec<(f32, f32)>> {
    let mut x = x0;
    let mut seed = words.len() as f32;
    words
        .iter()
        .map(|&letters| {
            seed += 1.7;
            let n = letters * 24;
            let word: Vec<(f32, f32)> = (0..=n)
                .map(|i| {
                    let t = i as f32 / 24.0 * std::f32::consts::TAU;
                    let tall = 1.0 + 0.7 * ((t * 0.21 + seed).sin()).max(0.0).powi(3);
                    (x + t * 3.8 - 8.0 * t.sin(), y0 - 9.0 * tall * t.cos() + 4.0 * tall - 4.0)
                })
                .collect();
            x = word.last().unwrap().0 + 22.0;
            word
        })
        .collect()
}

fn wave(x0: f32, y0: f32, len: f32, amp: f32, turns: f32) -> Vec<(f32, f32)> {
    (0..=60)
        .map(|i| {
            let t = i as f32 / 60.0;
            (x0 + t * len, y0 + (t * turns * std::f32::consts::TAU).sin() * amp)
        })
        .collect()
}

/// A little landscape to drop onto the page.
fn picture() -> Vec<u8> {
    let (w, h) = (720u32, 480u32);
    let img = image::RgbImage::from_fn(w, h, |x, y| {
        let (fx, fy) = (x as f32 / w as f32, y as f32 / h as f32);
        let ridge = |f: f32, a: f32, b: f32| b + a * ((fx * f).sin() * 0.6 + (fx * f * 2.3 + 1.0).sin() * 0.4);
        let sun = ((fx - 0.68).powi(2) * 2.25 + (fy - 0.36).powi(2)).sqrt();
        let px = if fy > ridge(9.0, 0.05, 0.78) {
            [0x2d, 0x4a, 0x3e]
        } else if fy > ridge(5.0, 0.08, 0.6) {
            [0x4f, 0x6d, 0x8f]
        } else if sun < 0.11 {
            [0xff, 0xe2, 0x9a]
        } else {
            let k = fy / 0.7;
            [
                (0xff as f32 * (1.0 - k) + 0x8a as f32 * k) as u8,
                (0xa0 as f32 * (1.0 - k) + 0xb8 as f32 * k) as u8,
                (0x7a as f32 * (1.0 - k) + 0xe8 as f32 * k) as u8,
            ]
        };
        image::Rgb(px)
    });
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

// Toolbar positions (points) in a 960-wide window.
const PEN: (f32, f32) = (124.0, 34.0);
const HIGHLIGHTER: (f32, f32) = (160.0, 34.0);
/// The select tools' button; its menu holds lasso, rectangle select and snapshot.
const SELECT: (f32, f32) = (232.0, 34.0);
const HAND: (f32, f32) = (268.0, 34.0);
const TEXT: (f32, f32) = (304.0, 34.0);
const SHAPE: (f32, f32) = (340.0, 34.0);
const BLACK: (f32, f32) = (380.0, 34.0);
const BLUE: (f32, f32) = (406.0, 34.0);
const RED: (f32, f32) = (432.0, 34.0);

#[test]
#[ignore = "records docs/tour.gif; needs ffmpeg"]
fn record_tour() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let frames = std::env::temp_dir().join("plexi-reel-frames");
    let mut r = Recorder::new(&frames);
    r.wait(0.4);

    // 1. Notes: a title, handwriting, a highlight and an arrow.
    r.say("Type, write and sketch on an endless page");
    r.click(TEXT);
    r.click((90.0, 110.0));
    r.type_text("Launch plan");
    r.click(PEN);
    let lines: [&[usize]; 3] = [&[4, 6, 3, 5], &[3, 7, 4], &[6, 2, 5, 4, 3]];
    for (i, words) in lines.into_iter().enumerate() {
        match i {
            1 => r.click(BLUE),
            2 => r.click(BLACK),
            _ => {}
        }
        for word in handwriting(96.0, 180.0 + i as f32 * 48.0, words) {
            r.drag(PointerButton::Primary, &word, 6);
        }
    }
    r.click(SHAPE);
    r.snapshot("shapes");
    r.click(RED);
    r.click(HIGHLIGHTER);
    r.drag(PointerButton::Primary, &wave(92.0, 180.0, 150.0, 0.0, 0.0), 2);
    r.snapshot("notes");

    // 2. An image: resize it by a corner, then lock it from the right-click menu.
    r.say("Drop in images, resize them and lock them in place");
    let ctx = r.ctx.clone();
    r.app.insert_image(picture(), Some([660.0, 200.0]), &ctx);
    r.wait(0.6);
    r.click(SELECT);
    r.click((700.0, 270.0));
    r.drag(
        PointerButton::Primary,
        &wave(798.0, 332.0, 0.0, 0.0, 0.0)
            .iter()
            .enumerate()
            .map(|(i, _)| (798.0 + i as f32 * 1.2, 332.0 + i as f32 * 0.8))
            .collect::<Vec<_>>(),
        2,
    );
    r.snapshot("resized");
    r.move_to((720.0, 280.0), 0.4);
    r.button(PointerButton::Secondary, true);
    r.button(PointerButton::Secondary, false);
    r.wait(0.3);
    r.click((735.0, 233.0));
    r.wait(0.6);
    r.snapshot("locked");

    // 3. The page never ends: zoom out, keep writing onto the next sheet, pan around.
    r.say("No page edges: scroll to zoom, right-drag to pan");
    r.wheel((520.0, 330.0), -5);
    r.click(PEN);
    r.click(BLUE);
    r.drag(PointerButton::Primary, &wave(310.0, 400.0, 600.0, 18.0, 4.0), 2);
    r.click(BLACK);
    for word in handwriting(640.0, 220.0, &[4, 3]) {
        r.drag(PointerButton::Primary, &word, 6);
    }
    r.wait(0.3);
    r.snapshot("zoomed");
    let pan: Vec<(f32, f32)> = (0..=40).map(|i| (700.0 - i as f32 * 6.0, 330.0 - i as f32 * 1.0)).collect();
    r.drag(PointerButton::Secondary, &pan, 1);
    r.wait(0.4);
    r.snapshot("panned");

    // 4. Paper colors and rulings from the right-click menu.
    r.say("Right-click for paper colors, rulings and more");
    let at = (300.0, 140.0);
    r.move_to(at, 0.4);
    r.button(PointerButton::Secondary, true);
    r.button(PointerButton::Secondary, false);
    r.wait(0.3);
    r.snapshot("menu2");
    r.click((at.0 + 115.0, at.1 + 117.0));
    r.click((at.0 + 144.0, at.1 + 151.0));
    r.wait(0.4);
    r.click((60.0, 500.0));
    r.snapshot("paper");

    // 5. Export with a preview of each sheet.
    r.say("Export to PDF, PNG, SVG and more, with a live preview");
    r.caption.as_mut().unwrap().1 = SIZE[1] - 30.0;
    r.click((97.0, 34.0));
    r.click((115.0, 172.0));
    std::thread::sleep(std::time::Duration::from_secs(2));
    r.wait(1.0);
    r.snapshot("export");
    r.click((281.0, 490.0));
    std::thread::sleep(std::time::Duration::from_secs(1));
    r.wait(1.6);
    r.snapshot("export2");
    r.finish();

    encode_gif(&frames, &root.join("docs/tour.gif"));
}

/// Turn the numbered frames into a 15 fps GIF with one shared palette.
fn encode_gif(frames: &Path, gif: &Path) {
    let filter = "fps=15,split[a][b];[a]palettegen=max_colors=256:stats_mode=full[p];                  [b][p]paletteuse=dither=none:diff_mode=rectangle";
    let status = std::process::Command::new("ffmpeg")
        .args(["-loglevel", "error", "-y", "-framerate", "30", "-i"])
        .arg(frames.join("%05d.png"))
        .args(["-vf", filter])
        .arg(gif)
        .status()
        .expect("ffmpeg on PATH");
    assert!(status.success());
}

#[test]
#[ignore = "renders stills of the newer tools to the temp folder, for a look"]
fn preview_tools() {
    let out = std::env::temp_dir().join("plexi-preview");
    let mut r = Recorder::new(&out);
    r.wait(0.2);
    r.click(TEXT);
    r.click((300.0, 220.0));
    r.type_text("Fonts now");
    r.app.apply_text_style(|t| {
        t.font = Some("Georgia".into());
        t.size = 40.0;
        t.bold = true;
    });
    r.wait(0.3);
    r.snapshot("text");
    r.click((120.0, 300.0));
    r.click(SELECT);
    r.click((640.0, 400.0));
    let ctx = r.ctx.clone();
    r.app.insert_image(picture(), Some([480.0, 420.0]), &ctx);
    r.wait(0.2);
    let id = r.app.selection.iter().next().copied().unwrap();
    r.app.start_crop(crate::history::Target::Page(0), id);
    r.wait(0.2);
    let c = r.app.crop.clone().unwrap();
    let left = r.app.view.to_screen(r.app.canvas_origin, [c.shown.min[0], c.shown.center()[1]]);
    r.drag(PointerButton::Primary, &[(left.x, left.y), (left.x + 40.0, left.y), (left.x + 80.0, left.y)], 1);
    r.snapshot("crop");
    r.run(
        vec![Event::Key {
            key: egui::Key::Enter,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Modifiers::default(),
        }],
        true,
    );
    r.wait(0.2);
    r.snapshot("cropped");

    // Rectangle select: drag a marquee around everything, then Hand: pan the view.
    r.app.tool = crate::app::Tool::RectSelect;
    r.drag(PointerButton::Primary, &[(60.0, 90.0), (760.0, 90.0), (760.0, 520.0), (60.0, 520.0), (60.0, 100.0)], 1);
    r.wait(0.2);
    r.snapshot("rect-select");
    r.click(HAND);
    let pan: Vec<(f32, f32)> = (0..=20).map(|i| (500.0 - i as f32 * 4.0, 300.0 - i as f32 * 2.0)).collect();
    r.drag(PointerButton::Primary, &pan, 1);
    r.wait(0.2);
    r.snapshot("panned-hand");

    let far = plexi_core::Shape {
        id: plexi_core::new_id(),
        kind: plexi_core::ShapeKind::Rect,
        a: [1000.0, 100.0],
        b: [1100.0, 200.0],
        color: plexi_core::Color::BLACK,
        width: 3.0,
    };
    r.app.doc.pages[0].elements.push(plexi_core::Element::Shape(far));
    r.app.touch();
    r.app.open_export_dialog();
    if let Some(d) = &mut r.app.export {
        d.pick_pages = true;
        d.page_list = "2".into();
    }
    std::thread::sleep(std::time::Duration::from_secs(2));
    r.wait(0.5);
    r.snapshot("export");
    r.app.export = None;
    r.app.start_snip(&ctx);
    r.wait(0.1);
    r.snapshot("snip-start");
    r.move_to((200.0, 150.0), 0.2);
    r.button(PointerButton::Primary, true);
    for (x, y) in [(300.0, 200.0), (420.0, 280.0), (520.0, 330.0)] {
        r.pointer = pos2(x, y);
        r.run(vec![Event::PointerMoved(r.pointer)], true);
    }
    r.snapshot("snip");
    // Cancel rather than let the snip capture the real screen.
    r.button(PointerButton::Secondary, true);
    r.finish();
}

#[test]
#[ignore = "renders stills of the select-tools menu, hand cursor, text handles and snapshot tool"]
fn preview_planned() {
    use crate::app::Tool;
    let out = std::env::temp_dir().join("plexi-preview-planned");
    let mut r = Recorder::new(&out);
    r.wait(0.2);
    r.click(SELECT);
    r.wait(0.3);
    r.snapshot("select-menu");
    r.click((600.0, 400.0)); // close the menu
    r.click(TEXT);
    r.click((300.0, 260.0));
    r.type_text("Wrap me please");
    r.wait(0.3);
    r.snapshot("editing");
    r.click((700.0, 500.0));
    r.click(HAND);
    r.move_to((640.0, 420.0), 0.3);
    r.snapshot("hand-open");
    r.click((330.0, 262.0));
    r.wait(0.2);
    r.snapshot("text-selected");
    r.move_to((640.0, 420.0), 0.2);
    r.button(PointerButton::Primary, true);
    r.move_to((600.0, 400.0), 0.2);
    r.snapshot("hand-closed");
    r.button(PointerButton::Primary, false);
    r.app.tool = Tool::Snapshot;
    r.move_to((250.0, 200.0), 0.2);
    r.button(PointerButton::Primary, true);
    r.move_to((520.0, 330.0), 0.3);
    r.snapshot("snapshot-drag");
    r.button(PointerButton::Primary, false);
    r.wait(0.3);
    r.snapshot("snapshot-done");
    r.app.doc.pages[0].size = plexi_core::PageSize::A4;
    r.app.config.sheet_guides = false;
    r.app.config.sheet_numbers = false;
    r.app.fit_pending = true;
    r.wait(0.3);
    r.snapshot("seamless");
    r.finish();
}

/// A stand-in desktop for the draw-on-screen scene: a slide app with a bar chart,
/// painted by egui and rasterized like any app frame.
fn desktop() -> Vec<[f32; 4]> {
    use egui::{Align2, FontId, Rect, Stroke};
    let ctx = Context::default();
    ctx.set_pixels_per_point(PPP);
    let mut textures = HashMap::new();
    let mut prims = Vec::new();
    for _ in 0..3 {
        let mut raw = RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(SIZE[0], SIZE[1]))),
            ..Default::default()
        };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(PPP);
        let mut out = ctx.run_ui(raw, |ui| {
            let p = ui.ctx().layer_painter(egui::LayerId::background());
            let rgb = Color32::from_rgb;
            p.rect_filled(Rect::from_min_size(Pos2::ZERO, vec2(SIZE[0], SIZE[1])), 0.0, rgb(0x24, 0x3b, 0x55));
            let win = Rect::from_min_max(pos2(50.0, 30.0), pos2(910.0, 575.0));
            p.rect_filled(win, 8.0, rgb(0xe9, 0xeb, 0xef));
            let bar = Rect::from_min_max(win.min, pos2(win.max.x, win.min.y + 34.0));
            p.rect_filled(bar, egui::CornerRadius { nw: 8, ne: 8, sw: 0, se: 0 }, rgb(0x2b, 0x2f, 0x36));
            let title = FontId::proportional(13.0);
            p.text(pos2(70.0, bar.center().y), Align2::LEFT_CENTER, "Q3 review — Slides", title, Color32::WHITE);
            for (i, c) in [rgb(0xff, 0x5f, 0x57), rgb(0xfe, 0xbc, 0x2e), rgb(0x28, 0xc8, 0x40)].iter().enumerate() {
                p.circle_filled(pos2(890.0 - i as f32 * 20.0, bar.center().y), 5.5, *c);
            }
            let slide = Rect::from_min_max(pos2(100.0, 95.0), pos2(860.0, 545.0));
            p.rect_filled(slide, 2.0, Color32::WHITE);
            p.rect_stroke(slide, 2.0, Stroke::new(1.0, rgb(0xcc, 0xd0, 0xd6)), egui::StrokeKind::Outside);
            let ink = rgb(0x22, 0x26, 0x2e);
            p.text(pos2(140.0, 140.0), Align2::LEFT_CENTER, "Revenue by region", FontId::proportional(30.0), ink);
            let grey = rgb(0x6b, 0x72, 0x80);
            p.text(pos2(141.0, 174.0), Align2::LEFT_CENTER, "Q3, in $ thousands", FontId::proportional(15.0), grey);
            let base = 490.0;
            p.hline(160.0..=820.0, base, Stroke::new(1.5, rgb(0xb8, 0xbe, 0xc8)));
            let bars = [("North", 120.0), ("South", 175.0), ("West", 262.0), ("East", 148.0)];
            for (i, (name, v)) in bars.iter().enumerate() {
                let x = 190.0 + i as f32 * 160.0;
                let r = Rect::from_min_max(pos2(x, base - v), pos2(x + 90.0, base));
                p.rect_filled(r, 3.0, rgb(0x5b, 0x7f, 0xb6));
                let f = FontId::proportional(14.0);
                p.text(pos2(r.center().x, base + 16.0), Align2::CENTER_CENTER, *name, f.clone(), grey);
                p.text(pos2(r.center().x, r.top() - 12.0), Align2::CENTER_CENTER, format!("{v}"), f, ink);
            }
        });
        upload(&mut textures, &mut out.textures_delta);
        prims = ctx.tessellate(out.shapes, out.pixels_per_point);
    }
    let mut canvas = Canvas::new((SIZE[0] * PPP) as usize, (SIZE[1] * PPP) as usize);
    canvas.paint(&prims, &textures);
    canvas.px
}

/// A little looping animation: a ball bouncing on a pastel floor.
fn bouncing_gif() -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    let (w, h, n) = (480u32, 360u32, 16);
    let mut buf = Vec::new();
    {
        let mut enc = GifEncoder::new_with_speed(&mut buf, 10);
        enc.set_repeat(Repeat::Infinite).unwrap();
        for i in 0..n {
            let t = i as f32 / n as f32;
            let height = (t * std::f32::consts::PI).sin(); // 0 on the floor, 1 at the top
            let squash = if height < 0.15 { 1.0 + (0.15 - height) * 2.0 } else { 1.0 };
            let (cx, r) = (w as f32 / 2.0, 52.0);
            let cy = 280.0 - r / squash - height * 184.0;
            let img = image::RgbaImage::from_fn(w, h, |x, y| {
                let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
                let ball = ((fx - cx) / (r * squash)).powi(2) + ((fy - cy) / (r / squash)).powi(2);
                let shadow = ((fx - cx) / (60.0 - height * 24.0)).powi(2) + ((fy - 292.0) / 10.0).powi(2);
                let px = if ball < 1.0 {
                    let shine = ((fx - cx + 18.0).powi(2) + (fy - cy + 18.0).powi(2)).sqrt() < 14.0;
                    if shine { [0xff, 0xd9, 0xd0] } else { [0xf0, 0x5a, 0x4f] }
                } else if shadow < 1.0 {
                    [0xc9, 0xbf, 0xe0]
                } else if fy > 280.0 {
                    [0xe2, 0xda, 0xf2]
                } else {
                    [0xfb, 0xf3, 0xe4]
                };
                image::Rgba([px[0], px[1], px[2], 255])
            });
            let delay = image::Delay::from_numer_denom_ms(100, 1);
            enc.encode_frame(image::Frame::from_parts(img, 0, 0, delay)).unwrap();
        }
    }
    buf
}

/// A Catmull-Rom curve through `pts`, `per` points per span.
fn curve(pts: &[(f32, f32)], per: usize) -> Vec<(f32, f32)> {
    let mut out = vec![pts[0]];
    for i in 0..pts.len() - 1 {
        let p0 = pts[i.saturating_sub(1)];
        let (p1, p2) = (pts[i], pts[i + 1]);
        let p3 = pts[(i + 2).min(pts.len() - 1)];
        for k in 1..=per {
            let t = k as f32 / per as f32;
            let (t2, t3) = (t * t, t * t * t);
            let f = |a: f32, b: f32, c: f32, d: f32| {
                0.5 * (2.0 * b
                    + (c - a) * t
                    + (2.0 * a - 5.0 * b + 4.0 * c - d) * t2
                    + (3.0 * b - a - 3.0 * c + d) * t3)
            };
            out.push((f(p0.0, p1.0, p2.0, p3.0), f(p0.1, p1.1, p2.1, p3.1)));
        }
    }
    out
}

/// A slightly wobbly loop around (`cx`, `cy`) that overshoots its start, like a quick
/// circle drawn by hand.
fn hand_loop(cx: f32, cy: f32, rx: f32, ry: f32) -> Vec<(f32, f32)> {
    (0..=70)
        .map(|i| {
            let a = -2.2 + i as f32 / 70.0 * 7.0;
            let wob = 1.0 + 0.06 * (a * 2.3).sin() + i as f32 * 0.0012;
            (cx + a.cos() * rx * wob, cy + a.sin() * ry * wob)
        })
        .collect()
}

fn star(cx: f32, cy: f32, r: f32) -> Vec<(f32, f32)> {
    let pts: Vec<(f32, f32)> = (0..=10)
        .map(|i| {
            let a = -std::f32::consts::FRAC_PI_2 + i as f32 * std::f32::consts::PI / 5.0;
            let k = if i % 2 == 0 { r } else { r * 0.42 };
            (cx + a.cos() * k, cy + a.sin() * k)
        })
        .collect();
    let mut out = Vec::new();
    for w in pts.windows(2) {
        for k in 0..6 {
            let t = k as f32 / 6.0;
            out.push((w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t));
        }
    }
    out.push(pts[10]);
    out
}

/// Pen strokes for a few capitals, in the handwriting window's box units (`y` 0 at the
/// top, baseline at 0.8), a little wobbly like real handwriting.
fn glyph(c: char) -> Vec<Vec<(f32, f32)>> {
    let oval = (0..=16)
        .map(|i| {
            let a = -1.9 + i as f32 / 16.0 * 6.6;
            (0.37 + a.cos() * 0.24, 0.46 + a.sin() * 0.34)
        })
        .collect();
    match c {
        'H' => vec![
            vec![(0.15, 0.12), (0.16, 0.45), (0.15, 0.8)],
            vec![(0.55, 0.1), (0.54, 0.47), (0.56, 0.8)],
            vec![(0.14, 0.47), (0.35, 0.45), (0.56, 0.46)],
        ],
        'E' => vec![
            vec![(0.52, 0.11), (0.3, 0.1), (0.16, 0.12), (0.15, 0.46), (0.16, 0.79), (0.35, 0.8), (0.54, 0.78)],
            vec![(0.16, 0.45), (0.32, 0.46), (0.46, 0.44)],
        ],
        'L' => vec![vec![(0.17, 0.1), (0.16, 0.45), (0.17, 0.8), (0.35, 0.79), (0.52, 0.8)]],
        'O' => vec![oval],
        _ => vec![],
    }
}

#[test]
#[ignore = "records docs/showcase.gif; needs ffmpeg"]
fn record_showcase() {
    use crate::app::Tool;
    use plexi_core::Color;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let frames = std::env::temp_dir().join("plexi-showcase-frames");
    let mut r = Recorder::new(&frames);
    let ctx = r.ctx.clone();

    // 1. Draw on screen, right over another app.
    r.backdrop = Some(Arc::new(desktop()));
    r.app.enter_overlay(&ctx, false);
    r.app.tool = Tool::Pen;
    r.app.pen_color = Color::rgb(0xe5, 0x39, 0x35);
    r.wait(0.5);
    r.say("Ctrl+Alt+D: draw right on top of any app");
    r.wait(0.6);
    r.drag(PointerButton::Primary, &hand_loop(535.0, 222.0, 78.0, 44.0), 2);
    r.drag(PointerButton::Primary, &curve(&[(770.0, 190.0), (700.0, 205.0), (628.0, 232.0)], 12), 2);
    r.drag(PointerButton::Primary, &curve(&[(638.0, 216.0), (627.0, 233.0), (646.0, 244.0)], 6), 1);
    for word in handwriting(690.0, 160.0, &[4, 3]) {
        r.drag(PointerButton::Primary, &word, 5);
    }
    r.wait(1.0);
    r.snapshot("overlay");
    r.app.exit_overlay(&ctx);
    r.backdrop = None;
    r.caption = None;
    r.wait(0.3);

    // 2. Ink stays editable: lasso a doodle, move it, copy and paste it.
    r.click(PEN);
    r.click(BLACK);
    for (i, words) in [&[5usize, 3, 6][..], &[4, 6]].into_iter().enumerate() {
        for word in handwriting(100.0, 170.0 + i as f32 * 50.0, words) {
            r.drag(PointerButton::Primary, &word, 8);
        }
    }
    r.click(BLUE);
    r.drag(PointerButton::Primary, &star(680.0, 210.0, 62.0), 2);
    r.say("Ink stays editable: lasso it, move it, copy it");
    // Open the select tools menu, then pick the lasso from it.
    r.click(SELECT);
    r.wait(0.4);
    r.app.tool = Tool::Lasso;
    r.click((600.0, 470.0)); // closes the menu without drawing
    r.drag(PointerButton::Primary, &hand_loop(680.0, 215.0, 95.0, 88.0), 2);
    r.wait(0.3);
    let path: Vec<(f32, f32)> = curve(&[(680.0, 215.0), (520.0, 330.0), (300.0, 420.0)], 30);
    r.drag(PointerButton::Primary, &path, 1);
    r.wait(0.3);
    r.app.copy_selection();
    r.wait(0.4);
    r.app.paste_selection(&ctx);
    r.wait(0.4);
    let b = r.app.selection_bounds(r.app.target()).expect("pasted selection");
    let c = r.app.view.to_screen(r.app.canvas_origin, b.center());
    let path = curve(&[(c.x, c.y), (c.x + 150.0, c.y + 40.0), (720.0, 420.0)], 30);
    r.drag(PointerButton::Primary, &path, 1);
    r.wait(0.8);
    r.snapshot("lasso");

    // 3. Grab the page aside, then drop in an animated GIF.
    r.click(HAND);
    let pan = curve(&[(820.0, 300.0), (500.0, 290.0), (220.0, 300.0)], 30);
    r.drag(PointerButton::Primary, &pan, 1);
    r.say("Animated GIFs play right on the page");
    let at = r.app.view.to_page(r.app.canvas_origin, pos2(640.0, 200.0));
    r.app.insert_image(bouncing_gif(), Some(at), &ctx);
    r.wait(1.0);
    // Pull its corner out to a better size.
    let b = r.app.selection_bounds(r.app.target()).expect("the GIF");
    let corner = r.app.view.to_screen(r.app.canvas_origin, b.max);
    let grow: Vec<(f32, f32)> = (0..=30).map(|i| (corner.x + i as f32 * 4.4, corner.y + i as f32 * 3.3)).collect();
    r.drag(PointerButton::Primary, &grow, 1);
    r.wait(2.0);
    r.snapshot("gif");

    // 4. Teach it your handwriting, then type in it.
    r.say("Write each letter once...");
    r.app.handwriting.open = true;
    r.wait(0.5);
    for ch in ['H', 'E', 'L', 'O'] {
        let rect = r.app.handwriting.boxes[&ch];
        for stroke in glyph(ch) {
            let pts: Vec<(f32, f32)> = curve(&stroke, 6)
                .into_iter()
                .map(|(x, y)| (rect.left() + x * rect.height(), rect.top() + y * rect.height()))
                .collect();
            r.drag(PointerButton::Primary, &pts, 2);
        }
    }
    r.wait(0.3);
    r.snapshot("glyphs");
    let save = r.app.handwriting.save_button.expect("save button").center();
    r.click((save.x, save.y));
    r.wait(0.4);
    r.app.handwriting.open = false;
    r.say("...then type in your own handwriting");
    r.click(TEXT);
    r.click((300.0, 395.0));
    r.app.apply_text_style(|t| {
        t.font = Some(plexi_core::handwriting::FAMILY_NAME.into());
        t.size = 64.0;
    });
    r.type_text("HELLO HELLO");
    r.wait(2.0);
    r.snapshot("typed");
    r.finish();

    encode_gif(&frames, &root.join("docs/showcase.gif"));
}
