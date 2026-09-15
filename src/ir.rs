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
    /// Bullet (`-`) or enumeration (`+`) list.
    List { ordered: bool, items: Vec<Vec<Node>> },
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
    /// `#let name = value` (value is a literal in our subset).
    Let { name: String, value: Value },
    /// `#name` — a bound variable reference.
    Var { name: String },
    /// `#func(args)` with optional trailing `[content]`.
    Func {
        name: String,
        args: Vec<Arg>,
        content: Option<Vec<Node>>,
    },
    /// A bare literal in markup: `#"str"`, `#12pt`, ...
    Lit(Value),
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
}

impl Default for Styles {
    fn default() -> Self {
        // A4, 2.5cm margins, 11pt text — matching typst's defaults closely
        // enough for the supported subset.
        Self {
            page_width: 595.276,   // 210mm
            page_height: 841.890,  // 297mm
            margin_top: 70.866,    // 2.5cm
            margin_bottom: 70.866,
            margin_left: 70.866,
            margin_right: 70.866,
            font_size: 11.0,
            line_height: 14.3, // 1.3 * 11pt
            par_spacing: 9.0,
            justify: false,
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
    Paragraph { content: Vec<Inline>, align: HAlign },
    Heading { level: u8, content: Vec<Inline> },
    List { ordered: bool, items: Vec<Vec<Inline>> },
    /// Fenced code block; text is the verbatim content.
    RawBlock { text: String },
    /// Explicit vertical space, in pt (`#v(..)`).
    VSpace(f64),
    /// Forced page break (`#pagebreak()`).
    PageBreak,
}

/// Inline content inside a block.
#[derive(Debug, Clone)]
pub enum Inline {
    Text { text: String, style: TextStyle },
    LineBreak,
    /// Explicit horizontal space in pt (`#h(..)`).
    HSpace(f64),
}

/// Base-14 font identities. Exporters map these to PDF base-14 names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FontId {
    Regular,   // Helvetica
    Bold,      // Helvetica-Bold
    Italic,    // Helvetica-Oblique
    BoldItalic,// Helvetica-BoldOblique
    Mono,      // Courier
    MonoBold,  // Courier-Bold
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

/// The evaluated document.
#[derive(Debug, Clone)]
pub struct Document {
    pub styles: Styles,
    pub blocks: Vec<Block>,
    pub diags: Vec<Diag>,
}

// ------------------- Display list (layout output) -------------------

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
        text: String,
    },
}

// ------------------------- Font metrics -------------------------

/// Width metrics in 1/1000 em (AFM convention). Implementations must be
/// cheap to construct — no file I/O, no fontdb scanning. This is where our
/// startup advantage over typst comes from.
pub trait FontMetrics {
    /// Width of a Unicode scalar in 1/1000 em for `font`.
    fn char_width(&self, font: FontId, c: char) -> f64;

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
