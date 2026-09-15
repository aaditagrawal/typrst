mod eval;
mod font;
mod ir;
mod layout;
mod pdf;
mod svg;
mod syntax;

use std::process::ExitCode;
use std::time::Instant;

const USAGE: &str = "\
typrst — a fast, self-contained Typst-subset engine

USAGE:
    typrst build <input.typ> [-o <out.pdf>] [--svg <out.svg>] [--timing]
    typrst bench <input.typ> [-n <iters>]
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
        "bench" => cmd_bench(&args[1..]),
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
    args.iter().enumerate().find(|(i, a)| {
        !a.starts_with('-')
            && !(*i > 0 && matches!(args[*i - 1].as_str(), "-o" | "--svg" | "-n"))
    }).map(|(_, a)| a.as_str())
}

fn report_diags(path: &str, src: &str, diags: &[ir::Diag]) -> bool {
    let mut has_error = false;
    for d in diags {
        if d.severity == ir::Severity::Error {
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

fn cmd_build(args: &[String]) -> ExitCode {
    let Some(path) = positional(args) else {
        eprintln!("error: build needs an input file");
        return ExitCode::FAILURE;
    };
    let timing = args.iter().any(|a| a == "--timing");
    let out = flag_value(args, "-o")
        .map(String::from)
        .unwrap_or_else(|| path.replace(".typ", ".pdf"));
    let t0 = Instant::now();
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error: cannot read {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let t_read = t0.elapsed();

    let t = Instant::now();
    let (nodes, mut diags) = syntax::parse(&src);
    let t_parse = t.elapsed();

    let t = Instant::now();
    let doc = eval::eval(nodes, &mut diags);
    let t_eval = t.elapsed();

    let t = Instant::now();
    let metrics = font::Afm::new();
    let pages = layout::layout(&doc, &metrics);
    let t_layout = t.elapsed();

    let t = Instant::now();
    let pdf_bytes = pdf::write_pdf(&pages);
    let t_pdf = t.elapsed();

    let t = Instant::now();
    let mut io_err = false;
    if let Err(e) = std::fs::write(&out, &pdf_bytes) {
        eprintln!("error: cannot write {out}: {e}");
        io_err = true;
    }
    if let Some(svg_path) = flag_value(args, "--svg") {
        let svg = svg::render(&pages);
        if let Err(e) = std::fs::write(svg_path, svg) {
            eprintln!("error: cannot write {svg_path}: {e}");
            io_err = true;
        }
    }
    let t_write = t.elapsed();

    diags.extend(doc.diags.iter().cloned());
    let has_error = report_diags(path, &src, &diags);

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
    } else {
        eprintln!("rendered in {:?} -> {out}", t0.elapsed());
    }
    if io_err || has_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
    let metrics = font::Afm::new();
    for _ in 0..n {
        let t = Instant::now();
        let (nodes, mut diags) = syntax::parse(&src);
        parse_ts.push(t.elapsed());
        let t = Instant::now();
        let doc = eval::eval(nodes, &mut diags);
        eval_ts.push(t.elapsed());
        let t = Instant::now();
        let pages = layout::layout(&doc, &metrics);
        layout_ts.push(t.elapsed());
        let t = Instant::now();
        let bytes = pdf::write_pdf(&pages);
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
    println!("total   {:>9?}   {:>9?}", p_min + e_min + l_min + w_min, p_med + e_med + l_med + w_med);
    ExitCode::SUCCESS
}
