# typrst — supported Typst subset

typrst implements a growing subset of Typst markup, flash-tex style:
recognized-but-unimplemented constructs produce a `not implemented`
diagnostic with a recovery note — they are never silently dropped.
Unrecognized constructs produce `error`/`warning` diagnostics and the
parser recovers to the next paragraph boundary.

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
- Term lists `/ term: desc`, tables `$..$` math, `#table`, `#image`,
  `#figure`, `#footnote`, `#grid`, `#stack`, `#box`, `#block`, `#show`,
  `#import`, `#include`, `#context`, numbering patterns, labels `<x>`,
  references `@x` → `not implemented` diagnostics.

## Expressions (`#expr`)

Literals: integers, floats, strings `"..."`, lengths (`pt`, `mm`, `cm`,
`in`, `em`), `true`, `false`, `none`.

- `#set page(paper: "a4" | "a5" | "us-letter", margin: L | (x: L, y: L) | (top: L, right: L, bottom: L, left: L) | (rest: L), flipped: bool)`
- `#set text(size: L, font: "..")` — `font` is accepted; any value maps to
  the base-14 family (mono names → Courier) with a warning diagnostic
- `#set par(justify: bool, leading: L, spacing: L)` — `leading` sets
  baseline-to-baseline = `size + leading` if < size else `leading`
- `#let name = <literal>` — later `#name` substitutes the value;
  `Str`/`Content` render inline, lengths render as text like `12pt`
- `#lorem(n)` — n words of lorem ipsum text
- `#pagebreak()`, `#v(len)`, `#h(len)`, `#linebreak()`
- `#strong[..]`, `#emph[..]`, `#text(size: L, weight: "bold"|int)[..]`
- `#align(center|right|left)[..]` — horizontal alignment of the block
- Unknown `#foo` → `not implemented` diagnostic, recover at end of the
  expression.

## Layout model

- Page: width/height from paper; margins; text block =
  `(left, top)` to `(right, bottom)`; content that overflows starts a
  new page.
- Base style: 11pt Helvetica, line height 1.3 × size, paragraph spacing
  9pt, no justification (ragged right) unless `#set par(justify: true)`.
- Headings: bold, sizes rel. to base — L1 1.6em, L2 1.3em, L3 1.15em,
  L4+ 1.0em; space before = 1.5em of heading size, after = 0.75em.
- Lists: bullet `•` / `n.` marker, marker column 18pt from block edge,
  hanging indent — continuation lines align at text start.
- Raw inline: Courier, same size. Raw block: Courier at 0.9em, preserving
  internal line breaks, surrounded by par spacing.
- Greedy line breaking at spaces; `justify` distributes slack across
  space widths (no stretching beyond 2×, then ragged).

## Encodings

Output text is encoded WinAnsi (PDF base-14). Unmappable characters → `?`
plus a warning diagnostic (one per document, not per char).
