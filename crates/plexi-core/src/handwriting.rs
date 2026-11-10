//! The user's own handwriting as a font: samples captured with the pen in the
//! handwriting window, and the TrueType font built from them.
//!
//! Samples are stored in *box-normalized* coordinates: `x` is 0 at the left edge of the
//! writing cell, `y` is 0 at the top (the ascender line) and 1 at the bottom (the
//! descender line); both axes use the same physical scale, so a pen's round tip stays
//! round instead of being squashed into an ellipse. The baseline sits at [`BASELINE_Y`],
//! the x-height guide at [`XHEIGHT_Y`]. [`BOX_WIDTH`] is just where the writing cell's
//! right edge is drawn — wider or narrower ink is still accepted and still exported.
//!
//! [`build_font`] turns the samples into a real TrueType font: every pen stroke is a
//! disc-and-hull "pen piece" (see [`crate::ink::pen_pieces`]), and each piece becomes
//! its own closed polygon contour in the glyph outline. Contours are allowed to
//! overlap — TrueType's non-zero winding rule fills anywhere a contour covers, so as
//! long as every contour winds the same way nothing cancels out, and no Minkowski-sum
//! polygon union is needed.

use std::collections::BTreeMap;
use std::path::Path;

use kurbo::{BezPath, Point};
use serde::{Deserialize, Serialize};
use write_fonts::FontBuilder;
use write_fonts::tables::cmap::Cmap;
use write_fonts::tables::glyf::{Bbox, Contour, Glyf, GlyfLocaBuilder, Glyph, SimpleGlyph};
use write_fonts::tables::head::Head;
use write_fonts::tables::hhea::Hhea;
use write_fonts::tables::hmtx::{Hmtx, LongMetric};
use write_fonts::tables::loca::LocaFormat;
use write_fonts::tables::maxp::Maxp;
use write_fonts::tables::name::{Name, NameRecord};
use write_fonts::tables::os2::{Os2, SelectionFlags};
use write_fonts::tables::post::Post;
use write_fonts::types::{Fixed, GlyphId, NameId, Tag, Version16Dot16};

use crate::ink::{PenPiece, pen_pieces};
use crate::model::{Color, InkKind, Stroke, new_id};

/// The name the first handwriting font gets (and the one samples saved before fonts had
/// names are loaded as).
pub const FAMILY_NAME: &str = "My Handwriting";

/// Characters the handwriting window offers a box for. Anything not written falls back
/// to the default font.
pub const GRID_CHARS: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789.,!?'\"-():;&@";

/// The default pen width for a new stroke, in the same normalized units as `x`/`y`.
pub const PEN_WIDTH: f32 = 0.06;

/// Em size of the generated font. 1000 is the usual round number for a TrueType font
/// (versus 2048 for most CFF fonts), which also makes it equal to [`SPAN`] below.
const UNITS_PER_EM: u16 = 1000;
/// Top of the writing cell (`y` = 0), in font design units above the baseline.
const ASCENT: f32 = 800.0;
/// Bottom of the writing cell (`y` = 1), in font design units above the baseline
/// (negative: below it).
const DESCENT: f32 = -200.0;
/// Font units spanned by one normalized unit, on either axis.
const SPAN: f32 = ASCENT - DESCENT;
/// Left and right side bearing added around each glyph's ink.
const MARGIN: f32 = 40.0;
/// Advance width of the space glyph, which is never drawn but always present.
const SPACE_ADVANCE: f32 = 320.0;

/// Where the baseline sits in box-normalized `y` (0 top, 1 bottom).
pub const BASELINE_Y: f32 = ASCENT / SPAN;
/// Where the x-height guide sits in box-normalized `y`.
pub const XHEIGHT_Y: f32 = (ASCENT - 500.0) / SPAN;
/// Where the writing cell's right guide is drawn, in the same normalized units as `y`.
pub const BOX_WIDTH: f32 = 0.74;

/// Pen samples for the handwriting font: the strokes written for each character, in
/// box-normalized coordinates (see the module docs). Keyed by the character as a
/// one-character string, so the saved file is plain JSON a person could hand-edit.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Samples {
    pub glyphs: BTreeMap<String, Vec<Stroke>>,
}

impl Samples {
    pub fn strokes(&self, c: char) -> &[Stroke] {
        self.glyphs.get(c.to_string().as_str()).map(Vec::as_slice).unwrap_or(&[])
    }

    /// Start a fresh stroke for `c`, with [`PEN_WIDTH`] and black ink (color and kind
    /// don't matter for the font; they're only there because it's a [`Stroke`]).
    pub fn new_stroke() -> Stroke {
        Stroke {
            id: new_id(),
            kind: InkKind::Pen,
            color: Color::BLACK,
            width: PEN_WIDTH,
            points: Vec::new(),
            group: 0,
            locked: false,
        }
    }

    pub fn push_stroke(&mut self, c: char, stroke: Stroke) {
        self.glyphs.entry(c.to_string()).or_default().push(stroke);
    }

    pub fn clear_glyph(&mut self, c: char) {
        self.glyphs.remove(c.to_string().as_str());
    }

    /// Characters with at least one stroke, sorted and deduplicated.
    pub fn written_chars(&self) -> Vec<char> {
        let mut out: Vec<char> =
            self.glyphs.iter().filter(|(_, s)| !s.is_empty()).filter_map(|(k, _)| k.chars().next()).collect();
        out.sort_unstable();
        out.dedup();
        out
    }

    pub fn is_empty(&self) -> bool {
        self.written_chars().is_empty()
    }
}

/// One named handwriting font: its family name and the samples it's built from.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct HandwritingFont {
    pub name: String,
    pub samples: Samples,
}

/// Every handwriting font, as saved on disk.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Library {
    // No `default` here: a file of bare samples (from before fonts had names) must fail
    // to parse as a library, so `load` can read it the old way instead.
    pub fonts: Vec<HandwritingFont>,
}

/// Load the saved fonts, or none if there's no file yet (a fresh install, or one that
/// was deleted by hand). A file from before fonts had names becomes one font called
/// [`FAMILY_NAME`].
pub fn load(path: &Path) -> Library {
    let Ok(bytes) = std::fs::read(path) else { return Library::default() };
    if let Ok(lib) = serde_json::from_slice::<Library>(&bytes) {
        return lib;
    }
    match serde_json::from_slice::<Samples>(&bytes) {
        Ok(samples) if !samples.is_empty() => {
            Library { fonts: vec![HandwritingFont { name: FAMILY_NAME.into(), samples }] }
        }
        _ => Library::default(),
    }
}

/// Save the fonts as pretty-printed JSON, so they're still readable (and editable) later.
pub fn save(lib: &Library, path: &Path) -> std::io::Result<()> {
    let json = serde_json::to_vec_pretty(lib).unwrap_or_default();
    std::fs::write(path, json)
}

/// Signed area of a closed polygon (the shoelace formula, not divided by two — only the
/// sign is used, to tell which way a contour winds).
fn signed_area(poly: &[[f32; 2]]) -> f32 {
    let n = poly.len();
    (0..n).map(|i| poly[i][0] * poly[(i + 1) % n][1] - poly[(i + 1) % n][0] * poly[i][1]).sum()
}

/// A disc approximated as a many-sided polygon, wound the same way
/// [`crate::ink::tangent_hull`] winds its quads (positive shoelace), so every piece of
/// a stroke contributes the same sign to the glyph's fill and nothing cancels where
/// pieces overlap.
fn disc_polygon(c: [f32; 2], r: f32) -> Vec<[f32; 2]> {
    const SEGMENTS: usize = 24;
    let mut poly: Vec<[f32; 2]> = (0..SEGMENTS)
        .map(|k| {
            let a = k as f32 / SEGMENTS as f32 * std::f32::consts::TAU;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect();
    if signed_area(&poly) < 0.0 {
        poly.reverse();
    }
    poly
}

/// Every piece of every stroke of a glyph, as closed polygons in box-normalized
/// coordinates, all consistently wound.
///
/// The pen shape is worked out in font units, not box units: [`crate::ink::radius`] has
/// a floor meant for page units (0.3), which in box units would make every line at
/// least 30% of the box wide.
fn glyph_polygons(strokes: &[Stroke]) -> Vec<Vec<[f32; 2]>> {
    let down = |p: [f32; 2]| [p[0] / SPAN, p[1] / SPAN];
    let mut out = Vec::new();
    for stroke in strokes {
        let mut big = stroke.clone();
        big.width *= SPAN;
        for pt in &mut big.points {
            pt.x *= SPAN;
            pt.y *= SPAN;
        }
        for piece in pen_pieces(&big) {
            match piece {
                PenPiece::Disc { c, r } if r > 1e-5 => out.push(disc_polygon(down(c), r / SPAN)),
                PenPiece::Hull(q) => out.push(q.map(down).to_vec()),
                _ => {}
            }
        }
    }
    out.retain(|p| p.len() >= 3);
    out
}

/// A glyph's outline, its left side bearing and its advance width, all in font design
/// units — or `None` if the character has no ink at all.
///
/// The outline is shifted so its ink starts exactly [`MARGIN`] units from the glyph
/// origin: that fixes the left side bearing at a known constant, and (since every pen
/// piece already has a consistent winding, see [`glyph_polygons`]) the global flip from
/// box-normalized `y` (down) to font-space `y` (up) flips every contour together, so
/// winding stays consistent after the transform too.
fn glyph_outline(strokes: &[Stroke]) -> Option<(BezPath, f32, f32)> {
    let polys = glyph_polygons(strokes);
    if polys.is_empty() {
        return None;
    }
    let min_x = polys.iter().flatten().map(|p| p[0]).fold(f32::MAX, f32::min);
    let shift = MARGIN - min_x * SPAN;
    let to_pt = |x: f32, y: f32| Point::new((x * SPAN + shift) as f64, (ASCENT - y * SPAN) as f64);

    let mut path = BezPath::new();
    let mut max_x = f32::MIN;
    for poly in &polys {
        for &[x, _] in poly {
            max_x = max_x.max(x * SPAN + shift);
        }
        path.move_to(to_pt(poly[0][0], poly[0][1]));
        for &[x, y] in &poly[1..] {
            path.line_to(to_pt(x, y));
        }
        path.close_path();
    }
    Some((path, MARGIN, max_x + MARGIN))
}

/// Build a TrueType font from the given samples: one glyph per written character, plus
/// `.notdef` and a space. Returns the font's bytes, ready for `fontdb` or egui.
///
/// Characters that weren't written (an empty or missing entry) get no glyph and no
/// `cmap` mapping at all, so a renderer falls back to its next font for them, exactly
/// like a face that's simply missing a glyph.
pub fn build_font(samples: &Samples, family: &str) -> Vec<u8> {
    let chars = samples.written_chars();

    let mut builder = GlyfLocaBuilder::new();
    let mut hmtx = Vec::new();
    let mut cmap_mappings = Vec::new();
    let mut bbox = Bbox::default();
    let mut max_points = 0usize;
    let mut max_contours = 0usize;

    // Glyph 0 is the required, always-empty `.notdef`.
    builder.add_glyph(&Glyph::Empty).expect("an empty glyph always compiles");
    hmtx.push(LongMetric { advance: SPACE_ADVANCE as u16, side_bearing: 0 });

    for (i, &c) in chars.iter().enumerate() {
        match glyph_outline(samples.strokes(c)) {
            Some((path, lsb, advance)) => {
                let glyph = SimpleGlyph::from_bezpath(&path).expect("outlines are lines only, never cubics");
                max_points = max_points.max(glyph.contours.iter().map(Contour::len).sum());
                max_contours = max_contours.max(glyph.contours.len());
                let glyph = Glyph::from(glyph);
                if let Some(b) = glyph.bbox() {
                    bbox = bbox.union(b);
                }
                builder.add_glyph(&glyph).expect("a simple glyph of line segments always compiles");
                hmtx.push(LongMetric { advance: advance.round() as u16, side_bearing: lsb.round() as i16 });
            }
            None => {
                builder.add_glyph(&Glyph::Empty).expect("an empty glyph always compiles");
                hmtx.push(LongMetric { advance: SPACE_ADVANCE as u16, side_bearing: 0 });
            }
        }
        cmap_mappings.push((c, GlyphId::new((i + 1) as u32)));
    }
    // The space is never drawn (there's nothing to write), but always present so words
    // keep their gaps even when every other glyph fell back to the default font.
    builder.add_glyph(&Glyph::Empty).expect("an empty glyph always compiles");
    hmtx.push(LongMetric { advance: SPACE_ADVANCE as u16, side_bearing: 0 });
    cmap_mappings.push((' ', GlyphId::new((chars.len() + 1) as u32)));

    let (glyf, loca, loca_format) = builder.build();
    let num_glyphs = hmtx.len() as u16;

    let head = Head {
        font_revision: Fixed::from_i32(1),
        units_per_em: UNITS_PER_EM,
        x_min: bbox.x_min,
        y_min: DESCENT as i16,
        x_max: bbox.x_max,
        y_max: ASCENT as i16,
        lowest_rec_ppem: 8,
        index_to_loc_format: (loca_format == LocaFormat::Long) as i16,
        ..Default::default()
    };
    let hhea = Hhea {
        ascender: (ASCENT as i16).into(),
        descender: (DESCENT as i16).into(),
        line_gap: 90i16.into(),
        advance_width_max: (hmtx.iter().map(|m| m.advance).max().unwrap_or(0)).into(),
        min_left_side_bearing: (MARGIN as i16).into(),
        min_right_side_bearing: (MARGIN as i16).into(),
        x_max_extent: bbox.x_max.into(),
        caret_slope_rise: 1,
        caret_slope_run: 0,
        caret_offset: 0,
        number_of_h_metrics: num_glyphs,
    };
    let maxp = Maxp {
        num_glyphs,
        max_points: Some(max_points as u16),
        max_contours: Some(max_contours as u16),
        max_composite_points: Some(0),
        max_composite_contours: Some(0),
        max_zones: Some(1),
        max_twilight_points: Some(0),
        max_storage: Some(0),
        max_function_defs: Some(0),
        max_instruction_defs: Some(0),
        max_stack_elements: Some(0),
        max_size_of_instructions: Some(0),
        max_component_elements: Some(0),
        max_component_depth: Some(0),
    };
    let os2 = Os2 {
        us_weight_class: 400,
        ach_vend_id: Tag::new(b"PLXI"),
        fs_selection: SelectionFlags::REGULAR,
        us_first_char_index: cmap_mappings.iter().map(|(c, _)| *c as u32).min().unwrap_or(0x20) as u16,
        us_last_char_index: cmap_mappings.iter().map(|(c, _)| *c as u32).max().unwrap_or(0x7e).min(0xFFFF) as u16,
        s_typo_ascender: ASCENT as i16,
        s_typo_descender: DESCENT as i16,
        s_typo_line_gap: 90,
        us_win_ascent: ASCENT as u16,
        us_win_descent: (-DESCENT) as u16,
        ul_code_page_range_1: Some(1), // bit 0: Latin 1
        ul_code_page_range_2: Some(0),
        sx_height: Some(500),
        s_cap_height: Some(700),
        us_default_char: Some(0),
        us_break_char: Some(0x20),
        us_max_context: Some(0),
        ..Default::default()
    };
    // PostScript names allow only printable ASCII without spaces and a few symbols.
    let ps: String = family.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-').collect();
    let ps = if ps.is_empty() { "Handwriting".to_owned() } else { ps };
    let name = Name::new(vec![
        NameRecord::new(3, 1, 0x409, NameId::FAMILY_NAME, family.to_string().into()),
        NameRecord::new(3, 1, 0x409, NameId::SUBFAMILY_NAME, "Regular".to_string().into()),
        NameRecord::new(3, 1, 0x409, NameId::UNIQUE_ID, format!("1.0;Plexi;{ps}").into()),
        NameRecord::new(3, 1, 0x409, NameId::FULL_NAME, family.to_string().into()),
        NameRecord::new(3, 1, 0x409, NameId::VERSION_STRING, "Version 1.0".to_string().into()),
        NameRecord::new(3, 1, 0x409, NameId::POSTSCRIPT_NAME, format!("{ps}-Regular").into()),
    ]);
    let post = Post {
        version: Version16Dot16::VERSION_3_0,
        underline_position: (-100i16).into(),
        underline_thickness: (50i16).into(),
        ..Default::default()
    };
    let cmap = Cmap::from_mappings(cmap_mappings).expect("characters are deduplicated, so no id can conflict");

    let mut fb = FontBuilder::new();
    fb.add_table(&head).expect("head always compiles");
    fb.add_table(&hhea).expect("hhea always compiles");
    fb.add_table(&maxp).expect("maxp always compiles");
    fb.add_table(&os2).expect("os2 always compiles");
    fb.add_table(&Hmtx::new(hmtx, Vec::new())).expect("hmtx always compiles");
    fb.add_table(&cmap).expect("cmap always compiles");
    fb.add_table(&glyf_table(glyf)).expect("glyf always compiles");
    fb.add_table(&loca).expect("loca always compiles");
    fb.add_table(&name).expect("name always compiles");
    fb.add_table(&post).expect("post always compiles");
    fb.build()
}

/// `GlyfLocaBuilder::build` already returns a [`Glyf`]; this just names the no-op so the
/// call sites above read as a flat list of tables.
fn glyf_table(glyf: Glyf) -> Glyf {
    glyf
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ink::StrokeBuilder;

    /// A short left-to-right stroke across the middle of the box, as if someone dragged
    /// the pen for the given character.
    fn scribble(c: char, samples: &mut Samples) {
        let mut b = StrokeBuilder::new(Samples::new_stroke(), 0.0, 0.001);
        for i in 0..=10 {
            let t = i as f32 / 10.0;
            b.push(t as f64 / 60.0, [0.2 + t * 0.4, BASELINE_Y - 0.2], 0.5);
        }
        samples.push_stroke(c, b.finish());
    }

    #[test]
    fn written_chars_ignore_empty_entries() {
        let mut s = Samples::default();
        s.glyphs.insert("Z".into(), Vec::new()); // cleared, or never actually drawn
        assert!(s.is_empty());
        scribble('A', &mut s);
        assert_eq!(s.written_chars(), vec!['A']);
    }

    #[test]
    fn empty_samples_build_a_font_with_only_notdef_and_space() {
        let bytes = build_font(&Samples::default(), FAMILY_NAME);
        let face = ttf_parser::Face::parse(&bytes, 0).expect("a valid font, even with nothing drawn");
        assert_eq!(face.number_of_glyphs(), 2); // .notdef, space
        assert!(face.glyph_index(' ').is_some());
        assert!(face.glyph_index('A').is_none());
    }

    #[test]
    fn a_written_character_gets_a_filled_outline() {
        let mut samples = Samples::default();
        scribble('A', &mut samples);
        let bytes = build_font(&samples, FAMILY_NAME);

        let face = ttf_parser::Face::parse(&bytes, 0).expect("fontdb/ttf-parser can read the generated font");
        let gid = face.glyph_index('A').expect("the written character is mapped in cmap");

        struct CountingSink(u32);
        impl ttf_parser::OutlineBuilder for CountingSink {
            fn move_to(&mut self, _x: f32, _y: f32) {
                self.0 += 1;
            }
            fn line_to(&mut self, _x: f32, _y: f32) {}
            fn quad_to(&mut self, _x1: f32, _y1: f32, _x: f32, _y: f32) {}
            fn curve_to(&mut self, _x1: f32, _y1: f32, _x2: f32, _y2: f32, _x: f32, _y: f32) {}
            fn close(&mut self) {}
        }
        let mut sink = CountingSink(0);
        let bbox = face.outline_glyph(gid, &mut sink).expect("the glyph has an outline");
        assert!(sink.0 > 0, "at least one contour");
        assert!(bbox.width() > 0 && bbox.height() > 0, "the outline covers real area");
        // A flat stroke is only as tall as the pen is wide (0.06 of the box, 60 units),
        // not a blob.
        assert!(bbox.height() < 100, "outline is {} units tall", bbox.height());

        // An unwritten character has no glyph at all, so renderers fall back.
        assert!(face.glyph_index('Q').is_none());
    }

    #[test]
    fn fontdb_also_reads_the_family_name() {
        let mut samples = Samples::default();
        scribble('A', &mut samples);
        let bytes = build_font(&samples, FAMILY_NAME);

        let mut db = resvg::usvg::fontdb::Database::new();
        db.load_font_data(bytes);
        let found = db.faces().any(|f| f.families.iter().any(|(name, _)| name == FAMILY_NAME));
        assert!(found, "fontdb should see the family name from the `name` table");
    }

    #[test]
    fn roundtrips_through_json() {
        let mut samples = Samples::default();
        scribble('B', &mut samples);
        let path = std::env::temp_dir().join(format!("plexi-handwriting-test-{}.json", new_id()));
        let lib = Library { fonts: vec![HandwritingFont { name: "B".into(), samples: samples.clone() }] };
        save(&lib, &path).unwrap();
        let loaded = load(&path).fonts.remove(0).samples;
        let _ = std::fs::remove_file(&path);
        assert_eq!(loaded.written_chars(), vec!['B']);
        // Stroke ids aren't persisted (they're regenerated, like everywhere else in the
        // document), so compare the ink itself rather than the whole struct.
        assert_eq!(loaded.strokes('B')[0].points, samples.strokes('B')[0].points);
        assert_eq!(loaded.strokes('B')[0].width, samples.strokes('B')[0].width);
    }

    #[test]
    fn fonts_have_names_and_old_samples_still_load() {
        let dir = std::env::temp_dir().join(format!("plexi-hw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("handwriting.json");
        let mut samples = Samples::default();
        scribble('A', &mut samples);
        // A file of bare samples, the way they were saved before fonts had names.
        std::fs::write(&path, serde_json::to_vec(&samples).unwrap()).unwrap();
        let lib = load(&path);
        assert_eq!(lib.fonts.len(), 1);
        assert_eq!(lib.fonts[0].name, FAMILY_NAME);
        assert!(!lib.fonts[0].samples.is_empty());

        let lib = Library {
            fonts: vec![
                HandwritingFont { name: "Neat".into(), samples: samples.clone() },
                HandwritingFont { name: "Messy".into(), samples },
            ],
        };
        save(&lib, &path).unwrap();
        let back = load(&path);
        let names: Vec<&str> = back.fonts.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["Neat", "Messy"]);
        assert_eq!(back.fonts[1].samples.written_chars(), vec!['A']);
        let bytes = build_font(&lib.fonts[1].samples, "Messy");
        let face = ttf_parser::Face::parse(&bytes, 0).unwrap();
        let family = face.names().into_iter().find(|n| n.name_id == ttf_parser::name_id::FAMILY);
        assert_eq!(family.and_then(|n| n.to_string()).as_deref(), Some("Messy"));
        std::fs::remove_dir_all(dir).ok();
    }
}
