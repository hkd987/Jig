---
name: jig-proofs
description: Make Jig contracts and runtime-safety obligations prove with `jig prove`. Use when a `prove` result shows FAILED or unknown obligations, when writing requires/ensures/invariant clauses, or when asked to remove runtime checks from Jig code.
---

# Getting Jig proofs through

`jig prove` (the `prove` tool) checks every non-generic function on its own: it knows the
function's body, its `requires`, the types of its parameters (including refinements and
struct invariants), and the **contracts** of the functions it calls, never their bodies.
A proved obligation costs nothing at run time; an unproven one keeps its runtime check.

Contract obligations (`requires` at call sites, `ensures`, invariants, refinements,
`assert`) are what you must get through. Safety obligations (overflow, bounds, division,
`unwrap`) are optional: unproven ones just keep a check. Proofs are cached per function, so
re-running after an edit only redoes what changed.

## Reading a failure

```
FAILED postcondition in `clamp` at main.jig:12:11: result <= hi
  counterexample: x = 7, lo = 0, hi = 5
```

`jig prove` then runs the function on the interpreter to tell you which case you are in:

- `confirmed: running it with x = 7, lo = 0, hi = 5 fails this check`: a real bug. Fix the
  code or the contract.
- `not reproduced: the counterexample passes at run time`: the code is fine; the prover is
  missing a fact.

With neither line (the inputs could not be rebuilt or the run stopped elsewhere), ask
yourself whether the inputs can really happen. Facts come only from these places:

| Missing fact about | Put it in |
| --- | --- |
| an input | the function's `requires`, or a refinement type (`n: Int where n >= 0`) |
| a call's result or effect on `&mut` args | the callee's `ensures` (`ensures result >= 0`, `ensures v.len() == old(v.len()) + 1`) |
| state after a loop | a loop `invariant` that holds before each iteration |
| a struct's fields | a struct `invariant` (assumed for every value of the type) |

## Patterns

- **Loops.** After a loop the prover knows only the invariants and that the loop ended.
  For `var total = 0; for x in xs { total += x }`, prove `total >= 0` with
  `for x in xs invariant total >= 0`. If the body needs a bound to avoid overflow, put the
  bound in the invariant too (`invariant total <= i * 1000`) and in `requires`.
- **Counting.** `for i in 0..n invariant out.len() == i { out.push(...) }` proves
  `out.len() == n` afterwards. Quantified facts work the same way:
  `invariant forall k in 0..out.len(): out[k] > 0`.
- **Indexing.** `for i in 0..xs.len() { xs[i] }` proves its bounds; so does
  `if i < xs.len() { xs[i] }`. Writing `xs[i] = v` keeps `xs.len()` known.
- **Option/Result.** Prefer `match` to `unwrap()`. When a producer always succeeds under a
  condition, say so: `ensures n >= 0 || result.is_none()`.
- **Overflow.** Bound inputs in `requires` (`requires a <= 1_000_000_000`) or use
  `wrapping_add` when wrapping is the intent.
- **Calls with no contract** return an arbitrary value of their type. Add `ensures` to the
  callee rather than asserting after the call.
- **Pure helpers** with scalar arguments are treated as mathematical functions: equal
  arguments give equal results, so `ensures result == f(x)` can be proved by returning
  `f(x)`.

## Limits

Strings and floats are opaque except for string length and concatenation; generic functions
are not proved (their checks stay); closure bodies are not analyzed. Write contracts about
lengths, integers, booleans, enum tags and struct fields for the best results.
