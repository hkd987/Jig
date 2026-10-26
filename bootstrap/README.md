# Bootstrap

`jigc.c` is the Jig compiler (`selfhost/`, written in Jig) compiled to C by itself: one
self-contained file, runtime included, that any C11 compiler builds (decision D6).

```
cc -O2 -std=gnu11 -pthread bootstrap/jigc.c -lm -o jigc
./jigc cc selfhost compiler/std > jigc.c    # the same bytes as bootstrap/jigc.c
```

`jigc cc DIR STD` prints the C program for the package in DIR (STD is the standard library,
`compiler/std`), run from the repository root so the source locations it embeds are
relative. The file is a fixed point: the compiler it builds regenerates it exactly, with gcc
or clang.

Change anything under `selfhost/`, `compiler/std/` or `compiler/runtime/` and the file goes
stale; `bootstrap/update.sh` rebuilds it with only a C compiler (twice, so a changed back
end compiles itself) and checks the fixed point. `cargo test` fails while it is stale, and
CI checks the fixed point with `bootstrap/update.sh --check` without Rust installed.

The Rust compiler (`compiler/`) is kept, and kept passing the same tests, until `jigc`
passes every test suite; `compiler/tests/selfhost.rs` checks that both generate the same C
for every example and run test.
