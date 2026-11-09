//! Fonts for text boxes. The editor and every exporter find faces through the same
//! database, so a text box renders in the same face on screen and in exports.

use std::sync::{Arc, OnceLock, RwLock};

use resvg::usvg::fontdb;

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

/// The user's own handwriting fonts, by family name, once saved in the handwriting
/// window. Kept separately from [`database`] so saving a new one takes effect
/// immediately, for everything that reads faces through this module: the editor (via
/// `FontBook`, once it drops its own cache), every exporter, and the font picker.
static HANDWRITING: RwLock<Vec<(String, Arc<Vec<u8>>)>> = RwLock::new(Vec::new());

/// Install (or, with `None`, remove) the handwriting font called `family`. Called at
/// startup for every font saved before, and again each time the handwriting window
/// saves, renames or deletes one.
pub fn set_handwriting(family: &str, data: Option<Vec<u8>>) {
    let mut fonts = HANDWRITING.write().unwrap();
    fonts.retain(|(n, _)| n != family);
    if let Some(d) = data {
        fonts.push((family.to_owned(), Arc::new(d)));
    }
}

/// The handwriting fonts installed, in the order they were made.
pub fn handwriting_families() -> Vec<String> {
    HANDWRITING.read().unwrap().iter().map(|(n, _)| n.clone()).collect()
}

/// Whether `family` is one of the user's handwriting fonts.
pub fn is_handwriting(family: &str) -> bool {
    HANDWRITING.read().unwrap().iter().any(|(n, _)| n == family)
}

fn handwriting_face(family: &str) -> Option<Face> {
    let data = HANDWRITING.read().unwrap().iter().find(|(n, _)| n == family)?.1.clone();
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

/// Font families to offer: the user's own handwriting fonts first, then favorites that
/// are installed, then every other family.
pub fn families() -> (Vec<String>, Vec<String>) {
    let db = database();
    let mut all: Vec<String> = db.faces().filter_map(|f| f.families.first().map(|(name, _)| name.clone())).collect();
    all.sort_by_key(|n| n.to_lowercase());
    all.dedup();
    all.retain(|n| n != DEFAULT_FONT && !n.starts_with('.'));
    let mut favorites: Vec<String> =
        FAVORITES.iter().filter(|f| all.iter().any(|n| n == *f)).map(|f| f.to_string()).collect();
    let hand = handwriting_families();
    favorites.retain(|f| !hand.contains(f));
    favorites.splice(0..0, hand);
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
    if let Some(f) = handwriting_face(family) {
        return Some(f);
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
    if is_handwriting(family) {
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
    fn handwriting_families_are_offered_once_installed() {
        use crate::handwriting::{self, Samples};
        use crate::ink::StrokeBuilder;

        // Names no other test installs, so this can't affect any other test.
        let (neat, messy) = ("Test Neat Hand", "Test Messy Hand");
        assert!(!is_handwriting(neat));
        assert!(face(neat, false, false).is_none());

        let mut samples = Samples::default();
        let mut b = StrokeBuilder::new(Samples::new_stroke(), 0.0, 0.001);
        b.push(0.0, [0.3, 0.5], 0.5);
        b.push(1.0 / 60.0, [0.6, 0.5], 0.5);
        samples.push_stroke('A', b.finish());
        set_handwriting(neat, Some(handwriting::build_font(&samples, neat)));
        set_handwriting(messy, Some(handwriting::build_font(&samples, messy)));

        assert!(is_handwriting(neat) && is_handwriting(messy));
        let f = face(neat, true, true).expect("installed handwriting face");
        assert!(!f.bold && !f.italic);
        assert_eq!(styles(messy), (false, false));
        let offered = families().0;
        assert!(offered.iter().any(|n| n == neat) && offered.iter().any(|n| n == messy), "{offered:?}");

        set_handwriting(neat, None);
        set_handwriting(messy, None);
        assert!(face(neat, false, false).is_none());
    }
}
