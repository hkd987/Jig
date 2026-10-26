# Decisions

Provisional decisions taken to unblock the build. Each one is reversible; the PRD owner can
overturn any of them. Newest last.

| # | Decision | Status | Why |
| --- | --- | --- | --- |
| D1 | The language is named **Jig**; files are `.jig`. | Provisional (PRD open question) | Matches the repository name. |
| D2 | New language, not a strict Rust subset plus Verus. | Provisional (PRD open question) | The PRD's design (no lifetimes, effects, typed scenarios, dead code as an error) does not fit inside Rust's surface. |
| D3 | Solver: Z3, pinned (currently 5.3.2 from the `z3-solver` wheel). The bootstrap compiler talks SMT-LIB 2 to a solver process over a pipe. | Provisional (PRD open question) | A text interface keeps cvc5 swappable and needs no linking. The self-hosted compiler moves to the C ABI, as the PRD specifies. The solver version is part of every proof-cache key. |
| D4 | Concurrency: plain structured threads under the `spawn` effect (scoped spawn, joined at scope exit). No async, no green threads. | Provisional (PRD open question) | Smallest model that is safe with value semantics and deterministic to simulate. Implementation is Phase 3. |
| D5 | License: MIT, as already in the repository. Governance deferred to Phase 4. | Provisional (PRD open question) | |
| D6 | Bootstrap path after self-hosting: deferred to Phase 4. | Open | The answer depends on how stable the language is by then. |
| D7 | Memory model: mutable value semantics implemented with non-atomic reference counting and copy-on-write for heap-backed values (`Str`, `List`, `Map`, `Set`, recursive enum payloads, closures). The compiler turns last uses into moves. | Accepted | Value semantics without lifetimes or a GC, as the PRD requires. Atomicity is revisited with D4. |
| D8 | `&mut` appears only on parameter types and call arguments. No reference types in fields, locals or return types. Two `&mut` arguments to one call must be disjoint places. | Accepted | "No references stored inside values" from the PRD, enforced by syntax. |
| D9 | Recursive enum payloads are boxed automatically. | Accepted | Trees stay plain values; agents never write `Box`. |
| D10 | Effects: `io`, `fs`, `net`, `clock`, `rand`, `env`, `spawn`, `ffi`. Pure by default. Panics and failed checks are not effects. | Accepted | The PRD's five plus `ffi`, and `rand`/`env`, which deterministic tests and sandboxing also need. |
| D11 | Methods are declared as `fn Type.name(self, …)`; associated functions as `fn Type.name(…)`. | Accepted | Greppable, mirrors interface declarations, no `impl` blocks. |
| D12 | Generics use angle brackets. In expressions, `<` starts type arguments only if a matching `>` is followed by `(`, `{`, `.`, `)`, `,`, `]` or a line end (the C# rule). | Accepted | Matches the PRD's notation without a turbofish. |
| D13 | Named function bodies return with an explicit `return`; `if`, `match` and closure bodies are expressions whose blocks yield their last expression. | Accepted | Explicit returns where code is long; short forms where it is short. No semicolon subtleties. |
| D14 | Integer overflow, out-of-bounds indexing and division by zero trap in every build unless proven impossible, in which case the check is removed. Unproven `requires`/`ensures` are checked in debug builds only, per the PRD. | Accepted | Memory safety is not optional; contract checks follow the PRD. |
| D15 | Back ends: a MIR interpreter (deterministic test runner), Cranelift (debug builds), LLVM via textual IR compiled by `clang` (release builds). | Superseded by D20 | The PRD's two native back ends, plus an interpreter so tests start in milliseconds with faked effects. |
| D16 | Feature files are standard Gherkin `.feature` files; steps bind with `step "…" fn …` in Jig. | Accepted | PMs write Gherkin; the PRD excludes hooks, tags and backgrounds, which implies standard Gherkin. |
| D17 | Dead-code roots: `main`, the public API of a library root package, and step functions used by a scenario. Tests do not justify code. | Accepted | "Nothing exists unless a feature justifies it." |
| D18 | `Map` and `Set` iterate in insertion order. | Accepted | Deterministic tests and output. |
| D19 | No serde or parser libraries in the bootstrap compiler, and no other large dependencies. | Accepted | Compile speed and the PRD's hand-written front end rule. |
| D20 | The native back end emits C from MIR. Debug builds: `cc -O0`, split into translation units compiled in parallel and cached by content hash. Release builds: one unit through `clang -O2` (LLVM), contract checks dropped, arithmetic and bounds checks kept. Cranelift stays an option for an in-process debug path later. | Accepted | One code generator instead of two; C gives the platform ABI, struct layout and C interop for free; `clang -O2` *is* the LLVM release path. Cold debug build of a 20k-line program: 3.5 s on 4 cores, 0.3 s when cached; release hello world 60 KB. |
