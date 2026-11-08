//! Plexi: a light, fast notebook for handwriting and sketching that can also turn your
//! whole screen into a sheet of glass to draw on.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod canvas;
mod cli;
mod config;
mod fonts;
mod handwriting;
mod history;
mod platform;
#[cfg(test)]
mod reel;
mod shell;
mod ui;
mod updater;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

/// The app icon as RGBA at `size` pixels.
pub fn app_icon_rgba(size: u32) -> (Vec<u8>, u32, u32) {
    let img = image::load_from_memory(include_bytes!("../../../assets/plexi-256.png"))
        .map(|i| i.resize_exact(size, size, image::imageops::FilterType::Lanczos3).to_rgba8())
        .unwrap_or_else(|_| image::RgbaImage::from_pixel(size, size, image::Rgba([47, 124, 246, 255])));
    (img.into_raw(), size, size)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if cli::is_cli(&args) {
        platform::attach_parent_console();
        return cli::run(&args);
    }
    env_logger::init();

    let start = app::StartOptions {
        file: args.iter().find(|a| !a.starts_with("--")).map(PathBuf::from),
        overlay: args.iter().any(|a| a == "--draw" || a == "--overlay"),
        background: args.iter().any(|a| a == "--background"),
        shell: true,
    };
    let (rgba, w, h) = app_icon_rgba(64);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Plexi")
        .with_app_id("plexi")
        .with_inner_size([1040.0, 760.0])
        .with_clamp_size_to_monitor_size(true)
        .with_min_inner_size([480.0, 360.0])
        .with_transparent(true)
        .with_drag_and_drop(true)
        .with_icon(Arc::new(egui::IconData { rgba, width: w, height: h }));
    if start.overlay || start.background {
        // Start already frameless so the notebook window never flashes up first.
        viewport = viewport.with_decorations(false).with_taskbar(false).with_always_on_top();
    }
    if start.background {
        viewport = viewport.with_mouse_passthrough(true);
    }
    let options =
        eframe::NativeOptions { viewport, renderer: eframe::Renderer::Glow, centered: true, ..Default::default() };
    let result = eframe::run_native("Plexi", options, Box::new(move |cc| Ok(Box::new(app::PlexiApp::new(cc, start)))));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("{e}");
            ExitCode::FAILURE
        }
    }
}
