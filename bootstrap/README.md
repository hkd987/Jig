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
stale; `bootstrap/update.sh` rewrites `selfhost/sema/stdlib.jig` (the standard library and
runtime jigc carries, with `stdlib.py`), rebuilds the file with only a C compiler (twice, so a
changed back end compiles itself) and checks the fixed point. `cargo test` fails while it is
stale, and CI checks the fixed point with `bootstrap/update.sh --check` without Rust
installed.

`bootstrap/suites.py` builds `jigc` from this file and runs the test suites through it alone:
the UI annotations, run and native outputs, prover reports, property tests, traceability,
the cookbook, every example and `jigc`'s own tests, modules over git, and the network
example. CI runs it without Rust, with gcc and with clang.

`bootstrap/install.sh [PREFIX]` builds `jigc` from this file and installs it as `jig` in
`PREFIX/bin` (`~/.local/bin` by default). The Rust compiler (`compiler/`) is frozen (D6): it
stays as the reference `compiler/tests/selfhost.rs` compares `jigc` with, pass by pass and
command by command.
