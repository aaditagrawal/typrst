//! Font metrics: static base-14 AFM widths plus dynamically loaded
//! `FontId::Ext` faces via `fontdb`. Zero I/O at startup for `Afm`.
//! Serif faces map to Times to visually match typst's Libertinus default.
use crate::fontdb::{self, FaceMetrics};
use crate::ir::{FontId, FontMetrics, FontUse};

pub struct Afm;

impl Afm {
    #[allow(dead_code)] // used by layout tests
    pub fn new() -> Self {
        Afm
    }
}

/// PDF BaseFont name for a base-14 face. Ext faces carry their own
/// PostScript name in `FontUse`; this fallback is never used for them.
pub fn base14_name(font: FontId) -> &'static str {
    match font {
        FontId::Regular => "Times-Roman",
        FontId::Bold => "Times-Bold",
        FontId::Italic => "Times-Italic",
        FontId::BoldItalic => "Times-BoldItalic",
        FontId::Mono => "Courier",
        FontId::MonoBold => "Courier-Bold",
        FontId::Ext(_) => "Times-Roman",
    }
}

/// Map a Unicode char to its WinAnsiEncoding byte, if representable.
pub(crate) fn winansi_code(c: char) -> Option<u8> {
    Some(match c {
        ' '..='~' | '\u{a0}'..='\u{ff}' => c as u8,
        '\u{20ac}' => 0x80,
        '\u{201a}' => 0x82,
        '\u{192}' => 0x83,
        '\u{201e}' => 0x84,
        '\u{2026}' => 0x85,
        '\u{2020}' => 0x86,
        '\u{2021}' => 0x87,
        '\u{2c6}' => 0x88,
        '\u{2030}' => 0x89,
        '\u{160}' => 0x8a,
        '\u{2039}' => 0x8b,
        '\u{152}' => 0x8c,
        '\u{17d}' => 0x8e,
        '\u{2018}' => 0x91,
        '\u{2019}' => 0x92,
        '\u{201c}' => 0x93,
        '\u{201d}' => 0x94,
        '\u{2022}' => 0x95,
        '\u{2013}' => 0x96,
        '\u{2014}' => 0x97,
        '\u{2dc}' => 0x98,
        '\u{2122}' => 0x99,
        '\u{161}' => 0x9a,
        '\u{203a}' => 0x9b,
        '\u{153}' => 0x9c,
        '\u{17e}' => 0x9e,
        '\u{178}' => 0x9f,
        _ => return None,
    })
}

/// Inverse of `winansi_code`: WinAnsi byte -> Unicode scalar (0 = unmapped).
/// Used to build `/Widths` arrays for embedded fonts.
pub(crate) fn winansi_inverse() -> [u32; 256] {
    let mut t = [0u32; 256];
    for u in 1..0x2200u32 {
        if let Some(c) = char::from_u32(u) {
            if let Some(b) = winansi_code(c) {
                t[b as usize] = u;
            }
        }
    }
    t
}

// Times widths in 1/1000 em (extracted from the system Times faces, which
// are metric-compatible with the Adobe base-14 AFMs), indexed by WinAnsi
// code minus 32. Undefined slots carry the '?' width; they are unreachable
// because winansi_code() never produces them.
static TIMES: [u16; 224] = [
    250, 333, 408, 500, 500, 833, 778, 180, 333, 333, 500, 564, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 278, 278, 564, 564, 564, 444, 921, 722, 667, 667, 722, 611,
    556, 722, 722, 333, 389, 722, 611, 889, 722, 722, 556, 722, 667, 556, 611, 722, 722, 944, 722,
    722, 611, 333, 278, 333, 469, 500, 333, 444, 500, 444, 500, 444, 333, 500, 500, 278, 278, 500,
    278, 778, 500, 500, 500, 500, 333, 389, 278, 500, 500, 722, 500, 500, 444, 480, 200, 480, 541,
    444, 744, 444, 333, 500, 444, 1000, 500, 500, 333, 1000, 556, 333, 889, 444, 611, 444, 444,
    333, 333, 444, 444, 350, 500, 1000, 333, 980, 389, 333, 722, 444, 444, 722, 250, 333, 500, 500,
    500, 500, 200, 500, 333, 760, 276, 500, 564, 333, 760, 333, 400, 549, 300, 300, 333, 576, 453,
    250, 333, 300, 310, 500, 750, 750, 750, 444, 722, 722, 722, 722, 722, 722, 889, 667, 611, 611,
    611, 611, 333, 333, 333, 333, 722, 722, 722, 722, 722, 722, 722, 564, 722, 722, 722, 722, 722,
    722, 556, 500, 444, 444, 444, 444, 444, 444, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278,
    500, 500, 500, 500, 500, 500, 500, 549, 500, 500, 500, 500, 500, 500, 500, 500,
];

static TIMES_BOLD: [u16; 224] = [
    250, 333, 555, 500, 500, 1000, 833, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 930, 722, 667, 722, 722, 667,
    611, 778, 778, 389, 500, 778, 667, 944, 722, 778, 611, 778, 722, 556, 667, 722, 722, 1000, 722,
    722, 667, 333, 278, 333, 581, 500, 333, 500, 556, 444, 556, 444, 333, 500, 556, 278, 333, 556,
    278, 833, 556, 500, 556, 556, 444, 389, 333, 556, 500, 722, 500, 500, 444, 394, 220, 394, 520,
    500, 744, 500, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 1000, 500, 667, 500, 500,
    333, 333, 500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 500, 444, 722, 250, 333, 500,
    500, 500, 500, 220, 500, 333, 747, 300, 500, 570, 333, 747, 333, 400, 549, 300, 300, 333, 576,
    540, 250, 333, 300, 330, 500, 750, 750, 750, 500, 722, 722, 722, 722, 722, 722, 1000, 722, 667,
    667, 667, 667, 389, 389, 389, 389, 722, 722, 778, 778, 778, 778, 778, 570, 778, 722, 722, 722,
    722, 722, 611, 556, 500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 444, 278, 278, 278,
    278, 500, 556, 500, 500, 500, 500, 500, 549, 500, 556, 556, 556, 556, 500, 556, 500,
];

static TIMES_ITALIC: [u16; 224] = [
    250, 333, 420, 500, 500, 833, 778, 214, 333, 333, 500, 675, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 675, 675, 675, 500, 920, 611, 611, 667, 722, 611,
    611, 722, 722, 333, 444, 667, 556, 833, 667, 722, 611, 722, 611, 500, 556, 722, 611, 833, 611,
    556, 556, 389, 278, 389, 422, 500, 333, 500, 500, 444, 500, 444, 278, 500, 500, 278, 278, 444,
    278, 722, 500, 500, 500, 500, 389, 389, 278, 500, 444, 667, 444, 444, 389, 400, 275, 400, 541,
    500, 744, 500, 333, 500, 556, 889, 500, 500, 333, 1000, 500, 333, 944, 500, 556, 500, 500, 333,
    333, 556, 556, 350, 500, 889, 333, 980, 389, 333, 667, 500, 389, 556, 250, 389, 500, 500, 500,
    500, 275, 500, 333, 760, 276, 500, 675, 333, 760, 333, 400, 549, 300, 300, 333, 576, 523, 250,
    333, 300, 310, 500, 750, 750, 750, 500, 611, 611, 611, 611, 611, 611, 889, 667, 611, 611, 611,
    611, 333, 333, 333, 333, 722, 667, 722, 722, 722, 722, 722, 675, 722, 722, 722, 722, 722, 556,
    611, 500, 500, 500, 500, 500, 500, 500, 667, 444, 444, 444, 444, 444, 278, 278, 278, 278, 500,
    500, 500, 500, 500, 500, 500, 549, 500, 500, 500, 500, 500, 444, 500, 444,
];

static TIMES_BOLD_ITALIC: [u16; 224] = [
    250, 389, 555, 500, 500, 833, 778, 278, 333, 333, 500, 570, 250, 333, 250, 278, 500, 500, 500,
    500, 500, 500, 500, 500, 500, 500, 333, 333, 570, 570, 570, 500, 832, 667, 667, 667, 722, 667,
    667, 722, 778, 389, 500, 667, 611, 889, 722, 722, 611, 722, 667, 556, 611, 722, 667, 889, 667,
    611, 611, 333, 278, 333, 570, 500, 333, 500, 500, 444, 500, 444, 333, 500, 556, 278, 278, 500,
    278, 778, 556, 500, 500, 500, 389, 389, 278, 556, 444, 667, 500, 444, 389, 348, 220, 348, 570,
    500, 744, 500, 333, 500, 500, 1000, 500, 500, 333, 1000, 556, 333, 944, 500, 611, 500, 500,
    333, 333, 500, 500, 350, 500, 1000, 333, 1000, 389, 333, 722, 500, 389, 611, 250, 389, 500,
    500, 500, 500, 220, 500, 333, 747, 266, 500, 606, 333, 747, 333, 400, 549, 300, 300, 333, 576,
    500, 250, 333, 300, 300, 500, 750, 750, 750, 500, 667, 667, 667, 667, 667, 667, 944, 667, 667,
    667, 667, 667, 389, 389, 389, 389, 722, 722, 722, 722, 722, 722, 722, 570, 722, 722, 722, 722,
    722, 611, 611, 500, 500, 500, 500, 500, 500, 500, 722, 444, 444, 444, 444, 485, 278, 278, 278,
    395, 500, 556, 500, 500, 500, 500, 500, 549, 500, 556, 556, 556, 556, 444, 500, 444,
];

fn is_mono(font: FontId) -> bool {
    matches!(font, FontId::Mono | FontId::MonoBold)
}

impl FontMetrics for Afm {
    fn char_width(&self, font: FontId, c: char) -> f64 {
        if is_mono(font) {
            return 600.0;
        }
        let code = winansi_code(c).unwrap_or(b'?') as usize;
        let table = match font {
            FontId::Regular => &TIMES,
            FontId::Bold => &TIMES_BOLD,
            FontId::Italic => &TIMES_ITALIC,
            FontId::BoldItalic => &TIMES_BOLD_ITALIC,
            _ => &TIMES,
        };
        table[code - 32] as f64
    }

    fn ascent(&self, font: FontId) -> f64 {
        // Serif faces use 645 — the effective first-baseline ascent that
        // matches typst/Libertinus output, not Times' nominal 750.
        if is_mono(font) {
            629.0
        } else {
            645.0
        }
    }

    fn descent(&self, font: FontId) -> f64 {
        if is_mono(font) {
            157.0
        } else {
            250.0
        }
    }
}

/// Base-14 tables plus one `FaceMetrics` per `Document::fonts` entry.
/// Faces that fail to load keep a `None` slot and fall back to Times
/// metrics so an `Ext` id can never panic.
pub struct DbMetrics {
    base: Afm,
    ext: Vec<Option<FaceMetrics>>,
}

impl DbMetrics {
    pub fn new(doc_fonts: &[FontUse]) -> Self {
        DbMetrics {
            base: Afm,
            ext: doc_fonts
                .iter()
                .map(|f| fontdb::load_metrics(&f.path, f.ttc_index))
                .collect(),
        }
    }
}

impl FontMetrics for DbMetrics {
    fn char_width(&self, font: FontId, c: char) -> f64 {
        match font {
            FontId::Ext(i) => match self.ext.get(i as usize) {
                Some(Some(m)) => m.char_width(c),
                _ => self.base.char_width(FontId::Regular, c),
            },
            _ => self.base.char_width(font, c),
        }
    }

    fn ascent(&self, font: FontId) -> f64 {
        match font {
            FontId::Ext(i) => match self.ext.get(i as usize) {
                Some(Some(m)) => m.ascent(),
                _ => self.base.ascent(FontId::Regular),
            },
            _ => self.base.ascent(font),
        }
    }

    fn descent(&self, font: FontId) -> f64 {
        match font {
            FontId::Ext(i) => match self.ext.get(i as usize) {
                Some(Some(m)) => m.descent(),
                _ => self.base.descent(FontId::Regular),
            },
            _ => self.base.descent(font),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn times_widths() {
        let m = Afm::new();
        assert_eq!(m.char_width(FontId::Regular, ' '), 250.0);
        assert_eq!(m.char_width(FontId::Regular, 'i'), 278.0);
        assert_eq!(m.char_width(FontId::Regular, 'W'), 944.0);
        assert_eq!(m.char_width(FontId::Regular, '—'), 1000.0);
        assert_eq!(m.char_width(FontId::Regular, '•'), 350.0);
    }

    #[test]
    fn bold_and_mono_widths() {
        let m = Afm::new();
        assert_eq!(m.char_width(FontId::Bold, 'i'), 278.0);
        assert_eq!(m.char_width(FontId::Mono, 'm'), 600.0);
        assert_eq!(m.char_width(FontId::MonoBold, ' '), 600.0);
        assert_eq!(m.char_width(FontId::Mono, '文'), 600.0);
    }

    #[test]
    fn unmappable_uses_question_width() {
        let m = Afm::new();
        assert_eq!(
            m.char_width(FontId::Regular, '文'),
            m.char_width(FontId::Regular, '?')
        );
        assert_eq!(
            m.char_width(FontId::Bold, '\u{1f600}'),
            m.char_width(FontId::Bold, '?')
        );
    }

    #[test]
    fn winansi_edge_cases() {
        assert_eq!(winansi_code(' '), Some(0x20));
        assert_eq!(winansi_code('~'), Some(0x7e));
        assert_eq!(winansi_code('é'), Some(0xe9));
        assert_eq!(winansi_code('ÿ'), Some(0xff));
        assert_eq!(winansi_code('\u{a0}'), Some(0xa0));
        assert_eq!(winansi_code('€'), Some(0x80));
        assert_eq!(winansi_code('•'), Some(0x95));
        assert_eq!(winansi_code('—'), Some(0x97));
        assert_eq!(winansi_code('\u{7f}'), None);
        assert_eq!(winansi_code('文'), None);
    }

    #[test]
    fn names_and_vertical_metrics() {
        assert_eq!(base14_name(FontId::Regular), "Times-Roman");
        assert_eq!(base14_name(FontId::Bold), "Times-Bold");
        assert_eq!(base14_name(FontId::Italic), "Times-Italic");
        assert_eq!(base14_name(FontId::BoldItalic), "Times-BoldItalic");
        assert_eq!(base14_name(FontId::Mono), "Courier");
        assert_eq!(base14_name(FontId::MonoBold), "Courier-Bold");
        let m = Afm::new();
        assert_eq!(m.ascent(FontId::Regular), 645.0);
        assert_eq!(m.descent(FontId::Regular), 250.0);
        assert_eq!(m.ascent(FontId::MonoBold), 629.0);
        assert_eq!(m.descent(FontId::Mono), 157.0);
        assert_eq!(m.str_width(FontId::Mono, 10.0, "abc"), 18.0);
    }
}
