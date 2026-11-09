//! Markup parser: source text -> Vec<Node> (see ir.rs / SPEC.md).
use crate::ir::{
    Arg, ArithOp, Cmp, CmpOp, Cond, Diag, Expr, Length, Node, Param, Rhs, RhsArg, Severity, Span,
    TermEntry, Value,
};

/// `#name` calls the parser accepts. Anything else (bound `#let` names
/// aside) is a `not implemented` diagnostic.
const IMPLEMENTED: &[&str] = &[
    "lorem",
    "pagebreak",
    "v",
    "h",
    "linebreak",
    "strong",
    "emph",
    "text",
    "align",
    "table",
    "grid",
    "image",
    "box",
    "block",
    "stack",
    "figure",
    "outline",
    "link",
];

/// Bare `#word` forms that are recognized but not implemented (rather
/// than treated as variable references, which unknown `#ident`s become).
const NOT_IMPL_BARE: &[&str] = &[
    "show", "context", "state", "counter", "place", "ref", "else", "in", "not",
    "and", "or", "return", "break", "continue",
];

/// Parse Typst markup. Recovering: returns whatever nodes parsed plus
/// diagnostics; never panics on bad input.
pub fn parse(src: &str) -> (Vec<Node>, Vec<Diag>) {
    let mut p = P {
        src,
        bytes: src.as_bytes(),
        pos: 0,
        diags: Vec::new(),
        lets: Vec::new(),
        star: false,
        depth: 0,
    };
    let nodes = p.body(false, 0);
    (nodes, p.diags)
}

struct P<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    diags: Vec<Diag>,
    lets: Vec<String>,
    /// `#import "...": *` was seen. Later `#name(...)` may be a user
    /// function; the imported names are not known at parse time.
    star: bool,
    depth: u32,
}

/// Bytes that can end a `*...*` / `_..._` scan, not counting the marker
/// itself. The marker is tested beside the table.
const INTERESTING: [u8; 256] = {
    let mut t = [0u8; 256];
    t[b'\n' as usize] = 1;
    t[b'\\' as usize] = 1;
    t[b'`' as usize] = 1;
    t[b'/' as usize] = 1;
    t[b']' as usize] = 1;
    t
};

/// Printable ASCII and space, excluding markup sigils. `]` is excluded so
/// a content block still stops on the closer instead of swallowing it.
const fn plain_byte(c: u8) -> bool {
    c == b' '
        || (c > b' '
            && c < 0x7f
            && !matches!(
                c,
                b'*' | b'_' | b'`' | b'\\' | b'#' | b'$' | b'<' | b'@' | b'/' | b']'
            ))
}

const PLAIN: [u8; 256] = {
    let mut t = [0u8; 256];
    let mut c = 0;
    while c < 256 {
        if plain_byte(c as u8) {
            t[c] = 1;
        }
        c += 1;
    }
    t
};

fn flush(out: &mut Vec<Node>, text: &mut String) {
    if !text.is_empty() {
        out.push(Node::Text(std::mem::take(text)));
    }
}

/// Paragraph continuation: a single newline becomes a space.
fn pad(t: &mut String, nodes: &[Node]) {
    if !t.is_empty() {
        if !t.ends_with(' ') {
            t.push(' ');
        }
    } else if !nodes.is_empty() && !matches!(nodes.last(), Some(Node::LineBreak)) {
        t.push(' ');
    }
}

fn is_label(c: u8) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.')
}

fn is_ident_start(c: u8) -> bool {
    c == b'_' || c.is_ascii_alphabetic()
}

fn is_ident_cont(c: u8) -> bool {
    c == b'_' || c == b'-' || c.is_ascii_alphanumeric()
}

/// The name an `#import "spec"` module binds under: file stem for paths,
/// package name for `@ns/name:ver`.
fn spec_stem(spec: &str) -> String {
    if spec.starts_with('@') {
        spec.rsplit('/')
            .next()
            .and_then(|s| s.split(':').next())
            .unwrap_or("module")
            .to_string()
    } else {
        spec.rsplit('/')
            .next()
            .and_then(|s| s.rsplit_once('.').map(|(s, _)| s).or(Some(s)))
            .unwrap_or("module")
            .to_string()
    }
}

/// Dotted name of a Var/Field rhs (`a.b.c`).
fn rhs_name(r: &Rhs) -> Option<String> {
    match r {
        Rhs::Var(n) => Some(n.clone()),
        Rhs::Field { base, name } => Some(format!("{}.{}", rhs_name(base)?, name)),
        _ => None,
    }
}

/// `Rhs` -> markup `Expr` for `#(expr)`: Var/Field collapse to a dotted
/// `Expr::Var` (eval resolves fields through dicts), Bin/Lit map over.
/// A method call (or a field hanging off one) stays an `Expr::Rhs`.
fn rhs_expr(r: Rhs) -> Expr {
    match r {
        Rhs::Lit(v) => Expr::Lit(v),
        Rhs::Bin { lhs, op, rhs } => Expr::Bin {
            lhs: *lhs,
            op,
            rhs: *rhs,
        },
        Rhs::Var(name) => Expr::Var { name },
        other => match rhs_name(&other) {
            Some(name) => Expr::Var { name },
            None => Expr::Rhs(other),
        },
    }
}

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
    fn at(&self, k: usize) -> Option<u8> {
        self.bytes.get(self.pos + k).copied()
    }
    fn eof(&self) -> bool {
        self.pos >= self.src.len()
    }
    fn span(&self, a: usize) -> Span {
        Span::new(a, self.pos)
    }
    fn diag(&mut self, sev: Severity, a: usize, msg: impl Into<String>) {
        self.diags.push(Diag::new(sev, Some(self.span(a)), msg));
    }
    fn adv(&mut self) {
        self.pos += self.src[self.pos..]
            .chars()
            .next()
            .map_or(1, |c| c.len_utf8());
    }
    fn skip_hspace(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t') | Some(b'\r')) {
            self.pos += 1;
        }
    }
    fn line_comment(&mut self) {
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }
    }
    fn block_comment(&mut self) {
        let a = self.pos;
        self.pos += 2;
        let mut d = 1u32;
        while let Some(c) = self.peek() {
            if c == b'/' && self.at(1) == Some(b'*') {
                d += 1;
                self.pos += 2;
            } else if c == b'*' && self.at(1) == Some(b'/') {
                self.pos += 2;
                d -= 1;
                if d == 0 {
                    return;
                }
            } else {
                self.adv();
            }
        }
        self.diag(Severity::Warning, a, "unclosed block comment");
    }
    fn skip_to_eol(&mut self) {
        while !matches!(self.peek(), None | Some(b'\n')) {
            self.pos += 1;
        }
    }
    /// Whitespace + comments, inside `#(..)` argument lists.
    fn skip_ws(&mut self) {
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') | Some(b'\n') => self.pos += 1,
                Some(b'/') if self.at(1) == Some(b'/') => self.line_comment(),
                Some(b'/') if self.at(1) == Some(b'*') => self.block_comment(),
                _ => break,
            }
        }
    }
    /// Inter-line gap: spaces, newlines, comments. Returns newline count.
    fn gap(&mut self) -> usize {
        let mut nls = 0;
        loop {
            match self.peek() {
                Some(b' ') | Some(b'\t') | Some(b'\r') => self.pos += 1,
                Some(b'\n') => {
                    nls += 1;
                    self.pos += 1;
                }
                Some(b'/') if self.at(1) == Some(b'/') => self.line_comment(),
                Some(b'/') if self.at(1) == Some(b'*') => self.block_comment(),
                _ => break,
            }
        }
        nls
    }

    /// Does a block construct start at byte offset `i` (line start)?
    fn block_at(&self, i: usize) -> bool {
        let s = self.bytes;
        let at = |k: usize| s.get(i + k).copied();
        match s.get(i).copied() {
            Some(b'=') => {
                let mut n = 0;
                while at(n) == Some(b'=') {
                    n += 1;
                }
                n <= 6 && matches!(at(n), None | Some(b' ') | Some(b'\t') | Some(b'\n'))
            }
            Some(b'-') | Some(b'+') | Some(b'/') => matches!(at(1), Some(b' ') | Some(b'\t')),
            Some(b'`') => at(1) == Some(b'`') && at(2) == Some(b'`'),
            Some(c) if c.is_ascii_digit() => {
                let mut n = 0;
                while at(n).map_or(false, |c| c.is_ascii_digit()) {
                    n += 1;
                }
                matches!(at(n), Some(b'.') | Some(b')'))
                    && matches!(at(n + 1), Some(b' ') | Some(b'\t'))
            }
            _ => false,
        }
    }
    fn starts_block(&self) -> bool {
        self.block_at(self.pos)
    }

    /// Parse a stream of block + inline nodes. `in_bracket` stops at the
    /// matching `]` (consumed); `bstart` is its `[` offset for diagnostics.
    fn body(&mut self, in_bracket: bool, bstart: usize) -> Vec<Node> {
        self.depth += 1;
        if self.depth > 32 {
            self.depth -= 1;
            self.diag(Severity::Error, bstart, "content nested too deeply");
            self.skip_balanced(b'[', b']');
            return Vec::new();
        }
        let mut out: Vec<Node> = Vec::new();
        let mut text = String::with_capacity(128);
        let mut in_para = false;
        let mut first = true;
        loop {
            let nls = self.gap();
            if in_bracket && self.peek() == Some(b']') {
                self.pos += 1;
                break;
            }
            if self.eof() {
                if in_bracket {
                    self.diag(Severity::Warning, bstart, "unclosed `[`");
                }
                break;
            }
            let line_start = first || nls > 0;
            first = false;
            let cont = in_para && nls == 1 && !self.starts_block();
            if nls >= 2 {
                flush(&mut out, &mut text);
                if !out.is_empty() && !matches!(out.last(), Some(Node::Parbreak)) {
                    out.push(Node::Parbreak);
                }
            } else if cont {
                pad(&mut text, &out);
            }
            in_para = false;
            // `cont` already proved this line is not a block start.
            if line_start && !cont && self.block(&mut out, &mut text, in_bracket) {
                continue;
            }
            self.inline(&mut out, &mut text, in_bracket, None);
            in_para = true;
        }
        self.depth -= 1;
        flush(&mut out, &mut text);
        out
    }

    /// One block construct at line start. Returns false if none matched.
    fn block(&mut self, out: &mut Vec<Node>, text: &mut String, in_bracket: bool) -> bool {
        match self.peek() {
            Some(b'=') if self.starts_block() => {
                let mut n = 0u8;
                while self.peek() == Some(b'=') {
                    n += 1;
                    self.pos += 1;
                }
                self.skip_hspace();
                let mut body = Vec::new();
                let mut t = String::new();
                self.inline(&mut body, &mut t, in_bracket, None);
                flush(&mut body, &mut t);
                flush(out, text);
                out.push(Node::Heading { level: n, body });
                true
            }
            Some(b'`') if self.at(1) == Some(b'`') && self.at(2) == Some(b'`') => {
                self.fence(out, text);
                true
            }
            Some(m @ (b'-' | b'+')) if matches!(self.at(1), Some(b' ') | Some(b'\t')) => {
                self.list(out, text, m == b'+', in_bracket, 0, 1);
                true
            }
            Some(b'/') if matches!(self.at(1), Some(b' ') | Some(b'\t')) => {
                self.terms(out, text, in_bracket)
            }
            Some(c) if c.is_ascii_digit() && self.starts_block() => {
                let a = self.pos;
                self.skip_to_eol();
                self.diag(
                    Severity::NotImplemented,
                    a,
                    "numbered lists are not implemented",
                );
                true
            }
            _ => false,
        }
    }

    /// ``` fenced raw block; optional language tag is ignored.
    fn fence(&mut self, out: &mut Vec<Node>, text: &mut String) {
        let a = self.pos;
        self.pos += 3;
        self.skip_to_eol();
        if self.peek() == Some(b'\n') {
            self.pos += 1;
        }
        let cs = self.pos;
        let s = self.src.as_bytes();
        let mut i = self.pos;
        let (end, close) = loop {
            if i >= s.len() {
                break (s.len(), None);
            }
            let mut j = i;
            while matches!(s.get(j), Some(b' ') | Some(b'\t')) {
                j += 1;
            }
            if s[j..].starts_with(b"```")
                && matches!(s.get(j + 3), None | Some(b'\n') | Some(b' ') | Some(b'\t'))
            {
                let mut k = j + 3;
                while matches!(s.get(k), Some(b' ') | Some(b'\t')) {
                    k += 1;
                }
                break (i, Some(k));
            }
            i = match s[i..].iter().position(|&b| b == b'\n') {
                Some(d) => i + d + 1,
                None => s.len(),
            };
        };
        let mut body_txt = &self.src[cs..end];
        if body_txt.ends_with('\n') {
            body_txt = &body_txt[..body_txt.len() - 1];
        }
        match close {
            Some(k) => self.pos = k,
            None => {
                self.pos = end;
                self.diag(Severity::Warning, a, "unclosed raw block");
            }
        }
        flush(out, text);
        out.push(Node::Raw {
            text: body_txt.into(),
            block: true,
        });
    }

    /// `- ` / `+ ` list whose marker sits at `indent` (leading spaces/tabs).
    /// `depth` is 1 at the outermost list and at most 6. A column-0
    /// sibling is recognized before any indent is counted.
    fn list(
        &mut self,
        out: &mut Vec<Node>,
        text: &mut String,
        ordered: bool,
        in_bracket: bool,
        indent: usize,
        depth: u8,
    ) {
        let marker = if ordered { b'+' } else { b'-' };
        let mut items = Vec::new();
        loop {
            self.pos += 1;
            self.skip_hspace();
            items.push(self.list_item(marker, indent, depth, in_bracket));
            if self.peek() != Some(marker) || !matches!(self.at(1), Some(b' ') | Some(b'\t')) {
                break;
            }
        }
        flush(out, text);
        out.push(Node::List { ordered, items });
    }

    /// One item. Ends at a same-indent sibling marker, a blank line, a
    /// heading, or a fence. A deeper `-`/`+` marker becomes a nested list.
    fn list_item(
        &mut self,
        marker: u8,
        parent_indent: usize,
        depth: u8,
        in_bracket: bool,
    ) -> Vec<Node> {
        let mut nodes = Vec::new();
        let mut t = String::new();
        loop {
            self.inline(&mut nodes, &mut t, in_bracket, None);
            let gs = self.pos;
            // `row` counts consecutive newlines with only whitespace
            // between them. A comment breaks the streak so `- a\n// c\n- b`
            // stays one list, while a real blank line ends it.
            let mut row = 0usize;
            let mut line_indent = 0usize;
            let mut counting = false;
            let mut blank = false;
            let mut sibling = false;
            let mut hard_stop = false;
            loop {
                match self.peek() {
                    Some(b'\n') => {
                        self.pos += 1;
                        row += 1;
                        if row >= 2 {
                            blank = true;
                            break;
                        }
                        line_indent = 0;
                        counting = true;
                        // Flat lists: the next byte is the marker or a
                        // block start. Don't measure indent on that path.
                        if parent_indent == 0 {
                            if self.peek() == Some(marker)
                                && matches!(self.at(1), Some(b' ') | Some(b'\t'))
                            {
                                sibling = true;
                                break;
                            }
                            if self.block_at(self.pos) {
                                hard_stop = true;
                                break;
                            }
                        }
                    }
                    Some(b' ') | Some(b'\t') if counting => {
                        self.pos += 1;
                        line_indent += 1;
                    }
                    Some(b'\r') => self.pos += 1,
                    Some(b'/') if self.at(1) == Some(b'/') => {
                        self.line_comment();
                        counting = false;
                        row = 0;
                    }
                    Some(b'/') if self.at(1) == Some(b'*') => {
                        self.block_comment();
                        counting = false;
                        row = 0;
                    }
                    _ => break,
                }
            }
            if blank || self.eof() || (in_bracket && self.peek() == Some(b']')) {
                self.pos = gs;
                break;
            }
            if sibling {
                break;
            }
            if hard_stop {
                self.pos = gs;
                break;
            }
            if row == 0 {
                if self.pos == gs {
                    break;
                }
                continue;
            }
            let marker_here = matches!(self.peek(), Some(b'-') | Some(b'+'))
                && matches!(self.at(1), Some(b' ') | Some(b'\t'));
            if marker_here && line_indent > parent_indent {
                if depth >= 6 {
                    self.diag(Severity::Warning, self.pos, "lists nested too deeply");
                    pad(&mut t, &nodes);
                    continue;
                }
                flush(&mut nodes, &mut t);
                let ordered = self.peek() == Some(b'+');
                self.list(
                    &mut nodes,
                    &mut t,
                    ordered,
                    in_bracket,
                    line_indent,
                    depth + 1,
                );
                continue;
            }
            if marker_here && line_indent == parent_indent && self.peek() == Some(marker) {
                break;
            }
            if line_indent < parent_indent
                || (marker_here && line_indent == parent_indent)
                || (!marker_here && line_indent <= parent_indent && self.block_at(self.pos))
            {
                self.pos = gs;
                break;
            }
            // Continuation. A run that follows a nested list is its own
            // text node; don't glue a leading space onto it.
            if !(matches!(nodes.last(), Some(Node::List { .. })) && t.is_empty()) {
                pad(&mut t, &nodes);
            }
        }
        flush(&mut nodes, &mut t);
        nodes
    }

    /// `/ term: description` list. Returns false (and rewinds) when the
    /// first marker has no colon, so the line is ordinary text.
    fn terms(&mut self, out: &mut Vec<Node>, text: &mut String, in_bracket: bool) -> bool {
        let mut items: Vec<TermEntry> = Vec::new();
        loop {
            if self.peek() != Some(b'/') || !matches!(self.at(1), Some(b' ') | Some(b'\t')) {
                break;
            }
            let mark = self.pos;
            self.pos += 1;
            self.skip_hspace();
            let Some(col) = self.find_term_colon() else {
                self.diag(Severity::Warning, mark, "term list item missing ':'");
                self.pos = mark;
                break;
            };
            let term = self.parse_restricted(col, in_bracket);
            self.pos = col + 1;
            self.skip_hspace();
            let desc = self.term_desc(in_bracket);
            items.push(TermEntry { term, desc });
            if self.peek() != Some(b'/') || !matches!(self.at(1), Some(b' ') | Some(b'\t')) {
                break;
            }
        }
        if items.is_empty() {
            return false;
        }
        flush(out, text);
        out.push(Node::Terms { items });
        true
    }

    /// First colon that ends a term: not escaped, not inside `` `raw` ``
    /// or a `#"..."`. Stops at end of line.
    fn find_term_colon(&self) -> Option<usize> {
        let s = self.bytes;
        let mut i = self.pos;
        while i < s.len() {
            match s[i] {
                b'\n' => return None,
                b'\\' => {
                    i += 1;
                    if i < s.len() && s[i] != b'\n' {
                        i += 1;
                    }
                }
                b'`' => {
                    i += 1;
                    while i < s.len() && s[i] != b'`' && s[i] != b'\n' {
                        i += 1;
                    }
                    if i < s.len() && s[i] == b'`' {
                        i += 1;
                    }
                }
                b'#' if s.get(i + 1) == Some(&b'"') => {
                    i += 2;
                    while i < s.len() && s[i] != b'"' && s[i] != b'\n' {
                        if s[i] == b'\\' && i + 1 < s.len() {
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                    if i < s.len() && s[i] == b'"' {
                        i += 1;
                    }
                }
                b':' => return Some(i),
                _ => i += 1,
            }
        }
        None
    }

    /// Parse inline markup in `self.src[..end]`, then restore the full
    /// source. Positions stay absolute, so no extra scan of other text.
    fn parse_restricted(&mut self, end: usize, in_bracket: bool) -> Vec<Node> {
        let src = self.src;
        let bytes = self.bytes;
        self.src = &src[..end];
        self.bytes = &bytes[..end];
        let mut nodes = Vec::new();
        let mut t = String::new();
        self.inline(&mut nodes, &mut t, in_bracket, None);
        flush(&mut nodes, &mut t);
        self.src = src;
        self.bytes = bytes;
        nodes
    }

    /// Description: the rest of this line, plus lines indented further
    /// than the `/`. An indented `/` stays literal.
    fn term_desc(&mut self, in_bracket: bool) -> Vec<Node> {
        let mut nodes = Vec::new();
        let mut t = String::new();
        loop {
            self.inline(&mut nodes, &mut t, in_bracket, None);
            let gs = self.pos;
            let mut row = 0usize;
            let mut line_indent = 0usize;
            let mut counting = false;
            let mut stop_rewind = false;
            let mut stop_sibling = false;
            loop {
                match self.peek() {
                    Some(b'\n') => {
                        self.pos += 1;
                        row += 1;
                        if row >= 2 {
                            stop_rewind = true;
                            break;
                        }
                        line_indent = 0;
                        counting = true;
                        if self.peek() == Some(b'/')
                            && matches!(self.at(1), Some(b' ') | Some(b'\t'))
                        {
                            stop_sibling = true;
                            break;
                        }
                        if self.block_at(self.pos) {
                            stop_rewind = true;
                            break;
                        }
                    }
                    Some(b' ') | Some(b'\t') if counting => {
                        self.pos += 1;
                        line_indent += 1;
                    }
                    Some(b'\r') => self.pos += 1,
                    Some(b'/') if self.at(1) == Some(b'/') => {
                        self.line_comment();
                        counting = false;
                        row = 0;
                    }
                    Some(b'/') if self.at(1) == Some(b'*') => {
                        self.block_comment();
                        counting = false;
                        row = 0;
                    }
                    _ => break,
                }
            }
            if stop_rewind || self.eof() || (in_bracket && self.peek() == Some(b']')) {
                self.pos = gs;
                break;
            }
            if stop_sibling {
                break;
            }
            if row == 0 {
                if self.pos == gs {
                    break;
                }
                continue;
            }
            if line_indent > 0 {
                pad(&mut t, &nodes);
                continue;
            }
            self.pos = gs;
            break;
        }
        flush(&mut nodes, &mut t);
        nodes
    }

    /// One line of inline content; stops at `\n`, `]` (in bracket), the
    /// `stop` marker, a comment opener, or EOF — all left unconsumed.
    fn inline(
        &mut self,
        out: &mut Vec<Node>,
        text: &mut String,
        in_bracket: bool,
        stop: Option<u8>,
    ) {
        let bytes = self.bytes;
        loop {
            let pos = self.pos;
            if pos >= bytes.len() {
                return;
            }
            let c = bytes[pos];
            if c == b'\n' || (in_bracket && c == b']') {
                return;
            }
            if stop == Some(c) && self.at_marker(c) {
                return;
            }
            if c == b'/' && matches!(bytes.get(pos + 1), Some(&b'/') | Some(&b'*')) {
                return;
            }
            // simdjson-style stage split: copy a plain run, then handle one sigil.
            if PLAIN[c as usize] != 0 {
                let mut i = pos + 1;
                let n = bytes.len();
                // Eight loads, then a scalar tail. The class of a byte is
                // one table load instead of the sigil match.
                while i + 8 <= n
                    && PLAIN[bytes[i] as usize]
                        & PLAIN[bytes[i + 1] as usize]
                        & PLAIN[bytes[i + 2] as usize]
                        & PLAIN[bytes[i + 3] as usize]
                        & PLAIN[bytes[i + 4] as usize]
                        & PLAIN[bytes[i + 5] as usize]
                        & PLAIN[bytes[i + 6] as usize]
                        & PLAIN[bytes[i + 7] as usize]
                        != 0
                {
                    i += 8;
                }
                while i < n && PLAIN[bytes[i] as usize] != 0 {
                    i += 1;
                }
                text.push_str(&self.src[pos..i]);
                self.pos = i;
                continue;
            }
            match c {
                b'*' => {
                    if !self.marker(b'*', out, text, in_bracket) {
                        text.push('*');
                        self.pos += 1;
                    }
                }
                b'_' => {
                    let boundary =
                        self.pos == 0 || !self.src.as_bytes()[self.pos - 1].is_ascii_alphanumeric();
                    if boundary && self.marker(b'_', out, text, in_bracket) {
                    } else {
                        text.push('_');
                        self.pos += 1;
                    }
                }
                b'`' => self.raw_inline(out, text),
                b'\\' => self.escape(out, text),
                b'#' => self.hash(out, text),
                b'$' => self.math(),
                b'<' => {
                    if !self.label(out, text) {
                        text.push('<');
                        self.pos += 1;
                    }
                }
                b'@' => {
                    if !self.at_ref(out, text) {
                        text.push('@');
                        self.pos += 1;
                    }
                }
                b'/' if self.at(1) == Some(b'/') || self.at(1) == Some(b'*') => return,
                b' ' | b'\t' | b'\r' => {
                    text.push(' ');
                    self.pos += 1;
                }
                _ => {
                    text.push(self.src[self.pos..].chars().next().unwrap());
                    self.adv();
                }
            }
        }
    }

    fn at_marker(&self, m: u8) -> bool {
        self.peek() == Some(m)
            && (m == b'*' || !self.at(1).map_or(false, |c| c.is_ascii_alphanumeric()))
    }

    /// `*strong*` / `_emph_`. Only entered when a closer exists in this
    /// paragraph; the closer may span single newlines.
    fn marker(&mut self, m: u8, out: &mut Vec<Node>, text: &mut String, in_bracket: bool) -> bool {
        if self.find_close(m, in_bracket).is_none() {
            return false;
        }
        self.pos += 1;
        let mut nodes = Vec::new();
        let mut t = String::new();
        loop {
            self.inline(&mut nodes, &mut t, in_bracket, Some(m));
            if self.at_marker(m) {
                self.pos += 1;
                break;
            }
            if self.eof() || (in_bracket && self.peek() == Some(b']')) {
                break;
            }
            let gs = self.pos;
            let nls = self.gap();
            if self.at_marker(m) {
                continue;
            }
            if self.eof() || (in_bracket && self.peek() == Some(b']')) {
                break;
            }
            if nls > 0 && (nls >= 2 || self.starts_block()) {
                self.pos = gs;
                break;
            }
            if nls > 0 {
                pad(&mut t, &nodes);
            }
        }
        flush(&mut nodes, &mut t);
        flush(out, text);
        out.push(if m == b'*' {
            Node::Strong(nodes)
        } else {
            Node::Emph(nodes)
        });
        true
    }

    /// Look for a closing `*`/`_` before the paragraph ends. Bounded scan.
    fn find_close(&self, m: u8, in_bracket: bool) -> Option<usize> {
        let s = self.bytes;
        let mut i = self.pos + 1;
        let limit = (self.pos + (1 << 16)).min(s.len());
        while i < limit {
            let c = s[i];
            if INTERESTING[c as usize] == 0 && c != m {
                i += 1;
                while i < limit && INTERESTING[s[i] as usize] == 0 && s[i] != m {
                    i += 1;
                }
                continue;
            }
            match c {
                b'\n' => {
                    // paragraph ends at a blank line or a block-start line
                    let mut j = i + 1;
                    loop {
                        match s.get(j).copied() {
                            Some(b' ') | Some(b'\t') | Some(b'\r') => j += 1,
                            Some(b'/') if s.get(j + 1) == Some(&b'/') => {
                                while j < s.len() && s[j] != b'\n' {
                                    j += 1;
                                }
                            }
                            Some(b'/') if s.get(j + 1) == Some(&b'*') => {
                                j += 2;
                                let mut d = 1;
                                while j < s.len() && d > 0 {
                                    if s[j] == b'/' && s.get(j + 1) == Some(&b'*') {
                                        d += 1;
                                        j += 2;
                                    } else if s[j] == b'*' && s.get(j + 1) == Some(&b'/') {
                                        d -= 1;
                                        j += 2;
                                    } else {
                                        j += 1;
                                    }
                                }
                            }
                            _ => break,
                        }
                    }
                    if j >= s.len() || s[j] == b'\n' {
                        return None;
                    }
                    if in_bracket && s[j] == b']' {
                        return None;
                    }
                    if self.block_at(j) {
                        return None;
                    }
                    i += 1;
                }
                b'\\' => i += 2,
                b'`' => {
                    i += 1;
                    while i < s.len() && s[i] != b'`' && s[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if s.get(i + 1) == Some(&b'/') => {
                    while i < s.len() && s[i] != b'\n' {
                        i += 1;
                    }
                }
                b'/' if s.get(i + 1) == Some(&b'*') => {
                    i += 2;
                    let mut d = 1;
                    while i < s.len() && d > 0 {
                        if s[i] == b'/' && s.get(i + 1) == Some(&b'*') {
                            d += 1;
                            i += 2;
                        } else if s[i] == b'*' && s.get(i + 1) == Some(&b'/') {
                            d -= 1;
                            i += 2;
                        } else {
                            i += 1;
                        }
                    }
                }
                b']' if in_bracket => return None,
                c if c == m => {
                    if m == b'*' || !s.get(i + 1).map_or(false, |n| n.is_ascii_alphanumeric()) {
                        return Some(i);
                    }
                    i += 1;
                }
                _ => i += 1,
            }
        }
        None
    }

    /// `` `inline` `` raw; an unclosed backtick is literal text.
    fn raw_inline(&mut self, out: &mut Vec<Node>, text: &mut String) {
        let s = self.src.as_bytes();
        let mut i = self.pos + 1;
        while i < s.len() {
            match s[i] {
                b'`' => {
                    let t = self.src[self.pos + 1..i].to_string();
                    self.pos = i + 1;
                    flush(out, text);
                    out.push(Node::Raw {
                        text: t,
                        block: false,
                    });
                    return;
                }
                b'\n' => break,
                _ => i += 1,
            }
        }
        text.push('`');
        self.pos += 1;
    }

    /// `\x`: ASCII punctuation escapes literally; anything else is a
    /// hard line break.
    fn escape(&mut self, out: &mut Vec<Node>, text: &mut String) {
        self.pos += 1;
        match self.peek() {
            Some(c) if c.is_ascii_punctuation() => {
                text.push(c as char);
                self.pos += 1;
            }
            Some(b' ') | Some(b'\t') | Some(b'\r') => {
                self.skip_hspace();
                flush(out, text);
                out.push(Node::LineBreak);
            }
            _ => {
                flush(out, text);
                out.push(Node::LineBreak);
            }
        }
    }

    /// `$...$` math: recognized, not implemented.
    fn math(&mut self) {
        let a = self.pos;
        self.pos += 1;
        while let Some(c) = self.peek() {
            if c == b'$' {
                self.pos += 1;
                break;
            }
            if c == b'\n' {
                break;
            }
            self.adv();
        }
        self.diag(Severity::NotImplemented, a, "math is not implemented");
    }

    /// `<name>` label. Letters, digits, `_`, `-`, `.`.
    fn label(&mut self, out: &mut Vec<Node>, text: &mut String) -> bool {
        let a = self.pos;
        let (name, end) = {
            let s = self.src.as_bytes();
            let mut i = a + 1;
            while i < s.len() && is_label(s[i]) {
                i += 1;
            }
            if i > a + 1 && s.get(i) == Some(&b'>') {
                (Some(self.src[a + 1..i].to_string()), i + 1)
            } else {
                (None, a)
            }
        };
        if let Some(name) = name {
            self.pos = end;
            flush(out, text);
            out.push(Node::Label(name));
            true
        } else {
            false
        }
    }

    /// `@name` reference. A `.` stays in the name only when another label
    /// character follows it (`@fig.1`, `@a.b`, `@a..b`). Dots before
    /// space, punctuation, or end of input stay in the text (`@intro.`).
    fn at_ref(&mut self, out: &mut Vec<Node>, text: &mut String) -> bool {
        let a = self.pos;
        let (name, end) = {
            let s = self.src.as_bytes();
            let mut i = a + 1;
            while i < s.len() && is_label(s[i]) {
                i += 1;
            }
            // Typst uneats trailing `.` after the label scan, so `@intro..`
            // is `intro` and both dots remain text.
            while i > a + 1 && s[i - 1] == b'.' {
                i -= 1;
            }
            if i > a + 1 {
                (Some(self.src[a + 1..i].to_string()), i)
            } else {
                (None, a)
            }
        };
        if let Some(name) = name {
            self.pos = end;
            flush(out, text);
            out.push(Node::Ref(name));
            true
        } else {
            false
        }
    }

    /// `#expr`.
    fn hash(&mut self, out: &mut Vec<Node>, text: &mut String) {
        let a = self.pos;
        self.pos += 1;
        match self.peek() {
            Some(c) if is_ident_start(c) => {
                let name = self.ident().unwrap();
                match name.as_str() {
                    "let" => self.parse_let(a, out, text),
                    "set" => self.parse_set(a, out, text),
                    "import" => self.parse_import(a, out, text),
                    "include" => self.parse_include(a, out, text),
                    "if" => self.parse_if(a, out, text),
                    "for" => self.parse_for(a, out, text),
                    "while" => self.parse_while(a, out, text),
                    "show" => self.parse_show(a, out, text),
                    _ => self.hash_call(a, name, out, text),
                }
            }
            Some(b'"') => {
                let v = Value::Str(self.string(a));
                flush(out, text);
                out.push(Node::Expr(Expr::Lit(v)));
            }
            Some(c)
                if c.is_ascii_digit()
                    || (matches!(c, b'-' | b'+')
                        && self.at(1).map_or(false, |d| d.is_ascii_digit())) =>
            {
                if let Some(v) = self.number() {
                    flush(out, text);
                    out.push(Node::Expr(Expr::Lit(v)));
                }
            }
            Some(b'(') => {
                // `#(expr)` — a parenthesized expression if it scans as
                // `rhs )`, otherwise an array/dict/group literal.
                let save = self.pos;
                self.pos += 1;
                let mut handled = false;
                if let Some(r) = self.rhs() {
                    self.skip_ws();
                    if self.peek() == Some(b')') {
                        self.pos += 1;
                        flush(out, text);
                        out.push(Node::Expr(rhs_expr(r)));
                        handled = true;
                    }
                }
                if !handled {
                    self.pos = save;
                    match self.paren_value() {
                        Some(v) => {
                            flush(out, text);
                            out.push(Node::Expr(Expr::Lit(v)));
                        }
                        None => self.diag(
                            Severity::NotImplemented,
                            a,
                            "code expressions are not implemented",
                        ),
                    }
                }
            }
            _ => text.push('#'),
        }
    }

    /// `#name` — a function call, a variable reference (possibly with
    /// `.field` access), or a spaced binary expression `#a + b`.
    fn hash_call(&mut self, a: usize, name: String, out: &mut Vec<Node>, text: &mut String) {
        let bound = self.lets.iter().any(|l| l == &name);
        // `import *` does not list names. Those calls resolve in eval.
        if (bound || self.star)
            && !IMPLEMENTED.contains(&name.as_str())
            && matches!(self.peek(), Some(b'(') | Some(b'['))
        {
            let args = if self.peek() == Some(b'(') {
                self.rhs_args()
            } else {
                Vec::new()
            };
            let content = self.trailing_content();
            flush(out, text);
            out.push(Node::Expr(Expr::Call { name, args, content }));
            return;
        }
        if matches!(self.peek(), Some(b'(') | Some(b'[')) {
            let implemented = IMPLEMENTED.contains(&name.as_str()) || bound;
            let mut args = if self.peek() == Some(b'(') {
                self.args()
            } else {
                Vec::new()
            };
            let mut content = if self.peek() == Some(b'[') {
                let bs = self.pos;
                self.pos += 1;
                Some(self.body(true, bs))
            } else {
                None
            };
            // `#stack[a][b][c]` — every consecutive content block is a
            // positional content arg. Other calls keep a single trailing
            // block, so `#emph[x]`, `#if`, and `#for` are unchanged.
            if name == "stack" {
                if let Some(first) = content.take() {
                    args.push(Arg {
                        name: None,
                        value: Value::Content(first),
                    });
                }
                loop {
                    let save = self.pos;
                    self.skip_ws();
                    if self.peek() == Some(b'[') {
                        let bs = self.pos;
                        self.pos += 1;
                        args.push(Arg {
                            name: None,
                            value: Value::Content(self.body(true, bs)),
                        });
                    } else {
                        self.pos = save;
                        break;
                    }
                }
            }
            if implemented {
                flush(out, text);
                out.push(Node::Expr(Expr::Func {
                    name,
                    args,
                    content,
                }));
            } else {
                self.diag(
                    Severity::NotImplemented,
                    a,
                    format!("#{name} is not implemented"),
                );
            }
        } else if bound
            || !(IMPLEMENTED.contains(&name.as_str()) || NOT_IMPL_BARE.contains(&name.as_str()))
        {
            // `#var`, `#var.field`, `#xs.len()`, `#a + b`. Unknown names
            // still parse as `Expr::Var` — eval reports "undefined
            // variable" (this also covers names bound via `#import "..": *`).
            // A pure field chain stays a dotted `Var`; a method call does not.
            let lhs = self.with_suffix(Rhs::Var(name));
            let save = self.pos;
            self.skip_hspace();
            if self.pos > save {
                if let Some(op) = self.arith_op() {
                    self.skip_hspace();
                    if self.rhs_starts() {
                        if let Some(rhs) = self.rhs() {
                            flush(out, text);
                            out.push(Node::Expr(Expr::Bin { lhs, op, rhs }));
                            return;
                        }
                    }
                }
            }
            self.pos = save;
            flush(out, text);
            out.push(Node::Expr(rhs_expr(lhs)));
        } else if IMPLEMENTED.contains(&name.as_str()) {
            flush(out, text);
            out.push(Node::Expr(Expr::Func {
                name,
                args: Vec::new(),
                content: None,
            }));
        } else {
            self.diag(
                Severity::NotImplemented,
                a,
                format!("#{name} is not implemented"),
            );
            self.skip_tail();
        }
    }

    /// `#import "path" [: a, b | *]` — or `"@ns/name:ver"` package spec.
    fn parse_import(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        self.skip_hspace();
        if self.peek() != Some(b'"') {
            self.diag(Severity::Error, a, "expected a string after `#import`");
            self.skip_to_eol();
            return;
        }
        let path = self.string(self.pos);
        self.skip_hspace();
        let mut items = Vec::new();
        let mut all = false;
        if self.peek() == Some(b':') {
            self.pos += 1;
            self.skip_hspace();
            if self.peek() == Some(b'*') {
                self.pos += 1;
                all = true;
            } else {
                loop {
                    self.skip_hspace();
                    match self.ident() {
                        Some(n) => items.push(n),
                        None => break,
                    }
                    self.skip_hspace();
                    if self.peek() == Some(b',') {
                        self.pos += 1;
                    } else {
                        break;
                    }
                }
            }
        }
        // Register names bound by the import so later `#name` parses as a
        // variable. With `*` the names are unknown at parse time; the
        // generic `#ident` -> Var fallback in hash_call covers them.
        if all {
            self.star = true;
        } else if items.is_empty() {
            self.lets.push(spec_stem(&path));
        } else {
            self.lets.extend(items.iter().cloned());
        }
        flush(out, text);
        out.push(Node::Expr(Expr::Import { path, items, all }));
    }

    /// `#include "path"` — emits `#include("path")` with no parens in source.
    fn parse_include(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        self.skip_hspace();
        if self.peek() != Some(b'"') {
            self.diag(Severity::Error, a, "expected a string after `#include`");
            self.skip_to_eol();
            return;
        }
        let path = self.string(self.pos);
        flush(out, text);
        out.push(Node::Expr(Expr::Func {
            name: "include".into(),
            args: vec![Arg {
                name: None,
                value: Value::Str(path),
            }],
            content: None,
        }));
    }

    /// `#if cond [..] else if cond [..] else [..]`.
    fn parse_if(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        let mut arms = Vec::new();
        let mut els = None;
        loop {
            let Some(c) = self.cond() else {
                self.diag(Severity::Error, a, "expected a condition after `#if`");
                break;
            };
            self.skip_ws();
            if self.peek() != Some(b'[') {
                self.diag(Severity::Error, a, "expected `[` after `#if` condition");
                break;
            }
            let bs = self.pos;
            self.pos += 1;
            arms.push((c, self.body(true, bs)));
            let save = self.pos;
            self.gap();
            if self.ident().as_deref() != Some("else") {
                self.pos = save;
                break;
            }
            self.gap();
            if self.ident().as_deref() == Some("if") {
                continue;
            }
            self.skip_ws();
            if self.peek() == Some(b'[') {
                let bs = self.pos;
                self.pos += 1;
                els = Some(self.body(true, bs));
            } else {
                self.diag(Severity::Error, a, "expected `[` or `if` after `else`");
            }
            break;
        }
        if !arms.is_empty() || els.is_some() {
            flush(out, text);
            out.push(Node::Expr(Expr::If { arms, els }));
        }
    }

    /// `#for x in expr [..]` / `#for (k, v) in expr [..]`.
    fn parse_for(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        self.skip_hspace();
        let mut vars = Vec::new();
        if self.peek() == Some(b'(') {
            self.pos += 1;
            loop {
                self.skip_ws();
                if let Some(n) = self.ident() {
                    vars.push(n);
                }
                self.skip_ws();
                match self.peek() {
                    Some(b',') => self.pos += 1,
                    Some(b')') => {
                        self.pos += 1;
                        break;
                    }
                    _ => break,
                }
            }
        } else if let Some(n) = self.ident() {
            vars.push(n);
        }
        if vars.is_empty() {
            self.diag(Severity::Error, a, "expected a variable after `#for`");
            self.skip_to_eol();
            return;
        }
        self.skip_hspace();
        if self.ident().as_deref() != Some("in") {
            self.diag(Severity::Error, a, "expected `in` in `#for`");
            self.skip_to_eol();
            return;
        }
        let Some(iter) = self.rhs() else {
            self.diag(Severity::Error, a, "expected an expression after `in`");
            self.skip_to_eol();
            return;
        };
        self.skip_ws();
        if self.peek() != Some(b'[') {
            self.diag(Severity::Error, a, "expected `[` after `#for` expression");
            self.skip_to_eol();
            return;
        }
        let bs = self.pos;
        self.pos += 1;
        // Register loop vars before parsing the body so `#var` resolves.
        self.lets.extend(vars.iter().cloned());
        let body = self.body(true, bs);
        flush(out, text);
        out.push(Node::Expr(Expr::For { vars, iter, body }));
    }

    /// `#while cond [body]`. Same condition language as `#if`.
    fn parse_while(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        let Some(cond) = self.cond() else {
            self.diag(Severity::Error, a, "expected a condition after `#while`");
            self.skip_to_eol();
            return;
        };
        self.skip_ws();
        if self.peek() != Some(b'[') {
            self.diag(Severity::Error, a, "expected `[` after `#while` condition");
            self.skip_to_eol();
            return;
        }
        let bs = self.pos;
        self.pos += 1;
        let body = self.body(true, bs);
        flush(out, text);
        out.push(Node::Expr(Expr::While { cond, body }));
    }

    /// `#if` / `#while` condition. `and` binds tighter than `or`; prefix
    /// `not` binds tighter than both. A bare value desugars to `x != none`
    /// (eval treats that as truthiness). Parentheses group a condition,
    /// but a parenthesized bare value stays an rhs so `(1) == 2` is still
    /// a comparison.
    fn cond(&mut self) -> Option<Cond> {
        self.cond_or().map(|(c, _)| c)
    }

    fn cond_or(&mut self) -> Option<(Cond, bool)> {
        let (mut lhs, mut bare) = self.cond_and()?;
        loop {
            let save = self.pos;
            self.skip_ws();
            if self.word("or") {
                match self.cond_and() {
                    Some((r, _)) => {
                        lhs = Cond::Or(Box::new(lhs), Box::new(r));
                        bare = false;
                    }
                    None => {
                        self.diag(Severity::Error, self.pos, "expected an expression after `or`");
                        return None;
                    }
                }
            } else {
                self.pos = save;
                break;
            }
        }
        Some((lhs, bare))
    }

    fn cond_and(&mut self) -> Option<(Cond, bool)> {
        let (mut lhs, mut bare) = self.cond_unary()?;
        loop {
            let save = self.pos;
            self.skip_ws();
            if self.word("and") {
                match self.cond_unary() {
                    Some((r, _)) => {
                        lhs = Cond::And(Box::new(lhs), Box::new(r));
                        bare = false;
                    }
                    None => {
                        self.diag(Severity::Error, self.pos, "expected an expression after `and`");
                        return None;
                    }
                }
            } else {
                self.pos = save;
                break;
            }
        }
        Some((lhs, bare))
    }

    fn cond_unary(&mut self) -> Option<(Cond, bool)> {
        self.skip_ws();
        if self.word("not") {
            let (c, _) = self.cond_unary()?;
            return Some((Cond::Not(Box::new(c)), false));
        }
        self.cond_primary()
    }

    fn cond_primary(&mut self) -> Option<(Cond, bool)> {
        self.skip_ws();
        if self.peek() == Some(b'(') {
            let save = self.pos;
            self.pos += 1;
            self.depth += 1;
            let grouped = if self.depth > 64 {
                self.diag(Severity::Error, save, "expression nested too deeply");
                None
            } else {
                self.cond_or()
            };
            if let Some((c, bare)) = grouped {
                self.skip_ws();
                if self.peek() == Some(b')') && !bare {
                    self.pos += 1;
                    self.depth -= 1;
                    return Some((c, false));
                }
            }
            self.depth -= 1;
            self.pos = save;
        }
        self.comparison()
    }

    /// `rhs (cmp-op rhs)?`. Without an operator the value is bare
    /// (`bare == true`) and desugars to `lhs != none`.
    fn comparison(&mut self) -> Option<(Cond, bool)> {
        let lhs = self.rhs()?;
        self.skip_ws();
        let (op, adv) = match self.peek() {
            Some(b'=') if self.at(1) == Some(b'=') => (CmpOp::Eq, 2),
            Some(b'!') if self.at(1) == Some(b'=') => (CmpOp::Ne, 2),
            Some(b'<') if self.at(1) == Some(b'=') => (CmpOp::Le, 2),
            Some(b'>') if self.at(1) == Some(b'=') => (CmpOp::Ge, 2),
            Some(b'<') => (CmpOp::Lt, 1),
            Some(b'>') => (CmpOp::Gt, 1),
            _ => (CmpOp::Ne, 0),
        };
        if adv == 0 {
            return Some((
                Cond::Cmp(Cmp {
                    lhs,
                    op: CmpOp::Ne,
                    rhs: Rhs::Lit(Value::None),
                }),
                true,
            ));
        }
        self.pos += adv;
        let rhs = self.rhs()?;
        Some((Cond::Cmp(Cmp { lhs, op, rhs }), false))
    }

    /// Exact identifier `kw`. Does not match a longer name (`or` / `order`).
    fn word(&mut self, kw: &str) -> bool {
        let s = self.pos;
        if !self.peek().map_or(false, is_ident_start) {
            return false;
        }
        let mut i = self.pos;
        while self.bytes.get(i).copied().map_or(false, is_ident_cont) {
            i += 1;
        }
        if &self.src[s..i] == kw {
            self.pos = i;
            true
        } else {
            false
        }
    }

    /// `word`, but does not consume. A longer ident (`set` vs `setting`) misses.
    fn at_word(&self, kw: &str) -> bool {
        let b = kw.as_bytes();
        let s = self.pos;
        if s + b.len() > self.bytes.len() || &self.bytes[s..s + b.len()] != b {
            return false;
        }
        !self.bytes.get(s + b.len()).copied().map_or(false, is_ident_cont)
    }

    /// Show-rule body. A line break ends it, so the next line's markup stays.
    fn show_rhs(&mut self) -> Option<Rhs> {
        self.fold_rhs(true, false)
    }

    /// Right-hand side: `mul-level` terms joined by `+`/`-`, folded left.
    fn rhs(&mut self) -> Option<Rhs> {
        self.fold_rhs(false, false)
    }

    fn fold_rhs(&mut self, line: bool, mul: bool) -> Option<Rhs> {
        let mut lhs = if mul {
            self.rhs_term()?
        } else {
            self.fold_rhs(line, true)?
        };
        loop {
            let save = self.pos;
            if line {
                self.skip_hspace();
            } else {
                self.skip_ws();
            }
            let op = match self.peek() {
                Some(b'+') if !mul => ArithOp::Add,
                Some(b'-') if !mul => ArithOp::Sub,
                Some(b'*') if mul => ArithOp::Mul,
                Some(b'/') if mul && !matches!(self.at(1), Some(b'/') | Some(b'*')) => ArithOp::Div,
                _ => {
                    self.pos = save;
                    break;
                }
            };
            self.pos += 1;
            if line {
                self.skip_hspace();
            } else {
                self.skip_ws();
            }
            let next = if mul {
                self.rhs_term()
            } else {
                self.fold_rhs(line, true)
            };
            match next {
                Some(r) => {
                    lhs = Rhs::Bin {
                        lhs: Box::new(lhs),
                        op,
                        rhs: Box::new(r),
                    }
                }
                None => {
                    self.pos = save;
                    break;
                }
            }
        }
        Some(lhs)
    }

    /// One operand: literal, `var`, `var.field`, `value.method(args)`,
    /// `(...)`, or `[content]`.
    fn rhs_term(&mut self) -> Option<Rhs> {
        self.skip_ws();
        let base = match self.peek()? {
            b'(' => self.paren_value().map(Rhs::Lit)?,
            b'"' => Rhs::Lit(Value::Str(self.string(self.pos))),
            b'[' => {
                let bs = self.pos;
                self.pos += 1;
                Rhs::Lit(Value::Content(self.body(true, bs)))
            }
            c if c.is_ascii_digit()
                || (matches!(c, b'-' | b'+')
                    && self.at(1).map_or(false, |d| d.is_ascii_digit()))
                || (c == b'.' && self.at(1).map_or(false, |d| d.is_ascii_digit())) =>
            {
                self.number().map(Rhs::Lit)?
            }
            c if is_ident_start(c) => {
                let n = self.ident().unwrap();
                match n.as_str() {
                    "true" => Rhs::Lit(Value::Bool(true)),
                    "false" => Rhs::Lit(Value::Bool(false)),
                    "none" | "auto" => Rhs::Lit(Value::None),
                    _ => Rhs::Var(n),
                }
            }
            _ => return None,
        };
        Some(self.with_suffix(base))
    }

    /// `.field` or `.name(args)` after an atom. `(` must follow the name
    /// immediately; a bare `.name` stays a field.
    fn with_suffix(&mut self, mut r: Rhs) -> Rhs {
        while self.peek() == Some(b'.') && self.at(1).map_or(false, is_ident_start) {
            self.pos += 1;
            let name = self.ident().unwrap();
            if self.peek() == Some(b'(') {
                let args = self.rhs_call_args();
                r = Rhs::Call {
                    recv: Box::new(r),
                    method: name,
                    args,
                };
            } else {
                r = Rhs::Field {
                    base: Box::new(r),
                    name,
                };
            }
        }
        r
    }

    /// Argument list of a method call. The opening `(` is current.
    fn rhs_call_args(&mut self) -> Vec<Rhs> {
        self.pos += 1;
        self.depth += 1;
        if self.depth > 64 {
            self.diag(Severity::Error, self.pos, "expression nested too deeply");
            self.skip_balanced(b'(', b')');
            self.depth -= 1;
            return Vec::new();
        }
        let mut args = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    self.diag(Severity::Error, self.pos, "unclosed `(`");
                    break;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            match self.rhs() {
                Some(v) => args.push(v),
                None => {
                    self.diag(Severity::Error, self.pos, "expected a value");
                    break;
                }
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    self.skip_ws();
                    if self.peek() == Some(b')') {
                        self.pos += 1;
                        break;
                    }
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                None => {
                    self.diag(Severity::Error, self.pos, "unclosed `(`");
                    break;
                }
                _ => {
                    self.diag(Severity::Error, self.pos, "expected `,` or `)`");
                    break;
                }
            }
        }
        self.depth -= 1;
        args
    }

    /// Does an rhs operand start here?
    fn rhs_starts(&self) -> bool {
        match self.peek() {
            Some(b'"') | Some(b'(') | Some(b'[') => true,
            Some(c) if c.is_ascii_digit() || is_ident_start(c) => true,
            Some(b'-') | Some(b'+') => self.at(1).map_or(false, |d| d.is_ascii_digit()),
            Some(b'.') => self.at(1).map_or(false, |d| d.is_ascii_digit()),
            _ => false,
        }
    }

    /// `+` `-` `*` `/` — but `/` must not start a `//` or `/*` comment.
    fn arith_op(&mut self) -> Option<ArithOp> {
        let op = match self.peek()? {
            b'+' => ArithOp::Add,
            b'-' => ArithOp::Sub,
            b'*' => ArithOp::Mul,
            b'/' if !matches!(self.at(1), Some(b'/') | Some(b'*')) => ArithOp::Div,
            _ => return None,
        };
        self.pos += 1;
        Some(op)
    }

    /// `( a, b, ... )` literal: dict if any item is `name: value`, array
    /// if >1 items or a trailing comma, otherwise a plain group.
    fn paren_value(&mut self) -> Option<Value> {
        self.pos += 1;
        self.depth += 1;
        let v = self.paren_inner();
        self.depth -= 1;
        v
    }

    fn paren_inner(&mut self) -> Option<Value> {
        if self.depth > 64 {
            self.diag(Severity::Error, self.pos, "expression nested too deeply");
            self.skip_balanced(b'(', b')');
            return None;
        }
        let mut items: Vec<Arg> = Vec::new();
        let mut trailing = false;
        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    self.diag(Severity::Error, self.pos, "unclosed `(`");
                    return None;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            let mut name = None;
            let save = self.pos;
            if let Some(n) = self.ident() {
                self.skip_hspace();
                if self.peek() == Some(b':') {
                    self.pos += 1;
                    name = Some(n);
                } else {
                    self.pos = save;
                }
            }
            match self.value() {
                Some(v) => items.push(Arg { name, value: v }),
                None => {
                    self.diag(Severity::Error, self.pos, "expected a value");
                    self.recover_arg();
                }
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                    let s2 = self.pos;
                    self.skip_ws();
                    if self.peek() == Some(b')') {
                        self.pos += 1;
                        trailing = true;
                        break;
                    }
                    self.pos = s2;
                }
                Some(b')') | None => {}
                _ => {
                    self.diag(Severity::Error, self.pos, "expected `,` or `)`");
                    self.recover_arg();
                }
            }
        }
        if items.iter().any(|a| a.name.is_some()) {
            let mut ps = Vec::new();
            for a in items {
                match a.name {
                    Some(k) => ps.push((k, a.value)),
                    None => self.diag(
                        Severity::Warning,
                        self.pos,
                        "positional item ignored in dict literal",
                    ),
                }
            }
            Some(Value::Dict(ps))
        } else if items.len() == 1 && !trailing {
            Some(items.pop().unwrap().value)
        } else {
            Some(Value::Array(items.into_iter().map(|a| a.value).collect()))
        }
    }

    /// Remainder of an unrecognized `#foo` construct: an optional ident,
    /// trailing string / `(...)` / `[...]` operands, or a `#show heading:
    /// ...` rest of line. Bracket groups may span lines. The following
    /// paragraph is left for the markup parser.
    fn skip_tail(&mut self) {
        self.skip_hspace();
        if self.peek().map_or(false, is_ident_start) {
            let _ = self.ident();
        }
        loop {
            self.skip_hspace();
            match self.peek() {
                Some(b'"') => {
                    self.string(self.pos);
                }
                Some(b'(') => {
                    self.pos += 1;
                    self.skip_balanced(b'(', b')');
                }
                Some(b'[') => {
                    self.pos += 1;
                    self.skip_balanced(b'[', b']');
                }
                Some(b':') => {
                    self.skip_colon_tail();
                    return;
                }
                _ => return,
            }
        }
    }

    /// After `:`, consume the rest of this line. A `[...]` or `(...)`
    /// group is consumed whole, even when it crosses lines.
    fn skip_colon_tail(&mut self) {
        while let Some(c) = self.peek() {
            match c {
                b'\n' => return,
                b'[' => {
                    self.pos += 1;
                    self.skip_balanced(b'[', b']');
                }
                b'(' => {
                    self.pos += 1;
                    self.skip_balanced(b'(', b')');
                }
                b'"' => {
                    self.string(self.pos);
                }
                _ => self.pos += 1,
            }
        }
    }

    /// `#show target: param => body` for the five element targets.
    /// Anything else is `not implemented` and is consumed so it is not
    /// printed as page text.
    fn parse_show(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        let start = self.pos;
        self.skip_hspace();
        let Some(target) = self.ident() else {
            self.reject_show(a, start);
            return;
        };
        if !matches!(
            target.as_str(),
            "heading" | "strong" | "emph" | "link" | "figure"
        ) {
            self.reject_show(a, start);
            return;
        }
        self.skip_hspace();
        if self.peek() != Some(b':') {
            self.reject_show(a, start);
            return;
        }
        self.pos += 1;
        self.skip_ws();
        if self.at_word("set") {
            self.reject_show(a, start);
            return;
        }
        let Some(param) = self.ident() else {
            self.reject_show(a, start);
            return;
        };
        self.skip_ws();
        if self.peek() != Some(b'=') || self.at(1) != Some(b'>') {
            self.reject_show(a, start);
            return;
        }
        self.pos += 2;
        let mark = self.lets.len();
        self.lets.push(param.clone());
        let body = self.show_rhs();
        if self.lets.get(mark).map(|n| n == &param).unwrap_or(false) {
            self.lets.remove(mark);
        } else {
            self.lets.truncate(mark);
        }
        let Some(body) = body else {
            self.diag(Severity::Error, a, "expected a body in `#show`");
            self.skip_colon_tail();
            return;
        };
        flush(out, text);
        out.push(Node::Expr(Expr::Show {
            target,
            param,
            body,
        }));
    }

    fn reject_show(&mut self, a: usize, start: usize) {
        self.pos = start;
        self.diag(Severity::NotImplemented, a, "#show is not implemented");
        self.skip_show_rest();
    }

    /// Consume an unimplemented `#show` selector and transform. `self.pos`
    /// is just after the `show` keyword. The following paragraph stays.
    fn skip_show_rest(&mut self) {
        self.skip_hspace();
        if self.peek() == Some(b'"') {
            self.string(self.pos);
        } else if self.peek().map_or(false, is_ident_start) {
            let _ = self.ident();
            loop {
                self.skip_hspace();
                if self.peek() != Some(b'.') {
                    break;
                }
                self.pos += 1;
                let _ = self.ident();
                self.skip_hspace();
                if self.peek() == Some(b'(') {
                    self.pos += 1;
                    self.skip_balanced(b'(', b')');
                }
            }
        }
        self.skip_hspace();
        if self.peek() == Some(b':') {
            self.pos += 1;
            self.skip_show_transform();
        }
    }

    /// After the colon of an unimplemented show rule. A `set` rule, an
    /// `ident => rhs` transform (body may span lines), or the rest of
    /// this line.
    fn skip_show_transform(&mut self) {
        let line = self.pos;
        self.skip_ws();
        if self.at_word("set") {
            self.word("set");
            self.skip_ws();
            let _ = self.ident();
            self.skip_ws();
            if self.peek() == Some(b'(') {
                self.pos += 1;
                self.skip_balanced(b'(', b')');
            }
            return;
        }
        if self.ident().is_some() {
            self.skip_ws();
            if self.peek() == Some(b'=') && self.at(1) == Some(b'>') {
                self.pos += 2;
                let mark = self.lets.len();
                if self.show_rhs().is_none() {
                    self.pos = line;
                    self.skip_colon_tail();
                }
                self.lets.truncate(mark);
                return;
            }
        }
        self.pos = line;
        self.skip_colon_tail();
    }

    fn parse_let(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        self.skip_hspace();
        let Some(name) = self.ident() else {
            self.diag(Severity::Error, a, "expected a name after `#let`");
            self.skip_to_eol();
            return;
        };
        self.skip_hspace();
        if self.peek() == Some(b'(') {
            self.parse_let_fn(a, name, out, text);
            return;
        }
        if self.peek() != Some(b'=') {
            self.diag(Severity::Error, a, "expected `=` in `#let`");
            self.skip_to_eol();
            return;
        }
        self.pos += 1;
        match self.rhs() {
            Some(v) => {
                self.lets.push(name.clone());
                flush(out, text);
                out.push(Node::Expr(Expr::Let { name, value: v }));
                // A trailing `(..)` is a function call — not implemented.
                let save = self.pos;
                self.skip_hspace();
                if self.peek() == Some(b'(') {
                    self.pos += 1;
                    self.skip_balanced(b'(', b')');
                    self.diag(
                        Severity::NotImplemented,
                        a,
                        "function calls in `#let` are not implemented",
                    );
                } else {
                    self.pos = save;
                }
            }
            None => {
                self.diag(Severity::Error, a, "expected a value in `#let`");
                self.skip_to_eol();
            }
        }
    }

    /// `#let name(params) = [content] | expr`.
    fn parse_let_fn(&mut self, a: usize, name: String, out: &mut Vec<Node>, text: &mut String) {
        let params = self.fn_params();
        self.skip_hspace();
        if self.peek() != Some(b'=') {
            self.diag(Severity::Error, a, "expected `=` in `#let`");
            self.skip_to_eol();
            return;
        }
        self.pos += 1;
        let mark = self.lets.len();
        self.lets.push(name.clone());
        for p in &params {
            self.lets.push(p.name.clone());
        }
        let body = self.rhs();
        self.lets.truncate(mark);
        let Some(body) = body else {
            self.diag(Severity::Error, a, "expected a value in `#let`");
            self.skip_to_eol();
            return;
        };
        if !self.lets.iter().any(|n| n == &name) {
            self.lets.push(name.clone());
        }
        flush(out, text);
        out.push(Node::Expr(Expr::LetFn { name, params, body }));
    }

    /// Parameter list, including the surrounding parentheses.
    fn fn_params(&mut self) -> Vec<Param> {
        self.pos += 1;
        self.depth += 1;
        let params = self.fn_param_list();
        self.depth -= 1;
        params
    }

    fn fn_param_list(&mut self) -> Vec<Param> {
        if self.depth > 64 {
            self.diag(Severity::Error, self.pos, "parameters nested too deeply");
            self.skip_balanced(b'(', b')');
            return Vec::new();
        }
        let mut params = Vec::new();
        loop {
            let start = self.pos;
            self.skip_ws();
            match self.peek() {
                None => {
                    self.diag(Severity::Error, self.pos, "unclosed `(`");
                    break;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b'.') if self.at(1) == Some(b'.') => {
                    self.diag(
                        Severity::NotImplemented,
                        self.pos,
                        "parameter sinks are not implemented",
                    );
                    self.pos += 2;
                    while self.peek().map_or(false, is_ident_cont) {
                        self.pos += 1;
                    }
                    self.skip_param_tail();
                }
                Some(b'(') => {
                    self.diag(
                        Severity::NotImplemented,
                        self.pos,
                        "parameter destructuring is not implemented",
                    );
                    self.pos += 1;
                    self.skip_balanced(b'(', b')');
                    self.skip_param_tail();
                }
                _ => {
                    if let Some(pname) = self.ident() {
                    self.skip_ws();
                    let default = if self.peek() == Some(b':') {
                        self.pos += 1;
                        let save = self.pos;
                        match self.literal_default() {
                            Some(v) => Some(v),
                            None => {
                                self.diag(
                                    Severity::Error,
                                    save,
                                    "parameter default must be a literal",
                                );
                                if self.pos == save {
                                    let _ = self.rhs();
                                }
                                None
                            }
                        }
                    } else {
                        None
                    };
                    params.push(Param {
                        name: pname,
                        default,
                    });
                    self.skip_ws();
                    match self.peek() {
                        Some(b',') => self.pos += 1,
                        Some(b')') | None => {}
                        _ => {
                            self.diag(Severity::Error, self.pos, "expected `,` or `)`");
                            let before = self.pos;
                            self.recover_arg();
                            if self.pos == before
                                && !matches!(self.peek(), Some(b',') | Some(b')') | None)
                            {
                                self.pos += 1;
                            }
                            if self.peek() == Some(b',') {
                                self.pos += 1;
                            }
                        }
                    }
                    } else {
                        self.diag(Severity::Error, self.pos, "expected a parameter name");
                        let before = self.pos;
                        self.recover_arg();
                        if self.pos == before
                            && !matches!(self.peek(), Some(b',') | Some(b')') | None)
                        {
                            self.pos += 1;
                        }
                        if self.peek() == Some(b',') {
                            self.pos += 1;
                        }
                    }
                }
            }
            if self.pos == start {
                if self.eof() {
                    break;
                }
                self.pos += 1;
            }
        }
        params
    }

    fn skip_param_tail(&mut self) {
        self.skip_ws();
        if self.peek() == Some(b':') {
            self.pos += 1;
            let _ = self.rhs();
        }
        self.skip_ws();
        if self.peek() == Some(b',') {
            self.pos += 1;
        }
    }

    /// Literal allowed as a parameter default: int, float, string, length, bool, none.
    fn literal_default(&mut self) -> Option<Value> {
        self.skip_ws();
        match self.peek()? {
            b'"' => Some(Value::Str(self.string(self.pos))),
            c if c.is_ascii_digit()
                || (matches!(c, b'-' | b'+') && self.at(1).map_or(false, |d| d.is_ascii_digit()))
                || (c == b'.' && self.at(1).map_or(false, |d| d.is_ascii_digit())) =>
            {
                match self.number()? {
                    v @ (Value::Int(_) | Value::Float(_) | Value::Length(_)) => Some(v),
                    _ => None,
                }
            }
            c if is_ident_start(c) => {
                let save = self.pos;
                let n = self.ident()?;
                match n.as_str() {
                    "true" => Some(Value::Bool(true)),
                    "false" => Some(Value::Bool(false)),
                    "none" => Some(Value::None),
                    _ => {
                        self.pos = save;
                        None
                    }
                }
            }
            _ => None,
        }
    }

    /// `(expr, name: expr, ...)` for a user-function call.
    fn rhs_args(&mut self) -> Vec<RhsArg> {
        self.pos += 1;
        self.depth += 1;
        let args = self.rhs_arg_list();
        self.depth -= 1;
        args
    }

    fn rhs_arg_list(&mut self) -> Vec<RhsArg> {
        if self.depth > 64 {
            self.diag(Severity::Error, self.pos, "arguments nested too deeply");
            self.skip_balanced(b'(', b')');
            return Vec::new();
        }
        let mut args = Vec::new();
        loop {
            let start = self.pos;
            self.skip_ws();
            match self.peek() {
                None => {
                    self.diag(Severity::Error, self.pos, "unclosed `(`");
                    break;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                Some(b'.') if self.at(1) == Some(b'.') => {
                    self.diag(
                        Severity::NotImplemented,
                        self.pos,
                        "spread arguments are not implemented",
                    );
                    self.pos += 2;
                    self.recover_arg();
                    if self.peek() == Some(b',') {
                        self.pos += 1;
                    }
                }
                _ => {
                    let mut name = None;
                    let save = self.pos;
                    if let Some(n) = self.ident() {
                        self.skip_hspace();
                        if self.peek() == Some(b':') {
                            self.pos += 1;
                            name = Some(n);
                        } else {
                            self.pos = save;
                        }
                    }
                    if let Some(v) = self.rhs() {
                        args.push(RhsArg { name, value: v });
                    } else {
                        self.diag(Severity::Error, self.pos, "expected a value");
                        let before = self.pos;
                        self.recover_arg();
                        if self.pos == before
                            && !matches!(self.peek(), Some(b',') | Some(b')') | None)
                        {
                            self.pos += 1;
                        }
                    }
                    self.skip_ws();
                    if self.peek() == Some(b',') {
                        self.pos += 1;
                    } else if !matches!(self.peek(), Some(b')') | None) {
                        self.diag(Severity::Error, self.pos, "expected `,` or `)`");
                        let before = self.pos;
                        self.recover_arg();
                        if self.pos == before
                            && !matches!(self.peek(), Some(b',') | Some(b')') | None)
                        {
                            self.pos += 1;
                        }
                        if self.peek() == Some(b',') {
                            self.pos += 1;
                        }
                    }
                }
            }
            if self.pos == start {
                if self.eof() {
                    break;
                }
                self.pos += 1;
            }
        }
        args
    }

    /// Trailing `[content]`, allowing spaces before the bracket.
    fn trailing_content(&mut self) -> Option<Vec<Node>> {
        let save = self.pos;
        self.skip_hspace();
        if self.peek() == Some(b'[') {
            let bs = self.pos;
            self.pos += 1;
            Some(self.body(true, bs))
        } else {
            self.pos = save;
            None
        }
    }

    fn parse_set(&mut self, a: usize, out: &mut Vec<Node>, text: &mut String) {
        self.skip_hspace();
        let Some(target) = self.ident() else {
            self.diag(Severity::Error, a, "expected a target after `#set`");
            self.skip_to_eol();
            return;
        };
        self.skip_hspace();
        let args = if self.peek() == Some(b'(') {
            self.args()
        } else {
            self.diag(Severity::Error, a, "expected `(..)` after `#set` target");
            Vec::new()
        };
        flush(out, text);
        out.push(Node::Expr(Expr::Set { target, args }));
    }

    fn ident(&mut self) -> Option<String> {
        if !self.peek().map_or(false, is_ident_start) {
            return None;
        }
        let s = self.pos;
        while self.peek().map_or(false, is_ident_cont) {
            self.pos += 1;
        }
        Some(self.src[s..self.pos].to_string())
    }

    /// `( arg, name: arg, ... )` — consumes the `(` and `)`.
    fn args(&mut self) -> Vec<Arg> {
        self.pos += 1;
        self.depth += 1;
        let args = self.arg_list();
        self.depth -= 1;
        args
    }

    /// Arg list body after `(` is consumed. Nested `(x: v, ...)` groups
    /// become `Value::Dict`/`Value::Array` via `paren_value`.
    fn arg_list(&mut self) -> Vec<Arg> {
        if self.depth > 64 {
            self.diag(Severity::Error, self.pos, "arguments nested too deeply");
            self.skip_balanced(b'(', b')');
            return Vec::new();
        }
        let mut args = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => {
                    self.diag(Severity::Error, self.pos, "unclosed `(`");
                    break;
                }
                Some(b')') => {
                    self.pos += 1;
                    break;
                }
                _ => {}
            }
            let mut name = None;
            let save = self.pos;
            if let Some(n) = self.ident() {
                self.skip_hspace();
                if self.peek() == Some(b':') {
                    self.pos += 1;
                    name = Some(n);
                } else {
                    self.pos = save;
                }
            }
            self.skip_ws();
            if let Some(v) = self.value() {
                args.push(Arg { name, value: v });
            } else {
                self.diag(Severity::Error, self.pos, "expected a value");
                self.recover_arg();
            }
            self.skip_ws();
            match self.peek() {
                Some(b',') => {
                    self.pos += 1;
                }
                Some(b')') | None => {}
                _ => {
                    self.diag(Severity::Error, self.pos, "expected `,` or `)`");
                    self.recover_arg();
                }
            }
        }
        args
    }

    /// After a bad arg, skip to `,` or `)` at this depth.
    fn recover_arg(&mut self) {
        let mut d = 0i32;
        while let Some(c) = self.peek() {
            match c {
                b'(' | b'[' => d += 1,
                b')' | b']' => {
                    if d == 0 {
                        return;
                    }
                    d -= 1;
                }
                b',' if d == 0 => return,
                b'"' => {
                    self.pos += 1;
                    while let Some(c) = self.peek() {
                        self.pos += 1;
                        if c == b'"' || c == b'\n' {
                            break;
                        }
                    }
                    continue;
                }
                _ => {}
            }
            self.pos += 1;
        }
    }

    /// Consume through the `close` that balances an already-eaten `open`.
    fn skip_balanced(&mut self, open: u8, close: u8) {
        let mut d = 1i32;
        while let Some(c) = self.peek() {
            if c == b'"' {
                self.pos += 1;
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == b'"' || c == b'\n' {
                        break;
                    }
                }
                continue;
            }
            if c == open {
                d += 1;
            } else if c == close {
                self.pos += 1;
                d -= 1;
                if d == 0 {
                    return;
                }
                continue;
            }
            self.pos += 1;
        }
    }

    /// A literal value: string, number/length, bool/none/auto, `[content]`,
    /// a `(...)` array/dict/group, or a bare identifier (stored as
    /// `Value::Str`, e.g. `center`).
    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        match self.peek()? {
            b'"' => Some(Value::Str(self.string(self.pos))),
            b'(' => self.paren_value(),
            b'[' => {
                let bs = self.pos;
                self.pos += 1;
                Some(Value::Content(self.body(true, bs)))
            }
            c if c.is_ascii_digit()
                || (matches!(c, b'-' | b'+')
                    && self.at(1).map_or(false, |d| d.is_ascii_digit()))
                || (c == b'.' && self.at(1).map_or(false, |d| d.is_ascii_digit())) =>
            {
                self.number()
            }
            c if is_ident_start(c) => {
                let n = self.ident().unwrap();
                Some(match n.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "none" | "auto" => Value::None,
                    _ => Value::Str(n),
                })
            }
            _ => None,
        }
    }

    /// `"..."` string literal with backslash escapes.
    fn string(&mut self, a: usize) -> String {
        self.pos += 1;
        let mut s = String::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => {
                    self.diag(Severity::Error, a, "unclosed string");
                    break;
                }
                Some(b'"') => {
                    self.pos += 1;
                    break;
                }
                Some(b'\\') => {
                    self.pos += 1;
                    match self.peek() {
                        Some(b'n') => {
                            s.push('\n');
                            self.pos += 1;
                        }
                        Some(b't') => {
                            s.push('\t');
                            self.pos += 1;
                        }
                        Some(b'r') => {
                            s.push('\r');
                            self.pos += 1;
                        }
                        Some(_) => {
                            let ch = self.src[self.pos..].chars().next().unwrap();
                            s.push(ch);
                            self.pos += ch.len_utf8();
                        }
                        None => {
                            self.diag(Severity::Error, a, "unclosed string");
                            break;
                        }
                    }
                }
                Some(_) => {
                    let ch = self.src[self.pos..].chars().next().unwrap();
                    s.push(ch);
                    self.pos += ch.len_utf8();
                }
            }
        }
        s
    }

    /// Number with optional fraction and length unit (pt/mm/cm/in/em).
    fn number(&mut self) -> Option<Value> {
        let a = self.pos;
        if matches!(self.peek(), Some(b'-') | Some(b'+')) {
            self.pos += 1;
        }
        while self.peek().map_or(false, |c| c.is_ascii_digit()) {
            self.pos += 1;
        }
        let mut float = false;
        if self.peek() == Some(b'.') && self.at(1).map_or(false, |c| c.is_ascii_digit()) {
            float = true;
            self.pos += 1;
            while self.peek().map_or(false, |c| c.is_ascii_digit()) {
                self.pos += 1;
            }
        }
        let v: f64 = self.src[a..self.pos].parse().ok()?;
        let us = self.pos;
        while self.peek().map_or(false, |c| c.is_ascii_alphabetic()) {
            self.pos += 1;
        }
        let unit = &self.src[us..self.pos];
        let plain = || {
            if float {
                Value::Float(v)
            } else {
                Value::Int(v as i64)
            }
        };
        Some(match unit {
            "" => plain(),
            "pt" => Value::Length(Length::Pt(v)),
            "em" => Value::Length(Length::Em(v)),
            "mm" => Value::Length(Length::Pt(v * 2.83465)),
            "cm" => Value::Length(Length::Pt(v * 28.3465)),
            "in" => Value::Length(Length::Pt(v * 72.0)),
            // `1fr` column fractions — no Value variant exists, so they
            // are encoded as strings and recognized by table/grid eval.
            "fr" => Value::Str(format!("{v}fr")),
            _ => {
                self.diag(Severity::Warning, us, format!("unknown unit `{unit}`"));
                plain()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nodes(s: &str) -> Vec<Node> {
        parse(s).0
    }

    fn t(s: &str) -> Node {
        Node::Text(s.into())
    }

    fn list_depth(nodes: &[Node]) -> usize {
        nodes
            .iter()
            .map(|n| match n {
                Node::List { items, .. } => {
                    1 + items.iter().map(|it| list_depth(it)).max().unwrap_or(0)
                }
                _ => 0,
            })
            .max()
            .unwrap_or(0)
    }

    #[test]
    fn headings() {
        assert_eq!(
            nodes("= A"),
            vec![Node::Heading {
                level: 1,
                body: vec![t("A")]
            }]
        );
        assert_eq!(
            nodes("====== A"),
            vec![Node::Heading {
                level: 6,
                body: vec![t("A")]
            }]
        );
        assert_eq!(nodes("======= A"), vec![t("======= A")]);
        assert_eq!(nodes("=x"), vec![t("=x")]);
    }

    #[test]
    fn labels_and_refs_parse() {
        assert_eq!(
            nodes("= Intro <intro>"),
            vec![Node::Heading {
                level: 1,
                body: vec![t("Intro "), Node::Label("intro".into())],
            }]
        );
        let n = nodes("= Intro\n<intro>");
        assert!(matches!(n[0], Node::Heading { level: 1, .. }));
        assert!(matches!(&n[1], Node::Label(s) if s == "intro"));
        assert_eq!(
            nodes("See @intro now"),
            vec![t("See "), Node::Ref("intro".into()), t(" now")]
        );
        let (n, d) = parse("<a-b_c.d> @a-b_c.d");
        assert!(d.is_empty(), "{d:?}");
        assert!(n.iter().any(|x| matches!(x, Node::Label(s) if s == "a-b_c.d")));
        assert!(n.iter().any(|x| matches!(x, Node::Ref(s) if s == "a-b_c.d")));
        assert!(!nodes("<a:b>").iter().any(|x| matches!(x, Node::Label(_))));
        // A dot is part of the name only when a label character follows it.
        assert_eq!(
            nodes("Dot @intro."),
            vec![t("Dot "), Node::Ref("intro".into()), t(".")]
        );
        assert_eq!(
            nodes("@intro.."),
            vec![Node::Ref("intro".into()), t("..")]
        );
        assert_eq!(
            nodes("@fig.1 @a.b. @a..b"),
            vec![
                Node::Ref("fig.1".into()),
                t(" "),
                Node::Ref("a.b".into()),
                t(". "),
                Node::Ref("a..b".into()),
            ]
        );
        assert_eq!(
            nodes("@details!"),
            vec![Node::Ref("details".into()), t("!")]
        );
    }


    #[test]
    fn strong_emph_nesting() {
        assert_eq!(
            nodes("*a _b_ c*"),
            vec![Node::Strong(vec![
                t("a "),
                Node::Emph(vec![t("b")]),
                t(" c")
            ])]
        );
        assert_eq!(
            nodes("_a *b* c_"),
            vec![Node::Emph(vec![
                t("a "),
                Node::Strong(vec![t("b")]),
                t(" c")
            ])]
        );
    }

    #[test]
    fn unclosed_marker() {
        assert_eq!(nodes("a *b"), vec![t("a *b")]);
        assert_eq!(nodes("a_b_c"), vec![t("a_b_c")]);
        assert_eq!(nodes("*a\n\nb*"), vec![t("*a"), Node::Parbreak, t("b*")]);
    }

    #[test]
    fn lists() {
        assert_eq!(
            nodes("- a\n- b\n\n+ c\n+ d"),
            vec![
                Node::List {
                    ordered: false,
                    items: vec![vec![t("a")], vec![t("b")]],
                },
                Node::Parbreak,
                Node::List {
                    ordered: true,
                    items: vec![vec![t("c")], vec![t("d")]],
                },
            ]
        );
        // continuation line stays in the item; a non-marker indent is not a list
        assert_eq!(
            nodes("- a\n  cont\n- c"),
            vec![Node::List {
                ordered: false,
                items: vec![vec![t("a cont")], vec![t("c")]],
            }]
        );
        // indented marker nests; the following column-0 marker is a sibling
        assert_eq!(
            nodes("- a\n  cont\n  - b\n- c"),
            vec![Node::List {
                ordered: false,
                items: vec![
                    vec![
                        t("a cont"),
                        Node::List {
                            ordered: false,
                            items: vec![vec![t("b")]],
                        },
                    ],
                    vec![t("c")],
                ],
            }]
        );
    }

    #[test]
    fn nested_and_mixed_lists() {
        assert_eq!(
            nodes("- a\n  - b\n- c"),
            vec![Node::List {
                ordered: false,
                items: vec![
                    vec![
                        t("a"),
                        Node::List {
                            ordered: false,
                            items: vec![vec![t("b")]],
                        },
                    ],
                    vec![t("c")],
                ],
            }]
        );
        assert_eq!(
            nodes("- a\n  + b\n  - c\n+ d"),
            vec![
                Node::List {
                    ordered: false,
                    items: vec![vec![
                        t("a"),
                        Node::List {
                            ordered: true,
                            items: vec![vec![t("b")]],
                        },
                        Node::List {
                            ordered: false,
                            items: vec![vec![t("c")]],
                        },
                    ]],
                },
                Node::List {
                    ordered: true,
                    items: vec![vec![t("d")]],
                },
            ]
        );
        // a tab is enough extra indent
        assert_eq!(
            nodes("- a\n\t- b"),
            vec![Node::List {
                ordered: false,
                items: vec![vec![
                    t("a"),
                    Node::List {
                        ordered: false,
                        items: vec![vec![t("b")]],
                    },
                ]],
            }]
        );
        // blank line ends the list
        assert_eq!(
            nodes("- a\n\n- b"),
            vec![
                Node::List {
                    ordered: false,
                    items: vec![vec![t("a")]],
                },
                Node::Parbreak,
                Node::List {
                    ordered: false,
                    items: vec![vec![t("b")]],
                },
            ]
        );
        // a comment between items does not split the list
        assert_eq!(
            nodes("- a\n// c\n- b"),
            vec![Node::List {
                ordered: false,
                items: vec![vec![t("a")], vec![t("b")]],
            }]
        );
    }

    #[test]
    fn list_depth_limit() {
        let mut src = String::new();
        for i in 0..6 {
            for _ in 0..i {
                src.push(' ');
            }
            src.push_str("- x\n");
        }
        let (n, d) = parse(&src);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(list_depth(&n), 6);

        src.push_str("      - y\n");
        let (n, d) = parse(&src);
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Warning && x.message.contains("nested")),
            "{d:?}"
        );
        assert_eq!(list_depth(&n), 6);
        fn has_literal(nodes: &[Node]) -> bool {
            nodes.iter().any(|n| match n {
                Node::Text(t) => t.contains("- y"),
                Node::List { items, .. } => items.iter().any(|it| has_literal(it)),
                _ => false,
            })
        }
        assert!(has_literal(&n));
    }

    #[test]
    fn term_lists() {
        assert_eq!(
            nodes("/ *A*: a _b_ `c`"),
            vec![Node::Terms {
                items: vec![TermEntry {
                    term: vec![Node::Strong(vec![t("A")])],
                    desc: vec![
                        t("a "),
                        Node::Emph(vec![t("b")]),
                        t(" "),
                        Node::Raw {
                            text: "c".into(),
                            block: false,
                        },
                    ],
                }],
            }]
        );
        assert_eq!(
            nodes("/ a: b\n  c _d_\n/ e: f"),
            vec![Node::Terms {
                items: vec![
                    TermEntry {
                        term: vec![t("a")],
                        desc: vec![t("b c "), Node::Emph(vec![t("d")])],
                    },
                    TermEntry {
                        term: vec![t("e")],
                        desc: vec![t("f")],
                    },
                ],
            }]
        );
        // colon inside raw does not end the term; an indented `/` is literal
        assert_eq!(
            nodes("/ see `a:b`: done\n  / nested: no"),
            vec![Node::Terms {
                items: vec![TermEntry {
                    term: vec![
                        t("see "),
                        Node::Raw {
                            text: "a:b".into(),
                            block: false,
                        },
                    ],
                    desc: vec![t("done / nested: no")],
                }],
            }]
        );
        let (n, d) = parse("/ no colon");
        assert_eq!(n, vec![t("/ no colon")]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        assert!(d[0].message.contains(':'));
    }

    #[test]
    fn comments() {
        assert_eq!(nodes("a // hi\nb"), vec![t("a b")]);
        assert_eq!(nodes("a /* x /* y */ z */ b"), vec![t("a b")]);
        let (n, d) = parse("a /* never closed");
        assert_eq!(n, vec![t("a ")]);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn escapes_and_breaks() {
        assert_eq!(nodes("\\* \\` \\# \\\\ \\/"), vec![t("* ` # \\ /")]);
        assert_eq!(nodes("a\\ b"), vec![t("a"), Node::LineBreak, t("b")]);
        assert_eq!(nodes("a\\\nb"), vec![t("a"), Node::LineBreak, t("b")]);
    }

    #[test]
    fn raw() {
        assert_eq!(
            nodes("`x`"),
            vec![Node::Raw {
                text: "x".into(),
                block: false
            }]
        );
        assert_eq!(
            nodes("```rust\nfn main() {}\n```"),
            vec![Node::Raw {
                text: "fn main() {}".into(),
                block: true
            }]
        );
        let (n, d) = parse("```\nabc");
        assert_eq!(
            n,
            vec![Node::Raw {
                text: "abc".into(),
                block: true
            }]
        );
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
    }

    #[test]
    fn paragraphs() {
        assert_eq!(nodes("a\nb"), vec![t("a b")]);
        assert_eq!(nodes("a\n\n\nb"), vec![t("a"), Node::Parbreak, t("b")]);
    }

    #[test]
    fn exprs() {
        let (n, d) = parse("#set page(paper: \"a4\", margin: 2cm)");
        assert!(d.is_empty());
        assert_eq!(
            n,
            vec![Node::Expr(Expr::Set {
                target: "page".into(),
                args: vec![
                    Arg {
                        name: Some("paper".into()),
                        value: Value::Str("a4".into())
                    },
                    Arg {
                        name: Some("margin".into()),
                        value: Value::Length(Length::Pt(2.0 * 28.3465)),
                    },
                ],
            })]
        );

        assert_eq!(
            nodes("#let x = \"hi\"\n#x"),
            vec![
                Node::Expr(Expr::Let {
                    name: "x".into(),
                    value: Rhs::Lit(Value::Str("hi".into())),
                }),
                t(" "),
                Node::Expr(Expr::Var { name: "x".into() }),
            ]
        );

        assert_eq!(
            nodes("#lorem(10)"),
            vec![Node::Expr(Expr::Func {
                name: "lorem".into(),
                args: vec![Arg {
                    name: None,
                    value: Value::Int(10)
                }],
                content: None,
            })]
        );

        assert_eq!(
            nodes("#text(size: 9pt)[hi]"),
            vec![Node::Expr(Expr::Func {
                name: "text".into(),
                args: vec![Arg {
                    name: Some("size".into()),
                    value: Value::Length(Length::Pt(9.0)),
                }],
                content: Some(vec![t("hi")]),
            })]
        );

        assert_eq!(
            nodes("#align(center)[x]"),
            vec![Node::Expr(Expr::Func {
                name: "align".into(),
                args: vec![Arg {
                    name: None,
                    value: Value::Str("center".into())
                }],
                content: Some(vec![t("x")]),
            })]
        );

        assert_eq!(
            nodes("#12 #1.5pt #\"s\""),
            vec![
                Node::Expr(Expr::Lit(Value::Int(12))),
                t(" "),
                Node::Expr(Expr::Lit(Value::Length(Length::Pt(1.5)))),
                t(" "),
                Node::Expr(Expr::Lit(Value::Str("s".into()))),
            ]
        );
    }

    #[test]
    fn set_margin_dict() {
        // `margin: (x: 1in, top: 2cm)` -> Value::Dict of named pairs.
        let (n, d) = parse("#set page(margin: (x: 1in, top: 2cm))");
        assert!(d.is_empty());
        let Node::Expr(Expr::Set { args, .. }) = &n[0] else {
            panic!()
        };
        assert_eq!(args[0].name.as_deref(), Some("margin"));
        assert_eq!(
            args[0].value,
            Value::Dict(vec![
                ("x".into(), Value::Length(Length::Pt(72.0))),
                ("top".into(), Value::Length(Length::Pt(2.0 * 28.3465))),
            ])
        );
    }

    #[test]
    fn import_include() {
        assert_eq!(
            nodes("#import \"lib.typ\""),
            vec![Node::Expr(Expr::Import {
                path: "lib.typ".into(),
                items: vec![],
                all: false,
            })]
        );
        assert_eq!(
            nodes("#import \"lib.typ\": a, b"),
            vec![Node::Expr(Expr::Import {
                path: "lib.typ".into(),
                items: vec!["a".into(), "b".into()],
                all: false,
            })]
        );
        assert_eq!(
            nodes("#import \"lib.typ\": *"),
            vec![Node::Expr(Expr::Import {
                path: "lib.typ".into(),
                items: vec![],
                all: true,
            })]
        );
        assert_eq!(
            nodes("#import \"@preview/cetz:0.3.1\": canvas"),
            vec![Node::Expr(Expr::Import {
                path: "@preview/cetz:0.3.1".into(),
                items: vec!["canvas".into()],
                all: false,
            })]
        );
        assert_eq!(
            nodes("#include \"ch.typ\""),
            vec![Node::Expr(Expr::Func {
                name: "include".into(),
                args: vec![Arg {
                    name: None,
                    value: Value::Str("ch.typ".into())
                }],
                content: None,
            })]
        );
    }

    #[test]
    fn ifs() {
        // bare `#if x` desugars to `x != none`
        let (n, d) = parse("#if x [yes]");
        assert!(d.is_empty());
        assert_eq!(
            n,
            vec![Node::Expr(Expr::If {
                arms: vec![(
                    Cond::Cmp(Cmp {
                        lhs: Rhs::Var("x".into()),
                        op: CmpOp::Ne,
                        rhs: Rhs::Lit(Value::None),
                    }),
                    vec![t("yes")],
                )],
                els: None,
            })]
        );

        let (n, d) = parse("#if a == 1 [x] else if not b [y] else [z]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::If { arms, els }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(arms.len(), 2);
        let Cond::Cmp(c0) = &arms[0].0 else {
            panic!("{:?}", arms[0].0)
        };
        assert_eq!(c0.op, CmpOp::Eq);
        assert_eq!(c0.rhs, Rhs::Lit(Value::Int(1)));
        assert!(matches!(arms[1].0, Cond::Not(_)));
        assert_eq!(els.as_ref().unwrap()[0], t("z"));

        for (src, op) in [
            ("#if a != b [x]", CmpOp::Ne),
            ("#if a < b [x]", CmpOp::Lt),
            ("#if a <= b [x]", CmpOp::Le),
            ("#if a > b [x]", CmpOp::Gt),
            ("#if a >= b [x]", CmpOp::Ge),
        ] {
            let (n, d) = parse(src);
            assert!(d.is_empty(), "{src}: {d:?}");
            let Node::Expr(Expr::If { arms, .. }) = &n[0] else {
                panic!("{src}")
            };
            let Cond::Cmp(c) = &arms[0].0 else {
                panic!("{src}")
            };
            assert_eq!(c.op, op, "{src}");
        }
    }

    #[test]
    fn while_and_bool_combinators() {
        let (n, d) = parse("#while n < 3 [x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::While { cond, body }) = &n[0] else {
            panic!("{n:?}")
        };
        let Cond::Cmp(c) = cond else { panic!("{cond:?}") };
        assert_eq!(c.op, CmpOp::Lt);
        assert_eq!(*body, vec![t("x")]);

        let (n, d) = parse("#if a or b and c [x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::If { arms, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        let Cond::Or(l, r) = &arms[0].0 else {
            panic!("{:?}", arms[0].0)
        };
        assert!(matches!(l.as_ref(), Cond::Cmp(_)));
        assert!(matches!(r.as_ref(), Cond::And(_, _)));

        let (n, d) = parse("#if (a or b) and c [x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::If { arms, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        let Cond::And(l, _) = &arms[0].0 else {
            panic!("{:?}", arms[0].0)
        };
        assert!(matches!(l.as_ref(), Cond::Or(_, _)));

        let (n, d) = parse("#if not a and b [x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::If { arms, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        let Cond::And(l, _) = &arms[0].0 else {
            panic!("{:?}", arms[0].0)
        };
        assert!(matches!(l.as_ref(), Cond::Not(_)));

        let (n, d) = parse("#if (1) == 2 [x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::If { arms, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        let Cond::Cmp(c) = &arms[0].0 else {
            panic!("{:?}", arms[0].0)
        };
        assert_eq!(c.op, CmpOp::Eq);
        assert_eq!(c.lhs, Rhs::Lit(Value::Int(1)));
    }

    #[test]
    fn method_call_rhs() {
        let (n, d) = parse("#let n = xs.len()\n#let y = xs.at(-1)\n#let z = m.x");
        assert!(d.is_empty(), "{d:?}");
        let lets: Vec<&Rhs> = n
            .iter()
            .filter_map(|n| {
                if let Node::Expr(Expr::Let { value, .. }) = n {
                    Some(value)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(lets.len(), 3);
        let Rhs::Call { method, args, .. } = lets[0] else {
            panic!("{lets:?}")
        };
        assert_eq!(method, "len");
        assert!(args.is_empty());
        let Rhs::Call { method, args, .. } = lets[1] else {
            panic!("{lets:?}")
        };
        assert_eq!(method, "at");
        assert_eq!(args, &[Rhs::Lit(Value::Int(-1))]);
        let Rhs::Field { name, .. } = lets[2] else {
            panic!("{lets:?}")
        };
        assert_eq!(name, "x");

        let (n, d) = parse("#xs.at(i)");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Rhs(Rhs::Call { method, args, .. })) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(method, "at");
        assert_eq!(args, &[Rhs::Var("i".into())]);
    }

    #[test]
    fn fors() {
        let (n, d) = parse("#for x in xs [#x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::For { vars, iter, body }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(vars, &["x"]);
        assert_eq!(*iter, Rhs::Var("xs".into()));
        assert_eq!(*body, vec![Node::Expr(Expr::Var { name: "x".into() })]);

        let (n, d) = parse("#for (k, v) in dict [#k = #v]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::For { vars, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(vars, &["k", "v"]);

        // iterating an array literal
        let (n, d) = parse("#for x in (1, 2) [#x]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::For { iter, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(
            *iter,
            Rhs::Lit(Value::Array(vec![Value::Int(1), Value::Int(2)]))
        );
    }

    #[test]
    fn let_rhs_literals() {
        let (n, d) = parse(
            "#let n = 1 + 2\n#let a = (1, 2pt, \"s\")\n#let d = (k: 1)\n#let g = (5,)\n#let h = (5)\n#let f = m.x",
        );
        assert!(d.is_empty(), "{d:?}");
        // `\n` after each `#let` becomes a separating space Text node.
        let lets: Vec<&Rhs> = n
            .iter()
            .filter_map(|n| {
                if let Node::Expr(Expr::Let { value, .. }) = n {
                    Some(value)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(lets.len(), 6);
        let Rhs::Bin {
            op: ArithOp::Add, ..
        } = lets[0]
        else {
            panic!("{lets:?}")
        };
        let Rhs::Lit(Value::Array(v)) = lets[1] else {
            panic!("{lets:?}")
        };
        assert_eq!(v.len(), 3);
        let Rhs::Lit(Value::Dict(p)) = lets[2] else {
            panic!("{lets:?}")
        };
        assert_eq!(p, &[("k".into(), Value::Int(1))]);
        let Rhs::Lit(Value::Array(v)) = lets[3] else {
            panic!("{lets:?}")
        };
        assert_eq!(v, &[Value::Int(5)]);
        let Rhs::Lit(Value::Int(5)) = lets[4] else {
            panic!("{lets:?}")
        };
        let Rhs::Field { name, .. } = lets[5] else {
            panic!("{lets:?}")
        };
        assert_eq!(name, "x");
    }

    #[test]
    fn markup_bin() {
        let (n, d) = parse("#let a = 1\n#a + 2 end");
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            n[2],
            Node::Expr(Expr::Bin {
                lhs: Rhs::Var("a".into()),
                op: ArithOp::Add,
                rhs: Rhs::Lit(Value::Int(2)),
            })
        );
        assert_eq!(n[3], t(" end"));
    }

    #[test]
    fn image_args() {
        let (n, d) = parse("#image(\"pic.jpg\", width: 100pt, height: 2cm)");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Func { name, args, content }) = &n[0] else {
            panic!("{n:?}");
        };
        assert_eq!(name, "image");
        assert!(content.is_none());
        assert_eq!(args.len(), 3);
        assert_eq!(args[0].name, None);
        assert_eq!(args[0].value, Value::Str("pic.jpg".into()));
        assert_eq!(args[1].name.as_deref(), Some("width"));
        assert_eq!(args[1].value, Value::Length(Length::Pt(100.0)));
        assert_eq!(args[2].name.as_deref(), Some("height"));
        match args[2].value {
            Value::Length(Length::Pt(p)) => assert!((p - 56.6929).abs() < 0.01, "{p}"),
            ref other => panic!("{other:?}"),
        }
        let (n, d) = parse("#image(\"a.jpeg\", height: 12pt)");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Func { args, .. }) = &n[0] else {
            panic!("{n:?}");
        };
        assert_eq!(args[0].value, Value::Str("a.jpeg".into()));
        assert_eq!(args[1].name.as_deref(), Some("height"));
    }

    #[test]
    fn stack_content_blocks_stay_positional() {
        let (n, d) = parse("#stack(dir: ttb, spacing: 4pt)[a][b][c]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Func { name, args, content }) = &n[0] else {
            panic!("{n:?}");
        };
        assert_eq!(name, "stack");
        assert!(content.is_none());
        let blocks: Vec<_> = args.iter().filter(|a| a.name.is_none()).collect();
        assert_eq!(blocks.len(), 3);
        for (b, word) in blocks.iter().zip(["a", "b", "c"]) {
            match &b.value {
                Value::Content(ns) => assert_eq!(ns, &vec![Node::Text(word.into())]),
                other => panic!("{other:?}"),
            }
        }
        let (n, d) = parse("#emph[x][y]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Func { name, content, .. }) = &n[0] else {
            panic!("{n:?}");
        };
        assert_eq!(name, "emph");
        assert_eq!(content.as_ref().map(|c| c.len()), Some(1));
        assert_eq!(n[1], Node::Text("[y]".into()));
    }

    #[test]
    fn let_functions() {
        let (n, d) = parse("#let f(x) = x * 2");
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            n,
            vec![Node::Expr(Expr::LetFn {
                name: "f".into(),
                params: vec![Param {
                    name: "x".into(),
                    default: None,
                }],
                body: Rhs::Bin {
                    lhs: Box::new(Rhs::Var("x".into())),
                    op: ArithOp::Mul,
                    rhs: Box::new(Rhs::Lit(Value::Int(2))),
                },
            })]
        );

        let (n, d) = parse("#let f(name: \"Ada\") = name");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::LetFn { params, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(params[0].default, Some(Value::Str("Ada".into())));

        let n = nodes("#let f(x) = x\n#f(1 + 2)");
        let call = n.iter().find_map(|node| match node {
            Node::Expr(Expr::Call { name, args, content }) => Some((name, args, content)),
            _ => None,
        });
        let (name, args, content) = call.expect("call");
        assert_eq!(name, "f");
        assert!(content.is_none());
        assert_eq!(args.len(), 1);
        assert!(args[0].name.is_none());
        assert_eq!(
            args[0].value,
            Rhs::Bin {
                lhs: Box::new(Rhs::Lit(Value::Int(1))),
                op: ArithOp::Add,
                rhs: Box::new(Rhs::Lit(Value::Int(2))),
            }
        );

        let n = nodes("#let f(x) = x\n#f[hi]");
        let Node::Expr(Expr::Call { content, args, .. }) = n.last().unwrap() else {
            panic!("{n:?}")
        };
        assert!(args.is_empty());
        let c = content.as_ref().unwrap();
        assert!(c.iter().any(|node| matches!(node, Node::Text(s) if s == "hi")));

        let n = nodes("#let f(text) = [#text]\n#text");
        let Node::Expr(Expr::LetFn { body, .. }) = &n[0] else {
            panic!("{n:?}")
        };
        let Rhs::Lit(Value::Content(body)) = body else {
            panic!("{body:?}")
        };
        assert!(body.iter().any(|node| matches!(
            node,
            Node::Expr(Expr::Var { name }) if name == "text"
        )));
        assert!(matches!(
            n.last(),
            Some(Node::Expr(Expr::Func { name, .. })) if name == "text"
        ));
    }

    #[test]
    fn not_implemented() {
        let (n, d) = parse("$x^2$");
        assert!(n.is_empty());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::NotImplemented);

        for (src, what) in [
            ("#footnote[..]", "#footnote"),
            ("1. item", "numbered"),
            ("#show: it => it", "#show"),
            ("#break", "#break"),
            ("#continue", "#continue"),
        ] {
            let (_, d) = parse(src);
            assert!(!d.is_empty(), "{src}");
            assert_eq!(d[0].severity, Severity::NotImplemented, "{src} ({what})");
        }
    }

    fn contains_text(nodes: &[Node], needle: &str) -> bool {
        nodes.iter().any(|n| match n {
            Node::Text(s) => s.contains(needle),
            Node::Strong(body) | Node::Emph(body) => contains_text(body, needle),
            Node::Heading { body, .. } => contains_text(body, needle),
            Node::List { items, .. } => items.iter().any(|item| contains_text(item, needle)),
            Node::Terms { items } => items.iter().any(|item| {
                contains_text(&item.term, needle) || contains_text(&item.desc, needle)
            }),
            _ => false,
        })
    }

    #[test]
    fn show_rule_parses() {
        let (n, d) = parse("#show heading: it => [Chapter #it]");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Show { target, param, body }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(target, "heading");
        assert_eq!(param, "it");
        let Rhs::Lit(Value::Content(body)) = body else {
            panic!("{body:?}")
        };
        assert!(body.iter().any(|node| matches!(node, Node::Text(s) if s.contains("Chapter"))));
        assert!(body.iter().any(|node| matches!(
            node,
            Node::Expr(Expr::Var { name }) if name == "it"
        )));

        let (n, d) = parse("#show emph: body => body");
        assert!(d.is_empty(), "{d:?}");
        let Node::Expr(Expr::Show { target, param, body }) = &n[0] else {
            panic!("{n:?}")
        };
        assert_eq!(target, "emph");
        assert_eq!(param, "body");
        assert_eq!(body, &Rhs::Var("body".into()));

        for target in ["strong", "link", "figure"] {
            let (n, d) = parse(&format!("#show {target}: el => el"));
            assert!(d.is_empty(), "{target} {d:?}");
            assert!(matches!(
                &n[0],
                Node::Expr(Expr::Show { target: t, .. }) if t == target
            ));
        }

        // The rule itself is not page text.
        let (n, d) = parse("#show heading: it => it\nHello");
        assert!(d.is_empty(), "{d:?}");
        assert!(!contains_text(&n, "heading"), "{n:?}");
        assert!(!contains_text(&n, "it"), "{n:?}");
        assert!(contains_text(&n, "Hello"), "{n:?}");
    }

    #[test]
    fn not_implemented_is_not_printed() {
        let src = "#show quote: it => it";
        let (n, d) = parse(src);
        assert!(
            d.iter().any(|x| {
                x.severity == Severity::NotImplemented
                    && x.span.map(|sp| crate::ir::line_col(src, sp.start)) == Some((1, 1))
            }),
            "{d:?}"
        );
        assert!(!contains_text(&n, "quote"), "{n:?}");
        assert!(!contains_text(&n, "it"), "{n:?}");

        let src = "#show quote: it => it\nHello";
        let (n, d) = parse(src);
        assert!(d.iter().any(|x| {
            x.severity == Severity::NotImplemented && x.span.is_some()
        }));
        assert!(!contains_text(&n, "quote"), "{n:?}");
        assert!(contains_text(&n, "Hello"), "{n:?}");

        let src = "#show quote: it => [\nsecret\n]\nNext";
        let (n, d) = parse(src);
        assert!(d.iter().any(|x| x.severity == Severity::NotImplemented && x.span.is_some()));
        assert!(!contains_text(&n, "secret"), "{n:?}");
        assert!(!contains_text(&n, "quote"), "{n:?}");
        assert!(contains_text(&n, "Next"), "{n:?}");

        let src = "#show heading.where(level: 1): it => it\nHello";
        let (n, d) = parse(src);
        assert!(d.iter().any(|x| x.severity == Severity::NotImplemented && x.span.is_some()));
        assert!(!contains_text(&n, "heading"), "{n:?}");
        assert!(!contains_text(&n, "where"), "{n:?}");
        assert!(!contains_text(&n, "it"), "{n:?}");
        assert!(contains_text(&n, "Hello"), "{n:?}");

        let src = "#show heading: set text(size: 12pt)\nHello";
        let (n, d) = parse(src);
        assert!(d.iter().any(|x| x.severity == Severity::NotImplemented && x.span.is_some()));
        assert!(!contains_text(&n, "heading"), "{n:?}");
        assert!(!contains_text(&n, "text"), "{n:?}");
        assert!(contains_text(&n, "Hello"), "{n:?}");

        let (n, d) = parse("$x$");
        assert!(d.iter().any(|x| x.severity == Severity::NotImplemented && x.span.is_some()));
        assert!(!contains_text(&n, "x"), "{n:?}");

        let (n, d) = parse("hello");
        assert!(d.iter().all(|x| x.severity != Severity::NotImplemented));
        assert!(contains_text(&n, "hello"));
    }

    #[test]
    fn no_panics_on_malformed() {
        for src in [
            "",
            "#",
            "# ",
            "##",
            "#(",
            "#f(",
            "#f(x",
            "#let",
            "#let 1",
            "#let x",
            "#let x =",
            "#let f(",
            "#let f()",
            "#let f() =",
            "#let f(x: ) = 1",
            "#let f(..) = 1",
            "#let f((a)) = 1",
            "#set",
            "#set x",
            "\"",
            "`",
            "```",
            "*",
            "_",
            "$",
            "<",
            "@",
            "\\",
            "/",
            "//",
            "/*",
            "[",
            "]",
            "-",
            "+",
            "=",
            "=======",
            "#f[#f[#f[x]",
            "((((",
            "*a _b `c",
            "#12.5.3",
            "#1xx",
            "a\\\r\nb",
            "- a\n- \n- c",
            "#strong[",
            "#show",
            "#show:",
            "#show heading",
            "#show heading:",
            "#show heading: it",
            "#show heading: it =>",
            "#show quote: it => it",
            "#while",
            "#while [",
            "#if a and",
            "#if (",
            "#xs.len(",
            "\u{1f600} *é_ \u{4e2d}\u{6587}",
        ] {
            let _ = parse(src);
        }
        // deep nesting stays bounded
        let deep = "#f[".repeat(2000) + &"]".repeat(2000);
        let _ = parse(&deep);
    }
}
