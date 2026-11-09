mod eval;
mod font;
mod fontdb;
mod ir;
mod layout;
mod package;
mod pdf;
mod svg;
mod syntax;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, Instant};

const USAGE: &str = "\
typrst — a fast, self-contained Typst-subset engine

USAGE:
    typrst build <input.typ> [-o <out.pdf>] [--svg <out.svg>] [--timing]
    typrst watch <input.typ> [-o <out.pdf>] [--svg <out.svg>]
    typrst bench <input.typ> [-n <iters>]
    typrst fonts [--verbose]
    typrst supported
    typrst --help
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() || args[0] == "--help" || args[0] == "-h" {
        eprint!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    match args[0].as_str() {
        "build" | "compile" => cmd_build(&args[1..]),
        "watch" => cmd_watch(&args[1..]),
        "bench" => cmd_bench(&args[1..]),
        "fonts" => cmd_fonts(&args[1..]),
        "supported" => {
            print!("{}", include_str!("../SPEC.md"));
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("error: unknown command '{other}'");
            eprint!("{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn flag_value<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(|s| s.as_str())
}

fn positional(args: &[String]) -> Option<&str> {
    // First bare argument that isn't the value of a preceding flag.
    args.iter()
        .enumerate()
        .find(|(i, a)| {
            !a.starts_with('-')
                && !(*i > 0 && matches!(args[*i - 1].as_str(), "-o" | "--svg" | "-n"))
        })
        .map(|(_, a)| a.as_str())
}

fn report_diags(path: &str, src: &str, diags: &[ir::Diag]) -> bool {
    let mut has_error = false;
    for d in diags {
        if matches!(
            d.severity,
            ir::Severity::Error | ir::Severity::NotImplemented
        ) {
            has_error = true;
        }
        match d.span {
            Some(sp) => {
                let (line, col) = ir::line_col(src, sp.start);
                eprintln!("{path}:{line}:{col}: {}: {}", d.severity.label(), d.message);
            }
            None => eprintln!("{path}: {}: {}", d.severity.label(), d.message),
        }
    }
    has_error
}

fn unknown_flag<'a>(args: &'a [String], known: &[&str]) -> Option<&'a str> {
    args.iter()
        .find(|a| a.starts_with('-') && !known.contains(&a.as_str()))
        .map(|s| s.as_str())
}

fn cmd_build(args: &[String]) -> ExitCode {
    let Some(path) = positional(args) else {
        eprintln!("error: build needs an input file");
        return ExitCode::FAILURE;
    };
    if let Some(flag) = unknown_flag(args, &["-o", "--svg", "--timing"]) {
        eprintln!("error: unknown flag '{flag}'");
        return ExitCode::FAILURE;
    }
    let timing = args.iter().any(|a| a == "--timing");
    let out = flag_value(args, "-o")
        .map(String::from)
        .unwrap_or_else(|| path.replace(".typ", ".pdf"));
    build(path, &out, flag_value(args, "--svg"), timing, false, None).code
}

struct BuildResult {
    code: ExitCode,
    written: Option<Vec<u8>>,
}

fn should_write_pdf(previous: Option<&[u8]>, next: &[u8]) -> bool {
    previous != Some(next)
}

/// Parse, eval, layout, PDF, write. Shared by `build` and `watch`.
/// `success_only` prints the rendered line only on success (`watch`).
fn build(
    path: &str,
    out: &str,
    svg_out: Option<&str>,
    timing: bool,
    success_only: bool,
    previous: Option<&[u8]>,
) -> BuildResult {
    // One-shot `build` starts empty; `watch` must not reuse the last compile's imports.
    package::clear_module_cache();
    let fail = || BuildResult { code: ExitCode::FAILURE, written: None };
    let t0 = Instant::now();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return fail();
        }
    };
    let t_read = t0.elapsed();

    let t = Instant::now();
    let (nodes, mut diags) = syntax::parse(&src);
    let t_parse = t.elapsed();

    let t = Instant::now();
    let base_dir = Path::new(path).parent().unwrap_or(Path::new("."));
    let mut fonts: Vec<ir::FontUse> = Vec::new();
    let mut loaded = HashSet::new();
    let db_cell = std::cell::OnceCell::new();
    let mut doc = {
        let mut reg = fontdb::Registry::lazy(&db_cell, &mut fonts);
        let mut env = eval::EvalEnv {
            base_dir,
            registry: &mut reg,
            depth: 0,
            loaded: &mut loaded,
        };
        eval::eval(nodes, &mut diags, &mut env)
    };
    doc.fonts = fonts;
    let t_eval = t.elapsed();

    let t = Instant::now();
    let pages = layout_doc(&doc);
    let t_layout = t.elapsed();

    let t = Instant::now();
    let pdf_bytes = pdf::write_pdf(&pages, &doc.fonts, &doc.images);
    let t_pdf = t.elapsed();

    let t = Instant::now();
    let mut io_err = false;
    let mut written = None;
    if should_write_pdf(previous, &pdf_bytes) {
        if let Err(e) = std::fs::write(out, &pdf_bytes) {
            eprintln!("error: cannot write {out}: {e}");
            io_err = true;
        } else {
            written = Some(pdf_bytes);
        }
    }
    if let Some(svg_path) = svg_out {
        let svg = svg::render(&pages, &doc.fonts);
        if let Err(e) = std::fs::write(svg_path, svg) {
            eprintln!("error: cannot write {svg_path}: {e}");
            io_err = true;
        }
    }
    let t_write = t.elapsed();

    diags.extend(doc.diags.iter().cloned());
    let has_error = report_diags(path, &src, &diags);
    let ok = !io_err && !has_error;

    if timing {
        eprintln!(
            "timing: read {:?} | parse {:?} | eval {:?} | layout {:?} | pdf {:?} | write {:?} | total {:?} | pages {}",
            t_read,
            t_parse,
            t_eval,
            t_layout,
            t_pdf,
            t_write,
            t0.elapsed(),
            pages.len()
        );
    } else if ok || !success_only {
        eprintln!("rendered in {:?} -> {out}", t0.elapsed());
    }
    BuildResult {
        code: if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE },
        written,
    }
}

/// Base-14 documents skip `DbMetrics` so the line breaker monomorphizes
/// straight onto the AFM tables. Embedded faces still go through the db.
fn layout_doc(doc: &ir::Document) -> Vec<ir::Page> {
    if doc.fonts.is_empty() {
        layout::layout(doc, &font::Afm)
    } else {
        let metrics = font::DbMetrics::new(&doc.fonts);
        layout::layout(doc, &metrics)
    }
}

fn cmd_watch(args: &[String]) -> ExitCode {
    let Some(path) = positional(args) else {
        eprintln!("error: watch needs an input file");
        return ExitCode::FAILURE;
    };
    if let Some(flag) = unknown_flag(args, &["-o", "--svg"]) {
        eprintln!("error: unknown flag '{flag}'");
        return ExitCode::FAILURE;
    }
    let out = flag_value(args, "-o")
        .map(String::from)
        .unwrap_or_else(|| path.replace(".typ", ".pdf"));
    let svg = flag_value(args, "--svg").map(String::from);
    let input = Path::new(path);
    let mut last_pdf = None;
    let mut deps = watch_deps(input);
    let mut seen = watch_seen(input, &deps);
    loop {
        let result = build(path, &out, svg.as_deref(), false, true, last_pdf.as_deref());
        if let Some(pdf) = result.written {
            last_pdf = Some(pdf);
        }
        // A blocking 200ms sleep: Ctrl-C kills the process, which is acceptable.
        loop {
            std::thread::sleep(Duration::from_millis(200));
            if watch_seen(input, &deps) != seen {
                break;
            }
        }
        deps = watch_deps(input);
        seen = watch_seen(input, &deps);
    }
}

fn stamp(path: &Path) -> (u64, Option<std::time::SystemTime>) {
    match std::fs::metadata(path) {
        Ok(m) => (m.len(), m.modified().ok()),
        Err(_) => (0, None),
    }
}

fn watch_seen(input: &Path, deps: &[PathBuf]) -> Vec<(u64, Option<std::time::SystemTime>)> {
    std::iter::once(stamp(input)).chain(deps.iter().map(|p| stamp(p))).collect()
}

/// `#import` / `#include` paths quoted in `input`, resolved next to that file.
fn watch_deps(input: &Path) -> Vec<PathBuf> {
    let Ok(src) = std::fs::read_to_string(input) else {
        return Vec::new();
    };
    let base = input.parent().unwrap_or(Path::new("."));
    let mut out = Vec::new();
    for (i, _) in src.match_indices('#') {
        let s = &src[i..];
        let n = if s.starts_with("#import") {
            7
        } else if s.starts_with("#include") {
            8
        } else {
            continue;
        };
        if s.as_bytes().get(n).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
            continue;
        }
        let Some(rel) = s[n..].split('"').nth(1).filter(|p| {
            !p.is_empty() && !p.starts_with('@') && !p.contains('\n')
        }) else {
            continue;
        };
        let path = base.join(rel);
        if !out.contains(&path) {
            out.push(path);
        }
    }
    out
}

fn cmd_bench(args: &[String]) -> ExitCode {
    let Some(path) = positional(args) else {
        eprintln!("error: bench needs an input file");
        return ExitCode::FAILURE;
    };
    let n: usize = flag_value(args, "-n")
        .and_then(|s| s.parse().ok())
        .unwrap_or(50);
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut parse_ts = Vec::new();
    let mut eval_ts = Vec::new();
    let mut layout_ts = Vec::new();
    let mut pdf_ts = Vec::new();
    let base_dir = Path::new(path).parent().unwrap_or(Path::new("."));
    let db_cell = std::cell::OnceCell::new();
    for _ in 0..n {
        let t = Instant::now();
        let (nodes, mut diags) = syntax::parse(&src);
        parse_ts.push(t.elapsed());
        let t = Instant::now();
        let mut fonts: Vec<ir::FontUse> = Vec::new();
        let mut loaded = HashSet::new();
        let mut doc = {
            let mut reg = fontdb::Registry::lazy(&db_cell, &mut fonts);
            let mut env = eval::EvalEnv {
                base_dir,
                registry: &mut reg,
                depth: 0,
                loaded: &mut loaded,
            };
            eval::eval(nodes, &mut diags, &mut env)
        };
        doc.fonts = fonts;
        eval_ts.push(t.elapsed());
        let t = Instant::now();
        let pages = layout_doc(&doc);
        layout_ts.push(t.elapsed());
        let t = Instant::now();
        let bytes = pdf::write_pdf(&pages, &doc.fonts, &doc.images);
        pdf_ts.push(t.elapsed());
        std::hint::black_box(bytes);
    }
    let stats = |v: &mut Vec<std::time::Duration>| {
        v.sort();
        (v[0], v[v.len() / 2])
    };
    let (p_min, p_med) = stats(&mut parse_ts);
    let (e_min, e_med) = stats(&mut eval_ts);
    let (l_min, l_med) = stats(&mut layout_ts);
    let (w_min, w_med) = stats(&mut pdf_ts);
    println!("{n} iterations of {path} ({} bytes)", src.len());
    println!("stage        min       median");
    println!("parse   {:>9?}   {:>9?}", p_min, p_med);
    println!("eval    {:>9?}   {:>9?}", e_min, e_med);
    println!("layout  {:>9?}   {:>9?}", l_min, l_med);
    println!("pdf     {:>9?}   {:>9?}", w_min, w_med);
    println!(
        "total   {:>9?}   {:>9?}",
        p_min + e_min + l_min + w_min,
        p_med + e_med + l_med + w_med
    );
    ExitCode::SUCCESS
}

fn cmd_fonts(args: &[String]) -> ExitCode {
    let t = Instant::now();
    let db = fontdb::FontDb::discover();
    let dt = t.elapsed();
    let verbose = args.iter().any(|a| a == "--verbose" || a == "-v");
    if verbose {
        for f in &db.faces {
            println!(
                "{:<40} {:<20} w={:<3}{}{} {}",
                f.family,
                f.subfamily,
                f.weight,
                if f.italic { " italic" } else { "" },
                if f.monospace { " mono" } else { "" },
                f.path.display()
            );
        }
    } else {
        let mut fams: Vec<&str> = db.faces.iter().map(|f| f.family.as_str()).collect();
        fams.sort_unstable();
        fams.dedup();
        for f in fams {
            println!("{f}");
        }
    }
    eprintln!("{} faces indexed in {:?}", db.len(), dt);
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::report_diags;
    use crate::ir::{Diag, Severity, Span};

    #[test]
    fn not_implemented_fails_the_build() {
        let src = "#show heading: it => it\nHello";
        let failed = [Diag::new(
            Severity::NotImplemented,
            Some(Span::new(0, 5)),
            "#show is not implemented",
        )];
        assert!(report_diags("t.typ", src, &failed));

        // Eval warnings carry no span; that stays a successful build.
        let warned = [Diag::new(Severity::Warning, None, "undefined variable")];
        assert!(!report_diags("t.typ", "hello", &warned));

        let errored = [Diag::new(Severity::Error, Some(Span::new(0, 1)), "bad")];
        assert!(report_diags("t.typ", "x", &errored));

        let unspanned = [Diag::new(
            Severity::NotImplemented,
            None,
            "function '#foo' is not implemented",
        )];
        assert!(report_diags("t.typ", "#foo()", &unspanned));
    }

    #[test]
    fn identical_pdf_bytes_skip_write() {
        let last = [1u8, 2, 3, 4];
        assert!(!super::should_write_pdf(Some(&last), &last));
        assert!(super::should_write_pdf(Some(&last), &[1, 2, 3]));
        assert!(super::should_write_pdf(None, &last));
    }
}
