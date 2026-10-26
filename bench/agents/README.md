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
