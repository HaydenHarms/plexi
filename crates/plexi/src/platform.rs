//! Everything that talks to Win32 directly lives here: overlay window placement, screen
//! capture, video encoding for recordings, and attaching to the parent console for
//! command-line use.
//!
//! Elsewhere the native window handle is not used: the overlay is placed with viewport
//! commands instead (see `PlexiApp::drive_overlay`), and on macOS screenshots go through
//! `xcap` (`capture_main_screen`).

/// A rectangle in physical screen pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PxRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

#[cfg(windows)]
mod imp {
    use super::PxRect;
    use windows::Win32::Foundation::{HWND, POINT, RECT};
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BitBlt, CAPTUREBLT, CreateCompatibleBitmap, CreateCompatibleDC,
        DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDC, GetDIBits, GetMonitorInfoW, MONITOR_DEFAULTTONEAREST,
        MONITORINFO, MonitorFromPoint, MonitorFromWindow, ReleaseDC, SRCCOPY, SelectObject,
    };
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
    use windows::Win32::UI::WindowsAndMessaging::{
        GetCursorPos, GetWindowRect, HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOZORDER, SWP_SHOWWINDOW,
        SetForegroundWindow, SetWindowDisplayAffinity, SetWindowPos, WDA_EXCLUDEFROMCAPTURE, WDA_NONE,
    };

    fn hwnd(h: isize) -> HWND {
        HWND(h as *mut core::ffi::c_void)
    }

    fn monitor_rect(m: windows::Win32::Graphics::Gdi::HMONITOR) -> Option<PxRect> {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        unsafe { GetMonitorInfoW(m, &mut info) }.as_bool().then(|| {
            let r = info.rcMonitor;
            PxRect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top }
        })
    }

    /// The monitor under the mouse cursor.
    pub fn cursor_monitor() -> Option<PxRect> {
        let mut p = POINT::default();
        unsafe { GetCursorPos(&mut p) }.ok()?;
        monitor_rect(unsafe { MonitorFromPoint(p, MONITOR_DEFAULTTONEAREST) })
    }

    /// The monitor a window is mostly on.
    pub fn window_monitor(h: isize) -> Option<PxRect> {
        monitor_rect(unsafe { MonitorFromWindow(hwnd(h), MONITOR_DEFAULTTONEAREST) })
    }

    pub fn window_rect(h: isize) -> Option<PxRect> {
        let mut r = RECT::default();
        unsafe { GetWindowRect(hwnd(h), &mut r) }.ok()?;
        Some(PxRect { x: r.left, y: r.top, w: r.right - r.left, h: r.bottom - r.top })
    }

    /// Pull a window that hangs off its monitor's work area (e.g. under the taskbar)
    /// back inside it, shrinking it if it is bigger than the work area.
    pub fn fit_to_work_area(h: isize) {
        let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
        let m = unsafe { MonitorFromWindow(hwnd(h), MONITOR_DEFAULTTONEAREST) };
        if !unsafe { GetMonitorInfoW(m, &mut info) }.as_bool() {
            return;
        }
        let (work, Some(r)) = (info.rcWork, window_rect(h)) else { return };
        let (ww, wh) = (work.right - work.left, work.bottom - work.top);
        let (w, hh) = (r.w.min(ww), r.h.min(wh));
        let x = r.x.clamp(work.left, work.right - w);
        let y = r.y.clamp(work.top, work.bottom - hh);
        if (x, y, w, hh) != (r.x, r.y, r.w, r.h) {
            let _ = unsafe { SetWindowPos(hwnd(h), None, x, y, w, hh, SWP_NOACTIVATE | SWP_NOZORDER) };
        }
    }

    /// Cover `r` exactly and stay above other windows.
    pub fn place_topmost(h: isize, r: PxRect, activate: bool) {
        let flags = if activate { SWP_SHOWWINDOW } else { SWP_SHOWWINDOW | SWP_NOACTIVATE };
        let _ = unsafe { SetWindowPos(hwnd(h), HWND_TOPMOST, r.x, r.y, r.w, r.h, flags) };
    }

    /// Put a window back to an ordinary (not topmost) position and size.
    pub fn place_normal(h: isize, r: PxRect) {
        let _ = unsafe { SetWindowPos(hwnd(h), HWND_NOTOPMOST, r.x, r.y, r.w, r.h, SWP_SHOWWINDOW) };
    }

    pub fn focus(h: isize) {
        let _ = unsafe { SetForegroundWindow(hwnd(h)) };
    }

    /// Hide (or show) the window from screenshots and screen sharing.
    pub fn exclude_from_capture(h: isize, exclude: bool) {
        let mode = if exclude { WDA_EXCLUDEFROMCAPTURE } else { WDA_NONE };
        let _ = unsafe { SetWindowDisplayAffinity(hwnd(h), mode) };
    }

    /// Grab a screen area as straight RGBA.
    pub fn capture(r: PxRect) -> Option<image::RgbaImage> {
        if r.w <= 0 || r.h <= 0 {
            return None;
        }
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(screen);
            let bmp = CreateCompatibleBitmap(screen, r.w, r.h);
            let old = SelectObject(mem, bmp);
            let ok = BitBlt(mem, 0, 0, r.w, r.h, screen, r.x, r.y, SRCCOPY | CAPTUREBLT).is_ok();
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: r.w,
                    biHeight: -r.h, // top-down rows
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut buf = vec![0u8; (r.w * r.h * 4) as usize];
            let rows = GetDIBits(mem, bmp, 0, r.h as u32, Some(buf.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
            SelectObject(mem, old);
            let _ = DeleteObject(bmp);
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);
            if !ok || rows == 0 {
                return None;
            }
            for px in buf.as_chunks_mut::<4>().0 {
                px.swap(0, 2); // BGRA -> RGBA
                px[3] = 255;
            }
            image::RgbaImage::from_raw(r.w as u32, r.h as u32, buf)
        }
    }

    /// Add or remove the window's taskbar button. The overlay lives in the tray instead.
    pub fn show_in_taskbar(h: isize, show: bool) {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
        };
        use windows::Win32::UI::Shell::{ITaskbarList, TaskbarList};
        unsafe {
            // winit has already initialized COM on this thread; this is a cheap no-op then.
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let Ok(tb) = CoCreateInstance::<_, ITaskbarList>(&TaskbarList, None, CLSCTX_INPROC_SERVER) else { return };
            if tb.HrInit().is_ok() {
                let _ = if show { tb.AddTab(hwnd(h)) } else { tb.DeleteTab(hwnd(h)) };
            }
        }
    }

    /// When launched from a terminal, print to it (the release exe has no console).
    pub fn attach_parent_console() {
        let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
    }

    pub fn capture_main_screen() -> Option<image::RgbaImage> {
        cursor_monitor().and_then(capture)
    }

    /// An MP4 (H.264) file being written one frame at a time, with Windows' own Media
    /// Foundation encoder. Create it, `write` and `finish` it on the same thread.
    pub struct VideoWriter {
        writer: windows::Win32::Media::MediaFoundation::IMFSinkWriter,
        stream: u32,
        frame_len: usize,
    }

    impl VideoWriter {
        /// Start a `w` x `h` video at `path` (both even, at most 4096 x 2304), at about
        /// `fps` frames a second.
        pub fn create(path: &std::path::Path, w: u32, h: u32, fps: u32) -> Result<Self, String> {
            use windows::Win32::Media::MediaFoundation::*;
            use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};
            let err = |what: &str, e: windows::core::Error| format!("{what}: {}", e.message());
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_FULL).map_err(|e| err("Media Foundation", e))?;
                let url = windows::core::HSTRING::from(path.as_os_str());
                // A hardware encoder if there is one: far less work for the CPU.
                let writer = (|| {
                    let mut attrs = None;
                    MFCreateAttributes(&mut attrs, 1)?;
                    let attrs = attrs.ok_or_else(windows::core::Error::empty)?;
                    attrs.SetUINT32(&MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS, 1)?;
                    MFCreateSinkWriterFromURL(&url, None, &attrs)
                })()
                .or_else(|_| MFCreateSinkWriterFromURL(&url, None, None))
                .map_err(|e| err("Couldn't create the video file", e))?;
                let size = ((w as u64) << 32) | h as u64;
                let rate = ((fps as u64) << 32) | 1;
                let typed = |subtype: &windows::core::GUID| -> windows::core::Result<IMFMediaType> {
                    let t = MFCreateMediaType()?;
                    t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
                    t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
                    t.SetUINT64(&MF_MT_FRAME_SIZE, size)?;
                    t.SetUINT64(&MF_MT_FRAME_RATE, rate)?;
                    t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1 << 32) | 1)?;
                    Ok(t)
                };
                let out = typed(&MFVideoFormat_H264).map_err(|e| err("Video format", e))?;
                // About 0.12 bits a pixel: crisp for ink and text at screen sizes.
                let bitrate = ((w as f64 * h as f64 * fps as f64 * 0.12) as u32).clamp(2_000_000, 40_000_000);
                out.SetUINT32(&MF_MT_AVG_BITRATE, bitrate).map_err(|e| err("Video format", e))?;
                let stream = writer.AddStream(&out).map_err(|e| err("Video stream", e))?;
                let input = typed(&MFVideoFormat_RGB32).map_err(|e| err("Video format", e))?;
                // Rows top to bottom (a negative or missing stride would mean bottom-up).
                input.SetUINT32(&MF_MT_DEFAULT_STRIDE, w * 4).map_err(|e| err("Video format", e))?;
                writer.SetInputMediaType(stream, &input, None).map_err(|e| err("Video size", e))?;
                writer.BeginWriting().map_err(|e| err("Couldn't start the video", e))?;
                Ok(Self { writer, stream, frame_len: (w * h * 4) as usize })
            }
        }

        /// Add a frame: `bgra` is `w * h` pixels, rows top to bottom, in Windows' byte
        /// order (blue, green, red, unused). `time` and `duration` are in seconds.
        pub fn write(&mut self, bgra: &[u8], time: f64, duration: f64) -> Result<(), String> {
            use windows::Win32::Media::MediaFoundation::*;
            if bgra.len() != self.frame_len {
                return Err("frame is the wrong size".into());
            }
            let ticks = |s: f64| (s * 10_000_000.0).round() as i64; // 100 ns units
            unsafe {
                let r = (|| -> windows::core::Result<()> {
                    let buf = MFCreateMemoryBuffer(bgra.len() as u32)?;
                    let mut dst = std::ptr::null_mut();
                    buf.Lock(&mut dst, None, None)?;
                    std::ptr::copy_nonoverlapping(bgra.as_ptr(), dst, bgra.len());
                    buf.Unlock()?;
                    buf.SetCurrentLength(bgra.len() as u32)?;
                    let sample = MFCreateSample()?;
                    sample.AddBuffer(&buf)?;
                    sample.SetSampleTime(ticks(time))?;
                    sample.SetSampleDuration(ticks(duration).max(1))?;
                    self.writer.WriteSample(self.stream, &sample)
                })();
                r.map_err(|e| e.message())
            }
        }

        /// Write out what's left and close the file.
        pub fn finish(self) -> Result<(), String> {
            let r = unsafe { self.writer.Finalize() }.map_err(|e| e.message());
            drop(self.writer);
            unsafe {
                let _ = windows::Win32::Media::MediaFoundation::MFShutdown();
            }
            r
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use super::PxRect;
    pub fn cursor_monitor() -> Option<PxRect> {
        None
    }
    pub fn window_monitor(_: isize) -> Option<PxRect> {
        None
    }
    pub fn window_rect(_: isize) -> Option<PxRect> {
        None
    }
    pub fn place_topmost(_: isize, _: PxRect, _: bool) {}
    pub fn place_normal(_: isize, _: PxRect) {}
    pub fn fit_to_work_area(_: isize) {}
    pub fn focus(_: isize) {}
    pub fn exclude_from_capture(_: isize, _: bool) {}
    pub fn capture(_: PxRect) -> Option<image::RgbaImage> {
        None
    }
    pub fn show_in_taskbar(_: isize, _: bool) {}
    pub fn attach_parent_console() {}

    /// The primary display as straight RGBA. macOS asks for Screen Recording permission
    /// the first time.
    #[cfg(target_os = "macos")]
    pub fn capture_main_screen() -> Option<image::RgbaImage> {
        let monitors = xcap::Monitor::all().ok()?;
        let main = monitors.iter().find(|m| m.is_primary().unwrap_or(false)).or(monitors.first())?;
        main.capture_image().ok()
    }

    #[cfg(not(target_os = "macos"))]
    pub fn capture_main_screen() -> Option<image::RgbaImage> {
        None
    }

    /// Recording to video needs Windows' encoder for now.
    pub struct VideoWriter;

    impl VideoWriter {
        pub fn create(_: &std::path::Path, _: u32, _: u32, _: u32) -> Result<Self, String> {
            Err("Recording is only on Windows for now".into())
        }
        pub fn write(&mut self, _: &[u8], _: f64, _: f64) -> Result<(), String> {
            Ok(())
        }
        pub fn finish(self) -> Result<(), String> {
            Ok(())
        }
    }
}

pub use imp::*;

/// The native window handle behind eframe's window, as an integer.
#[cfg(windows)]
pub fn native_handle(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Only Windows uses the native handle; other platforms place windows through egui.
#[cfg(not(windows))]
pub fn native_handle(_: &eframe::Frame) -> Option<isize> {
    None
}

#[cfg(all(test, windows))]
mod tests {
    use super::VideoWriter;

    /// Encode a short clip whose top half is red and bottom half blue, and (when ffmpeg
    /// is around to decode it) check it comes back the right way up and the right colors.
    #[test]
    fn writes_an_mp4_the_right_way_up() {
        let dir = std::env::temp_dir().join(format!("plexi-video-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("clip.mp4");
        let (w, h) = (320u32, 240u32);
        let mut frame = vec![0u8; (w * h * 4) as usize];
        for (i, px) in frame.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            let top = (i as u32 / w) < h / 2;
            // Blue, green, red, unused.
            px.copy_from_slice(if top { &[0, 0, 255, 255] } else { &[255, 0, 0, 255] });
        }
        // Windows Server (CI) can be missing Media Foundation entirely.
        let mut v = match VideoWriter::create(&path, w, h, 30) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("no video encoder here ({e}); skipping");
                return;
            }
        };
        for i in 0..15 {
            v.write(&frame, i as f64 / 30.0, 1.0 / 30.0).expect("a frame is written");
        }
        v.finish().expect("the file is closed");
        let len = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        assert!(len > 1000, "the video has something in it: {len} bytes");

        let png = dir.join("frame.png");
        let decoded = std::process::Command::new("ffmpeg")
            .args(["-loglevel", "error", "-y", "-i"])
            .arg(&path)
            .args(["-frames:v", "1"])
            .arg(&png)
            .status();
        if !decoded.is_ok_and(|s| s.success()) {
            eprintln!("no ffmpeg to decode with; only checked the file was written");
            return;
        }
        let img = image::open(&png).unwrap().to_rgba8();
        assert_eq!((img.width(), img.height()), (w, h));
        let near = |p: &image::Rgba<u8>, rgb: [u8; 3]| p.0.iter().zip(rgb).all(|(a, b)| a.abs_diff(b) < 40);
        assert!(near(img.get_pixel(w / 2, h / 4), [255, 0, 0]), "top is red: {:?}", img.get_pixel(w / 2, h / 4));
        assert!(
            near(img.get_pixel(w / 2, h * 3 / 4), [0, 0, 255]),
            "bottom is blue: {:?}",
            img.get_pixel(w / 2, h * 3 / 4)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
