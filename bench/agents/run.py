#!/usr/bin/env python3
"""Agent benchmark: the same small programming tasks in Jig, Rust and TypeScript.

The PRD's measures: how many tasks an agent brings to a verified build, and how many
iterations (agent turns) it needs, in Jig against Rust and TypeScript.

usage:
  run.py JIG --reference [--lang jig,rust,ts] [--task NAME]
      Build every reference solution and run it on the task's cases (keeps the tasks honest;
      CI runs this).
  run.py JIG --agent 'claude -p --output-format json --max-turns 40' [--lang ...] [--task ...]
      [--out results.json]
      For each task and language, give the agent a fresh workspace with the task, a stub and
      a check script, run it, then score the result on the cases (the workspace's own copy may
      have been edited). Turns are read from the agent's JSON output (`num_turns`) when it
      prints one. With `--transcripts DIR`, each run's output is kept as DIR/TASK-LANG.jsonl
      (give the agent `--output-format stream-json --verbose` to keep every message).

A task is `tasks/NAME/task.md` (what to build: a program reading standard input) and
`tasks/NAME/cases/*.in|.out`. A Jig solution must also pass `jig check` (types, effects,
lints, dead code) to count as a verified build; Rust must compile without warnings, and
TypeScript runs on Node's type stripping.
"""
import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
TASKS = os.path.join(HERE, "tasks")
REFERENCE = os.path.join(HERE, "reference")
LANGS = ["jig", "rust", "ts"]
SOURCE = {"jig": "main.jig", "rust": "main.rs", "ts": "main.ts"}

PROMPT = """Implement the program described in task.md, in {lang}, in the file {src}.
The cases/ directory holds example inputs and expected outputs; ./check.sh builds the program
and runs them. You are done when ./check.sh reports that every case passes."""


def tasks(only):
    names = sorted(d for d in os.listdir(TASKS) if os.path.isdir(os.path.join(TASKS, d)))
    return [n for n in names if not only or n == only]


def cases(task):
    d = os.path.join(TASKS, task, "cases")
    ins = sorted(f for f in os.listdir(d) if f.endswith(".in"))
    return [(open(os.path.join(d, f)).read(), open(os.path.join(d, f[:-3] + ".out")).read()) for f in ins]


def build(jig, lang, ws):
    """Build the program in workspace `ws`; returns (command to run it, error)."""
    if lang == "jig":
        r = subprocess.run([jig, "check", ws], capture_output=True, text=True)
        if r.returncode != 0:
            return None, "jig check failed:\n" + r.stdout + r.stderr
        exe = os.path.join(ws, "prog")
        r = subprocess.run([jig, "build", ws, "--release", "-o", exe], capture_output=True, text=True,
                           env={**os.environ, "JIG_QUIET": "1"})
        if r.returncode != 0:
            return None, r.stdout + r.stderr
        return [exe], None
    if lang == "rust":
        exe = os.path.join(ws, "prog")
        r = subprocess.run(["rustc", "-O", "--edition", "2021", "-D", "warnings", "-o", exe, os.path.join(ws, "main.rs")],
                           capture_output=True, text=True)
        if r.returncode != 0:
            return None, r.stdout + r.stderr
        return [exe], None
    if lang == "ts":
        return ["node", "--experimental-strip-types", "--no-warnings", os.path.join(ws, "main.ts")], None
    raise ValueError(lang)


def score(jig, lang, task, ws):
    """Build and run every case; returns (passed, total, first failure)."""
    cmd, err = build(jig, lang, ws)
    cs = cases(task)
    if cmd is None:
        return 0, len(cs), err
    passed, first = 0, None
    for i, (inp, want) in enumerate(cs, 1):
        try:
            r = subprocess.run(cmd, input=inp, capture_output=True, text=True, timeout=10)
            got = r.stdout
        except subprocess.TimeoutExpired:
            got = "<timeout>"
        if got == want:
            passed += 1
        elif first is None:
            first = f"case {i}: input {inp!r}: expected {want!r}, got {got!r}"
    return passed, len(cs), first


def scaffold(lang, task, ws, jig):
    shutil.copy(os.path.join(TASKS, task, "task.md"), ws)
    shutil.copytree(os.path.join(TASKS, task, "cases"), os.path.join(ws, "cases"))
    if lang == "jig":
        open(os.path.join(ws, "jig.mod"), "w").write(f"module {task}\n")
        open(os.path.join(ws, "main.jig"), "w").write('import "std/io"\n\nfn main() effects {io} {\n}\n')
        build_cmd = f'{jig} check . && {jig} build . -o prog >/dev/null && run="./prog"'
        open(os.path.join(ws, "CLAUDE.md"), "w").write(
            "This project is written in Jig. `jig reference` prints the language reference; read it first.\n"
            "`jig check` reports errors (every lint is an error).\n")
    elif lang == "rust":
        open(os.path.join(ws, "main.rs"), "w").write("fn main() {\n}\n")
        build_cmd = 'rustc -O --edition 2021 -D warnings -o prog main.rs && run="./prog"'
    else:
        open(os.path.join(ws, "main.ts"), "w").write("// Read standard input with: require('fs').readFileSync(0, 'utf8')\n")
        build_cmd = 'run="node --experimental-strip-types --no-warnings main.ts"'
    check = f"""#!/bin/sh
cd "$(dirname "$0")"
{build_cmd} || exit 1
pass=0; fail=0
for f in cases/*.in; do
  if $run < "$f" | cmp -s - "${{f%.in}}.out"; then pass=$((pass+1)); else fail=$((fail+1)); echo "FAIL $f"; fi
done
echo "$pass passed, $fail failed"
[ $fail -eq 0 ]
"""
    path = os.path.join(ws, "check.sh")
    open(path, "w").write(check)
    os.chmod(path, 0o755)


def agent_turns(out):
    """`num_turns` from the agent's JSON output: one object (`--output-format json`), or the
    last line of a stream (`stream-json`)."""
    lines = out.strip().splitlines()
    for text in [out] + lines[-1:]:
        try:
            return json.loads(text).get("num_turns")
        except (ValueError, AttributeError):
            pass
    return None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("jig")
    ap.add_argument("--reference", action="store_true")
    ap.add_argument("--agent")
    ap.add_argument("--lang", default=",".join(LANGS))
    ap.add_argument("--task")
    ap.add_argument("--out")
    ap.add_argument("--transcripts")
    ap.add_argument("--timeout", type=int, default=1800)
    a = ap.parse_args()
    jig = os.path.abspath(a.jig)
    langs = a.lang.split(",")
    rows = []
    failures = 0
    for task in tasks(a.task):
        for lang in langs:
            ws = tempfile.mkdtemp(prefix=f"jig-agents-{task}-{lang}-")
            try:
                turns, secs = None, 0.0
                if a.reference:
                    src = os.path.join(REFERENCE, lang, task)
                    for f in os.listdir(src):
                        shutil.copy(os.path.join(src, f), ws)
                else:
                    scaffold(lang, task, ws, jig)
                    t = time.time()
                    r = subprocess.run(a.agent.split() + [PROMPT.format(lang={"jig": "Jig", "rust": "Rust", "ts": "TypeScript"}[lang], src=SOURCE[lang])],
                                       cwd=ws, capture_output=True, text=True, timeout=a.timeout)
                    secs = time.time() - t
                    if a.transcripts:
                        os.makedirs(a.transcripts, exist_ok=True)
                        with open(os.path.join(a.transcripts, f"{task}-{lang}.jsonl"), "w") as f:
                            f.write(r.stdout)
                    turns = agent_turns(r.stdout)
                passed, total, first = score(jig, lang, task, ws)
                ok = passed == total
                failures += not ok
                rows.append({"task": task, "lang": lang, "passed": passed, "cases": total, "solved": ok,
                             "turns": turns, "seconds": round(secs, 1)})
                extra = "" if ok else f"  {first}"
                print(f"{task:10} {lang:5} {passed}/{total}{'' if turns is None else f'  {turns} turns'}{extra}", flush=True)
            finally:
                shutil.rmtree(ws, ignore_errors=True)
    if not a.reference:
        print()
        for lang in langs:
            rs = [r for r in rows if r["lang"] == lang]
            solved = sum(r["solved"] for r in rs)
            ts = [r["turns"] for r in rs if r["turns"] is not None and r["solved"]]
            avg = f"{sum(ts) / len(ts):.1f}" if ts else "-"
            print(f"{lang:5} solved {solved}/{len(rs)}, mean turns on solved tasks {avg}")
    if a.out:
        json.dump(rows, open(a.out, "w"), indent=2)
    if a.reference and failures:
        sys.exit(f"{failures} reference solution(s) fail their cases")


if __name__ == "__main__":
    main()
