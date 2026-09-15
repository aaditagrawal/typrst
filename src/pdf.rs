//! Minimal PDF writer: display list -> PDF bytes (base-14 fonts).
use crate::font::{base14_name, winansi_code};
use crate::ir::{DrawItem, FontId, Page};
use std::io::Write;

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

pub fn write_pdf(pages: &[Page]) -> Vec<u8> {
    // Font objects only for faces actually used, in FontId::ALL order.
    let used: Vec<FontId> = FontId::ALL
        .into_iter()
        .filter(|f| {
            pages.iter().any(|p| {
                p.items
                    .iter()
                    .any(|i| matches!(i, DrawItem::Text { font, .. } if font == f))
            })
        })
        .collect();
    let font_base = 3 + 2 * pages.len(); // first font object id
    let nobj = 2 + 2 * pages.len() + used.len();

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
    let mut res = Vec::with_capacity(64);
    res.extend_from_slice(b"/Resources << ");
    if !used.is_empty() {
        res.extend_from_slice(b"/Font << ");
        for (j, _) in used.iter().enumerate() {
            write!(res, "/F{} {} 0 R ", j + 1, font_base + j).unwrap();
        }
        res.extend_from_slice(b">> ");
    }
    res.extend_from_slice(b">>");

    // FontId -> /F<n> index map (FontId is a small fieldless enum).
    let mut fnum = [0u8; 8];
    for (j, f) in used.iter().enumerate() {
        fnum[*f as usize] = j as u8 + 1;
    }

    for (i, page) in pages.iter().enumerate() {
        let mut content = Vec::with_capacity(page.items.len() * 64 + 16);
        for item in &page.items {
            match item {
                DrawItem::Text { x, y, font, size, text } => {
                    content.extend_from_slice(b"BT /F");
                    content.push(b'0' + fnum[*font as usize]);
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

    for (j, f) in used.iter().enumerate() {
        begin(&mut out, font_base + j);
        writeln!(
            out,
            "<< /Type /Font /Subtype /Type1 /BaseFont /{} /Encoding /WinAnsiEncoding >>\nendobj",
            base14_name(*f)
        )
        .unwrap();
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
        let pdf = write_pdf(&sample_pages());
        assert!(pdf.starts_with(b"%PDF-1.4"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        assert!(has(&pdf, b"/Type /Catalog"));
        assert!(has(&pdf, b"/Count 1"));
        assert!(has(&pdf, b"/MediaBox [0 0 595.28 841.89]"));
        assert!(has(&pdf, b"/BaseFont /Helvetica "));
        assert!(has(&pdf, b"/BaseFont /Courier "));
        // Unused faces get no font object.
        assert!(!has(&pdf, b"Bold"));
        assert!(!has(&pdf, b"Oblique"));
        // Text op: y flipped (841.89 - 85 = 756.89), <=2-decimal numbers.
        assert!(has(&pdf, b"BT /F1 11 Tf 1 0 0 1 70.87 756.89 Tm "));
        assert!(has(&pdf, b"BT /F2 10 Tf 1 0 0 1 70.87 742.59 Tm (code) Tj ET"));
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
        let pdf = write_pdf(&sample_pages());
        let sx = pdf.windows(9).rposition(|w| w == b"startxref").unwrap();
        let tail = std::str::from_utf8(&pdf[sx + 9..]).unwrap();
        let xoff: usize = tail.trim().split_whitespace().next().unwrap().parse().unwrap();
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
        let pdf = write_pdf(&[]);
        assert!(pdf.starts_with(b"%PDF-"));
        assert!(pdf.ends_with(b"%%EOF\n"));
        assert!(has(&pdf, b"/Count 0 /Kids []"));

        let pages = vec![
            Page { width: 100.0, height: 200.0, items: vec![] },
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
        let pdf = write_pdf(&pages);
        assert!(has(&pdf, b"/Count 2"));
        assert!(has(&pdf, b"/BaseFont /Helvetica-BoldOblique "));
        assert!(has(&pdf, b"BT /F1 12.5 Tf 1 0 0 1 10 180 Tm (x) Tj ET"));
        assert!(!has(&pdf, b"/Courier"));
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
