# Jig plugin for Claude Code

The official agent plugin for the Jig language.

- **Skill `jig`:** the language reference, an idiom cookbook and the rules that trip up newcomers.
- **Edit hook:** after every edit to a `.jig` file, `jig check` runs on the project and its
  errors are fed back to the agent.
- **Commit gate:** `git commit` is blocked until the project checks, its tests pass and every
  file is formatted.
- **Skill `jig-proofs`:** how to read `jig prove` counterexamples and add the facts the prover
  needs (requires, callee ensures, loop invariants).
- **MCP tools** (`jig mcp`): `check`, `test`, `prove`, `trace`, `run`, `fmt`, `explain`,
  `search` (by type signature) and `reference`.

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
