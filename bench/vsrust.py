#!/usr/bin/env python3
"""Phase 3 gate: Jig within 10% of Rust on the benchmark suite.

usage: vsrust.py JIG [--runs N] [--instructions] [--only NAME] [--gate] [--out FILE]

Each bench/vsrust/<name>/ holds the same program as main.jig and main.rs. The Jig side is
proved (`jig prove`, so proven checks disappear) and built with `jig build --release`; the
Rust side with `rustc -O`. Both must print the same output. Times are the best of N runs;
`--instructions` counts instructions with Valgrind instead (deterministic, for CI). The
suite's score is the geometric mean of the Jig/Rust ratios; `--gate` fails above 1.10.
"""
import argparse
import json
import math
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
SUITE = os.path.join(HERE, "vsrust")

# name: (timing argument, instruction-count argument)
ARGS = {
    "nbody": ("5000000", "200000"),
    "spectral": ("1000", "200"),
    "fannkuch": ("10", "8"),
    "binarytrees": ("17", "12"),
    "sieve": ("50000000", "2000000"),
    "collatz": ("2000000", "100000"),
    "wordfreq": ("300000", "30000"),
    "sort": ("3000000", "200000"),
}


def run(cmd, **kw):
    r = subprocess.run(cmd, capture_output=True, text=True, **kw)
    if r.returncode != 0:
        sys.exit(f"{' '.join(cmd)} failed:\n{r.stdout}{r.stderr}")
    return r.stdout


def best_time(exe, arg, runs):
    best = float("inf")
    for _ in range(runs):
        t = time.perf_counter()
        subprocess.run([exe, arg], stdout=subprocess.DEVNULL, check=True)
        best = min(best, time.perf_counter() - t)
    return best


def instructions(exe, arg):
    r = subprocess.run(
        ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null", exe, arg],
        capture_output=True,
        text=True,
    )
    m = re.search(r"I\s+refs:\s+([\d,]+)", r.stderr)
    if not m:
        sys.exit(f"valgrind failed on {exe}:\n{r.stderr}")
    return int(m.group(1).replace(",", ""))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("jig")
    ap.add_argument("--runs", type=int, default=5)
    ap.add_argument("--instructions", action="store_true")
    ap.add_argument("--only")
    ap.add_argument("--gate", action="store_true")
    ap.add_argument("--limit", type=float, default=1.10)
    ap.add_argument("--out")
    a = ap.parse_args()
    jig = os.path.abspath(a.jig)
    work = tempfile.mkdtemp(prefix="jig-vsrust-")
    rows = []
    try:
        for name in sorted(ARGS):
            if a.only and a.only != name:
                continue
            src = os.path.join(SUITE, name)
            pkg = os.path.join(work, name)
            shutil.copytree(src, pkg)
            subprocess.run([jig, "prove", pkg], capture_output=True)  # cache proofs; failures are fine
            jexe = os.path.join(work, f"{name}-jig")
            run([jig, "build", pkg, "--release", "-o", jexe], env={**os.environ, "JIG_QUIET": "1"})
            rexe = os.path.join(work, f"{name}-rust")
            run(["rustc", "-O", "--edition", "2021", "-o", rexe, os.path.join(pkg, "main.rs")])
            targ, iarg = ARGS[name]
            arg = iarg if a.instructions else targ
            jout, rout = run([jexe, arg]), run([rexe, arg])
            if jout != rout:
                sys.exit(f"{name}: outputs differ\n--- jig ---\n{jout}--- rust ---\n{rout}")
            if a.instructions:
                j, r = instructions(jexe, arg), instructions(rexe, arg)
            else:
                j, r = best_time(jexe, arg, a.runs), best_time(rexe, arg, a.runs)
            rows.append({"name": name, "jig": j, "rust": r, "ratio": j / r})
            unit = "instr" if a.instructions else "s"
            print(f"{name:12} jig {j:>14.4f} {unit}  rust {r:>14.4f} {unit}  ratio {j / r:5.2f}", flush=True)
    finally:
        shutil.rmtree(work, ignore_errors=True)
    if not rows:
        sys.exit("no benchmarks ran")
    geo = math.exp(sum(math.log(r["ratio"]) for r in rows) / len(rows))
    print(f"geometric mean ratio {geo:.3f} (limit {a.limit})")
    result = {"metric": "instructions" if a.instructions else "seconds", "benchmarks": rows, "geomean": geo, "limit": a.limit}
    if a.out:
        with open(a.out, "w") as f:
            json.dump(result, f, indent=2)
    if a.gate and geo > a.limit:
        sys.exit(f"gate failed: Jig is {geo:.2f}x Rust (limit {a.limit})")


if __name__ == "__main__":
    main()
