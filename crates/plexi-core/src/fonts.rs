//! Fonts for text boxes. The editor and every exporter find faces through the same
//! database, so a text box renders in the same face on screen and in exports.

use std::sync::{Arc, OnceLock};

use resvg::usvg::fontdb;

use crate::model::DEFAULT_FONT;

/// System fonts plus the bundled default font. Loading the system fonts takes a moment,
/// so callers on a UI thread should warm this up in the background first.
pub fn database() -> Arc<fontdb::Database> {
    static DB: OnceLock<Arc<fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = fontdb::Database::new();
        db.load_font_data(epaint_default_fonts::UBUNTU_LIGHT.to_vec());
        db.load_system_fonts();
        db.set_sans_serif_family(DEFAULT_FONT);
        Arc::new(db)
    })
    .clone()
}

/// Common fonts listed first in the font picker, when installed.
const FAVORITES: &[&str] = &[
    "Segoe UI",
    "Arial",
    "Calibri",
    "Verdana",
    "Helvetica",
    "Georgia",
    "Times New Roman",
    "Cambria",
    "Garamond",
    "Consolas",
    "Courier New",
    "Menlo",
    "Comic Sans MS",
    "Segoe Print",
    "Ink Free",
];

/// Font families to offer: favorites that are installed, then every other family.
pub fn families() -> (Vec<String>, Vec<String>) {
    let db = database();
    let mut all: Vec<String> = db.faces().filter_map(|f| f.families.first().map(|(name, _)| name.clone())).collect();
    all.sort_by_key(|n| n.to_lowercase());
    all.dedup();
    all.retain(|n| n != DEFAULT_FONT && !n.starts_with('.'));
    let favorites: Vec<String> =
        FAVORITES.iter().filter(|f| all.iter().any(|n| n == *f)).map(|f| f.to_string()).collect();
    let rest = all.into_iter().filter(|n| !favorites.contains(n)).collect();
    (favorites, rest)
}

/// The CSS/SVG weight for a text box: the default font only has a light face.
pub fn weight(family: &str, bold: bool) -> u16 {
    match (bold, family == DEFAULT_FONT) {
        (true, _) => 700,
        (false, true) => 300,
        (false, false) => 400,
    }
}

/// One font face file.
#[derive(Clone)]
pub struct Face {
    pub data: Arc<Vec<u8>>,
    pub index: u32,
    /// Whether the face really is bold / italic (a family may lack those faces).
    pub bold: bool,
    pub italic: bool,
}

fn query(family: &str, bold: bool, italic: bool) -> Option<fontdb::ID> {
    let db = database();
    let families = [fontdb::Family::Name(family)];
    let q = fontdb::Query {
        families: &families,
        weight: fontdb::Weight(weight(family, bold)),
        stretch: fontdb::Stretch::Normal,
        style: if italic { fontdb::Style::Italic } else { fontdb::Style::Normal },
    };
    db.query(&q)
}

/// The face `family` uses for the given style, if the family is installed.
pub fn face(family: &str, bold: bool, italic: bool) -> Option<Face> {
    let db = database();
    let id = query(family, bold, italic)?;
    let info = db.face(id)?;
    let (is_bold, is_italic) = (info.weight.0 >= 600, info.style != fontdb::Style::Normal);
    db.with_face_data(id, |data, index| Face { data: Arc::new(data.to_vec()), index, bold: is_bold, italic: is_italic })
}

/// Whether `family` has real bold and italic faces.
pub fn styles(family: &str) -> (bool, bool) {
    let db = database();
    let has = |bold, italic| {
        query(family, bold, italic)
            .and_then(|id| db.face(id))
            .is_some_and(|f| (!bold || f.weight.0 >= 600) && (!italic || f.style != fontdb::Style::Normal))
    };
    (has(true, false), has(false, true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_font_is_always_there() {
        let f = face(DEFAULT_FONT, false, false).expect("bundled font");
        assert!(!f.bold && !f.italic);
        assert!(!f.data.is_empty());
        assert_eq!(styles(DEFAULT_FONT), (false, false));
        assert!(face("No Such Font Family", false, false).is_none());
    }
}
