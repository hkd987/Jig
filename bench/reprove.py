#!/usr/bin/env python3
"""Phase 2 gate: re-proof under 1 s after a one-function change.

usage: reprove.py JIG [--packages N] [--units M] [--limit SECONDS] [--out FILE]

Generates the benchmark corpus, proves it once (filling the proof cache), changes the body of
one function in one package, and proves again. The second run must finish within the limit
(wall clock, including analysis) and must re-prove exactly the changed function.
"""
import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))


def prove(jig, app):
    t = time.monotonic()
    r = subprocess.run([jig, "prove", app, "--json"], capture_output=True, text=True)
    dt = time.monotonic() - t
    if r.returncode not in (0, 1):
        sys.exit(f"jig prove failed:\n{r.stdout}{r.stderr}")
    summary = json.loads(r.stdout.strip().splitlines()[-1])
    return dt, summary


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("jig")
    ap.add_argument("--packages", type=int, default=20)
    ap.add_argument("--units", type=int, default=20)
    ap.add_argument("--limit", type=float, default=1.0)
    ap.add_argument("--out")
    a = ap.parse_args()
    jig = os.path.abspath(a.jig)
    work = tempfile.mkdtemp(prefix="jig-reprove-")
    try:
        subprocess.run(
            [sys.executable, os.path.join(HERE, "gen.py"), work, "--packages", str(a.packages), "--units", str(a.units)],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        app = os.path.join(work, "app")
        cold, s1 = prove(jig, app)
        # Change one function body (not its signature): `total` of the first unit of p0.
        path = next(
            os.path.join(root, f) for root, _, fs in os.walk(os.path.join(work, "p0")) for f in fs if f.endswith(".jig")
        )
        src = open(path).read()
        changed, n = re.subn(r"(fn S0\.total\(self\) -> Int \{\n  return )", r"\g<1>0 + ", src, count=1)
        if n != 1:
            sys.exit("could not find the function to change")
        open(path, "w").write(changed)
        warm, s2 = prove(jig, app)
        redone = s2["functions"] - s2.get("cached", s2["functions"])
        result = {
            "functions": s1["functions"],
            "obligations": s1["contracts"] + s1["safety"],
            "cold_seconds": round(cold, 3),
            "reprove_seconds": round(warm, 3),
            "limit_seconds": a.limit,
            "reproved_functions": redone,
            "pass": warm < a.limit and redone == 1,
        }
        print(json.dumps(result, indent=2))
        if a.out:
            with open(a.out, "w") as f:
                json.dump(result, f, indent=2)
        if not result["pass"]:
            sys.exit(f"gate failed: re-proof took {warm:.3f} s (limit {a.limit} s), re-proved {redone} functions")
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == "__main__":
    main()
