# jigc: the Jig compiler in Jig

The PRD's north star is a compiler written in Jig that compiles itself. It is being ported
from the bootstrap compiler (`compiler/`, Rust) one pass at a time, keeping its design:
flat arenas and indices, which map directly onto Jig's value semantics.

Each ported pass must agree with the bootstrap compiler byte for byte on a dump of its
output. `compiler/tests/selfhost.rs` builds `jigc` natively and compares it with the
bootstrap compiler on every `.jig` file in the repository plus a few hundred mutated copies.

| Pass | Bootstrap | Port | Check |
| --- | --- | --- | --- |
| Lexer | `compiler/src/syntax/lexer.rs` | `syntax/lexer.jig` | `jigc tokens FILE` = `jig debug tokens FILE` |
| Parser | `compiler/src/syntax/parser.rs` | `syntax/ast.jig`, `syntax/parser.jig` | `jigc ast FILE` = `jig debug ast FILE` |
| Formatter, checker, MIR, back end | | not yet | |

The AST dump (`syntax/dump.jig`) prints every node with its span, one top-level item per
line, then the errors, so a difference anywhere in the tree or in error recovery shows up.
Most of the mutated files have syntax errors.

Every contract in `jigc` proves (`jig prove selfhost`): token spans are ordered, the cursor
only moves forward, the lexer's state stays consistent, and the parser's literal decoding
stays in range.

## Speed

The PRD asks the self-hosted compiler to be as fast as the bootstrap compiler on the same
corpus. `bench/selfhost.py` builds `jigc` with `jig build --release` and runs each shared
pass on every `.jig` file in the repository, repeated, against `jig debug`:

| Pass | jigc / jig, instructions | jigc / jig, wall time |
| --- | --- | --- |
| Lexer and token dump | 0.99 | 1.08 |
| Parser and AST dump | 0.74 | 1.10 |

CI gates the instruction ratio's geometric mean at 1.10. The remaining wall-time gap is
memory: jigc's tokens and nodes use 64-bit `Int` fields where the bootstrap compiler uses
32-bit ones, so it touches about twice the pages.

Porting the passes found compiler and runtime problems that every Jig program had: a
struct invariant check copied the whole struct and kept the copy alive, so the next `push`
copied the list (the lexer was quadratic); `a + b + c` allocated a string per `+`; numbers
and payload-free enums went through a formatting buffer; and a `match` on strings compared
against every literal in turn. All four are fixed.

```
jig run selfhost -- tokens examples/hello/main.jig
jig run selfhost -- ast examples/hello/main.jig
jig test selfhost
```
