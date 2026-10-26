#!/bin/sh
# Builds jigc from bootstrap/jigc.c with only a C compiler, rebuilds it from the Jig sources
# until it reproduces itself, and writes that C back to bootstrap/jigc.c (D6). With --check,
# fails instead if the file is not already that fixed point; CI runs this.
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

build bootstrap/jigc.c "$tmp/jigc0"
"$tmp/jigc0" cc selfhost compiler/std >"$tmp/jigc1.c"
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
"$tmp/jigc1" cc selfhost compiler/std >"$tmp/jigc2.c"
build "$tmp/jigc2.c" "$tmp/jigc2"
"$tmp/jigc2" cc selfhost compiler/std >"$tmp/jigc3.c"
if ! cmp -s "$tmp/jigc2.c" "$tmp/jigc3.c"; then
  echo "no fixed point: jigc built from its own C generates different C" >&2
  exit 1
fi
cp "$tmp/jigc2.c" bootstrap/jigc.c
echo "bootstrap/jigc.c updated"
