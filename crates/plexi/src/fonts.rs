//! Gets the faces text boxes ask for into egui, on demand. Faces come from
//! `plexi_core::fonts`, the same place the exporters find them, so a text box looks the
//! same on screen and in an export.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egui::{Context, FontData, FontDefinitions, FontFamily};
use plexi_core::TextBox;

#[derive(Default)]
pub struct FontBook {
    /// The fonts egui has: its defaults plus every face loaded so far.
    defs: Option<FontDefinitions>,
    /// Faces looked up so far, and whether they were found.
    tried: HashMap<String, bool>,
    /// Faces egui can use this frame.
    ready: HashSet<String>,
    /// Faces handed to egui this frame; usable from the next one.
    pending: Vec<String>,
    styles: HashMap<String, (bool, bool)>,
    families: Option<Arc<(Vec<String>, Vec<String>)>>,
    /// What is typed in the font picker's search box.
    pub search: String,
}

impl FontBook {
    /// Load the system font list in the background so the first text box in another
    /// font doesn't wait for it.
    pub fn warm_up() {
        std::thread::spawn(|| {
            plexi_core::fonts::database();
        });
    }

    pub fn begin_frame(&mut self) {
        self.ready.extend(self.pending.drain(..));
    }

    /// The egui family that draws `t`. Until its face is loaded (and for a font that
    /// isn't installed) this is the default font.
    pub fn family(&mut self, ctx: &Context, t: &TextBox) -> FontFamily {
        let Some(name) = &t.font else { return FontFamily::Proportional };
        let key = format!("{name}|{}{}", t.bold as u8, t.italic as u8);
        if self.ready.contains(&key) {
            return FontFamily::Name(key.into());
        }
        if !self.tried.contains_key(&key) {
            let face = plexi_core::fonts::face(name, t.bold, t.italic);
            self.tried.insert(key.clone(), face.is_some());
            if let Some(face) = face {
                let defs = self.defs.get_or_insert_with(FontDefinitions::default);
                let mut data = FontData::from_owned(face.data.to_vec());
                data.index = face.index;
                defs.font_data.insert(key.clone(), Arc::new(data));
                // Characters the face lacks (emoji, symbols) fall back to the defaults.
                let mut chain = vec![key.clone()];
                chain.extend(defs.families.get(&FontFamily::Proportional).cloned().unwrap_or_default());
                defs.families.insert(FontFamily::Name(key.clone().into()), chain);
                ctx.set_fonts(defs.clone());
                self.pending.push(key);
                ctx.request_repaint();
            }
        }
        FontFamily::Proportional
    }

    /// Whether `family` has real (bold, italic) faces.
    pub fn styles(&mut self, family: Option<&str>) -> (bool, bool) {
        let family = family.unwrap_or(plexi_core::DEFAULT_FONT);
        *self.styles.entry(family.to_owned()).or_insert_with(|| plexi_core::fonts::styles(family))
    }

    /// Installed families: common ones first, then the rest.
    pub fn families(&mut self) -> Arc<(Vec<String>, Vec<String>)> {
        self.families.get_or_insert_with(|| Arc::new(plexi_core::fonts::families())).clone()
    }
}
