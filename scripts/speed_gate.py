#!/usr/bin/env python3
"""Fail unless typrst stays at least 10x faster than typst on both fixtures.

Same measurement as scripts/bench.py: each sample is a fresh process, and
the score is the median wall-clock. This script does not build anything.

Exit 0 when both speedups are >= 10. Exit 1 when a ratio is below 10 (both
ratios are printed). Exit 2 when typst or the typrst binary is missing.
"""
import argparse, os, shutil, statistics, subprocess, sys, tempfile, time

THRESHOLD = 10.0
FIXTURES = ["fixtures/small.typ", "fixtures/large.typ"]


def bench(cmd, n):
    # Fresh process per run, median wall-clock — same method as scripts/bench.py.
    times = []
    failed = False
    for _ in range(n):
        t = time.perf_counter()
        result = subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        times.append((time.perf_counter() - t) * 1000)
        if result.returncode != 0:
            failed = True
    times.sort()
    return statistics.median(times), failed


def executable(path):
    return bool(path) and os.path.isfile(path) and os.access(path, os.X_OK)


def locate_typst(explicit):
    if explicit:
        if executable(explicit):
            return explicit
        if os.sep not in explicit:
            found = shutil.which(explicit)
            if executable(found):
                return found
        return None
    found = shutil.which("typst")
    return found if executable(found) else None


def main():
    parser = argparse.ArgumentParser(
        description="Require a 10x wall-clock speedup versus typst on the release fixtures."
    )
    parser.add_argument(
        "-n", "--runs", type=int, default=8,
        help="fresh-process runs per tool per fixture (default: 8)",
    )
    parser.add_argument(
        "--typrst", default="./target/release/typrst",
        help="typrst binary (default: ./target/release/typrst)",
    )
    parser.add_argument(
        "--typst", default=None,
        help="typst binary (default: search PATH)",
    )
    args = parser.parse_args()
    if args.runs < 1:
        print("speed gate: --runs must be at least 1", file=sys.stderr)
        return 2

    typrst = args.typrst
    typst = locate_typst(args.typst)
    missing = []
    if not executable(typrst):
        missing.append(
            f"typrst binary not found at {typrst} "
            "(build with cargo build --release, or pass --typrst)"
        )
    if typst is None:
        where = f"at {args.typst}" if args.typst else "on PATH"
        missing.append(
            f"typst binary not found {where} "
            "(install typst 0.15.1, or pass --typst)"
        )
    for fixture in FIXTURES:
        if not os.path.isfile(fixture):
            missing.append(f"fixture not found: {fixture} (run from the repo root)")
    if missing:
        for line in missing:
            print(f"speed gate: {line}", file=sys.stderr)
        return 2

    parts = []
    ratios = []
    any_failed = False
    for fixture in FIXTURES:
        name = os.path.basename(fixture)
        with tempfile.TemporaryDirectory() as td:
            typst_ms, typst_failed = bench(
                [typst, "compile", fixture, f"{td}/t.pdf"], args.runs
            )
            typrst_ms, typrst_failed = bench(
                [typrst, "build", fixture, "-o", f"{td}/r.pdf"], args.runs
            )
        any_failed = any_failed or typst_failed or typrst_failed
        speedup = (typst_ms / typrst_ms) if typrst_ms else float("inf")
        ratios.append((name, speedup))
        parts.append(f"{name} {speedup:.1f}x ({typst_ms:.1f}ms / {typrst_ms:.1f}ms)")

    ok = (not any_failed) and all(speedup >= THRESHOLD for _, speedup in ratios)
    verdict = "pass" if ok else "fail"
    extra = "; a compile exited non-zero" if any_failed else ""
    print(
        f"speed gate: {', '.join(parts)}: {verdict} "
        f"(need >= {THRESHOLD:.0f}x vs typst 0.15.1{extra})"
    )
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
