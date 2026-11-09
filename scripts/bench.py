#!/usr/bin/env python3
# Pass/fail 10x gate versus typst: scripts/speed_gate.py
"""Benchmark typrst vs typst: process wall-clock, median + best of N runs."""
import subprocess, sys, time, statistics, tempfile, os

def bench(cmd, n):
    times = []
    for _ in range(n):
        t = time.perf_counter()
        subprocess.run(cmd, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        times.append((time.perf_counter() - t) * 1000)
    times.sort()
    return times[0], statistics.median(times)

def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 15
    fixtures = sys.argv[2:] or ["fixtures/small.typ", "fixtures/large.typ"]
    typrst = "./target/release/typrst"
    print(f"{'document':<22} {'typst compile':>22} {'typrst build':>22} {'speedup':>8}")
    for f in fixtures:
        name = os.path.basename(f)
        with tempfile.TemporaryDirectory() as td:
            t_best, t_med = bench(["typst", "compile", f, f"{td}/t.pdf"], n)
            r_best, r_med = bench([typrst, "build", f, "-o", f"{td}/r.pdf"], n)
            print(f"{name:<22} {t_med:>9.1f}ms ({t_best:.1f}) {r_med:>9.1f}ms ({r_best:.1f}) {t_med/r_med:>7.1f}x")
    print("\n(columns: median (best), ms; speedup = typst_median / typrst_median)")

if __name__ == "__main__":
    main()
