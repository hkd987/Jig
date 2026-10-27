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
cargo build --release                       # the compiler: target/release/jig
jig check examples/bank                     # types, effects, lints, dead code (milliseconds)
jig test examples/bank                      # unit tests and feature scenarios
jig prove examples/bank                     # contracts and safety, with counterexamples
jig trace uncovered examples/bank           # code no scenario exercises
jig run examples/hello                      # run main on the interpreter
jig build examples/hello --release          # native binary through C
jig fmt examples                            # the one formatting
```

`jig prove` needs Z3 on the `PATH` (`pip install z3-solver==5.3.2.0`); native builds need a
C compiler (`cc`, or `clang` for release builds). `jig help` lists every command, and
`jig explain E0314` explains a diagnostic. `check`, `test`, `prove` and `trace` take `--json`
for agents.

## For agents

The [plugin](plugin/README.md) gives Claude Code the reference as a skill, runs `jig check`
after every edit, blocks commits that fail to check, test or format, and adds spec-first
commands (`/jig:spec`, `/jig:contracts`, `/jig:implement`, `/jig:review`) that stop for
human approval between steps. `jig mcp` serves the same tools over MCP.

## Numbers

Measured in CI on every push (see `bench/` and `docs/gates/`):

| Metric | Target | Now |
| --- | --- | --- |
| `jig check` on a 100k-line project | under 100 ms | about 70 ms (4 cores) |
| Re-proof after changing one function | under 1 s | 0.2 s |
| Runtime vs Rust, 8 benchmarks (instructions) | within 10% | 0.89 |
| Hello-world release binary | under 100 KB | 84 KB |
| Self-hosted compiler vs bootstrap (instructions) | equal or faster | 0.94 |
| Standard library functions with proven contracts | over 80% | 91% |

## Building without Rust

`bootstrap/jigc.c` is `jigc` compiled to C by itself: one file that any C11 compiler builds.

```
cc -O2 -std=gnu11 -pthread bootstrap/jigc.c -lm -o jigc
./jigc check "$PWD/examples/hello" "$PWD/compiler/std"
./jigc cc selfhost compiler/std > jigc.c    # the same bytes as bootstrap/jigc.c
```

`jigc` checks programs and compiles them to C with output identical to the Rust compiler's.
It binds `.feature` scenarios, loads required modules, formats, and runs tests and scenarios
on its own interpreter (`jigc test`, the lines of `jig test --json`), and proves contracts with
Z3 (`jigc prove`, the same results as `jig prove`, sharing its proof cache), and builds
executables with the checks those proofs discharge removed (`jigc build`, the same C as
`jig build`). It has no full command line of its own yet (human diagnostics, `run` with real
effects, `get`, `trace`, `mcp`); until it does,
the Rust compiler (`compiler/`) is the one `jig` command and is kept passing the same tests.

## License

MIT
