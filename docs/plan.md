# Jig build plan

Source: [Agent-First Verified Language — Initial PRD](https://claude.ai/artifact/T7PT1aUWZtF2a7L6Dv6eZ3).
This plan turns the PRD into work. Phases are sequenced by gate, not date. Each phase ships
only when its gate passes. Status boxes are updated as work lands on `main`.

The language is named **Jig** (the repository name; the PRD left the name open). Source
files use `.jig`; feature files use `.feature`.

## Architecture at a glance

One binary, `jig`, written in Rust for Phases 1–3 (the bootstrap compiler), rewritten in Jig
in Phase 4.

```
.jig / .feature ──► lexer ─► parser ─► AST (flat arenas, interned symbols)
                                        │
                     resolve ─► typecheck ─► effects ─► exhaustiveness ─► lints ─► dead code
                                        │                      (millisecond tier: `jig check`)
                                        ▼
                                   typed AST ──► VC gen ─► SMT-LIB ─► Z3   (`jig prove`, cached)
                                        │
                                        ▼
                                  MIR (CFG, monomorphized)
                         ┌──────────────┼───────────────┐
                    interpreter    C ─► cc -O0      C ─► clang -O2 (LLVM)
              (tests, scenarios,   (debug builds)   (release builds)
               property tests)
```

Every stage reports through one diagnostics module: stable code, span, counterexample,
candidate fixes, rendered as JSON for agents and as text for humans.

## Phase 1 · Core language

Goal: a strict, Go-shaped language with a millisecond check loop, a native fast backend,
structured diagnostics and a working agent plugin.

**Gate:** an agent writes a 1,000-line program using only the plugin reference.

### 1.1 Foundations
- [x] Cargo workspace, `jig` binary, CI skeleton (build, unit tests, fmt, clippy).
- [x] Utilities: FxHash, string interner, typed index arenas, minimal JSON reader/writer
      (no serde, keeps compile times down).
- [x] Diagnostics: code registry (`E0xxx`), spans, labels, notes, fixes; JSON and human
      renderers; `jig explain <code>`.

### 1.2 Front end
- [x] Hand-written lexer with Go-style automatic semicolon insertion.
- [x] Hand-written recursive-descent parser into an index-based AST (no per-node heap allocation).
- [x] Name resolution across packages (package = directory; import cycles are errors).
- [x] Type checker: structs, enums (sum types), tuples, generics (monomorphized, no trait
      search), structural interfaces as bounds, built-in `Eq`/`Ord`/`Hash`, `Option`/`Result`
      and `?`, methods, closures, no implicit conversions.
- [x] Mutable value semantics: `&mut` only on parameters and call arguments; exclusivity
      check at call sites; `var` vs `let`.
- [x] Effects: declared per function, pure by default, checked transitively.
- [x] Exhaustive `match` and unreachable-arm detection.
- [x] Refinement and contract syntax parsed and type-checked (proved in Phase 2).
- [x] Units of measure on `Int` and `Float`.

### 1.3 Strictness
- [x] Lints as errors: unused variables/imports/results, needless `var`, naming, shadowing,
      unreachable code, self-comparison.
- [x] Dead code as an error: unreachable functions, unused fields, unused exports, impossible
      match arms, unused step functions.
- [x] Canonical formatter (`jig fmt`), no options; comment-preserving.

### 1.4 Back end
- [x] MIR: CFG with typed locals and places, monomorphization, copy/drop glue.
- [ ] Last-use moves: elide the retain/release pair when a value's last use is a copy.
- [x] MIR interpreter: the deterministic runner for tests (fake `io`, `clock`, `rand`, `fs`).
- [x] Fast native backend: MIR → C + small C runtime (`compiler/runtime/`), parallel
      per-unit compilation with an object cache; `jig build`, `jig run --native` (D20).
- [x] Runtime checks: overflow, bounds, division by zero, casts, contracts in debug builds.

### 1.5 Agent plugin (minimal) and docs
- [x] Language reference under 10k tokens (`docs/reference.md`), shipped as the plugin skill.
- [x] Claude Code plugin: reference skill, edit hook running `jig check --json`, commit hook,
      MCP server (`jig mcp`) with check/explain/search tools.
- [x] Std library: `io`, `fs`, `os`, `time`, `rand`, `math`, collections.

### 1.6 Performance from day one
- [x] Benchmark corpus (examples + generated 100k-line project).
- [x] Instruction-count benchmarks (Valgrind) in CI; >2% regression blocks merge without
      a `perf-signoff` label (or a `[perf-signoff]` tag on a direct push to `main`).
- [ ] Incremental check: per-package results cached by content hash.

### 1.7 Gate
- [x] Agent-written 1,000-line program built with only the plugin reference
      (`examples/gate1/`, 1,096 lines plus 35 tests), with the report in
      `docs/gates/phase1.md`. Passed; its compiler bugs are fixed and its reference gaps
      closed (status table in the report).

## Phase 2 · Verification

Goal: contracts and refinement types proved by SMT, per function, cached; same-file unit
tests; generated property and fuzz tests for whatever isn't proved.

**Gate:** re-proof under 1 s after a one-function change.

- [x] VC generation from the typed AST: `requires`, `ensures` (`result`, `old`), loop
      `invariant`, struct `invariant`, refinements, `assert`, safety obligations (overflow,
      bounds, division, `unwrap`, `panic` reachability). Encoding in D21.
- [x] Modular proofs: callers see callees' contracts, never bodies.
- [x] SMT-LIB backend over a pinned Z3 process (solver-agnostic text interface, so cvc5
      stays an option); timeouts; counterexamples mapped back to source values; one solver
      per core, reset per function so results are deterministic.
- [x] Proof cache keyed by body + own contract + callee contracts + type predicates + std +
      solver version; `.jig/proofs`, shareable via CI cache.
- [x] Proven obligations elide their runtime checks in builds (contract checks and
      arithmetic traps); unproven ones keep their debug check.
- [x] `test` blocks with parameters as property tests: generation by type, `requires` as a
      filter, shrinking.
- [x] Counterexample replay: failed contracts are re-run on the interpreter with the
      solver's inputs (or, when those can't be rebuilt, up to 200 generated inputs) and
      reported as confirmed bugs or missing facts.
- [x] Plugin: `prove` MCP tool with per-obligation hints; `jig-proofs` skill.
- [x] Gate measurement in CI (`bench/reprove.py`): 0.21 s re-proof of a 20k-line project
      after a one-function change (limit 1 s); `docs/gates/phase2.md`.

## Phase 3 · Features and performance

Goal: typed Gherkin scenarios that bind to code at compile time, traceability, and an
optimizing release backend.

**Gate:** within 10% of Rust on the benchmark suite.

- [x] Gherkin subset parser (`Feature`, `Scenario`, `Scenario Outline`/`Examples`,
      `Given/When/Then/And/But`); no hooks, tags or backgrounds (rejected with E0801).
- [x] Typed step binding: `step "pattern {param}" fn …`; missing (with a suggested step),
      ambiguous or mistyped steps are compile errors; unused steps are dead code.
- [x] Scenarios run as tests in the deterministic runner; failures name the step line.
- [x] Traceability: `jig trace` answers "which scenarios cover this function", "what is
      reachable but uncovered", "what dies if this scenario is deleted"; JSON + text; MCP
      `trace` tool.
- [ ] Deterministic simulation for `clock`, `rand`, `net`, `fs`, `spawn`.
- [ ] Structured concurrency under the `spawn` effect.
- [x] LLVM release backend: one C unit through `clang -O2` (D20), internal linkage so unused
      code is dropped; release hello world is 60 KB.
- [ ] Benchmark suite vs. Rust (same programs in both languages), tracked in CI.
- [ ] Plugin: spec-first commands (draft scenarios → draft contracts → implement), trace tools.

## Phase 4 · Self-hosting and alpha

Goal: agents rewrite the compiler in Jig through the plugin, with proven contracts on its
core passes.

**Gate:** the compiler compiles itself; public alpha release.

- [ ] Std library completed in Jig, with > 80% proven contracts.
- [ ] C interop behind the `ffi` effect (SMT solver and LLVM called through the C ABI).
- [ ] Package manager.
- [ ] Self-hosted compiler: lexer, parser, checker, MIR, backends, ported pass by pass;
      arena-and-index design carries over directly.
- [ ] Agent benchmark vs. Rust and TypeScript; opt-in trace corpus.
- [ ] Bootstrap path decided (see open questions).

## Working rules

- Every change keeps `cargo test`, `jig check` on all examples, and the formatter idempotence
  test green.
- Hot-path rules from the PRD apply to the bootstrap compiler: flat arenas and indices, interned
  symbols, FxHash, no `Rc<RefCell<_>>` graphs, no casual `.clone()`, no per-node allocation.
- Decisions that the PRD left open are recorded in `docs/decisions.md` as provisional and
  reversible.
