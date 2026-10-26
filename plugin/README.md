# Jig plugin for Claude Code

The official agent plugin for the Jig language.

- **Skill `jig`:** the language reference, an idiom cookbook and the rules that trip up newcomers.
- **Edit hook:** after every edit to a `.jig` file, `jig check` runs on the project and its
  errors are fed back to the agent.
- **Commit gate:** `git commit` is blocked until the project checks, its tests pass and every
  file is formatted.
- **Skill `jig-proofs`:** how to read `jig prove` counterexamples and add the facts the prover
  needs (requires, callee ensures, loop invariants).
- **Spec-first commands:** `/jig:spec` drafts feature scenarios, `/jig:contracts` drafts
  signatures, contracts and step functions with placeholder bodies, `/jig:implement` fills
  them in until scenarios, tests and proofs pass; each step stops for human approval.
  `/jig:review` writes the report a reviewer reads instead of the code.
- **MCP tools** (`jig mcp`): `check`, `test`, `prove`, `trace`, `run`, `fmt`, `explain`,
  `search` (by type signature) and `reference`.
- **Opt-in trace corpus:** after `jig traces on`, the hooks record each edit (the file and the
  errors it got) and each verified commit in `~/.jig/traces`, for a corpus to train models
  on Jig. Nothing is sent anywhere; `jig traces export FILE` writes what you choose to share,
  and `jig traces off` stops.

## Install

1. Install the `jig` binary on your `PATH`:

   ```
   cargo install --path compiler
   ```

2. In Claude Code:

   ```
   /plugin marketplace add hkd987/Jig
   /plugin install jig@jig
   ```
