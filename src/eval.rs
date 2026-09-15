//! Evaluator: Vec<Node> -> Document (styles + blocks).
use crate::ir::*;
use crate::package;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

const LOREM: &[&str] = &[
    "lorem",
    "ipsum",
    "dolor",
    "sit",
    "amet",
    "consectetur",
    "adipiscing",
    "elit",
    "sed",
    "do",
    "eiusmod",
    "tempor",
    "incididunt",
    "ut",
    "labore",
    "et",
    "dolore",
    "magna",
    "aliqua",
    "enim",
    "ad",
    "minim",
    "veniam",
    "quis",
    "nostrud",
    "exercitation",
    "ullamco",
    "laboris",
    "nisi",
    "aliquip",
    "ex",
    "ea",
    "commodo",
    "consequat",
    "duis",
    "aute",
    "irure",
    "in",
    "reprehenderit",
    "voluptate",
    "velit",
    "esse",
    "cillum",
    "eu",
    "fugiat",
    "nulla",
    "pariatur",
    "excepteur",
    "sint",
    "occaecat",
    "cupidatat",
    "non",
    "proident",
    "sunt",
    "culpa",
    "qui",
    "officia",
    "deserunt",
    "mollit",
    "anim",
    "id",
    "est",
    "laborum",
];

/// External context handed to `eval`: where files resolve from, how
/// fonts resolve, and guards against runaway imports.
pub struct EvalEnv<'a> {
    /// Directory of the file being compiled; `#import`/`#include` path
    /// specs resolve relative to it.
    pub base_dir: &'a Path,
    /// Font resolution. Discovered faces are recorded by the registry
    /// implementation itself (e.g. pushed to a `Vec<FontUse>` it owns a
    /// borrow of).
    pub registry: &'a mut dyn FaceRegistry,
    /// Import/include recursion depth; capped at 64.
    pub depth: usize,
    /// Files currently being evaluated (cycle detection).
    pub loaded: &'a mut HashSet<PathBuf>,
}

/// Evaluate parsed nodes into a Document. `diags` receives all
/// diagnostics; `Document::diags` stays empty and `Document::fonts` is
/// filled by the caller from `env.fonts`.
pub fn eval(nodes: Vec<Node>, diags: &mut Vec<Diag>, env: &mut EvalEnv) -> Document {
    let mut ev = Ev::new(diags, env);
    let mut pending = Vec::new();
    for n in &nodes {
        ev.node(n, &mut pending);
    }
    ev.flush(&mut pending);
    Document {
        styles: ev.st,
        blocks: ev.blocks,
        diags: Vec::new(),
        fonts: Vec::new(),
    }
}

/// Convenience wrapper: evaluate without a font database (all family
/// lookups fall back to base-14) rooted at `base_dir`.
#[allow(dead_code)] // integrator may switch main.rs to this or to `eval` + EvalEnv
pub fn eval_plain(nodes: Vec<Node>, diags: &mut Vec<Diag>, base_dir: &Path) -> Document {
    struct NoFaces;
    impl FaceRegistry for NoFaces {
        fn resolve(&mut self, _: &str, _: bool, _: bool, _: bool) -> Option<FontId> {
            None
        }
    }
    let mut reg = NoFaces;
    let mut loaded = HashSet::new();
    let mut env = EvalEnv {
        base_dir,
        registry: &mut reg,
        depth: 0,
        loaded: &mut loaded,
    };
    eval(nodes, diags, &mut env)
}

struct Ev<'e, 'a> {
    st: Styles,
    scope: HashMap<String, Value>,
    diags: &'e mut Vec<Diag>,
    blocks: Vec<Block>,
    env: &'e mut EvalEnv<'a>,
    /// `FontId::Ext(i)` -> (family, bold, italic, mono) of resolved faces
    /// (linear map; FontId has no Hash impl).
    face_info: Vec<(FontId, (String, bool, bool, bool))>,
    /// (family, bold, italic, mono) -> resolved face (None = fallback).
    face_cache: HashMap<(String, bool, bool, bool), Option<FontId>>,
    /// Families already warned about (diag once each).
    font_warned: HashSet<String>,
}

impl<'e, 'a> Ev<'e, 'a> {
    fn new(diags: &'e mut Vec<Diag>, env: &'e mut EvalEnv<'a>) -> Self {
        Self {
            st: Styles::default(),
            scope: HashMap::new(),
            diags,
            blocks: Vec::new(),
            env,
            face_info: Vec::new(),
            face_cache: HashMap::new(),
            font_warned: HashSet::new(),
        }
    }

    /// Current text style: the family's regular face (or base-14 serif).
    fn cur(&mut self) -> TextStyle {
        let font = match self.st.font_family.clone() {
            Some(fam) => self.resolve_face(&fam, false, false, false),
            None => FontId::Regular,
        };
        TextStyle {
            font,
            size: self.st.font_size,
        }
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

    // --------------------------- fonts ---------------------------

    /// (bold, italic, mono) flags of a FontId — derived for base-14,
    /// remembered for registry faces.
    fn flags_of(&self, f: FontId) -> (bool, bool, bool) {
        match f {
            FontId::Regular => (false, false, false),
            FontId::Bold => (true, false, false),
            FontId::Italic => (false, true, false),
            FontId::BoldItalic => (true, true, false),
            FontId::Mono => (false, false, true),
            FontId::MonoBold => (true, false, true),
            FontId::Ext(_) => self
                .face_info
                .iter()
                .find(|(id, _)| *id == f)
                .map(|(_, t)| (t.1, t.2, t.3))
                .unwrap_or_default(),
        }
    }

    /// Resolve `family` at a style combination through the registry,
    /// caching per combo and warning once per family on failure.
    fn resolve_face(&mut self, family: &str, bold: bool, italic: bool, mono: bool) -> FontId {
        let key = (family.to_string(), bold, italic, mono);
        if let Some(&r) = self.face_cache.get(&key) {
            return r.unwrap_or_else(|| base14(bold, italic, mono));
        }
        let r = self.env.registry.resolve(family, bold, italic, mono);
        if let Some(id) = r {
            self.face_info
                .push((id, (key.0.clone(), bold, italic, mono)));
        } else if self.font_warned.insert(key.0.clone()) {
            self.diag(
                Severity::Warning,
                format!("font '{family}' not found; using base-14 fallback"),
            );
        }
        self.face_cache.insert(key, r);
        r.unwrap_or_else(|| base14(bold, italic, mono))
    }

    /// `f` with some style flags changed (and optionally a different
    /// family), resolved to a concrete face.
    fn vary(
        &mut self,
        f: FontId,
        bold: Option<bool>,
        italic: Option<bool>,
        family: Option<&str>,
    ) -> FontId {
        let (mut b, mut i, m) = self.flags_of(f);
        if let Some(x) = bold {
            b = x;
        }
        if let Some(x) = italic {
            i = x;
        }
        let fam = family
            .map(str::to_string)
            .or_else(|| {
                self.face_info
                    .iter()
                    .find(|(id, _)| *id == f)
                    .map(|(_, t)| t.0.clone())
            })
            .or_else(|| self.st.font_family.clone());
        match fam {
            Some(fam) => self.resolve_face(&fam, b, i, m),
            None => base14(b, i, m),
        }
    }

    // --------------------------- nodes ---------------------------

    fn node(&mut self, n: &Node, out: &mut Vec<Inline>) {
        match n {
            Node::Text(s) => {
                let st = self.cur();
                out.push(Inline::Text {
                    text: s.clone(),
                    style: st,
                });
            }
            Node::LineBreak => out.push(Inline::LineBreak),
            Node::Strong(ch) => {
                let st = self.cur();
                let f = self.vary(st.font, Some(true), None, None);
                inlines(ch, with_font(st, f), out, self);
            }
            Node::Emph(ch) => {
                let st = self.cur();
                let f = self.vary(st.font, None, Some(true), None);
                inlines(ch, with_font(st, f), out, self);
            }
            Node::Raw { text, block: false } => out.push(Inline::Text {
                text: text.clone(),
                style: TextStyle {
                    font: FontId::Mono,
                    size: self.st.font_size,
                },
            }),
            Node::Parbreak => self.flush(out),
            Node::Heading { level, body } => {
                self.flush(out);
                let mut c = Vec::new();
                let st = self.cur();
                inlines(body, st, &mut c, self);
                self.blocks.push(Block::Heading {
                    level: *level,
                    content: c,
                });
            }
            Node::List { ordered, items } => {
                self.flush(out);
                let st = self.cur();
                let items = items
                    .iter()
                    .map(|it| {
                        let mut c = Vec::new();
                        inlines(it, st, &mut c, self);
                        c
                    })
                    .collect();
                self.blocks.push(Block::List {
                    ordered: *ordered,
                    items,
                });
            }
            Node::Raw { text, block: true } => {
                self.flush(out);
                self.blocks.push(Block::RawBlock { text: text.clone() });
            }
            Node::Expr(e) => self.expr(e, out),
        }
    }

    /// Evaluate a run of nodes either block-level (`st: None`, via
    /// `self.node`) or inline (`st: Some`, via `inlines`).
    fn run_body(&mut self, body: &[Node], st: Option<TextStyle>, out: &mut Vec<Inline>) {
        match st {
            Some(st) => inlines(body, st, out, self),
            None => {
                for n in body {
                    self.node(n, out);
                }
            }
        }
    }

    fn expr(&mut self, e: &Expr, out: &mut Vec<Inline>) {
        match e {
            Expr::Set { target, args } => self.do_set(target, args),
            Expr::Let { name, value } => {
                if let Some(v) = self.rhs_val(value) {
                    self.scope.insert(name.clone(), v);
                }
            }
            Expr::Var { name } => match self.resolve_var(name) {
                Some(v) => {
                    let st = self.cur();
                    value_inline(&v, st, out, self);
                }
                None => self.diag(Severity::Warning, format!("undefined variable '#{name}'")),
            },
            Expr::Import { path, items, all } => self.do_import(path, items, *all),
            Expr::If { arms, els } => self.do_if(arms, els.as_deref(), None, out),
            Expr::For { vars, iter, body } => self.do_for(vars, iter, body, None, out),
            Expr::Bin { lhs, op, rhs } => {
                if let Some(v) = self.bin_val(lhs, *op, rhs) {
                    let st = self.cur();
                    value_inline(&v, st, out, self);
                }
            }
            Expr::Lit(v) => {
                let st = self.cur();
                value_inline(v, st, out, self);
            }
            Expr::Func {
                name,
                args,
                content,
            } => self.func(name, args, content.as_deref(), out),
        }
    }

    // --------------------- rhs / conditions ---------------------

    /// Resolve a variable name, walking `.field` segments through dicts.
    fn resolve_var(&mut self, name: &str) -> Option<Value> {
        if let Some(v) = self.scope.get(name) {
            return Some(v.clone());
        }
        let mut parts = name.split('.');
        let mut v = self.scope.get(parts.next()?)?.clone();
        for p in parts {
            match v {
                Value::Dict(ps) => {
                    v = ps.iter().find(|(k, _)| k == p)?.1.clone();
                }
                _ => return None,
            }
        }
        Some(v)
    }

    fn rhs_val(&mut self, r: &Rhs) -> Option<Value> {
        match r {
            Rhs::Lit(v) => Some(v.clone()),
            Rhs::Var(n) => match self.resolve_var(n) {
                Some(v) => Some(v),
                None => {
                    self.diag(Severity::Warning, format!("undefined variable '#{n}'"));
                    None
                }
            },
            Rhs::Field { base, name } => {
                let b = self.rhs_val(base)?;
                match b {
                    Value::Dict(ps) => match ps.iter().find(|(k, _)| k == name) {
                        Some((_, v)) => Some(v.clone()),
                        None => {
                            self.diag(Severity::Warning, format!("no field '{name}' in dict"));
                            None
                        }
                    },
                    _ => {
                        self.diag(
                            Severity::Warning,
                            format!("cannot access field '{name}' of {}", kind(&b)),
                        );
                        None
                    }
                }
            }
            Rhs::Bin { lhs, op, rhs } => self.bin_val(lhs, *op, rhs),
        }
    }

    fn bin_val(&mut self, lhs: &Rhs, op: ArithOp, rhs: &Rhs) -> Option<Value> {
        let l = self.rhs_val(lhs)?;
        let r = self.rhs_val(rhs)?;
        self.arith(&l, op, &r)
    }

    /// `+ - * /` on ints/floats/lengths; `+` concatenates strings.
    fn arith(&mut self, l: &Value, op: ArithOp, r: &Value) -> Option<Value> {
        let em = self.st.font_size;
        match (l, r) {
            (Value::Int(a), Value::Int(b)) => {
                let v = match op {
                    ArithOp::Add => a.checked_add(*b),
                    ArithOp::Sub => a.checked_sub(*b),
                    ArithOp::Mul => a.checked_mul(*b),
                    ArithOp::Div => a.checked_div(*b),
                };
                match v {
                    Some(x) => Some(Value::Int(x)),
                    None => {
                        self.diag(Severity::Warning, "integer overflow or division by zero");
                        None
                    }
                }
            }
            (Value::Str(a), Value::Str(b)) => match op {
                ArithOp::Add => Some(Value::Str(format!("{a}{b}"))),
                _ => {
                    self.diag(Severity::Warning, "strings only support `+`");
                    None
                }
            },
            (Value::Length(x), Value::Length(y)) => {
                let (a, b) = (x.pt(em), y.pt(em));
                match op {
                    ArithOp::Add => Some(Value::Length(Length::Pt(a + b))),
                    ArithOp::Sub => Some(Value::Length(Length::Pt(a - b))),
                    ArithOp::Div if b != 0.0 => Some(Value::Float(a / b)),
                    _ => {
                        self.diag(Severity::Warning, "unsupported length operation");
                        None
                    }
                }
            }
            (Value::Length(x), s) if scalar(s).is_some() => {
                let (a, b) = (x.pt(em), scalar(s).unwrap());
                match op {
                    ArithOp::Mul => Some(Value::Length(Length::Pt(a * b))),
                    ArithOp::Div if b != 0.0 => Some(Value::Length(Length::Pt(a / b))),
                    _ => {
                        self.diag(Severity::Warning, "unsupported length operation");
                        None
                    }
                }
            }
            (s, Value::Length(y)) if scalar(s).is_some() => {
                let (a, b) = (scalar(s).unwrap(), y.pt(em));
                match op {
                    ArithOp::Mul => Some(Value::Length(Length::Pt(a * b))),
                    _ => {
                        self.diag(Severity::Warning, "unsupported length operation");
                        None
                    }
                }
            }
            _ => match (scalar(l), scalar(r)) {
                (Some(a), Some(b)) => {
                    let x = match op {
                        ArithOp::Add => a + b,
                        ArithOp::Sub => a - b,
                        ArithOp::Mul => a * b,
                        ArithOp::Div => {
                            if b == 0.0 {
                                self.diag(Severity::Warning, "division by zero");
                                return None;
                            }
                            a / b
                        }
                    };
                    Some(Value::Float(x))
                }
                _ => {
                    self.diag(
                        Severity::Warning,
                        format!("cannot apply arithmetic to {} and {}", kind(l), kind(r)),
                    );
                    None
                }
            },
        }
    }

    /// Evaluate a comparison to a boolean.
    fn cmp_val(&mut self, c: &Cmp) -> bool {
        let Some(l) = self.rhs_val(&c.lhs) else {
            return false;
        };
        let Some(r) = self.rhs_val(&c.rhs) else {
            return false;
        };
        // `x != none` / `x == none` are truthiness tests — this also
        // covers the bare `#if x` desugar.
        let res = match (&l, &r, c.op) {
            (_, Value::None, CmpOp::Ne) => truthy(&l),
            (Value::None, _, CmpOp::Ne) => truthy(&r),
            (_, Value::None, CmpOp::Eq) => !truthy(&l),
            (Value::None, _, CmpOp::Eq) => !truthy(&r),
            _ => self.compare(c.op, &l, &r),
        };
        if c.negated {
            !res
        } else {
            res
        }
    }

    fn compare(&mut self, op: CmpOp, l: &Value, r: &Value) -> bool {
        use std::cmp::Ordering::*;
        let em = self.st.font_size;
        let ord = match (l, r) {
            (Value::Int(a), Value::Int(b)) => Some(a.cmp(b)),
            (Value::Str(a), Value::Str(b)) => Some(a.cmp(b)),
            (Value::Bool(a), Value::Bool(b)) => Some(a.cmp(b)),
            _ => match (num_or_len(l, em), num_or_len(r, em)) {
                (Some(a), Some(b)) => a.partial_cmp(&b),
                _ => None,
            },
        };
        match ord {
            Some(o) => match op {
                CmpOp::Eq => o == Equal,
                CmpOp::Ne => o != Equal,
                CmpOp::Lt => o == Less,
                CmpOp::Le => o != Greater,
                CmpOp::Gt => o == Greater,
                CmpOp::Ge => o != Less,
            },
            None => match op {
                CmpOp::Eq => l == r,
                CmpOp::Ne => l != r,
                _ => {
                    self.diag(
                        Severity::Warning,
                        format!("cannot compare {} and {}", kind(l), kind(r)),
                    );
                    false
                }
            },
        }
    }

    // ----------------- control flow / imports -----------------

    fn do_if(
        &mut self,
        arms: &[(Cmp, Vec<Node>)],
        els: Option<&[Node]>,
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) {
        for (c, body) in arms {
            if self.cmp_val(c) {
                self.run_body(body, st, out);
                return;
            }
        }
        if let Some(b) = els {
            self.run_body(b, st, out);
        }
    }

    fn do_for(
        &mut self,
        vars: &[String],
        iter: &Rhs,
        body: &[Node],
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) {
        enum It {
            One(Value),
            Two(Value, Value),
        }
        if vars.is_empty() {
            self.diag(Severity::Warning, "#for needs a variable");
            return;
        }
        let Some(it) = self.rhs_val(iter) else { return };
        let items: Vec<It> = match it {
            Value::Array(vs) if vars.len() == 2 => vs
                .into_iter()
                .enumerate()
                .map(|(i, v)| It::Two(Value::Int(i as i64), v))
                .collect(),
            Value::Array(vs) => vs.into_iter().map(It::One).collect(),
            Value::Dict(ps) if vars.len() == 2 => ps
                .into_iter()
                .map(|(k, v)| It::Two(Value::Str(k), v))
                .collect(),
            Value::Dict(ps) => ps
                .into_iter()
                .map(|(k, _)| It::One(Value::Str(k)))
                .collect(),
            other => {
                self.diag(
                    Severity::Warning,
                    format!("cannot iterate over {}", kind(&other)),
                );
                return;
            }
        };
        if vars.len() > 2 {
            self.diag(Severity::Warning, "#for accepts at most two variables");
        }
        let saved: Vec<Option<Value>> = vars.iter().map(|v| self.scope.get(v).cloned()).collect();
        for item in items {
            match item {
                It::One(v) => {
                    self.scope.insert(vars[0].clone(), v);
                }
                It::Two(k, v) => {
                    self.scope.insert(vars[0].clone(), k);
                    self.scope.insert(vars[1].clone(), v);
                }
            }
            self.run_body(body, st, out);
        }
        for (v, s) in vars.iter().zip(saved) {
            match s {
                Some(x) => {
                    self.scope.insert(v.clone(), x);
                }
                None => {
                    self.scope.remove(v);
                }
            }
        }
    }

    /// Resolve a spec, check the recursion guards, load + parse the file.
    /// On success the path is in `env.loaded`; caller removes it after.
    fn enter_file(&mut self, spec: &str) -> Option<(Rc<Vec<Node>>, PathBuf)> {
        let path = match package::resolve(spec, self.env.base_dir) {
            Ok(p) => p,
            Err(e) => {
                self.diag(Severity::Error, e);
                return None;
            }
        };
        if self.env.depth >= 64 {
            self.diag(
                Severity::Error,
                format!("{}: imports nested too deeply", path.display()),
            );
            return None;
        }
        let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if !self.env.loaded.insert(canon.clone()) {
            self.diag(
                Severity::Error,
                format!("cyclic import of {}", path.display()),
            );
            return None;
        }
        match package::load(&canon) {
            Ok((nodes, pds)) => {
                for d in pds {
                    self.diags.push(Diag::new(
                        d.severity,
                        None,
                        format!("{}: {}", canon.display(), d.message),
                    ));
                }
                Some((nodes, canon))
            }
            Err(e) => {
                self.env.loaded.remove(&canon);
                self.diag(Severity::Error, e);
                None
            }
        }
    }

    /// `#import` — evaluate the file in a fresh scope, expose the bound
    /// names as a dict, then bind per items/all/stem.
    fn do_import(&mut self, spec: &str, items: &[String], all: bool) {
        let Some((nodes, canon)) = self.enter_file(spec) else {
            return;
        };
        let dict = {
            let base = canon.parent().unwrap_or(self.env.base_dir);
            let mut subenv = EvalEnv {
                base_dir: base,
                registry: &mut *self.env.registry,
                depth: self.env.depth + 1,
                loaded: &mut *self.env.loaded,
            };
            let mut sub = Ev::new(self.diags, &mut subenv);
            let mut o = Vec::new();
            for n in nodes.iter() {
                sub.node(n, &mut o);
            }
            sub.flush(&mut o);
            let mut entries: Vec<(String, Value)> = sub.scope.into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Dict(entries)
        };
        self.env.loaded.remove(&canon);
        let Value::Dict(entries) = dict else { return };
        if all {
            for (k, v) in entries {
                self.scope.insert(k, v);
            }
        } else if !items.is_empty() {
            for it in items {
                match entries.iter().find(|(k, _)| k == it) {
                    Some((_, v)) => {
                        self.scope.insert(it.clone(), v.clone());
                    }
                    None => self.diag(
                        Severity::Warning,
                        format!("module '{spec}' has no member '{it}'"),
                    ),
                }
            }
        } else {
            self.scope.insert(module_stem(spec), Value::Dict(entries));
        }
    }

    /// `#include` — parse and evaluate the file's nodes inline (blocks
    /// are spliced into the output at this point).
    fn do_include(&mut self, spec: &str, out: &mut Vec<Inline>, st: Option<TextStyle>) {
        let Some((nodes, canon)) = self.enter_file(spec) else {
            return;
        };
        self.env.depth += 1;
        self.run_body(&nodes, st, out);
        self.env.depth -= 1;
        self.env.loaded.remove(&canon);
    }

    // ------------------------- functions -------------------------

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
                let st = self.cur();
                self.blocks.push(Block::Paragraph {
                    content: vec![Inline::Text { text: s, style: st }],
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
            "h" => match arg_len(args, self.st.font_size) {
                Some(p) => out.push(Inline::HSpace(p)),
                None => self.diag(Severity::Error, "#h expects a length"),
            },
            "linebreak" => out.push(Inline::LineBreak),
            "strong" => {
                if let Some(c) = content {
                    let st = self.cur();
                    let f = self.vary(st.font, Some(true), None, None);
                    inlines(c, with_font(st, f), out, self);
                }
            }
            "emph" => {
                if let Some(c) = content {
                    let st = self.cur();
                    let f = self.vary(st.font, None, Some(true), None);
                    inlines(c, with_font(st, f), out, self);
                }
            }
            "text" => {
                let st = self.cur();
                let st = text_style(args, st, self);
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
                            if let Node::Text(t) = n {
                                Some(t.trim().to_string())
                            } else {
                                None
                            }
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
                    let st = self.cur();
                    inlines(c, st, &mut v, self);
                    self.blocks.push(Block::Paragraph {
                        content: v,
                        align: al,
                    });
                }
            }
            "include" => {
                let spec = args.iter().find_map(|a| {
                    if a.name.is_none() {
                        if let Value::Str(s) = &a.value {
                            Some(s.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                match spec {
                    Some(s) => self.do_include(&s, out, None),
                    None => self.diag(Severity::Error, "#include expects a string path"),
                }
            }
            "table" | "grid" => self.table(name, args, out),
            _ => self.diag(
                Severity::NotImplemented,
                format!("function '#{name}' is not implemented"),
            ),
        }
    }

    /// `#table(..)` / `#grid(..)` -> `Block::Table`.
    fn table(&mut self, name: &str, args: &[Arg], out: &mut Vec<Inline>) {
        self.flush(out);
        let mut columns = None;
        let mut stroke = name == "table";
        let mut cells = Vec::new();
        for a in args {
            match a.name.as_deref() {
                Some("columns") => columns = self.col_specs(&a.value),
                Some("stroke") => match &a.value {
                    Value::Bool(b) => stroke = *b,
                    _ => self.diag(Severity::Warning, "stroke expects a boolean"),
                },
                Some(other) => self.diag(
                    Severity::Warning,
                    format!("{name} argument '{other}' ignored"),
                ),
                None => {
                    let st = self.cur();
                    let mut c = Vec::new();
                    value_inline(&a.value, st, &mut c, self);
                    cells.push(c);
                }
            }
        }
        let columns = columns.unwrap_or_else(|| vec![ColSpec::Auto]);
        self.blocks.push(Block::Table {
            columns,
            cells,
            stroke,
        });
    }

    /// `columns:` arg -> column specs. Int(n) -> n auto columns; an
    /// array of `auto`/lengths/`Nfr`; a bare length -> one fixed column.
    fn col_specs(&mut self, v: &Value) -> Option<Vec<ColSpec>> {
        let em = self.st.font_size;
        match v {
            Value::Int(n) => {
                if *n <= 0 || *n > 256 {
                    self.diag(Severity::Warning, format!("invalid column count {n}"));
                    Some(vec![ColSpec::Auto])
                } else {
                    Some(vec![ColSpec::Auto; *n as usize])
                }
            }
            Value::Array(vs) => {
                let mut out = Vec::new();
                for v in vs {
                    match col_spec(v, em) {
                        Some(c) => out.push(c),
                        None => self.diag(Severity::Warning, "invalid column spec"),
                    }
                }
                if out.is_empty() {
                    out.push(ColSpec::Auto);
                }
                Some(out)
            }
            Value::Length(_) => col_spec(v, em).map(|c| vec![c]),
            _ => {
                self.diag(Severity::Warning, "invalid columns value");
                None
            }
        }
    }

    // ----------------------------- set ----------------------------

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
            Value::Dict(ps) => {
                let mut any = false;
                for (k, vv) in ps {
                    let Some(p) = val_pt(vv, em) else { continue };
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
                        _ => self.diag(Severity::Warning, format!("unknown margin key '{k}'")),
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
                        self.st.line_height = 1.3082 * p;
                    }
                    None => self.diag(Severity::Warning, "text size expects a length"),
                },
                Some("font") => match &a.value {
                    Value::Str(s) => {
                        self.st.font_family = Some(s.clone());
                        // Eager resolve of the regular face so a missing
                        // family warns immediately, not at first glyph.
                        self.resolve_face(s, false, false, false);
                    }
                    Value::None => self.st.font_family = None,
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
                Some(other) => {
                    self.diag(Severity::Warning, format!("par argument '{other}' ignored"))
                }
                None => self.diag(Severity::Warning, "positional par argument ignored"),
            }
        }
    }
}

// ------------------------- free helpers -------------------------

/// Base-14 face for a (bold, italic, mono) combination.
fn base14(bold: bool, italic: bool, mono: bool) -> FontId {
    if mono {
        if bold {
            FontId::MonoBold
        } else {
            FontId::Mono
        }
    } else {
        match (bold, italic) {
            (true, true) => FontId::BoldItalic,
            (true, false) => FontId::Bold,
            (false, true) => FontId::Italic,
            (false, false) => FontId::Regular,
        }
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

/// Int/Float -> f64.
fn scalar(v: &Value) -> Option<f64> {
    match v {
        Value::Int(i) => Some(*i as f64),
        Value::Float(f) => Some(*f),
        _ => None,
    }
}

/// Int/Float/Length -> f64 for comparisons.
fn num_or_len(v: &Value, em: f64) -> Option<f64> {
    match v {
        Value::Length(l) => Some(l.pt(em)),
        _ => scalar(v),
    }
}

/// Truthiness for bare `#if x`: non-none, non-false, non-zero.
fn truthy(v: &Value) -> bool {
    match v {
        Value::None => false,
        Value::Bool(b) => *b,
        Value::Int(i) => *i != 0,
        Value::Float(f) => *f != 0.0,
        _ => true,
    }
}

/// Value kind name for diagnostics.
fn kind(v: &Value) -> &'static str {
    match v {
        Value::None => "none",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Length(_) => "length",
        Value::Str(_) => "string",
        Value::Content(_) => "content",
        Value::Array(_) => "array",
        Value::Dict(_) => "dict",
    }
}

/// `columns:` element -> ColSpec.
fn col_spec(v: &Value, em: f64) -> Option<ColSpec> {
    match v {
        Value::None => Some(ColSpec::Auto),
        Value::Str(s) if s.trim() == "auto" => Some(ColSpec::Auto),
        Value::Str(s) if s.ends_with("fr") => {
            s[..s.len() - 2].trim().parse::<f64>().ok().map(ColSpec::Fr)
        }
        Value::Length(l) => Some(ColSpec::Fixed(l.pt(em))),
        Value::Int(i) => Some(ColSpec::Fr(*i as f64)),
        Value::Float(f) => Some(ColSpec::Fr(*f)),
        _ => None,
    }
}

/// The name an `#import "spec"` module binds under.
fn module_stem(spec: &str) -> String {
    if spec.starts_with('@') {
        spec.rsplit('/')
            .next()
            .and_then(|s| s.split(':').next())
            .unwrap_or("module")
            .to_string()
    } else {
        Path::new(spec)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("module")
            .to_string()
    }
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
                    st.font = cx.vary(st.font, Some(true), None, None);
                }
            }
            Some("font") => match &a.value {
                Value::Str(f) => {
                    st.font = cx.vary(st.font, None, None, Some(f));
                }
                _ => cx.diag(Severity::Warning, "text font expects a string"),
            },
            Some("style") => match &a.value {
                Value::Str(s) if s.eq_ignore_ascii_case("italic") => {
                    st.font = cx.vary(st.font, None, Some(true), None);
                }
                _ => cx.diag(Severity::Warning, "text style expects a string"),
            },
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
            Node::Text(s) => out.push(Inline::Text {
                text: s.clone(),
                style: st,
            }),
            Node::LineBreak => out.push(Inline::LineBreak),
            Node::Strong(ch) => {
                let f = cx.vary(st.font, Some(true), None, None);
                inlines(ch, with_font(st, f), out, cx);
            }
            Node::Emph(ch) => {
                let f = cx.vary(st.font, None, Some(true), None);
                inlines(ch, with_font(st, f), out, cx);
            }
            Node::Raw { text, block } => {
                if *block {
                    cx.diag(Severity::NotImplemented, "raw block inside inline content");
                }
                out.push(Inline::Text {
                    text: text.clone(),
                    style: TextStyle {
                        font: FontId::Mono,
                        size: st.size,
                    },
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
            if let Some(v) = cx.rhs_val(value) {
                cx.scope.insert(name.clone(), v);
            }
        }
        Expr::Var { name } => match cx.resolve_var(name) {
            Some(v) => value_inline(&v, st, out, cx),
            None => cx.diag(Severity::Warning, format!("undefined variable '#{name}'")),
        },
        Expr::Lit(v) => value_inline(v, st, out, cx),
        Expr::Bin { lhs, op, rhs } => {
            if let Some(v) = cx.bin_val(lhs, *op, rhs) {
                value_inline(&v, st, out, cx);
            }
        }
        Expr::Import { path, items, all } => cx.do_import(path, items, *all),
        Expr::If { arms, els } => cx.do_if(arms, els.as_deref(), Some(st), out),
        Expr::For { vars, iter, body } => cx.do_for(vars, iter, body, Some(st), out),
        Expr::Func {
            name,
            args,
            content,
        } => match name.as_str() {
            "h" => match arg_len(args, st.size) {
                Some(p) => out.push(Inline::HSpace(p)),
                None => cx.diag(Severity::Error, "#h expects a length"),
            },
            "linebreak" => out.push(Inline::LineBreak),
            "strong" => {
                if let Some(c) = content {
                    let f = cx.vary(st.font, Some(true), None, None);
                    inlines(c, with_font(st, f), out, cx);
                }
            }
            "emph" => {
                if let Some(c) = content {
                    let f = cx.vary(st.font, None, Some(true), None);
                    inlines(c, with_font(st, f), out, cx);
                }
            }
            "text" => {
                let st2 = text_style(args, st, cx);
                if let Some(c) = content {
                    inlines(c, st2, out, cx);
                }
            }
            "include" => {
                let spec = args.iter().find_map(|a| {
                    if a.name.is_none() {
                        if let Value::Str(s) = &a.value {
                            Some(s.clone())
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                });
                match spec {
                    Some(s) => cx.do_include(&s, out, Some(st)),
                    None => cx.diag(Severity::Error, "#include expects a string path"),
                }
            }
            "lorem" | "pagebreak" | "v" | "align" | "table" | "grid" => cx.diag(
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
        Value::Str(_)
        | Value::Int(_)
        | Value::Float(_)
        | Value::Length(_)
        | Value::Array(_)
        | Value::Dict(_) => {
            out.push(Inline::Text {
                text: value_str(v),
                style: st,
            });
        }
        Value::Content(ns) => inlines(ns, st, out, cx),
        Value::Bool(_) | Value::None => {}
    }
}

/// Display form of a value: arrays join with ", ", dicts render
/// `(k: v, ...)`.
fn value_str(v: &Value) -> String {
    match v {
        Value::Str(s) => s.clone(),
        Value::Int(i) => format!("{i}"),
        Value::Float(f) => format!("{f}"),
        Value::Bool(b) => format!("{b}"),
        Value::Length(Length::Pt(p)) => format!("{p}pt"),
        Value::Length(Length::Em(e)) => format!("{e}em"),
        Value::Array(vs) => vs.iter().map(value_str).collect::<Vec<_>>().join(", "),
        Value::Dict(ps) => format!(
            "({})",
            ps.iter()
                .map(|(k, v)| format!("{k}: {}", value_str(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Content(ns) => ns
            .iter()
            .filter_map(|n| {
                if let Node::Text(t) = n {
                    Some(t.as_str())
                } else {
                    None
                }
            })
            .collect(),
        Value::None => "none".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// No fonts resolve — family requests fall back to base-14.
    struct NoFonts;
    impl FaceRegistry for NoFonts {
        fn resolve(&mut self, _: &str, _: bool, _: bool, _: bool) -> Option<FontId> {
            None
        }
    }

    fn run_env(nodes: Vec<Node>, base: &Path) -> (Document, Vec<Diag>) {
        let mut reg = NoFonts;
        let mut loaded = HashSet::new();
        let mut env = EvalEnv {
            base_dir: base,
            registry: &mut reg,
            depth: 0,
            loaded: &mut loaded,
        };
        let mut d = Vec::new();
        let doc = eval(nodes, &mut d, &mut env);
        (doc, d)
    }

    fn run(nodes: Vec<Node>) -> (Document, Vec<Diag>) {
        run_env(nodes, Path::new("."))
    }

    /// Parse + eval a source string rooted at `base`.
    fn run_src(src: &str, base: &Path) -> (Document, Vec<Diag>) {
        let (nodes, mut d) = crate::syntax::parse(src);
        let (doc, ed) = run_env(nodes, base);
        d.extend(ed);
        (doc, d)
    }

    /// All rendered text of a document, concatenated.
    fn all_text(doc: &Document) -> String {
        let mut s = String::new();
        fn walk(v: &[Inline], s: &mut String) {
            for i in v {
                if let Inline::Text { text, .. } = i {
                    s.push_str(text);
                }
            }
        }
        for b in &doc.blocks {
            match b {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    walk(content, &mut s)
                }
                Block::List { items, .. } => items.iter().for_each(|i| walk(i, &mut s)),
                Block::Table { cells, .. } => cells.iter().for_each(|c| walk(c, &mut s)),
                _ => {}
            }
        }
        s
    }

    fn named(name: &str, value: Value) -> Arg {
        Arg {
            name: Some(name.to_string()),
            value,
        }
    }

    fn pos(value: Value) -> Arg {
        Arg { name: None, value }
    }

    fn letv(name: &str, value: Rhs) -> Node {
        Node::Expr(Expr::Let {
            name: name.into(),
            value,
        })
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
        let Block::Paragraph { content, align } = &doc.blocks[0] else {
            panic!()
        };
        assert_eq!(*align, HAlign::Left);
        assert_eq!(content.len(), 3);
        let Inline::Text { text, style } = &content[1] else {
            panic!()
        };
        assert_eq!(text, "bold");
        assert_eq!(style.font, FontId::Bold);
        assert_eq!(style.size, 11.0);
        let Inline::Text { style, .. } = &content[2] else {
            panic!()
        };
        assert_eq!(style.font, FontId::BoldItalic);
    }

    #[test]
    fn eval_block_nodes() {
        let (doc, _) = run(vec![
            Node::Text("intro".into()),
            Node::Heading {
                level: 2,
                body: vec![Node::Text("T".into())],
            },
            Node::List {
                ordered: true,
                items: vec![vec![Node::Text("a".into())], vec![Node::Text("b".into())]],
            },
            Node::Raw {
                text: "x\ny".into(),
                block: true,
            },
            Node::Raw {
                text: "code".into(),
                block: false,
            },
        ]);
        assert_eq!(doc.blocks.len(), 5);
        assert!(matches!(doc.blocks[0], Block::Paragraph { .. }));
        assert!(matches!(doc.blocks[1], Block::Heading { level: 2, .. }));
        assert!(matches!(doc.blocks[2], Block::List { ordered: true, .. }));
        assert!(matches!(doc.blocks[3], Block::RawBlock { .. }));
        // trailing inline raw becomes its own paragraph in Mono.
        let Block::Paragraph { content, .. } = &doc.blocks[4] else {
            panic!()
        };
        let Inline::Text { style, .. } = &content[0] else {
            panic!()
        };
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
        let (doc, _) = run(vec![Node::Expr(Expr::Set {
            target: "page".into(),
            args: vec![named(
                "margin",
                Value::Dict(vec![
                    ("x".into(), Value::Length(Length::Pt(10.0))),
                    ("top".into(), Value::Length(Length::Pt(20.0))),
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
            letv("x", Rhs::Lit(Value::Str("hi".into()))),
            letv("n", Rhs::Lit(Value::Length(Length::Pt(12.0)))),
            Node::Text("say ".into()),
            Node::Expr(Expr::Var { name: "x".into() }),
            Node::Text(" at ".into()),
            Node::Expr(Expr::Var { name: "n".into() }),
            Node::Expr(Expr::Var {
                name: "missing".into(),
            }),
        ]);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!()
        };
        let texts: Vec<&str> = content
            .iter()
            .filter_map(|i| {
                if let Inline::Text { text, .. } = i {
                    Some(text.as_str())
                } else {
                    None
                }
            })
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
            Node::Expr(Expr::Func {
                name: "pagebreak".into(),
                args: vec![],
                content: None,
            }),
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
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!()
        };
        let Inline::Text { text, .. } = &content[0] else {
            panic!()
        };
        assert_eq!(text, "Lorem ipsum dolor sit amet.");
        assert!(matches!(doc.blocks[1], Block::VSpace(v) if v == 24.0));
        assert!(matches!(doc.blocks[2], Block::PageBreak));
        let Block::Paragraph { content, .. } = &doc.blocks[3] else {
            panic!()
        };
        let Inline::Text { style, .. } = &content[0] else {
            panic!()
        };
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
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!()
        };
        let Inline::Text { style, .. } = &content[0] else {
            panic!()
        };
        assert_eq!(style.size, 20.0);
        assert_eq!(style.font, FontId::Bold);
        let Block::Paragraph { align, .. } = &doc.blocks[1] else {
            panic!()
        };
        assert_eq!(*align, HAlign::Center);
    }

    #[test]
    fn eval_unknown_set_and_font() {
        let (_doc, d) = run(vec![
            Node::Expr(Expr::Set {
                target: "grid".into(),
                args: vec![],
            }),
            Node::Expr(Expr::Set {
                target: "text".into(),
                args: vec![named("font", Value::Str("Comic Sans".into()))],
            }),
        ]);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].severity, Severity::NotImplemented);
        assert_eq!(d[1].severity, Severity::Warning);
    }

    // ------------------- new-language tests -------------------

    #[test]
    fn eval_arith_and_fields() {
        let (doc, d) = run_src(
            "#let n = 4\n#let m = n * 3\n#let d = (a: 7)\n#let x = d.a\n#let s = \"he\" + \"llo\"\nm=#m x=#x s=#s",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "m=12 x=7 s=hello");
    }

    #[test]
    fn eval_bin_in_markup() {
        let (doc, d) = run_src("#let a = 20\na + 1 = #a + 1", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "a + 1 = 21");
    }

    #[test]
    fn eval_if() {
        let (doc, d) = run_src(
            "#if 2 > 1 [yes] else [no]\n#if not false [t]\n#if x [a] else [b]",
            Path::new("."),
        );
        assert_eq!(d.len(), 1); // undefined 'x' warning
        assert_eq!(all_text(&doc), "yes t b");
    }

    #[test]
    fn eval_for() {
        let (doc, d) = run_src(
            "#for x in (1, 2, 3) [#x]\n#for (i, v) in (\"a\", \"b\") [#i:#v;]\n#for k in (z: 1) [#k]",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc), "123 0:a;1:b; z");
    }

    #[test]
    fn eval_table_grid() {
        let (doc, d) = run_src(
            "#table(columns: 2, [a], [b], [c], [d])\n#grid(columns: (1fr, auto), [x])",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let tables: Vec<&Block> = doc
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Table { .. }))
            .collect();
        assert_eq!(tables.len(), 2);
        let Block::Table {
            columns,
            cells,
            stroke,
        } = tables[0]
        else {
            panic!()
        };
        assert_eq!(columns.len(), 2);
        assert_eq!(cells.len(), 4);
        assert!(*stroke);
        let Block::Table {
            columns,
            cells,
            stroke,
        } = tables[1]
        else {
            panic!()
        };
        assert_eq!(columns, &[ColSpec::Fr(1.0), ColSpec::Auto]);
        assert_eq!(cells.len(), 1);
        assert!(!*stroke);
    }

    #[test]
    fn eval_array_dict_display() {
        let (doc, d) = run_src(
            "#let a = (1, \"x\")\n#let d = (k: 2)\n#a / #d",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "1, x / (k: 2)");
    }

    #[test]
    fn eval_include_and_import() {
        let dir = std::env::temp_dir().join(format!("typrst-eval-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("lib.typ"),
            "#let greeting = \"hello\"\n#let n = 41\n",
        )
        .unwrap();
        std::fs::write(dir.join("part.typ"), "Included text.").unwrap();

        // `#import "lib.typ"` binds the module under its stem.
        let (doc, d) = run_src("#import \"lib.typ\"\n#lib.greeting #lib.n", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "hello 41");

        // `: items` binds members directly.
        let (doc, d) = run_src("#import \"lib.typ\": greeting\n#greeting!", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "hello!");

        // `*` binds every member.
        let (doc, d) = run_src("#import \"lib.typ\": *\n#greeting/#n", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "hello/41");

        // missing member warns.
        let (_doc, d) = run_src("#import \"lib.typ\": nope", &dir);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].severity, Severity::Warning);

        // `#include` splices blocks.
        let (doc, d) = run_src("Before #include \"part.typ\" after.", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc), "Before Included text. after.");

        // missing file errors.
        let (_doc, d) = run_src("#include \"missing.typ\"", &dir);
        assert!(d.iter().any(|x| x.severity == Severity::Error));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eval_import_cycle() {
        let dir = std::env::temp_dir().join(format!("typrst-cyc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.typ"), "#import \"b.typ\"\n#let x = 1").unwrap();
        std::fs::write(dir.join("b.typ"), "#import \"a.typ\"\n#let y = 2").unwrap();
        let (_doc, d) = run_src("#import \"a.typ\"", &dir);
        assert!(d.iter().any(|x| x.message.contains("cyclic")), "{d:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn eval_font_registry() {
        // Registry that resolves every family to successive Ext faces.
        struct Faces(Vec<(String, bool, bool, bool)>);
        impl FaceRegistry for Faces {
            fn resolve(&mut self, f: &str, b: bool, i: bool, m: bool) -> Option<FontId> {
                self.0.push((f.to_string(), b, i, m));
                Some(FontId::Ext(self.0.len() as u16 - 1))
            }
        }
        let (nodes, _) = crate::syntax::parse("#set text(font: \"Fira\")\nplain *bold* _it_");
        let mut reg = Faces(Vec::new());
        let mut loaded = HashSet::new();
        let mut env = EvalEnv {
            base_dir: Path::new("."),
            registry: &mut reg,
            depth: 0,
            loaded: &mut loaded,
        };
        let mut d = Vec::new();
        let doc = eval(nodes, &mut d, &mut env);
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(doc.styles.font_family.as_deref(), Some("Fira"));
        // regular + bold + italic combos resolved through the registry.
        assert!(reg.0.contains(&("Fira".into(), false, false, false)));
        assert!(reg.0.contains(&("Fira".into(), true, false, false)));
        assert!(reg.0.contains(&("Fira".into(), false, true, false)));
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!()
        };
        let Inline::Text { style, .. } = &content[0] else {
            panic!()
        };
        assert!(matches!(style.font, FontId::Ext(_)));
    }
}
