---
description: Summarize a Jig package for human review - scenarios, contracts, proofs and coverage
argument-hint: [package directory]
---

# Review report

Package: $ARGUMENTS (if empty: the current project).

Produce the report a reviewer reads instead of the implementation. Run the tools; do not
summarize from memory.

1. `jig check` and `jig test`: list failures, if any, first.
2. Scenarios: each `.feature` file's scenarios, with pass or fail.
3. Contracts: from `jig prove --json`, every function with `requires`, `ensures`,
   invariants or refinements, and for each contract whether it is proved, failed (with the
   counterexample) or unknown. Count safety obligations (overflow, bounds, division) proved
   versus kept as runtime checks.
4. Coverage: `jig trace` (summary) and `jig trace uncovered`: which code no scenario
   exercises.
5. Risks: unproven contracts, uncovered code, `panic("todo...")` placeholders left, and
   anything a reviewer should read line by line.

Keep it short: the reviewer needs to know what is guaranteed, what is only tested and what
is neither.
