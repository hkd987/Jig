# jigc: the Jig compiler in Jig

The PRD's north star is a compiler written in Jig that compiles itself. It is being ported
from the bootstrap compiler (`compiler/`, Rust) one pass at a time, keeping its design:
flat arenas and indices, which map directly onto Jig's value semantics.

Each ported pass must agree with the bootstrap compiler byte for byte on a dump of its
output. `compiler/tests/selfhost.rs` builds `jigc` natively and compares it with the
bootstrap compiler on every `.jig` file in the repository plus a few hundred mutated copies.

| Pass | Bootstrap | Port | Check |
| --- | --- | --- | --- |
| Lexer | `compiler/src/syntax/lexer.rs` | `syntax/lexer.jig` | `jigc debug tokens FILE` = `jig debug tokens FILE` |
| Parser | `compiler/src/syntax/parser.rs` | `syntax/ast.jig`, `syntax/parser.jig` | `jigc debug ast FILE` = `jig debug ast FILE` |
| Checker: loading, collection, types, effects, exhaustiveness, lints, dead code, feature binding | `compiler/src/sema/`, `compiler/src/feature/` | `sema/` | `jigc debug check DIR` = `jig debug check DIR` |
| `jig check`: project analysis, diagnostics as text and JSON | `compiler/src/tools/project.rs`, `compiler/src/diag/` | `sema/project.jig`, `sema/render.jig` | `jigc check [PATH] [--json]` = `jig check [PATH] [--json]` |
| MIR lowering, ownership analyses, C back end | `compiler/src/mir/lower.rs`, `compiler/src/codegen/` | `sema/mir.jig`, `sema/lower*.jig`, `sema/own.jig`, `sema/cgen*.jig` | `jigc debug c DIR` = `jig debug c DIR` |
| Formatter | `compiler/src/fmt/` | `syntax/fmt.jig`, `syntax/fmt_expr.jig` | `jigc fmt [--check] PATH` = `jig fmt [--check] PATH` |
| Interpreter, test runner, property tests | `compiler/src/mir/interp.rs`, `compiler/src/runner.rs`, `compiler/src/proptest.rs` | `sema/interp.jig`, `sema/intrinsics.jig`, `sema/runner.jig` | `jigc test [--json] DIR` = `jig test [--json] DIR` |
| Real effects (`run`) | `Effects::real` in `compiler/src/mir/interp.rs`, `compiler/src/cli.rs` | `sema/real.jig`, `sema/cli.jig` | `jigc run [--native] DIR -- ARGS` = `jig run [--native] DIR -- ARGS` |
| Prover: verification conditions | `compiler/src/verify/vc.rs`, `frame.rs` | `sema/vc*.jig`, `sema/frame.jig` | `jigc debug vc DIR` = `jig debug vc DIR` |
| Prover: solving, replay, reports | `compiler/src/verify/mod.rs`, `smt.rs` | `sema/prove.jig` | `jigc prove --json DIR` = `jig prove --json DIR` |

Its command line is `jig`'s: `jigc check`, `test`, `prove`, `run`, `build` and `fmt` take the
same arguments and print the same (`run` with real effects, on its interpreter or with
`--native` built and run), and the dumps the tests compare are `jigc debug ...`, as in
`jig debug ...`. It carries the standard library and the C runtime (`sema/stdlib.jig`, written
by `bootstrap/stdlib.py` and kept current by a test); `$JIG_STD` names a directory to read
instead.

`jigc` compiles itself: `jigc cc selfhost compiler/std` prints a self-contained C file that a
C compiler builds into a `jigc` printing the same file again. That file is checked in as
`bootstrap/jigc.c` (decision D6; see `bootstrap/README.md`).

The AST dump (`syntax/dump.jig`) prints every node with its span, one top-level item per
line, then the errors, so a difference anywhere in the tree or in error recovery shows up.
Most of the mutated files have syntax errors.

The checker dump (`sema/analyze.jig`) prints every diagnostic (code, file, range, message)
and then the type of every expression and pattern of the project's own packages, so it
compares name resolution, inference (including what is left unresolved), effects,
mutability, exhaustiveness, lints and dead code at once. The test runs it on every example,
UI and run test, the agent benchmark's Jig solutions and `jigc` itself, then on 300 mutated
copies (identifiers, types, literals and operators replaced, lines deleted, duplicated or
swapped, `let`/`var`, `&mut`, `pub` and `effects` changed), which between them raise more than
fifty different error codes. `jigc` reads the standard library from STD
(`compiler/std`); the bootstrap compiler embeds it. Imports resolve through the modules
`jig.mod` requires, as in the bootstrap compiler's `modules.rs`: one version of each (the
highest any module requires), `replace` directories, and the module cache under
`$JIG_HOME`; the test compares projects with missing, raised, replaced and misnamed
modules.

Feature files (`sema/feature.jig`) are parsed and their steps bound to `step` functions
with typed placeholders, as in the bootstrap compiler's `feature/`; the steps and world
constructors that scenarios use are roots of the dead-code check. The test also compares
300 projects with mutated `.feature` files and step patterns (keywords, tables, outlines,
placeholder values that do not parse or do not fit, missing and ambiguous steps).

The formatter (`syntax/fmt.jig`, `syntax/fmt_expr.jig`) is the bootstrap compiler's two
passes: comments get slots in the contexts that hold them, then each node prints flat when
it fits in 100 columns and broken otherwise. The test formats every `.jig` file in the
repository and 600 copies with their layout mutated (indentation stripped, lines joined
and split, names lengthened past the width, comments and blank lines inserted, spacing
changed); `jigc fmt` must write exactly what `jig fmt` writes, or refuse the same files.

The interpreter (`sema/interp.jig`, `sema/intrinsics.jig`) runs the same MIR with the same
faked effects (captured console, in-memory files, a deterministic clock, random numbers and
network), checks and messages, and the runner (`sema/runner.jig`) runs scenarios, tests and
property tests with the same seeds, generated inputs and shrinking. The test compares
`jigc test` with `jig test --json` (results, messages, locations, backtraces, captured
output) on every package with tests, every run test called from a test, and 150 mutated
copies that still check, about a third of them with failing tests; `jigc sim` must print
every run test's expected output. C functions are called as the bootstrap's interpreter
calls them (`sema/ffi.jig`): found by name with `dlsym` among the process's symbols and the
libraries `jig.mod` links, and called through the runtime's `jig_ccall_*`, which fill every
argument register.

`jigc run` gives the interpreter the process's own effects (`sema/real.jig`): output as the
program prints it, standard input, files, the environment, the clock and the network. A
`net.serve` handler runs on the interpreter, so serving goes through the runtime's
`jig_ccall_listen`, `_accept`, `_request` and `_answer` rather than `jig_net_serve`. The test
runs every run test, every example and a program using each effect with `jigc run` and `jig
run`, and the examples and that program with `--native`: the same standard output, standard
error and exit code.

The prover (`sema/frame.jig`, `sema/vc*.jig`, `sema/prove.jig`) evaluates each function
symbolically into SMT-LIB obligations, as the bootstrap compiler's `verify/` does, and the
test requires the same text: `jigc debug vc DIR` prints what `jig debug vc DIR` prints (every
declaration, path fact and goal) for every example, run test, prover test, agent solution,
the standard library and 200 mutations, and for `jigc` itself (5.7 million lines, which jigc
generates eight times faster). `jigc prove` then runs Z3 once on a script of every
function's queries (through the C library's `system`; `echo` markers tell the answers apart)
and replays failed contracts on the interpreter; its report matches `jig prove --no-cache`
on the prover tests, examples, agent solutions and 60 mutations. It shares `jig prove`'s proof
cache: the same keys (the bootstrap's stable hash, ported in `sema/hash.jig`, over the same
contract texts and frames) and the same files, so either compiler reads what the other
proved. Builds use those proofs as the bootstrap's do: `jigc debug c DIR proofs` prints what
`jig debug c DIR proofs` prints (the checks discharged removed from lowering and the C), and
`jigc build DIR [--release]` compiles it.

The back end (about 6,600 lines of Jig for 7,300 of Rust) lowers to the same MIR, runs the
same ownership analyses (borrowed locals, last uses, unique lists) and prints the same C:
the test compares the whole program, byte for byte, for every example, run test, agent
benchmark solution and `jigc` itself (about 110,000 lines of C). To make that comparable the
bootstrap compiler now numbers the C names it derives from types (reference-count glue,
task and sort thunks) in order of first use rather than by its type table's ids.

`jig prove selfhost` proves the contracts `jigc` states. The lexer states one for every
function, and each of its obligations proves, its traps included: the cursor stays in the
source and only moves forward, tokens lie inside it and end with `Eof`, global offsets
cannot overflow, and literal decoding stays in range. The parser's cursor is always on a
token, no parsing function moves it back, and node constructors return the new index; all
of the parser proves but the six functions that index the tree by a node id or slice the
source by a token's offsets, which would need facts about every node or token.

## Speed

The PRD asks the self-hosted compiler to be as fast as the bootstrap compiler on the same
corpus. `bench/selfhost.py` builds `jigc` with `jig build --release` (after `jig prove`,
with `--prove` as in CI, so the checks its proofs discharge are gone) and runs each shared
pass against `jig debug`: the lexer and parser on every `.jig` file in the repository,
repeated, and the checker (on one thread) and the whole pipeline to C on a project
generated by `bench/gen.py`:

| Pass | jigc / jig, instructions | jigc / jig, wall time |
| --- | --- | --- |
| Lexer and token dump | 0.92 | 1.05 |
| Parser and AST dump | 0.61 | 1.16 |
| Checker and type dump (98k lines) | 1.33 | 2.17 |
| Checker, MIR lowering and C (98k lines) | 1.03 | 1.31 |

The geometric mean of the instruction ratios is 0.94 (CI gate 1.05), and of wall times
1.36: by instructions the self-hosted compiler is faster than the bootstrap compiler. The checker is still behind.
The lexer and parser do 2.6 times the bootstrap compiler's work (every identifier is sliced
into a new string before the keyword lookup); their dumps are cheaper, which hides it in
the dump passes. Bodies cost more because name lookups hash strings where the bootstrap
compiler compares interned symbols. Wall time is worse than instructions: jigc's nodes use
64-bit `Int` fields where the bootstrap compiler uses 32-bit ones, so it touches about
twice the memory, and the bootstrap compiler parses on a second thread.

What closed most of the gap was a change to the language's implementation, not to jigc:
a copy of a place (`match self.tys.kinds[t]`, `let e = self.ast.exprs[i]`) used to retain
everything it held and release it again. Now such a copy aliases the place, with no count
of its own, whenever no path from the copy to its uses can write, drop or move the place's
root (`codegen/own.rs`, `sema/own.jig`); pattern bindings used after a write still take
their own counts. That cut jigc's checker by 13% and helps every Jig program that reads
structured values out of lists.

Porting the passes found compiler and runtime problems that every Jig program had: a
struct invariant check copied the whole struct and kept the copy alive, so the next `push`
copied the list (the lexer was quadratic); `a + b + c` allocated a string per `+`; numbers
and payload-free enums went through a formatting buffer; and a `match` on strings compared
against every literal in turn. All four are fixed. So is a fifth, found by the back end's
port: after a write to one field, every invariant of the struct was re-evaluated (copying
the fields it reads), and invariants that call `.len()` were silently never checked at run
time. Now a write re-checks only the predicates that read the written field, and `.len()`
and `.is_empty()` are evaluated.

```
jig run selfhost -- tokens examples/hello/main.jig
jig run selfhost -- ast examples/hello/main.jig
jig run selfhost -- check "$PWD/examples/hello" "$PWD/compiler/std"
jig run selfhost -- c "$PWD/examples/hello" "$PWD/compiler/std"
jig test selfhost
```
