#!/bin/sh
# Builds jigc from bootstrap/jigc.c with only a C compiler, rebuilds it from the Jig sources
# until it reproduces itself, and writes that C back to bootstrap/jigc.c (D6). With --check,
# fails instead if the file is not already that fixed point; CI runs this.
#
# The C leaves out the checks jigc's proofs remove, as `jig build` does. The proofs are kept
# in bootstrap/proofs.txt so that building needs no solver: when Z3 is installed (or JIG_Z3
# names it), this proves selfhost/ again first and rewrites the file; without it, functions
# changed since the file was written keep their checks.
set -eu
cd "$(dirname "$0")/.."
PWD=$(pwd)
export PWD
CC=${CC:-cc}
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

build() { # build C_FILE EXE
  "$CC" -O2 -fno-math-errno -std=gnu11 -pthread "$1" -lm -o "$2"
}

# The standard library and runtime jigc carries (selfhost/sema/stdlib.jig).
if [ "${1:-}" = "--check" ]; then
  python3 bootstrap/stdlib.py --check
else
  python3 bootstrap/stdlib.py
fi
build bootstrap/jigc.c "$tmp/jigc0"
"$tmp/jigc0" cc selfhost compiler/std bootstrap/proofs.txt >"$tmp/jigc1.c"
if [ "${1:-}" = "--check" ]; then
  if ! cmp -s bootstrap/jigc.c "$tmp/jigc1.c"; then
    echo "bootstrap/jigc.c is stale: run bootstrap/update.sh" >&2
    exit 1
  fi
  echo "bootstrap/jigc.c is a fixed point"
  exit 0
fi
# jigc0 has the old back end; jigc1 has the new one, so its output is the new bootstrap.
build "$tmp/jigc1.c" "$tmp/jigc1"
if command -v "${JIG_Z3:-z3}" >/dev/null 2>&1; then
  # Obligations that do not prove are not an error here: they keep their checks.
  JIG_STD=compiler/std "$tmp/jigc1" prove selfhost >/dev/null || true
  JIG_STD=compiler/std "$tmp/jigc1" debug proofs selfhost >"$tmp/proofs.txt"
  cp "$tmp/proofs.txt" bootstrap/proofs.txt
fi
"$tmp/jigc1" cc selfhost compiler/std bootstrap/proofs.txt >"$tmp/jigc2.c"
build "$tmp/jigc2.c" "$tmp/jigc2"
"$tmp/jigc2" cc selfhost compiler/std bootstrap/proofs.txt >"$tmp/jigc3.c"
if ! cmp -s "$tmp/jigc2.c" "$tmp/jigc3.c"; then
  echo "no fixed point: jigc built from its own C generates different C" >&2
  exit 1
fi
cp "$tmp/jigc2.c" bootstrap/jigc.c
echo "bootstrap/jigc.c updated"
