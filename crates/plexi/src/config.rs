//! User settings and where Plexi keeps its files.
//!
//! Installed builds keep data in `%APPDATA%\Plexi`. A portable build (an exe whose name
//! contains "portable", or one with a `plexi-portable` file beside it) keeps everything
//! in a `PlexiData` folder next to the exe instead, so it can live on a USB stick.
//! `PLEXI_DATA_DIR` overrides both (used by tests).

use std::path::{Path, PathBuf};

use plexi_core::{Background, Color, PageSize};
use serde::{Deserialize, Serialize};

pub fn is_portable() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    let named = exe.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.to_ascii_lowercase().contains("portable"));
    named || exe.with_file_name("plexi-portable").exists()
}

pub fn data_dir() -> PathBuf {
    let dir = if let Some(d) = std::env::var_os("PLEXI_DATA_DIR") {
        Some(PathBuf::from(d))
    } else if is_portable() {
        std::env::current_exe().ok().and_then(|e| e.parent().map(|p| p.join("PlexiData")))
    } else {
        directories::ProjectDirs::from("", "", "Plexi").map(|d| d.data_dir().to_path_buf())
    }
    .unwrap_or_else(|| std::env::temp_dir().join("Plexi"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// Where untitled notes are kept until they are saved somewhere.
pub fn scratch_path() -> PathBuf {
    data_dir().join("Untitled.plexi")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// File open at last exit; reopened on launch.
    pub last_file: Option<PathBuf>,
    pub recent: Vec<PathBuf>,
    /// One Euro smoothing amount, 0 (raw) ..= 1 (very smooth).
    pub smoothing: f32,
    pub pen_colors: Vec<Color>,
    pub highlighter_colors: Vec<Color>,
    pub pen_width: f32,
    pub highlighter_width: f32,
    pub eraser_size: f32,
    pub eraser_partial: bool,
    pub new_page_size: PageSize,
    pub new_page_background: Background,
    /// Toggle overlay drawing (and click-through) from anywhere.
    pub hotkey_draw: String,
    /// Clear the overlay ink from anywhere.
    pub hotkey_clear: String,
    /// Leave overlay mode and return to the notebook.
    pub hotkey_notebook: String,
    /// Keep overlay ink when leaving overlay mode.
    pub keep_overlay_ink: bool,
    pub autosave: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            last_file: None,
            recent: Vec::new(),
            smoothing: 0.35,
            pen_colors: vec![
                Color::rgb(0x1d, 0x1f, 0x24),
                Color::rgb(0x1f, 0x5f, 0xd6),
                Color::rgb(0xd6, 0x28, 0x28),
                Color::rgb(0x23, 0x9b, 0x4f),
                Color::rgb(0xf0, 0x8c, 0x00),
                Color::rgb(0x7b, 0x3f, 0xe4),
                Color::rgb(0xff, 0xff, 0xff),
            ],
            highlighter_colors: vec![
                Color::rgba(0xff, 0xe0, 0x2e, 110),
                Color::rgba(0x5c, 0xf0, 0x7a, 110),
                Color::rgba(0xff, 0x6b, 0xc8, 110),
                Color::rgba(0x4d, 0xc3, 0xff, 110),
            ],
            pen_width: 3.0,
            highlighter_width: 20.0,
            eraser_size: 14.0,
            eraser_partial: false,
            new_page_size: PageSize::Letter,
            new_page_background: Background::Blank,
            hotkey_draw: "Ctrl+Alt+D".into(),
            hotkey_clear: "Ctrl+Alt+X".into(),
            hotkey_notebook: "Ctrl+Alt+N".into(),
            keep_overlay_ink: true,
            autosave: true,
        }
    }
}

impl Config {
    fn path() -> PathBuf {
        data_dir().join("config.json")
    }

    pub fn load() -> Self {
        std::fs::read(Self::path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(Self::path(), json);
        }
    }

    pub fn push_recent(&mut self, path: &Path) {
        self.recent.retain(|p| p != path);
        self.recent.insert(0, path.to_path_buf());
        self.recent.truncate(10);
    }
}
