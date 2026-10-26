# Phase 2 gate: re-proof under 1 s after a one-function change

Measured by `bench/reprove.py`, which CI runs on every push (bench job):

1. Generate the benchmark project (20 packages × 20 units: about 20,000 lines, 3,221
   functions, 11,254 proof obligations).
2. `jig prove` it once, filling `.jig/proofs`.
3. Change the body of one function (`S0.total` in package `p0`), not its signature.
4. `jig prove` again, timing the whole command (analysis included).

| Measurement (4 cores) | Result |
| --- | --- |
| Cold proof of the project | 4.5 s |
| Re-proof after the change | **0.21 s** (limit 1 s) |
| Functions re-proved | 1 |

Why it is fast: each function's proof depends on its own text, the contracts (not bodies) of
what it calls, the type predicates and the solver version, so editing a body invalidates only
that function. Callers keep their cached proofs because they never saw the body.

## Beyond the gate

- `jig prove` on the PRD's `transfer` example and `examples/bank` proves every contract
  obligation; `tests/prove/semantics` checks soundness case by case (each `bad_*` function
  must fail with a counterexample, each `ok_*` must prove).
- Builds use cached proofs: on `tests/prove/semantics`, 26 of 42 contract checks and 15 of
  35 checked arithmetic operations disappear from the generated C, with identical output.
- On the Phase 1 gate program (`examples/gate1`, written without contracts in mind), 66 of
  109 contract obligations prove; the rest are genuine modular gaps (parsed input passed to
  refined parameters without a check, callees without `ensures`).
