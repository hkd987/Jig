# Phase 4 gate: the compiler compiles itself; public alpha

Gate: "the compiler compiles itself; public alpha release."

## The compiler compiles itself

`jigc` (`selfhost/`) is the Jig compiler written in Jig: 18,500 lines ported pass by pass
from the bootstrap compiler (about 16,000 lines of Rust for the same passes), keeping its
flat arenas and indices. It lexes, parses, loads packages, collects items, type-checks
(inference, effects, `&mut` rules, contracts, units), checks exhaustiveness, runs the lints
and the whole-program dead-code check, lowers to MIR, runs the ownership analyses and
prints C.

The fixed point, checked in CI by `bootstrap/update.sh --check` with gcc and with clang, on
a runner without Rust:

1. `cc bootstrap/jigc.c -o jigc0`: the checked-in C (3.5 MB, runtime included) builds a
   compiler with nothing but a C compiler.
2. `jigc0 cc selfhost compiler/std` regenerates the C from the Jig sources.
3. The two files are byte-identical, so the compiler built by itself rebuilds itself.

The bootstrap compiler agrees with it: `compiler/tests/selfhost.rs` compares every pass
byte for byte on every example, UI and run test, the agent benchmark's solutions and `jigc`
itself (token and AST dumps on every `.jig` file plus 400 mutated copies; diagnostics and
the type of every expression on 300 semantically mutated projects; the whole C program for
every program that builds). `cargo test` fails if `bootstrap/jigc.c` is stale.

The port paid for itself before it finished. It found bugs every Jig program had:

- A struct invariant check copied the whole struct and kept the copy alive, so the next
  `push` copied the list: the lexer was quadratic.
- `a + b + c` allocated a string per `+`; numbers and payload-free enums went through a
  formatting buffer; a `match` on strings compared against every literal in turn.
- Invariants that call `.len()` were never checked at run time, and every write to a field
  re-evaluated every invariant of the struct, copying the fields they read.
- `jig prove` overflowed its stack on any type that holds a list of itself (a tree).
- The C back end named generated helpers by type-table ids, so the same program could
  produce different C; it now numbers them by first use.

## Success metrics

The PRD's proposed targets, measured on `main` (4-core Xeon, clang 18):

| Metric | Target | Now | |
| --- | --- | --- | --- |
| Check after one edit, 100k-line project | under 100 ms | 70 ms for a full `jig check` of 98k lines | met |
| Re-proof after changing one function | under 1 s, warm cache | 0.2 s (`bench/reprove.py`, CI) | met |
| Bootstrap compiler regression per change | under 2% or sign-off | gated in CI (`bench/compare.py`) | met |
| Runtime vs Rust, shared suite | within 10% | 0.89 by instructions, 0.97 wall clock ([phase 3](phase3.md)) | met |
| Hello-world release binary | under 100 KB | 84 KB (72 KB stripped) | met |
| Self-hosted vs bootstrap compiler, same corpus | equal or faster | 0.94 by instructions (CI gate 1.05); 1.36 by wall time | instructions only |
| Agent tasks reaching a verified build, vs Rust | equal or better | harness and tasks ready (`bench/agents`); no agent runs yet | not measured |
| Agent iterations per task, vs Rust and TypeScript | equal or fewer | as above | not measured |
| Std and compiler functions with proven contracts | over 80% | std 91%; jigc 3% (31 of 1,069 functions state contracts, and all 31 prove completely, every function of the lexer among them) | std only |
| Language reference size | fits in 10k tokens | about 6,000 | met |

**Self-hosted speed.** By pass (`bench/selfhost.py --instructions --prove`, with jigc built
as a real build is, after `jig prove`, so the checks its proofs discharge are gone): lexer
0.92, parser 0.61, checker 1.33, checker through C 1.03; geometric mean 0.94. Most of the closing came
from the language's implementation: a copy of a place now aliases it, with no reference
count, while nothing can write, drop or move the place's root, so reading a type's or
expression's kind out of a list no longer retains and releases its payload. The checker's
remaining gap is jigc's own: it slices every identifier into a new string and hashes
strings where the bootstrap compiler compares interned symbols. Values now also die at
their last use (an owned local is released after the statement that last reads it, counting
reads of what borrows from it), and a function's return releases only the locals that may
still hold a value, which removed most of the release calls in large functions. Wall time
is further behind (1.36; 1.29 for the build before the lexer's contracts, on the same
machine), because jigc's nodes use 64-bit fields and the bootstrap compiler parses on a
second thread.

**Agent measures.** `bench/agents/run.py --agent CMD` runs an agent per task and language
and reports tasks solved and turns. Its six tasks have reference solutions in all three
languages, checked in CI, but the agent runs themselves are not part of CI and have not
been done.

**Compiler contracts.** The standard library meets the target; the compiler does not. The
lexer is proved completely: each of its 26 functions states a contract and every
obligation in it proves, its traps included (the cursor stays in the source and only moves
forward, tokens lie inside it and end with `Eof`, global offsets cannot overflow, literal
decoding stays in range). That took one prover change, in both compilers: a call to a pure
function that reads only scalars is a function of what it reads (read off its body like a
frame), so `self.at(i)` gives the same character twice even after other fields of the lexer
changed. Most of jigc's 1,069 functions state no contract yet, and those in the checker and
back end index the program's arenas, where proofs need invariants on the indices.

## The alpha

Version 0.1.0-alpha is what `main` holds now: the `jig` command (Rust), the standard library,
the agent plugin, `jigc` and its C bootstrap. `.github/workflows/release.yml` builds the
Linux x86-64 binary and publishes it with `bootstrap/jigc.c` as a GitHub pre-release when a
`v*` tag is pushed:

```
git tag v0.1.0-alpha && git push origin v0.1.0-alpha
```

Release notes are in [`CHANGELOG.md`](../../CHANGELOG.md).

## Open

- `jigc`: a full command line (human
  diagnostics, `run` with real effects, `build`, `get`, `trace`, `mcp`); its checker, back end,
  formatter, test runner and prover are done. Until `jigc` passes every test suite the Rust
  compiler stays the `jig` command (D6).
- Self-hosted speed: identifiers interned instead of sliced, and wall time (memory).
- Contracts on the compiler's passes.
- Agent benchmark runs.
