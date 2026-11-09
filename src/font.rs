//! Font metrics: static base-14 AFM widths plus dynamically loaded
//! `FontId::Ext` faces via `fontdb`. Zero I/O at startup for `Afm`.
//! Serif faces map to Times to visually match typst's Libertinus default.
use crate::fontdb::{self, FaceMetrics};
use crate::ir::{FontId, FontMetrics, FontUse};
use std::cell::RefCell;
use std::collections::HashMap;

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

/// Map a Unicode scalar to its WinAnsiEncoding byte, if representable.
/// Const so the inverse table is built at compile time (Adobe AFM: width
/// and encoding are a 256-slot array, not a search).
pub(crate) const fn winansi_u32(c: u32) -> Option<u8> {
    match c {
        0x20..=0x7e | 0xa0..=0xff => Some(c as u8),
        0x20ac => Some(0x80),
        0x201a => Some(0x82),
        0x192 => Some(0x83),
        0x201e => Some(0x84),
        0x2026 => Some(0x85),
        0x2020 => Some(0x86),
        0x2021 => Some(0x87),
        0x2c6 => Some(0x88),
        0x2030 => Some(0x89),
        0x160 => Some(0x8a),
        0x2039 => Some(0x8b),
        0x152 => Some(0x8c),
        0x17d => Some(0x8e),
        0x2018 => Some(0x91),
        0x2019 => Some(0x92),
        0x201c => Some(0x93),
        0x201d => Some(0x94),
        0x2022 => Some(0x95),
        0x2013 => Some(0x96),
        0x2014 => Some(0x97),
        0x2dc => Some(0x98),
        0x2122 => Some(0x99),
        0x161 => Some(0x9a),
        0x203a => Some(0x9b),
        0x153 => Some(0x9c),
        0x17e => Some(0x9e),
        0x178 => Some(0x9f),
        _ => None,
    }
}

/// Map a Unicode char to its WinAnsiEncoding byte, if representable.
pub(crate) fn winansi_code(c: char) -> Option<u8> {
    winansi_u32(c as u32)
}

const fn winansi_inverse_const() -> [u32; 256] {
    let mut t = [0u32; 256];
    let mut u = 1u32;
    while u < 0x2200 {
        if let Some(b) = winansi_u32(u) {
            t[b as usize] = u;
        }
        u += 1;
    }
    t
}

/// Inverse of `winansi_code`: WinAnsi byte -> Unicode scalar (0 = unmapped).
pub(crate) const WINANSI_INV: [u32; 256] = winansi_inverse_const();

/// Inverse of `winansi_code`. Compile-time table; the copy is ~1 KB.
pub(crate) fn winansi_inverse() -> [u32; 256] {
    WINANSI_INV
}

// Times widths in 1/1000 em (extracted from the system Times faces, which
// are metric-compatible with the Adobe base-14 AFMs), indexed by WinAnsi
// code minus 32. Undefined slots carry the '?' width; they are unreachable
// because winansi_code() never produces them.
const TIMES: [u16; 224] = [
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

const TIMES_BOLD: [u16; 224] = [
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

const TIMES_ITALIC: [u16; 224] = [
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

const TIMES_BOLD_ITALIC: [u16; 224] = [
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

/// Byte-indexed widths for `0..128`. Controls and DEL use the '?' width.
/// A direct load is the AFM model (`cwi[256]`) and stays in L1.
const fn ascii_lut(table: &[u16; 224]) -> [u16; 128] {
    let mut a = [0u16; 128];
    let q = table[b'?' as usize - 32];
    let mut i = 0;
    while i < 128 {
        a[i] = if i >= 32 && i < 127 {
            table[i - 32]
        } else {
            q
        };
        i += 1;
    }
    a
}

const TIMES_ASCII: [u16; 128] = ascii_lut(&TIMES);
const TIMES_BOLD_ASCII: [u16; 128] = ascii_lut(&TIMES_BOLD);
const TIMES_ITALIC_ASCII: [u16; 128] = ascii_lut(&TIMES_ITALIC);
const TIMES_BI_ASCII: [u16; 128] = ascii_lut(&TIMES_BOLD_ITALIC);

fn is_mono(font: FontId) -> bool {
    matches!(font, FontId::Mono | FontId::MonoBold)
}

fn serif_table(font: FontId) -> &'static [u16; 224] {
    match font {
        FontId::Bold => &TIMES_BOLD,
        FontId::Italic => &TIMES_ITALIC,
        FontId::BoldItalic => &TIMES_BOLD_ITALIC,
        _ => &TIMES,
    }
}

fn serif_ascii(font: FontId) -> &'static [u16; 128] {
    match font {
        FontId::Bold => &TIMES_BOLD_ASCII,
        FontId::Italic => &TIMES_ITALIC_ASCII,
        FontId::BoldItalic => &TIMES_BI_ASCII,
        _ => &TIMES_ASCII,
    }
}

/// Sum of AFM units (1/1000 em) for `s` in a serif face.
/// One predictable `b < 128` branch per byte; the Latin text in a document
/// never takes it. `str::is_ascii` is not used: its prologue loses on the
/// short words this loop sees thousands of times.
#[inline(always)]
fn serif_units(font: FontId, s: &str) -> u32 {
    let lut = serif_ascii(font);
    let bytes = s.as_bytes();
    let mut sum = 0u32;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 128 {
            sum += lut[b as usize] as u32;
            i += 1;
        } else {
            let table = serif_table(font);
            let ch = s[i..].chars().next().unwrap();
            let code = winansi_code(ch).unwrap_or(b'?');
            sum += table[code as usize - 32] as u32;
            i += ch.len_utf8();
        }
    }
    sum
}

impl FontMetrics for Afm {
    #[inline(always)]
    fn char_width(&self, font: FontId, c: char) -> f64 {
        if is_mono(font) {
            return 600.0;
        }
        let u = c as u32;
        if u < 128 {
            return serif_ascii(font)[u as usize] as f64;
        }
        let code = winansi_code(c).unwrap_or(b'?') as usize;
        serif_table(font)[code - 32] as f64
    }

    #[inline(always)]
    fn ascii_units(&self, font: FontId) -> Option<&'static [u16; 128]> {
        if is_mono(font) {
            None
        } else {
            Some(serif_ascii(font))
        }
    }

    #[inline(always)]
    fn str_width(&self, font: FontId, size: f64, s: &str) -> f64 {
        let scale = size / 1000.0;
        if is_mono(font) {
            let n = if s.is_ascii() {
                s.len()
            } else {
                s.chars().count()
            };
            return n as f64 * 600.0 * scale;
        }
        serif_units(font, s) as f64 * scale
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

/// Word widths for one `FontId::Ext` face, after hmtx and kern.
/// Keyed by (ext index, size in 1/100 pt, word). Never used for AFM.
struct WordCache {
    map: HashMap<(u16, i32, String), f64>,
    /// Test-only counters. Layout never reads them.
    #[allow(dead_code)]
    hits: u64,
    #[allow(dead_code)]
    misses: u64,
}

/// Base-14 tables plus one `FaceMetrics` per `Document::fonts` entry.
/// Faces that fail to load keep a `None` slot and fall back to Times
/// metrics so an `Ext` id can never panic. Repeated Ext words are
/// cached here; the base-14 path is not.
pub struct DbMetrics {
    base: Afm,
    ext: Vec<Option<FaceMetrics>>,
    words: RefCell<WordCache>,
}

impl DbMetrics {
    pub fn new(doc_fonts: &[FontUse]) -> Self {
        DbMetrics {
            base: Afm,
            ext: doc_fonts
                .iter()
                .map(|f| fontdb::load_metrics(&f.path, f.ttc_index))
                .collect(),
            words: RefCell::new(WordCache {
                map: HashMap::new(),
                hits: 0,
                misses: 0,
            }),
        }
    }

    /// Ext word width, measured once per (face, size, word). `size` is
    /// quantized to 1/100 pt. The stored width already includes kerning.
    fn cached_ext(&self, id: u16, face: &FaceMetrics, size: f64, s: &str) -> f64 {
        let cents = (size * 100.0).round() as i32;
        let mut cache = self.words.borrow_mut();
        let key = (id, cents, s.to_string());
        if let Some(&w) = cache.map.get(&key) {
            cache.hits += 1;
            return w;
        }
        let w = face.str_width(size, s);
        cache.misses += 1;
        cache.map.insert(key, w);
        w
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

    fn ascii_units(&self, font: FontId) -> Option<&'static [u16; 128]> {
        match font {
            FontId::Ext(_) => None,
            _ => self.base.ascii_units(font),
        }
    }

    fn str_width(&self, font: FontId, size: f64, s: &str) -> f64 {
        match font {
            FontId::Ext(i) => match self.ext.get(i as usize) {
                Some(Some(m)) => self.cached_ext(i, m, size, s),
                _ => self.base.str_width(FontId::Regular, size, s),
            },
            _ => self.base.str_width(font, size, s),
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

    #[test]
    fn regular_str_width_matches_afm_units() {
        let m = Afm::new();
        let word = "Typography";
        // AFM units are integers. `str_width` sums them, then scales once.
        let units: u32 = word
            .chars()
            .map(|c| m.char_width(FontId::Regular, c) as u32)
            .sum();
        assert_eq!(
            m.str_width(FontId::Regular, 11.0, word),
            units as f64 * (11.0 / 1000.0)
        );
    }

    #[test]
    fn ext_word_width_hits_cache() {
        let dir = std::env::temp_dir().join(format!("typrst-extw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("k.ttf");
        let mut tables =
            fontdb::synth_tables("Cache Face", "Regular", "CacheFace", 400, false, false);
        tables.push((b"kern", fontdb::synth_kern(&[(1, 1, -100)])));
        std::fs::write(&p, fontdb::synth_ttf(&tables)).unwrap();
        let m = DbMetrics::new(&[FontUse {
            postscript_name: "CacheFace".into(),
            family: "Cache Face".into(),
            path: p,
            ttc_index: 0,
        }]);
        // 600+600-100 font units, upm 1000, 10 pt -> 11.
        let w1 = m.str_width(FontId::Ext(0), 10.0, "AA");
        assert_eq!(w1, 11.0);
        assert_eq!(m.words.borrow().misses, 1);
        assert_eq!(m.words.borrow().hits, 0);
        assert_eq!(m.words.borrow().map.len(), 1);
        let w2 = m.str_width(FontId::Ext(0), 10.0, "AA");
        assert_eq!(w1, w2);
        assert_eq!(m.words.borrow().hits, 1);
        assert_eq!(m.words.borrow().misses, 1);
        assert_eq!(m.words.borrow().map.len(), 1);

        let word = "Typography";
        let afm = Afm::new();
        let units: u32 = word
            .chars()
            .map(|c| afm.char_width(FontId::Regular, c) as u32)
            .sum();
        assert_eq!(
            m.str_width(FontId::Regular, 11.0, word),
            afm.str_width(FontId::Regular, 11.0, word)
        );
        assert_eq!(
            afm.str_width(FontId::Regular, 11.0, word),
            units as f64 * (11.0 / 1000.0)
        );
        // The AFM face does not enter the Ext cache.
        assert_eq!(m.words.borrow().map.len(), 1);
        assert_eq!(m.words.borrow().hits, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
