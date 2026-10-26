---
description: Implement approved contracts until scenarios, tests and proofs pass (spec-first step 3 of 3)
argument-hint: [function or file names]
---

# Spec-first, step 3: implement

Scope: $ARGUMENTS (if empty: every `panic("todo...")` placeholder from step 2).

Replace the placeholders with implementations. The approved scenarios and contracts are
fixed: if one turns out to be wrong or impossible, stop and ask instead of changing it.

1. Implement one function at a time. The edit hook runs `jig check` after every edit; fix
   every error it reports before moving on.
2. Run `jig test` (the `test` tool). Every scenario runs as a test; a failure names the
   step and its line. Add unit tests (`test` blocks) for logic the scenarios don't reach,
   and property tests (`test "..."(x: Int) requires ...`) for contracts the prover can't
   discharge.
3. Run `jig prove` (the `prove` tool). Every FAILED contract needs a fix; read the
   counterexample (the `jig-proofs` skill explains how). "confirmed" means a real bug;
   "not reproduced" means a missing fact: a callee `ensures`, a loop `invariant`, or a
   `requires`.
4. Run `jig trace uncovered` (the `trace` tool). Code that no scenario exercises either
   needs a scenario (ask the user) or should not exist.
5. Finish with `jig fmt`, then report: scenarios passing, contracts proved (and any left
   unproven with why), and what `jig trace` shows.
