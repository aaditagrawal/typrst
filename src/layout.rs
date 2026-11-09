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

/// One token of the line being filled. A word records its parent string
/// and byte range so a merged run is one copy, not a copy per word.
enum Atom<'a> {
    W(&'a str, u32, u32, TextStyle, f64),
    S(f64),
    G(f64),
}

struct St {
    w: f64,
    spaces: u32,
    has: bool,
    pend_w: f64,
    pend_n: u32,
}

struct Meta {
    on: bool,
    w: f64,
    spaces: u32,
    hard: bool,
}

struct Geom {
    x0: f64,
    width: f64,
    align: HAlign,
    justify: bool,
}

/// A laid-out line: token range plus precomputed natural width.
#[derive(Clone, Copy)]
struct Line {
    start: usize,
    end: usize,
    // Table cells place token ranges directly; width and break kind stay
    // on the line so `break_lines` keeps one definition of a break.
    #[allow(dead_code)]
    w: f64,
    #[allow(dead_code)]
    spaces: u32,
    #[allow(dead_code)]
    hard: bool,
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
        // Resolved faces have no base-14 sibling; keep the face as-is.
        FontId::Ext(_) => f,
    }
}

fn ident(s: TextStyle) -> TextStyle {
    s
}

pub fn layout<M: FontMetrics>(doc: &Document, metrics: &M) -> Vec<Page> {
    let mut lx = Lx {
        m: metrics,
        st: &doc.styles,
        pages: Vec::new(),
        items: Vec::new(),
        y: doc.styles.margin_top,
        pending_gap: 0.0,
        after: 0.0,
        fresh: true,
        cur: Vec::new(),
        held: Vec::new(),
        term_prefix: None,
    };
    for b in &doc.blocks {
        lx.block(b);
    }
    if !lx.items.is_empty() || lx.pages.is_empty() {
        lx.finish_page();
    }
    lx.pages
}

struct Lx<'a, M: FontMetrics> {
    m: &'a M,
    st: &'a Styles,
    pages: Vec<Page>,
    items: Vec<DrawItem>,
    y: f64,           // baseline of the previous line (margin_top when fresh)
    pending_gap: f64, // inter-block spacing applied to the next line's delta
    after: f64,       // trailing spacing of the previous block
    fresh: bool,      // nothing drawn on this page yet (top spacing collapses)
    /// Current line, and the previous line held back so justification
    /// sees whether a line is the last one. Reused across blocks.
    cur: Vec<Atom<'a>>,
    held: Vec<Atom<'a>>,
    /// Term-list label drawn with the first line's marker slot. Paragraphs
    /// and bullet lists leave it `None`; the check lives in that slot.
    term_prefix: Option<&'a [Inline]>,
}

impl<'a, M: FontMetrics> Lx<'a, M> {
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
                    &move |s: TextStyle| TextStyle {
                        font: bolden(s.font),
                        size,
                    },
                );
                self.after = 1.1;
            }
            Block::List { ordered, items } => {
                // Small before-gap; collapses to the previous block's
                // trailing gap when larger.
                self.gap(1.1);
                self.lay_list(*ordered, items, ml);
                self.after = self.st.par_spacing;
            }
            Block::Terms { items } => {
                self.gap(1.1);
                let tw = self.text_w();
                for it in items {
                    self.lay_term(&it.term, &it.desc, ml, tw);
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
                        text: InlineStr::from(line),
                    });
                    self.y = baseline;
                    self.fresh = false;
                }
                self.after = self.st.par_spacing;
            }
            Block::Table {
                columns,
                cells,
                stroke,
            } => {
                const IX: f64 = 5.0; // cell inset, horizontal
                const IY: f64 = 4.0; // cell inset, vertical
                const W: f64 = 0.5; // stroke weight
                self.gap(0.0);
                self.after = self.st.par_spacing;
                if cells.is_empty() {
                    return;
                }
                // Empty/absent column specs degrade to a single auto column;
                // specs beyond `columns.len()` are treated as Auto.
                let ncols = columns.len().max(1);
                let total = self.text_w();
                let mut widths = vec![0.0; ncols];
                let mut used = 0.0;
                let mut frsum = 0.0;
                for c in 0..ncols {
                    match columns.get(c).copied().unwrap_or(ColSpec::Auto) {
                        ColSpec::Fixed(p) => widths[c] = p.max(0.0),
                        ColSpec::Fr(f) => frsum += f,
                        ColSpec::Auto => {
                            // Sized to the widest single word + insets; cells
                            // wrap, so multi-word text isn't measured whole.
                            let mut w = 0.0f64;
                            let mut i = c;
                            while i < cells.len() {
                                w = w.max(self.cell_word_w(&cells[i]));
                                i += ncols;
                            }
                            widths[c] = w + 2.0 * IX;
                        }
                    }
                    used += widths[c];
                }
                if frsum > 0.0 {
                    let rem = (total - used).max(0.0);
                    for (c, w) in widths.iter_mut().enumerate() {
                        if let Some(ColSpec::Fr(f)) = columns.get(c).copied() {
                            *w = rem * f / frsum;
                        }
                    }
                }
                let table_w: f64 = widths.iter().sum();
                let limit = self.st.page_height - self.st.margin_bottom;
                // Top edge of the next row: margin on a fresh page, else the
                // previous baseline plus the collapsed inter-block gap.
                let mut top = if self.fresh {
                    self.st.margin_top
                } else {
                    self.y + self.pending_gap
                };
                self.pending_gap = 0.0;
                let nrows = cells.len().div_ceil(ncols);
                for r in 0..nrows {
                    // Measure pass: row height = tallest cell.
                    let mut row_h = 2.0 * IY;
                    for c in 0..ncols {
                        let i = r * ncols + c;
                        if i >= cells.len() {
                            break;
                        }
                        let (toks, lines) =
                            self.cell_lines(&cells[i], (widths[c] - 2.0 * IX).max(0.0));
                        let mut h = 2.0 * IY;
                        for l in &lines {
                            h += self.lh(self.extents(&toks, *l, None).0);
                        }
                        row_h = row_h.max(h);
                    }
                    // Rows are atomic; an oversized row just overflows.
                    if !self.items.is_empty() && top + row_h > limit {
                        self.finish_page();
                        top = self.st.margin_top;
                    }
                    if *stroke {
                        // Grid lines once per row: the top edge here, the
                        // shared bottom edge after the last row (no doubled
                        // lines), column edges spanning this row's height.
                        self.items.push(DrawItem::Rule {
                            x: ml,
                            y: top,
                            w: table_w,
                            h: W,
                            gray: 0.0,
                        });
                        let mut cx = ml;
                        for c in 0..=ncols {
                            self.items.push(DrawItem::Rule {
                                x: cx,
                                y: top,
                                w: W,
                                h: row_h,
                                gray: 0.0,
                            });
                            if c < ncols {
                                cx += widths[c];
                            }
                        }
                    }
                    // Emit pass: cell text, left aligned, body line height.
                    let mut cx = ml;
                    for c in 0..ncols {
                        let i = r * ncols + c;
                        if i < cells.len() {
                            let (toks, lines) =
                                self.cell_lines(&cells[i], (widths[c] - 2.0 * IX).max(0.0));
                            let mut base = top + IY;
                            for (k, l) in lines.iter().enumerate() {
                                let (size, asc, _) = self.extents(&toks, *l, None);
                                base += if k == 0 { asc } else { self.lh(size) };
                                emit_line(
                                    &mut self.items,
                                    self.m,
                                    self.st,
                                    &toks,
                                    *l,
                                    cx + IX,
                                    base,
                                    0.0,
                                    size,
                                );
                            }
                        }
                        cx += widths[c];
                    }
                    top += row_h;
                }
                if *stroke {
                    self.items.push(DrawItem::Rule {
                        x: ml,
                        y: top,
                        w: table_w,
                        h: W,
                        gray: 0.0,
                    });
                }
                self.y = top;
                self.fresh = false;
            }
            Block::VSpace(v) => {
                self.pending_gap += self.after + v;
                self.after = 0.0;
            }
            Block::PageBreak => self.finish_page(),
            Block::Image { index, w, h } => {
                self.gap(0.0);
                let limit = self.st.page_height - self.st.margin_bottom;
                let mut top = if self.fresh {
                    self.st.margin_top
                } else {
                    self.y + self.pending_gap
                };
                self.pending_gap = 0.0;
                if !self.items.is_empty() && top + *h > limit {
                    self.finish_page();
                    top = self.st.margin_top;
                }
                self.items.push(DrawItem::Image {
                    x: ml,
                    y: top,
                    w: *w,
                    h: *h,
                    index: *index,
                });
                self.y = top + *h;
                self.fresh = false;
                self.after = self.st.par_spacing;
            }
            Block::Framed(_) | Block::Stack { .. } => {}
        }
    }

    /// Bullet/enum list. `marker_x` is where this level's markers sit.
    /// A nested list shifts by one hanging indent (marker width + 0.5em).
    fn lay_list(&mut self, ordered: bool, items: &'a [ListItem], marker_x: f64) {
        let ml = self.st.margin_left;
        let right = ml + self.text_w();
        for (i, it) in items.iter().enumerate() {
            let mk = if ordered {
                format!("{}.", i + 1)
            } else {
                "\u{2022}".to_string()
            };
            let mst = TextStyle {
                font: FontId::Regular,
                size: self.st.font_size,
            };
            // typst hangs the marker; item text starts after the marker
            // plus a 0.5em gap, and continuations align there.
            let ind = self.m.str_width(mst.font, mst.size, &mk) + 0.5 * mst.size;
            let text_x = marker_x + ind;
            let width = (right - text_x).max(0.0);
            self.flow(
                &it.content,
                text_x,
                width,
                HAlign::Left,
                false,
                Some((mk, mst, marker_x)),
                &ident,
            );
            if it.rest.is_empty() {
                continue;
            }
            for part in &it.rest {
                match part {
                    ListRest::List(list) => self.lay_list(list.ordered, &list.items, text_x),
                    ListRest::Inlines(content) => self.flow(
                        content,
                        text_x,
                        width,
                        HAlign::Left,
                        false,
                        None,
                        &ident,
                    ),
                }
            }
        }
    }

    /// Term in bold, description on the same line when the term fits.
    /// Continuations align under the description.
    fn lay_term(&mut self, term: &'a [Inline], desc: &'a [Inline], ml: f64, tw: f64) {
        let gap = 0.5 * self.st.font_size;
        let Some(term_w) = self.inline_width(term) else {
            self.flow(term, ml, tw, HAlign::Left, false, None, &ident);
            if !desc.is_empty() {
                self.flow(desc, ml, tw, HAlign::Left, false, None, &ident);
            }
            return;
        };
        let hang = if term.is_empty() { 0.0 } else { term_w + gap };
        if hang >= tw {
            if !term.is_empty() {
                self.flow(term, ml, tw, HAlign::Left, false, None, &ident);
            }
            if !desc.is_empty() {
                self.flow(desc, ml, tw, HAlign::Left, false, None, &ident);
            }
            return;
        }
        let mst = TextStyle {
            font: FontId::Bold,
            size: self.inline_size(term),
        };
        self.term_prefix = Some(term);
        self.flow(
            desc,
            ml + hang,
            tw - hang,
            HAlign::Left,
            false,
            Some((String::new(), mst, ml)),
            &ident,
        );
        self.term_prefix = None;
    }

    /// Width of a single-line inline run. `None` when it contains a break.
    fn inline_width(&self, inl: &[Inline]) -> Option<f64> {
        let mut w = 0.0;
        for i in inl {
            match i {
                Inline::Text { text, style } => {
                    if text.as_bytes().contains(&b'\n') {
                        return None;
                    }
                    w += self.m.str_width(style.font, style.size, text);
                }
                Inline::HSpace(g) => w += *g,
                Inline::Ref { name, style } => {
                    if name.as_bytes().contains(&b'\n') {
                        return None;
                    }
                    w += self.m.str_width(style.font, style.size, name);
                }
                Inline::LineBreak => return None,
                Inline::Frame(_) => {}
            }
        }
        Some(w)
    }

    fn inline_size(&self, inl: &[Inline]) -> f64 {
        let mut s = self.st.font_size;
        for i in inl {
            if let Inline::Text { style, .. } | Inline::Ref { style, .. } = i {
                s = s.max(style.size);
            }
        }
        s
    }

    fn paint_prefix(&mut self, runs: &[Inline], mut x: f64, baseline: f64) {
        for r in runs {
            match r {
                Inline::Text { text, style } => {
                    if text.is_empty() {
                        continue;
                    }
                    let w = self.m.str_width(style.font, style.size, text);
                    self.items.push(DrawItem::Text {
                        x,
                        y: baseline,
                        font: style.font,
                        size: style.size,
                        text: InlineStr::from(text.as_str()),
                    });
                    x += w;
                }
                Inline::HSpace(g) => x += *g,
                Inline::Ref { name, style } => {
                    if name.is_empty() {
                        continue;
                    }
                    let w = self.m.str_width(style.font, style.size, name);
                    self.items.push(DrawItem::Text {
                        x,
                        y: baseline,
                        font: style.font,
                        size: style.size,
                        text: InlineStr::from(name.as_str()),
                    });
                    x += w;
                }
                Inline::LineBreak => {}
                Inline::Frame(_) => {}
            }
        }
    }

    /// Lay out inline content one line at a time. The finished line is held
    /// back one step so justification can see which line is last.
    fn flow<F: Fn(TextStyle) -> TextStyle>(
        &mut self,
        content: &'a [Inline],
        x0: f64,
        width: f64,
        align: HAlign,
        justify: bool,
        mut marker: Option<(String, TextStyle, f64)>,
        xf: &F,
    ) {
        let mut cur = std::mem::take(&mut self.cur);
        let mut held = std::mem::take(&mut self.held);
        cur.clear();
        held.clear();
        let g = Geom {
            x0,
            width,
            align,
            justify,
        };
        let mut st = St {
            w: 0.0,
            spaces: 0,
            has: false,
            pend_w: 0.0,
            pend_n: 0,
        };
        let mut meta = Meta {
            on: false,
            w: 0.0,
            spaces: 0,
            hard: false,
        };
        for inl in content {
            match inl {
                Inline::Text { text, style } => {
                    self.feed_text(
                        text,
                        xf(*style),
                        &mut cur,
                        &mut held,
                        &mut st,
                        &mut meta,
                        &g,
                        &mut marker,
                    );
                }
                Inline::Ref { name, style } => {
                    self.feed_text(
                        name,
                        xf(*style),
                        &mut cur,
                        &mut held,
                        &mut st,
                        &mut meta,
                        &g,
                        &mut marker,
                    );
                }
                Inline::LineBreak => {
                    self.park(
                        true,
                        &mut cur,
                        &mut held,
                        &mut st,
                        &mut meta,
                        &g,
                        &mut marker,
                    );
                }
                Inline::HSpace(w) => {
                    self.push_gap(*w, &mut cur, &mut held, &mut st, &mut meta, &g, &mut marker);
                }
                Inline::Frame(_) => {}
            }
        }
        if st.has {
            self.park(
                false,
                &mut cur,
                &mut held,
                &mut st,
                &mut meta,
                &g,
                &mut marker,
            );
        }
        if meta.on {
            self.place_atoms(&held, meta.w, meta.spaces, meta.hard, &g, true, &mut marker);
        } else if marker.is_some() {
            self.place_atoms(&[], 0.0, 0, false, &g, true, &mut marker);
        }
        self.cur = cur;
        self.held = held;
    }

    fn feed_text(
        &mut self,
        text: &'a str,
        style: TextStyle,
        cur: &mut Vec<Atom<'a>>,
        held: &mut Vec<Atom<'a>>,
        st: &mut St,
        meta: &mut Meta,
        g: &Geom,
        marker: &mut Option<(String, TextStyle, f64)>,
    ) {
        let bytes = text.as_bytes();
        let scale = style.size / 1000.0;
        let lut = self.m.ascii_units(style.font);
        let space_w = match lut {
            Some(t) => t[b' ' as usize] as f64 * scale,
            None => self.m.char_width(style.font, ' ') * scale,
        };
        let mut i = 0;
        while i < bytes.len() {
            let nws = ws_run(text, i);
            if nws > 0 {
                i += nws;
                // A space before the first committed token stays in the line
                // (it shifts x) but is not part of the line width. Spaces
                // after a word stay pending until the next word fits.
                if !st.has {
                    if !matches!(cur.last(), Some(Atom::S(_))) {
                        cur.push(Atom::S(space_w));
                    }
                } else if st.pend_n == 0 {
                    st.pend_w = space_w;
                    st.pend_n = 1;
                }
                continue;
            }
            let start = i;
            let mut units = 0u32;
            let mut simple = lut.is_some();
            while i < bytes.len() && ws_run(text, i) == 0 {
                let b = bytes[i];
                if b < 0x80 {
                    if let Some(t) = lut {
                        units += t[b as usize] as u32;
                    }
                    i += 1;
                } else {
                    let c = text[i..].chars().next().unwrap();
                    simple = false;
                    i += c.len_utf8();
                }
            }
            let ww = if simple {
                units as f64 * scale
            } else {
                self.m.str_width(style.font, style.size, &text[start..i])
            };
            self.push_word(
                text,
                start as u32,
                i as u32,
                style,
                ww,
                cur,
                held,
                st,
                meta,
                g,
                marker,
            );
        }
    }

    fn push_word(
        &mut self,
        src: &'a str,
        a: u32,
        b: u32,
        style: TextStyle,
        ww: f64,
        cur: &mut Vec<Atom<'a>>,
        held: &mut Vec<Atom<'a>>,
        st: &mut St,
        meta: &mut Meta,
        g: &Geom,
        marker: &mut Option<(String, TextStyle, f64)>,
    ) {
        if st.has && st.w + st.pend_w + ww > g.width {
            st.pend_w = 0.0;
            st.pend_n = 0;
            self.park(false, cur, held, st, meta, g, marker);
        }
        if st.has && st.pend_n > 0 {
            cur.push(Atom::S(st.pend_w));
            st.w += st.pend_w;
            st.spaces += 1;
        }
        cur.push(Atom::W(src, a, b, style, ww));
        st.w += ww;
        st.has = true;
        st.pend_w = 0.0;
        st.pend_n = 0;
    }

    fn push_gap(
        &mut self,
        gw: f64,
        cur: &mut Vec<Atom<'a>>,
        held: &mut Vec<Atom<'a>>,
        st: &mut St,
        meta: &mut Meta,
        g: &Geom,
        marker: &mut Option<(String, TextStyle, f64)>,
    ) {
        if st.has && st.w + st.pend_w + gw > g.width {
            st.pend_w = 0.0;
            st.pend_n = 0;
            self.park(false, cur, held, st, meta, g, marker);
            return;
        }
        if st.has && st.pend_n > 0 {
            cur.push(Atom::S(st.pend_w));
            st.w += st.pend_w;
            st.spaces += 1;
        }
        cur.push(Atom::G(gw));
        st.w += gw;
        st.has = true;
        st.pend_w = 0.0;
        st.pend_n = 0;
    }

    /// Move the current line into the hold slot, placing the previous one.
    fn park(
        &mut self,
        hard: bool,
        cur: &mut Vec<Atom<'a>>,
        held: &mut Vec<Atom<'a>>,
        st: &mut St,
        meta: &mut Meta,
        g: &Geom,
        marker: &mut Option<(String, TextStyle, f64)>,
    ) {
        if !hard && !st.has {
            st.pend_w = 0.0;
            st.pend_n = 0;
            return;
        }
        // An empty hard line's range does not include a leading space.
        if !st.has {
            cur.clear();
        }
        if meta.on {
            self.place_atoms(held, meta.w, meta.spaces, meta.hard, g, false, marker);
        }
        std::mem::swap(cur, held);
        cur.clear();
        meta.w = st.w;
        meta.spaces = st.spaces;
        meta.hard = hard;
        meta.on = true;
        st.w = 0.0;
        st.spaces = 0;
        st.has = false;
        st.pend_w = 0.0;
        st.pend_n = 0;
    }

    fn place_atoms(
        &mut self,
        atoms: &[Atom],
        line_w: f64,
        spaces: u32,
        hard: bool,
        g: &Geom,
        last: bool,
        marker: &mut Option<(String, TextStyle, f64)>,
    ) {
        let mk = marker.take();
        let (size, asc, desc) = self.atom_extents(atoms, mk.as_ref().map(|m| m.1));
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
        let mut extra = 0.0;
        if g.justify && !last && !hard && spaces > 0 && line_w < g.width {
            let e = (g.width - line_w) / spaces as f64;
            if atoms.iter().all(|t| !matches!(t, Atom::S(s) if *s < e)) {
                extra = e;
            }
        }
        let slack = (g.width - line_w).max(0.0);
        let x = g.x0
            + match g.align {
                HAlign::Center => slack / 2.0,
                HAlign::Right => slack,
                HAlign::Left => 0.0,
            };
        if let Some((mt, mst, mx)) = mk {
            if !mt.is_empty() {
                self.items.push(DrawItem::Text {
                    x: mx,
                    y: baseline,
                    font: mst.font,
                    size: mst.size,
                    text: InlineStr::from(mt.as_str()),
                });
            }
            // Term lists only. Bullet markers take the branch above;
            // `term_prefix` stays `None` for them.
            if let Some(pref) = self.term_prefix.take() {
                self.paint_prefix(pref, mx, baseline);
            }
        }
        emit_atoms(&mut self.items, atoms, x, baseline, extra);
        self.y = baseline;
        self.fresh = false;
    }

    fn atom_extents(&self, atoms: &[Atom], marker: Option<TextStyle>) -> (f64, f64, f64) {
        let mut size = 0.0f64;
        let mut asc = 0.0f64;
        let mut desc = 0.0f64;
        let mut extent = |st: TextStyle| {
            size = size.max(st.size);
            asc = asc.max(self.m.ascent(st.font) * st.size / 1000.0);
            desc = desc.max(self.m.descent(st.font) * st.size / 1000.0);
        };
        for a in atoms {
            if let Atom::W(_, _, _, st, _) = a {
                extent(*st);
            }
        }
        if let Some(st) = marker {
            extent(st);
        }
        if size == 0.0 {
            size = self.st.font_size;
            asc = self.m.ascent(FontId::Regular) * size / 1000.0;
            desc = self.m.descent(FontId::Regular) * size / 1000.0;
        }
        (size, asc, desc)
    }

    /// (size, ascent, descent) of a line's tallest run, in pt. Falls back to
    /// the body font when the line has no word tokens.
    fn extents(&self, toks: &[Tok], l: Line, marker: Option<TextStyle>) -> (f64, f64, f64) {
        let mut size = 0.0f64;
        let mut asc = 0.0f64;
        let mut desc = 0.0f64;
        let mut extent = |st: &TextStyle| {
            size = size.max(st.size);
            asc = asc.max(self.m.ascent(st.font) * st.size / 1000.0);
            desc = desc.max(self.m.descent(st.font) * st.size / 1000.0);
        };
        for t in &toks[l.start..l.end] {
            if let Tok::W(_, st, _) = t {
                extent(st);
            }
        }
        if let Some(st) = &marker {
            extent(st);
        }
        if size == 0.0 {
            size = self.st.font_size;
            asc = self.m.ascent(FontId::Regular) * size / 1000.0;
            desc = self.m.descent(FontId::Regular) * size / 1000.0;
        }
        (size, asc, desc)
    }

    /// Tokenize and wrap a table cell at the given width.
    fn cell_lines(&self, cell: &'a [Inline], width: f64) -> (Vec<Tok<'a>>, Vec<Line>) {
        let mut toks = Vec::new();
        tokenize(&mut toks, cell, self.m, &ident);
        let mut lines = Vec::new();
        break_lines(&toks, width, &mut lines);
        (toks, lines)
    }

    /// Widest single word in a cell — the `Auto` column measure.
    fn cell_word_w(&self, cell: &[Inline]) -> f64 {
        let mut w = 0.0f64;
        for i in cell {
            match i {
                Inline::Text { text, style } => {
                    for wd in text.split_whitespace() {
                        w = w.max(self.m.str_width(style.font, style.size, wd));
                    }
                }
                Inline::Ref { name, style } => {
                    for wd in name.split_whitespace() {
                        w = w.max(self.m.str_width(style.font, style.size, wd));
                    }
                }
                Inline::HSpace(g) => w = w.max(*g),
                Inline::LineBreak => {}
                Inline::Frame(_) => {}
            }
        }
        w
    }
}

/// Length of a whitespace run at `i`, or 0 when `i` is not whitespace.
#[inline(always)]
fn ws_run(text: &str, i: usize) -> usize {
    let bytes = text.as_bytes();
    let mut j = i;
    while j < bytes.len() {
        let b = bytes[j];
        if b < 0x80 {
            if byte_is_ws(b) {
                j += 1;
            } else {
                break;
            }
        } else {
            let c = text[j..].chars().next().unwrap();
            if c.is_whitespace() {
                j += c.len_utf8();
            } else {
                break;
            }
        }
    }
    j - i
}

/// Emit one line of atoms. Same-style words separated by a single source
/// space become one draw item copied from that parent slice.
fn emit_atoms(items: &mut Vec<DrawItem>, atoms: &[Atom], mut x: f64, baseline: f64, extra: f64) {
    let mut sep = Sep::Brk;
    let mut buf = InlineStr::new();
    let mut bx = 0.0;
    let mut bs = TextStyle {
        font: FontId::Regular,
        size: 0.0,
    };
    let mut has = false;
    let mut span_src: Option<&str> = None;
    let mut span_a = 0usize;
    let mut span_b = 0usize;
    let mut span_ok = false;

    for t in atoms {
        match *t {
            Atom::W(src, a, b, st, wid) => {
                let a = a as usize;
                let b = b as usize;
                if has && bs == st && sep != Sep::Brk {
                    let extend = span_ok
                        && std::ptr::eq(span_src.unwrap(), src)
                        && ((sep == Sep::Sp && a == span_b + 1 && src.as_bytes()[span_b] == b' ')
                            || (sep == Sep::Cat && a == span_b));
                    if extend {
                        span_b = b;
                    } else {
                        if span_ok {
                            buf = InlineStr::from(&span_src.unwrap()[span_a..span_b]);
                            span_ok = false;
                        }
                        if sep == Sep::Sp {
                            buf.push_str(" ");
                        }
                        buf.push_str(&src[a..b]);
                    }
                } else {
                    if has {
                        items.push(DrawItem::Text {
                            x: bx,
                            y: baseline,
                            font: bs.font,
                            size: bs.size,
                            text: take_run(&mut buf, span_ok, span_src, span_a, span_b),
                        });
                    }
                    bx = x;
                    bs = st;
                    span_src = Some(src);
                    span_a = a;
                    span_b = b;
                    span_ok = true;
                    has = true;
                }
                x += wid;
                sep = Sep::Cat;
            }
            Atom::S(nat) => {
                x += if extra > 0.0 { nat + extra } else { nat };
                sep = if extra > 0.0 { Sep::Brk } else { Sep::Sp };
            }
            Atom::G(g) => {
                x += g;
                sep = Sep::Brk;
            }
        }
    }
    if has {
        items.push(DrawItem::Text {
            x: bx,
            y: baseline,
            font: bs.font,
            size: bs.size,
            text: take_run(&mut buf, span_ok, span_src, span_a, span_b),
        });
    }
}

fn take_run(
    buf: &mut InlineStr,
    span_ok: bool,
    span_src: Option<&str>,
    span_a: usize,
    span_b: usize,
) -> InlineStr {
    if span_ok {
        InlineStr::from(&span_src.unwrap()[span_a..span_b])
    } else {
        std::mem::take(buf)
    }
}

/// Emit merged draw runs for one line of tokens starting at x on `baseline`.
/// Adjacent same-style words merge into a single `DrawItem::Text`; `extra`
/// is per-space justification stretch (0 for ragged-right).
fn emit_line<M: FontMetrics>(
    items: &mut Vec<DrawItem>,
    _m: &M,
    _styles: &Styles,
    toks: &[Tok],
    l: Line,
    mut x: f64,
    baseline: f64,
    extra: f64,
    size: f64,
) {
    let mut sep = Sep::Brk;
    let mut buf = InlineStr::new();
    let mut bx = 0.0;
    let mut bs = TextStyle {
        font: FontId::Regular,
        size,
    };
    let mut has = false;
    for t in &toks[l.start..l.end] {
        match *t {
            Tok::W(w, st, wid) => {
                if has && bs == st && sep != Sep::Brk {
                    if sep == Sep::Sp {
                        buf.push_str(" ");
                    }
                    buf.push_str(w);
                } else {
                    if has {
                        items.push(DrawItem::Text {
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
        items.push(DrawItem::Text {
            x: bx,
            y: baseline,
            font: bs.font,
            size: bs.size,
            text: buf,
        });
    }
}

/// ASCII bytes that `char::is_whitespace` treats as whitespace.
#[inline(always)]
fn byte_is_ws(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0c)
}

fn push_space<'a>(toks: &mut Vec<Tok<'a>>, space_w: f64) {
    if !matches!(toks.last(), Some(Tok::S(_))) {
        toks.push(Tok::S(space_w));
    }
}

/// Word widths from a byte LUT. The font match happened once, outside.
fn push_text_lut<'a, M: FontMetrics>(
    toks: &mut Vec<Tok<'a>>,
    text: &'a str,
    st: TextStyle,
    lut: &[u16; 128],
    m: &M,
) {
    let scale = st.size / 1000.0;
    let space_w = lut[b' ' as usize] as f64 * scale;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 && byte_is_ws(b) {
            push_space(toks, space_w);
            i += 1;
            continue;
        }
        if b >= 0x80 {
            let c = text[i..].chars().next().unwrap();
            if c.is_whitespace() {
                push_space(toks, space_w);
                i += c.len_utf8();
                continue;
            }
        }
        let start = i;
        let mut units = 0u32;
        let mut simple = true;
        while i < bytes.len() {
            let b = bytes[i];
            if b < 0x80 {
                if byte_is_ws(b) {
                    break;
                }
                units += lut[b as usize] as u32;
                i += 1;
            } else {
                let c = text[i..].chars().next().unwrap();
                if c.is_whitespace() {
                    break;
                }
                simple = false;
                i += c.len_utf8();
            }
        }
        let w = &text[start..i];
        let width = if simple {
            units as f64 * scale
        } else {
            m.str_width(st.font, st.size, w)
        };
        toks.push(Tok::W(w, st, width));
    }
}

fn push_text_slow<'a, M: FontMetrics>(
    toks: &mut Vec<Tok<'a>>,
    text: &'a str,
    st: TextStyle,
    m: &M,
) {
    let scale = st.size / 1000.0;
    let space_w = m.char_width(st.font, ' ') * scale;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        if b < 0x80 && byte_is_ws(b) {
            push_space(toks, space_w);
            i += 1;
            continue;
        }
        if b >= 0x80 {
            let c = text[i..].chars().next().unwrap();
            if c.is_whitespace() {
                push_space(toks, space_w);
                i += c.len_utf8();
                continue;
            }
        }
        let start = i;
        while i < bytes.len() {
            let b = bytes[i];
            if b < 0x80 {
                if byte_is_ws(b) {
                    break;
                }
                i += 1;
            } else {
                let c = text[i..].chars().next().unwrap();
                if c.is_whitespace() {
                    break;
                }
                i += c.len_utf8();
            }
        }
        let w = &text[start..i];
        toks.push(Tok::W(w, st, m.str_width(st.font, st.size, w)));
    }
}

/// Split inline content into word/space tokens; words get `xf`-mapped styles.
/// One forward scan per run (first-fit, Θ(n)); width is one table load per byte.
fn tokenize<'a, M: FontMetrics, F: Fn(TextStyle) -> TextStyle>(
    toks: &mut Vec<Tok<'a>>,
    content: &'a [Inline],
    m: &M,
    xf: &F,
) {
    for i in content {
        match i {
            Inline::Text { text, style } => {
                let st = xf(*style);
                if let Some(lut) = m.ascii_units(st.font) {
                    push_text_lut(toks, text, st, lut, m);
                } else {
                    push_text_slow(toks, text, st, m);
                }
            }
            Inline::Ref { name, style } => {
                let st = xf(*style);
                if let Some(lut) = m.ascii_units(st.font) {
                    push_text_lut(toks, name, st, lut, m);
                } else {
                    push_text_slow(toks, name, st, m);
                }
            }
            Inline::LineBreak => toks.push(Tok::B),
            Inline::HSpace(w) => toks.push(Tok::G(*w)),
            Inline::Frame(_) => {}
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
                    lines.push(Line {
                        start,
                        end,
                        w,
                        spaces: sp,
                        hard: false,
                    });
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
                    lines.push(Line {
                        start,
                        end,
                        w,
                        spaces: sp,
                        hard: false,
                    });
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
                lines.push(Line {
                    start,
                    end,
                    w,
                    spaces: sp,
                    hard: true,
                });
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
        lines.push(Line {
            start,
            end,
            w,
            spaces: sp,
            hard: false,
        });
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
        Document {
            styles: Styles::default(),
            blocks,
            diags: vec![],
            fonts: vec![],
            images: vec![],
        }
    }

    fn para(words: usize) -> Block {
        let text = vec!["aaaa"; words].join(" ");
        Block::Paragraph {
            content: vec![Inline::Text {
                text,
                style: TextStyle {
                    font: FontId::Regular,
                    size: 11.0,
                },
            }],
            align: HAlign::Left,
        }
    }

    fn texts(p: &Page) -> Vec<(f64, f64, &str, FontId, f64)> {
        p.items
            .iter()
            .filter_map(|i| match i {
                DrawItem::Text {
                    x,
                    y,
                    font,
                    size,
                    text,
                } => Some((*x, *y, text.as_str(), *font, *size)),
                _ => None,
            })
            .collect()
    }

    fn rules(p: &Page) -> Vec<(f64, f64, f64, f64, f64)> {
        p.items
            .iter()
            .filter_map(|i| match i {
                DrawItem::Rule { x, y, w, h, gray } => Some((*x, *y, *w, *h, *gray)),
                _ => None,
            })
            .collect()
    }

    fn lit(text: &str, font: FontId) -> Inline {
        Inline::Text {
            text: text.into(),
            style: TextStyle { font, size: 11.0 },
        }
    }

    fn flat(text: &str) -> ListItem {
        ListItem {
            content: vec![lit(text, FontId::Regular)],
            rest: Vec::new(),
        }
    }

    fn cell(t: &str) -> Vec<Inline> {
        vec![Inline::Text {
            text: t.into(),
            style: TextStyle {
                font: FontId::Regular,
                size: 11.0,
            },
        }]
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
                    style: TextStyle {
                        font: FontId::Regular,
                        size: 11.0,
                    },
                }],
            },
            Block::List {
                ordered: true,
                items: vec![flat("first"), flat("second")],
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
    fn layout_nested_and_mixed_lists() {
        let doc = tdoc(vec![Block::List {
            ordered: false,
            items: vec![ListItem {
                content: vec![lit("parent", FontId::Regular)],
                rest: vec![ListRest::List(Box::new(ListBody {
                    ordered: false,
                    items: vec![flat("child")],
                }))],
            }],
        }]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        let parent_m = items.iter().find(|i| i.2 == "\u{2022}").unwrap();
        let parent_t = items.iter().find(|i| i.2 == "parent").unwrap();
        assert!((parent_m.0 - 70.866).abs() < 1e-6);
        // bullet 5.5pt + 0.5em 5.5pt
        assert!((parent_t.0 - 70.866 - 11.0).abs() < 1e-6);
        assert_eq!(parent_m.1, parent_t.1);
        let child_m = items.iter().filter(|i| i.2 == "\u{2022}").nth(1).unwrap();
        let child_t = items.iter().find(|i| i.2 == "child").unwrap();
        assert!((child_m.0 - parent_t.0).abs() < 1e-6);
        assert!((child_t.0 - child_m.0 - 11.0).abs() < 1e-6);
        assert!((child_m.1 - parent_m.1 - 14.39).abs() < 1e-6);

        // mixed: nested enum under a bullet. "1." is 11pt + 0.5em.
        let doc = tdoc(vec![Block::List {
            ordered: false,
            items: vec![ListItem {
                content: vec![lit("a", FontId::Regular)],
                rest: vec![ListRest::List(Box::new(ListBody {
                    ordered: true,
                    items: vec![flat("b")],
                }))],
            }],
        }]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        let bullet = items.iter().find(|i| i.2 == "\u{2022}").unwrap();
        let num = items.iter().find(|i| i.2 == "1.").unwrap();
        let b = items.iter().find(|i| i.2 == "b").unwrap();
        assert!((num.0 - bullet.0 - 11.0).abs() < 1e-6);
        assert!((b.0 - num.0 - 16.5).abs() < 1e-6);
        assert_eq!(num.1, b.1);
    }

    #[test]
    fn layout_term_list() {
        let term = vec![lit("Term", FontId::Bold)];
        let desc = vec![lit(
            &vec!["aaaa"; 20].join(" "),
            FontId::Regular,
        )];
        // emphasis in the description stays italic and shares the line
        let emph = vec![
            lit("a ", FontId::Regular),
            lit("b", FontId::Italic),
        ];
        let doc = tdoc(vec![
            Block::Terms {
                items: vec![TermItem {
                    term: term.clone(),
                    desc: emph,
                }],
            },
            Block::Terms {
                items: vec![TermItem { term, desc }],
            },
        ]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        let term_i = items.iter().find(|i| i.2 == "Term").unwrap();
        assert_eq!(term_i.3, FontId::Bold);
        assert!((term_i.0 - 70.866).abs() < 1e-6);
        // "Term" = 22pt, gap 0.5em = 5.5, description at +27.5
        let a = items.iter().find(|i| i.2 == "a").unwrap();
        assert!((a.0 - 70.866 - 27.5).abs() < 1e-3);
        assert_eq!(a.1, term_i.1);
        let b = items.iter().find(|i| i.2 == "b").unwrap();
        assert_eq!(b.3, FontId::Italic);
        assert_eq!(b.1, a.1);
        assert!((b.0 - a.0 - 11.0).abs() < 1e-3); // "a" + space
        // wrapped description hangs at the same x as the first desc line
        let lines: Vec<_> = items.iter().filter(|i| i.2.contains("aaaa")).collect();
        assert!(lines.len() >= 2, "{lines:?}");
        assert!((lines[0].0 - 70.866 - 27.5).abs() < 1e-3);
        assert!((lines[1].0 - lines[0].0).abs() < 1e-6);
        assert!((lines[1].1 - lines[0].1 - 14.39).abs() < 1e-6);
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
                style: TextStyle {
                    font: FontId::Regular,
                    size: 11.0,
                },
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
            Block::RawBlock {
                text: "code\ntwo".into(),
            },
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
                    style: TextStyle {
                        font: FontId::Regular,
                        size: 11.0,
                    },
                },
                Inline::HSpace(50.0),
                Inline::Text {
                    text: "bb".into(),
                    style: TextStyle {
                        font: FontId::Regular,
                        size: 11.0,
                    },
                },
                Inline::LineBreak,
                Inline::Text {
                    text: "cc".into(),
                    style: TextStyle {
                        font: FontId::Regular,
                        size: 11.0,
                    },
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

    // Table geometry with Fake metrics: cell inset 5/4pt, asc = 7.898,
    // lh = 14.39 -> one-line row height = 22.39, first baseline inside a
    // row = row_top + 4 + 7.898.

    #[test]
    fn table_fixed_columns() {
        let doc = tdoc(vec![
            Block::Table {
                columns: vec![ColSpec::Fixed(100.0), ColSpec::Fixed(200.0)],
                cells: vec![cell("aaaa"), cell("bbbb"), cell("cccc"), cell("dddd")],
                stroke: false,
            },
            para(1),
        ]);
        let pages = layout(&doc, &Fake);
        assert_eq!(pages.len(), 1);
        let items = texts(&pages[0]);
        assert_eq!(items.len(), 5);
        assert!(rules(&pages[0]).is_empty());
        // first baseline of row 1: margin_top + inset + ascent
        assert!((items[0].1 - (70.866 + 4.0 + 7.898)).abs() < 1e-3);
        // col 0 text at margin + inset; col 1 at margin + 100 + inset
        assert!((items[0].0 - (70.866 + 5.0)).abs() < 1e-6);
        assert!((items[1].0 - (70.866 + 100.0 + 5.0)).abs() < 1e-6);
        assert_eq!(items[1].1, items[0].1); // same row, same baseline
                                            // row 2 starts one row height (lh + 2*inset) lower
        assert!((items[2].1 - items[0].1 - (14.39 + 8.0)).abs() < 1e-6);
        assert!((items[3].0 - (70.866 + 100.0 + 5.0)).abs() < 1e-6);
        // paragraph after the table: table bottom + lh + par_spacing
        let bottom = 70.866 + 2.0 * 22.39;
        assert!((items[4].1 - (bottom + 14.39 + 6.05)).abs() < 1e-3);
    }

    #[test]
    fn table_auto_and_fr_columns() {
        // "bbbbbb" = 33pt is the widest word -> auto col = 33 + 2*5 = 43.
        // The fr column takes the rest: 453.544 - 43 = 410.544.
        let doc = tdoc(vec![Block::Table {
            columns: vec![ColSpec::Auto, ColSpec::Fr(1.0)],
            cells: vec![cell("aaaa bbbbbb"), cell("cc")],
            stroke: false,
        }]);
        let pages = layout(&doc, &Fake);
        let items = texts(&pages[0]);
        assert_eq!(items.len(), 3); // cell 0 wraps to "aaaa" / "bbbbbb"
        let cc = items.iter().find(|i| i.2 == "cc").unwrap();
        assert!((cc.0 - (70.866 + 43.0 + 5.0)).abs() < 1e-3);
        let a = items.iter().find(|i| i.2 == "aaaa").unwrap();
        let b = items.iter().find(|i| i.2 == "bbbbbb").unwrap();
        assert!((a.0 - (70.866 + 5.0)).abs() < 1e-6);
        assert!((b.1 - a.1 - 14.39).abs() < 1e-6); // wrapped, same column
        assert_eq!(cc.1, a.1); // cells share the row's first baseline
    }

    #[test]
    fn table_paginates_rows() {
        // 40 single-line rows of 22.39pt; page fits 31 -> 31 + 9 rows.
        let doc = tdoc(vec![Block::Table {
            columns: vec![ColSpec::Fixed(200.0)],
            cells: (0..40).map(|_| cell("aaaa")).collect(),
            stroke: false,
        }]);
        let pages = layout(&doc, &Fake);
        assert_eq!(pages.len(), 2);
        assert_eq!(texts(&pages[0]).len(), 31);
        let p2 = texts(&pages[1]);
        assert_eq!(p2.len(), 9);
        // continued rows start at the top margin again
        assert!((p2[0].1 - (70.866 + 4.0 + 7.898)).abs() < 1e-3);
    }

    #[test]
    fn table_stroke_rules() {
        let doc = tdoc(vec![Block::Table {
            columns: vec![ColSpec::Fixed(100.0), ColSpec::Fixed(200.0)],
            cells: vec![cell("aaaa"), cell("bbbb"), cell("cccc"), cell("dddd")],
            stroke: true,
        }]);
        let pages = layout(&doc, &Fake);
        let rs = rules(&pages[0]);
        // per row: 1 horizontal top edge + 3 column edges; +1 bottom edge.
        assert_eq!(rs.len(), 9);
        assert!(rs.iter().all(|r| r.4 == 0.0));
        // table top edge spans the full 300pt
        assert!(rs.iter().any(|r| (r.0 - 70.866).abs() < 1e-6
            && (r.1 - 70.866).abs() < 1e-6
            && (r.2 - 300.0).abs() < 1e-6
            && (r.3 - 0.5).abs() < 1e-6));
        // column boundaries at margin + 0 / 100 / 300, spanning row height
        for x in [70.866, 170.866, 370.866] {
            assert!(rs.iter().any(|r| (r.0 - x).abs() < 1e-6
                && (r.1 - 70.866).abs() < 1e-6
                && (r.2 - 0.5).abs() < 1e-6
                && (r.3 - 22.39).abs() < 1e-3));
        }
        // shared bottom edge drawn once below the last row
        let bottom = 70.866 + 2.0 * 22.39;
        assert!(rs
            .iter()
            .any(|r| (r.1 - bottom).abs() < 1e-3 && (r.2 - 300.0).abs() < 1e-6));
    }
}
