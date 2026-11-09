//! Shared intermediate representations — the contract between the parser
//! (`syntax`), the evaluator (`eval`), the layout engine (`layout`) and the
//! exporters (`pdf`, `svg`). Everything passes through these types.

/// Byte-offset span into the source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

/// Diagnostic severity. Following the flash-tex philosophy, constructs we
/// recognize but do not implement are reported, never silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    NotImplemented,
}

impl Severity {
    pub fn label(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::NotImplemented => "not implemented",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Diag {
    pub severity: Severity,
    pub span: Option<Span>,
    pub message: String,
}

impl Diag {
    pub fn new(severity: Severity, span: Option<Span>, message: impl Into<String>) -> Self {
        Self {
            severity,
            span,
            message: message.into(),
        }
    }
}

// ------------------------- AST (parser output) -------------------------

/// One `/ term: description` entry. `term` stops at the colon.
#[derive(Debug, Clone, PartialEq)]
pub struct TermEntry {
    pub term: Vec<Node>,
    pub desc: Vec<Node>,
}

/// A parsed markup node. Block- and inline-level nodes share one type;
/// the evaluator resolves the distinction.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// A run of literal text. Newlines inside a paragraph are already
    /// normalized to spaces by the parser.
    Text(String),
    /// A forced line break (`\` in markup, or `#linebreak()`).
    LineBreak,
    /// `*strong*`
    Strong(Vec<Node>),
    /// `_emph_`
    Emph(Vec<Node>),
    /// `=`, `==`, ... heading with level 1..=6.
    Heading { level: u8, body: Vec<Node> },
    /// Bullet (`-`) or enumeration (`+`) list. A nested list is a
    /// `Node::List` inside an item; the item's other nodes are inline.
    List {
        ordered: bool,
        items: Vec<Vec<Node>>,
    },
    /// `/ term: description`. Consecutive items are one node.
    Terms { items: Vec<TermEntry> },
    /// Raw code: `` `inline` `` (block=false) or a fenced ``` block.
    Raw { text: String, block: bool },
    /// Explicit paragraph boundary (a blank line).
    Parbreak,
    /// A `#`-expression.
    Expr(Expr),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// `#set page(...)`, `#set text(...)`, `#set par(...)`, ...
    Set { target: String, args: Vec<Arg> },
    /// `#let name = value` where value is a literal, a variable
    /// reference, or a simple binary expression.
    Let { name: String, value: Rhs },
    /// `#name` — a bound variable reference.
    Var { name: String },
    /// `#func(args)` with optional trailing `[content]`.
    Func {
        name: String,
        args: Vec<Arg>,
        content: Option<Vec<Node>>,
    },
    /// `#import "path"` / `#import "@preview/pkg:1.0.0": a, b` /
    /// `#import "x.typ": *`. `items` lists named bindings; `all` is the
    /// `*` wildcard; neither means the module binds under its file stem.
    Import {
        path: String,
        items: Vec<String>,
        all: bool,
    },
    /// `#if cond [..] else if cond [..] else [..]`.
    If {
        arms: Vec<(Cmp, Vec<Node>)>,
        els: Option<Vec<Node>>,
    },
    /// `#for x in expr [..]` — `vars` is 1 name, or 2 for `(k, v)` pairs.
    For {
        vars: Vec<String>,
        iter: Rhs,
        body: Vec<Node>,
    },
    /// A bare literal in markup: `#"str"`, `#12pt`, ...
    Lit(Value),
    /// A simple binary expression in markup: `#a + b`, `#x * 2`.
    Bin { lhs: Rhs, op: ArithOp, rhs: Rhs },
}

/// Right-hand side of a binding, condition, or iteration.
#[derive(Debug, Clone, PartialEq)]
pub enum Rhs {
    Lit(Value),
    Var(String),
    /// Field access on a module/dict: `mod.name`.
    Field {
        base: Box<Rhs>,
        name: String,
    },
    /// Binary operation: `a + b`, `n * 2`, ...
    Bin {
        lhs: Box<Rhs>,
        op: ArithOp,
        rhs: Box<Rhs>,
    },
}

/// Comparison in `#if`/`#while` conditions.
#[derive(Debug, Clone, PartialEq)]
pub struct Cmp {
    pub lhs: Rhs,
    pub op: CmpOp,
    pub rhs: Rhs,
    /// Unary `not` applied to the comparison.
    pub negated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Arg {
    /// `None` for positional args, `Some(name)` for `name: value`.
    pub name: Option<String>,
    pub value: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    None,
    Bool(bool),
    Int(i64),
    Float(f64),
    Length(Length),
    /// String literal.
    Str(String),
    /// `[...]` markup passed as an argument.
    Content(Vec<Node>),
    /// `(1, 2, 3)` array literal.
    Array(Vec<Value>),
    /// `(a: 1, b: 2)` dictionary literal.
    Dict(Vec<(String, Value)>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Length {
    /// Absolute length in PostScript points (1/72 in). The parser converts
    /// mm/cm/in to this.
    Pt(f64),
    /// Font-relative; resolves against the current text size.
    Em(f64),
}

impl Length {
    /// Resolve to points given the em base font size in pt.
    pub fn pt(&self, em_base: f64) -> f64 {
        match *self {
            Length::Pt(p) => p,
            Length::Em(e) => e * em_base,
        }
    }
}

// ------------------- Evaluated document (eval output) -------------------

/// Resolved page and text configuration.
#[derive(Debug, Clone)]
pub struct Styles {
    pub page_width: f64,  // pt
    pub page_height: f64, // pt
    pub margin_top: f64,
    pub margin_bottom: f64,
    pub margin_left: f64,
    pub margin_right: f64,
    pub font_size: f64,   // pt
    pub line_height: f64, // pt, baseline to baseline
    pub par_spacing: f64, // pt of vertical space between paragraphs
    pub justify: bool,
    /// Requested font family from `#set text(font: "..")`. `None` means
    /// the built-in base-14 serif stack (Times). A `Some` is resolved
    /// through the font database during eval.
    pub font_family: Option<String>,
}

impl Default for Styles {
    fn default() -> Self {
        // A4, 2.5cm margins, 11pt text — matching typst's defaults closely
        // enough for the supported subset.
        Self {
            page_width: 595.276,  // 210mm
            page_height: 841.890, // 297mm
            margin_top: 70.866,   // 2.5cm
            margin_bottom: 70.866,
            margin_left: 70.866,
            margin_right: 70.866,
            font_size: 11.0,
            line_height: 14.39, // measured baseline-to-baseline in typst output
            par_spacing: 6.05,  // measured block gap in typst output
            justify: false,
            font_family: None,
        }
    }
}

/// Horizontal alignment for a block (`#align(..)`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Block-level evaluated content, ready for layout.
#[derive(Debug, Clone)]
pub enum Block {
    Paragraph {
        content: Vec<Inline>,
        align: HAlign,
    },
    Heading {
        level: u8,
        content: Vec<Inline>,
    },
    /// Bullet or enumeration. `items[i].rest` is an empty vec (no heap
    /// allocation) when the item contains no nested list.
    List {
        ordered: bool,
        items: Vec<ListItem>,
    },
    /// `/ term: description`. Term inlines are already bold.
    Terms { items: Vec<TermItem> },
    /// Fenced code block; text is the verbatim content.
    RawBlock {
        text: String,
    },
    /// Explicit vertical space, in pt (`#v(..)`).
    VSpace(f64),
    /// Forced page break (`#pagebreak()`).
    PageBreak,
    /// `#table(..)` / `#grid(..)` evaluated to rows of cells.
    /// Cells are row-major inline content; `columns` describes each
    /// column's width spec. Row/col spans are out of scope for now.
    Table {
        columns: Vec<ColSpec>,
        cells: Vec<Vec<Inline>>,
        /// Draw cell borders (`#table` true, `#grid` false).
        stroke: bool,
    },
}

/// One bullet or enum item. `rest` stays empty on the flat-list path.
#[derive(Debug, Clone)]
pub struct ListItem {
    pub content: Vec<Inline>,
    /// Nested lists and inline runs that follow them, in source order.
    pub rest: Vec<ListRest>,
}

/// What follows the leading inline run of a list item.
#[derive(Debug, Clone)]
pub enum ListRest {
    /// Nested `-` / `+` list.
    List(Box<ListBody>),
    /// Inline continuation after a nested list.
    Inlines(Vec<Inline>),
}

/// Nested list body. Layout walks this directly; no second pass.
#[derive(Debug, Clone)]
pub struct ListBody {
    pub ordered: bool,
    pub items: Vec<ListItem>,
}

/// One evaluated term-list entry.
#[derive(Debug, Clone)]
pub struct TermItem {
    pub term: Vec<Inline>,
    pub desc: Vec<Inline>,
}

/// Column width spec for `Block::Table`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ColSpec {
    /// Sized to the widest cell.
    Auto,
    /// Fractional share of the remaining width (`1fr`).
    Fr(f64),
    /// Fixed width in pt.
    Fixed(f64),
}

/// Inline content inside a block.
#[derive(Debug, Clone)]
pub enum Inline {
    Text {
        text: String,
        style: TextStyle,
    },
    LineBreak,
    /// Explicit horizontal space in pt (`#h(..)`).
    HSpace(f64),
}

/// Font identities. The six base variants map to PDF base-14 faces;
/// `Ext(i)` indexes `Document::fonts` — a face resolved through the
/// font database and embedded into the PDF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontId {
    Regular,    // Times-Roman
    Bold,       // Times-Bold
    Italic,     // Times-Italic
    BoldItalic, // Times-BoldItalic
    Mono,       // Courier
    MonoBold,   // Courier-Bold
    /// Dynamic face index into `Document::fonts`.
    Ext(u16),
}

impl FontId {
    pub const ALL: [FontId; 6] = [
        FontId::Regular,
        FontId::Bold,
        FontId::Italic,
        FontId::BoldItalic,
        FontId::Mono,
        FontId::MonoBold,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextStyle {
    pub font: FontId,
    pub size: f64, // pt
}

/// A discovered font face registered during evaluation. `FontId::Ext(i)`
/// refers to `fonts[i]`; exporters embed the file's TrueType data.
#[derive(Debug, Clone)]
pub struct FontUse {
    /// PostScript name from the font's name table (PDF BaseFont).
    pub postscript_name: String,
    /// Family name the document asked for (for SVG font-family).
    pub family: String,
    /// Path to the font file on disk.
    pub path: std::path::PathBuf,
    /// Face index inside a `.ttc` collection (0 for single fonts).
    pub ttc_index: u32,
}

/// Registry handed to the evaluator: resolves a requested family + style
/// to a `FontId`, recording `FontUse` entries along the way. The fontdb
/// provides the real implementation; tests may stub it.
pub trait FaceRegistry {
    /// Resolve `family` at the given style. Return `None` if unknown —
    /// the evaluator falls back to the base-14 stack with a diagnostic.
    fn resolve(&mut self, family: &str, bold: bool, italic: bool, mono: bool) -> Option<FontId>;
}

/// The evaluated document.
#[derive(Debug, Clone)]
pub struct Document {
    pub styles: Styles,
    pub blocks: Vec<Block>,
    pub diags: Vec<Diag>,
    /// Dynamic fonts referenced by `FontId::Ext`, in registration order.
    pub fonts: Vec<FontUse>,
}

// ------------------- Display list (layout output) -------------------

/// Short runs (a styled word, a list marker) stay inside the draw item.
/// Longer lines spill to one heap string. Avoids a malloc per word, which
/// is where line placement was spending its time.
const INLINE_CAP: usize = 23;

#[derive(Clone)]
enum InlineStore {
    Inl([u8; INLINE_CAP], u8),
    Heap(String),
}

#[derive(Clone)]
pub struct InlineStr {
    store: InlineStore,
}

impl InlineStr {
    pub fn new() -> Self {
        Self {
            store: InlineStore::Inl([0; INLINE_CAP], 0),
        }
    }

    pub fn as_str(&self) -> &str {
        match &self.store {
            InlineStore::Inl(buf, n) => std::str::from_utf8(&buf[..*n as usize]).unwrap(),
            InlineStore::Heap(s) => s.as_str(),
        }
    }

    pub fn push_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if let InlineStore::Heap(h) = &mut self.store {
            h.push_str(s);
            return;
        }
        let len = match &self.store {
            InlineStore::Inl(_, n) => *n as usize,
            InlineStore::Heap(_) => return,
        };
        if len + s.len() <= INLINE_CAP {
            if let InlineStore::Inl(buf, n) = &mut self.store {
                buf[len..len + s.len()].copy_from_slice(s.as_bytes());
                *n = (len + s.len()) as u8;
            }
        } else {
            let mut h = String::with_capacity(len + s.len() + 16);
            h.push_str(self.as_str());
            h.push_str(s);
            self.store = InlineStore::Heap(h);
        }
    }
}

impl Default for InlineStr {
    fn default() -> Self {
        Self::new()
    }
}

impl From<&str> for InlineStr {
    fn from(s: &str) -> Self {
        if s.len() <= INLINE_CAP {
            let mut buf = [0u8; INLINE_CAP];
            buf[..s.len()].copy_from_slice(s.as_bytes());
            Self {
                store: InlineStore::Inl(buf, s.len() as u8),
            }
        } else {
            Self {
                store: InlineStore::Heap(s.to_string()),
            }
        }
    }
}

impl From<String> for InlineStr {
    fn from(s: String) -> Self {
        if s.len() <= INLINE_CAP {
            Self::from(s.as_str())
        } else {
            Self {
                store: InlineStore::Heap(s),
            }
        }
    }
}

impl std::ops::Deref for InlineStr {
    type Target = str;
    fn deref(&self) -> &str {
        self.as_str()
    }
}

impl PartialEq for InlineStr {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

impl PartialEq<str> for InlineStr {
    fn eq(&self, other: &str) -> bool {
        self.as_str() == other
    }
}

impl Eq for InlineStr {}

impl std::fmt::Debug for InlineStr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(self.as_str(), f)
    }
}

#[derive(Debug, Clone)]
pub struct Page {
    pub width: f64,
    pub height: f64,
    pub items: Vec<DrawItem>,
}

#[derive(Debug, Clone)]
pub enum DrawItem {
    /// Text with baseline origin at (x, y). `text` is Unicode; exporters
    /// encode to WinAnsi, replacing unmappable characters with '?'.
    Text {
        x: f64,
        y: f64,
        font: FontId,
        size: f64,
        text: InlineStr,
    },
    /// A filled rectangle at (x, y) with size (w, h) — used for table
    /// borders and rules. `gray` is 0.0 black .. 1.0 white.
    Rule {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
        gray: f64,
    },
}

// ------------------------- Font metrics -------------------------

/// Width metrics in 1/1000 em (AFM convention). Implementations must be
/// cheap to construct — no file I/O, no fontdb scanning. This is where our
/// startup advantage over typst comes from.
pub trait FontMetrics {
    /// Width of a Unicode scalar in 1/1000 em for `font`.
    fn char_width(&self, font: FontId, c: char) -> f64;

    /// Byte LUT of AFM units for `0..128`, if this face has one.
    /// The line breaker uses it to turn a word's width into a few adds
    /// with the font match hoisted out of the byte loop. `None` falls
    /// back to [`char_width`](Self::char_width).
    fn ascii_units(&self, _font: FontId) -> Option<&'static [u16; 128]> {
        None
    }

    /// Width of a string rendered at `size` pt, in points.
    fn str_width(&self, font: FontId, size: f64, s: &str) -> f64 {
        s.chars().map(|c| self.char_width(font, c)).sum::<f64>() * size / 1000.0
    }

    /// Typographic ascent in 1/1000 em.
    fn ascent(&self, _font: FontId) -> f64 {
        718.0
    }
    /// Typographic descent in 1/1000 em (positive number).
    fn descent(&self, _font: FontId) -> f64 {
        207.0
    }
}

// ------------------------- Utilities -------------------------

/// Convert a byte offset to 1-based (line, col).
pub fn line_col(src: &str, offset: usize) -> (usize, usize) {
    let mut line = 1usize;
    let mut col = 1usize;
    for (i, b) in src.bytes().enumerate() {
        if i >= offset {
            break;
        }
        if b == b'\n' {
            line += 1;
            col = 1;
        } else {
            col += 1;
        }
    }
    (line, col)
}
