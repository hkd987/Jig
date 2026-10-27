#!/bin/sh
# Builds the Jig compiler from bootstrap/jigc.c with only a C compiler and installs it as
# `jig` in PREFIX/bin (PREFIX is the first argument, else $PREFIX, else ~/.local).
set -eu
cd "$(dirname "$0")/.."
PREFIX=${1:-${PREFIX:-$HOME/.local}}
CC=${CC:-cc}
mkdir -p "$PREFIX/bin"
"$CC" -O2 -fno-math-errno -std=gnu11 -w -pthread bootstrap/jigc.c -lm -o "$PREFIX/bin/jig.tmp"
mv "$PREFIX/bin/jig.tmp" "$PREFIX/bin/jig"
echo "installed $PREFIX/bin/jig ($("$PREFIX/bin/jig" version))"
