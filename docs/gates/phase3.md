# Phase 3 gate: within 10% of Rust on the benchmark suite

Measured by `bench/vsrust.py`. Each program in `bench/vsrust/` is written idiomatically in
both languages and must print the same output. The Jig side is proved (`jig prove`, so
proved checks disappear) and built with `jig build --release`; the Rust side with
`rustc -O`. The score is the geometric mean of the Jig/Rust ratios; the gate is 1.10.

Machine: 4-core Xeon @ 2.1 GHz, clang 18.1.3, rustc 1.97.0. Wall clock is the best of 7 runs
at the timing sizes; instruction counts come from Valgrind at the smaller CI sizes (both in
`vsrust.py`'s `ARGS`).

| Benchmark | Wall clock (Jig / Rust) | Instructions (Jig / Rust) |
| --- | --- | --- |
| binarytrees | 0.65 | 0.32 |
| collatz | 1.45 | 0.93 |
| fannkuch | 1.12 | 1.23 |
| nbody | 1.16 | 1.47 |
| sieve | 0.83 | 0.77 |
| sort | 0.89 | 0.91 |
| spectral | 1.05 | 1.00 |
| wordfreq | 0.85 | 1.03 |
| **geometric mean** | **0.97** (limit 1.10) | **0.89** (limit 1.10) |

Wall-clock timings on a shared machine vary by about 10% between runs; instruction counts
are exact, so CI gates on them (bench job, `vsrust.py --instructions --gate`).

The suite started at 7.2x (wall clock) when it was added. Every Jig program keeps checked
arithmetic and bounds checks wherever they are not proved; Rust's release builds check
bounds but not overflow.

## What closed the gap

Proofs remove checks, so most of the work was making idiomatic programs provable and the
proved code cheap:

- **Checks the prover removes.** Indexing proved in bounds and arithmetic proved not to
  overflow compile to plain C. The prover learned list lengths after push loops, that
  `swap`/`reverse`/`sort` keep a length, and `List.repeat`'s length. The benchmarks state
  the contracts an agent would write (`requires` ranges, loop invariants, an `n` guard):
  spectral and nbody prove completely, and every index in sieve proves.
- **Preconditions proved at every call** (D22) are not checked again in the callee. The
  program is closed, so when every call site proves a `requires`, the function needs no
  entry check (spectral's `a(i, j)` runs its arithmetic unchecked).
- **Reference counting.** Lists written by index are made unique once where they are
  defined, not on every write. Locals that only alias part of a parameter take no count, and
  the last use of a local moves its value instead of retaining it.
- **Allocation.** Release builds allocate from per-thread size-class pools; fixed-size
  objects (enum boxes, closures, list and map headers) carry no block header, and variants
  without fields are static. A binary-tree node takes 32 bytes, like Rust's `Box`.
- **Runtime.** Typed introsort with a branchless partition; `split` scanning with `memchr`
  and iterating lazily in `for` loops; inline short appends; an 8-bytes-at-a-time string
  hash; constant list literals as static data.

## Where Jig is still slower

- **collatz** (1.45x wall clock, 0.93x instructions): `3 * n + 1` may overflow and nothing
  bounds `n`, so the multiplication keeps its check. The check's branch in the odd arm stops
  clang from turning the parity `if` into a conditional move, and that branch mispredicts.
  Rust does not check overflow in release builds. Deferring the check to the join point
  was tried and did not help (clang re-threads it into the arm).
- **nbody** (1.16x, 1.47x instructions): Rust knows the vector has five elements and
  unrolls the pair loop completely; Jig's list length is only known at run time.
- **fannkuch** (1.12x): the swap indices come from the data (`perm[0]`), so the swap keeps its
  bounds check, as Rust's does.

## A soundness fix found on the way

Release builds used to drop every contract check while still removing safety checks that
proofs had justified with those contracts. A violated `requires` could then skip a removed
bounds check. Every build now keeps every unproven check, contracts included (D22); proving
contracts is what makes a program fast.
