//! Minimal PDF writer: display list -> PDF bytes. Base-14 faces are
//! referenced by name; `FontId::Ext` faces are embedded as simple
//! TrueType fonts (WinAnsiEncoding, `/FontFile2`), with `.ttc` members
//! repacked into standalone sfnts. Faces that fail to load or extract
//! (including CFF/`OTTO` outlines) degrade to a Times-Roman reference.
use crate::font::{base14_name, winansi_code, winansi_inverse};
use crate::fontdb::{self, u16be, u32be, FaceMetrics};
use crate::ir::{DrawItem, FontId, FontUse, Page};
use std::io::Write;
use std::path::Path;

fn push_uint(out: &mut Vec<u8>, mut n: u64) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (n % 10) as u8;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    out.extend_from_slice(&buf[i..]);
}

fn push_int(out: &mut Vec<u8>, v: i64) {
    if v < 0 {
        out.push(b'-');
    }
    push_uint(out, v.unsigned_abs());
}

/// Format a number: <=2 decimals, no trailing zeros. Integer math only —
/// this is the hottest function in the writer.
fn push_num(out: &mut Vec<u8>, v: f64) {
    let scaled = (v * 100.0).round() as i64;
    if scaled == 0 {
        out.push(b'0');
        return;
    }
    let mag = scaled.unsigned_abs();
    if scaled < 0 {
        out.push(b'-');
    }
    let (int, frac) = (mag / 100, mag % 100);
    push_uint(out, int);
    if frac != 0 {
        out.push(b'.');
        out.push(b'0' + (frac / 10) as u8);
        if frac % 10 != 0 {
            out.push(b'0' + (frac % 10) as u8);
        }
    }
}

/// WinAnsi-encode `s` as an escaped PDF literal string, `(…)` included.
fn push_lit(out: &mut Vec<u8>, s: &str) {
    out.push(b'(');
    for c in s.chars() {
        let b = winansi_code(c).unwrap_or(b'?');
        if matches!(b, b'(' | b')' | b'\\') {
            out.extend_from_slice(&[b'\\', b]);
        } else if (0x20..=0x7e).contains(&b) {
            out.push(b);
        } else {
            write!(out, "\\{b:03o}").unwrap();
        }
    }
    out.push(b')');
}

/// PostScript names must be bare PDF names: keep a safe alphabet.
fn ps_name(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        o.push(
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '+') {
                c
            } else {
                '-'
            },
        );
    }
    if o.is_empty() {
        o.push('F');
    }
    o
}

/// sfnt table checksum: big-endian u32 sum, zero-padded to a multiple of 4.
fn checksum(d: &[u8]) -> u32 {
    let mut s = 0u32;
    for c in d.chunks(4) {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        s = s.wrapping_add(u32::from_be_bytes(b));
    }
    s
}

/// Repack face `idx` of a 'ttcf' collection as a standalone sfnt:
/// fresh offset table, per-table checksums, and a fixed
/// head.checkSumAdjustment so the file sums to 0xB1B0AFBA.
fn extract_ttc(d: &[u8], idx: u32) -> Option<Vec<u8>> {
    let base = fontdb::face_base(d, idx)?;
    let n = u16be(d, base + 4)? as usize;
    if n == 0 {
        return None;
    }
    let version = u32be(d, base)?;
    let mut tabs: Vec<([u8; 4], usize, usize)> = Vec::with_capacity(n);
    for i in 0..n {
        let r = base + 12 + 16 * i;
        let tag: [u8; 4] = d.get(r..r + 4)?.try_into().ok()?;
        let (off, len) = (u32be(d, r + 8)? as usize, u32be(d, r + 12)? as usize);
        d.get(off..off + len)?; // bounds-check the table data
        tabs.push((tag, off, len));
    }
    tabs.sort_by_key(|(t, _, _)| *t);

    let mut esel = 0u16;
    while 1usize << (esel + 1) <= n {
        esel += 1;
    }
    let search = 16u16 << esel;
    let mut out = Vec::with_capacity(12 + 16 * n + tabs.iter().map(|t| t.2 + 3).sum::<usize>());
    out.extend(version.to_be_bytes());
    out.extend((n as u16).to_be_bytes());
    out.extend(search.to_be_bytes());
    out.extend(esel.to_be_bytes());
    out.extend((16 * n as u16 - search).to_be_bytes());
    let dir = out.len();
    out.resize(dir + 16 * n, 0);

    let mut offs = Vec::with_capacity(n);
    let mut head_pos = None;
    for (tag, off, len) in &tabs {
        while out.len() % 4 != 0 {
            out.push(0);
        }
        let no = out.len();
        out.extend(&d[*off..*off + *len]);
        if *tag == *b"head" && *len >= 12 {
            out[no + 8..no + 12].fill(0); // checkSumAdjustment
            head_pos = Some(no);
        }
        offs.push(no);
    }
    while out.len() % 4 != 0 {
        out.push(0);
    }
    for (i, (tag, _, len)) in tabs.iter().enumerate() {
        let chk = checksum(&out[offs[i]..offs[i] + len]);
        let r = dir + 16 * i;
        out[r..r + 4].copy_from_slice(&tag[..]);
        out[r + 4..r + 8].copy_from_slice(&chk.to_be_bytes());
        out[r + 8..r + 12].copy_from_slice(&(offs[i] as u32).to_be_bytes());
        out[r + 12..r + 16].copy_from_slice(&(*len as u32).to_be_bytes());
    }
    if let Some(hp) = head_pos {
        let adj = 0xB1B0AFBAu32.wrapping_sub(checksum(&out));
        out[hp + 8..hp + 12].copy_from_slice(&adj.to_be_bytes());
    }
    Some(out)
}

/// Extract one face of a `.ttc` as standalone sfnt bytes.
#[allow(dead_code)] // used when embedding TTC faces; exercised by tests
pub fn extract_ttc_face(path: &Path, idx: u32) -> Option<Vec<u8>> {
    extract_ttc(&std::fs::read(path).ok()?, idx)
}

/// Raw embeddable font bytes for a `FontUse` (repacks ttc members).
/// CFF/'OTTO' faces are rejected — they need /FontFile3 we don't emit.
fn font_data(fu: &FontUse) -> Option<Vec<u8>> {
    let d = std::fs::read(&fu.path).ok()?;
    let out = if d.get(0..4) == Some(b"ttcf") {
        extract_ttc(&d, fu.ttc_index)?
    } else if fu.ttc_index == 0 {
        d
    } else {
        return None;
    };
    if out.get(0..4) == Some(b"OTTO") {
        return None;
    }
    Some(out)
}

/// How each used face reaches the PDF.
enum Plan {
    /// Base-14 reference — also the graceful fallback for failed embeds.
    Base(&'static str),
    /// Embedded TrueType face.
    Ext {
        name: String,
        data: Vec<u8>,
        m: FaceMetrics,
    },
}

fn plan_for(font: FontId, fonts: &[FontUse]) -> Plan {
    let FontId::Ext(i) = font else {
        return Plan::Base(base14_name(font));
    };
    let Some(fu) = fonts.get(i as usize) else {
        return Plan::Base("Times-Roman");
    };
    let (Some(data), Some(m)) = (font_data(fu), fontdb::load_metrics(&fu.path, fu.ttc_index))
    else {
        return Plan::Base("Times-Roman");
    };
    Plan::Ext {
        name: ps_name(&fu.postscript_name),
        data,
        m,
    }
}

/// All faces referenced by the display list: base-14 first (in
/// `FontId::ALL` order), then Ext indices ascending.
fn used_fonts(pages: &[Page]) -> Vec<FontId> {
    let mut base = [false; 6];
    let mut exts: Vec<u16> = Vec::new();
    for p in pages {
        for i in &p.items {
            if let DrawItem::Text { font, .. } = i {
                match font {
                    FontId::Ext(e) => exts.push(*e),
                    f => {
                        if let Some(k) = FontId::ALL.iter().position(|x| x == f) {
                            base[k] = true;
                        }
                    }
                }
            }
        }
    }
    exts.sort_unstable();
    exts.dedup();
    FontId::ALL
        .iter()
        .zip(base)
        .filter_map(|(&f, u)| u.then_some(f))
        .chain(exts.into_iter().map(FontId::Ext))
        .collect()
}

fn fnum(used: &[FontId], f: FontId) -> u64 {
    used.iter()
        .position(|x| *x == f)
        .map(|i| i as u64 + 1)
        .unwrap_or(1)
}

pub fn write_pdf(pages: &[Page], fonts: &[FontUse]) -> Vec<u8> {
    let used = used_fonts(pages);
    let plans: Vec<Plan> = used.iter().map(|f| plan_for(*f, fonts)).collect();

    // Object ids: base faces take 1 object; embedded faces take 3
    // (font dict, descriptor, font file stream).
    let font_base = 3 + 2 * pages.len();
    let mut obj = font_base;
    let mut ids = Vec::with_capacity(plans.len());
    for p in &plans {
        ids.push((obj, obj + 1, obj + 2));
        obj += if matches!(p, Plan::Ext { .. }) { 3 } else { 1 };
    }
    let nobj = obj - 1;

    let mut out = Vec::with_capacity(512 + pages.iter().map(|p| p.items.len() * 96).sum::<usize>());
    let mut offsets = vec![0usize; nobj + 1];
    let mut begin = |out: &mut Vec<u8>, n: usize| {
        offsets[n] = out.len();
        writeln!(out, "{n} 0 obj").unwrap();
    };

    out.extend_from_slice(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n");

    begin(&mut out, 1);
    out.extend_from_slice(b"<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

    begin(&mut out, 2);
    write!(out, "<< /Type /Pages /Count {} /Kids [", pages.len()).unwrap();
    for i in 0..pages.len() {
        write!(out, "{} 0 R ", 3 + 2 * i).unwrap();
    }
    out.extend_from_slice(b"] >>\nendobj\n");

    // Shared resource dict fragment.
    let mut res = Vec::with_capacity(64 + 16 * used.len());
    res.extend_from_slice(b"/Resources << ");
    if !used.is_empty() {
        res.extend_from_slice(b"/Font << ");
        for (j, (fo, _, _)) in ids.iter().enumerate() {
            write!(res, "/F{} {} 0 R ", j + 1, fo).unwrap();
        }
        res.extend_from_slice(b">> ");
    }
    res.extend_from_slice(b">>");

    for (i, page) in pages.iter().enumerate() {
        let mut content = Vec::with_capacity(page.items.len() * 64 + 16);
        for item in &page.items {
            match item {
                DrawItem::Text {
                    x,
                    y,
                    font,
                    size,
                    text,
                } => {
                    content.extend_from_slice(b"BT /F");
                    push_uint(&mut content, fnum(&used, *font));
                    content.push(b' ');
                    push_num(&mut content, *size);
                    content.extend_from_slice(b" Tf 1 0 0 1 ");
                    push_num(&mut content, *x);
                    content.push(b' ');
                    push_num(&mut content, page.height - y);
                    content.extend_from_slice(b" Tm ");
                    push_lit(&mut content, text);
                    content.extend_from_slice(b" Tj ET\n");
                }
                DrawItem::Rule { x, y, w, h, gray } => {
                    push_num(&mut content, *gray);
                    content.extend_from_slice(b" g ");
                    push_num(&mut content, *x);
                    content.push(b' ');
                    push_num(&mut content, page.height - y - h);
                    content.push(b' ');
                    push_num(&mut content, *w);
                    content.push(b' ');
                    push_num(&mut content, *h);
                    content.extend_from_slice(b" re f\n");
                }
            }
        }
        begin(&mut out, 3 + 2 * i);
        out.extend_from_slice(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 ");
        push_num(&mut out, page.width);
        out.push(b' ');
        push_num(&mut out, page.height);
        out.push(b']');
        out.extend_from_slice(&res);
        writeln!(out, " /Contents {} 0 R >>", 4 + 2 * i).unwrap();
        out.extend_from_slice(b"endobj\n");

        begin(&mut out, 4 + 2 * i);
        writeln!(out, "<< /Length {} >>", content.len()).unwrap();
        out.extend_from_slice(b"stream\n");
        out.extend_from_slice(&content);
        out.extend_from_slice(b"endstream\nendobj\n");
    }

    let winansi = winansi_inverse();
    for (j, p) in plans.iter().enumerate() {
        let (fo, d, s) = ids[j];
        begin(&mut out, fo);
        match p {
            Plan::Base(name) => {
                writeln!(
                    out,
                    "<< /Type /Font /Subtype /Type1 /BaseFont /{name} /Encoding /WinAnsiEncoding >>\nendobj"
                )
                .unwrap();
            }
            Plan::Ext { name, data, m } => {
                write!(
                    out,
                    "<< /Type /Font /Subtype /TrueType /BaseFont /{name} /Encoding /WinAnsiEncoding /FirstChar 32 /LastChar 255 /Widths ["
                )
                .unwrap();
                for code in 32..=255usize {
                    let c = char::from_u32(winansi[code]).unwrap_or('?');
                    push_uint(&mut out, m.char_width(c).round().max(0.0) as u64);
                    out.push(b' ');
                }
                writeln!(out, "] /FontDescriptor {d} 0 R >>\nendobj").unwrap();

                begin(&mut out, d);
                let flags = 32 + if m.italic_angle != 0 { 64 } else { 0 };
                write!(
                    out,
                    "<< /Type /FontDescriptor /FontName /{name} /Flags {flags} /FontBBox ["
                )
                .unwrap();
                for v in m.bbox_1000() {
                    push_int(&mut out, v);
                    out.push(b' ');
                }
                out.extend_from_slice(b"] /ItalicAngle ");
                push_num(&mut out, m.italic_angle as f64 / 65536.0);
                out.extend_from_slice(b" /Ascent ");
                push_num(&mut out, m.ascent());
                out.extend_from_slice(b" /Descent -");
                push_num(&mut out, m.descent());
                out.extend_from_slice(b" /CapHeight ");
                push_num(&mut out, m.ascent() * 0.7);
                writeln!(out, " /StemV 80 /FontFile2 {s} 0 R >>\nendobj").unwrap();

                begin(&mut out, s);
                writeln!(out, "<< /Length {} /Length1 {} >>", data.len(), data.len()).unwrap();
                out.extend_from_slice(b"stream\n");
                out.extend_from_slice(data);
                out.extend_from_slice(b"\nendstream\nendobj\n");
            }
        }
    }

    let xref_pos = out.len();
    writeln!(out, "xref\n0 {}", nobj + 1).unwrap();
    out.extend_from_slice(b"0000000000 65535 f \n");
    for n in 1..=nobj {
        writeln!(out, "{:010} 00000 n ", offsets[n]).unwrap();
    }
    writeln!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF",
        nobj + 1,
        xref_pos
    )
    .unwrap();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fontdb::{synth_tables, synth_ttc, synth_ttf};

    fn sample_pages() -> Vec<Page> {
        vec![Page {
            width: 595.276,
            height: 841.89,
            items: vec![
                DrawItem::Text {
                    x: 70.866,
                    y: 85.0,
                    font: FontId::Regular,
                    size: 11.0,
                    text: "Hello, “world” — é (test) \\ done".into(),
                },
                DrawItem::Text {
                    x: 70.866,
                    y: 99.3,
                    font: FontId::Mono,
                    size: 10.0,
                    text: "code".into(),
                },
            ],
        }]
    }

    fn has(pdf: &[u8], pat: &[u8]) -> bool {
        pdf.windows(pat.len()).any(|w| w == pat)
    }

    #[test]
    fn structure_and_winansi_encoding() {
        let pdf = write_pdf(&sample_pages(), &[]);
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        assert!(has(&pdf, b"/Type /Catalog"));
        assert!(has(&pdf, b"/Count 1"));
        assert!(has(&pdf, b"/MediaBox [0 0 595.28 841.89]"));
        assert!(has(&pdf, b"/BaseFont /Times-Roman "));
        assert!(has(&pdf, b"/BaseFont /Courier "));
        // Unused faces get no font object.
        assert!(!has(&pdf, b"Bold"));
        assert!(!has(&pdf, b"Oblique"));
        // Text op: y flipped (841.89 - 85 = 756.89), <=2-decimal numbers.
        assert!(has(&pdf, b"BT /F1 11 Tf 1 0 0 1 70.87 756.89 Tm "));
        assert!(has(
            &pdf,
            b"BT /F2 10 Tf 1 0 0 1 70.87 742.59 Tm (code) Tj ET"
        ));
        // WinAnsi: “ ” — é as octal escapes; ( ) \ escaped.
        assert!(has(
            &pdf,
            b"(Hello, \\223world\\224 \\227 \\351 \\(test\\) \\\\ done) Tj ET"
        ));
        assert!(has(&pdf, b"/Encoding /WinAnsiEncoding"));
        assert!(!has(&pdf, b"/ID"));
    }

    #[test]
    fn xref_offsets_point_at_objects() {
        let pdf = write_pdf(&sample_pages(), &[]);
        let sx = pdf.windows(9).rposition(|w| w == b"startxref").unwrap();
        let tail = std::str::from_utf8(&pdf[sx + 9..]).unwrap();
        let xoff: usize = tail
            .trim()
            .split_whitespace()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(&pdf[xoff..xoff + 4], b"xref");
        let xsec = std::str::from_utf8(&pdf[xoff..sx]).unwrap();
        let mut lines = xsec.lines();
        assert_eq!(lines.next().unwrap(), "xref");
        let count: usize = lines
            .next()
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let mut entries: Vec<&str> = lines.by_ref().take(count).collect();
        assert_eq!(entries.len(), count);
        assert_eq!(entries.remove(0), "0000000000 65535 f ");
        for (n, line) in entries.iter().enumerate() {
            assert_eq!(line.len(), 19);
            assert!(line.ends_with(" 00000 n "));
            let off: usize = line[..10].parse().unwrap();
            let expect = format!("{} 0 obj\n", n + 1);
            assert_eq!(&pdf[off..off + expect.len()], expect.as_bytes());
        }
    }

    #[test]
    fn empty_and_multi_page() {
        let pdf = write_pdf(&[], &[]);
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        assert!(has(&pdf, b"/Count 0 /Kids []"));

        let pages = vec![
            Page {
                width: 100.0,
                height: 200.0,
                items: vec![],
            },
            Page {
                width: 100.0,
                height: 200.0,
                items: vec![DrawItem::Text {
                    x: 10.0,
                    y: 20.0,
                    font: FontId::BoldItalic,
                    size: 12.5,
                    text: "x".into(),
                }],
            },
        ];
        let pdf = write_pdf(&pages, &[]);
        assert!(has(&pdf, b"/Count 2"));
        assert!(has(&pdf, b"/BaseFont /Times-BoldItalic "));
        assert!(has(&pdf, b"BT /F1 12.5 Tf 1 0 0 1 10 180 Tm (x) Tj ET"));
        assert!(!has(&pdf, b"/Courier"));
    }

    #[test]
    fn rule_rect_and_gray() {
        let pages = vec![Page {
            width: 100.0,
            height: 200.0,
            items: vec![DrawItem::Rule {
                x: 10.0,
                y: 20.0,
                w: 50.0,
                h: 2.5,
                gray: 0.5,
            }],
        }];
        let pdf = write_pdf(&pages, &[]);
        // y flipped: 200 - 20 - 2.5 = 177.5
        assert!(has(&pdf, b"0.5 g 10 177.5 50 2.5 re f"));
    }

    #[test]
    fn ext_font_embeds_truetype() {
        let dir = std::env::temp_dir().join(format!("typrst-pdf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("synth.ttf");
        let bytes = synth_ttf(&synth_tables(
            "Test Family",
            "Bold",
            "TestFam-Bold",
            700,
            false,
            true,
        ));
        std::fs::write(&p, &bytes).unwrap();
        let fonts = vec![FontUse {
            postscript_name: "TestFam-Bold".into(),
            family: "Test Family".into(),
            path: p.clone(),
            ttc_index: 0,
        }];
        let pages = vec![Page {
            width: 100.0,
            height: 200.0,
            items: vec![DrawItem::Text {
                x: 10.0,
                y: 20.0,
                font: FontId::Ext(0),
                size: 12.0,
                text: "A".into(),
            }],
        }];
        let pdf = write_pdf(&pages, &fonts);
        assert!(has(&pdf, b"/Subtype /TrueType"));
        assert!(has(&pdf, b"/BaseFont /TestFam-Bold "));
        assert!(has(&pdf, b"/FontDescriptor"));
        assert!(has(&pdf, b"/FontFile2"));
        assert!(has(&pdf, b"/FirstChar 32 /LastChar 255 /Widths ["));
        assert!(has(&pdf, b"BT /F1 12 Tf "));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_ext_falls_back_to_times() {
        let fonts = vec![FontUse {
            postscript_name: "Nope".into(),
            family: "Nope".into(),
            path: Path::new("/nonexistent/font.ttf").into(),
            ttc_index: 0,
        }];
        let pages = vec![Page {
            width: 100.0,
            height: 200.0,
            items: vec![DrawItem::Text {
                x: 10.0,
                y: 20.0,
                font: FontId::Ext(0),
                size: 12.0,
                text: "x".into(),
            }],
        }];
        let pdf = write_pdf(&pages, &fonts);
        assert!(has(&pdf, b"/BaseFont /Times-Roman "));
        assert!(!has(&pdf, b"/FontFile2"));
    }

    #[test]
    fn ttc_extraction_checksums() {
        let a = synth_ttf(&synth_tables(
            "Fam",
            "Regular",
            "Fam-Regular",
            400,
            false,
            false,
        ));
        let b = synth_ttf(&synth_tables("Fam", "Bold", "Fam-Bold", 700, false, false));
        let ttc = synth_ttc(&[a, b]);
        let dir = std::env::temp_dir().join(format!("typrst-ttc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.ttc");
        std::fs::write(&p, &ttc).unwrap();
        for idx in 0..2 {
            let f = extract_ttc_face(&p, idx).unwrap();
            assert_eq!(&f[0..4], &0x00010000u32.to_be_bytes());
            // whole-file checksum must equal the sfnt magic
            assert_eq!(checksum(&f), 0xB1B0AFBA);
        }
        assert!(extract_ttc_face(&p, 9).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn number_formatting() {
        let mut v = Vec::new();
        for (x, want) in [
            (11.0, "11"),
            (12.5, "12.5"),
            (595.276, "595.28"),
            (70.866, "70.87"),
            (0.0, "0"),
            (1.999, "2"),
        ] {
            v.clear();
            push_num(&mut v, x);
            assert_eq!(v, want.as_bytes());
        }
    }
}
