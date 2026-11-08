//! Fonts for text boxes. The editor and every exporter find faces through the same
//! database, so a text box renders in the same face on screen and in exports.

use std::sync::{Arc, OnceLock, RwLock};

use resvg::usvg::fontdb;

use crate::handwriting::FAMILY_NAME as HANDWRITING_FAMILY;
use crate::model::DEFAULT_FONT;

/// System fonts plus the bundled default font. Loading the system fonts takes a moment,
/// so callers on a UI thread should warm this up in the background first.
///
/// The handwriting font lives outside this database instead of being loaded into it:
/// rebuilding the whole system font database every time someone saves a new sample
/// would be slow, and `fontdb::Database` has no way to replace one face in place. See
/// [`set_handwriting`].
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

/// The user's own handwriting font ([`HANDWRITING_FAMILY`]), once saved in the
/// handwriting window. Kept separately from [`database`] so saving a new one takes
/// effect immediately, for everything that reads faces through this module: the editor
/// (via `FontBook`, once it drops its own cache), every exporter, and the font picker.
static HANDWRITING: RwLock<Option<Arc<Vec<u8>>>> = RwLock::new(None);

/// Install (or, with `None`, clear) the handwriting face. Called once at startup if a
/// font was saved before, and again every time the handwriting window saves a new one.
pub fn set_handwriting(data: Option<Vec<u8>>) {
    *HANDWRITING.write().unwrap() = data.map(Arc::new);
}

/// Whether a handwriting font has been saved and installed.
pub fn has_handwriting() -> bool {
    HANDWRITING.read().unwrap().is_some()
}

fn handwriting_face() -> Option<Face> {
    let data = HANDWRITING.read().unwrap().clone()?;
    Some(Face { data, index: 0, bold: false, italic: false })
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

/// Font families to offer: the user's own handwriting (if saved) first, then favorites
/// that are installed, then every other family.
pub fn families() -> (Vec<String>, Vec<String>) {
    let db = database();
    let mut all: Vec<String> = db.faces().filter_map(|f| f.families.first().map(|(name, _)| name.clone())).collect();
    all.sort_by_key(|n| n.to_lowercase());
    all.dedup();
    all.retain(|n| n != DEFAULT_FONT && !n.starts_with('.'));
    let mut favorites: Vec<String> =
        FAVORITES.iter().filter(|f| all.iter().any(|n| n == *f)).map(|f| f.to_string()).collect();
    if has_handwriting() {
        favorites.insert(0, HANDWRITING_FAMILY.to_string());
    }
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
    if family == HANDWRITING_FAMILY {
        return handwriting_face();
    }
    let db = database();
    let id = query(family, bold, italic)?;
    let info = db.face(id)?;
    let (is_bold, is_italic) = (info.weight.0 >= 600, info.style != fontdb::Style::Normal);
    db.with_face_data(id, |data, index| Face { data: Arc::new(data.to_vec()), index, bold: is_bold, italic: is_italic })
}

/// Whether `family` has real bold and italic faces. The handwriting font, like the
/// bundled default font, only ever has one.
pub fn styles(family: &str) -> (bool, bool) {
    if family == HANDWRITING_FAMILY {
        return (false, false);
    }
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

    /// The only test in this file that touches the handwriting slot; it cleans up after
    /// itself so it can't affect any other test's view of `families()`/`face()`.
    #[test]
    fn handwriting_family_is_offered_once_installed() {
        use crate::handwriting::{self, Samples};
        use crate::ink::StrokeBuilder;

        assert!(!has_handwriting());
        assert!(face(HANDWRITING_FAMILY, false, false).is_none());

        let mut samples = Samples::default();
        let mut b = StrokeBuilder::new(Samples::new_stroke(), 0.0, 0.001);
        b.push(0.0, [0.3, 0.5], 0.5);
        b.push(1.0 / 60.0, [0.6, 0.5], 0.5);
        samples.push_stroke('A', b.finish());
        set_handwriting(Some(handwriting::build_font(&samples)));

        assert!(has_handwriting());
        let f = face(HANDWRITING_FAMILY, true, true).expect("installed handwriting face");
        assert!(!f.bold && !f.italic);
        assert_eq!(styles(HANDWRITING_FAMILY), (false, false));
        assert_eq!(families().0.first().map(String::as_str), Some(HANDWRITING_FAMILY));

        set_handwriting(None);
        assert!(face(HANDWRITING_FAMILY, false, false).is_none());
    }
}
