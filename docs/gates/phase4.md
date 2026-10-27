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

`jigc` is the `jig` command. It has the whole command line (`check`, `test`, `prove`, `run`
with real effects or `--native`, `build`, `trace`, `search`, `fmt`, `get`, `traces`, `hook`,
`mcp`, `explain`, `reference`), and for the same input every command prints what the Rust
compiler's prints, which the same test checks command by command (hook input and MCP
sessions included). It passes every test suite on its own: `bootstrap/suites.py` builds it
from `bootstrap/jigc.c` and runs the UI, run, native, prover, property, trace, cookbook,
project, module and network suites through it, in CI on a runner without Rust. So, per D6,
the Rust compiler is frozen as the reference the differential tests compare with, and
`bootstrap/install.sh` installs `jigc` as `jig` with only a C compiler.

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
| Check after one edit, 100k-line project | under 100 ms | 70 ms for a full check of 98k lines by the Rust compiler, on four threads; 209 ms by jigc, which `jig` now runs, on one thread | Rust compiler only |
| Re-proof after changing one function | under 1 s, warm cache | 0.2 s (`bench/reprove.py`, CI) | met |
| Bootstrap compiler regression per change | under 2% or sign-off | gated in CI (`bench/compare.py`) | met |
| Runtime vs Rust, shared suite | within 10% | 0.89 by instructions, 0.97 wall clock ([phase 3](phase3.md)) | met |
| Hello-world release binary | under 100 KB | 84 KB (72 KB stripped) | met |
| Self-hosted vs bootstrap compiler, same corpus | equal or faster | 0.92 by instructions (CI gate 1.05); 1.21 by wall time | instructions only |
| Agent tasks reaching a verified build, vs Rust | equal or better | harness and tasks ready (`bench/agents`); no agent runs yet | not measured |
| Agent iterations per task, vs Rust and TypeScript | equal or fewer | as above | not measured |
| Std and compiler functions with proven contracts | over 80% | std 91%; jigc 39% (491 of 1,209 functions state contracts and 475 of them (97%) prove completely, every safety obligation included: the lexer, nearly all of the parser, the JSON reader, and helpers across every pass, among them those that index an arena by a parameter, which now require the index; most of the rest read an index out of a tree node or a table, so proving it needs an invariant over every node's payload, which contracts cannot yet state) | std only |
| Language reference size | fits in 10k tokens | about 6,000 | met |

**Self-hosted speed.** By pass (`bench/selfhost.py --instructions --prove`, with jigc built
as a real build is, after `jig prove`, so the checks its proofs discharge are gone): lexer
0.93, parser 0.69, checker 1.10, checker through C 1.02; geometric mean 0.92. Most of the closing came
from the language's implementation: a copy of a place now aliases it, with no reference
count, while nothing can write, drop or move the place's root, so reading a type's or
expression's kind out of a list no longer retains and releases its payload. The checker's
remaining gap is jigc's own: it slices every identifier into a new string and hashes
strings where the bootstrap compiler compares interned symbols. Profiling the checker found
three costs of jigc's own, now gone: binding `.feature` scenarios scanned every function once
per package (8% of a check without features); `zonk` rebuilt and re-interned every compound
type, now skipped when a type has no inference variables (a flag set when it is interned,
which also makes `has_infer` and `has_vars` one lookup); and finding a project's `main`
packages split every line of every file into characters. Values now also die at
their last use (an owned local is released after the statement that last reads it, counting
reads of what borrows from it), and a function's return releases only the locals that may
still hold a value, which removed most of the release calls in large functions. Wall time
is further behind (1.21, the checker 1.71), because jigc touches about twice the memory
(64-bit node fields, reference counts) and the bootstrap compiler parses on other threads.
That also decides a full `jig check`, which the Rust compiler runs on four threads and jigc
on one: 209 ms against 70 ms on 98k lines, so the 100 ms target is met only by the Rust
compiler. Parsing a load wave's files with `std/task` does not close it: a program that
starts tasks counts every reference atomically, which costs jigc about 14% of its CPU time,
about what parsing on four threads saves (best wall time 169 ms against 180 ms, the median
unchanged). Checking jigc in parallel needs reference counts that stay cheap on the thread
that owns a value (biased counting, say), or a checker whose threads share no counted
values.

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
changed. The parser states that its cursor is always on a token and the last token is the
end of the file, that no parsing function moves the cursor back, and that the node
constructors return the new node's index; all of it proves, and with it every bounds and
overflow check in the parser but those that index the tree by a node id. Those, and the
checker and back end, which index the program's arenas everywhere, need facts about every
node (quantified invariants). The prover handles those, but the bootstrap C is built
without proofs, where a quantified invariant is checked at each change: on a list that
grows token by token or node by node, that makes compiling quadratic. Most of jigc's 1,069
functions state no contract yet.

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

- Self-hosted speed: identifiers interned instead of sliced, and wall time (memory).
- Contracts on the compiler's passes.
- Agent benchmark runs.
