//! SVG export of the display list (debug/preview route).
use crate::ir::{DrawItem, FontId, FontUse, Page};
use std::borrow::Cow;
use std::fmt::Write;

const GAP: f64 = 10.0;

fn esc(s: &str, out: &mut String) {
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
}

fn num(v: f64) -> String {
    let mut s = format!("{v:.2}");
    if s.contains('.') {
        while s.ends_with('0') {
            s.pop();
        }
        if s.ends_with('.') {
            s.pop();
        }
    }
    if s == "-0" {
        s = "0".into();
    }
    s
}

/// CSS family + style flags for a FontId. Ext faces defer to the family
/// name recorded in `FontUse` (quoted); bold/italic are baked into the
/// resolved face already, so no style flags are emitted for them.
fn family<'a>(font: FontId, fonts: &'a [FontUse]) -> (Cow<'a, str>, bool, bool) {
    match font {
        FontId::Regular => ("Times New Roman, serif".into(), false, false),
        FontId::Bold => ("Times New Roman, serif".into(), true, false),
        FontId::Italic => ("Times New Roman, serif".into(), false, true),
        FontId::BoldItalic => ("Times New Roman, serif".into(), true, true),
        FontId::Mono => ("Courier New, monospace".into(), false, false),
        FontId::MonoBold => ("Courier New, monospace".into(), true, false),
        FontId::Ext(i) => {
            let fam = fonts
                .get(i as usize)
                .map(|f| format!("'{}'", f.family.replace('\'', " ")))
                .unwrap_or_else(|| "serif".into());
            (fam.into(), false, false)
        }
    }
}

fn gray_hex(g: f64) -> String {
    let v = (g * 255.0).round().clamp(0.0, 255.0) as u8;
    format!("#{v:02x}{v:02x}{v:02x}")
}

pub fn render(pages: &[Page], fonts: &[FontUse]) -> String {
    let w = pages.iter().map(|p| p.width).fold(0.0, f64::max);
    let h =
        pages.iter().map(|p| p.height).sum::<f64>() + GAP * pages.len().saturating_sub(1) as f64;
    let mut s =
        String::with_capacity(256 + pages.iter().map(|p| p.items.len() * 96).sum::<usize>());
    writeln!(
        s,
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{}\" height=\"{}\" viewBox=\"0 0 {} {}\">",
        num(w),
        num(h),
        num(w),
        num(h)
    )
    .unwrap();
    let mut top = 0.0;
    for page in pages {
        writeln!(
            s,
            "<rect x=\"4\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#d5d5d5\"/>",
            num(top + 4.0),
            num(page.width),
            num(page.height)
        )
        .unwrap();
        writeln!(
            s,
            "<rect x=\"0\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#fff\" stroke=\"#bbb\" stroke-width=\"0.5\"/>",
            num(top),
            num(page.width),
            num(page.height)
        )
        .unwrap();
        for item in &page.items {
            match item {
                DrawItem::Text {
                    x,
                    y,
                    font,
                    size,
                    text,
                } => {
                    let (fam, bold, italic) = family(*font, fonts);
                    write!(
                        s,
                        "<text x=\"{}\" y=\"{}\" font-family=\"",
                        num(*x),
                        num(top + *y)
                    )
                    .unwrap();
                    esc(&fam, &mut s);
                    write!(s, "\" font-size=\"{}\"", num(*size)).unwrap();
                    if bold {
                        s.push_str(" font-weight=\"bold\"");
                    }
                    if italic {
                        s.push_str(" font-style=\"italic\"");
                    }
                    s.push('>');
                    esc(text, &mut s);
                    s.push_str("</text>\n");
                }
                DrawItem::Rule { x, y, w, h, gray } => {
                    writeln!(
                        s,
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"{}\"/>",
                        num(*x),
                        num(top + *y),
                        num(*w),
                        num(*h),
                        gray_hex(*gray)
                    )
                    .unwrap();
                }
                DrawItem::Image { x, y, w, h, .. } => {
                    writeln!(
                        s,
                        "<rect x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\" fill=\"#808080\"/>",
                        num(*x),
                        num(top + *y),
                        num(*w),
                        num(*h)
                    )
                    .unwrap();
                }
            }
        }
        top += page.height + GAP;
    }
    s.push_str("</svg>\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> Vec<Page> {
        vec![
            Page {
                width: 595.28,
                height: 841.89,
                items: vec![
                    DrawItem::Text {
                        x: 70.87,
                        y: 85.0,
                        font: FontId::Regular,
                        size: 11.0,
                        text: "a<b>&\"'".into(),
                    },
                    DrawItem::Text {
                        x: 70.87,
                        y: 99.3,
                        font: FontId::BoldItalic,
                        size: 16.0,
                        text: "hi".into(),
                    },
                    DrawItem::Rule {
                        x: 70.87,
                        y: 110.0,
                        w: 200.0,
                        h: 0.5,
                        gray: 0.5,
                    },
                ],
            },
            Page {
                width: 595.28,
                height: 841.89,
                items: vec![DrawItem::Text {
                    x: 10.0,
                    y: 20.0,
                    font: FontId::Mono,
                    size: 10.0,
                    text: "code".into(),
                }],
            },
        ]
    }

    #[test]
    fn stacks_pages_and_escapes() {
        let s = render(&pages(), &[]);
        assert!(s.starts_with("<svg xmlns="));
        assert!(s.ends_with("</svg>\n"));
        // viewBox height = 2 pages + 1 gap
        assert!(s.contains("viewBox=\"0 0 595.28 1693.78\""));
        // 2 page rects + shadow + rule rect
        assert!(s.matches("<rect").count() == 5);
        // text on page 1 keeps its y; page 2 text is offset by page height + gap
        assert!(s.contains("x=\"70.87\" y=\"85\""));
        assert!(s.contains("x=\"10\" y=\"871.89\""));
        // styles
        assert!(s.contains("font-family=\"Times New Roman, serif\""));
        assert!(s.contains("font-weight=\"bold\" font-style=\"italic\""));
        assert!(s.contains("font-family=\"Courier New, monospace\""));
        // rule -> rect, 0.5 gray -> #808080
        assert!(s.contains(
            "<rect x=\"70.87\" y=\"110\" width=\"200\" height=\"0.5\" fill=\"#808080\"/>"
        ));
        // escaping
        assert!(s.contains(">a&lt;b&gt;&amp;&quot;&apos;</text>"));
    }

    #[test]
    fn ext_font_family() {
        let fonts = vec![FontUse {
            postscript_name: "Menlo-Regular".into(),
            family: "Menlo".into(),
            path: "/System/Library/Fonts/Menlo.ttc".into(),
            ttc_index: 0,
        }];
        let pages = vec![Page {
            width: 100.0,
            height: 100.0,
            items: vec![DrawItem::Text {
                x: 5.0,
                y: 10.0,
                font: FontId::Ext(0),
                size: 12.0,
                text: "hi".into(),
            }],
        }];
        let s = render(&pages, &fonts);
        assert!(s.contains("font-family=\"&apos;Menlo&apos;\""));
        // out-of-range Ext index degrades to generic serif
        let s2 = render(
            &[Page {
                width: 10.0,
                height: 10.0,
                items: vec![DrawItem::Text {
                    x: 0.0,
                    y: 0.0,
                    font: FontId::Ext(9),
                    size: 1.0,
                    text: "z".into(),
                }],
            }],
            &fonts,
        );
        assert!(s2.contains("font-family=\"serif\""));
    }

    #[test]
    fn empty_document() {
        let s = render(&[], &[]);
        assert!(s.contains("viewBox=\"0 0 0 0\""));
        assert!(s.ends_with("</svg>\n"));
    }

    #[test]
    fn image_is_a_grey_rect() {
        let pages = vec![Page {
            width: 200.0,
            height: 100.0,
            items: vec![DrawItem::Image {
                x: 10.0,
                y: 20.0,
                w: 30.0,
                h: 40.0,
                index: 0,
            }],
        }];
        let s = render(&pages, &[]);
        assert!(s.contains(
            "<rect x=\"10\" y=\"20\" width=\"30\" height=\"40\" fill=\"#808080\"/>"
        ));
    }
}
