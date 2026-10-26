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
                    interpreter     Cranelift          LLVM IR
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
- [ ] Cargo workspace, `jig` binary, CI skeleton (build, unit tests, fmt, clippy).
- [ ] Utilities: FxHash, string interner, typed index arenas, minimal JSON reader/writer
      (no serde, keeps compile times down).
- [ ] Diagnostics: code registry (`E0xxx`), spans, labels, notes, fixes; JSON and human
      renderers; `jig explain <code>`.

### 1.2 Front end
- [ ] Hand-written lexer with Go-style automatic semicolon insertion.
- [ ] Hand-written recursive-descent parser into an index-based AST (no per-node heap allocation).
- [ ] Name resolution across packages (package = directory; import cycles are errors).
- [ ] Type checker: structs, enums (sum types), tuples, generics (monomorphized, no trait
      search), structural interfaces as bounds, built-in `Eq`/`Ord`/`Hash`, `Option`/`Result`
      and `?`, methods, closures, no implicit conversions.
- [ ] Mutable value semantics: `&mut` only on parameters and call arguments; exclusivity
      check at call sites; `var` vs `let`.
- [ ] Effects: declared per function, pure by default, checked transitively.
- [ ] Exhaustive `match` and unreachable-arm detection.
- [ ] Refinement and contract syntax parsed and type-checked (proved in Phase 2).
- [ ] Units of measure on `Int` and `Float`.

### 1.3 Strictness
- [ ] Lints as errors: unused variables/imports/results, needless `var`, naming, shadowing,
      unreachable code, self-comparison.
- [ ] Dead code as an error: unreachable functions, unused fields, unused exports, impossible
      match arms, unused step functions.
- [ ] Canonical formatter (`jig fmt`), no options; comment-preserving.

### 1.4 Back end
- [ ] MIR: CFG with typed locals and places, monomorphization, copy/drop glue, last-use moves.
- [ ] MIR interpreter: the deterministic runner for tests (fake `io`, `clock`, `rand`, `fs`).
- [ ] Fast native backend on Cranelift + small C runtime; `jig build`, `jig run`.
- [ ] Runtime checks: overflow, bounds, division by zero, contracts in debug builds.

### 1.5 Agent plugin (minimal) and docs
- [ ] Language reference under 10k tokens (`docs/reference.md`), shipped as the plugin skill.
- [ ] Claude Code plugin: reference skill, edit hook running `jig check --json`, commit hook,
      MCP server (`jig mcp`) with check/explain/search tools.
- [ ] Std library: `io`, `fs`, `os`, `time`, `rand`, `math`, collections.

### 1.6 Performance from day one
- [ ] Benchmark corpus (examples + generated 100k-line project).
- [ ] Instruction-count benchmarks (Valgrind) in CI; >2% regression blocks merge without
      a `perf-signoff` label.
- [ ] Incremental check: per-package results cached by content hash.

### 1.7 Gate
- [ ] Agent-written 1,000-line program built with only the plugin reference
      (`examples/gate1/`), with the transcript summary in `docs/gates/phase1.md`.

## Phase 2 · Verification

Goal: contracts and refinement types proved by SMT, per function, cached; same-file unit
tests; generated property and fuzz tests for whatever isn't proved.

**Gate:** re-proof under 1 s after a one-function change.

- [ ] VC generation from the typed AST: `requires`, `ensures` (`result`, `old`), loop
      `invariant`, struct `invariant`, refinements, `assert`, safety obligations (overflow,
      bounds, division, `unwrap`, `panic` reachability).
- [ ] Modular proofs: callers see callees' contracts, never bodies.
- [ ] SMT-LIB backend over a pinned Z3 process (solver-agnostic text interface, so cvc5
      stays an option); timeouts; minimal counterexamples mapped back to source values.
- [ ] Proof cache keyed by body + own contract + callee contracts + solver version;
      local directory, shareable via CI cache.
- [ ] Proven obligations elide their runtime checks; unproven ones keep a debug check and
      get a generated property test.
- [ ] `test` blocks with parameters as property tests; generated property/fuzz tests from
      unproven contracts; counterexample replay.
- [ ] Plugin: `prove` and `explain counterexample` tools; proof-hint library skill.
- [ ] Gate measurement in CI (`bench/reprove`).

## Phase 3 · Features and performance

Goal: typed Gherkin scenarios that bind to code at compile time, traceability, and an
optimizing release backend.

**Gate:** within 10% of Rust on the benchmark suite.

- [ ] Gherkin subset parser (`Feature`, `Scenario`, `Scenario Outline`/`Examples`,
      `Given/When/Then/And/But`); no hooks, tags or backgrounds.
- [ ] Typed step binding: `step "pattern {param}" fn …`; missing, ambiguous or mistyped steps
      are compile errors; unused steps are dead code.
- [ ] Scenarios run as tests in the deterministic runner.
- [ ] Traceability: `jig trace` answers "which scenarios cover this function", "what is
      reachable but uncovered", "what dies if this scenario is deleted"; JSON + text.
- [ ] Deterministic simulation for `clock`, `rand`, `net`, `fs`, `spawn`.
- [ ] Structured concurrency under the `spawn` effect.
- [ ] LLVM release backend (textual IR → `clang -O2`), whole-program dead-code elimination,
      small binaries (hello world < 100 KB).
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
