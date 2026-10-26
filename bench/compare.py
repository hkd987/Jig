#!/usr/bin/env python3
"""Compare two benchmark result files.

usage: compare.py BASE.json HEAD.json [--threshold PCT] [--signoff true|false]

Exits 1 if any benchmark regressed by more than the threshold (default 2%), unless the change
was signed off (a `perf-signoff` label on the pull request).
"""
import json
import sys


def main():
    args = sys.argv[1:]
    if len(args) < 2:
        print(__doc__)
        sys.exit(2)
    base = json.load(open(args[0]))
    head = json.load(open(args[1]))
    threshold = float(args[args.index("--threshold") + 1]) if "--threshold" in args else 2.0
    signoff = "--signoff" in args and args[args.index("--signoff") + 1] == "true"
    if base.get("unit") != head.get("unit"):
        print("units differ; cannot compare", file=sys.stderr)
        sys.exit(2)
    regressions = []
    rows = []
    for name, h in sorted(head["benchmarks"].items()):
        b = base["benchmarks"].get(name)
        if b is None:
            rows.append(f"| {name} | new | {h:,} | |")
            continue
        pct = (h - b) / b * 100 if b else 0.0
        mark = ""
        if pct > threshold:
            regressions.append((name, pct))
            mark = " (regression)"
        rows.append(f"| {name} | {b:,} | {h:,} | {pct:+.2f}%{mark} |")
    unit = head.get("unit")
    print(f"| benchmark | base ({unit}) | head ({unit}) | change |")
    print("| --- | ---: | ---: | ---: |")
    print("\n".join(rows))
    if regressions:
        names = ", ".join(f"{n} {p:+.2f}%" for n, p in regressions)
        if signoff:
            print(f"\nRegressions over {threshold}% were signed off: {names}")
            return
        print(f"\nRegressions over {threshold}%: {names}. Add the `perf-signoff` label to accept them.")
        sys.exit(1)


if __name__ == "__main__":
    main()
