# Phase 5 gate: closing the gaps

The README's known gaps, one row each, with where they stand. "Done" means the feature works
in both compilers, has tests, its contracts prove, and an example or the docs site uses it.
Some rows are research rather than implementation; they say so.

| Gap | Plan | Status |
| --- | --- | --- |
| HTML rendering in the standard library | `std/html` | done: 5 of its 7 functions prove completely; `examples/ssr` and the site use it |
| Docs site | `site/`, a Jig program, deployed to GitHub Pages (D32) | done: 15 pages, built the same by both compilers in CI; deploying waits on Pages being turned on |
| Dates and times | `std/time` calendar dates and ISO 8601 | done: UTC dates in years 0 to 9999, ISO 8601 both ways, HTTP dates; every function proves; a round-trip property test |
| JSON | `std/json` | done: strict RFC 8259 reader, compact and indented writers; every contract proves; round-trip property tests; the guestbook's `/api/entries` |
| Hashing | `std/hash`: SHA-256, HMAC-SHA-256 | done: matches FIPS 180-4 and RFC 4231 vectors; every obligation proves; the site versions its stylesheet with it |
| Regular expressions | `std/regex`, linear-time matching | done: a Pike VM, so matching is linear in the text; Perl's choice among matches and captures; every contract proves; `(a*)*b` and the like on 5,000 characters in the tests |
| Built-in methods take only pure functions | effect parameters on `map`, `filter`, `fold`, ... | done: thirteen functions in the prelude and `std/lists` take their effects from their function argument; proofs about their callers are unchanged |
| No HTTP client, no TLS | `std/net` requests over http and https, OpenSSL (D33) | done: `net.get`, `post`, `send` over `net.fetch`; TLS through OpenSSL opened at run time, certificates and names checked; `examples/fetch` against a local HTTPS server in CI, natively and on both interpreters |
| One connection at a time | concurrent serving (D34) | done: `net.serve_http_parallel` answers on a pool of threads natively; `examples/api` answers eight 400 ms requests at once in CI with both compilers |
| C interop: numbers and strings only | buffers, pointers, structs, callbacks (D35) | done for buffers (`List<U8>`, `&mut List<U8>`), pointers (`Int`) and structs (buffers read by offset), natively and on both interpreters; callbacks are out: jigc's interpreter cannot be re-entered from C (D35) |
| Prover: `starts_with`, `join`, bit operations | models in both provers | done for bit operations (exact for constant shifts, masks and `~`, bounded otherwise) and the length facts of `starts_with`/`ends_with`/`contains`; string contents and `join` stay out, as strings are lengths to the prover |
| Prover: invariants over a table's every element (D29) | quantified invariants checked incrementally | open; research: the design is not settled |
| Checker speed | 100k lines under 100 ms with jigc | open; research. jigc checks on one thread, and checking on several needs reference counts that stay cheap on the thread that owns a value (phase 4's report). Today's additions left the check where it was: instruction counts are within CI's 2% gate on every push, and wall times on this machine (0.40 to 0.48 s for the 98k-line corpus) show no trend across the day's commits |
| Float-heavy code behind Rust | nbody within 10% | not met; the cause is measured, and it is not in Jig's code. With `advance` kept out of line (`#[inline(never)]`), Rust runs 5 million steps in 461 ms, the time Jig takes (433 to 470 ms): the inner loops are equal, and Jig's has no checks left in it. The whole gap is LLVM inlining a step into `main`, seeing that the vector holds five bodies, and unrolling and vectorizing the step (Rust: 313 ms). The same program written in C with that knowledge given (a stack array of five, the step inlined) takes 434 ms under clang 18, which CI and Jig's release builds use, and 332 ms only under clang 20 at `-O3`; so a Jig that inlined calls and sized lists itself would still be bound by its C compiler. 1.47 in instructions, 1.18 to 1.4 in time |
| Linux only | macOS in CI and the release | done: CI's macOS job (Apple silicon) checks the fixed point and passes every suite with jigc; the release builds for both |
| No editor support | `jig lsp` and a VS Code extension (D36) | done: diagnostics as you type, hover, definition and formatting, tested by a scripted session in jigc's suites and against vscode-jsonrpc; `editors/vscode` packages in CI. Completion, references and rename are not done |
| No package registry | an index on the site; modules resolve by URL and tag | done: [`docs/packages.md`](../packages.md), on the site as Packages: using and publishing a module, and the index (published by pull request); a hosted registry is outside this repository |
| No Windows | port the runtime (D37) | done: the runtime and jigc build with MinGW-w64's gcc (Winsock, `LoadLibrary` for C libraries and OpenSSL, binary-mode streams, `cmd.exe` quoting, paths with drives, Windows x64 calls from jigc's interpreter). CI's Windows job builds `jig.exe` from `bootstrap/jigc.c`, checks that it reproduces that file, and passes every suite (native builds, HTTPS, the language server, modules, proofs); the release publishes a Windows zip. Not done: MSVC, ARM64, Windows' certificate store |
| Agent turns | three runs per task and language | done: three runs of each of the six tasks in each language (below); every run reached a verified build. Jig took 6.7 turns on average, Rust 6.3 and TypeScript 6.1, so the PRD's "equal or fewer" is still not met, by less than the one-turn spread between runs of the same task |

## Agent runs

`bench/agents/run.py` on 2026-10-28, with jigc at 41daf20 as `jig`: a Claude Code agent
(`claude -p`, at most 40 turns, the same short list of tools in every language), three
times per task and language (`bench/agents/results/2026-10-28.json`). Turns per run:

| Task | Jig | Rust | TypeScript |
| --- | --- | --- | --- |
| intervals | 6, 6, 6 | 6, 6, 6 | 6, 6, 6 |
| ledger | 7, 6, 8 | 6, 7, 7 | 6, 6, 7 |
| maze | 6, 6, 6 | 6, 6, 6 | 6, 6, 6 |
| roman | 9, 7, 7 | 6, 6, 10 | 7, 6, 6 |
| rpn | 8, 6, 6 | 6, 6, 6 | 6, 6, 6 |
| wordfreq | 9, 6, 6 | 6, 6, 6 | 6, 6, 6 |
| Mean | 6.7 | 6.3 | 6.1 |

All 54 runs solved their task, and most took six turns in every language. Three more Jig
runs kept their transcripts (`run.py --transcripts`) to show what the turns above six are.
Most are the agent's own extra care, such as probing amounts near 2^63 in `ledger` or
dumping the cases with `cat -A`. One was the language: a lookup table written as
`const VALUES: List<Int> = [...]` failed with E0329, since a constant could only be a
literal or arithmetic on other constants.
