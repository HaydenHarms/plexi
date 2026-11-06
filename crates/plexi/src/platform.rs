//! Everything that talks to Win32 directly lives here: overlay window placement, screen
//! capture, and attaching to the parent console for command-line use.
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
}

pub use imp::*;

/// The native window handle behind eframe's window, as an integer.
pub fn native_handle(frame: &eframe::Frame) -> Option<isize> {
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match frame.window_handle().ok()?.as_raw() {
        #[cfg(windows)]
        RawWindowHandle::Win32(h) => Some(h.hwnd.get()),
        _ => None,
    }
}
