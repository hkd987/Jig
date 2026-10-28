# Working on Jig

**If you make the same mistake more than once, add a rule here that prevents it, or a skill
(`.claude/skills/<name>/SKILL.md`) when the fix is a procedure.** Keep each rule short and say
what to do, not what went wrong.

## Layout

- `compiler/`: the Rust bootstrap compiler, frozen as the reference (D6). `compiler/std/` and
  `compiler/runtime/` are shared by both compilers.
- `selfhost/`: `jigc`, the compiler in Jig, which ships as `jig`. `bootstrap/jigc.c` is its
  generated C.
- `docs/reference.md` is the language reference; `docs/gates/` holds the phase reports and
  their metrics.
- `site/` is the documentation site, a Jig program (`jig run site -- OUT`) that
  `.github/workflows/pages.yml` deploys; it renders the pages listed in `site/main.jig`.
- A new standard library module is listed in `compiler/src/sema/load.rs`,
  `selfhost/sema/load.jig` (whose `std_packages` states the count), and the package lists in
  `selfhost/sema/analyze.jig` and `selfhost/sema/prove.jig`, all in the same order. Every
  contract in the standard library must prove, and 80% of its functions completely
  (`compiler/tests/prove.rs`). Its tests go in `compiler/tests/stdlib/`.

## Rules

- After changing `selfhost/`, `compiler/std/`, `compiler/runtime/`, `compiler/src/diag/codes.rs` or
  `docs/reference.md`, run `sh bootstrap/update.sh` and commit the new `bootstrap/jigc.c`. If
  the old `jigc.c` cannot compile the new sources, regenerate in two steps: first without the
  code it cannot compile, then with it.
- `plugin/skills/jig/reference.md` is a copy of `docs/reference.md`: change both.
- `jigc` must print what the Rust compiler prints, byte for byte (`compiler/tests/selfhost.rs`).
  A change to behaviour or output goes into both compilers; a prover change also bumps
  `VC_VERSION` in `compiler/src/verify/mod.rs` and `selfhost/sema/prove.jig`.
- Before committing, run what CI runs, one run at a time, and do not edit `selfhost/`,
  `compiler/std/` or `compiler/runtime/` while it runs (the tests read them):
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`,
  `jig fmt --check compiler/std compiler/tests/run compiler/tests/fixtures compiler/tests/prove compiler/tests/props compiler/tests/stdlib examples selfhost site`,
  and `jig check` and `jig test` on each of `examples/*/`, `selfhost/`, `site/` and `compiler/tests/stdlib/`.
- Format with `cargo fmt` and `jig fmt selfhost` (and any other Jig directory you touched).
- Never put a model name or identifier in a commit, comment or document.

## Contracts and `jig prove`

`jig prove selfhost --json` ends with a summary line (functions, with contracts, proved). A
function counts as proved only if it states a contract and every obligation in it proves.

- A `for x in xs` loop cannot name its index: to state a length fact, loop over
  `0..xs.len()` with `invariant out.len() == i` (or `<= i`).
- Loop invariants cannot use `old()`: save the value in a local before the loop
  (`let len0 = o.len()`) and write `invariant o.len() >= len0`.
- A loop forgets what its body may change, struct invariants included: restate bounds such as
  `self.i <= self.b.len()` in the loop's invariant.
- Contract arithmetic is checked for overflow: write `result == self.xs.len() - 1`, not
  `result + 1 == ...`, and `pos - f.base <= n`, not `pos <= f.base + n`.
- No `match` in contracts, and a parameter named `result` hides the return value.
- Bit operations, shifts, `starts_with` and `join` are not modeled: use `%` and `*` where the
  values allow it, or check lengths explicitly.
- A struct invariant is checked after every write to a field, so every intermediate state
  must satisfy it.
- An arena index that comes in as a parameter can be required (`requires e >= 0 && e <
  self.ast.exprs.len()`); add it only where the function then proves, since each call that
  reads the index out of a node or table cannot prove it.
