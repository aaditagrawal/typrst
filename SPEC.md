# typrst — supported Typst subset

typrst implements a growing subset of Typst, flash-tex style:
recognized-but-unimplemented constructs produce a `not implemented`
diagnostic with a recovery note, fail the build, and are not printed —
they are never silently dropped.
Unrecognized constructs produce `error`/`warning` diagnostics and the
parser recovers to the next paragraph boundary. Diagnostics from
`#import`/`#include` files are prefixed with the imported file's path.

## Markup

- Comments: `// line`, `/* block */` (nestable)
- Headings: `=` through `======` (levels 1–6), inline content after space
- Paragraphs separated by blank lines; single newlines inside a paragraph
  collapse to spaces
- Inline: `*strong*`, `_emph_`, `` `raw` `` (inline), fenced
  ```` ```lang ... ``` ```` raw blocks
- Escapes: `\*`, `\_`, `` \` ``, `\#`, `\\`, `\/`, `\-`, `\+`, `\=`,
  `\[`, `\]`, `\(`, `\)`, `\~`, `\$`, `\"` — backslash followed by any
  other ASCII punctuation emits that punctuation literally; `\` at
  end-of-line (or anywhere inline) is a hard line break (`Node::LineBreak`)
- Lists: `- item` bullet, `+ item` enumeration. Consecutive items at the
  same indent group into one `Node::List`. An item's body runs until the
  next sibling marker at the same indent, a blank line, a heading, or a
  fence. A `-` or `+` marker indented further than its parent marker (at
  least one extra space or tab) is a nested list, up to 6 levels. A
  deeper marker is a warning and stays literal text.
- Term lists: `/ term: description`. Consecutive items group. The term is
  inline markup up to the first unescaped colon outside raw spans
  (rendered bold). The description is the rest of the line plus following
  indented lines, and may contain inline markup. A missing colon is a
  warning and the line stays literal text. An indented `/` is literal
  (nested term lists are not supported).
- `$..$` math, `#footnote`, `#context`,
  `#break`, `#continue` → `not implemented` diagnostics.
  `#show` with no target, a `where` selector, a `set` transform, or any
  target other than `heading`, `strong`, `emph`, `link`, and `figure`
  is `not implemented` and is not printed.
- `<ident>` labels (letters, digits, `_`, `-`, `.`). A label on the same
  line as a heading, or the next non-empty construct before another block,
  attaches to that heading. A label elsewhere attaches to the previous
  block. With no previous block it is an error. Labels do not cross
  `#import`.
- `@ident` in inline text resolves to a label in the same document. A
  `.` is part of the name only when another label character follows it
  (`@fig.1`, `@a.b`); a dot before space, punctuation, or end of input
  stays in the text (`@intro.` is the label `intro`). A numbered
  heading renders as `Section` plus its number (`Section 1`,
  `Section 1.1`) and not the title. A heading with no numbering is an
  error (`cannot reference heading without numbering`) and contributes
  no text. Any other label renders its stored text. An unknown label
  is a warning and renders the literal `@ident`.

## Expressions (`#expr`)

Literals: integers, floats, strings `"..."`, lengths (`pt`, `mm`, `cm`,
`in`, `em`), `true`, `false`, `none`, arrays `(1, 2pt, "x")` (trailing
comma required for 1-element), dictionaries `(a: 1, b: "x")`, column
fractions `1fr` (table context only).

- `#set page(paper: "a4" | "a5" | "us-letter", margin: L | (x: L, y: L) | (top: .., right: .., bottom: .., left: ..) | (rest: L), flipped: bool)`
- `#set page(width: L, height: L)` — `pt`/`mm`/`cm`/`in`/`em` (`em` is the current font size). Setting one side leaves the other. `paper` still sets both. `flipped` swaps the final size. Unknown paper stays a warning.
- `#set text(size: L, font: "..")` — `font` resolves through the font
  database: base-14 aliases (`times`, `courier`, `serif`, `mono`, `sans`,
  …) map to the built-in Times/Courier faces; any other family is looked
  up in the discovered system fonts and embedded in the PDF as a real
  TrueType face (`FontId::Ext`). Unknown families warn and fall back.
- `#set par(justify: bool, leading: L, spacing: L)` — `leading` sets
  baseline-to-baseline = `size + leading` if < size else `leading`
- `#set heading(numbering: "1" | "1." | "1.1" | "1.1.1" | none)` — per-level
  counters. Default is `none` (headings are unchanged). The pattern is one
  or more `1` tokens separated by `.`, with an optional trailing `.`. A
  level-n heading shows n numbers (`1`, `1.2`, `1.2.3`); a shorter pattern
  still shows the full chain, and a longer pattern trims to the heading's
  level. A trailing `.` is kept (`1.` / `1.1.`). The number is part of the
  heading text, then a space, then the body. `none` clears the pattern.
- `#let name = <expr>` — literal, variable, `mod.field`, method call,
  or binary arithmetic (`+ - * /` on numbers/lengths; `+` also
  concatenates strings and arrays). Later `#name` substitutes the
  value. Lookup walks from the innermost scope outward, and `#let`
  binds in the current scope (a later `#let` of the same name in that
  same scope replaces it). A content block (`#if` / `#while` / `#for`
  body, function body, or bracket content passed as an argument) is a
  nested scope, popped when the block finishes even if a diagnostic
  fires, so a `#let` inside it shadows outer names and does not update
  them, and `#for` loop variables are bindings in that body scope.
- `#let name(params) = [content]` and `#let name(params) = expr` —
  user functions. A parameter is `name` or `name: literal` (int, float,
  string, length, bool, `none`). Calls are `#name(args)`,
  `#name(args)[content]`, and `#name[content]`. Arguments are
  expressions (literals, variables, fields, arithmetic, method calls).
  Trailing content is the last positional argument, or binds to a
  parameter named `body` when there is one. Positional arguments are
  applied, then named; a missing required parameter or an unknown named
  argument is an error, and defaults fill the rest. Content bodies
  evaluate like `#for` bodies with the parameters in a scope frame
  local to that call; expression bodies use the `#let`
  arithmetic/field language. Calls recurse to
  depth 64, then error. A call inside a paragraph flattens the body's
  paragraph inlines into that paragraph; at block position, headings,
  lists, tables, page breaks, and vertical space are spliced in; those
  same block-level pieces are skipped with a diagnostic when the call
  is inline. Builtin names (`#lorem`, `#text`, …) stay builtins. No
  destructuring, sinks, spreads, or closures.
- `#var` — bound variable; arrays/dicts render as `1, 2` / `(k: v)`.
- `#a.b` — field access on dictionaries / imported modules.
- Method calls (`value.method(args)`; `value.field` is still a field):
  - `len()` on array, dict, and string (string length in characters)
  - `at(i)` on array and string. `i` is an integer literal or a
    variable; negative indexes count from the end. Out of range is an
    error and the value is `none`
  - `first()` and `last()` on array
  - `contains(v)` on array (element equality) and string (substring)
  - any other method is an error and the value is `none`
- `#if <cond> [..] else if <cond> [..] else [..]` — a condition is a
  comparison (`== != < <= > >=`), a bare value (truthiness: not `none`,
  not `false`, not zero), or those joined by `and`, `or`, and prefix
  `not`. `and` binds tighter than `or`; `not` binds tighter than both.
  Parentheses group a condition. This is not a general expression
  language.
- `#while <cond> [..]` — same conditions as `#if`. The body is
  evaluated until the condition is false, at most 10000 times.
  Exceeding the cap is an error and the loop stops. The body is a
  nested scope, so `#let n = n + 1` inside it does not change the `n`
  the condition reads.
- `#for x in <array> [..]`, `#for (i, x) in <array> [..]` (index+elem),
  `#for (k, v) in <dict> [..]` (key+value) — body evaluated per item.
  Loop variables are bindings in that body scope; an outer name is
  unchanged after the loop.
- `#import "spec" [: a, b | : *]` — see Packages below.
- `#include "file.typ"` — parses and evaluates the file inline.
- `#lorem(n)` — n words of lorem ipsum text
- `#pagebreak()`, `#v(len)`, `#h(len)`, `#linebreak()`
- `#strong[..]`, `#emph[..]`, `#text(size: L, weight: "bold"|int,
  font: "..")[..]`
- `#align(center|right|left)[..]` — horizontal alignment of the block
- `#table(columns: n | (spec, ..), stroke: bool, ..cells)`,
  `#grid(columns: .., ..cells)` — column spec `auto`, length, or `Nfr`;
  cells are row-major positional args. Row/column spans are not
  supported.
- `#image("path.jpg" | "path.jpeg", width: L, height: L)` — JPEG only.
  The path is relative to the file. `width` and `height` are optional
  lengths; setting one preserves aspect ratio, setting neither uses the
  intrinsic pixel size as points (1 px = 1 pt). Other formats error.
  The image is a block that occupies its height.
- `#box(width: L, inset: L, fill: luma, stroke: L, outset: none)[content]`
  — inline framed run. `width` defaults to the content width (capped at
  the line); `inset` defaults to `0pt`. `fill` is a luma: float `0.0`
  (black) ..= `1.0` (white), or an integer `0..=255` (0 black, 255
  white). Omitted `fill` draws no background. `stroke` is the border
  thickness in pt; omitted or `none` draws no border. `outset` accepts
  only `none`. Content is inline markup. A heading, list, or table
  inside the content is a `not implemented` diagnostic; the inline text
  is still shown. A blank line inside the content promotes the box to
  a block (still shrink-to-fit unless `width` is set).
- `#block(...)[content]` — block-level frame with the same arguments.
  It breaks the surrounding paragraph. `width` defaults to the full
  text width. Blank lines inside the content become line breaks.
- `#stack(dir: ttb|ltr, spacing: L)[a][b][c]` — `ttb` (default) stacks
  children vertically with `spacing` between them (`0pt` default).
  `ltr` places children in one row; each child's width is the available
  width divided by the child count, unless that child is a box with an
  explicit `width`. Children are the trailing content blocks and any
  positional content arguments, in source order.
- `#figure(caption: [..])[body]` and `#figure([body], caption: [..])`.
  The body is inline content or one block (paragraph, image, or table).
  A caption line follows the body: `Figure N: ` plus the caption, at
  0.9em and not bold, with a 4pt gap. N is a document counter starting
  at 1. A `<label>` on the figure stores `Figure N` (no caption text).
- `#link("url")[text]` and `#link("url", [text])` — inline content, drawn with a thin black underline. The PDF adds a URI annotation over that rectangle. No url is an error and the content is plain text. SVG underlines and skips the annotation.
- `#outline()` takes no arguments. It emits one paragraph per heading
  in the document, including headings that appear after the call and
  headings inside figures. Each entry is the heading's laid-out text,
  with the number prefix when numbering is on. Entries indent 12pt per
  level below 1, with no dot leaders. An outline with no headings emits
  nothing.
- `#show heading|strong|emph|link|figure: name => body` — one rule per
  target. A later rule for the same target replaces it; rules do not
  chain. `name` is any identifier. `body` is `[content]` or an
  expression. The parameter is bound to that element's body
  (`Value::Content`): the heading's inline body before its number
  prefix, the inner content of strong or emph, the link's visible
  content, or the figure body (the caption stays the caption). The body
  is evaluated in a nested scope and replaces that element body. Heading
  numbering and style, the strong/emph face, the link underline and
  annotation, and the `Figure N:` caption still apply.
- Unknown `#foo` → `not implemented` diagnostic, recover at end of the
  expression.

## Packages

`#import` accepts typst package specs and plain paths:

- `"@preview/name:version"` — searched in `$XDG_DATA_HOME/typst/packages`,
  `~/.local/share/typst/packages`, `$XDG_CACHE_HOME/typst/packages`,
  `~/.cache/typst/packages` (so packages already fetched by `typst` are
  reused), then `~/.cache/typrst/packages`. Missing `@preview` packages
  are downloaded from `packages.typst.org` (via `curl`/`tar`) into the
  typrst cache. `typst.toml` `[package].entrypoint` selects the module.
- `"file.typ"` / `"../dir/file.typ"` — resolved relative to the
  importing file's directory.
- Imported modules evaluate to a dictionary of their `#let` bindings:
  `#import "lib.typ": a, b` binds names, `: *` binds all, bare
  `#import "lib.typ"` binds `lib` to the dict. Imported `#let` functions
  are included and callable.
- Cyclic imports and depth > 64 are `error` diagnostics; the cycle is
  cut and compilation continues.

## Fonts

`typrst fonts` lists the index. Discovery (`FontDb::discover`):

- Scans `/System/Library/Fonts{,/Supplemental}`, `/Library/Fonts`,
  `~/Library/Fonts`, `/usr/share/fonts`, `/usr/local/share/fonts`,
  `~/.fonts`, `~/.local/share/fonts`, and `$TYPRST_FONT_PATHS`
  (colon-separated), in parallel across `available_parallelism` threads
  sharing a directory work-queue.
- Parses sfnt headers (`ttf`/`otf`/`ttc`): `name` (family/subfamily/
  PostScript name), `OS/2` (weight, italic), `head`, `hhea`, `post`
  (monospace). Classifies ~800+ faces per scan.
- Caches the index in `~/.cache/typrst/fontdb-v1.bin`, validated against
  every scanned directory's mtime — a warm load is a single file read
  (~1 ms).
- Discovery is lazy: documents that only use base-14 aliases never scan.
- `#set text(font: "..")` on a discovered family registers a
  `FontId::Ext` face: layout uses real cmap+hmtx metrics; the PDF writer
  embeds the font as `/TrueType` + `/FontFile2` (TTC faces are extracted
  to standalone sfnts first). CFF/OTTO faces fall back to base-14.
- Non-base-14 faces use cached hmtx widths and legacy kern pairs when
  the table exists.

## Layout model

- Page: width/height from paper; margins; text block =
  `(left, top)` to `(right, bottom)`; content that overflows starts a
  new page.
- Base style: 11pt Times, line height 1.3082 × size, block gap 6.05pt,
  no justification (ragged right) unless `#set par(justify: true)`.
  Baseline model: delta to next line = that line's own line height +
  `max(prev below, next above)` — fitted against typst's PDF output.
- Headings: bold, sizes rel. to base — L1 1.4em, L2 1.2em, L3 1.05em,
  L4+ 1.0em; space before = 0.536em of heading size, after ≈ 0.
  A numbering prefix is part of that text (same bold face and size), then a space, then the heading body.
- Lists: bullet `•` / `n.` marker at the margin, 0.5em gap, hanging
  indent — continuation lines align at text start. A nested list shifts
  its marker by one hanging indent (marker width + 0.5em) per level and
  repeats that hanging indent.
- Term lists: the term is bold, then a 0.5em gap and the description on
  the same line when the term fits in the text block. Continuation lines
  align under the description. A term wider than the block is its own
  line and the description follows at the margin.
- Raw inline: Courier, same size. Raw block: Courier at 0.9em, preserving
  internal line breaks, surrounded by block spacing.
- Tables: cell inset 5pt/4pt, atomic-row pagination, 0.5pt borders when
  `stroke` is on (`#grid` defaults off).
- Frames (`#box` / `#block`): content is flowed in the inner width
  (`width - 2*inset`). A fill is a `Rule` inserted behind the text;
  a stroke is four thin rules on the edges. `#stack(dir: ttb)` inserts
  `spacing` between children; `dir: ltr` places them on one row.
- Greedy line breaking at spaces; `justify` distributes slack across
  space widths.

## Encodings

Output text is encoded WinAnsi (PDF base-14 and embedded simple TrueType
alike). Unmappable characters → `?` plus a warning diagnostic (one per
document, not per char).

## Speed

Compatibility work must keep a 10x wall-clock speedup versus typst 0.15.1
on `fixtures/small.typ` and `fixtures/large.typ` (fresh process, median).
`scripts/speed_gate.py` enforces it: both ratios must be at least 10x.
