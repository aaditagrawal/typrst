//! Markup parser: source text -> Vec<Node> (see ir.rs / SPEC.md).
use crate::ir::{Arg, ArithOp, Cmp, CmpOp, Diag, Expr, Length, Node, Rhs, Severity, Span, Value};

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
];

/// Bare `#word` forms that are recognized but not implemented (rather
/// than treated as variable references, which unknown `#ident`s become).
const NOT_IMPL_BARE: &[&str] = &[
    "show", "context", "state", "counter", "place", "figure", "link", "ref", "while", "else", "in",
    "not", "and", "or", "return", "break", "continue",
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
    c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.' | b':')
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
fn rhs_expr(r: Rhs) -> Expr {
    match r {
        Rhs::Lit(v) => Expr::Lit(v),
        Rhs::Bin { lhs, op, rhs } => Expr::Bin {
            lhs: *lhs,
            op,
            rhs: *rhs,
        },
        other => Expr::Var {
            name: rhs_name(&other).unwrap_or_default(),
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
        if self.depth > 250 {
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
                self.list(out, text, m == b'+', in_bracket);
                true
            }
            Some(b'/') if matches!(self.at(1), Some(b' ') | Some(b'\t')) => {
                let a = self.pos;
                self.skip_to_eol();
                self.diag(
                    Severity::NotImplemented,
                    a,
                    "term lists are not implemented",
                );
                true
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

    /// `- ` / `+ ` list. Items end at the next column-0 marker, a blank
    /// line, a heading, or a fence; indented markers are literal text.
    fn list(&mut self, out: &mut Vec<Node>, text: &mut String, ordered: bool, in_bracket: bool) {
        let marker = if ordered { b'+' } else { b'-' };
        let mut items = Vec::new();
        loop {
            self.pos += 1;
            self.skip_hspace();
            let mut nodes: Vec<Node> = Vec::new();
            let mut t = String::new();
            loop {
                self.inline(&mut nodes, &mut t, in_bracket, None);
                let gs = self.pos;
                let mut nls = 0;
                let mut stop = false;
                loop {
                    match self.peek() {
                        Some(b'\n') => {
                            self.pos += 1;
                            nls += 1;
                            if self.peek() == Some(marker)
                                && matches!(self.at(1), Some(b' ') | Some(b'\t'))
                            {
                                stop = true; // next item, keep pos at marker
                                break;
                            }
                            if self.block_at(self.pos) {
                                stop = true;
                                break;
                            }
                        }
                        Some(b' ') | Some(b'\t') | Some(b'\r') => self.pos += 1,
                        Some(b'/') if self.at(1) == Some(b'/') => self.line_comment(),
                        Some(b'/') if self.at(1) == Some(b'*') => self.block_comment(),
                        _ => break,
                    }
                }
                if stop {
                    if !(self.peek() == Some(marker)
                        && matches!(self.at(1), Some(b' ') | Some(b'\t')))
                    {
                        self.pos = gs; // let body() recount the boundary
                    }
                    break;
                }
                if self.eof() || nls >= 2 || (in_bracket && self.peek() == Some(b']')) {
                    self.pos = gs;
                    break;
                }
                if nls > 0 {
                    pad(&mut t, &nodes);
                }
            }
            flush(&mut nodes, &mut t);
            items.push(nodes);
            if !(self.peek() == Some(marker) && matches!(self.at(1), Some(b' ') | Some(b'\t'))) {
                break;
            }
        }
        flush(out, text);
        out.push(Node::List { ordered, items });
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
                    if !self.label() {
                        text.push('<');
                        self.pos += 1;
                    }
                }
                b'@' => {
                    if !self.at_ref() {
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

    /// `<name>` label.
    fn label(&mut self) -> bool {
        let a = self.pos;
        let s = self.src.as_bytes();
        let mut i = self.pos + 1;
        while i < s.len() && is_label(s[i]) {
            i += 1;
        }
        if i > a + 1 && s.get(i) == Some(&b'>') {
            self.pos = i + 1;
            self.diag(Severity::NotImplemented, a, "labels are not implemented");
            true
        } else {
            false
        }
    }

    /// `@name` reference.
    fn at_ref(&mut self) -> bool {
        let a = self.pos;
        let s = self.src.as_bytes();
        let mut i = self.pos + 1;
        while i < s.len() && is_label(s[i]) {
            i += 1;
        }
        if i > a + 1 {
            self.pos = i;
            self.diag(
                Severity::NotImplemented,
                a,
                "references are not implemented",
            );
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
        if matches!(self.peek(), Some(b'(') | Some(b'[')) {
            let implemented = IMPLEMENTED.contains(&name.as_str()) || bound;
            let args = if self.peek() == Some(b'(') {
                self.args()
            } else {
                Vec::new()
            };
            let content = if self.peek() == Some(b'[') {
                let bs = self.pos;
                self.pos += 1;
                Some(self.body(true, bs))
            } else {
                None
            };
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
            // `#var`, `#var.field`, `#a + b`. Unknown names still parse as
            // `Expr::Var` — eval reports "undefined variable" (this also
            // covers names bound via `#import "..": *`).
            let mut dotted = name;
            let mut lhs = Rhs::Var(dotted.clone());
            while self.peek() == Some(b'.') && self.at(1).map_or(false, is_ident_start) {
                self.pos += 1;
                let f = self.ident().unwrap();
                dotted.push('.');
                dotted.push_str(&f);
                lhs = Rhs::Field {
                    base: Box::new(lhs),
                    name: f,
                };
            }
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
            out.push(Node::Expr(Expr::Var { name: dotted }));
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
            let Some(c) = self.cmp() else {
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

    /// A condition: `[not] rhs (cmp-op rhs)?`. Bare `#if x` desugars to
    /// `x != none` (eval treats it as a truthiness test).
    fn cmp(&mut self) -> Option<Cmp> {
        self.skip_ws();
        let save = self.pos;
        let negated = self.ident().as_deref() == Some("not");
        if !negated {
            self.pos = save;
        }
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
        self.pos += adv;
        let rhs = if adv > 0 {
            self.rhs()?
        } else {
            Rhs::Lit(Value::None)
        };
        Some(Cmp {
            lhs,
            op,
            rhs,
            negated,
        })
    }

    /// Right-hand side: `mul-level` terms joined by `+`/`-`, folded left.
    fn rhs(&mut self) -> Option<Rhs> {
        let mut lhs = self.rhs_mul()?;
        loop {
            let save = self.pos;
            self.skip_ws();
            let op = match self.peek() {
                Some(b'+') => ArithOp::Add,
                Some(b'-') => ArithOp::Sub,
                _ => {
                    self.pos = save;
                    break;
                }
            };
            self.pos += 1;
            self.skip_ws();
            match self.rhs_mul() {
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

    /// `*`/`/` level of an rhs, folded left.
    fn rhs_mul(&mut self) -> Option<Rhs> {
        let mut lhs = self.rhs_term()?;
        loop {
            let save = self.pos;
            self.skip_ws();
            let op = match self.peek() {
                Some(b'*') => ArithOp::Mul,
                Some(b'/') if !matches!(self.at(1), Some(b'/') | Some(b'*')) => ArithOp::Div,
                _ => {
                    self.pos = save;
                    break;
                }
            };
            self.pos += 1;
            self.skip_ws();
            match self.rhs_term() {
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

    /// One operand: literal, `var`, `var.field`, `(...)`, or `[content]`.
    fn rhs_term(&mut self) -> Option<Rhs> {
        self.skip_ws();
        match self.peek()? {
            b'(' => self.paren_value().map(Rhs::Lit),
            b'"' => Some(Rhs::Lit(Value::Str(self.string(self.pos)))),
            b'[' => {
                let bs = self.pos;
                self.pos += 1;
                Some(Rhs::Lit(Value::Content(self.body(true, bs))))
            }
            c if c.is_ascii_digit()
                || (matches!(c, b'-' | b'+')
                    && self.at(1).map_or(false, |d| d.is_ascii_digit()))
                || (c == b'.' && self.at(1).map_or(false, |d| d.is_ascii_digit())) =>
            {
                self.number().map(Rhs::Lit)
            }
            c if is_ident_start(c) => {
                let n = self.ident().unwrap();
                let mut r = match n.as_str() {
                    "true" => Rhs::Lit(Value::Bool(true)),
                    "false" => Rhs::Lit(Value::Bool(false)),
                    "none" | "auto" => Rhs::Lit(Value::None),
                    _ => Rhs::Var(n),
                };
                while self.peek() == Some(b'.') && self.at(1).map_or(false, is_ident_start) {
                    self.pos += 1;
                    let f = self.ident().unwrap();
                    r = Rhs::Field {
                        base: Box::new(r),
                        name: f,
                    };
                }
                Some(r)
            }
            _ => None,
        }
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
        if self.depth > 250 {
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

    /// Remainder of an unrecognized `#foo` construct: trailing string /
    /// `(...)` / `[...]` operands, or a `#show: ...`-style rest of line.
    fn skip_tail(&mut self) {
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
                    self.skip_to_eol();
                    return;
                }
                _ => return,
            }
        }
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
            // `#let f(..) = ..` — function definitions are out of scope.
            self.pos += 1;
            self.skip_balanced(b'(', b')');
            self.skip_hspace();
            if self.peek() == Some(b'=') {
                self.pos += 1;
                self.skip_hspace();
            }
            match self.peek() {
                Some(b'[') => {
                    self.pos += 1;
                    self.skip_balanced(b'[', b']');
                }
                Some(b'(') => {
                    self.pos += 1;
                    self.skip_balanced(b'(', b')');
                }
                Some(b'"') => {
                    self.string(self.pos);
                }
                _ => self.skip_to_eol(),
            }
            self.diag(
                Severity::NotImplemented,
                a,
                "#let function definitions are not implemented",
            );
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
        if self.depth > 250 {
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
        // continuation line and indented marker inside an item
        assert_eq!(
            nodes("- a\n  cont\n  - b\n- c"),
            vec![Node::List {
                ordered: false,
                items: vec![vec![t("a cont - b")], vec![t("c")]],
            }]
        );
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
                    Cmp {
                        lhs: Rhs::Var("x".into()),
                        op: CmpOp::Ne,
                        rhs: Rhs::Lit(Value::None),
                        negated: false,
                    },
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
        assert_eq!(arms[0].0.op, CmpOp::Eq);
        assert_eq!(arms[0].0.rhs, Rhs::Lit(Value::Int(1)));
        assert!(arms[1].0.negated);
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
            assert_eq!(arms[0].0.op, op, "{src}");
        }
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
    fn not_implemented() {
        let (n, d) = parse("$x^2$");
        assert!(n.is_empty());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::NotImplemented);

        for (src, what) in [
            ("#image(\"x.png\")", "#image"),
            ("#figure[..]", "#figure"),
            ("/ term: desc", "term"),
            ("1. item", "numbered"),
            ("<lbl>", "labels"),
            ("@lbl", "references"),
            ("#let f(x) = x * 2", "function definition"),
            ("#show: it => it", "#show"),
        ] {
            let (_, d) = parse(src);
            assert!(!d.is_empty(), "{src}");
            assert_eq!(d[0].severity, Severity::NotImplemented, "{src} ({what})");
        }
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
            "\u{1f600} *é_ \u{4e2d}\u{6587}",
        ] {
            let _ = parse(src);
        }
        // deep nesting stays bounded
        let deep = "#f[".repeat(2000) + &"]".repeat(2000);
        let _ = parse(&deep);
    }
}
