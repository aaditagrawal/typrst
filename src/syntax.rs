//! Markup parser: source text -> Vec<Node> (see ir.rs / SPEC.md).
use crate::ir::{Arg, Diag, Expr, Length, Node, Severity, Span, Value};

/// `#name` calls the parser accepts. Anything else (bound `#let` names
/// aside) is a `not implemented` diagnostic.
const IMPLEMENTED: &[&str] = &[
    "lorem", "pagebreak", "v", "h", "linebreak", "strong", "emph", "text", "align",
];

/// Parse Typst markup. Recovering: returns whatever nodes parsed plus
/// diagnostics; never panics on bad input.
pub fn parse(src: &str) -> (Vec<Node>, Vec<Diag>) {
    let mut p = P { src, pos: 0, diags: Vec::new(), lets: Vec::new(), depth: 0 };
    let nodes = p.body(false, 0);
    (nodes, p.diags)
}

struct P<'a> {
    src: &'a str,
    pos: usize,
    diags: Vec<Diag>,
    lets: Vec<String>,
    depth: u32,
}

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

impl<'a> P<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }
    fn at(&self, k: usize) -> Option<u8> {
        self.src.as_bytes().get(self.pos + k).copied()
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
        self.pos += self.src[self.pos..].chars().next().map_or(1, |c| c.len_utf8());
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
        let s = self.src.as_bytes();
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
        let mut text = String::new();
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
            if line_start && self.block(&mut out, &mut text, in_bracket) {
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
                self.diag(Severity::NotImplemented, a, "term lists are not implemented");
                true
            }
            Some(c) if c.is_ascii_digit() && self.starts_block() => {
                let a = self.pos;
                self.skip_to_eol();
                self.diag(Severity::NotImplemented, a, "numbered lists are not implemented");
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
        out.push(Node::Raw { text: body_txt.into(), block: true });
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
        loop {
            let c = match self.peek() {
                None => return,
                Some(b'\n') => return,
                Some(b']') if in_bracket => return,
                Some(c) if stop == Some(c) && self.at_marker(c) => return,
                Some(c) => c,
            };
            match c {
                b'*' => {
                    if !self.marker(b'*', out, text, in_bracket) {
                        text.push('*');
                        self.pos += 1;
                    }
                }
                b'_' => {
                    let boundary = self.pos == 0
                        || !self.src.as_bytes()[self.pos - 1].is_ascii_alphanumeric();
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
        out.push(if m == b'*' { Node::Strong(nodes) } else { Node::Emph(nodes) });
        true
    }

    /// Look for a closing `*`/`_` before the paragraph ends. Bounded scan.
    fn find_close(&self, m: u8, in_bracket: bool) -> Option<usize> {
        let s = self.src.as_bytes();
        let mut i = self.pos + 1;
        let limit = (self.pos + (1 << 16)).min(s.len());
        while i < limit {
            match s[i] {
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
                    out.push(Node::Raw { text: t, block: false });
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
            self.diag(Severity::NotImplemented, a, "references are not implemented");
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
                    _ => {
                        let implemented = IMPLEMENTED.contains(&name.as_str())
                            || self.lets.iter().any(|l| l == &name);
                        if matches!(self.peek(), Some(b'(') | Some(b'[')) {
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
                                out.push(Node::Expr(Expr::Func { name, args, content }));
                            } else {
                                self.diag(
                                    Severity::NotImplemented,
                                    a,
                                    format!("#{name} is not implemented"),
                                );
                            }
                        } else if self.lets.iter().any(|l| l == &name) {
                            flush(out, text);
                            out.push(Node::Expr(Expr::Var { name }));
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
                self.pos += 1;
                self.skip_balanced(b'(', b')');
                self.diag(Severity::NotImplemented, a, "code expressions are not implemented");
            }
            _ => text.push('#'),
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
        if self.peek() != Some(b'=') {
            self.diag(Severity::Error, a, "expected `=` in `#let`");
            self.skip_to_eol();
            return;
        }
        self.pos += 1;
        match self.value() {
            Some(v) => {
                self.lets.push(name.clone());
                flush(out, text);
                out.push(Node::Expr(Expr::Let { name, value: v }));
            }
            None => {
                self.diag(Severity::Error, a, "expected a literal value in `#let`");
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

    /// Arg list body after `(` is consumed. A parenthesized group value
    /// `(x: L, ...)` becomes `Value::Content` holding `Expr::Func{key,
    /// [value]}` nodes (eval's dict encoding).
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
            if self.peek() == Some(b'(') {
                self.pos += 1;
                self.depth += 1;
                let inner = self.arg_list();
                self.depth -= 1;
                let mut ns = Vec::new();
                for a2 in inner {
                    let node = match a2.name {
                        Some(k) => Node::Expr(Expr::Func {
                            name: k,
                            args: vec![Arg { name: None, value: a2.value }],
                            content: None,
                        }),
                        None => Node::Expr(Expr::Lit(a2.value)),
                    };
                    ns.push(node);
                }
                args.push(Arg { name, value: Value::Content(ns) });
            } else if let Some(v) = self.value() {
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
    /// or a bare identifier (stored as `Value::Str`, e.g. `center`).
    fn value(&mut self) -> Option<Value> {
        self.skip_ws();
        match self.peek()? {
            b'"' => Some(Value::Str(self.string(self.pos))),
            b'[' => {
                let bs = self.pos;
                self.pos += 1;
                Some(Value::Content(self.body(true, bs)))
            }
            c if c.is_ascii_digit()
                || (matches!(c, b'-' | b'+') && self.at(1).map_or(false, |d| d.is_ascii_digit()))
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
        assert_eq!(nodes("= A"), vec![Node::Heading { level: 1, body: vec![t("A")] }]);
        assert_eq!(nodes("====== A"), vec![Node::Heading { level: 6, body: vec![t("A")] }]);
        assert_eq!(nodes("======= A"), vec![t("======= A")]);
        assert_eq!(nodes("=x"), vec![t("=x")]);
    }

    #[test]
    fn strong_emph_nesting() {
        assert_eq!(
            nodes("*a _b_ c*"),
            vec![Node::Strong(vec![t("a "), Node::Emph(vec![t("b")]), t(" c")])]
        );
        assert_eq!(
            nodes("_a *b* c_"),
            vec![Node::Emph(vec![t("a "), Node::Strong(vec![t("b")]), t(" c")])]
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
        assert_eq!(
            nodes("a\\ b"),
            vec![t("a"), Node::LineBreak, t("b")]
        );
        assert_eq!(
            nodes("a\\\nb"),
            vec![t("a"), Node::LineBreak, t("b")]
        );
    }

    #[test]
    fn raw() {
        assert_eq!(nodes("`x`"), vec![Node::Raw { text: "x".into(), block: false }]);
        assert_eq!(
            nodes("```rust\nfn main() {}\n```"),
            vec![Node::Raw { text: "fn main() {}".into(), block: true }]
        );
        let (n, d) = parse("```\nabc");
        assert_eq!(n, vec![Node::Raw { text: "abc".into(), block: true }]);
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
                    Arg { name: Some("paper".into()), value: Value::Str("a4".into()) },
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
                Node::Expr(Expr::Let { name: "x".into(), value: Value::Str("hi".into()) }),
                t(" "),
                Node::Expr(Expr::Var { name: "x".into() }),
            ]
        );

        assert_eq!(
            nodes("#lorem(10)"),
            vec![Node::Expr(Expr::Func {
                name: "lorem".into(),
                args: vec![Arg { name: None, value: Value::Int(10) }],
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
                args: vec![Arg { name: None, value: Value::Str("center".into()) }],
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
        // `margin: (x: 1in, top: 2cm)` -> Content of Func-encoded pairs.
        let (n, d) = parse("#set page(margin: (x: 1in, top: 2cm))");
        assert!(d.is_empty());
        let Node::Expr(Expr::Set { args, .. }) = &n[0] else { panic!() };
        assert_eq!(args[0].name.as_deref(), Some("margin"));
        let Value::Content(ns) = &args[0].value else { panic!() };
        assert_eq!(
            *ns,
            vec![
                Node::Expr(Expr::Func {
                    name: "x".into(),
                    args: vec![Arg { name: None, value: Value::Length(Length::Pt(72.0)) }],
                    content: None,
                }),
                Node::Expr(Expr::Func {
                    name: "top".into(),
                    args: vec![Arg {
                        name: None,
                        value: Value::Length(Length::Pt(2.0 * 28.3465)),
                    }],
                    content: None,
                }),
            ]
        );
    }

    #[test]
    fn not_implemented() {
        let (n, d) = parse("$x^2$");
        assert!(n.is_empty());
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::NotImplemented);

        for (src, what) in [
            ("#table(1)", "#table"),
            ("#image(\"x.png\")", "#image"),
            ("#figure[..]", "#figure"),
            ("/ term: desc", "term"),
            ("1. item", "numbered"),
            ("<lbl>", "labels"),
            ("@lbl", "references"),
        ] {
            let (_, d) = parse(src);
            assert!(!d.is_empty(), "{src}");
            assert_eq!(d[0].severity, Severity::NotImplemented, "{src} ({what})");
        }
    }

    #[test]
    fn no_panics_on_malformed() {
        for src in [
            "", "#", "# ", "##", "#(", "#f(", "#f(x", "#let", "#let 1", "#let x",
            "#let x =", "#set", "#set x", "\"", "`", "```", "*", "_", "$", "<",
            "@", "\\", "/", "//", "/*", "[", "]", "-", "+", "=", "=======",
            "#f[#f[#f[x]", "((((", "*a _b `c", "#12.5.3", "#1xx", "a\\\r\nb",
            "- a\n- \n- c", "#strong[", "\u{1f600} *é_ \u{4e2d}\u{6587}",
        ] {
            let _ = parse(src);
        }
        // deep nesting stays bounded
        let deep = "#f[".repeat(2000) + &"]".repeat(2000);
        let _ = parse(&deep);
    }
}
