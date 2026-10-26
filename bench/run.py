#!/usr/bin/env python3
"""Measure the compiler by instruction count (stable, unlike wall time), as rustc-perf does.

usage: run.py JIG_BINARY [--out results.json] [--quick]

Benchmarks run under `valgrind --tool=cachegrind --cache-sim=no` and report `I refs`.
Without valgrind (or with --wall), wall-clock milliseconds are reported instead, for local use.
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def corpus(quick):
    """Generate the benchmark corpus (deterministic) into a cache directory."""
    name = "gen20k" if quick else "gen100k"
    units = "5" if quick else "22"
    out = os.path.join(tempfile.gettempdir(), "jig-bench-" + name)
    marker = os.path.join(out, ".done")
    if not os.path.exists(marker):
        shutil.rmtree(out, ignore_errors=True)
        subprocess.run([sys.executable, os.path.join(HERE, "gen.py"), out, "--units", units], check=True, stdout=subprocess.DEVNULL)
        open(marker, "w").close()
    return out


def measure(cmd, use_valgrind):
    if use_valgrind:
        full = ["valgrind", "--tool=cachegrind", "--cache-sim=no", "--cachegrind-out-file=/dev/null"] + cmd
        p = subprocess.run(full, capture_output=True, text=True)
        m = re.search(r"I\s+refs:\s+([\d,]+)", p.stderr)
        if not m:
            raise RuntimeError("no instruction count in valgrind output:\n" + p.stderr[-2000:])
        return int(m.group(1).replace(",", ""))
    best = None
    for _ in range(5):
        t = time.perf_counter()
        subprocess.run(cmd, capture_output=True)
        dt = (time.perf_counter() - t) * 1000
        best = dt if best is None else min(best, dt)
    return round(best, 2)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        sys.exit(2)
    jig = os.path.abspath(args[0])
    quick = "--quick" in args
    use_valgrind = shutil.which("valgrind") is not None and "--wall" not in args
    out_path = args[args.index("--out") + 1] if "--out" in args else None
    gen = corpus(quick)
    run_dir = os.path.join(ROOT, "compiler", "tests", "run")
    benches = {
        "check-corpus": [jig, "check", os.path.join(gen, "app")],
        "fmt-corpus": [jig, "fmt", "--check", gen],
        "run-corpus": [jig, "run", os.path.join(gen, "app")],
        "check-reference-tour": [jig, "check", os.path.join(ROOT, "compiler", "tests", "fixtures")],
    }
    results = {"unit": "instructions" if use_valgrind else "ms", "benchmarks": {}}
    for name, cmd in benches.items():
        results["benchmarks"][name] = measure(cmd, use_valgrind)
        print(f"{name:24} {results['benchmarks'][name]:>16,}", file=sys.stderr)
    text = json.dumps(results, indent=2, sort_keys=True)
    if out_path:
        with open(out_path, "w") as f:
            f.write(text + "\n")
    else:
        print(text)
    _ = run_dir


if __name__ == "__main__":
    main()
