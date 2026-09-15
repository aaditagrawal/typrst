//! Evaluator: Vec<Node> -> Document (styles + blocks).
use crate::ir::*;
use std::collections::HashMap;

const LOREM: &[&str] = &[
    "lorem", "ipsum", "dolor", "sit", "amet", "consectetur", "adipiscing", "elit",
    "sed", "do", "eiusmod", "tempor", "incididunt", "ut", "labore", "et",
    "dolore", "magna", "aliqua", "enim", "ad", "minim", "veniam", "quis",
    "nostrud", "exercitation", "ullamco", "laboris", "nisi", "aliquip", "ex",
    "ea", "commodo", "consequat", "duis", "aute", "irure", "in", "reprehenderit",
    "voluptate", "velit", "esse", "cillum", "eu", "fugiat", "nulla", "pariatur",
    "excepteur", "sint", "occaecat", "cupidatat", "non", "proident", "sunt",
    "culpa", "qui", "officia", "deserunt", "mollit", "anim", "id", "est",
    "laborum",
];

struct Ev<'a> {
    st: Styles,
    base: FontId,
    scope: HashMap<String, Value>,
    diags: &'a mut Vec<Diag>,
    blocks: Vec<Block>,
}

pub fn eval(nodes: Vec<Node>, diags: &mut Vec<Diag>) -> Document {
    let mut ev = Ev {
        st: Styles::default(),
        base: FontId::Regular,
        scope: HashMap::new(),
        diags,
        blocks: Vec::new(),
    };
    let mut pending = Vec::new();
    for n in &nodes {
        ev.node(n, &mut pending);
    }
    ev.flush(&mut pending);
    Document { styles: ev.st, blocks: ev.blocks, diags: Vec::new() }
}

impl Ev<'_> {
    fn cur(&self) -> TextStyle {
        TextStyle { font: self.base, size: self.st.font_size }
    }

    fn diag(&mut self, sev: Severity, msg: impl Into<String>) {
        self.diags.push(Diag::new(sev, None, msg));
    }

    fn flush(&mut self, out: &mut Vec<Inline>) {
        if !out.is_empty() {
            self.blocks.push(Block::Paragraph {
                content: std::mem::take(out),
                align: HAlign::Left,
            });
        }
    }

    fn node(&mut self, n: &Node, out: &mut Vec<Inline>) {
        match n {
            Node::Text(s) => out.push(Inline::Text { text: s.clone(), style: self.cur() }),
            Node::LineBreak => out.push(Inline::LineBreak),
            Node::Strong(ch) => inlines(ch, with_font(self.cur(), bolden(self.cur().font)), out, self),
            Node::Emph(ch) => inlines(ch, with_font(self.cur(), ital(self.cur().font)), out, self),
            Node::Raw { text, block: false } => out.push(Inline::Text {
                text: text.clone(),
                style: TextStyle { font: FontId::Mono, size: self.st.font_size },
            }),
            Node::Parbreak => self.flush(out),
            Node::Heading { level, body } => {
                self.flush(out);
                let mut c = Vec::new();
                inlines(body, self.cur(), &mut c, self);
                self.blocks.push(Block::Heading { level: *level, content: c });
            }
            Node::List { ordered, items } => {
                self.flush(out);
                let items = items
                    .iter()
                    .map(|it| {
                        let mut c = Vec::new();
                        inlines(it, self.cur(), &mut c, self);
                        c
                    })
                    .collect();
                self.blocks.push(Block::List { ordered: *ordered, items });
            }
            Node::Raw { text, block: true } => {
                self.flush(out);
                self.blocks.push(Block::RawBlock { text: text.clone() });
            }
            Node::Expr(e) => self.expr(e, out),
        }
    }

    fn expr(&mut self, e: &Expr, out: &mut Vec<Inline>) {
        match e {
            Expr::Set { target, args } => self.do_set(target, args),
            Expr::Let { name, value } => {
                self.scope.insert(name.clone(), value.clone());
            }
            Expr::Var { name } => match self.scope.get(name).cloned() {
                Some(v) => value_inline(&v, self.cur(), out, self),
                None => self.diag(Severity::Error, format!("undefined variable '#{name}'")),
            },
            Expr::Lit(v) => value_inline(v, self.cur(), out, self),
            Expr::Func { name, args, content } => {
                self.func(name, args, content.as_deref(), out)
            }
        }
    }

    fn func(&mut self, name: &str, args: &[Arg], content: Option<&[Node]>, out: &mut Vec<Inline>) {
        match name {
            "lorem" => {
                self.flush(out);
                let n = args
                    .iter()
                    .find_map(|a| match &a.value {
                        Value::Int(i) if a.name.is_none() => Some(*i),
                        Value::Float(f) if a.name.is_none() => Some(*f as i64),
                        _ => None,
                    })
                    .unwrap_or(0);
                if n <= 0 {
                    self.diag(Severity::Error, "#lorem expects a positive word count");
                    return;
                }
                let mut s = String::new();
                for i in 0..n as usize {
                    if i > 0 {
                        s.push(' ');
                    }
                    s.push_str(LOREM[i % LOREM.len()]);
                }
                s[..1].make_ascii_uppercase();
                s.push('.');
                self.blocks.push(Block::Paragraph {
                    content: vec![Inline::Text { text: s, style: self.cur() }],
                    align: HAlign::Left,
                });
            }
            "pagebreak" => {
                self.flush(out);
                self.blocks.push(Block::PageBreak);
            }
            "v" => {
                self.flush(out);
                match arg_len(args, self.st.font_size) {
                    Some(p) => self.blocks.push(Block::VSpace(p)),
                    None => self.diag(Severity::Error, "#v expects a length"),
                }
            }
            "h" => match arg_len(args, self.cur().size) {
                Some(p) => out.push(Inline::HSpace(p)),
                None => self.diag(Severity::Error, "#h expects a length"),
            },
            "linebreak" => out.push(Inline::LineBreak),
            "strong" => {
                if let Some(c) = content {
                    inlines(c, with_font(self.cur(), bolden(self.cur().font)), out, self);
                }
            }
            "emph" => {
                if let Some(c) = content {
                    inlines(c, with_font(self.cur(), ital(self.cur().font)), out, self);
                }
            }
            "text" => {
                let st = text_style(args, self.cur(), self);
                if let Some(c) = content {
                    inlines(c, st, out, self);
                }
            }
            "align" => {
                let kw = args.iter().find_map(|a| {
                    if let Some(k) = a.name.as_deref() {
                        return Some(k.to_string());
                    }
                    match &a.value {
                        Value::Str(s) => Some(s.clone()),
                        Value::Content(ns) => ns.iter().find_map(|n| {
                            if let Node::Text(t) = n { Some(t.trim().to_string()) } else { None }
                        }),
                        _ => None,
                    }
                });
                let al = match kw.as_deref().map(|s| s.trim().to_ascii_lowercase()) {
                    Some(ref k) if k == "center" || k == "centre" => HAlign::Center,
                    Some(ref k) if k == "right" => HAlign::Right,
                    Some(ref k) if k == "left" => HAlign::Left,
                    None => HAlign::Left,
                    Some(k) => {
                        self.diag(Severity::Warning, format!("unknown alignment '{k}'"));
                        HAlign::Left
                    }
                };
                self.flush(out);
                if let Some(c) = content {
                    let mut v = Vec::new();
                    inlines(c, self.cur(), &mut v, self);
                    self.blocks.push(Block::Paragraph { content: v, align: al });
                }
            }
            _ => self.diag(
                Severity::NotImplemented,
                format!("function '#{name}' is not implemented"),
            ),
        }
    }

    fn do_set(&mut self, target: &str, args: &[Arg]) {
        match target {
            "page" => self.set_page(args),
            "text" => self.set_text(args),
            "par" => self.set_par(args),
            _ => self.diag(
                Severity::NotImplemented,
                format!("set {target} is not implemented"),
            ),
        }
    }

    fn set_page(&mut self, args: &[Arg]) {
        let mut flipped = false;
        for a in args {
            match a.name.as_deref() {
                Some("paper") | None => match &a.value {
                    Value::Str(s) => match s.to_ascii_lowercase().as_str() {
                        "a4" => {
                            self.st.page_width = 595.28;
                            self.st.page_height = 841.89;
                        }
                        "a5" => {
                            self.st.page_width = 419.53;
                            self.st.page_height = 595.28;
                        }
                        "us-letter" | "letter" => {
                            self.st.page_width = 612.0;
                            self.st.page_height = 792.0;
                        }
                        _ => self.diag(Severity::Warning, format!("unknown paper '{s}'")),
                    },
                    _ => self.diag(Severity::Warning, "page paper expects a string"),
                },
                Some("flipped") => match &a.value {
                    Value::Bool(b) => flipped = *b,
                    _ => self.diag(Severity::Warning, "flipped expects a boolean"),
                },
                Some("margin") => self.set_margin(&a.value),
                Some(other) => self.diag(
                    Severity::Warning,
                    format!("page argument '{other}' ignored"),
                ),
            }
        }
        if flipped {
            std::mem::swap(&mut self.st.page_width, &mut self.st.page_height);
        }
    }

    fn set_margin(&mut self, v: &Value) {
        let em = self.st.font_size;
        match v {
            Value::Length(_) | Value::Int(_) | Value::Float(_) => {
                if let Some(p) = val_pt(v, em) {
                    self.st.margin_top = p;
                    self.st.margin_bottom = p;
                    self.st.margin_left = p;
                    self.st.margin_right = p;
                }
            }
            Value::Content(ns) => {
                let mut any = false;
                for (k, vv) in named_pairs(ns) {
                    let Some(p) = val_pt(&vv, em) else { continue };
                    any = true;
                    match k.as_str() {
                        "x" => {
                            self.st.margin_left = p;
                            self.st.margin_right = p;
                        }
                        "y" => {
                            self.st.margin_top = p;
                            self.st.margin_bottom = p;
                        }
                        "top" => self.st.margin_top = p,
                        "right" => self.st.margin_right = p,
                        "bottom" => self.st.margin_bottom = p,
                        "left" => self.st.margin_left = p,
                        "rest" => {
                            self.st.margin_top = p;
                            self.st.margin_bottom = p;
                            self.st.margin_left = p;
                            self.st.margin_right = p;
                        }
                        _ => self
                            .diag(Severity::Warning, format!("unknown margin key '{k}'")),
                    }
                }
                if !any {
                    self.diag(Severity::Warning, "could not parse page margin");
                }
            }
            _ => self.diag(Severity::Warning, "unsupported margin value"),
        }
    }

    fn set_text(&mut self, args: &[Arg]) {
        for a in args {
            match a.name.as_deref() {
                Some("size") | None => match val_pt(&a.value, self.st.font_size) {
                    Some(p) => {
                        self.st.font_size = p;
                        self.st.line_height = 1.3 * p;
                    }
                    None => self.diag(Severity::Warning, "text size expects a length"),
                },
                Some("font") => match &a.value {
                    Value::Str(s) => {
                        self.diag(
                            Severity::Warning,
                            format!("font '{s}' ignored, base-14 family in use"),
                        );
                        let l = s.to_ascii_lowercase();
                        if l.contains("mono") || l.contains("courier") {
                            self.base = FontId::Mono;
                        }
                    }
                    _ => self.diag(Severity::Warning, "text font expects a string"),
                },
                Some(other) => self.diag(
                    Severity::Warning,
                    format!("text argument '{other}' ignored"),
                ),
            }
        }
    }

    fn set_par(&mut self, args: &[Arg]) {
        for a in args {
            match a.name.as_deref() {
                Some("justify") => match &a.value {
                    Value::Bool(b) => self.st.justify = *b,
                    _ => self.diag(Severity::Warning, "justify expects a boolean"),
                },
                Some("leading") => match val_pt(&a.value, self.st.font_size) {
                    Some(p) => {
                        let s = self.st.font_size;
                        self.st.line_height = if p < s { s + p } else { p };
                    }
                    None => self.diag(Severity::Warning, "leading expects a length"),
                },
                Some("spacing") => match val_pt(&a.value, self.st.font_size) {
                    Some(p) => self.st.par_spacing = p,
                    None => self.diag(Severity::Warning, "spacing expects a length"),
                },
                Some(other) => self.diag(
                    Severity::Warning,
                    format!("par argument '{other}' ignored"),
                ),
                None => self.diag(Severity::Warning, "positional par argument ignored"),
            }
        }
    }
}

// ------------------------- free helpers -------------------------

fn bolden(f: FontId) -> FontId {
    match f {
        FontId::Regular | FontId::Bold => FontId::Bold,
        FontId::Italic | FontId::BoldItalic => FontId::BoldItalic,
        FontId::Mono | FontId::MonoBold => FontId::MonoBold,
    }
}

fn ital(f: FontId) -> FontId {
    match f {
        FontId::Regular | FontId::Italic => FontId::Italic,
        FontId::Bold | FontId::BoldItalic => FontId::BoldItalic,
        FontId::Mono | FontId::MonoBold => FontId::Mono,
    }
}

fn with_font(st: TextStyle, f: FontId) -> TextStyle {
    TextStyle { font: f, ..st }
}

fn val_pt(v: &Value, em: f64) -> Option<f64> {
    match v {
        Value::Length(l) => Some(l.pt(em)),
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// First positional length-ish argument.
fn arg_len(args: &[Arg], em: f64) -> Option<f64> {
    args.iter()
        .filter(|a| a.name.is_none())
        .find_map(|a| val_pt(&a.value, em))
}

/// Style modifications for `#text(..)[..]`.
fn text_style(args: &[Arg], mut st: TextStyle, cx: &mut Ev) -> TextStyle {
    for a in args {
        match a.name.as_deref() {
            Some("size") | None => {
                if let Some(p) = val_pt(&a.value, st.size) {
                    st.size = p;
                }
            }
            Some("weight") => {
                let b = match &a.value {
                    Value::Str(s) => s.eq_ignore_ascii_case("bold"),
                    Value::Int(i) => *i >= 600,
                    Value::Float(f) => *f >= 600.0,
                    _ => false,
                };
                if b {
                    st.font = bolden(st.font);
                }
            }
            Some(other) => cx.diag(
                Severity::Warning,
                format!("text argument '{other}' ignored"),
            ),
        }
    }
    st
}

/// Convert a run of nodes to inline content under a base style.
fn inlines(nodes: &[Node], st: TextStyle, out: &mut Vec<Inline>, cx: &mut Ev) {
    for n in nodes {
        match n {
            Node::Text(s) => out.push(Inline::Text { text: s.clone(), style: st }),
            Node::LineBreak => out.push(Inline::LineBreak),
            Node::Strong(ch) => inlines(ch, with_font(st, bolden(st.font)), out, cx),
            Node::Emph(ch) => inlines(ch, with_font(st, ital(st.font)), out, cx),
            Node::Raw { text, block } => {
                if *block {
                    cx.diag(
                        Severity::NotImplemented,
                        "raw block inside inline content",
                    );
                }
                out.push(Inline::Text {
                    text: text.clone(),
                    style: TextStyle { font: FontId::Mono, size: st.size },
                });
            }
            Node::Parbreak => out.push(Inline::LineBreak),
            Node::Heading { .. } | Node::List { .. } => cx.diag(
                Severity::NotImplemented,
                "block-level construct inside inline content",
            ),
            Node::Expr(e) => inline_expr(e, st, out, cx),
        }
    }
}

/// Expressions legal in inline (nested) context.
fn inline_expr(e: &Expr, st: TextStyle, out: &mut Vec<Inline>, cx: &mut Ev) {
    match e {
        Expr::Set { target, args } => cx.do_set(target, args),
        Expr::Let { name, value } => {
            cx.scope.insert(name.clone(), value.clone());
        }
        Expr::Var { name } => match cx.scope.get(name).cloned() {
            Some(v) => value_inline(&v, st, out, cx),
            None => cx.diag(Severity::Error, format!("undefined variable '#{name}'")),
        },
        Expr::Lit(v) => value_inline(v, st, out, cx),
        Expr::Func { name, args, content } => match name.as_str() {
            "h" => match arg_len(args, st.size) {
                Some(p) => out.push(Inline::HSpace(p)),
                None => cx.diag(Severity::Error, "#h expects a length"),
            },
            "linebreak" => out.push(Inline::LineBreak),
            "strong" => {
                if let Some(c) = content {
                    inlines(c, with_font(st, bolden(st.font)), out, cx);
                }
            }
            "emph" => {
                if let Some(c) = content {
                    inlines(c, with_font(st, ital(st.font)), out, cx);
                }
            }
            "text" => {
                let st2 = text_style(args, st, cx);
                if let Some(c) = content {
                    inlines(c, st2, out, cx);
                }
            }
            "lorem" | "pagebreak" | "v" | "align" => cx.diag(
                Severity::NotImplemented,
                format!("'#{name}' is block-level; ignored inside inline content"),
            ),
            _ => cx.diag(
                Severity::NotImplemented,
                format!("function '#{name}' is not implemented"),
            ),
        },
    }
}

/// Render a Value as inline content (var substitution / literal).
fn value_inline(v: &Value, st: TextStyle, out: &mut Vec<Inline>, cx: &mut Ev) {
    match v {
        Value::Str(s) => out.push(Inline::Text { text: s.clone(), style: st }),
        Value::Content(ns) => inlines(ns, st, out, cx),
        Value::Int(i) => out.push(Inline::Text { text: format!("{i}"), style: st }),
        Value::Float(f) => out.push(Inline::Text { text: format!("{f}"), style: st }),
        Value::Length(Length::Pt(p)) => {
            out.push(Inline::Text { text: format!("{p}pt"), style: st })
        }
        Value::Length(Length::Em(e)) => {
            out.push(Inline::Text { text: format!("{e}em"), style: st })
        }
        Value::Bool(_) | Value::None => {}
    }
}

/// Extract `name: value` pairs from a dict encoded as Content nodes.
/// Accepts the plausible encodings: Let, Func, Set, Var/Text + ":" + Lit.
fn named_pairs(nodes: &[Node]) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < nodes.len() {
        match &nodes[i] {
            Node::Expr(Expr::Let { name, value }) => out.push((name.clone(), value.clone())),
            Node::Expr(Expr::Func { name, args, .. }) => {
                if let Some(a) = args.first() {
                    out.push((name.clone(), a.value.clone()));
                }
            }
            Node::Expr(Expr::Set { target, args }) => {
                if let Some(a) = args.first() {
                    out.push((target.clone(), a.value.clone()));
                }
            }
            Node::Expr(Expr::Var { name }) => {
                if let Some((v, adv)) = pair_after(nodes, i) {
                    out.push((name.clone(), v));
                    i += adv;
                }
            }
            Node::Text(t) => {
                let s = t.trim_end();
                if let Some(name) = s.strip_suffix(':') {
                    let name = name.trim();
                    if !name.is_empty() {
                        if let Some(Node::Expr(Expr::Lit(v))) = nodes.get(i + 1) {
                            out.push((name.to_string(), v.clone()));
                            i += 1;
                        }
                    }
                } else if !s.is_empty() {
                    if let Some((v, adv)) = pair_after(nodes, i) {
                        out.push((s.to_string(), v));
                        i += adv;
                    }
                }
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// `name <Text ":"> <Expr::Lit>` — returns the literal and consumed count.
fn pair_after(nodes: &[Node], name_idx: usize) -> Option<(Value, usize)> {
    if let Some(Node::Text(t)) = nodes.get(name_idx + 1) {
        if t.trim() == ":" {
            if let Some(Node::Expr(Expr::Lit(v))) = nodes.get(name_idx + 2) {
                return Some((v.clone(), 2));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(nodes: Vec<Node>) -> (Document, Vec<Diag>) {
        let mut d = Vec::new();
        let doc = eval(nodes, &mut d);
        (doc, d)
    }

    fn named(name: &str, value: Value) -> Arg {
        Arg { name: Some(name.to_string()), value }
    }

    fn pos(value: Value) -> Arg {
        Arg { name: None, value }
    }

    #[test]
    fn eval_paragraphs_and_styles() {
        let (doc, d) = run(vec![
            Node::Text("Hello ".into()),
            Node::Strong(vec![Node::Text("bold".into())]),
            Node::Emph(vec![Node::Strong(vec![Node::Text("both".into())])]),
            Node::Parbreak,
            Node::Text("second".into()),
        ]);
        assert!(d.is_empty());
        assert_eq!(doc.blocks.len(), 2);
        let Block::Paragraph { content, align } = &doc.blocks[0] else { panic!() };
        assert_eq!(*align, HAlign::Left);
        assert_eq!(content.len(), 3);
        let Inline::Text { text, style } = &content[1] else { panic!() };
        assert_eq!(text, "bold");
        assert_eq!(style.font, FontId::Bold);
        assert_eq!(style.size, 11.0);
        let Inline::Text { style, .. } = &content[2] else { panic!() };
        assert_eq!(style.font, FontId::BoldItalic);
    }

    #[test]
    fn eval_block_nodes() {
        let (doc, _) = run(vec![
            Node::Text("intro".into()),
            Node::Heading { level: 2, body: vec![Node::Text("T".into())] },
            Node::List {
                ordered: true,
                items: vec![vec![Node::Text("a".into())], vec![Node::Text("b".into())]],
            },
            Node::Raw { text: "x\ny".into(), block: true },
            Node::Raw { text: "code".into(), block: false },
        ]);
        assert_eq!(doc.blocks.len(), 5);
        assert!(matches!(doc.blocks[0], Block::Paragraph { .. }));
        assert!(matches!(doc.blocks[1], Block::Heading { level: 2, .. }));
        assert!(matches!(doc.blocks[2], Block::List { ordered: true, .. }));
        assert!(matches!(doc.blocks[3], Block::RawBlock { .. }));
        // trailing inline raw becomes its own paragraph in Mono.
        let Block::Paragraph { content, .. } = &doc.blocks[4] else { panic!() };
        let Inline::Text { style, .. } = &content[0] else { panic!() };
        assert_eq!(style.font, FontId::Mono);
    }

    #[test]
    fn eval_set_page_text_par() {
        let (doc, d) = run(vec![
            Node::Expr(Expr::Set {
                target: "page".into(),
                args: vec![
                    named("paper", Value::Str("us-letter".into())),
                    named("flipped", Value::Bool(true)),
                    named("margin", Value::Length(Length::Pt(36.0))),
                ],
            }),
            Node::Expr(Expr::Set {
                target: "text".into(),
                args: vec![named("size", Value::Length(Length::Pt(14.0)))],
            }),
            Node::Expr(Expr::Set {
                target: "par".into(),
                args: vec![
                    named("justify", Value::Bool(true)),
                    named("leading", Value::Length(Length::Pt(4.0))),
                    named("spacing", Value::Length(Length::Pt(12.0))),
                ],
            }),
        ]);
        assert!(d.is_empty());
        assert_eq!(doc.styles.page_width, 792.0);
        assert_eq!(doc.styles.page_height, 612.0);
        assert_eq!(doc.styles.margin_left, 36.0);
        assert_eq!(doc.styles.margin_top, 36.0);
        assert_eq!(doc.styles.font_size, 14.0);
        assert!(doc.styles.justify);
        // leading 4 < size 14 -> line_height = 14 + 4
        assert_eq!(doc.styles.line_height, 18.0);
        assert_eq!(doc.styles.par_spacing, 12.0);
    }

    #[test]
    fn eval_set_margin_dict() {
        // `margin: (x: 10pt, top: 20pt)` encoded as Content of Func exprs.
        let (doc, _) = run(vec![Node::Expr(Expr::Set {
            target: "page".into(),
            args: vec![named(
                "margin",
                Value::Content(vec![
                    Node::Expr(Expr::Func {
                        name: "x".into(),
                        args: vec![pos(Value::Length(Length::Pt(10.0)))],
                        content: None,
                    }),
                    Node::Expr(Expr::Func {
                        name: "top".into(),
                        args: vec![pos(Value::Length(Length::Pt(20.0)))],
                        content: None,
                    }),
                ]),
            )],
        })]);
        assert_eq!(doc.styles.margin_left, 10.0);
        assert_eq!(doc.styles.margin_right, 10.0);
        assert_eq!(doc.styles.margin_top, 20.0);
        assert_eq!(doc.styles.margin_bottom, 70.866); // unchanged
    }

    #[test]
    fn eval_let_and_var() {
        let (doc, d) = run(vec![
            Node::Expr(Expr::Let { name: "x".into(), value: Value::Str("hi".into()) }),
            Node::Expr(Expr::Let { name: "n".into(), value: Value::Length(Length::Pt(12.0)) }),
            Node::Text("say ".into()),
            Node::Expr(Expr::Var { name: "x".into() }),
            Node::Text(" at ".into()),
            Node::Expr(Expr::Var { name: "n".into() }),
            Node::Expr(Expr::Var { name: "missing".into() }),
        ]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Error);
        let Block::Paragraph { content, .. } = &doc.blocks[0] else { panic!() };
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|i| if let Inline::Text { text, .. } = i { Some(text.as_str()) } else { None })
            .collect();
        assert_eq!(texts, ["say ", "hi", " at ", "12pt"]);
    }

    #[test]
    fn eval_functions() {
        let (doc, d) = run(vec![
            Node::Expr(Expr::Func {
                name: "lorem".into(),
                args: vec![pos(Value::Int(5))],
                content: None,
            }),
            Node::Expr(Expr::Func {
                name: "v".into(),
                args: vec![pos(Value::Length(Length::Pt(24.0)))],
                content: None,
            }),
            Node::Expr(Expr::Func { name: "pagebreak".into(), args: vec![], content: None }),
            Node::Expr(Expr::Func {
                name: "strong".into(),
                args: vec![],
                content: Some(vec![Node::Text("B".into())]),
            }),
            Node::Expr(Expr::Func {
                name: "h".into(),
                args: vec![pos(Value::Length(Length::Pt(8.0)))],
                content: None,
            }),
            Node::Expr(Expr::Func {
                name: "frobnicate".into(),
                args: vec![],
                content: Some(vec![Node::Text("dropped".into())]),
            }),
        ]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::NotImplemented);
        assert!(d[0].message.contains("frobnicate"));
        let Block::Paragraph { content, .. } = &doc.blocks[0] else { panic!() };
        let Inline::Text { text, .. } = &content[0] else { panic!() };
        assert_eq!(text, "Lorem ipsum dolor sit amet.");
        assert!(matches!(doc.blocks[1], Block::VSpace(v) if v == 24.0));
        assert!(matches!(doc.blocks[2], Block::PageBreak));
        let Block::Paragraph { content, .. } = &doc.blocks[3] else { panic!() };
        let Inline::Text { style, .. } = &content[0] else { panic!() };
        assert_eq!(style.font, FontId::Bold);
        assert!(matches!(content[1], Inline::HSpace(w) if w == 8.0));
    }

    #[test]
    fn eval_text_and_align() {
        let (doc, _) = run(vec![
            Node::Expr(Expr::Func {
                name: "text".into(),
                args: vec![
                    named("size", Value::Length(Length::Pt(20.0))),
                    named("weight", Value::Str("bold".into())),
                ],
                content: Some(vec![Node::Text("big".into())]),
            }),
            Node::Parbreak,
            Node::Expr(Expr::Func {
                name: "align".into(),
                args: vec![pos(Value::Str("center".into()))],
                content: Some(vec![Node::Text("mid".into())]),
            }),
        ]);
        let Block::Paragraph { content, .. } = &doc.blocks[0] else { panic!() };
        let Inline::Text { style, .. } = &content[0] else { panic!() };
        assert_eq!(style.size, 20.0);
        assert_eq!(style.font, FontId::Bold);
        let Block::Paragraph { align, .. } = &doc.blocks[1] else { panic!() };
        assert_eq!(*align, HAlign::Center);
    }

    #[test]
    fn eval_unknown_set_and_font() {
        let (_doc, d) = run(vec![
            Node::Expr(Expr::Set { target: "grid".into(), args: vec![] }),
            Node::Expr(Expr::Set {
                target: "text".into(),
                args: vec![named("font", Value::Str("Comic Sans".into()))],
            }),
        ]);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::NotImplemented);
        assert_eq!(d[1].severity, Severity::Warning);
    }
}
