---
name: jig
description: Write, check and test code in the Jig language (.jig source files and .feature scenario files). Use whenever reading, writing or fixing Jig code, or when the user mentions Jig.
---

# Writing Jig

Jig is new, so you have not seen it before. Read [reference.md](reference.md) before writing
code; it is short and complete. [cookbook.md](cookbook.md) has idioms for common tasks.

## The loop

1. Write or edit code. After every edit the plugin runs `jig check` and shows you any errors.
   Every lint is an error, including unused variables, needless `var`, unused imports and dead
   code. Fix all of them before moving on.
2. Run the tests with the `test` tool (or `jig test`). Tests run with faked console, clock,
   randomness and files, so they are deterministic.
3. Run `jig fmt` (or the `fmt` tool) before committing. Commits are blocked until the project
   checks, tests pass and files are formatted.

Use the MCP tools: `check`, `test`, `run`, `fmt`, `explain <code>` for any diagnostic code,
`search` to find a function by type (`Str -> Option<Int>`), and `reference`.

## What trips up newcomers

- Named function bodies need an explicit `return`. Closures, `if` and `match` blocks yield
  their last expression: `xs.map(fn(x) { x * 2 })`.
- No semicolons. A line ends a statement; put a binary operator at the end of a line to
  continue it, or start the next line with `.` to continue a method chain.
- `let` is immutable, `var` is mutable. A `var` that is never mutated is an error.
- To let a function change a caller's variable, take `x: &mut T` and pass `&mut x`. Methods
  that mutate take `&mut self`. Values are copied otherwise; there are no pointers.
- A function that prints needs `effects {io}`; files need `fs`; `os.args()` needs `env`. Pure
  is the default, and declaring an effect you never use is an error.
- No implicit conversions: `n as Float`, `x.str()`, `Some(v)`, `Ok(v)`.
- Methods are declared `fn Type.name(self, ...)`. Variants are qualified when built
  (`Shape.Circle(1.0)`) and unqualified in patterns (`Circle(r) => ...`); `Some`, `None`,
  `Ok`, `Err` never need a qualifier.
- `match` must be exhaustive and has no unreachable arms.
- Everything must be reachable from `main` (or from the `pub` API of a library). Code used
  only by tests is dead code. Fields must be read; variants must be constructed.
- Empty collections need a type: `let xs: List<Int> = []`, `var m: Map<Str, Int> = Map.new()`.
- `for` is the only loop: `for i in 0..n`, `for x in xs`, `for (k, v) in m`, `for cond`, `for`.

When a diagnostic is unclear, call `explain` with its code; most diagnostics include a fix.
