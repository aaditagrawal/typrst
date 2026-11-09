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
    // Own the tree so paragraph strings move into the document instead of
    // being cloned. Imports and `#for` bodies stay on the borrowing path
    // (`node`) because those trees are shared or evaluated more than once.
    for n in nodes {
        ev.eat(n, &mut pending);
    }
    ev.flush(&mut pending);
    if ev.saw_ref {
        ev.resolve();
    }
    // Headings after `#outline()` are included, so the entries are filled
    // in a second walk. Documents that never call it pay for this check
    // and nothing else. `#figure` only bumps a counter while it is called.
    if ev.saw_outline {
        fill_outlines(&mut ev.blocks);
    }
    Document {
        styles: ev.st,
        blocks: ev.blocks,
        diags: Vec::new(),
        fonts: Vec::new(),
        images: ev.images,
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

/// What `@name` renders. Non-heading labels keep a display string
/// (a paragraph, or `Figure N`). A heading stores its section number,
/// or `Unnumbered` when numbering is off.
#[derive(Clone, Debug)]
enum LabelTarget {
    Text(String),
    /// `1`, `1.1`, … without a pattern's decorative trailing dot.
    Section(String),
    Unnumbered,
}

struct Ev<'e, 'a> {
    st: Styles,
    /// Scope stack. Index 0 is the root document. Nested frames exist
    /// only while a content block (`#if` / `#while` / `#for` body,
    /// function body, bracket argument) is being evaluated.
    scopes: Vec<HashMap<String, Value>>,
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
    /// Heading numbering. `None` leaves headings unnumbered.
    numbering: Option<NumPat>,
    /// Per-level counters. Index 0 is heading level 1.
    hcount: [u32; 6],
    /// Label name to what `@name` renders. Allocated only when a label
    /// is defined.
    labels: Option<HashMap<String, LabelTarget>>,
    /// Labels seen inside the block currently being built.
    pending_labels: Vec<String>,
    /// Section number of the heading just finished (`1`, `1.1`).
    /// `None` when that heading has no numbering. Read only while the
    /// last block is that heading.
    heading_number: Option<String>,
    /// An `@ref` was emitted. Gates the single resolve walk.
    saw_ref: bool,
    /// `#outline()` was evaluated. Gates the heading-collection walk.
    saw_outline: bool,
    /// `#figure` counter. Incremented only inside `do_figure`.
    figures: u32,
    /// JPEGs loaded by `#image`, in document order.
    images: Vec<Image>,
    /// Directory stack for the file being evaluated. Empty means
    /// `env.base_dir`. `#include` pushes the included file's directory
    /// so image paths resolve next to that file.
    include_dirs: Vec<PathBuf>,
    /// User-function recursion depth. Calls error at 64.
    fn_depth: u32,
}

impl<'e, 'a> Ev<'e, 'a> {
    fn new(diags: &'e mut Vec<Diag>, env: &'e mut EvalEnv<'a>) -> Self {
        let mut scopes = Vec::with_capacity(2);
        scopes.push(HashMap::new());
        Self {
            st: Styles::default(),
            scopes,
            diags,
            blocks: Vec::new(),
            env,
            face_info: Vec::new(),
            face_cache: HashMap::new(),
            font_warned: HashSet::new(),
            numbering: None,
            hcount: [0; 6],
            labels: None,
            pending_labels: Vec::new(),
            heading_number: None,
            saw_ref: false,
            saw_outline: false,
            figures: 0,
            images: Vec::new(),
            include_dirs: Vec::new(),
            fn_depth: 0,
        }
    }

    /// Directory of the file currently being evaluated.
    fn dir(&self) -> &Path {
        self.include_dirs
            .last()
            .map(PathBuf::as_path)
            .unwrap_or(self.env.base_dir)
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
        if out.is_empty() {
            self.seal();
            return;
        }
        self.push_block(Block::Paragraph {
            content: std::mem::take(out),
            align: HAlign::Left,
        });
    }

    fn push_block(&mut self, block: Block) {
        self.blocks.push(block);
        self.seal();
    }

    /// Bind labels collected while this block was built. A no-op (one
    /// empty-vec check) when the document has no labels.
    fn seal(&mut self) {
        if self.pending_labels.is_empty() {
            return;
        }
        if self.blocks.is_empty() {
            let names = std::mem::take(&mut self.pending_labels);
            for name in names {
                self.diag(
                    Severity::Error,
                    format!("label `<{name}>` is not attached to a block"),
                );
            }
            return;
        }
        let target = self.label_target_of_last();
        let names = std::mem::take(&mut self.pending_labels);
        for name in names {
            self.insert_label(name, target.clone());
        }
    }

    /// A label node outside the block currently being built.
    ///
    /// A run of only spaces is the padding the parser inserts around a
    /// block-level call. When that run sits on a figure, the label
    /// attaches to the figure (stored text `Figure N`), the same way a
    /// label on the next line attaches to a heading.
    fn on_label(&mut self, name: String, out: &mut Vec<Inline>) {
        let onto_figure = pad_only(out) && matches!(self.blocks.last(), Some(Block::Figure(_)));
        if !out.is_empty() && !onto_figure {
            self.pending_labels.push(name);
            return;
        }
        if onto_figure {
            out.clear();
        }
        if self.blocks.is_empty() {
            self.diag(
                Severity::Error,
                format!("label `<{name}>` is not attached to a block"),
            );
            return;
        }
        self.insert_label(name, self.label_target_of_last());
    }

    /// Supplement for a label attached to the block just pushed.
    /// Headings do not use their plain text: a numbered heading is
    /// `Section {number}`, and an unnumbered one errors at the reference.
    fn label_target_of_last(&self) -> LabelTarget {
        match self.blocks.last() {
            Some(Block::Heading { .. }) => match &self.heading_number {
                Some(n) => LabelTarget::Section(n.clone()),
                None => LabelTarget::Unnumbered,
            },
            Some(b) => LabelTarget::Text(plain_block(b)),
            None => LabelTarget::Text(String::new()),
        }
    }

    fn insert_label(&mut self, name: String, target: LabelTarget) {
        let dup = {
            let map = self.labels.get_or_insert_with(HashMap::new);
            let dup = map.contains_key(&name);
            map.insert(name.clone(), target);
            dup
        };
        if dup {
            self.diag(Severity::Warning, format!("duplicate label `<{name}>`"));
        }
    }

    fn push_ref(&mut self, name: String, style: TextStyle, out: &mut Vec<Inline>) {
        self.saw_ref = true;
        out.push(Inline::Ref { name, style });
    }

    fn finish_heading(&mut self, level: u8, mut content: Vec<Inline>, st: TextStyle) {
        // Set before `push_block`: sealing a `<label>` in the heading
        // body reads this. `None` must overwrite a previous heading so an
        // unnumbered heading is not given the last number.
        self.heading_number = if let Some(pat) = self.numbering {
            let prefix = self.heading_prefix(level);
            let number = section_number(&prefix, pat.trailing_dot);
            content.insert(0, Inline::Text { text: prefix, style: st });
            Some(number)
        } else {
            None
        };
        self.push_block(Block::Heading { level, content });
    }

    /// `1`, `1.2`, `1.2.3` for the heading's level, plus the pattern's
    /// trailing dot and a separating space.
    fn heading_prefix(&mut self, level: u8) -> String {
        let n = (level as usize).clamp(1, 6);
        self.hcount[n - 1] = self.hcount[n - 1].saturating_add(1);
        for c in &mut self.hcount[n..] {
            *c = 0;
        }
        let trailing = matches!(self.numbering, Some(p) if p.trailing_dot);
        let mut s = String::with_capacity(n * 2 + 2);
        for i in 0..n {
            if i > 0 {
                s.push('.');
            }
            s.push_str(&self.hcount[i].to_string());
        }
        if trailing {
            s.push('.');
        }
        s.push(' ');
        s
    }

    fn resolve(&mut self) {
        let Ev {
            blocks,
            labels,
            diags,
            ..
        } = self;
        resolve_refs(blocks, labels.as_ref(), diags);
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

    /// Consuming walk used for the root tree. Moves `Node::Text` buffers.
    fn eat(&mut self, n: Node, out: &mut Vec<Inline>) {
        match n {
            Node::Text(s) => {
                let st = self.cur();
                out.push(Inline::Text { text: s, style: st });
            }
            Node::LineBreak => out.push(Inline::LineBreak),
            Node::Strong(ch) => {
                let st = self.cur();
                let f = self.vary(st.font, Some(true), None, None);
                self.eat_styled(ch, with_font(st, f), out);
            }
            Node::Emph(ch) => {
                let st = self.cur();
                let f = self.vary(st.font, None, Some(true), None);
                self.eat_styled(ch, with_font(st, f), out);
            }
            Node::Raw { text, block: false } => out.push(Inline::Text {
                text,
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
                self.eat_styled(body, st, &mut c);
                self.finish_heading(level, c, st);
            }
            Node::List { ordered, items } => {
                self.flush(out);
                let st = self.cur();
                let items = items.into_iter().map(|it| self.eat_item(it, st)).collect();
                self.push_block(Block::List { ordered, items });
            }
            Node::Terms { items } => self.eat_terms(items, out),
            Node::Raw { text, block: true } => {
                self.flush(out);
                self.push_block(Block::RawBlock { text });
            }
            Node::Label(name) => self.on_label(name, out),
            Node::Ref(name) => {
                let st = self.cur();
                self.push_ref(name, st, out);
            },
            Node::Expr(e) => self.expr(&e, out),
        }
    }

    /// Owned inline walk. Same rules as `inlines`, but text buffers move.
    fn eat_styled(&mut self, nodes: Vec<Node>, st: TextStyle, out: &mut Vec<Inline>) {
        for n in nodes {
            match n {
                Node::Text(s) => out.push(Inline::Text { text: s, style: st }),
                Node::LineBreak => out.push(Inline::LineBreak),
                Node::Strong(ch) => {
                    let f = self.vary(st.font, Some(true), None, None);
                    self.eat_styled(ch, with_font(st, f), out);
                }
                Node::Emph(ch) => {
                    let f = self.vary(st.font, None, Some(true), None);
                    self.eat_styled(ch, with_font(st, f), out);
                }
                Node::Raw { text, block } => {
                    if block {
                        self.diag(Severity::NotImplemented, "raw block inside inline content");
                    }
                out.push(Inline::Text {
                    text,
                    style: TextStyle {
                        font: FontId::Mono,
                        size: st.size,
                    },
                });
                }
                Node::Parbreak => out.push(Inline::LineBreak),
                Node::Heading { .. } | Node::List { .. } | Node::Terms { .. } => self.diag(
                    Severity::NotImplemented,
                    "block-level construct inside inline content",
                ),
                Node::Label(name) => self.pending_labels.push(name),
                Node::Ref(name) => self.push_ref(name, st, out),
                Node::Expr(e) => inline_expr(&e, st, out, self),
            }
        }
    }

    /// Flat items take the existing inline walk. The `List` check is one
    /// match per node; `rest` is allocated only when a nested list exists.
    fn eat_item(&mut self, nodes: Vec<Node>, st: TextStyle) -> ListItem {
        if !nodes.iter().any(|n| matches!(n, Node::List { .. })) {
            let mut content = Vec::new();
            self.eat_styled(nodes, st, &mut content);
            return ListItem {
                content,
                rest: Vec::new(),
            };
        }
        let mut content = Vec::new();
        let mut rest = Vec::new();
        let mut buf = Vec::new();
        let mut seen = false;
        for n in nodes {
            if let Node::List { ordered, items } = n {
                if !seen {
                    self.eat_styled(std::mem::take(&mut buf), st, &mut content);
                    seen = true;
                } else if !buf.is_empty() {
                    let mut inl = Vec::new();
                    self.eat_styled(std::mem::take(&mut buf), st, &mut inl);
                    if !inl.is_empty() {
                        rest.push(ListRest::Inlines(inl));
                    }
                }
                let items = items.into_iter().map(|it| self.eat_item(it, st)).collect();
                rest.push(ListRest::List(Box::new(ListBody { ordered, items })));
            } else {
                buf.push(n);
            }
        }
        if !seen {
            self.eat_styled(buf, st, &mut content);
        } else if !buf.is_empty() {
            let mut inl = Vec::new();
            self.eat_styled(buf, st, &mut inl);
            if !inl.is_empty() {
                rest.push(ListRest::Inlines(inl));
            }
        }
        ListItem { content, rest }
    }

    fn eat_terms(&mut self, items: Vec<TermEntry>, out: &mut Vec<Inline>) {
        self.flush(out);
        let st = self.cur();
        let bold = with_font(st, self.vary(st.font, Some(true), None, None));
        let mut out_items = Vec::with_capacity(items.len());
        for it in items {
            let mut term = Vec::new();
            let mut desc = Vec::new();
            self.eat_styled(it.term, bold, &mut term);
            self.eat_styled(it.desc, st, &mut desc);
            out_items.push(TermItem { term, desc });
        }
        self.push_block(Block::Terms { items: out_items });
    }

    fn item_ref(&mut self, nodes: &[Node], st: TextStyle) -> ListItem {
        if !nodes.iter().any(|n| matches!(n, Node::List { .. })) {
            let mut content = Vec::new();
            inlines(nodes, st, &mut content, self);
            return ListItem {
                content,
                rest: Vec::new(),
            };
        }
        let mut content = Vec::new();
        let mut rest = Vec::new();
        let mut start = 0;
        let mut seen = false;
        for (i, n) in nodes.iter().enumerate() {
            if let Node::List { ordered, items } = n {
                if !seen {
                    inlines(&nodes[start..i], st, &mut content, self);
                    seen = true;
                } else if start < i {
                    let mut inl = Vec::new();
                    inlines(&nodes[start..i], st, &mut inl, self);
                    if !inl.is_empty() {
                        rest.push(ListRest::Inlines(inl));
                    }
                }
                let items = items.iter().map(|it| self.item_ref(it, st)).collect();
                rest.push(ListRest::List(Box::new(ListBody {
                    ordered: *ordered,
                    items,
                })));
                start = i + 1;
            }
        }
        if seen && start < nodes.len() {
            let mut inl = Vec::new();
            inlines(&nodes[start..], st, &mut inl, self);
            if !inl.is_empty() {
                rest.push(ListRest::Inlines(inl));
            }
        }
        ListItem { content, rest }
    }

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
                self.finish_heading(*level, c, st);
            }
            Node::List { ordered, items } => {
                self.flush(out);
                let st = self.cur();
                let items = items.iter().map(|it| self.item_ref(it, st)).collect();
                self.push_block(Block::List {
                    ordered: *ordered,
                    items,
                });
            }
            Node::Terms { items } => {
                self.flush(out);
                let st = self.cur();
                let bold = with_font(st, self.vary(st.font, Some(true), None, None));
                let items = items
                    .iter()
                    .map(|it| {
                        let mut term = Vec::new();
                        let mut desc = Vec::new();
                        inlines(&it.term, bold, &mut term, self);
                        inlines(&it.desc, st, &mut desc, self);
                        TermItem { term, desc }
                    })
                    .collect();
                self.push_block(Block::Terms { items });
            }
            Node::Raw { text, block: true } => {
                self.flush(out);
                self.push_block(Block::RawBlock { text: text.clone() });
            }
            Node::Label(name) => self.on_label(name.clone(), out),
            Node::Ref(name) => {
                let st = self.cur();
                self.push_ref(name.clone(), st, out);
            },
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

    /// Push a scope, run `f`, and pop it. Pop runs when `f` returns,
    /// including after a diagnostic.
    fn with_scope<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        self.scopes.push(HashMap::new());
        let result = f(self);
        self.scopes.pop();
        debug_assert!(!self.scopes.is_empty());
        result
    }

    /// `#let` / `#import` / loop-variable binding in the innermost scope.
    /// A second bind of the same name in that scope replaces the slot.
    #[inline]
    fn bind(&mut self, name: String, value: Value) {
        self.scopes.last_mut().expect("scope").insert(name, value);
    }

    /// Walk scopes from the innermost outward.
    #[inline]
    fn lookup(&self, name: &str) -> Option<&Value> {
        for scope in self.scopes.iter().rev() {
            if let Some(v) = scope.get(name) {
                return Some(v);
            }
        }
        None
    }

    /// Bracket content passed as an argument: its own scope.
    fn scoped_inlines(&mut self, nodes: &[Node], st: TextStyle, out: &mut Vec<Inline>) {
        self.with_scope(|this| inlines(nodes, st, out, this));
    }

    fn scoped_frame(&mut self, nodes: &[Node], st: TextStyle, out: &mut Vec<Inline>) -> bool {
        self.with_scope(|this| this.frame_body(nodes, st, out))
    }

    fn expr(&mut self, e: &Expr, out: &mut Vec<Inline>) {
        match e {
            Expr::Set { target, args } => self.do_set(target, args),
            Expr::Let { name, value } => {
                if let Some(v) = self.rhs_val(value) {
                    self.bind(name.clone(), v);
                }
            }
            Expr::LetFn { name, params, body } => self.def_fn(name, params, body),
            Expr::Call {
                name,
                args,
                content,
            } => self.call_user(name, args, content.as_deref(), None, out),
            Expr::Var { name } => match self.resolve_var(name) {
                Some(v) => {
                    let st = self.cur();
                    value_inline(&v, st, out, self);
                }
                None => self.diag(Severity::Warning, format!("undefined variable '#{name}'")),
            },
            Expr::Import { path, items, all } => self.do_import(path, items, *all),
            Expr::If { arms, els } => self.do_if(arms, els.as_deref(), None, out),
            Expr::While { cond, body } => self.do_while(cond, body, None, out),
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
            Expr::Rhs(r) => self.show_rhs(r, None, out),
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
        if let Some(v) = self.lookup(name) {
            return Some(v.clone());
        }
        let mut parts = name.split('.');
        let mut v = self.lookup(parts.next()?)?.clone();
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
            Rhs::Call { recv, method, args } => Some(self.call_method(recv, method, args)),
        }
    }

    /// Render an rhs. A `.name(` call whose field is a function uses the
    /// same argument rules as `#name(...)`; other calls stay method calls.
    fn show_rhs(&mut self, r: &Rhs, st: Option<TextStyle>, out: &mut Vec<Inline>) {
        if let Rhs::Call { recv, method, args } = r {
            if self.try_field_fn(recv, method, args, st, out) {
                return;
            }
        }
        if let Some(v) = self.rhs_val(r) {
            let style = match st {
                Some(s) => s,
                None => self.cur(),
            };
            value_inline(&v, style, out, self);
        }
    }

    /// Invoke `recv.method(...)` when that field is a `#let` function.
    /// `true` means the call was handled (including a failed receiver).
    fn try_field_fn(
        &mut self,
        recv: &Rhs,
        method: &str,
        args: &[Rhs],
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) -> bool {
        let Some(v) = self.rhs_val(recv) else {
            return true;
        };
        let Value::Dict(ps) = v else {
            return false;
        };
        let Some(Value::Func(func)) = ps.into_iter().find(|(k, _)| k == method).map(|(_, v)| v)
        else {
            return false;
        };
        let args: Vec<RhsArg> = args
            .iter()
            .map(|value| RhsArg {
                name: None,
                value: value.clone(),
            })
            .collect();
        self.invoke(method, func, &args, None, st, out);
        true
    }

    /// `value.method(args)`. Unknown methods and bad indexes are errors
    /// and yield `none` (still a value, so `#let` binds it).
    fn call_method(&mut self, recv: &Rhs, method: &str, args: &[Rhs]) -> Value {
        let Some(v) = self.rhs_val(recv) else {
            return Value::None;
        };
        match method {
            "len" => {
                if !args.is_empty() {
                    self.diag(Severity::Error, "len() takes no arguments");
                    return Value::None;
                }
                match v {
                    Value::Array(a) => Value::Int(a.len() as i64),
                    Value::Dict(d) => Value::Int(d.len() as i64),
                    Value::Str(s) => Value::Int(s.chars().count() as i64),
                    other => {
                        self.diag(
                            Severity::Error,
                            format!("len() is not a method on {}", kind(&other)),
                        );
                        Value::None
                    }
                }
            }
            "at" => {
                if args.len() != 1 {
                    self.diag(Severity::Error, "at() expects 1 argument");
                    return Value::None;
                }
                let Some(idxv) = self.rhs_val(&args[0]) else {
                    return Value::None;
                };
                let Value::Int(i) = idxv else {
                    self.diag(Severity::Error, "at() expects an integer");
                    return Value::None;
                };
                match v {
                    Value::Array(a) => match idx_of(a.len(), i) {
                        Some(u) => a.into_iter().nth(u).unwrap_or(Value::None),
                        None => {
                            self.diag(Severity::Error, "index out of range");
                            Value::None
                        }
                    },
                    Value::Str(s) => match idx_of(s.chars().count(), i) {
                        Some(u) => Value::Str(s.chars().nth(u).unwrap().to_string()),
                        None => {
                            self.diag(Severity::Error, "index out of range");
                            Value::None
                        }
                    },
                    other => {
                        self.diag(
                            Severity::Error,
                            format!("at() is not a method on {}", kind(&other)),
                        );
                        Value::None
                    }
                }
            }
            "first" | "last" => {
                if !args.is_empty() {
                    self.diag(Severity::Error, format!("{method}() takes no arguments"));
                    return Value::None;
                }
                match v {
                    Value::Array(a) => {
                        if a.is_empty() {
                            self.diag(Severity::Error, "index out of range");
                            Value::None
                        } else if method == "first" {
                            a.into_iter().next().unwrap()
                        } else {
                            a.into_iter().next_back().unwrap()
                        }
                    }
                    other => {
                        self.diag(
                            Severity::Error,
                            format!("{method}() is not a method on {}", kind(&other)),
                        );
                        Value::None
                    }
                }
            }
            "contains" => {
                if args.len() != 1 {
                    self.diag(Severity::Error, "contains() expects 1 argument");
                    return Value::None;
                }
                let Some(arg) = self.rhs_val(&args[0]) else {
                    return Value::None;
                };
                match v {
                    Value::Array(a) => Value::Bool(a.iter().any(|e| e == &arg)),
                    Value::Str(s) => match arg {
                        Value::Str(p) => Value::Bool(s.contains(&p)),
                        _ => {
                            self.diag(Severity::Error, "string contains() expects a string");
                            Value::None
                        }
                    },
                    other => {
                        self.diag(
                            Severity::Error,
                            format!("contains() is not a method on {}", kind(&other)),
                        );
                        Value::None
                    }
                }
            }
            _ => {
                self.diag(Severity::Error, format!("unknown method '{method}'"));
                Value::None
            }
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
            (Value::Array(a), Value::Array(b)) => match op {
                ArithOp::Add => {
                    let mut out = Vec::with_capacity(a.len() + b.len());
                    out.extend(a.iter().cloned());
                    out.extend(b.iter().cloned());
                    Some(Value::Array(out))
                }
                _ => {
                    self.diag(Severity::Warning, "arrays only support `+`");
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
        match (&l, &r, c.op) {
            (_, Value::None, CmpOp::Ne) => truthy(&l),
            (Value::None, _, CmpOp::Ne) => truthy(&r),
            (_, Value::None, CmpOp::Eq) => !truthy(&l),
            (Value::None, _, CmpOp::Eq) => !truthy(&r),
            _ => self.compare(c.op, &l, &r),
        }
    }

    /// `#if` / `#while` condition. `and` / `or` short-circuit.
    fn cond_val(&mut self, c: &Cond) -> bool {
        match c {
            Cond::Cmp(c) => self.cmp_val(c),
            Cond::Not(c) => !self.cond_val(c),
            Cond::And(a, b) => self.cond_val(a) && self.cond_val(b),
            Cond::Or(a, b) => self.cond_val(a) || self.cond_val(b),
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
        arms: &[(Cond, Vec<Node>)],
        els: Option<&[Node]>,
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) {
        for (c, body) in arms {
            if self.cond_val(c) {
                self.with_scope(|this| this.run_body(body, st, out));
                return;
            }
        }
        if let Some(b) = els {
            self.with_scope(|this| this.run_body(b, st, out));
        }
    }

    /// `#while`. At most 10_000 body evaluations; a still-true condition
    /// after that is an error and the loop stops.
    fn do_while(
        &mut self,
        cond: &Cond,
        body: &[Node],
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) {
        let mut n = 0usize;
        while self.cond_val(cond) {
            if n >= 10_000 {
                self.diag(Severity::Error, "#while exceeded 10000 iterations");
                return;
            }
            n += 1;
            self.with_scope(|this| this.run_body(body, st, out));
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
        // Loop variables live in the body scope. They do not write the
        // outer binding of the same name.
        for item in items {
            self.with_scope(|this| {
                match item {
                    It::One(v) => {
                        this.bind(vars[0].clone(), v);
                    }
                    It::Two(k, v) => {
                        this.bind(vars[0].clone(), k);
                        this.bind(vars[1].clone(), v);
                    }
                }
                this.run_body(body, st, out);
            });
        }
    }

    /// Resolve a spec, check the recursion guards, load + parse the file.
    /// On success the path is in `env.loaded`; caller removes it after.
    fn enter_file(&mut self, spec: &str) -> Option<(Rc<Vec<Node>>, PathBuf)> {
        let path = match package::resolve(spec, self.dir()) {
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
            // Only the module's root frame is exported. Bindings from
            // content blocks inside the file stay in the frames that
            // were popped around those blocks.
            let mut entries: Vec<(String, Value)> = sub.scopes.swap_remove(0).into_iter().collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Dict(entries)
        };
        self.env.loaded.remove(&canon);
        let Value::Dict(entries) = dict else { return };
        if all {
            for (k, v) in entries {
                self.bind(k, v);
            }
        } else if !items.is_empty() {
            for it in items {
                match entries.iter().find(|(k, _)| k == it) {
                    Some((_, v)) => {
                        self.bind(it.clone(), v.clone());
                    }
                    None => self.diag(
                        Severity::Warning,
                        format!("module '{spec}' has no member '{it}'"),
                    ),
                }
            }
        } else {
            self.bind(module_stem(spec), Value::Dict(entries));
        }
    }

    /// `#include` — parse and evaluate the file's nodes inline (blocks
    /// are spliced into the output at this point).
    fn do_include(&mut self, spec: &str, out: &mut Vec<Inline>, st: Option<TextStyle>) {
        let Some((nodes, canon)) = self.enter_file(spec) else {
            return;
        };
        let parent = canon
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| self.dir().to_path_buf());
        self.include_dirs.push(parent);
        self.env.depth += 1;
        self.run_body(&nodes, st, out);
        self.env.depth -= 1;
        self.include_dirs.pop();
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
                self.push_block(Block::Paragraph {
                    content: vec![Inline::Text { text: s, style: st }],
                    align: HAlign::Left,
                });
            }
            "pagebreak" => {
                self.flush(out);
                self.push_block(Block::PageBreak);
            }
            "v" => {
                self.flush(out);
                match arg_len(args, self.st.font_size) {
                    Some(p) => self.push_block(Block::VSpace(p)),
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
                    self.scoped_inlines(c, with_font(st, f), out);
                }
            }
            "emph" => {
                if let Some(c) = content {
                    let st = self.cur();
                    let f = self.vary(st.font, None, Some(true), None);
                    self.scoped_inlines(c, with_font(st, f), out);
                }
            }
            "text" => {
                let st = self.cur();
                let st = text_style(args, st, self);
                if let Some(c) = content {
                    self.scoped_inlines(c, st, out);
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
                    self.scoped_inlines(c, st, &mut v);
                    self.push_block(Block::Paragraph {
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
            "image" => self.image(args, out),
            "box" | "block" => self.do_frame(name, args, content, out),
            "stack" => self.do_stack(args, content, out),
            "figure" => self.do_figure(args, content, out),
            "outline" => self.do_outline(args, content, out),
            "link" => {
                let st = self.cur();
                self.do_link(args, content, st, out);
            }
            _ => self.diag(
                Severity::NotImplemented,
                format!("function '#{name}' is not implemented"),
            ),
        }
    }

    /// `#link(url)[text]` / `#link(url, [text])`. No url is an error and
    /// the content is emitted as plain inline text.
    fn do_link(
        &mut self,
        args: &[Arg],
        content: Option<&[Node]>,
        st: TextStyle,
        out: &mut Vec<Inline>,
    ) {
        let mut url: Option<String> = None;
        let mut body = Vec::new();
        for a in args {
            match a.name.as_deref() {
                None => match &a.value {
                    Value::Str(s) if url.is_none() => url = Some(s.clone()),
                    Value::Content(ns) => self.scoped_inlines(ns, st, &mut body),
                    Value::Str(_) => self.diag(Severity::Warning, "extra link url ignored"),
                    _ => self.diag(Severity::Warning, "link argument ignored"),
                },
                Some("dest") | Some("url") => match &a.value {
                    Value::Str(s) => url = Some(s.clone()),
                    _ => self.diag(Severity::Warning, "link url expects a string"),
                },
                Some(other) => {
                    self.diag(Severity::Warning, format!("link argument '{other}' ignored"))
                }
            }
        }
        if let Some(c) = content {
            self.scoped_inlines(c, st, &mut body);
        }
        match url {
            Some(u) if !u.is_empty() => {
                out.push(Inline::Link(Box::new(LinkRun { url: u, content: body })))
            }
            _ => {
                self.diag(Severity::Error, "#link expects a url");
                out.append(&mut body);
            }
        }
    }

    /// `#box` stays inline unless the content has a block break.
    /// `#block` always breaks the surrounding paragraph.
    fn do_frame(
        &mut self,
        name: &str,
        args: &[Arg],
        content: Option<&[Node]>,
        out: &mut Vec<Inline>,
    ) {
        let st = self.cur();
        let (spec, inl, blocky) = self.read_frame(name, args, content, st);
        let frame = Frame {
            width: spec.width,
            expand: name == "block" && spec.width.is_none(),
            inset: spec.inset,
            fill: spec.fill,
            stroke: spec.stroke,
            content: inl,
        };
        if name == "block" || blocky {
            self.flush(out);
            self.push_block(Block::Framed(Box::new(frame)));
        } else {
            out.push(Inline::Frame(Box::new(frame)));
        }
    }

    fn do_stack(&mut self, args: &[Arg], content: Option<&[Node]>, out: &mut Vec<Inline>) {
        self.flush(out);
        let em = self.st.font_size;
        let mut dir = String::from("ttb");
        let mut spacing = 0.0;
        let mut groups: Vec<&[Node]> = Vec::new();
        for a in args {
            match a.name.as_deref() {
                Some("dir") => match &a.value {
                    Value::Str(s) => dir = s.to_ascii_lowercase(),
                    _ => self.diag(Severity::Warning, "stack dir expects ttb or ltr"),
                },
                Some("spacing") => match val_pt(&a.value, em) {
                    Some(p) => spacing = p.max(0.0),
                    None => self.diag(Severity::Warning, "stack spacing expects a length"),
                },
                Some(other) => {
                    self.diag(Severity::Warning, format!("stack argument '{other}' ignored"))
                }
                None => match &a.value {
                    Value::Content(ns) => groups.push(ns),
                    _ => self.diag(Severity::Warning, "stack positional argument ignored"),
                },
            }
        }
        if let Some(c) = content {
            groups.push(c);
        }
        let horizontal = match dir.as_str() {
            "ltr" => true,
            "ttb" => false,
            other => {
                self.diag(
                    Severity::Warning,
                    format!("stack dir '{other}' is not implemented; using ttb"),
                );
                false
            }
        };
        let mut children = Vec::new();
        for g in groups {
            children.extend(self.capture_blocks(g));
        }
        self.push_block(Block::Stack {
            horizontal,
            spacing,
            children,
        });
    }

    /// `#figure(caption: [..])[body]` and `#figure([body], caption: [..])`.
    /// The counter advances only here. Body blocks are captured with the
    /// normal walk so a heading inside the figure stays a heading.
    fn do_figure(&mut self, args: &[Arg], content: Option<&[Node]>, out: &mut Vec<Inline>) {
        self.flush(out);
        let mut caption: Option<&[Node]> = None;
        let mut bodies: Vec<&[Node]> = Vec::new();
        for a in args {
            match a.name.as_deref() {
                Some("caption") => match &a.value {
                    Value::Content(ns) => caption = Some(ns),
                    Value::None => {}
                    _ => self.diag(Severity::Warning, "figure caption expects content"),
                },
                Some(other) => {
                    self.diag(Severity::Warning, format!("figure argument '{other}' ignored"))
                }
                None => match &a.value {
                    Value::Content(ns) => bodies.push(ns),
                    _ => self.diag(Severity::Warning, "figure positional argument ignored"),
                },
            }
        }
        if let Some(c) = content {
            bodies.push(c);
        }
        let mut body = Vec::new();
        for g in bodies {
            body.extend(self.capture_blocks(g));
        }
        self.figures = self.figures.saturating_add(1);
        let number = self.figures;
        let mut cap_inl = Vec::new();
        if let Some(ns) = caption {
            let st = self.cur();
            let cap = TextStyle {
                font: st.font,
                size: self.st.font_size * 0.9,
            };
            cap_inl.push(Inline::Text {
                text: format!("Figure {number}: "),
                style: cap,
            });
            self.scoped_inlines(ns, cap, &mut cap_inl);
        }
        self.push_block(Block::Figure(Box::new(Fig {
            body,
            caption: cap_inl,
            number,
        })));
    }

    /// `#outline()`. Entries are filled after the document is evaluated
    /// so headings that follow the call are included.
    fn do_outline(&mut self, args: &[Arg], content: Option<&[Node]>, out: &mut Vec<Inline>) {
        self.flush(out);
        if !args.is_empty() || content.is_some() {
            self.diag(Severity::Warning, "outline arguments are ignored");
        }
        self.saw_outline = true;
        let style = self.cur();
        self.push_block(Block::Outline { style });
    }

    /// Evaluate `nodes` into blocks without mixing them into the parent list.
    /// Each stack child is bracket content, so it gets its own scope.
    fn capture_blocks(&mut self, nodes: &[Node]) -> Vec<Block> {
        self.with_scope(|this| {
            let saved = std::mem::take(&mut this.blocks);
            let saved_num = this.heading_number.take();
            let mut inline = Vec::new();
            for n in nodes {
                this.node(n, &mut inline);
            }
            this.flush(&mut inline);
            let produced = std::mem::take(&mut this.blocks);
            this.blocks = saved;
            this.heading_number = saved_num;
            produced
        })
    }

    fn read_frame(
        &mut self,
        name: &str,
        args: &[Arg],
        content: Option<&[Node]>,
        st: TextStyle,
    ) -> (FrameSpec, Vec<Inline>, bool) {
        let em = st.size;
        let mut spec = FrameSpec {
            width: None,
            inset: 0.0,
            fill: None,
            stroke: None,
        };
        let mut inl = Vec::new();
        let mut blocky = false;
        for a in args {
            match a.name.as_deref() {
                Some("width") => match val_pt(&a.value, em) {
                    Some(p) => spec.width = Some(p.max(0.0)),
                    None => self.diag(Severity::Warning, format!("{name} width expects a length")),
                },
                Some("inset") => match val_pt(&a.value, em) {
                    Some(p) => spec.inset = p.max(0.0),
                    None => self.diag(Severity::Warning, format!("{name} inset expects a length")),
                },
                Some("fill") => spec.fill = self.luma(name, &a.value),
                Some("stroke") => match &a.value {
                    Value::None => spec.stroke = None,
                    _ => match val_pt(&a.value, em) {
                        Some(p) if p > 0.0 => spec.stroke = Some(p),
                        Some(_) => spec.stroke = None,
                        None => {
                            self.diag(Severity::Warning, format!("{name} stroke expects a length"))
                        }
                    },
                },
                Some("outset") => {
                    if !matches!(a.value, Value::None) {
                        self.diag(
                            Severity::NotImplemented,
                            format!("{name} outset is not implemented"),
                        );
                    }
                }
                Some(other) => {
                    self.diag(Severity::Warning, format!("{name} argument '{other}' ignored"))
                }
                None => match &a.value {
                    Value::Content(ns) => {
                        if self.scoped_frame(ns, st, &mut inl) {
                            blocky = true;
                        }
                    }
                    _ => self.diag(Severity::Warning, format!("{name} positional argument ignored")),
                },
            }
        }
        if let Some(c) = content {
            if self.scoped_frame(c, st, &mut inl) {
                blocky = true;
            }
        }
        (spec, inl, blocky)
    }

    /// Luma for `fill`: float 0..=1, or int 0..=255. `None` means no fill.
    fn luma(&mut self, name: &str, v: &Value) -> Option<f64> {
        match v {
            Value::None => None,
            Value::Float(f) if f.is_finite() && (0.0..=1.0).contains(f) => Some(*f),
            Value::Int(i) if (0..=255).contains(i) => Some(*i as f64 / 255.0),
            _ => {
                self.diag(
                    Severity::Warning,
                    format!("{name} fill expects luma 0.0-1.0 or an integer 0-255"),
                );
                None
            }
        }
    }

    /// Lower frame content to inlines. Returns true when a block break
    /// (blank line, heading, list, table, …) was seen.
    fn frame_body(&mut self, nodes: &[Node], st: TextStyle, out: &mut Vec<Inline>) -> bool {
        let mut blocky = false;
        for n in nodes {
            if self.frame_node(n, st, out) {
                blocky = true;
            }
        }
        blocky
    }

    fn frame_node(&mut self, n: &Node, st: TextStyle, out: &mut Vec<Inline>) -> bool {
        match n {
            Node::Text(s) => {
                out.push(Inline::Text {
                    text: s.clone(),
                    style: st,
                });
                false
            }
            Node::LineBreak => {
                out.push(Inline::LineBreak);
                false
            }
            Node::Strong(ch) => {
                let f = self.vary(st.font, Some(true), None, None);
                self.frame_body(ch, with_font(st, f), out)
            }
            Node::Emph(ch) => {
                let f = self.vary(st.font, None, Some(true), None);
                self.frame_body(ch, with_font(st, f), out)
            }
            Node::Raw { text, block } => {
                if *block {
                    self.diag(
                        Severity::NotImplemented,
                        "raw block inside a frame is not implemented",
                    );
                }
                out.push(Inline::Text {
                    text: text.clone(),
                    style: TextStyle {
                        font: FontId::Mono,
                        size: st.size,
                    },
                });
                *block
            }
            Node::Parbreak => {
                out.push(Inline::LineBreak);
                true
            }
            Node::Heading { body, .. } => {
                self.diag(
                    Severity::NotImplemented,
                    "heading inside a frame is not implemented",
                );
                let _ = self.frame_body(body, st, out);
                true
            }
            Node::List { items, .. } => {
                self.diag(
                    Severity::NotImplemented,
                    "list inside a frame is not implemented",
                );
                for it in items {
                    let _ = self.frame_body(it, st, out);
                    out.push(Inline::LineBreak);
                }
                true
            }
            Node::Terms { items } => {
                self.diag(
                    Severity::NotImplemented,
                    "term list inside a frame is not implemented",
                );
                for it in items {
                    let _ = self.frame_body(&it.term, st, out);
                    let _ = self.frame_body(&it.desc, st, out);
                    out.push(Inline::LineBreak);
                }
                true
            }
            Node::Label(name) => {
                self.pending_labels.push(name.clone());
                false
            }
            Node::Ref(name) => {
                self.push_ref(name.clone(), st, out);
                false
            }
            Node::Expr(e) => self.frame_expr(e, st, out),
        }
    }

    fn frame_expr(&mut self, e: &Expr, st: TextStyle, out: &mut Vec<Inline>) -> bool {
        if let Expr::Func { name, args, content } = e {
            match name.as_str() {
                "box" | "block" => {
                    let (spec, inl, blocky) = self.read_frame(name, args, content.as_deref(), st);
                    out.push(Inline::Frame(Box::new(Frame {
                        width: spec.width,
                        expand: false,
                        inset: spec.inset,
                        fill: spec.fill,
                        stroke: spec.stroke,
                        content: inl,
                    })));
                    return name == "block" || blocky;
                }
                "table" | "grid" => {
                    self.diag(
                        Severity::NotImplemented,
                        "table inside a frame is not implemented",
                    );
                    for a in args {
                        if a.name.is_none() {
                            if let Value::Content(ns) = &a.value {
                                let _ = self.scoped_frame(ns, st, out);
                                out.push(Inline::LineBreak);
                            }
                        }
                    }
                    return true;
                }
                "stack" | "align" | "v" | "pagebreak" | "lorem" | "image" => {
                    self.diag(
                        Severity::NotImplemented,
                        format!("#{name} inside a frame is not implemented"),
                    );
                    if let Some(c) = content {
                        let _ = self.scoped_frame(c, st, out);
                    }
                    for a in args {
                        if a.name.is_none() {
                            if let Value::Content(ns) = &a.value {
                                let _ = self.scoped_frame(ns, st, out);
                            }
                        }
                    }
                    return true;
                }
                _ => {}
            }
        }
        inline_expr(e, st, out, self);
        false
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
        self.push_block(Block::Table {
            columns,
            cells,
            stroke,
        });
    }

    /// `#image("path.jpg", width: .., height: ..)`. JPEG bytes are copied
    /// through; only the SOF marker is read for the pixel size.
    fn image(&mut self, args: &[Arg], out: &mut Vec<Inline>) {
        self.flush(out);
        let mut path: Option<String> = None;
        let mut width: Option<f64> = None;
        let mut height: Option<f64> = None;
        let em = self.st.font_size;
        for a in args {
            match a.name.as_deref() {
                Some("width") => match val_pt(&a.value, em) {
                    Some(p) => width = Some(p),
                    None => {
                        self.diag(Severity::Error, "#image width expects a length");
                        return;
                    }
                },
                Some("height") => match val_pt(&a.value, em) {
                    Some(p) => height = Some(p),
                    None => {
                        self.diag(Severity::Error, "#image height expects a length");
                        return;
                    }
                },
                Some(other) => self.diag(
                    Severity::Warning,
                    format!("image argument '{other}' ignored"),
                ),
                None => match &a.value {
                    Value::Str(s) if path.is_none() => path = Some(s.clone()),
                    Value::Str(_) => {
                        self.diag(Severity::Warning, "extra image path ignored");
                    }
                    _ => {
                        self.diag(Severity::Error, "#image expects a path string");
                        return;
                    }
                },
            }
        }
        let Some(path) = path else {
            self.diag(Severity::Error, "#image expects a path string");
            return;
        };
        if !jpeg_ext(&path) {
            self.diag(Severity::Error, "only jpeg images are supported");
            return;
        }
        let full = self.dir().join(&path);
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(e) => {
                self.diag(
                    Severity::Error,
                    format!("cannot read image '{path}': {e}"),
                );
                return;
            }
        };
        let info = match jpeg_info(&bytes) {
            Ok(i) => i,
            Err(msg) => {
                self.diag(Severity::Error, format!("image '{path}': {msg}"));
                return;
            }
        };
        let (w, h) = image_box(info.width, info.height, width, height);
        let index = self.images.len();
        self.images.push(Image {
            bytes,
            width: info.width,
            height: info.height,
            bits: info.bits,
            components: info.components,
        });
        self.push_block(Block::Image { index, w, h });
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
            "heading" => self.set_heading(args),
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
                Some("width") => match val_pt(&a.value, self.st.font_size) {
                    Some(p) => self.st.page_width = p,
                    None => self.diag(Severity::Warning, "page width expects a length"),
                },
                Some("height") => match val_pt(&a.value, self.st.font_size) {
                    Some(p) => self.st.page_height = p,
                    None => self.diag(Severity::Warning, "page height expects a length"),
                },
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

    fn def_fn(&mut self, name: &str, params: &[Param], body: &Rhs) {
        self.bind(
            name.to_string(),
            Value::Func(Box::new(Func {
                params: params.to_vec(),
                body: body.clone(),
            })),
        );
    }

    /// Bind arguments and evaluate a user function. `st: Some` means the
    /// call is inside a paragraph; `None` is block position.
    fn call_user(
        &mut self,
        name: &str,
        args: &[RhsArg],
        content: Option<&[Node]>,
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) {
        let Some(Value::Func(func)) = self.lookup(name).cloned() else {
            self.diag(
                Severity::NotImplemented,
                format!("function '#{name}' is not implemented"),
            );
            return;
        };
        self.invoke(name, func, args, content, st, out);
    }

    fn invoke(
        &mut self,
        name: &str,
        func: Box<Func>,
        args: &[RhsArg],
        content: Option<&[Node]>,
        st: Option<TextStyle>,
        out: &mut Vec<Inline>,
    ) {
        if self.fn_depth >= 64 {
            self.diag(
                Severity::Error,
                format!("function '#{name}' recursion limit exceeded"),
            );
            return;
        }
        let mut positional = Vec::new();
        let mut named = Vec::new();
        let mut bad = false;
        for a in args {
            let Some(v) = self.rhs_val(&a.value) else {
                bad = true;
                continue;
            };
            if let Some(n) = &a.name {
                named.push((n.clone(), v));
            } else {
                positional.push(v);
            }
        }
        let mut slots = vec![None; func.params.len()];
        if let Some(c) = content {
            let cv = Value::Content(c.to_vec());
            if let Some(i) = func.params.iter().position(|p| p.name == "body") {
                slots[i] = Some(cv);
            } else {
                positional.push(cv);
            }
        }
        let mut pi = 0;
        for slot in slots.iter_mut() {
            if slot.is_none() && pi < positional.len() {
                *slot = Some(positional[pi].clone());
                pi += 1;
            }
        }
        if pi < positional.len() {
            self.diag(
                Severity::Error,
                format!("function '#{name}' given too many arguments"),
            );
        }
        for (k, v) in named {
            match func.params.iter().position(|p| p.name == k) {
                Some(i) if slots[i].is_some() => {
                    self.diag(
                        Severity::Error,
                        format!("function '#{name}' argument '{k}' specified twice"),
                    );
                    bad = true;
                }
                Some(i) => slots[i] = Some(v),
                None => {
                    self.diag(
                        Severity::Error,
                        format!("function '#{name}' unknown argument '{k}'"),
                    );
                    bad = true;
                }
            }
        }
        let mut bound = Vec::new();
        for (i, p) in func.params.iter().enumerate() {
            if let Some(v) = slots[i].clone() {
                bound.push((p.name.clone(), v));
            } else if let Some(d) = &p.default {
                bound.push((p.name.clone(), d.clone()));
            } else {
                self.diag(
                    Severity::Error,
                    format!("function '#{name}' missing argument '{}'", p.name),
                );
                bad = true;
            }
        }
        if bad {
            return;
        }
        self.fn_depth += 1;
        // Parameters live in this frame only. The manual save/restore
        // is gone so a body `#let` cannot write through it into the caller.
        self.with_scope(|this| {
            for (k, v) in bound {
                this.bind(k, v);
            }
            this.exec_user_body(&func.body, st, out);
        });
        self.fn_depth -= 1;
    }

    fn exec_user_body(&mut self, body: &Rhs, st: Option<TextStyle>, out: &mut Vec<Inline>) {
        match body {
            Rhs::Lit(Value::Content(nodes)) => self.place_nodes(nodes, st, out),
            other => {
                if let Some(v) = self.rhs_val(other) {
                    self.render_value(&v, st, out);
                }
            }
        }
    }

    fn render_value(&mut self, v: &Value, st: Option<TextStyle>, out: &mut Vec<Inline>) {
        if let Value::Content(nodes) = v {
            self.place_nodes(nodes, st, out);
        } else {
            let style = match st {
                Some(s) => s,
                None => self.cur(),
            };
            value_inline(v, style, out, self);
        }
    }

    /// Splice block-level body nodes when the call is at block position
    /// (the pending paragraph is only newline padding). Otherwise flatten
    /// paragraph inlines into the surrounding paragraph.
    fn place_nodes(&mut self, nodes: &[Node], st: Option<TextStyle>, out: &mut Vec<Inline>) {
        // Function body (content) is a nested scope inside the parameter frame.
        self.with_scope(|this| {
            if let Some(st) = st {
                inlines(nodes, st, out, this);
                return;
            }
            if pad_only(out) {
                out.clear();
                this.run_body(nodes, None, out);
            } else {
                let st = this.cur();
                inlines(nodes, st, out, this);
            }
        });
    }
    fn set_heading(&mut self, args: &[Arg]) {
        for a in args {
            match a.name.as_deref() {
                Some("numbering") | None => self.apply_numbering(&a.value),
                Some(other) => {
                    self.diag(Severity::Warning, format!("heading argument '{other}' ignored"))
                }
            }
        }
    }

    fn apply_numbering(&mut self, v: &Value) {
        match v {
            Value::None => self.numbering = None,
            Value::Str(s) => match parse_numpat(s) {
                Some(p) => self.numbering = Some(p),
                None => self.diag(
                    Severity::Warning,
                    format!("unsupported heading numbering '{s}'"),
                ),
            },
            _ => self.diag(Severity::Warning, "heading numbering expects a string or none"),
        }
    }

}

fn pad_only(out: &[Inline]) -> bool {
    out.iter().all(|i| match i {
        Inline::Text { text, .. } => text.chars().all(|c| c == ' ' || c == '\t'),
        _ => false,
    })
}

// ------------------------- free helpers -------------------------

/// Heading numbering pattern. Depth is the heading's own level; the
/// pattern only contributes the optional trailing dot.
#[derive(Clone, Copy)]
struct NumPat {
    trailing_dot: bool,
}

/// `"1"`, `"1."`, `"1.1"`, `"1.1.1"`, ...
fn parse_numpat(s: &str) -> Option<NumPat> {
    let b = s.as_bytes();
    if b.first() != Some(&b'1') {
        return None;
    }
    let mut i = 0;
    loop {
        if i >= b.len() || b[i] != b'1' {
            return None;
        }
        i += 1;
        if i == b.len() {
            return Some(NumPat { trailing_dot: false });
        }
        if b[i] != b'.' {
            return None;
        }
        i += 1;
        if i == b.len() {
            return Some(NumPat { trailing_dot: true });
        }
    }
}

/// Counter chain from `heading_prefix`, without the separating space
/// and without a numbering pattern's decorative trailing dot.
/// `"1.1 "` → `"1.1"`, `"1. "` → `"1"`.
fn section_number(prefix: &str, trailing_dot: bool) -> String {
    let n = prefix.strip_suffix(' ').unwrap_or(prefix);
    if trailing_dot {
        n.strip_suffix('.').unwrap_or(n).to_string()
    } else {
        n.to_string()
    }
}

fn plain_inlines(v: &[Inline], s: &mut String) {
    for i in v {
        match i {
            Inline::Text { text, .. } => s.push_str(text),
            Inline::Frame(f) => plain_inlines(&f.content, s),
            Inline::Link(link) => plain_inlines(&link.content, s),
            _ => {}
        }
    }
}

fn plain_item(it: &ListItem, s: &mut String) {
    plain_inlines(&it.content, s);
    for r in &it.rest {
        match r {
            ListRest::Inlines(v) => plain_inlines(v, s),
            ListRest::List(b) => {
                for it in &b.items {
                    if !s.is_empty() && !s.ends_with(' ') {
                        s.push(' ');
                    }
                    plain_item(it, s);
                }
            }
        }
    }
}

fn plain_block(b: &Block) -> String {
    let mut s = String::new();
    match b {
        Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
            plain_inlines(content, &mut s);
        }
        Block::List { items, .. } => {
            for (i, it) in items.iter().enumerate() {
                if i > 0 && !s.is_empty() && !s.ends_with(' ') {
                    s.push(' ');
                }
                plain_item(it, &mut s);
            }
        }
        Block::Terms { items } => {
            for it in items {
                if !s.is_empty() && !s.ends_with(' ') {
                    s.push(' ');
                }
                plain_inlines(&it.term, &mut s);
                if !s.is_empty() && !s.ends_with(' ') {
                    s.push(' ');
                }
                plain_inlines(&it.desc, &mut s);
            }
        }
        Block::Table { cells, .. } => {
            for c in cells {
                plain_inlines(c, &mut s);
            }
        }
        Block::RawBlock { text } => s.push_str(text),
        Block::Framed(f) => plain_inlines(&f.content, &mut s),
        Block::Figure(f) => return format!("Figure {}", f.number),
        Block::Stack { children, .. } => {
            for c in children {
                if !s.is_empty() && !s.ends_with(' ') {
                    s.push(' ');
                }
                s.push_str(&plain_block(c));
            }
        }
        _ => {}
    }
    s.trim().to_string()
}

fn resolve_inlines(
    v: &mut [Inline],
    labels: Option<&HashMap<String, LabelTarget>>,
    diags: &mut Vec<Diag>,
) {
    for i in v.iter_mut() {
        if let Inline::Frame(f) = i {
            resolve_inlines(&mut f.content, labels, diags);
            continue;
        }
        if let Inline::Link(link) = i {
            resolve_inlines(&mut link.content, labels, diags);
            continue;
        }
        if !matches!(i, Inline::Ref { .. }) {
            continue;
        }
        let Inline::Ref { name, style } = std::mem::replace(i, Inline::LineBreak) else {
            continue;
        };
        let text = match labels.and_then(|m| m.get(&name)) {
            Some(LabelTarget::Text(t)) => t.clone(),
            Some(LabelTarget::Section(n)) => format!("Section {n}"),
            Some(LabelTarget::Unnumbered) => {
                diags.push(Diag::new(
                    Severity::Error,
                    None,
                    "cannot reference heading without numbering",
                ));
                String::new()
            }
            None => {
                diags.push(Diag::new(
                    Severity::Warning,
                    None,
                    format!("unknown label `@{name}`"),
                ));
                format!("@{name}")
            }
        };
        *i = Inline::Text { text, style };
    }
}

fn resolve_item(
    it: &mut ListItem,
    labels: Option<&HashMap<String, LabelTarget>>,
    diags: &mut Vec<Diag>,
) {
    resolve_inlines(&mut it.content, labels, diags);
    for r in &mut it.rest {
        match r {
            ListRest::Inlines(v) => resolve_inlines(v, labels, diags),
            ListRest::List(b) => {
                for it in &mut b.items {
                    resolve_item(it, labels, diags);
                }
            }
        }
    }
}

fn resolve_refs(
    blocks: &mut [Block],
    labels: Option<&HashMap<String, LabelTarget>>,
    diags: &mut Vec<Diag>,
) {
    for b in blocks {
        match b {
            Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                resolve_inlines(content, labels, diags);
            }
            Block::List { items, .. } => {
                for it in items {
                    resolve_item(it, labels, diags);
                }
            }
            Block::Terms { items } => {
                for it in items {
                    resolve_inlines(&mut it.term, labels, diags);
                    resolve_inlines(&mut it.desc, labels, diags);
                }
            }
            Block::Table { cells, .. } => {
                for c in cells {
                    resolve_inlines(c, labels, diags);
                }
            }
            Block::Framed(f) => resolve_inlines(&mut f.content, labels, diags),
            Block::Stack { children, .. } => resolve_refs(children, labels, diags),
            Block::Figure(f) => {
                resolve_refs(&mut f.body, labels, diags);
                resolve_inlines(&mut f.caption, labels, diags);
            }
            _ => {}
        }
    }
}

/// Second walk for `#outline()`. Collects every heading (including ones
/// inside figures and stacks), then replaces each placeholder. Not called
/// unless the document evaluated an outline.
fn fill_outlines(blocks: &mut Vec<Block>) {
    let mut heads = Vec::new();
    collect_headings(blocks, &mut heads);
    expand_outlines(blocks, &heads);
}

fn collect_headings(blocks: &[Block], out: &mut Vec<(u8, String)>) {
    for b in blocks {
        match b {
            Block::Heading { level, content } => {
                let mut s = String::new();
                plain_inlines(content, &mut s);
                out.push((*level, s.trim().to_string()));
            }
            Block::Figure(f) => collect_headings(&f.body, out),
            Block::Stack { children, .. } => collect_headings(children, out),
            _ => {}
        }
    }
}

fn expand_outlines(blocks: &mut Vec<Block>, heads: &[(u8, String)]) {
    let mut i = 0;
    while i < blocks.len() {
        let style = match &blocks[i] {
            Block::Outline { style } => Some(*style),
            Block::Figure(_) | Block::Stack { .. } => None,
            _ => {
                i += 1;
                continue;
            }
        };
        if let Some(style) = style {
            let paras = outline_paras(heads, style);
            let n = paras.len();
            blocks.splice(i..i + 1, paras);
            i += n;
            continue;
        }
        match &mut blocks[i] {
            Block::Figure(f) => expand_outlines(&mut f.body, heads),
            Block::Stack { children, .. } => expand_outlines(children, heads),
            _ => {}
        }
        i += 1;
    }
}

fn outline_paras(heads: &[(u8, String)], style: TextStyle) -> Vec<Block> {
    heads
        .iter()
        .map(|(level, text)| {
            let mut content = Vec::with_capacity(2);
            let indent = 12.0 * (*level as f64 - 1.0);
            if indent > 0.0 {
                content.push(Inline::HSpace(indent));
            }
            content.push(Inline::Text {
                text: text.clone(),
                style,
            });
            Block::Paragraph {
                content,
                align: HAlign::Left,
            }
        })
        .collect()
}


struct FrameSpec {
    width: Option<f64>,
    inset: f64,
    fill: Option<f64>,
    stroke: Option<f64>,
}

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

/// `.jpg` / `.jpeg`, any ASCII case. Checked before the file is opened.
fn jpeg_ext(path: &str) -> bool {
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some(e) => e.eq_ignore_ascii_case("jpg") || e.eq_ignore_ascii_case("jpeg"),
        None => false,
    }
}

/// Pixel size of a JPEG, read from the first SOF0 (`0xC0`) or SOF2
/// (`0xC2`) marker. Segments are skipped by their length; scan data is
/// not decoded. A header that runs past the buffer is an error.
#[derive(Debug)]
pub(crate) struct JpegInfo {
    pub width: u32,
    pub height: u32,
    pub bits: u8,
    pub components: u8,
}

pub(crate) fn jpeg_info(data: &[u8]) -> Result<JpegInfo, &'static str> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return Err("truncated jpeg image");
    }
    let mut i = 2usize;
    while i + 1 < data.len() {
        if data[i] != 0xFF {
            return Err("truncated jpeg image");
        }
        i += 1;
        while i < data.len() && data[i] == 0xFF {
            i += 1;
        }
        if i >= data.len() {
            return Err("truncated jpeg image");
        }
        let marker = data[i];
        i += 1;
        if marker == 0xD9 {
            return Err("jpeg image has no SOF marker");
        }
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        if i + 2 > data.len() {
            return Err("truncated jpeg image");
        }
        let len = u16::from_be_bytes([data[i], data[i + 1]]) as usize;
        if len < 2 || i + len > data.len() {
            return Err("truncated jpeg image");
        }
        if marker == 0xC0 || marker == 0xC2 {
            if len < 8 {
                return Err("truncated jpeg image");
            }
            let bits = data[i + 2];
            let height = u16::from_be_bytes([data[i + 3], data[i + 4]]) as u32;
            let width = u16::from_be_bytes([data[i + 5], data[i + 6]]) as u32;
            let components = data[i + 7];
            if bits == 0 || width == 0 || height == 0 || components == 0 {
                return Err("jpeg image has no dimensions");
            }
            return Ok(JpegInfo {
                width,
                height,
                bits,
                components,
            });
        }
        if marker == 0xDA {
            return Err("jpeg image has no SOF marker");
        }
        i += len;
    }
    Err("truncated jpeg image")
}

/// Layout size in points.
///
/// Both `width` and `height` set: use both. One set: preserve the
/// `px_w`:`px_h` ratio. Neither: 1 px = 1 pt.
pub(crate) fn image_box(
    px_w: u32,
    px_h: u32,
    width: Option<f64>,
    height: Option<f64>,
) -> (f64, f64) {
    let (iw, ih) = (px_w as f64, px_h as f64);
    match (width, height) {
        (Some(w), Some(h)) => (w, h),
        (Some(w), None) => (w, if iw > 0.0 { w * ih / iw } else { 0.0 }),
        (None, Some(h)) => (if ih > 0.0 { h * iw / ih } else { 0.0 }, h),
        (None, None) => (iw, ih),
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

/// Index into a sequence of `len` items. Negative counts from the end.
fn idx_of(len: usize, i: i64) -> Option<usize> {
    if i >= 0 {
        let u = usize::try_from(i).ok()?;
        if u < len { Some(u) } else { None }
    } else {
        let mag = usize::try_from(i.checked_neg()?).ok()?;
        if mag > 0 && mag <= len {
            Some(len - mag)
        } else {
            None
        }
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
        Value::Func(_) => "function",
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
            Node::Heading { .. } | Node::List { .. } | Node::Terms { .. } => cx.diag(
                Severity::NotImplemented,
                "block-level construct inside inline content",
            ),
            Node::Label(name) => cx.pending_labels.push(name.clone()),
            Node::Ref(name) => cx.push_ref(name.clone(), st, out),
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
                cx.bind(name.clone(), v);
            }
        }
        Expr::LetFn { name, params, body } => cx.def_fn(name, params, body),
        Expr::Call {
            name,
            args,
            content,
        } => cx.call_user(name, args, content.as_deref(), Some(st), out),
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
        Expr::While { cond, body } => cx.do_while(cond, body, Some(st), out),
        Expr::For { vars, iter, body } => cx.do_for(vars, iter, body, Some(st), out),
        Expr::Rhs(r) => cx.show_rhs(r, Some(st), out),
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
                    cx.scoped_inlines(c, with_font(st, f), out);
                }
            }
            "emph" => {
                if let Some(c) = content {
                    let f = cx.vary(st.font, None, Some(true), None);
                    cx.scoped_inlines(c, with_font(st, f), out);
                }
            }
            "text" => {
                let st2 = text_style(args, st, cx);
                if let Some(c) = content {
                    cx.scoped_inlines(c, st2, out);
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
            "link" => cx.do_link(args, content.as_deref(), st, out),
            "box" => {
                let (spec, inl, _) = cx.read_frame(name, args, content.as_deref(), st);
                out.push(Inline::Frame(Box::new(Frame {
                    width: spec.width,
                    expand: false,
                    inset: spec.inset,
                    fill: spec.fill,
                    stroke: spec.stroke,
                    content: inl,
                })));
            }
            "lorem" | "pagebreak" | "v" | "align" | "table" | "grid" | "image" | "block"
            | "stack" | "figure" | "outline" => cx.diag(
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
        Value::Content(ns) => cx.scoped_inlines(ns, st, out),
        Value::Bool(_) | Value::None | Value::Func(_) => {}
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
        Value::Func(_) => "function".into(),
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
                match i {
                    Inline::Text { text, .. } => s.push_str(text),
                    Inline::Frame(f) => walk(&f.content, s),
                    Inline::Link(link) => walk(&link.content, s),
                    _ => {}
                }
            }
        }
        fn walk_item(it: &ListItem, s: &mut String) {
            walk(&it.content, s);
            for r in &it.rest {
                match r {
                    ListRest::Inlines(v) => walk(v, s),
                    ListRest::List(b) => b.items.iter().for_each(|i| walk_item(i, s)),
                }
            }
        }
        fn walk_block(b: &Block, s: &mut String) {
            match b {
                Block::Paragraph { content, .. } | Block::Heading { content, .. } => {
                    walk(content, s)
                }
                Block::List { items, .. } => items.iter().for_each(|i| walk_item(i, s)),
                Block::Terms { items } => {
                    for it in items {
                        walk(&it.term, s);
                        walk(&it.desc, s);
                    }
                }
                Block::Table { cells, .. } => cells.iter().for_each(|c| walk(c, s)),
                Block::Framed(f) => walk(&f.content, s),
                Block::Stack { children, .. } => children.iter().for_each(|c| walk_block(c, s)),
                Block::Figure(f) => {
                    f.body.iter().for_each(|c| walk_block(c, s));
                    walk(&f.caption, s);
                }
                _ => {}
            }
        }
        for b in &doc.blocks {
            walk_block(b, &mut s);
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

    fn text_of(inl: &[Inline]) -> String {
        inl.iter()
            .filter_map(|i| match i {
                Inline::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn eval_lists_and_terms() {
        let (doc, d) = run_src("- a\n- b", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let Block::List { ordered, items } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert!(!ordered);
        assert_eq!(items.len(), 2);
        assert!(items.iter().all(|it| it.rest.is_empty()));
        assert_eq!(text_of(&items[0].content), "a");
        assert_eq!(text_of(&items[1].content), "b");

        let (doc, d) = run_src("- a\n  - b\n+ c", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert_eq!(text_of(&items[0].content), "a");
        let ListRest::List(nested) = &items[0].rest[0] else {
            panic!("{:?}", items[0].rest)
        };
        assert!(!nested.ordered);
        assert!(nested.items[0].rest.is_empty());
        assert_eq!(text_of(&nested.items[0].content), "b");
        let Block::List { ordered, items } = &doc.blocks[1] else {
            panic!("{:?}", doc.blocks)
        };
        assert!(ordered);
        assert!(items[0].rest.is_empty());

        let (doc, d) = run_src("- a\n  + b\n  - c", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert_eq!(items[0].rest.len(), 2);
        let ListRest::List(en) = &items[0].rest[0] else {
            panic!()
        };
        assert!(en.ordered);
        assert_eq!(text_of(&en.items[0].content), "b");
        let ListRest::List(bu) = &items[0].rest[1] else {
            panic!()
        };
        assert!(!bu.ordered);
        assert_eq!(text_of(&bu.items[0].content), "c");

        let (doc, d) = run_src("/ *T*: a _b_ c", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let Block::Terms { items } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert_eq!(items.len(), 1);
        let Inline::Text { text, style } = &items[0].term[0] else {
            panic!("{:?}", items[0].term)
        };
        assert_eq!(text, "T");
        assert_eq!(style.font, FontId::Bold);
        assert_eq!(items[0].desc.len(), 3);
        let Inline::Text { text, style } = &items[0].desc[0] else {
            panic!()
        };
        assert_eq!(text, "a ");
        assert_eq!(style.font, FontId::Regular);
        let Inline::Text { text, style } = &items[0].desc[1] else {
            panic!()
        };
        assert_eq!(text, "b");
        assert_eq!(style.font, FontId::Italic);

        // `#if` evaluates lists on the borrowing path.
        let (doc, d) = run_src("#if 1 == 1 [\n- a\n  + b\n]", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let Block::List { items, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert_eq!(text_of(&items[0].content), "a");
        let ListRest::List(nested) = &items[0].rest[0] else {
            panic!("{:?}", items[0].rest)
        };
        assert!(nested.ordered);
        assert_eq!(text_of(&nested.items[0].content), "b");

        let (doc, d) = run_src("/ no colon", Path::new("."));
        assert!(d.iter().any(|x| x.severity == Severity::Warning && x.message.contains(':')));
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert_eq!(text_of(content), "/ no colon");
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
    fn page_width_and_height() {
        let (doc, d) = run_src("#set page(width: 200pt, height: 100pt)", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(doc.styles.page_width, 200.0);
        assert_eq!(doc.styles.page_height, 100.0);

        let (doc, d) = run_src("#set page(width: 180pt)", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(doc.styles.page_width, 180.0);
        assert_eq!(doc.styles.page_height, 841.890);

        let (doc, _) = run_src("#set page(height: 400pt)", Path::new("."));
        assert_eq!(doc.styles.page_width, 595.276);
        assert_eq!(doc.styles.page_height, 400.0);

        // flipped runs after width and height, so it swaps the result.
        let (doc, d) = run_src(
            "#set page(width: 200pt, height: 100pt, flipped: true)",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(doc.styles.page_width, 100.0);
        assert_eq!(doc.styles.page_height, 200.0);

        let (doc, _) = run_src("#set page(width: 200pt, flipped: true)", Path::new("."));
        assert_eq!(doc.styles.page_width, 841.890);
        assert_eq!(doc.styles.page_height, 200.0);

        let (doc, _) = run_src(
            "#set text(size: 10pt)\n#set page(width: 12em, height: 20em)",
            Path::new("."),
        );
        assert_eq!(doc.styles.page_width, 120.0);
        assert_eq!(doc.styles.page_height, 200.0);

        let (doc, _) = run_src("#set page(width: 100mm)", Path::new("."));
        assert!((doc.styles.page_width - 283.465).abs() < 1e-6);
        assert_eq!(doc.styles.page_height, 841.890);

        let (doc, d) = run_src("#set page(width: 100pt, paper: \"tabloid\")", Path::new("."));
        assert!(d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("tabloid")));
        assert_eq!(doc.styles.page_width, 100.0);
        assert_eq!(doc.styles.page_height, 841.890);

        let (doc, _) = run_src("#set page(paper: \"a5\", width: 100pt)", Path::new("."));
        assert_eq!(doc.styles.page_width, 100.0);
        assert_eq!(doc.styles.page_height, 595.28);
    }

    #[test]
    fn link_text_and_missing_url() {
        let (doc, d) = run_src("#link(\"https://example.com\")[docs]", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        let Inline::Link(link) = &content[0] else {
            panic!("{content:?}")
        };
        assert_eq!(link.url, "https://example.com");
        assert_eq!(all_text(&doc), "docs");

        let (doc, d) = run_src("#link(\"https://example.com\", [more])", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc), "more");
        let Block::Paragraph { content, .. } = &doc.blocks[0] else {
            panic!("{:?}", doc.blocks)
        };
        assert!(matches!(&content[0], Inline::Link(link) if link.url == "https://example.com"));

        let (doc, d) = run_src("#link[plain]", Path::new("."));
        assert!(d.iter().any(|x| x.severity == Severity::Error && x.message.contains("url")));
        assert!(matches!(
            doc.blocks.first(),
            Some(Block::Paragraph { content, .. })
                if matches!(content.first(), Some(Inline::Text { text, .. }) if text == "plain")
        ));

        let (doc, d) = run_src("See #link(\"https://example.com\")[docs] now", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let pages = crate::layout::layout(&doc, &crate::font::Afm);
        let link = pages[0].items.iter().find_map(|i| match i {
            DrawItem::Link { url, w, .. } => Some((url.as_str(), *w)),
            _ => None,
        });
        let Some((url, w)) = link else {
            panic!("no annotation: {:?}", pages[0].items)
        };
        assert_eq!(url, "https://example.com");
        assert!(w > 0.0);
        assert!(pages[0].items.iter().any(|i| matches!(
            i,
            DrawItem::Rule { gray, w: rw, .. } if *gray == 0.0 && *rw > 0.0
        )));
        let pdf = crate::pdf::write_pdf(&pages, &[], &[]);
        let bytes = pdf.as_slice();
        assert!(bytes.windows(4).any(|c| c == b"/URI"));
        assert!(bytes.windows(19).any(|c| c == b"https://example.com"));
        let plain = crate::pdf::write_pdf(
            &[crate::ir::Page {
                width: doc.styles.page_width,
                height: doc.styles.page_height,
                items: vec![DrawItem::Text {
                    x: 10.0,
                    y: 20.0,
                    font: FontId::Regular,
                    size: 11.0,
                    text: "hi".into(),
                }],
            }],
            &[],
            &[],
        );
        assert!(!plain.windows(6).any(|c| c == b"/Annot"));
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
    fn eval_while_accumulates() {
        // A `#let` in the body shadows `n` and `s`. The condition keeps
        // reading the outer zeros, so the loop hits the iteration cap
        // and the outer bindings stay put.
        let (doc, d) = run_src(
            "#let n = 0\n#let s = \"\"\n#while n < 3 [\n#let s = s + \"z\"\n#let n = n + 1\n]\n#s#n",
            Path::new("."),
        );
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Error && x.message.contains("10000")),
            "{d:?}"
        );
        assert_eq!(all_text(&doc).trim(), "0");
        assert!(!all_text(&doc).contains('z'), "{}", all_text(&doc));
    }

    #[test]
    fn eval_while_paragraphs() {
        // A counted loop cannot advance a `#while` condition with `#let`.
        // Paragraph breaks inside a repeating body are covered by `#for`,
        // which uses the same body walk. A false `#while` emits nothing.
        let (doc, d) = run_src("#while false [\nP\n\n]\n", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert!(
            !doc.blocks.iter().any(|b| matches!(b, Block::Paragraph { .. })),
            "{:?}",
            doc.blocks
        );
        // The blank line is followed by a `#let` so the parser keeps the
        // paragraph break (a `]` immediately after the gap would swallow it).
        let (doc, d) = run_src(
            "#for n in (1, 2, 3) [\nP\n\n#let x = n\n]",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let paras: Vec<_> = doc
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Paragraph { .. }))
            .collect();
        assert_eq!(paras.len(), 3);
        for p in paras {
            let Block::Paragraph { content, .. } = p else {
                unreachable!()
            };
            assert_eq!(text_of(&content).trim(), "P");
        }
    }

    #[test]
    fn eval_while_cap() {
        let (_doc, d) = run_src("#while true [ ]", Path::new("."));
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Error && x.message.contains("10000")),
            "{d:?}"
        );
        assert_eq!(
            d.iter().filter(|x| x.severity == Severity::Error).count(),
            1,
            "{d:?}"
        );
    }

    #[test]
    fn eval_while_let_shadows() {
        // Typst rejects this as an infinite loop: the inner `#let`
        // shadows `n` and the condition keeps seeing 0.
        let (doc, d) = run_src(
            "#let n = 0\n#while n < 3 [\n  #let n = n + 1\n  tick #n\n]\n",
            Path::new("."),
        );
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Error && x.message.contains("10000")),
            "{d:?}"
        );
        let text = all_text(&doc);
        assert_eq!(text.matches("tick").count(), 10000, "{text}");
        assert!(text.contains("tick 1"), "{text}");
        assert!(!text.contains("tick 2"), "{text}");
        assert!(!text.contains("tick 3"), "{text}");
    }

    #[test]
    fn eval_if_let_does_not_leak() {
        let (doc, d) = run_src(
            concat!(
                "#let n = 10\n",
                "#if true [#let n = 1 #let n = n + 1 #n]\n",
                "|#n|\n",
                "#if false [no] else [#let n = 4 #n]\n",
                "|#n|\n",
                "#let n = n + 1\n",
                "|#n|\n",
            ),
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            all_text(&doc).split_whitespace().collect::<Vec<_>>(),
            ["2", "|10|", "4", "|10|", "|11|"]
        );

        // A diagnostic inside the body still pops the frame.
        let (doc, d) = run_src(
            "#let n = 1\n#if true [#let n = 9 #missing]\n#n",
            Path::new("."),
        );
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("missing")),
            "{d:?}"
        );
        assert_eq!(all_text(&doc).trim(), "1");

        // Bracket content passed as an argument is its own scope.
        let (doc, d) = run_src(
            "#let n = 1\n#strong[#let n = 8 #n]\n|#n|\n#box[#let n = 6 #n]\n|#n|",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            all_text(&doc).split_whitespace().collect::<Vec<_>>(),
            ["8", "|1|", "6", "|1|"]
        );
    }

    #[test]
    fn eval_for_binding_is_local() {
        let (doc, d) = run_src(
            concat!(
                "#let x = 7\n",
                "#for x in (1, 2, 3) [#x]\n",
                "|#x|\n",
                "#for x in (1, 2) [#let x = x + 10 #x]\n",
                "|#x|\n",
            ),
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            all_text(&doc).split_whitespace().collect::<Vec<_>>(),
            ["123", "|7|", "11", "12", "|7|"]
        );
    }

    #[test]
    fn eval_fn_param_is_local() {
        let (doc, d) = run_src(
            "#let n = 1\n#let f(n) = [#let n = n + 10 #n]\n#f(3)|#n|",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).split_whitespace().collect::<Vec<_>>(), ["13|1|"]);
    }

    #[test]
    fn eval_bool_combinators() {
        let (doc, d) = run_src(
            concat!(
                "#let a = true\n#let b = false\n#let c = 0\n",
                "#if a and not b [Y] else [N]\n",
                "#if a and b [Y] else [N]\n",
                "#if b or a [Y] else [N]\n",
                "#if b or c [Y] else [N]\n",
                "#if not a or b [Y] else [N]\n",
                "#if not (a and b) [Y] else [N]\n",
                "#if not a and b [Y] else [N]\n",
                "#if a or b and c [Y] else [N]\n",
                "#if (1) == 1 [Y] else [N]\n",
                "#if not a == false [Y] else [N]\n",
                "#if 0 [Y] else [N]\n",
            ),
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            all_text(&doc).split_whitespace().collect::<Vec<_>>(),
            ["Y", "N", "Y", "N", "N", "Y", "N", "Y", "Y", "Y", "N"]
        );

        // A combinator that is already false never enters the body.
        // One that stays true hits the iteration cap (`#let` cannot
        // update the binding the condition reads).
        let (doc, d) = run_src(
            "#let n = 0\n#while n < 5 and not n == 0 [nope]",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert!(!all_text(&doc).contains("nope"), "{}", all_text(&doc));
        let (_doc, d) = run_src(
            "#let n = 0\n#while n < 5 and not n == 3 []",
            Path::new("."),
        );
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Error && x.message.contains("10000")),
            "{d:?}"
        );
    }

    #[test]
    fn eval_methods_and_array_concat() {
        let (doc, d) = run_src(
            concat!(
                "#let xs = (10, 20, 30)\n#let m = (a: 1, b: 2)\n",
                "#let hello = \"hello\"\n#let hello2 = \"héllo\"\n",
                "#xs.len() #m.len() #hello2.len()\n",
                "#xs.at(0) #xs.at(-1) #xs.at(-2) #hello.at(4)\n",
                "#if xs.contains(20) [yes] else [no] ",
                "#if \"hello\".contains(\"ell\") [yes] else [no]\n",
                "#xs.first() #xs.last()\n",
                "#let ys = xs + (4,)\n#ys\n",
                "#let i = -1\n#xs.at(i)",
            ),
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            all_text(&doc).split_whitespace().collect::<Vec<_>>(),
            [
                "3", "2", "5", "10", "30", "20", "o", "yes", "yes", "10", "30", "10,", "20,",
                "30,", "4", "30",
            ]
        );
    }

    #[test]
    fn eval_method_errors() {
        let (_doc, d) = run_src(
            "#let a = (1, 2)\n#let s = \"hi\"\n#a.at(5)\n#a.at(-3)\n#a.nope()\n#s.first()",
            Path::new("."),
        );
        assert!(d.len() >= 4, "{d:?}");
        assert!(d.iter().any(|x| x.message.contains("out of range")));
        assert!(d.iter().any(|x| x.message.contains("unknown method")));
        assert!(d.iter().all(|x| x.severity == Severity::Error));
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
    fn import_let_fn() {
        let dir = std::env::temp_dir().join(format!("typrst-fnimp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.typ"), "#let greet(name) = [Hello #name]\n").unwrap();
        let (doc, d) = run_src("#import \"lib.typ\": greet\n#greet(\"Ada\")", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert!(all_text(&doc).contains("Hello Ada"), "{}", all_text(&doc));
        let (doc, d) = run_src("#import \"lib.typ\": *\n#greet(\"Ada\")", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert!(all_text(&doc).contains("Hello Ada"), "{}", all_text(&doc));
        let (doc, d) = run_src("#import \"lib.typ\": greet\n#let f = greet\n#f(\"Ada\")", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert!(all_text(&doc).contains("Hello Ada"), "{}", all_text(&doc));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_let_fn_bare() {
        let dir = std::env::temp_dir().join(format!("typrst-fnbare-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.typ"), "#let greet(name) = [Hello #name]\n").unwrap();
        let (doc, d) = run_src("#import \"lib.typ\"\n#lib.greet(\"Ada\")", &dir);
        assert!(d.is_empty(), "{d:?}");
        assert!(all_text(&doc).contains("Hello Ada"), "{}", all_text(&doc));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_missing_fn_warns() {
        let dir = std::env::temp_dir().join(format!("typrst-fnmiss-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.typ"), "#let greet(name) = [Hello #name]\n").unwrap();
        let (_doc, d) = run_src("#import \"lib.typ\": missing", &dir);
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("has no member")),
            "{d:?}"
        );
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
    fn eval_import_stays_in_scope() {
        let dir = std::env::temp_dir().join(format!("typrst-impscope-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.typ"), "#let greeting = \"hello\"\n").unwrap();
        std::fs::write(
            dir.join("nested.typ"),
            "#let shown = 1\n#if true [#let hidden = 2]\n",
        )
        .unwrap();

        // An import inside a content block binds in that frame.
        let (doc, d) = run_src(
            "#if true [\n#import \"lib.typ\": greeting\n#greeting\n]\n#greeting",
            &dir,
        );
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("greeting")),
            "{d:?}"
        );
        assert!(all_text(&doc).contains("hello"), "{}", all_text(&doc));
        assert_eq!(all_text(&doc).matches("hello").count(), 1, "{}", all_text(&doc));

        // A `#let` inside a content block is not a module-level binding.
        let (doc, d) = run_src("#import \"nested.typ\": *\n#shown #hidden", &dir);
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("hidden")),
            "{d:?}"
        );
        assert!(all_text(&doc).contains('1'), "{}", all_text(&doc));
        assert!(!all_text(&doc).contains('2'), "{}", all_text(&doc));
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

    fn tiny_jpeg(marker: u8, width: u16, height: u16, comps: u8) -> Vec<u8> {
        let mut v = vec![0xFF, 0xD8, 0xFF, marker, 0x00, 0x08, 0x08];
        v.extend_from_slice(&height.to_be_bytes());
        v.extend_from_slice(&width.to_be_bytes());
        v.push(comps);
        v.extend_from_slice(&[0xFF, 0xD9]);
        v
    }

    #[test]
    fn jpeg_header_sof0_sof2_and_truncated() {
        let sof0 = tiny_jpeg(0xC0, 32, 16, 3);
        let i = jpeg_info(&sof0).unwrap();
        assert_eq!((i.width, i.height, i.bits, i.components), (32, 16, 8, 3));
        let sof2 = tiny_jpeg(0xC2, 8, 4, 1);
        let i = jpeg_info(&sof2).unwrap();
        assert_eq!((i.width, i.height, i.components), (8, 4, 1));
        let mut with_app = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00];
        with_app.extend_from_slice(&sof0[2..]);
        assert_eq!(jpeg_info(&with_app).unwrap().width, 32);
        assert!(jpeg_info(&[0xFF, 0xD8]).unwrap_err().contains("truncated"));
        assert!(jpeg_info(&sof0[..sof0.len() - 4])
            .unwrap_err()
            .contains("truncated"));
    }

    #[test]
    fn image_box_aspect_ratio() {
        assert_eq!(image_box(32, 16, Some(64.0), None), (64.0, 32.0));
        assert_eq!(image_box(32, 16, None, Some(8.0)), (16.0, 8.0));
        assert_eq!(image_box(32, 16, Some(10.0), Some(20.0)), (10.0, 20.0));
        assert_eq!(image_box(32, 16, None, None), (32.0, 16.0));
        assert_eq!(image_box(100, 30, Some(10.0), None), (10.0, 3.0));
    }

    #[test]
    fn image_load_missing_and_skip() {
        let dir = std::env::temp_dir().join(format!("typrst-img-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        let jpeg = tiny_jpeg(0xC0, 32, 16, 3);
        std::fs::write(dir.join("a.jpg"), &jpeg).unwrap();
        std::fs::write(sub.join("pic.jpg"), &jpeg).unwrap();
        std::fs::write(dir.join("pic.jpg"), b"not-a-jpeg").unwrap();
        std::fs::write(sub.join("part.typ"), "#image(\"pic.jpg\", width: 64pt)").unwrap();
        std::fs::write(dir.join("bad.jpg"), [0xFF, 0xD8, 0xFF, 0xC0, 0x00, 0x08]).unwrap();

        let (doc, d) = run_src("Hello", &dir);
        assert!(d.iter().all(|x| x.severity != Severity::Error), "{d:?}");
        assert!(doc.images.is_empty());

        let (doc, d) = run_src("#image(\"a.jpg\")\nAfter", &dir);
        assert!(d.iter().all(|x| x.severity != Severity::Error), "{d:?}");
        assert_eq!(doc.images.len(), 1);
        assert_eq!(doc.images[0].bytes, jpeg);
        assert!(matches!(
            &doc.blocks[0],
            Block::Image { w, h, .. } if (*w - 32.0).abs() < 1e-9 && (*h - 16.0).abs() < 1e-9
        ));
        assert!(all_text(&doc).contains("After"));

        let (doc, _) = run_src("#image(\"a.jpg\", height: 8pt)", &dir);
        assert!(matches!(
            &doc.blocks[0],
            Block::Image { w, h, .. } if (*w - 16.0).abs() < 1e-9 && (*h - 8.0).abs() < 1e-9
        ));
        let (doc, _) = run_src("#image(\"a.jpg\", width: 10pt, height: 20pt)", &dir);
        assert!(matches!(
            &doc.blocks[0],
            Block::Image { w, h, .. } if (*w - 10.0).abs() < 1e-9 && (*h - 20.0).abs() < 1e-9
        ));

        let (doc, d) = run_src("#image(\"missing.jpg\")\nHi", &dir);
        assert!(d.iter().any(|x| x.severity == Severity::Error && x.message.contains("missing.jpg")));
        assert!(doc.images.is_empty());
        assert!(all_text(&doc).contains("Hi"));

        let (doc, d) = run_src("#image(\"x.png\")", &dir);
        assert!(d.iter().any(|x| x.message == "only jpeg images are supported"));
        assert!(doc.images.is_empty());

        let (_, d) = run_src("#image(\"bad.jpg\")", &dir);
        assert!(d.iter().any(|x| x.severity == Severity::Error && x.message.contains("truncated")));

        let (doc, d) = run_src("#include \"sub/part.typ\"", &dir);
        assert!(d.iter().all(|x| x.severity != Severity::Error), "{d:?}");
        assert_eq!(doc.images[0].bytes, jpeg);
        assert!(matches!(
            &doc.blocks[0],
            Block::Image { w, h, .. } if (*w - 64.0).abs() < 1e-9 && (*h - 32.0).abs() < 1e-9
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fixture_without_image_has_no_dct() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in ["fixtures/small.typ", "fixtures/large.typ"] {
            let src = std::fs::read_to_string(root.join(name)).unwrap();
            let (doc, d) = run_src(&src, &root.join("fixtures"));
            assert!(doc.images.is_empty(), "{name}");
            assert!(d.iter().all(|x| x.severity != Severity::Error), "{name} {d:?}");
            let pages = crate::layout::layout(&doc, &crate::font::Afm);
            let pdf = crate::pdf::write_pdf(&pages, &doc.fonts, &doc.images);
            assert!(pdf.starts_with(b"%PDF-1.4"), "{name}");
            assert!(!pdf.windows(b"/DCTDecode".len()).any(|w| w == b"/DCTDecode"), "{name}");
            assert!(!pdf.windows(b"/XObject".len()).any(|w| w == b"/XObject"), "{name}");
        }
    }

    #[test]
    fn fixture_fonts_stay_empty() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for name in ["fixtures/small.typ", "fixtures/large.typ"] {
            let src = std::fs::read_to_string(root.join(name)).unwrap();
            let (nodes, mut diags) = crate::syntax::parse(&src);
            let mut fonts = Vec::new();
            let db_cell = std::cell::OnceCell::new();
            let mut loaded = HashSet::new();
            let mut doc = {
                let mut reg = crate::fontdb::Registry::lazy(&db_cell, &mut fonts);
                let mut env = EvalEnv {
                    base_dir: &root.join("fixtures"),
                    registry: &mut reg,
                    depth: 0,
                    loaded: &mut loaded,
                };
                eval(nodes, &mut diags, &mut env)
            };
            doc.fonts = fonts;
            diags.extend(doc.diags.iter().cloned());
            assert!(
                diags.iter().all(|d| d.severity != Severity::Error),
                "{name} {diags:?}"
            );
            assert!(doc.fonts.is_empty(), "{name}");
            // No family lookup, so the font database is never opened.
            assert!(db_cell.get().is_none(), "{name}");
            let pages = crate::layout::layout(&doc, &crate::font::Afm);
            let pdf = crate::pdf::write_pdf(&pages, &doc.fonts, &doc.images);
            assert!(pdf.starts_with(b"%PDF-1.4"), "{name}");
            assert!(
                !pdf.windows(b"/FontFile2".len()).any(|w| w == b"/FontFile2"),
                "{name}"
            );
            assert!(
                !pdf.windows(b"/TrueType".len()).any(|w| w == b"/TrueType"),
                "{name}"
            );
            assert!(
                !pdf.windows(b"/XObject".len()).any(|w| w == b"/XObject"),
                "{name}"
            );
        }
    }

    #[test]
    fn user_fn_content_body() {
        let (doc, d) = run_src("#let greet(name) = [Hello #name]\n#greet(\"Ada\")", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "Hello Ada");
    }

    #[test]
    fn user_fn_expr_body() {
        let (doc, d) = run_src("#let n = 4\n#let double(x) = x * 2\n#double(n)", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "8");
    }

    #[test]
    fn user_fn_default_arg() {
        let (doc, d) = run_src("#let f(x: 3) = x * 10\n#f() #f(2)", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "30 20");
    }

    #[test]
    fn user_fn_trailing_content() {
        let (doc, d) = run_src(
            "#let pair(body, a) = [#body|#a]\n#pair(1)[Z]\n#let wrap(x) = [<#x>]\n#wrap[Q]",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let t = all_text(&doc);
        assert!(t.contains("Z|1"), "{t}");
        assert!(t.contains("<Q>"), "{t}");
    }

    #[test]
    fn user_fn_missing_and_unknown() {
        let (_, d) = run_src("#let f(x) = [#x]\n#f()", Path::new("."));
        assert!(d.iter().any(|x| x.message.contains("missing")), "{d:?}");
        let (_, d) = run_src("#let f(x) = [#x]\n#f(y: 1)", Path::new("."));
        assert!(d.iter().any(|x| x.message.contains("unknown")), "{d:?}");
    }

    #[test]
    fn user_fn_recursion_limit() {
        let (doc, d) = run_src(
            "#let f(n) = [#if n <= 0 [ok] else [#f(n - 1)]]\n#f(63)",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(all_text(&doc).trim(), "ok");

        let (_, d) = run_src(
            "#let f(n) = [#if n <= 0 [ok] else [#f(n - 1)]]\n#f(64)",
            Path::new("."),
        );
        assert_eq!(
            d.iter().filter(|x| x.message.contains("recursion")).count(),
            1,
            "{d:?}"
        );

        let (_, d) = run_src("#let f() = [#f()]\n#f()", Path::new("."));
        assert_eq!(
            d.iter().filter(|x| x.message.contains("recursion")).count(),
            1,
            "{d:?}"
        );
    }

    #[test]
    fn user_fn_heading_at_block() {
        let (doc, d) = run_src("#let hd(t) = [= #t]\n#hd[Intro]", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let heads: Vec<_> = doc
            .blocks
            .iter()
            .filter(|b| matches!(b, Block::Heading { .. }))
            .collect();
        assert_eq!(heads.len(), 1, "{:?}", doc.blocks);
        let Block::Heading { level, content } = heads[0] else {
            panic!()
        };
        assert_eq!(*level, 1);
        let text: String = content
            .iter()
            .filter_map(|i| match i {
                Inline::Text { text, .. } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(text.trim(), "Intro");
    }

    #[test]
    fn user_fn_flattens_in_paragraph() {
        let (doc, d) = run_src("#let two() = [\none\n\ntwo\n]\nGo #two() end", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(doc.blocks.len(), 1, "{:?}", doc.blocks);
        let t = all_text(&doc);
        assert!(t.contains("Go"), "{t}");
        assert!(t.contains("one"), "{t}");
        assert!(t.contains("two"), "{t}");
        assert!(t.contains("end"), "{t}");
    }

    #[test]
    fn user_fn_inline_skips_blocks() {
        let (doc, d) = run_src("#let hd() = [= Nope]\n*#hd()*", Path::new("."));
        assert!(d.iter().any(|x| x.message.contains("block-level")), "{d:?}");
        assert!(
            !doc.blocks.iter().any(|b| matches!(b, Block::Heading { .. })),
            "{:?}",
            doc.blocks
        );
    }

    #[test]
    fn user_fn_builtin_wins() {
        let (doc, d) = run_src("#let lorem(n) = [nope]\n#lorem(3)", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let t = all_text(&doc);
        assert!(t.contains("Lorem"), "{t}");
        assert!(!t.contains("nope"), "{t}");
    }

    #[test]
    fn user_fn_unknown_stays_unimplemented() {
        let (_, d) = run_src("#nope(1)", Path::new("."));
        assert!(
            d.iter().any(|x| x.severity == Severity::NotImplemented),
            "{d:?}"
        );
    }

    fn heading_words(doc: &Document) -> Vec<String> {
        doc.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Heading { content, .. } => {
                    let mut s = String::new();
                    for i in content {
                        if let Inline::Text { text, .. } = i {
                            s.push_str(text);
                        }
                    }
                    Some(s.split_whitespace().collect::<Vec<_>>().join(" "))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn label_on_heading() {
        let (doc, d) = run_src(
            "See @intro before\n= Introduction <intro>\n\n= Next\n<next>\nAlso @next here\n",
            Path::new("."),
        );
        let errs = d
            .iter()
            .filter(|x| {
                x.severity == Severity::Error
                    && x.message.contains("cannot reference heading without numbering")
            })
            .count();
        assert_eq!(errs, 2, "{d:?}");
        assert_eq!(heading_words(&doc), vec!["Introduction", "Next"]);
        let text = all_text(&doc);
        // The headings stay; the references contribute no text.
        assert!(text.contains("See "), "{text}");
        assert!(text.contains(" before"), "{text}");
        assert!(!text.contains("See Introduction"), "{text}");
        assert!(text.contains("Also "), "{text}");
        assert!(!text.contains("Also Next"), "{text}");
        assert!(!text.contains("@intro") && !text.contains("@next"), "{text}");
    }

    #[test]
    fn heading_ref_is_section_number() {
        let (doc, d) = run_src(
            "#set heading(numbering: \"1.1\")\n= Introduction <intro>\n== Details <details>\nSpace @intro next\nBang @details!\nDot @intro.\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            heading_words(&doc),
            vec!["1 Introduction", "1.1 Details"]
        );
        let text = all_text(&doc);
        assert!(text.contains("Space Section 1 next"), "{text}");
        assert!(text.contains("Bang Section 1.1!"), "{text}");
        assert!(text.contains("Dot Section 1."), "{text}");
        assert!(!text.contains("Section 1 Introduction"), "{text}");
        assert!(!text.contains("Section 1.1 Details"), "{text}");
        assert!(!text.contains("@intro"), "{text}");

        // A pattern's trailing dot stays on the heading, not the reference.
        let (doc, d) = run_src(
            "#set heading(numbering: \"1.\")\n= Introduction <intro>\n== Details <details>\n@intro / @details\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(heading_words(&doc), vec!["1. Introduction", "1.1. Details"]);
        let see = doc
            .blocks
            .iter()
            .rev()
            .find_map(|b| match b {
                Block::Paragraph { content, .. } => Some(text_of(content)),
                _ => None,
            })
            .unwrap();
        assert_eq!(see.trim(), "Section 1 / Section 1.1");

        // A label that is not a heading keeps the stored display string.
        // The blank line ends the paragraph so `<note>` attaches to it.
        let (doc, d) = run_src("Hello there.\n\n<note>\nSee @note end\n", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        let text = all_text(&doc);
        assert!(text.contains("See Hello there. end"), "{text}");
        assert!(!text.contains("@note"), "{text}");
    }

    #[test]
    fn unknown_ref_warns() {
        let (doc, d) = run_src("See @missing end", Path::new("."));
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("@missing")),
            "{d:?}"
        );
        assert!(all_text(&doc).contains("@missing"), "{}", all_text(&doc));
    }

    #[test]
    fn heading_numbering_pattern() {
        let (doc, d) = run_src(
            "#set heading(numbering: \"1.1\")\n= One <a>\n== Two <b>\n=== Three <c>\n== Four\n\n@a @b @c\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(
            heading_words(&doc),
            vec!["1 One", "1.1 Two", "1.1.1 Three", "1.2 Four"]
        );
        let text = all_text(&doc);
        assert!(text.contains("1 One 1.1 Two 1.1.1 Three"), "{text}");
        assert!(
            text.contains("Section 1 Section 1.1 Section 1.1.1"),
            "{text}"
        );

        let (doc, d) = run_src(
            "#set heading(numbering: \"1.\")\n= A\n== B\n#set heading(numbering: \"1.1.1\")\n= C\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(heading_words(&doc), vec!["1. A", "1.1. B", "2 C"]);
    }

    #[test]
    fn heading_numbering_none() {
        let (doc, d) = run_src(
            "#set heading(numbering: \"1.1\")\n= One\n#set heading(numbering: none)\n= Two\n== Three\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(heading_words(&doc), vec!["1 One", "Two", "Three"]);

        let (doc, d) = run_src("= One\n== Two\n=== Three\n", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert_eq!(heading_words(&doc), vec!["One", "Two", "Three"]);
    }

    #[test]
    fn label_without_block_errors() {
        let (_doc, d) = run_src("<orphan>\n", Path::new("."));
        assert!(
            d.iter().any(|x| x.severity == Severity::Error && x.message.contains("<orphan>")),
            "{d:?}"
        );
    }

    #[test]
    fn refs_stay_in_this_document() {
        let dir = std::env::temp_dir().join(format!("typrst-ref-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("lib.typ"), "= Secret <secret>\n").unwrap();
        std::fs::write(dir.join("part.typ"), "= Chapter <ch>\n").unwrap();
        let (doc, d) = run_src(
            "#import \"lib.typ\": *\nSee @secret\n",
            &dir,
        );
        assert!(
            d.iter().any(|x| x.severity == Severity::Warning && x.message.contains("@secret")),
            "{d:?}"
        );
        assert!(all_text(&doc).contains("@secret"), "{}", all_text(&doc));
        let (doc, d) = run_src("#include \"part.typ\"\nSee @ch\n", &dir);
        assert!(
            d.iter().any(|x| {
                x.severity == Severity::Error
                    && x.message.contains("cannot reference heading without numbering")
            }),
            "{d:?}"
        );
        let text = all_text(&doc);
        assert!(text.contains("Chapter"), "{text}");
        assert!(!text.contains("See Chapter"), "{text}");
        assert!(!text.contains("@ch"), "{text}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn numbered_heading_lays_out_bold_prefix() {
        let (doc, d) = run_src(
            "#set heading(numbering: \"1.1\")\n= Methods\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let pages = crate::layout::layout(&doc, &crate::font::Afm);
        // The prefix and the body share the heading face, so layout may
        // merge them into one run ("1 Methods") or keep them split.
        let mut words = String::new();
        for it in &pages[0].items {
            if let DrawItem::Text { font, size, text, .. } = it {
                assert_eq!(*font, FontId::Bold);
                assert!((*size - 11.0 * 1.4).abs() < 1e-6, "{size}");
                if !words.is_empty() {
                    words.push(' ');
                }
                words.push_str(text.as_str());
            }
        }
        assert_eq!(
            words.split_whitespace().collect::<Vec<_>>().join(" "),
            "1 Methods",
            "{:?}",
            pages[0].items
        );
    }

    #[test]
    fn box_unknown_fill_warns() {
        let (doc, d) = run_src("#box(fill: \"red\")[Hi]", Path::new("."));
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Warning && x.message.contains("fill")),
            "{d:?}"
        );
        assert!(all_text(&doc).contains("Hi"), "{}", all_text(&doc));
        let (_, d) = run_src("#box(fill: 2.0)[Hi]", Path::new("."));
        assert!(
            d.iter()
                .any(|x| x.severity == Severity::Warning && x.message.contains("fill")),
            "{d:?}"
        );
        let (_, d) = run_src("#box(fill: none)[Hi]", Path::new("."));
        assert!(
            d.iter().all(|x| !x.message.contains("fill")),
            "{d:?}"
        );
    }

    fn figures_of(doc: &Document) -> Vec<&Fig> {
        doc.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Figure(f) => Some(f.as_ref()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn figure_counter_caption_and_label() {
        let (doc, d) = run_src(
            "#figure(caption: [First])[Alpha] <fig>\n#figure([Beta], caption: [Second])\n#figure(caption: [Grid])[#table(columns: 1, [cell])]\nSee @fig here\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let figs = figures_of(&doc);
        assert_eq!(figs.len(), 3, "{:?}", doc.blocks);
        assert_eq!(figs[0].number, 1);
        assert_eq!(figs[1].number, 2);
        assert_eq!(figs[2].number, 3);
        assert_eq!(text_of(&figs[0].caption), "Figure 1: First");
        assert_eq!(text_of(&figs[1].caption), "Figure 2: Second");
        let Block::Paragraph { content, .. } = &figs[0].body[0] else {
            panic!("{:?}", figs[0].body);
        };
        assert_eq!(text_of(content).trim(), "Alpha");
        let Block::Paragraph { content, .. } = &figs[1].body[0] else {
            panic!("{:?}", figs[1].body);
        };
        assert_eq!(text_of(content).trim(), "Beta");
        assert!(matches!(figs[2].body.last(), Some(Block::Table { .. })));
        assert!(all_text(&doc).contains("cell"));

        let Inline::Text { text, style } = &figs[0].caption[0] else {
            panic!("{:?}", figs[0].caption);
        };
        assert_eq!(text, "Figure 1: ");
        assert_eq!(style.font, FontId::Regular);
        assert!((style.size - 11.0 * 0.9).abs() < 1e-9, "{}", style.size);
        let Inline::Text { style, .. } = &figs[0].caption[1] else {
            panic!("{:?}", figs[0].caption);
        };
        assert_eq!(style.font, FontId::Regular);
        assert!((style.size - 11.0 * 0.9).abs() < 1e-9);

        let see = doc
            .blocks
            .iter()
            .rev()
            .find_map(|b| match b {
                Block::Paragraph { content, .. } => Some(text_of(content)),
                _ => None,
            })
            .unwrap();
        assert_eq!(see.trim(), "See Figure 1 here");

        let pages = crate::layout::layout(&doc, &crate::font::Afm);
        let mut body_y = None;
        let mut cap_y = None;
        let mut cap_size = None;
        let mut cap_font = None;
        for it in &pages[0].items {
            if let DrawItem::Text { y, font, size, text, .. } = it {
                if text.as_str().contains("Alpha") {
                    body_y = Some(*y);
                }
                if text.as_str().contains("Figure 1:") {
                    cap_y = Some(*y);
                    cap_size = Some(*size);
                    cap_font = Some(*font);
                }
            }
        }
        let body_y = body_y.expect("body");
        let cap_y = cap_y.expect("caption");
        let size = cap_size.unwrap();
        assert_eq!(cap_font, Some(FontId::Regular));
        assert!((size - 11.0 * 0.9).abs() < 1e-6, "{size}");
        let lh = size * doc.styles.line_height / doc.styles.font_size;
        assert!(
            (cap_y - body_y - lh - 4.0).abs() < 1e-6,
            "body {body_y} caption {cap_y} lh {lh}"
        );

        let (doc, d) = run_src(
            "#figure(caption: [Cap])[Body]\n<fig>\nSee @fig here\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let see = doc
            .blocks
            .iter()
            .rev()
            .find_map(|b| match b {
                Block::Paragraph { content, .. } => Some(text_of(content)),
                _ => None,
            })
            .unwrap();
        assert_eq!(see.trim(), "See Figure 1 here");
    }

    #[test]
    fn figure_image_body() {
        let dir = std::env::temp_dir().join(format!("typrst-fig-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jpg"), tiny_jpeg(0xC0, 32, 16, 3)).unwrap();
        let (doc, d) = run_src(
            "#figure(caption: [Pic])[#image(\"a.jpg\")]\n",
            &dir,
        );
        assert!(d.is_empty(), "{d:?}");
        let figs = figures_of(&doc);
        assert_eq!(figs.len(), 1);
        assert!(matches!(
            figs[0].body.last(),
            Some(Block::Image { w, h, .. }) if (*w - 32.0).abs() < 1e-9 && (*h - 16.0).abs() < 1e-9
        ));
        assert_eq!(text_of(&figs[0].caption), "Figure 1: Pic");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn outline_numbers_indent_and_later_headings() {
        let (doc, d) = run_src(
            "#set heading(numbering: \"1.1\")\n= Alpha\n#outline()\n== Beta\n#figure(caption: [Pic])[\n= Inside\n]\n",
            Path::new("."),
        );
        assert!(d.is_empty(), "{d:?}");
        let paras: Vec<_> = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph { content, .. } => Some(content),
                _ => None,
            })
            .collect();
        assert_eq!(paras.len(), 3, "{:?}", doc.blocks);
        assert_eq!(text_of(paras[0]).trim(), "1 Alpha");
        assert_eq!(text_of(paras[1]).trim(), "1.1 Beta");
        assert_eq!(text_of(paras[2]).trim(), "2 Inside");
        assert!(
            !paras.iter().any(|c| text_of(c).contains("....")),
            "dot leaders"
        );
        assert!(matches!(paras[1][0], Inline::HSpace(w) if (w - 12.0).abs() < 1e-9));
        assert!(!matches!(paras[0][0], Inline::HSpace(_)));

        let pages = crate::layout::layout(&doc, &crate::font::Afm);
        let mut x1 = None;
        let mut x2 = None;
        for it in &pages[0].items {
            if let DrawItem::Text { x, font, size, text, .. } = it {
                if *font == FontId::Regular && (*size - 11.0).abs() < 1e-6 {
                    if text.as_str().contains("Alpha") {
                        x1 = Some(*x);
                    }
                    if text.as_str().contains("Beta") {
                        x2 = Some(*x);
                    }
                }
            }
        }
        let x1 = x1.expect("level 1");
        let x2 = x2.expect("level 2");
        assert!((x2 - x1 - 12.0).abs() < 1e-6, "x1 {x1} x2 {x2}");

        let (empty, d) = run_src("#outline()\n", Path::new("."));
        assert!(d.is_empty(), "{d:?}");
        assert!(empty.blocks.is_empty(), "{:?}", empty.blocks);
    }


}
