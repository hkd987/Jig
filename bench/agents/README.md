# Agent benchmark

The PRD measures Jig by what agents do with it: how many tasks reach a verified build, and
how many iterations they take, against the same tasks in Rust and TypeScript.

Each task in `tasks/` is a small command-line program specified once (`task.md`) with
acceptance cases (`cases/*.in` and `*.out`): word frequencies, an RPN calculator, interval
merging, Roman numerals, an account ledger and a maze. `reference/<lang>/<task>/` holds a
solution in each language; CI runs them all on the cases, so the tasks stay correct:

```
python3 bench/agents/run.py target/release/jig --reference
```

To benchmark an agent, give its command line; it runs once per task and language in a fresh
workspace holding the task, the cases, a stub and `check.sh`, and is scored on the original
cases afterwards. A Jig solution must also pass `jig check`; Rust must compile without
warnings.

```
python3 bench/agents/run.py target/release/jig \
  --agent 'claude -p --output-format json --max-turns 40 --dangerously-skip-permissions' \
  --out results.json
```

The summary prints, per language, the tasks solved and the mean number of agent turns on the
solved ones (read from the agent's JSON `num_turns`). Run it in a sandbox: the agent edits
files and runs commands in its workspace.

Where skipping permissions is not allowed, give every language the same short list of tools
instead: editing files, and running the check script, the program, the three toolchains and
a few commands that only read. No pattern may contain a space (the command is split on
spaces), and the final `--` ends the variadic `--allowedTools` before the prompt:

```
python3 bench/agents/run.py ~/.local/bin/jig \
  --agent 'claude -p --output-format json --max-turns 40 --permission-mode acceptEdits --allowedTools Read Edit Write Glob Grep Bash(./check.sh:*) Bash(./prog:*) Bash(jig:*) Bash(rustc:*) Bash(node:*) Bash(cat:*) Bash(ls:*) Bash(head:*) Bash(diff:*) Bash(cmp:*) Bash(echo:*) --' \
  --lang jig --out jig.json
```

Each language can run as its own process (`--lang jig`, `rust`, `ts`) at the same time.
To see where the turns go, give the agent `--output-format stream-json --verbose` and
`run.py --transcripts DIR`, which keeps each run's messages as `DIR/TASK-LANG.jsonl`.
`docs/gates/phase4.md` has the first runs, one per task and language, and
`docs/gates/phase5.md` three per task and language (`results/`).
