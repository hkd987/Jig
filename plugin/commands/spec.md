---
description: Draft feature scenarios for a Jig change and stop for approval (spec-first step 1 of 3)
argument-hint: <what the feature should do>
---

# Spec-first, step 1: scenarios

The user wants: $ARGUMENTS

Write **only** the scenarios. No Jig code yet: the scenarios are the specification a human
approves before anything is built.

1. Read the package's existing `.feature` files and step functions (`step "..."`) so new
   scenarios reuse existing steps and phrasing where they fit.
2. Add or extend a `.feature` file in the package directory. Use the supported subset only:
   `Feature`, `Scenario`, `Scenario Outline` with `Examples`, and `Given`/`When`/`Then`/`And`/
   `But` (no tags, backgrounds, tables or doc strings). One behavior per scenario; cover the
   normal case, the edges (empty, zero, limits) and the failures the user cares about. Put
   concrete values in the steps (`Given an account "A" with balance 100`), so each becomes a
   typed placeholder later.
3. Run `jig check` (the `check` tool). New steps report E0802 with a suggested step
   function; that is expected now. Fix anything else it reports about the feature file.
4. Show the user the scenarios, list the new steps the check suggested, and point out any
   behavior you had to guess.

**Stop here.** Ask the user to approve or change the scenarios. When they approve, the next
step is `/jig:contracts`.
