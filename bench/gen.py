#!/usr/bin/env python3
"""Generate a deterministic, error-free Jig project for benchmarking the compiler.

usage: gen.py OUT_DIR [--packages N] [--units M]

Each package holds M "units" (a struct, an enum, a method, and several functions with loops,
matches, closures and generics) plus `pub fn run_all() -> Int`, which reaches everything, so
the project has no dead code. `app/main.jig` calls every package.
"""
import os
import random
import sys


def unit(r, i):
    s = f"S{i}"
    e = f"E{i}"
    n = r.randint(2, 5)
    fields = "\n".join(f"  f{k}: Int" for k in range(n))
    reads = " + ".join(f"self.f{k}" for k in range(n))
    inits = ", ".join(f"f{k}: {r.randint(0, 99)} + x" for k in range(n))
    return f"""
struct {s} {{
{fields}
  tag: Str
}}

enum {e} {{
  A{i}(v: Int)
  B{i}(a: Int, b: Int)
  C{i}
}}

fn {s}.total(self) -> Int {{
  return {reads} + self.tag.len()
}}

fn {s}.bump(&mut self, by: Int) {{
  self.f0 += by
}}

fn make_{i}(x: Int) -> {s} {{
  return {s} {{ {inits}, tag: "t{i}" }}
}}

fn classify_{i}(x: Int) -> {e} {{
  if x % 3 == 0 {{
    return {e}.A{i}(x)
  }} else if x % 3 == 1 {{
    return {e}.B{i}(x, x + {i})
  }}
  return {e}.C{i}
}}

fn score_{i}(e: {e}) -> Int {{
  return match e {{
    A{i}(v) => v * 2
    B{i}(a, b) if a > b => a - b
    B{i}(a, b) => b - a
    C{i} => {r.randint(1, 9)}
  }}
}}

fn loop_{i}(n: Int) -> Int {{
  var acc = 0
  var items: List<Int> = []
  for k in 0..n {{
    items.push(k * {r.randint(1, 7)})
  }}
  for v in items {{
    if v % 2 == 0 {{
      acc += v
    }} else {{
      acc -= 1
    }}
  }}
  let doubled = items.map(fn(v) {{ v * 2 }}).filter(fn(v) {{ v > {r.randint(1, 20)} }})
  return acc + doubled.len()
}}

fn words_{i}(text: Str) -> Int {{
  var counts: Map<Str, Int> = Map.new()
  for w in text.split(" ") {{
    counts.insert(w, counts.get(w).unwrap_or(0) + 1)
  }}
  var best = 0
  for (k, v) in counts {{
    best = max(best, v + k.len())
  }}
  return best
}}

fn pick_{i}<T: Ord>(a: T, b: T) -> T {{
  if a < b {{
    return b
  }}
  return a
}}

fn run_{i}(x: Int) -> Int {{
  var s = make_{i}(x)
  s.bump({i % 7 + 1})
  let e = classify_{i}(x + {i})
  let w = words_{i}("a b a c {i}")
  return pick_{i}(s.total(), score_{i}(e)) + loop_{i}(x % 10 + 3) + w + pick_{i}(1, 2)
}}
"""


def package(r, p, units):
    parts = [f"// Generated package p{p}.\n"]
    for i in range(units):
        parts.append(unit(r, i))
    calls = "\n".join(f"  total += run_{i}({r.randint(0, 50)})" for i in range(units))
    parts.append(f"""
pub fn run_all() -> Int {{
  var total = 0
{calls}
  return total
}}
""")
    return "".join(parts)


def main():
    args = sys.argv[1:]
    if not args:
        print(__doc__)
        sys.exit(2)
    out = args[0]
    packages = 50
    units = 18
    if "--packages" in args:
        packages = int(args[args.index("--packages") + 1])
    if "--units" in args:
        units = int(args[args.index("--units") + 1])
    r = random.Random(1234)
    os.makedirs(os.path.join(out, "app"), exist_ok=True)
    with open(os.path.join(out, "jig.mod"), "w") as f:
        f.write("module bench\n")
    lines = 0
    for p in range(packages):
        d = os.path.join(out, f"p{p}")
        os.makedirs(d, exist_ok=True)
        src = package(r, p, units)
        lines += src.count("\n")
        with open(os.path.join(d, f"p{p}.jig"), "w") as f:
            f.write(src)
    imports = "\n".join(f'import "p{p}"' for p in range(packages))
    calls = "\n".join(f"  total += p{p}.run_all()" for p in range(packages))
    main_src = f"""import "std/io"
{imports}

fn main() effects {{io}} {{
  var total = 0
{calls}
  io.println(total.str())
}}
"""
    with open(os.path.join(out, "app", "main.jig"), "w") as f:
        f.write(main_src)
    lines += main_src.count("\n")
    print(f"generated {packages} packages, {lines} lines in {out}")


if __name__ == "__main__":
    main()
