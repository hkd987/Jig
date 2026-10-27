# Changelog

## 0.1.0-alpha

The first public release. Jig is usable for small programs and for trying the
agent-first workflow; the language may still change (see `docs/decisions.md`).

**Language.** Go-shaped syntax with structs, enums with exhaustive `match`, structural
interfaces, simple generics, closures, units of measure, refinement types
(`x: Int where x > 0`), contracts (`requires`, `ensures`, `invariant`, `old`, quantifiers),
declared effects, `Option` and `Result` with `?`, and mutable value semantics with `&mut`
only on parameters. No nil, no zero values, no implicit conversions, no GC.

**Verification.** Contracts and safety (overflow, bounds, division) proved per function by
Z3 against callees' contracts (and, read off their bodies, what they write and, for pure
functions, what they read), cached by content so one edit re-proves one function;
counterexamples are replayed to confirm them. Whatever does not prove stays a runtime
check and becomes a generated property test with shrinking. Gherkin scenarios bind to
typed step functions at compile time and run as tests. Unreachable functions, unread
fields, unused exports and impossible arms are errors, and `jig trace` maps scenarios to
the code they exercise.

**Toolchain.** One command: `check`, `test`, `prove`, `run`, `build`, `trace`, `fmt`, `get`,
`explain`, `search`, `reference`, `mcp`. Structured JSON diagnostics with stable codes.
Native code through C (release builds as one unit at `-O2`), within 10% of Rust on the
shared benchmark suite; a 100k-line project checks in about 70 ms.

**Standard library.** `std/io`, `fs`, `net` (TCP request/response and a simulator),
`time`, `rand`, `os`, `task` (fork-join), `math`, `lists`, `strings`; 91% of its functions
have contracts that prove.

**Agents.** A Claude Code plugin: the reference as a skill, `jig check` after every edit, a
commit gate, a proof-hint skill, spec-first commands and MCP tools. Opt-in, local-only
trace collection.

**Self-hosting.** `jig` is `jigc`, the compiler written in Jig. It compiles itself:
`bootstrap/jigc.c` builds with any C compiler and regenerates itself byte for byte, and
`bootstrap/install.sh` installs it with nothing else. It passes every test suite on its own
(`bootstrap/suites.py`, in CI without Rust), and for the same input each command prints what
the Rust compiler that bootstrapped it prints, which is now frozen as the reference the
tests compare with (D6).

**Not yet.** Agent benchmark results, contracts on most of the compiler (its lexer and nearly
all of its parser prove), and self-hosted speed parity are open (`docs/gates/phase4.md`).
