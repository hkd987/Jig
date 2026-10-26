---
description: Draft signatures, contracts and step functions for approved scenarios, then stop for approval (spec-first step 2 of 3)
argument-hint: [feature file or scenario names]
---

# Spec-first, step 2: contracts

Scope: $ARGUMENTS (if empty: the scenarios approved in step 1).

Write the **interface** the scenarios need, with contracts and placeholder bodies. Do not
implement anything yet: the contracts are what a human reviews instead of the code.

1. For every step the check reports as missing (E0802), add the suggested step function in a
   steps file, typed to the placeholders (`Str`, `Int`, `Float`, `Bool`, or a refinement
   alias such as `Cents`). Give the steps a world struct with `fn World.new() -> World`.
2. Declare the domain functions and types the steps will call. For each, write what a
   reviewer needs to trust it:
   - refinement aliases for constrained values (`type Cents = Int where self >= 0`);
   - struct `invariant`s for what must always hold;
   - `requires` for what callers must guarantee and `ensures` for what the function
     promises (`old(...)` for the value before the call).
   Prefer contracts that are short and checkable over ones that restate the body.
3. Give every body a placeholder: `return panic("todo: <name>")` (or `panic("todo")` for
   functions returning `()`). Step bodies may call the domain functions already.
4. Run `jig check` until it is clean, then `jig prove` to check the contracts are consistent
   (every call site satisfies its callee's `requires`). The placeholders show up as
   `may trap panic`; that is expected.
5. Show the user the signatures and contracts, grouped by type, and say which scenario
   each one serves.

**Stop here.** Ask the user to approve or change the contracts. When they approve, the next
step is `/jig:implement`.
