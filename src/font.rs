//! Base-14 font metrics (AFM widths, 1/1000 em). Zero I/O at startup.
use crate::ir::{FontId, FontMetrics};

pub struct Afm;

impl Afm {
    pub fn new() -> Self {
        Afm
    }
}

/// PDF BaseFont name for a base-14 face.
pub fn base14_name(font: FontId) -> &'static str {
    match font {
        FontId::Regular => "Helvetica",
        FontId::Bold => "Helvetica-Bold",
        FontId::Italic => "Helvetica-Oblique",
        FontId::BoldItalic => "Helvetica-BoldOblique",
        FontId::Mono => "Courier",
        FontId::MonoBold => "Courier-Bold",
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

// Adobe AFM widths, indexed by WinAnsi code minus 32 (codes 32..=255).
// Undefined slots (127, 129, 141, 143, 144, 157) carry a 556 placeholder;
// they are unreachable because winansi_code() never produces them.
static HELVETICA: [u16; 224] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, 389, 584, 278, 333, 278, 278, // 32
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, 556, // 48
    1015, 667, 667, 722, 722, 667, 611, 778, 722, 278, 500, 667, 556, 833, 722, 778, // 64
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 278, 278, 278, 469, 556, // 80
    333, 556, 556, 500, 556, 556, 278, 556, 556, 222, 222, 500, 222, 833, 556, 556, // 96
    556, 556, 333, 500, 278, 556, 500, 722, 500, 500, 500, 334, 260, 334, 584, 556, // 112
    556, 556, 222, 556, 333, 1000, 556, 556, 333, 1000, 667, 333, 1000, 556, 611, 556, // 128
    556, 222, 222, 333, 333, 350, 556, 1000, 333, 1000, 500, 333, 944, 556, 500, 667, // 144
    278, 333, 556, 556, 556, 556, 260, 556, 333, 737, 370, 556, 584, 333, 737, 333, // 160
    400, 584, 333, 333, 333, 556, 537, 278, 333, 333, 365, 556, 834, 834, 834, 611, // 176
    667, 667, 667, 667, 667, 667, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, // 192
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, // 208
    556, 556, 556, 556, 556, 556, 889, 500, 556, 556, 556, 556, 278, 278, 278, 278, // 224
    556, 556, 556, 556, 556, 556, 556, 584, 611, 556, 556, 556, 556, 500, 556, 500, // 240
];

static HELVETICA_BOLD: [u16; 224] = [
    278, 333, 474, 556, 556, 889, 722, 238, 333, 333, 389, 584, 278, 333, 278, 278, // 32
    556, 556, 556, 556, 556, 556, 556, 556, 556, 556, 333, 333, 584, 584, 584, 611, // 48
    975, 722, 722, 722, 722, 667, 611, 778, 722, 278, 556, 722, 611, 833, 722, 778, // 64
    667, 778, 722, 667, 611, 722, 667, 944, 667, 667, 611, 333, 278, 333, 584, 556, // 80
    333, 556, 611, 556, 611, 556, 333, 611, 611, 278, 278, 556, 278, 889, 611, 611, // 96
    611, 611, 389, 556, 333, 611, 556, 778, 556, 556, 500, 389, 280, 389, 584, 556, // 112
    556, 556, 278, 556, 500, 1000, 556, 556, 333, 1000, 667, 333, 1000, 556, 611, 556, // 128
    556, 278, 278, 333, 333, 350, 556, 1000, 333, 1000, 556, 333, 944, 556, 500, 667, // 144
    278, 333, 556, 556, 556, 556, 280, 556, 333, 737, 370, 556, 584, 333, 737, 333, // 160
    400, 584, 333, 333, 333, 611, 556, 278, 333, 333, 365, 556, 834, 834, 834, 611, // 176
    722, 722, 722, 722, 722, 722, 1000, 722, 667, 667, 667, 667, 278, 278, 278, 278, // 192
    722, 722, 778, 778, 778, 778, 778, 584, 778, 722, 722, 722, 722, 667, 667, 611, // 208
    556, 556, 556, 556, 556, 556, 889, 556, 556, 556, 556, 556, 278, 278, 278, 278, // 224
    611, 611, 611, 611, 611, 611, 611, 584, 611, 611, 611, 611, 611, 556, 611, 556, // 240
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
            FontId::Bold | FontId::BoldItalic => &HELVETICA_BOLD,
            _ => &HELVETICA,
        };
        table[code - 32] as f64
    }

    fn ascent(&self, font: FontId) -> f64 {
        if is_mono(font) { 629.0 } else { 718.0 }
    }

    fn descent(&self, font: FontId) -> f64 {
        if is_mono(font) { 157.0 } else { 207.0 }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helvetica_widths() {
        let m = Afm::new();
        assert_eq!(m.char_width(FontId::Regular, ' '), 278.0);
        assert_eq!(m.char_width(FontId::Regular, 'i'), 222.0);
        assert_eq!(m.char_width(FontId::Regular, 'W'), 944.0);
        assert_eq!(m.char_width(FontId::Italic, 'i'), 222.0);
        assert_eq!(m.char_width(FontId::Regular, 'é'), 556.0);
        assert_eq!(m.char_width(FontId::Regular, '—'), 1000.0);
        assert_eq!(m.char_width(FontId::Regular, '€'), 556.0);
    }

    #[test]
    fn bold_and_mono_widths() {
        let m = Afm::new();
        assert_eq!(m.char_width(FontId::Bold, 'i'), 278.0);
        assert_eq!(m.char_width(FontId::BoldItalic, 'b'), 611.0);
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
        assert_eq!(winansi_code('‚'), Some(0x82));
        assert_eq!(winansi_code('ƒ'), Some(0x83));
        assert_eq!(winansi_code('…'), Some(0x85));
        assert_eq!(winansi_code('Œ'), Some(0x8c));
        assert_eq!(winansi_code('Ž'), Some(0x8e));
        assert_eq!(winansi_code('‘'), Some(0x91));
        assert_eq!(winansi_code('’'), Some(0x92));
        assert_eq!(winansi_code('“'), Some(0x93));
        assert_eq!(winansi_code('”'), Some(0x94));
        assert_eq!(winansi_code('•'), Some(0x95));
        assert_eq!(winansi_code('–'), Some(0x96));
        assert_eq!(winansi_code('—'), Some(0x97));
        assert_eq!(winansi_code('™'), Some(0x99));
        assert_eq!(winansi_code('œ'), Some(0x9c));
        assert_eq!(winansi_code('Ÿ'), Some(0x9f));
        assert_eq!(winansi_code('\u{7f}'), None);
        assert_eq!(winansi_code('\n'), None);
        assert_eq!(winansi_code('文'), None);
    }

    #[test]
    fn names_and_vertical_metrics() {
        assert_eq!(base14_name(FontId::Regular), "Helvetica");
        assert_eq!(base14_name(FontId::Bold), "Helvetica-Bold");
        assert_eq!(base14_name(FontId::Italic), "Helvetica-Oblique");
        assert_eq!(base14_name(FontId::BoldItalic), "Helvetica-BoldOblique");
        assert_eq!(base14_name(FontId::Mono), "Courier");
        assert_eq!(base14_name(FontId::MonoBold), "Courier-Bold");
        let m = Afm::new();
        assert_eq!(m.ascent(FontId::Regular), 718.0);
        assert_eq!(m.descent(FontId::Regular), 207.0);
        assert_eq!(m.ascent(FontId::MonoBold), 629.0);
        assert_eq!(m.descent(FontId::Mono), 157.0);
        assert_eq!(m.str_width(FontId::Mono, 10.0, "abc"), 18.0);
    }
}
