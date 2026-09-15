//! Layout engine: Document + FontMetrics -> display list of Pages.
use crate::ir::*;

/// A paragraph token. Words borrow their text from the document.
enum Tok<'a> {
    /// Word text, resolved style, width in pt.
    W(&'a str, TextStyle, f64),
    /// Inter-word space: natural width in pt (stretchable under justify).
    S(f64),
    /// `#h(..)` gap: fixed width, breakable, never stretched.
    G(f64),
    /// Forced line break.
    B,
}

/// A laid-out line: token range plus precomputed natural width.
#[derive(Clone, Copy)]
struct Line {
    start: usize,
    end: usize,
    w: f64,
    spaces: u32,
    hard: bool, // terminated by an explicit LineBreak
}

#[derive(Clone, Copy, PartialEq)]
enum Sep {
    Cat, // previous token was a word: concat directly
    Sp,  // previous token was an unstretched space: insert ' '
    Brk, // cannot merge: start a new draw run
}

fn bolden(f: FontId) -> FontId {
    match f {
        FontId::Regular | FontId::Bold => FontId::Bold,
        FontId::Italic | FontId::BoldItalic => FontId::BoldItalic,
        FontId::Mono | FontId::MonoBold => FontId::MonoBold,
    }
}

fn ident(s: TextStyle) -> TextStyle {
    s
}

pub fn layout(doc: &Document, metrics: &dyn FontMetrics) -> Vec<Page> {
    let mut lx = Lx {
        m: metrics,
        st: &doc.styles,
        pages: Vec::new(),
        items: Vec::new(),
        y: doc.styles.margin_top,
        pending_gap: 0.0,
        after: 0.0,
        fresh: true,
        toks: Vec::new(),
        lines: Vec::new(),
    };
    for b in &doc.blocks {
        lx.block(b);
    }
    if !lx.items.is_empty() || lx.pages.is_empty() {
        lx.finish_page();
    }
    lx.pages
}

struct Lx<'a> {
    m: &'a dyn FontMetrics,
    st: &'a Styles,
    pages: Vec<Page>,
    items: Vec<DrawItem>,
    y: f64,          // baseline of the previous line (margin_top when fresh)
    pending_gap: f64, // inter-block spacing applied to the next line's delta
    after: f64,      // trailing spacing of the previous block
    fresh: bool,     // nothing drawn on this page yet (top spacing collapses)
    toks: Vec<Tok<'a>>,
    lines: Vec<Line>,
}

impl<'a> Lx<'a> {
    fn text_w(&self) -> f64 {
        self.st.page_width - self.st.margin_left - self.st.margin_right
    }

    fn finish_page(&mut self) {
        let items = std::mem::take(&mut self.items);
        self.pages.push(Page {
            width: self.st.page_width,
            height: self.st.page_height,
            items,
        });
        self.y = self.st.margin_top;
        self.fresh = true;
        self.after = 0.0;
        self.pending_gap = 0.0;
    }

    /// Inter-block spacing: collapses to max(prev_below, next_above),
    /// applied to the next baseline delta. Vanishes at page top.
    fn gap(&mut self, before: f64) {
        if !self.fresh {
            self.pending_gap = self.pending_gap.max(self.after.max(before));
        }
        self.after = 0.0;
    }

    /// Line height for a line whose tallest font is `size`.
    fn lh(&self, size: f64) -> f64 {
        if self.st.font_size > 0.0 {
            size * self.st.line_height / self.st.font_size
        } else {
            size * 1.3082
        }
    }

    fn block(&mut self, b: &'a Block) {
        let ml = self.st.margin_left;
        match b {
            Block::Paragraph { content, align } => {
                // Paragraphs contribute below-spacing only (6.05pt), matching
                // typst: heading->para gets no extra gap.
                self.gap(0.0);
                let justify = self.st.justify && *align == HAlign::Left;
                self.flow(content, ml, self.text_w(), *align, justify, None, &ident);
                self.after = self.st.par_spacing;
            }
            Block::Heading { level, content } => {
                let f = match level {
                    1 => 1.4,
                    2 => 1.2,
                    3 => 1.05,
                    _ => 1.0,
                };
                let size = self.st.font_size * f;
                self.gap(0.536 * size); // measured: 7.08pt before an h2
                self.flow(
                    content,
                    ml,
                    self.text_w(),
                    HAlign::Left,
                    false,
                    None,
                    &move |s: TextStyle| TextStyle { font: bolden(s.font), size },
                );
                self.after = 1.1;
            }
            Block::List { ordered, items } => {
                // Small before-gap; collapses to the previous block's
                // trailing gap when larger.
                self.gap(1.1);
                for (i, it) in items.iter().enumerate() {
                    let mk = if *ordered {
                        format!("{}.", i + 1)
                    } else {
                        "\u{2022}".to_string()
                    };
                    let mst = TextStyle { font: FontId::Regular, size: self.st.font_size };
                    // typst hangs the marker at the margin; item text starts
                    // after the marker + a 0.5em gap, continuations aligned.
                    let ind = self.m.str_width(mst.font, mst.size, &mk) + 0.5 * mst.size;
                    self.flow(
                        it,
                        ml + ind,
                        self.text_w() - ind,
                        HAlign::Left,
                        false,
                        Some((mk, mst, ml)),
                        &ident

                    );
                }
                self.after = self.st.par_spacing;
            }
            Block::RawBlock { text } => {
                self.gap(0.0);
                let size = self.st.font_size * 0.9;
                let lh = self.lh(size);
                let asc = self.m.ascent(FontId::Mono) * size / 1000.0;
                let desc = self.m.descent(FontId::Mono) * size / 1000.0;
                let limit = self.st.page_height - self.st.margin_bottom;
                for line in text.split('\n') {
                    let mut baseline = if self.fresh {
                        self.st.margin_top + asc
                    } else {
                        self.y + lh + self.pending_gap
                    };
                    self.pending_gap = 0.0;
                    if !self.items.is_empty() && baseline + desc > limit {
                        self.finish_page();
                        baseline = self.st.margin_top + asc;
                    }
                    self.items.push(DrawItem::Text {
                        x: ml,
                        y: baseline,
                        font: FontId::Mono,
                        size,
                        text: line.to_string(),
                    });
                    self.y = baseline;
                    self.fresh = false;
                }
                self.after = self.st.par_spacing;
            }
            Block::VSpace(v) => {
                self.pending_gap += self.after + v;
                self.after = 0.0;
            }
            Block::PageBreak => self.finish_page(),
        }
    }

    /// Lay out inline content: tokenize, break into lines, place each.
    fn flow(
        &mut self,
        content: &'a [Inline],
        x0: f64,
        width: f64,
        align: HAlign,
        justify: bool,
        marker: Option<(String, TextStyle, f64)>,
        xf: &dyn Fn(TextStyle) -> TextStyle,
    ) {
        let mut toks = std::mem::take(&mut self.toks);
        let mut lines = std::mem::take(&mut self.lines);
        toks.clear();
        lines.clear();
        tokenize(&mut toks, content, self.m, xf);
        break_lines(&toks, width, &mut lines);
        self.toks = toks;
        self.lines = lines;

        let n = self.lines.len();
        let mut marker = marker;
        if n == 0 {
            if let Some((mt, mst, mx)) = marker.take() {
                self.place_line(
                    Line { start: 0, end: 0, w: 0.0, spaces: 0, hard: false },
                    x0,
                    width,
                    align,
                    false,
                    true,
                    Some((mt.as_str(), mst, mx)),
                );
            }
            return;
        }
        for k in 0..n {
            let l = self.lines[k];
            let last = k + 1 == n;
            let mk = marker.take();
            match mk {
                Some((mt, mst, mx)) => self.place_line(
                    l,
                    x0,
                    width,
                    align,
                    justify,
                    last,
                    Some((mt.as_str(), mst, mx)),
                ),
                None => self.place_line(l, x0, width, align, justify, last, None),
            }
        }
    }

    /// Emit one line of tokens at the current vertical cursor.
    fn place_line(
        &mut self,
        l: Line,
        x0: f64,
        width: f64,
        align: HAlign,
        justify: bool,
        last: bool,
        marker: Option<(&str, TextStyle, f64)>,
    ) {
        // line box extents from the tallest run
        let mut size = 0.0f64;
        let mut asc = 0.0f64;
        let mut desc = 0.0f64;
        let mut extent = |st: &TextStyle| {
            size = size.max(st.size);
            asc = asc.max(self.m.ascent(st.font) * st.size / 1000.0);
            desc = desc.max(self.m.descent(st.font) * st.size / 1000.0);
        };
        for t in &self.toks[l.start..l.end] {
            if let Tok::W(_, st, _) = t {
                extent(st);
            }
        }
        if let Some((_, st, _)) = &marker {
            extent(st);
        }
        if size == 0.0 {
            size = self.st.font_size;
            asc = self.m.ascent(FontId::Regular) * size / 1000.0;
            desc = self.m.descent(FontId::Regular) * size / 1000.0;
        }
        // Baseline delta = this line's own line height + inter-block gap.
        let mut baseline = if self.fresh {
            self.st.margin_top + asc
        } else {
            self.y + self.lh(size) + self.pending_gap
        };
        self.pending_gap = 0.0;
        let limit = self.st.page_height - self.st.margin_bottom;
        if !self.items.is_empty() && baseline + desc > limit {
            self.finish_page();
            baseline = self.st.margin_top + asc;
        }

        // justification slack over inter-word spaces, capped at 2x stretch
        let mut extra = 0.0;
        if justify && !last && !l.hard && l.spaces > 0 && l.w < width {
            let e = (width - l.w) / l.spaces as f64;
            let ok = self.toks[l.start..l.end]
                .iter()
                .all(|t| !matches!(t, Tok::S(s) if *s < e));
            if ok {
                extra = e;
            }
        }
        let slack = (width - l.w).max(0.0);
        let mut x = x0
            + match align {
                HAlign::Center => slack / 2.0,
                HAlign::Right => slack,
                HAlign::Left => 0.0,
            };
        if let Some((mt, mst, mx)) = marker {
            self.items.push(DrawItem::Text {
                x: mx,
                y: baseline,
                font: mst.font,
                size: mst.size,
                text: mt.to_string(),
            });
        }

        let mut sep = Sep::Brk;
        let mut buf = String::new();
        let mut bx = 0.0;
        let mut bs = TextStyle { font: FontId::Regular, size };
        let mut has = false;
        for t in &self.toks[l.start..l.end] {
            match *t {
                Tok::W(w, st, wid) => {
                    if has && bs == st && sep != Sep::Brk {
                        if sep == Sep::Sp {
                            buf.push(' ');
                        }
                        buf.push_str(w);
                    } else {
                        if has {
                            self.items.push(DrawItem::Text {
                                x: bx,
                                y: baseline,
                                font: bs.font,
                                size: bs.size,
                                text: std::mem::take(&mut buf),
                            });
                        }
                        bx = x;
                        bs = st;
                        buf.push_str(w);
                        has = true;
                    }
                    x += wid;
                    sep = Sep::Cat;
                }
                Tok::S(nat) => {
                    x += if extra > 0.0 { nat + extra } else { nat };
                    sep = if extra > 0.0 { Sep::Brk } else { Sep::Sp };
                }
                Tok::G(g) => {
                    x += g;
                    sep = Sep::Brk;
                }
                Tok::B => {}
            }
        }
        if has {
            self.items.push(DrawItem::Text {
                x: bx,
                y: baseline,
                font: bs.font,
                size: bs.size,
                text: buf,
            });
        }
        self.y = baseline;
        self.fresh = false;
    }
}

/// Split inline content into word/space tokens; words get `xf`-mapped styles.
fn tokenize<'a>(
    toks: &mut Vec<Tok<'a>>,
    content: &'a [Inline],
    m: &dyn FontMetrics,
    xf: &dyn Fn(TextStyle) -> TextStyle,
) {
    for i in content {
        match i {
            Inline::Text { text, style } => {
                let st = xf(*style);
                let space_w = m.str_width(st.font, st.size, " ");
                let mut w0 = None;
                for (k, c) in text.char_indices() {
                    if c.is_whitespace() {
                        if let Some(s) = w0.take() {
                            let w = &text[s..k];
                            toks.push(Tok::W(w, st, m.str_width(st.font, st.size, w)));
                        }
                        if !matches!(toks.last(), Some(Tok::S(_))) {
                            toks.push(Tok::S(space_w));
                        }
                    } else if w0.is_none() {
                        w0 = Some(k);
                    }
                }
                if let Some(s) = w0 {
                    let w = &text[s..];
                    toks.push(Tok::W(w, st, m.str_width(st.font, st.size, w)));
                }
            }
            Inline::LineBreak => toks.push(Tok::B),
            Inline::HSpace(w) => toks.push(Tok::G(*w)),
        }
    }
}

/// Greedy line breaking at space tokens; `LineBreak` forces a break.
fn break_lines(toks: &[Tok], width: f64, lines: &mut Vec<Line>) {
    let mut start = 0usize;
    let mut end = 0usize;
    let mut w = 0.0f64;
    let mut sp = 0u32;
    let mut pend_w = 0.0f64; // uncommitted space width
    let mut pend_n = 0u32;
    let mut i = 0;
    while i < toks.len() {
        match toks[i] {
            Tok::W(_, _, ww) => {
                if end > start && w + pend_w + ww > width {
                    lines.push(Line { start, end, w, spaces: sp, hard: false });
                    start = i;
                    end = i + 1;
                    w = ww;
                    sp = 0;
                } else {
                    if end > start {
                        w += pend_w;
                        sp += pend_n;
                    }
                    w += ww;
                    end = i + 1;
                }
                pend_w = 0.0;
                pend_n = 0;
                i += 1;
            }
            Tok::S(sw) => {
                pend_w += sw;
                pend_n += 1;
                i += 1;
            }
            Tok::G(gw) => {
                if end > start && w + pend_w + gw > width {
                    lines.push(Line { start, end, w, spaces: sp, hard: false });
                    i += 1;
                    start = i;
                    end = i;
                    w = 0.0;
                    sp = 0;
                } else {
                    if end > start {
                        w += pend_w;
                        sp += pend_n;
                    }
                    w += gw;
                    end = i + 1;
                    i += 1;
                }
                pend_w = 0.0;
                pend_n = 0;
            }
            Tok::B => {
                lines.push(Line { start, end, w, spaces: sp, hard: true });
                i += 1;
                start = i;
                end = i;
                w = 0.0;
                sp = 0;
                pend_w = 0.0;
                pend_n = 0;
            }
        }
    }
    if end > start {
        lines.push(Line { start, end, w, spaces: sp, hard: false });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fake;
    impl FontMetrics for Fake {
        fn char_width(&self, _f: FontId, _c: char) -> f64 {
            500.0
        }
    }

    fn tdoc(blocks: Vec<Block>) -> Document {
        Document { styles: Styles::default(), blocks, diags: vec![] }
    }

    fn para(words: usize) -> Block {
        let text = vec!["aaaa"; words].join(" ");
        Block::Paragraph {
            content: vec![Inline::Text {
                text,
                style: TextStyle { font: FontId::Regular, size: 11.0 },
            }],
            align: HAlign::Left,
        }
    }

    fn texts(p: &Page) -> Vec<(f64, f64, &str, FontId, f64)> {
        p.items
            .iter()
            .map(|i| match i {
                DrawItem::Text { x, y, font, size, text } => (*x, *y, text.as_str(), *font, *size),
            })
            .collect()
    }

    // Fake metrics: word "aaaa" = 4*500*11/1000 = 22pt; space = 5.5pt.
    // Text width = 595.276 - 2*70.866 = 453.544 -> 16 words/line.
    // ascent 718 -> first baseline = 70.866 + 7.898 = 78.764; lh = 14.0.

    #[test]
    fn layout_wraps_greedily() {
        let doc = tdoc(vec![para(33)]);
        let pages = layout(&doc, &Fake);
        assert_eq!(pages.len(), 1);
        let items = texts(&pages[0]);
        // 16 + 16 + 1 words -> 3 merged runs
        assert_eq!(items.len(), 3);
        let (x, y, ..) = items[0];
        assert!((x - 70.866).abs() < 1e-6);
        assert!((y - 78.764).abs() < 1e-3);
        assert!((items[1].1 - items[0].1 - 14.39).abs() < 1e-6);
        assert!((items[2].1 - items[1].1 - 14.39).abs() < 1e-6);
        // first line has 16 words merged
        assert_eq!(items[0].2.matches("aaaa").count(), 16);
    }

    #[test]
    fn layout_page_break() {
        let doc = tdoc(vec![para(3), Block::PageBreak, para(3)]);
        let pages = layout(&doc, &Fake);
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].items.len(), 1);
        let items = texts(&pages[1]);
        assert_eq!(items.len(), 1);
        assert!((items[0].1 - 78.764).abs() < 1e-3); // back at top margin
    }

    #[test]
    fn layout_paginates_long_paragraph() {
        let doc = tdoc(vec![para(1200)]); // ~75 lines > ~49/page
        let pages = layout(&doc, &Fake);
        assert!(pages.len() >= 2);
        assert!(!pages[1].items.is_empty());
    }

    #[test]
    fn layout_heading_and_list() {
        let doc = tdoc(vec![
            Block::Heading {
                level: 1,
                content: vec![Inline::Text {
                    text: "Title".into(),
                    style: TextStyle { font: FontId::Regular, size: 11.0 },
                }],
            },
            Block::List {
                ordered: true,
                items: vec![
                    vec![Inline::Text {
                        text: "first".into(),
                        style: TextStyle { font: FontId::Regular, size: 11.0 },
                    }],
                    vec![Inline::Text {
                        text: "second".into(),
                        style: TextStyle { font: FontId::Regular, size: 11.0 },
                    }],
                ],
            },
        ]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        // heading: bold 15.4pt (1.4em)
        let (_, hy, ht, hf, hs) = items[0];
        assert_eq!(ht, "Title");
        assert_eq!(hf, FontId::Bold);
        assert!((hs - 15.4).abs() < 1e-6);
        assert!((hy - (70.866 + 718.0 * 15.4 / 1000.0)).abs() < 1e-3);
        // marker "1." at margin_left, item text at margin + marker + 0.5em
        // (fake metrics: "1." = 2 chars * 500 * 11 / 1000 = 11pt + 5.5 = 16.5)
        let m1 = items.iter().find(|i| i.2 == "1.").unwrap();
        assert!((m1.0 - 70.866).abs() < 1e-6);
        let it1 = items.iter().find(|i| i.2 == "first").unwrap();
        assert!((it1.0 - 70.866 - 16.5).abs() < 1e-6);
        assert_eq!(m1.1, it1.1); // same baseline
        let m2 = items.iter().find(|i| i.2 == "2.").unwrap();
        // item 2 baseline = item1 baseline + lh
        assert!((m2.1 - m1.1 - 14.39).abs() < 1e-6);
    }

    #[test]
    fn layout_justify_and_center() {
        let mut doc = tdoc(vec![para(33)]);
        doc.styles.justify = true;
        let pages = layout(&doc, &Fake);
        // justified lines: spaces stretched -> per-word draw items
        // 16 + 16 + 1(last line unmerged? same style unstretched -> merges)
        let items = texts(&pages[0]);
        assert_eq!(items.len(), 33);

        let mut doc = tdoc(vec![Block::Paragraph {
            content: vec![Inline::Text {
                text: "aaaa".into(),
                style: TextStyle { font: FontId::Regular, size: 11.0 },
            }],
            align: HAlign::Center,
        }]);
        doc.styles.justify = false;
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        // centered: x = margin_left + (453.544 - 22)/2
        assert!((items[0].0 - (70.866 + (453.544 - 22.0) / 2.0)).abs() < 1e-3);
    }

    #[test]
    fn layout_raw_and_vspace() {
        let doc = tdoc(vec![
            para(1),
            Block::RawBlock { text: "code\ntwo".into() },
            Block::VSpace(20.0),
            para(1),
        ]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        let raw: Vec<_> = items.iter().filter(|i| i.3 == FontId::Mono).collect();
        assert_eq!(raw.len(), 2);
        assert!((raw[0].4 - 9.9).abs() < 1e-6);
        // paragraph gap collapses with raw spacing; then VSpace + para gap.
        let p2 = items.last().unwrap();
        // y of para2 = para1_top(70.866) + lh(14) + gap(6.7) + 2*raw_lh + gap(6.7) + 20 + gap(6.7) + asc
        // para1 baseline = 70.866 + 7.898; raw adds lh(9.9*1.3082) per line,
        // first raw line also takes the 6.05 gap; vspace stacks gap+20; para2 adds its lh.
        let expect = 70.866 + 7.898 + 2.0 * (9.9 * 1.3082) + 6.05 + 6.05 + 20.0 + 14.39;
        assert!((p2.1 - expect).abs() < 1e-3);
    }

    #[test]
    fn layout_hspace_and_linebreak() {
        let doc = tdoc(vec![Block::Paragraph {
            content: vec![
                Inline::Text {
                    text: "aa".into(),
                    style: TextStyle { font: FontId::Regular, size: 11.0 },
                },
                Inline::HSpace(50.0),
                Inline::Text {
                    text: "bb".into(),
                    style: TextStyle { font: FontId::Regular, size: 11.0 },
                },
                Inline::LineBreak,
                Inline::Text {
                    text: "cc".into(),
                    style: TextStyle { font: FontId::Regular, size: 11.0 },
                },
            ],
            align: HAlign::Left,
        }]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        assert_eq!(items.len(), 3); // "aa", "bb", "cc"
        // bb is 11pt (aa width) + 50pt h-space to the right
        assert!((items[1].0 - items[0].0 - 11.0 - 50.0).abs() < 1e-6);
        // cc on next baseline
        assert!((items[2].1 - items[0].1 - 14.39).abs() < 1e-6);
    }
}
