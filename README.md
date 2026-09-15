# typrst

A blazing fast Typst-subset engine written in Rust — the flash-tex approach
applied to Typst: a ground-up rewrite that trades feature completeness for
speed, with a self-contained binary, no font scanning, and its own PDF
writer.

```sh
typrst build main.typ -o main.pdf     # compile to PDF
typrst build main.typ --timing        # per-stage timing breakdown
typrst bench main.typ -n 100          # in-process micro-benchmark
typrst supported                      # print the supported subset
```

## What it is

A complete rewrite of the Typst compilation pipeline from scratch:

- **Parser** (`src/syntax.rs`) — single-pass recovering parser producing an
  AST (`ir::Node`). Recognized-but-unimplemented constructs produce
  `not implemented` diagnostics with spans; nothing is silently dropped.
- **Evaluator** (`src/eval.rs`) — resolves `#set`/`#let`/function calls into
  a styled block model (`ir::Block`/`ir::Inline`).
- **Layout** (`src/layout.rs`) — greedy line breaking, justification, lists,
  headings, pagination → a flat display list (`ir::Page`/`ir::DrawItem`).
- **Export** (`src/pdf.rs`, `src/svg.rs`) — a minimal PDF 1.4 writer using
  the base-14 fonts (no font embedding, no font files, no fontdb — this is
  where the startup advantage over typst comes from) plus an SVG debug route.
- **Metrics** (`src/font.rs`) — embedded AFM width tables for the base-14
  faces. Zero I/O.

## What it is not (yet)

A full Typst engine. The supported subset is defined in
[SPEC.md](SPEC.md) — run `typrst supported`. Constructs outside the subset
(math, tables, show rules, packages, …) are reported as diagnostics.

## Benchmarks

Measured 2026-09-14 on an Apple M4 (16 GB), macOS 27.0, `typrst` built with
`cargo build --release` (rustc 1.96.0, `lto=fat`, `codegen-units=1`,
`target-cpu=native`). The comparison oracle is `typst` 0.15.1 — the fastest
existing Typst compiler — run as a fresh `typst compile` process per
iteration. Median of 20 runs, best in parentheses; both binaries write PDF.

| Document | `typst compile` wall | `typrst build` wall | speedup |
|---|---:|---:|---:|
| `fixtures/small.typ` (372 B, 1 page) | 51.9 ms (50.2) | **2.1 ms (2.0)** | **24.8x** |
| `fixtures/large.typ` (80 KB, ~30 pages) | 85.5 ms (84.3) | **3.8 ms (3.5)** | **22.7x** |

In-process pipeline (`typrst bench`, median, excludes process startup):

| Document | parse | eval | layout | pdf | total |
|---|---:|---:|---:|---:|---:|
| `small.typ` | 1.6 µs | 0.9 µs | 2.7 µs | 1.5 µs | **6.6 µs** |
| `large.typ` | 475 µs | 156 µs | 674 µs | 199 µs | **1.5 ms** |

`typst`'s wall clock is dominated by fixed startup (fontdb system-font scan,
general machinery), not per-document work; `typrst` does zero font I/O
(base-14 AFM metrics are embedded as static tables), so its wall clock is
nearly all exec + I/O overhead — the ~1.5–2 ms macOS process floor.

Both compilers render the same source; page counts differ (typst 25 vs
typrst 30 on `large.typ`) because typrst uses Helvetica base-14 metrics
rather than Linux Libertine — same trade-off flash-tex makes vs pdflatex.

Exact commands: `python3 scripts/bench.py 20` for the wall-clock table;
`./target/release/typrst bench fixtures/large.typ -n 50` for in-process.

## Design notes

- Zero external dependencies. `cargo build --release` in about a second.
- Compile cost is dominated by process startup, not compilation: the
  pipeline itself runs in well under a millisecond for typical documents.
- The `typst` CLI spends most of its wall-clock on font discovery
  (`fontdb` scans system fonts) and general-purpose machinery; typrst
  skips all of it for the subset.
