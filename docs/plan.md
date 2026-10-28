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
- [x] Last-use moves: elide the retain/release pair when a value's last use is a copy
      (a liveness pass over MIR in the C back end; also borrowed locals, `codegen/own.rs`).
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
- [x] Check under 100 ms on the 98k-line benchmark corpus (4 cores): packages are parsed in
      parallel waves and bodies checked on several threads (D25), 63–80 ms for a full check,
      down from 110–137 ms. `compiler/tests/parallel.rs` checks every example, the
      self-hosted compiler and every UI and run test on one and four threads and requires
      identical results. Per-package caching by content hash remains possible on top.

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
- [x] Modular proofs: callers see callees' contracts, and of their bodies only what they
      can write and, for pure functions, which scalars they read (so a call is a function
      of those).
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
- [x] Deterministic simulation for `clock`, `rand`, `net`, `fs`, `spawn`: tests run against a
      simulated world that `std/sim` sets up and inspects (captured output, stdin, args, env,
      in-memory files, a clock moved by `time.sleep`, a seed, queued network replies and
      connections); tasks are deterministic by construction; `std/net` (D23) over real TCP.
- [x] Structured concurrency under the `spawn` effect: fork-join `std/task` (`all`, `map`,
      `both`, D4); tasks may only spawn, so they are deterministic; a persistent worker pool
      with nested batches; atomic reference counts only in programs that use tasks.
- [x] LLVM release backend: one C unit through `clang -O2` (D20), internal linkage so unused
      code is dropped; release hello world is 60 KB.
- [x] Benchmark suite vs. Rust (same programs in both languages), tracked in CI:
      `bench/vsrust`, eight programs.
- [x] **Gate passed:** geometric mean 0.97x Rust by wall clock and 0.89x by instructions
      (limit 1.10); CI fails above the limit; `docs/gates/phase3.md`.
- [x] Plugin: spec-first commands (`/jig:spec` → `/jig:contracts` → `/jig:implement`, each
      stopping for approval; `/jig:review` for the reviewer's report), trace tools (MCP `trace`).

## Phase 4 · Self-hosting and alpha

Goal: agents rewrite the compiler in Jig through the plugin, with proven contracts on its
core passes.

**Gate:** the compiler compiles itself; public alpha release. Status in
[`gates/phase4.md`](gates/phase4.md): `jigc` compiles itself to a fixed point checked in CI,
and 0.1.0-alpha is ready to tag (`.github/workflows/release.yml`).

- [x] Std library in Jig, with > 80% proven contracts. The prelude's Jig functions and the
      Jig packages `std/lists`, `std/strings` and the integer half of `std/math` state
      contracts: 54 of 58 functions, 53 of which prove completely (91%); every contract
      obligation proves (`jig prove --std`, enforced by `compiler/tests/prove.rs`), so builds
      do not check std postconditions again. I/O, collections and formatting stay runtime
      intrinsics. Generic functions are proved with their type parameters abstract, and the
      prover models what string searches, slices and splits imply about lengths.
- [x] C interop behind the `ffi` effect (D26): bodiless functions with `effects {ffi}` call C
      symbols, with scalar and string signatures, runtime-checked contracts and `link`
      lines in `jig.mod`; native code and the interpreter both call C (`examples/normal`
      uses libm's `erf`). The self-hosted compiler will reach the SMT solver and LLVM this
      way once it needs pointers to their handles.
- [x] Package manager (D27): `require`/`replace` in `jig.mod`, `jig get` downloading git
      tags into a module cache with commits pinned in `jig.sum`, highest-version selection,
      imports through modules (`compiler/tests/modules.rs`).
- [ ] Self-hosted compiler: lexer, parser, checker, MIR, backends, ported pass by pass;
      arena-and-index design carries over directly. `selfhost/` (`jigc`): the lexer, parser
      and checker are done. Token and AST dumps match the bootstrap compiler's on every
      repository file and 400 mutations, most of them with syntax errors, so error recovery
      is compared too; the checker's dump (every diagnostic, and the type of every
      expression and pattern) matches on every example, UI and run test, `jigc` itself and
      300 semantic mutations raising over fifty error codes (`compiler/tests/selfhost.rs`).
      `bench/selfhost.py` compares speed on the same corpus (CI gate: instruction ratio at
      most 1.05, now 0.94); jigc is at 0.92 (lexer), 0.61 (parser), 1.33 (checker) and 1.03
      (checker through C) of the bootstrap compiler's instructions. MIR lowering, the ownership analyses and the C back
      end are done too: `jigc c` prints byte-identical C to `jig debug c` for every example,
      run test, agent benchmark solution and `jigc` itself, and `jigc` compiles itself to a
      fixed point (below). It binds `.feature` scenarios as well (300 mutated feature
      files and step patterns compared), resolves imports through required modules, and
      formats like `jig fmt` (every repository file and 600 layout mutations compared).
      It runs tests too: its interpreter and test runner print what `jig test --json`
      prints for every package with tests and 150 mutations, C calls included. And it
      proves: its verification conditions are the bootstrap's byte for byte (`jigc vc` =
      `jig debug vc`; 5.7 million lines for jigc itself), and `jigc prove` reports the same
      statuses, counterexamples and replays as `jig prove`, and shares its proof cache (the
      same keys and files); `jigc build` removes the checks cached proofs discharge, with
      the same C as `jig build`. Its command line is `jig`'s, every command: for the same
      input each prints what `jig` prints (diagnostics with notes and fixes, test and proof
      reports, `run` with real effects, `get`, `trace`, `search`, `traces`, the hooks, the
      MCP server), with the standard library built in.
- [ ] Agent benchmark vs. Rust and TypeScript. `bench/agents`: six tasks with acceptance cases
      and reference solutions in all three languages (checked in CI), and a harness that runs
      an agent per task and language and reports tasks solved and turns. Results need agent
      runs, which are not part of CI.
- [x] Opt-in trace corpus: `jig traces on` makes the plugin's hooks record edits (text and
      errors) and verified commits locally; `jig traces export` writes them out.
- [x] Bootstrap path decided (D6): a checked-in C bootstrap generated by `jigc` itself, with a
      fixed-point check; the Rust compiler is kept until `jigc` passes every test suite.
- [x] `jigc` passes every test suite on its own (`bootstrap/suites.py`, in CI without Rust)
      and is the `jig` command (`bootstrap/install.sh`); the Rust compiler is frozen as the
      reference the differential tests compare with (D6).
- [x] Self-compilation: `bootstrap/jigc.c` (one C file, runtime included) is `jigc`'s own
      output for its sources; `cc` alone builds it, and the `jigc` it builds regenerates it
      byte for byte with gcc and clang. CI checks that fixed point without Rust
      (`bootstrap/update.sh --check`) and `cargo test` fails when the file is stale.

## Phase 5 · Closing the gaps

Goal: everything the README lists as a known gap works in Jig, in both compilers, with tests,
contracts that prove and an example or a page that uses it. What cannot be closed in this
phase stays open in [`gates/phase5.md`](gates/phase5.md) with the reason.

**Gate:** every item below checked, or open in the gate report with its reason.

Pure Jig in the standard library (both compilers get it as it is; every new function proves,
keeping the standard library over 80%):

- [x] `std/html`: pages as trees of nodes rendered with every text and attribute value
      escaped. Its constructors, `escape` and the opening tag prove; `examples/ssr` uses it.
- [x] Docs site (option 1, D32): `site/` is a Jig program that renders `docs/*.md` to HTML
      with `std/html` (headings with GitHub's anchors, paragraphs, lists, tables, fenced and
      inline code, emphasis, links rewritten for the site), deployed to GitHub Pages by
      `.github/workflows/pages.yml`. Both compilers build the same site, every internal link
      and anchor resolves, and its tests cover the Markdown it reads.
- [ ] `std/time` dates: UTC calendar dates from timestamps and back, ISO 8601 formatting and
      parsing.
- [ ] `std/json`: a `Json` value, `parse` and `str`.
- [ ] `std/hash`: SHA-256 and HMAC-SHA-256, checked against the published test vectors.
- [ ] `std/regex`: literals, `.`, classes, anchors, `* + ? {m,n}`, groups, alternation and
      captures, matched in time linear in the input.

Changes to both compilers:

- [ ] Built-in methods (`map`, `filter`, `fold`, `any`, `all`, `sort_by`, ...) take functions
      with effects, as effect parameters do (D31).
- [ ] An HTTP client and TLS (D33): `http` and `https` requests from `std/net`, TLS through the
      system's OpenSSL, linked only by programs that use it; simulated in tests.
- [ ] Concurrent serving (D34): a server that answers connections in parallel.
- [ ] C interop beyond numbers and strings (D35): byte buffers in and out, opaque pointers,
      structs passed by pointer, and callbacks.
- [ ] The prover models `starts_with`, `ends_with`, `join` and bit operations on
      non-negative integers.
- [ ] Table invariants (D29): invariants over every element of a list, checked where an
      element changes and assumed by the prover, applied to the compiler's arenas.
- [ ] Checker speed: `jig check` on 100k lines under 100 ms with jigc.
- [ ] Float-heavy code within 10% of Rust (nbody).

Tools and platforms:

- [ ] macOS in CI and in the release.
- [ ] Editor support (D36): `jig lsp` (diagnostics, types on hover, go to definition,
      formatting) and a VS Code extension.
- [ ] Modules: a package index on the site. Modules already resolve by URL and git tag, as
      Go's do; hosting a registry service is outside what this repository can deploy.
- [ ] Windows: the runtime is POSIX (sockets, threads, `dlopen`), so it needs a port.
- [ ] Agent benchmark: three runs per task and language, to separate the turn counts from
      the noise.

## Working rules

- Every change keeps `cargo test`, `jig check` on all examples, and the formatter idempotence
  test green.
- Hot-path rules from the PRD apply to the bootstrap compiler: flat arenas and indices, interned
  symbols, FxHash, no `Rc<RefCell<_>>` graphs, no casual `.clone()`, no per-node allocation.
- Decisions that the PRD left open are recorded in `docs/decisions.md` as provisional and
  reversible.
