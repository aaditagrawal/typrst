# typrst — supported Typst subset

typrst implements a growing subset of Typst, flash-tex style:
recognized-but-unimplemented constructs produce a `not implemented`
diagnostic with a recovery note — they are never silently dropped.
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
- Lists: `- item` bullet, `+ item` enumeration. Consecutive items group
  into one `Node::List`. An item's body runs until the next item marker,
  a blank line, a heading, or a fence. Nested lists are NOT supported —
  an indented marker inside an item is literal text.
- Term lists `/ term: desc`, `$..$` math, `#image`, `#figure`,
  `#footnote`, `#stack`, `#box`, `#block`, `#show`, `#context`,
  numbering patterns, labels `<x>`, references `@x`, `#while`,
  `#let f(x) = ...` function definitions → `not implemented` diagnostics.

## Expressions (`#expr`)

Literals: integers, floats, strings `"..."`, lengths (`pt`, `mm`, `cm`,
`in`, `em`), `true`, `false`, `none`, arrays `(1, 2pt, "x")` (trailing
comma required for 1-element), dictionaries `(a: 1, b: "x")`, column
fractions `1fr` (table context only).

- `#set page(paper: "a4" | "a5" | "us-letter", margin: L | (x: L, y: L) | (top: .., right: .., bottom: .., left: ..) | (rest: L), flipped: bool)`
- `#set text(size: L, font: "..")` — `font` resolves through the font
  database: base-14 aliases (`times`, `courier`, `serif`, `mono`, `sans`,
  …) map to the built-in Times/Courier faces; any other family is looked
  up in the discovered system fonts and embedded in the PDF as a real
  TrueType face (`FontId::Ext`). Unknown families warn and fall back.
- `#set par(justify: bool, leading: L, spacing: L)` — `leading` sets
  baseline-to-baseline = `size + leading` if < size else `leading`
- `#let name = <expr>` — literal, variable, `mod.field`, or binary
  arithmetic (`+ - * /` on numbers/lengths; `+` also concatenates
  strings). Later `#name` substitutes the value.
- `#var` — bound variable; arrays/dicts render as `1, 2` / `(k: v)`.
- `#a.b` — field access on dictionaries / imported modules.
- `#if <cmp> [..] else if <cmp> [..] else [..]` — comparisons
  `== != < <= > >=` on values, optional `not` prefix; bare `#if x`
  tests truthiness (`x != none`, non-false, non-zero).
- `#for x in <array> [..]`, `#for (i, x) in <array> [..]` (index+elem),
  `#for (k, v) in <dict> [..]` (key+value) — body evaluated per item.
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
  `#import "lib.typ"` binds `lib` to the dict.
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
- Lists: bullet `•` / `n.` marker at the margin, 0.5em gap, hanging
  indent — continuation lines align at text start.
- Raw inline: Courier, same size. Raw block: Courier at 0.9em, preserving
  internal line breaks, surrounded by block spacing.
- Tables: cell inset 5pt/4pt, atomic-row pagination, 0.5pt borders when
  `stroke` is on (`#grid` defaults off).
- Greedy line breaking at spaces; `justify` distributes slack across
  space widths.

## Encodings

Output text is encoded WinAnsi (PDF base-14 and embedded simple TrueType
alike). Unmappable characters → `?` plus a warning diagnostic (one per
document, not per char).
