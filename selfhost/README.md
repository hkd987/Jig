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
| Parser | `compiler/src/syntax/parser.rs` | not yet | |
| Formatter, checker, MIR, back end | | not yet | |

Every contract in `jigc` proves (`jig prove selfhost`): token spans are ordered, the cursor
only moves forward, and the lexer's state stays consistent.

```
jig run selfhost -- tokens examples/hello/main.jig
jig test selfhost
```
