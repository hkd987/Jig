# Changelog

## 0.1.0-alpha

The first public release. Jig is usable for small programs and for trying the
agent-first workflow; the language may still change (see `docs/decisions.md`).

**Language.** Go-shaped syntax with structs, enums with exhaustive `match`, structural
interfaces, simple generics, closures, units of measure, refinement types
(`x: Int where x > 0`), contracts (`requires`, `ensures`, `invariant`, `old`, quantifiers),
declared effects (a function can take its effects from a function argument, so an HTTP
handler can use files or a database, and the callbacks of `map`, `filter` and the other
built-in methods can have effects), `Option` and `Result` with `?`, mutable value
semantics with `&mut` only on parameters, and calls to C libraries behind the `ffi`
effect, with numbers, strings and byte buffers. No nil, no zero values, no implicit conversions, no GC.

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
shared benchmark suite; a 100k-line project checks in about 70 ms with the Rust compiler on
four threads (jigc, which `jig` now runs, takes about 210 ms on one).

**Standard library.** `std/io`, `fs`, `net` (TCP request/response, HTTP serving for
browsers and HTTP clients, a server that answers many connections at once, an HTTP client
with HTTPS through the system's OpenSSL, and a simulator), `html` (pages as node trees, escaped when
rendered), `json`, `hash` (SHA-256, HMAC-SHA-256), `regex` (matched in linear time),
`time` (with UTC calendar dates, ISO 8601 and HTTP dates), `rand`, `os`, `task` (fork-join),
`math`, `lists`, `strings`; 83% of its functions have contracts that prove completely.

**Platforms.** Linux (x86-64) and macOS (Apple silicon), each tested in CI with the
compiler built from the C bootstrap; release binaries for both.

**Documentation.** A documentation site, built from the repository's Markdown by `site/`,
a Jig program, and deployed to GitHub Pages.

**Agents.** A Claude Code plugin: the reference as a skill, `jig check` after every edit, a
commit gate, a proof-hint skill, spec-first commands and MCP tools. Opt-in, local-only
trace collection.

**Self-hosting.** `jig` is `jigc`, the compiler written in Jig. It compiles itself:
`bootstrap/jigc.c` builds with any C compiler and regenerates itself byte for byte, and
`bootstrap/install.sh` installs it with nothing else. It passes every test suite on its own
(`bootstrap/suites.py`, in CI without Rust), and for the same input each command prints what
the Rust compiler that bootstrapped it prints, which is now frozen as the reference the
tests compare with (D6).

**Examples.** Nineteen small projects under `examples/`, listed in `docs/examples.md`: a
web server in 30 lines, a key-value store over HTTP, notes in Postgres through libpq, a
guestbook web app on Postgres, a blog rendered on the server with its own HTML engine and
router, a to-do list on files, a stack and a vending machine whose contracts prove,
Gherkin-first shopping-cart rules, units of measure and parallel prime counting among them.
CI checks and tests each one with both compilers.

**Not yet.** Fewer agent turns than Rust and TypeScript (the first runs solved every task
in all three, in 7.3, 6.8 and 6.2 turns on average), contracts on most of the compiler (477
of its 1,229 functions prove completely, the lexer and nearly all of the parser among them),
self-hosted speed parity, and checking 100k lines in under 100 ms with jigc (209 ms on one
thread; the Rust compiler's 70 ms uses four) are open (`docs/gates/phase4.md`).
