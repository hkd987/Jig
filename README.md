# Jig

Jig is a systems language that agents write and humans can trust: Go's small surface,
Rust-class performance, and verification built into the compiler. Humans write feature
scenarios and contracts. Agents write the implementation. The compiler proves, or failing
that tests, that the implementation meets them.

```
fn transfer(from: &mut Acct, to: &mut Acct, amt: Cents)
  requires amt > 0, from.bal >= amt
  ensures from.bal + to.bal == old(from.bal + to.bal)
  effects {}
{
  from.bal -= amt
  to.bal += amt
}
```

**Status:** alpha (0.1.0). All four phases of [the plan](docs/plan.md) are in place, and the
compiler compiles itself: `jigc`, the Jig compiler written in Jig (`selfhost/`), turns its
own sources into C that rebuilds it byte for byte. The language may still change; the
[decisions](docs/decisions.md) say which choices are provisional.

- [Documentation site](https://hkd987.github.io/Jig/): these documents as web pages, built by
  `site/`, a Jig program
- [Language reference](docs/reference.md) (about 6,000 tokens: it fits in an agent's context)
- [Build plan](docs/plan.md) and the gate reports in [`docs/gates`](docs/gates)
- [Decisions](docs/decisions.md)
- [The agent plugin](plugin/README.md)
- [The self-hosted compiler](selfhost/README.md) and [its bootstrap](bootstrap/README.md)

## What the compiler checks

| Layer | Written by | Checked by |
| --- | --- | --- |
| Feature scenarios (Gherkin `.feature` files) | PMs, engineers | Steps bind to typed functions at compile time; scenarios run as tests |
| Contracts (`requires`, `ensures`, `invariant`) | Agent drafts, human approves | Z3, per function, cached; what does not prove stays a runtime check and a generated property test |
| Refinement types (`x: Int where x > 0`) | Agent | Z3 |
| Unit tests (`test` blocks next to the code) | Agent | A deterministic runner with fake effects |
| Dead code | Nobody | Whole-program reachability: unreachable functions, unread fields and unused exports fail the build |

Strict by default: no nil, no zero values, no implicit conversions, exhaustive `match`,
declared effects (`io`, `fs`, `net`, `clock`, `rand`, `env`, `spawn`, `ffi`), and lints that
are errors. Overflow, out-of-bounds indexing and division by zero trap unless proven
impossible, in which case the check disappears from the binary.

## Quick start

```
sh bootstrap/install.sh                     # the compiler, built with only a C compiler: ~/.local/bin/jig
jig check examples/bank                     # types, effects, lints, dead code (milliseconds)
jig test examples/bank                      # unit tests and feature scenarios
jig prove examples/bank                     # contracts and safety, with counterexamples
jig trace uncovered examples/bank           # code no scenario exercises
jig run examples/hello                      # run main on the interpreter
jig build examples/hello --release          # native binary through C
jig run examples/http -- :8080              # a web server in 30 lines: curl localhost:8080
jig fmt examples                            # the one formatting
```

`jig prove` needs Z3 on the `PATH` (`pip install z3-solver==5.3.2.0`); native builds need a
C compiler (`cc`, or `clang` for release builds). `jig help` lists every command, and
`jig explain E0314` explains a diagnostic. `check`, `test`, `prove` and `trace` take `--json`
for agents.

## Examples

[Twenty-one small projects](docs/examples.md), each checked and tested in CI with both
compilers, show what Jig code looks like:

- [http](examples/http): a web server in 30 lines, tested on simulated requests
- [kv](examples/kv): a key-value store over HTTP
- [postgres](examples/postgres): notes in a Postgres table, through libpq behind the `ffi` effect
- [guestbook](examples/guestbook): a web app on Postgres, its handler tested against a fake database
- [ssr](examples/ssr): pages rendered on the server with `std/html`, and a router package
- [fetch](examples/fetch): an HTTP client, HTTPS included, tested on simulated servers
- [api](examples/api): a JSON API whose stateless handler answers many requests at once
- [todo](examples/todo): a command-line app on files, every effect in its signatures
- [stack](examples/stack) and [vending](examples/vending): contracts that all prove
- [cart](examples/cart) and [bank](examples/bank): behaviour written first as `.feature` scenarios
- [units](examples/units), [primes](examples/primes), [calc](examples/calc): units of
  measure, fork-join parallelism, a parser and evaluator

## For agents

The [plugin](plugin/README.md) gives Claude Code the reference as a skill, runs `jig check`
after every edit, blocks commits that fail to check, test or format, and adds spec-first
commands (`/jig:spec`, `/jig:contracts`, `/jig:implement`, `/jig:review`) that stop for
human approval between steps. `jig mcp` serves the same tools over MCP.

## Numbers

Measured in CI on every push, except the agent runs, which are one run per task and language
so far (see `bench/` and `docs/gates/`):

| Metric | Target | Now |
| --- | --- | --- |
| `jig check` on a 100k-line project | under 100 ms | about 210 ms (jigc, one thread; the Rust compiler takes 70 ms on 4 cores) |
| Re-proof after changing one function | under 1 s | 0.2 s |
| Runtime vs Rust, 8 benchmarks (instructions) | within 10% | 0.79 (slowest: nbody 1.44, wordfreq 1.13, fannkuch 1.12) |
| Hello-world release binary | under 100 KB | 94 KB |
| Self-hosted compiler vs bootstrap (instructions) | equal or faster | 0.94 |
| Standard library functions with proven contracts | over 80% | 83% (134 of 161) |
| Compiler functions with proven contracts | over 80% | 39% (477 of 1,229; 78% have no unproven obligation but a table index) |
| Agent tasks reaching a verified build, vs Rust | equal or better | 6 of 6 (Rust 6 of 6) |
| Agent turns per task, vs Rust and TypeScript | equal or fewer | 7.3 (Rust 6.8, TypeScript 6.2) |

## Known gaps

Jig is an alpha. What it does not do yet (phase 5 of [the plan](docs/plan.md) closes these;
[`docs/gates/phase5.md`](docs/gates/phase5.md) tracks each one):

- **Libraries.** The standard library has files, TCP, HTTP serving and an HTTP client (with
  HTTPS through the system's OpenSSL), HTML, JSON, regular expressions, hashing, dates,
  randomness, tasks and text helpers; a server can answer many connections at once when its
  handler keeps no state.
- **C interop** passes numbers and strings only: no structs, buffers or callbacks, so a
  library that needs them (or a parameterized SQL query) needs a small C wrapper.
- **Verification** proves what contracts and safety checks say, not what the author meant.
  Strings are known to the prover by their length, not their contents, and an invariant over
  every node of a tree or table cannot be stated, so most of the compiler's own table
  lookups stay run-time checks (D29).
- **Speed.** `jig check` is about three times slower than the Rust compiler it replaced (one
  thread against four), and float-heavy code runs behind Rust (nbody, 1.44).
- **Agents** took a few more turns in Jig than in Rust and TypeScript (one run per task so
  far).
- **Platforms and tools.** CI and the release cover Linux (x86-64) and macOS (Apple
  silicon); other POSIX systems should build it with `bootstrap/install.sh` but are untested,
  and there is no Windows port. There is no editor or language server support beyond the
  Claude Code plugin and `jig mcp`, and modules come from git (`jig get`), with no registry.

## Self-hosted

`jig` is `jigc`, the compiler written in Jig (`selfhost/`). `bootstrap/jigc.c` is `jigc`
compiled to C by itself: one file that any C11 compiler builds, which `bootstrap/install.sh`
installs as `jig`.

```
cc -O2 -std=gnu11 -pthread bootstrap/jigc.c -lm -o jigc
./jigc check examples/hello
./jigc cc selfhost compiler/std > jigc.c    # the same bytes as bootstrap/jigc.c
```

The compiler that bootstrapped it, written in Rust (`compiler/`, `cargo build --release`), is
frozen (D6): `jigc` passes every test suite on its own (`bootstrap/suites.py`, run in CI
without Rust), and the Rust compiler stays as the reference `compiler/tests/selfhost.rs`
compares `jigc` with, pass by pass and command by command: the same tokens, syntax trees,
diagnostics, types, verification conditions, proofs, test results and C, and the same output
from every command for the same input.

## License

MIT
