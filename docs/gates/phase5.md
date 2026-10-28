# Phase 5 gate: closing the gaps

The README's known gaps, one row each, with where they stand. "Done" means the feature works
in both compilers, has tests, its contracts prove, and an example or the docs site uses it.
Some rows are research rather than implementation; they say so.

| Gap | Plan | Status |
| --- | --- | --- |
| HTML rendering in the standard library | `std/html` | done: 5 of its 7 functions prove completely; `examples/ssr` and the site use it |
| Docs site | `site/`, a Jig program, deployed to GitHub Pages (D32) | done: 14 pages, built the same by both compilers in CI; deploying waits on Pages being turned on |
| Dates and times | `std/time` calendar dates and ISO 8601 | done: UTC dates in years 0 to 9999, ISO 8601 both ways, HTTP dates; every function proves; a round-trip property test |
| JSON | `std/json` | done: strict RFC 8259 reader, compact and indented writers; every contract proves; round-trip property tests; the guestbook's `/api/entries` |
| Hashing | `std/hash`: SHA-256, HMAC-SHA-256 | done: matches FIPS 180-4 and RFC 4231 vectors; every obligation proves; the site versions its stylesheet with it |
| Regular expressions | `std/regex`, linear-time matching | done: a Pike VM, so matching is linear in the text; Perl's choice among matches and captures; every contract proves; `(a*)*b` and the like on 5,000 characters in the tests |
| Built-in methods take only pure functions | effect parameters on `map`, `filter`, `fold`, ... | done: thirteen functions in the prelude and `std/lists` take their effects from their function argument; proofs about their callers are unchanged |
| No HTTP client, no TLS | `std/net` requests over http and https, OpenSSL (D33) | done: `net.get`, `post`, `send` over `net.fetch`; TLS through OpenSSL opened at run time, certificates and names checked; `examples/fetch` against a local HTTPS server in CI, natively and on both interpreters |
| One connection at a time | concurrent serving (D34) | done: `net.serve_http_parallel` answers on a pool of threads natively; `examples/api` answers eight 400 ms requests at once in CI with both compilers |
| C interop: numbers and strings only | buffers, pointers, structs, callbacks (D35) | open; callbacks into the interpreter need a trampoline per signature |
| Prover: `starts_with`, `join`, bit operations | models in both provers | done for bit operations (exact for constant shifts, masks and `~`, bounded otherwise) and the length facts of `starts_with`/`ends_with`/`contains`; string contents and `join` stay out, as strings are lengths to the prover |
| Prover: invariants over a table's every element (D29) | quantified invariants checked incrementally | open; research: the design is not settled |
| Checker speed | 100k lines under 100 ms with jigc | open |
| Float-heavy code behind Rust | nbody within 10% | open |
| Linux only | macOS in CI and the release | done: CI's macOS job (Apple silicon) checks the fixed point and passes every suite with jigc; the release builds for both |
| No editor support | `jig lsp` and a VS Code extension (D36) | open |
| No package registry | an index on the site; modules resolve by URL and tag | open; a hosted registry is outside this repository |
| No Windows | port the runtime | open; a port of sockets, threads and C loading |
| Agent turns | three runs per task and language | open; needs agent runs outside CI |
