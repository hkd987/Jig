#!/usr/bin/env python3
"""How much of a package `jig prove` proves, counted two ways.

usage: contracts.py JIG [DIR] [--json FILE]

Runs `JIG prove DIR --json` (DIR defaults to selfhost; or reads its output from FILE) and
reports:

- proven contracts (the PRD's measure): functions that state a contract and every one of
  whose obligations proves, traps included;
- safe but for arena indices: functions with no unproven obligation except an index into
  one of the compiler's arenas or tables (a type's kind, an expression, a function, a
  table slot), whether or not they state a contract. Such an index is read out of a tree
  node or a table, so proving it needs an invariant over every node's payload, which
  contracts cannot yet state; it stays checked at run time. The index preconditions some
  helpers state (`requires e >= 0 && e < self.ast.exprs.len()`) count the same way at
  their call sites.

Every other unproven obligation (overflow, conversion, a panic, an index into any other
list) keeps a function out of both counts.
"""
import argparse
import collections
import json
import re
import subprocess
import sys

# The fields that hold an arena or a table indexed by an id another node or table stores.
ARENAS = {
    "kinds", "exprs", "stmts", "types", "pats", "items", "fns", "adts", "ifaces", "consts",
    "aliases", "tests", "locals", "pkgs", "files", "imports", "tables", "expr_ty",
    "expr_res", "pat_ty", "var_ty", "var_kind", "infer_flags", "var_flags", "bodies",
    "alias_state", "blocks", "fields", "variants", "units", "adt_names",
}


def arena_index(ob):
    """Is this unproven obligation an index into an arena or table?"""
    msg = ob["message"]
    if ob["obligation"] == "precondition":
        return re.search(r"requires \w+ >= 0 && \w+ < [\w.\[\]]+\.len\(\)", msg) is not None
    if ob["obligation"] != "bounds":
        return False
    m = re.match(r"index in bounds: (.*)\[[^\[\]]*\]$", msg)
    if not m or "(" in m.group(1):
        return False
    base = re.sub(r"\[[^\]]*\]", "", m.group(1))
    return "." in base and base.split(".")[-1] in ARENAS


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("jig")
    ap.add_argument("dir", nargs="?", default="selfhost")
    ap.add_argument("--json", help="read `jig prove --json` output from this file")
    a = ap.parse_args()
    if a.json:
        lines = open(a.json).read().splitlines()
    else:
        r = subprocess.run([a.jig, "prove", a.dir, "--json"], capture_output=True, text=True)
        if r.returncode not in (0, 1):
            sys.exit(f"jig prove failed:\n{r.stdout}{r.stderr}")
        lines = r.stdout.splitlines()
    failed = collections.defaultdict(list)
    summary = None
    for line in lines:
        d = json.loads(line)
        if d.get("kind") == "summary":
            summary = d
        elif d.get("kind") == "obligation" and d["status"] != "proved":
            failed[d["function"]].append(d)
    if summary is None:
        sys.exit("no summary line in the prover's output")
    total = summary["functions"]
    proved = summary["functions_with_contracts_proved"]
    arena_only = sum(1 for obs in failed.values() if all(arena_index(o) for o in obs))
    safe = total - len(failed) + arena_only
    print(f"functions: {total}")
    print(f"proven contracts: {proved} ({100 * proved / total:.0f}%)"
          f" of {summary['functions_with_contracts']} that state one")
    print(f"safe but for arena indices: {safe} ({100 * safe / total:.0f}%):"
          f" {total - len(failed)} with every obligation proved, {arena_only} whose only"
          f" unproven ones are arena or table indices")
    print(f"other unproven obligations keep {len(failed) - arena_only} functions out")


if __name__ == "__main__":
    main()
