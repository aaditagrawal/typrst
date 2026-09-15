# typrst

typrst is a blazing fast Typst engine written in Rust — a complete rewrite
of the Typst compilation pipeline from scratch, in the spirit of
[flash-tex](https://github.com/flash-tex/flashtex): hand-rolled lexer and
parser, its own layout engine, its own PDF writer, parallel font indexing,
package resolution — no interpreter, no comemo.

```sh
typrst build main.typ -o main.pdf     # compile to PDF (also: --svg out.svg)
typrst build main.typ --timing        # per-stage timing breakdown
typrst bench main.typ -n 100          # in-process micro-benchmark
typrst fonts                          # list the indexed font families
typrst supported                      # print the supported subset inventory
```

`typrst build` lexes, lays out and paints a document in microseconds, writes
the PDF with its own bundled writer (base-14 fonts *and* embedded TrueType
for any system family), resolves `@preview` packages, and emits
`file:line:col` diagnostics for anything outside the supported subset —
recognized-but-unimplemented constructs are reported with a recovery note,
never silently dropped.

What it is **not** (yet): a full Typst engine. typrst implements a growing
subset of Typst (headings, paragraphs, `*strong*`/`_emph_`, lists, raw code,
tables, `#if`/`#for`, `#import`/`#include`, `#set page`/`#set text`/`#set
par`, `#let`, `#lorem`, `#align`, `#v`/`#h`, `#pagebreak`, …); constructs it
does not implement — math, show rules, `#let` functions — are reported as
diagnostics. `typrst supported` prints the exact inventory; see
[SPEC.md](SPEC.md).

## Features

- **The `typrst` CLI** — `build` (PDF + optional SVG), `bench` (per-stage
  in-process timing), `fonts` (dump the discovered index), `supported`
  (subset inventory), `compile` alias. `file:line:col` diagnostics,
  build-tool exit codes.
- **Parallel font discovery & indexing** — `FontDb::discover` walks the
  platform font directories across `available_parallelism` threads, parses
  sfnt headers (`name`/`OS/2`/`head`/`hhea`/`post`, TTC collections
  included), and indexes ~800+ faces by family/weight/style. The index is
  cached on disk (`~/.cache/typrst/fontdb-v1.bin`) and validated against
  directory mtimes — a warm load is ~1 ms, and discovery is lazy so
  base-14-only documents never scan at all.
- **Real font embedding** — `#set text(font: "Menlo")` resolves through
  the index to an actual face: layout uses the font's own cmap+hmtx
  metrics and the PDF writer embeds the TrueType data (`/FontFile2`,
  `/Widths`, descriptor), extracting faces from `.ttc` collections when
  needed. `pdffonts` confirms `emb=yes`.
- **Package resolution** — `#import "@preview/pkg:1.2.3"` searches the
  shared typst package dirs first (already-fetched packages work), then
  downloads from `packages.typst.org` into `~/.cache/typrst/packages`.
  `typst.toml` entrypoints, named/wildcard/stem imports, cycle detection.
- **A real evaluator** — `#let` bindings with arithmetic, dicts and
  arrays, `mod.field` access, `#if`/`else if`/`else`, `#for x in …`
  loops, `#include`, and a scope-carrying module system.
- **Recovering parser** — single-pass byte-cursor markup parser producing
  spans on every diagnostic; malformed input never panics.
- **Its own layout engine** — greedy line breaking with optional
  justification, hanging-indent lists, headings, stroked tables with
  auto/fr/fixed columns and atomic-row pagination — emitting a flat
  display list.
- **Its own PDF writer** — minimal PDF 1.4, WinAnsi text, only the faces
  actually used, deterministic output (no timestamps or IDs).
- **Zero dependencies** — `cargo build --release` finishes in seconds;
  the binary is ~450 KB and needs nothing but the source file.

## Benchmarks

Measured on 2026-09-15 on an Apple M4 (16 GB), macOS 27.0, `typrst` built
with `cargo build --release` (rustc 1.96.0, `lto=fat`, `codegen-units=1`,
`target-cpu=native`). The comparison oracle is `typst` 0.15.1 — the fastest
existing Typst compiler — run as a fresh `typst compile` process per
iteration. Median of 15 runs, best in parentheses; both binaries write PDF.

**Full render, fresh process, real documents.**

| Document | `typst compile` wall | `typrst build` wall | speedup |
|---|---:|---:|---:|
| `fixtures/small.typ` (372 B, 1 page) | 56.2 ms (54.8) | **2.4 ms (2.2)** | **23.8x** |
| `fixtures/large.typ` (80 KB, 25 pages) | 92.0 ms (89.8) | **4.4 ms (4.0)** | **20.9x** |

**In-process pipeline** (`typrst bench`, median, excludes process startup):

| Document | parse | eval | layout | pdf | total |
|---|---:|---:|---:|---:|---:|
| `small.typ` | 1.6 µs | 0.9 µs | 2.7 µs | 1.5 µs | **6.6 µs** |
| `large.typ` | 392 µs | 133 µs | 563 µs | 164 µs | **1.3 ms** |

`typst`'s wall clock is dominated by fixed startup — the fontdb
system-font scan and general machinery — not per-document work. typrst's
parallel index + disk cache makes the same work ~1 ms warm, deferred
entirely when a document only uses base-14 fonts, so its wall clock is
nearly all exec + I/O overhead: the ~1.5–2 ms macOS process floor.

<details>
<summary>Exact commands</summary>

```sh
cargo build --release
python3 scripts/bench.py 20            # wall-clock table (typst must be on PATH)
./target/release/typrst bench fixtures/large.typ -n 50   # in-process stages
./target/release/typrst build fixtures/small.typ -o /tmp/s.pdf --timing
./target/release/typrst fonts -v       # the parallel font index
```

</details>

## Visual fidelity

typrst uses the PDF base-14 **Times** faces by default — width tables
extracted from the system fonts, metric-compatible with the Adobe AFMs —
to match typst's Libertinus Serif default, and its spacing model was
fitted against typst's actual PDF content streams:

- Same page count on `large.typ`: **25 pages from both compilers**
- Baselines match typst's to within ~0.05 pt at block boundaries
- Line height 14.39 pt @ 11 pt body; block gap 6.05 pt; heading sizes
  1.4/1.2/1.05/1.0 em with tight leading; list markers hung at the margin
  with a 0.5 em gap — all measured, not guessed
- `#set text(font: "…")` swaps in any discovered system face with its
  real metrics and embeds it — closer than any base-14 stand-in
- Remaining default-stack differences are intrinsic: Libertinus vs Times
  glyph shapes and widths shift individual line breaks (±one line,
  resyncing at each block)

## Architecture

```
source ──▶ syntax::parse ──▶ Vec<Node>          recovering markup parser
        ──▶ eval::eval    ──▶ Document          set rules, scopes, imports
        │                    ▲                  │
        │              package::resolve ──▶ @preview/local/registry dirs
        │              fontdb::Registry ──▶ parallel index + disk cache
        ──▶ layout::layout──▶ Vec<Page>         line breaking + pagination
        ──▶ pdf::write_pdf──▶ Vec<u8>           display list -> PDF bytes
        ──▶ svg::render   ──▶ String            debug/preview route
```

Everything passes through the types in [`src/ir.rs`](src/ir.rs) — the fixed
contract between stages. `src/font.rs` carries the base-14 metric tables;
`src/fontdb.rs` the discovered-face index and per-face metrics.

## Installation

```sh
cargo install --path .
typrst build main.typ
```

## Development

```sh
cargo test            # 73 unit tests across all stages
cargo build --release
```

Fixture documents live in `fixtures/` and are valid input to both `typst`
and `typrst`, which is what makes the benchmark comparison honest.

## License

MIT
