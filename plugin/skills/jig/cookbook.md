# Jig cookbook

Idioms for common tasks. Every snippet type-checks.

## A program

```
import "std/io"
import "std/os"

fn main() effects {io, env} {
  let args = os.args()
  if args.len() < 2 {
    io.eprintln("usage: greet <name>")
    os.exit(2)
  }
  io.println("hello, " + args[1])
}
```

## Reading input line by line

```
fn read_numbers() -> List<Int> effects {io} {
  var out: List<Int> = []
  for {
    match io.read_line() {
      Some(line) => match line.trim().to_int() {
        Some(n) => out.push(n)
        None => {}
      }
      None => break
    }
  }
  return out
}
```

## Errors with `Result` and `?`

```
enum ConfigError {
  Missing(key: Str)
  BadNumber(key: Str, text: Str)
}

fn get_int(m: Map<Str, Str>, key: Str) -> Result<Int, ConfigError> {
  let text = m.get(key).ok_or(ConfigError.Missing(key))?
  return match text.to_int() {
    Some(n) => Ok(n)
    None => Err(ConfigError.BadNumber(key, text))
  }
}
```

Convert between error types with `.map_err(fn(e) { ... })`. `?` needs the error types to be
equal.

## Counting and grouping

```
fn counts(words: List<Str>) -> Map<Str, Int> {
  var m: Map<Str, Int> = Map.new()
  for w in words {
    m.insert(w, m.get(w).unwrap_or(0) + 1)
  }
  return m
}
```

Maps iterate in insertion order. To rank entries:

```
fn top(m: Map<Str, Int>, n: Int) -> List<(Str, Int)> {
  var es = m.entries()
  es.sort_by(fn(a, b) { a.1 > b.1 || (a.1 == b.1 && a.0 < b.0) })
  es.truncate(n)
  return es
}
```

`sort_by` takes a strict "less than"; `sort()` needs an `Ord` element type (not `Float`).

## Mutating through a function

```
struct Account {
  name: Str
  balance: Int
}

fn Account.deposit(&mut self, amount: Int)
  requires amount > 0
{
  self.balance += amount
}

fn pay_all(accounts: &mut List<Account>, amount: Int) {
  for i in 0..accounts.len() {
    accounts[i].balance += amount
  }
}
```

Call them as `acct.deposit(5)` (on a `var`) and `pay_all(&mut accounts, 5)`.

## Graphs and cycles: indices, not references

```
struct Graph {
  edges: List<List<Int>>
}

fn Graph.new(n: Int) -> Graph {
  var edges: List<List<Int>> = []
  for _i in 0..n {
    edges.push([])
  }
  return Graph { edges: edges }
}

fn Graph.add_edge(&mut self, a: Int, b: Int) {
  self.edges[a].push(b)
}

fn Graph.reachable(self, start: Int) -> List<Int> {
  var seen: Set<Int> = Set.new()
  var stack = [start]
  var order: List<Int> = []
  for stack.len() > 0 {
    let node = stack.pop().unwrap()
    if seen.contains(node) {
      continue
    }
    seen.insert(node)
    order.push(node)
    for next in self.edges[node] {
      stack.push(next)
    }
  }
  return order
}
```

## State machines with enums

```
enum Light {
  Red
  Green
  Yellow
}

fn Light.next(self) -> Light {
  return match self {
    Red => Light.Green
    Green => Light.Yellow
    Yellow => Light.Red
  }
}
```

## Trees

Recursive enums are boxed automatically:

```
enum Expr {
  Num(value: Int)
  Add(left: Expr, right: Expr)
  Mul(left: Expr, right: Expr)
}

fn eval(e: Expr) -> Int {
  return match e {
    Num(v) => v
    Add(a, b) => eval(a) + eval(b)
    Mul(a, b) => eval(a) * eval(b)
  }
}
```

## Generic functions and interfaces

```
interface Priced {
  fn price(self) -> Int
}

fn total<T: Priced>(items: List<T>) -> Int {
  return items.fold(0, fn(acc, x) { acc + x.price() })
}

fn largest<T: Ord>(xs: List<T>) -> Option<T> {
  var best: Option<T> = None
  for x in xs {
    best = match best {
      Some(b) => if x > b { Some(x) } else { Some(b) }
      None => Some(x)
    }
  }
  return best
}
```

Built-in bounds are `Eq`, `Ord`, `Hash` and `Show`. Structs and enums get them field by
field.

## Strings

```
fn initials(name: Str) -> Str {
  var out = ""
  for part in name.split(" ") {
    if !part.is_empty() {
      out = out + part[0..1].to_upper()
    }
  }
  return out
}
```

`s.len()` counts bytes; `s[i]` is a `U8`; `s.chars()` gives `Char`s; build strings from
characters with `Str.from_chars(cs)`.

## Tests

```
test "deposit adds" {
  var a = Account { name: "a", balance: 10 }
  a.deposit(5)
  assert(a.balance == 15)
}
```

A failing `assert(a == b)` reports both values. Tests do not justify code: everything they
call must also be reachable from `main` or the public API.

## Contracts

```
fn clamp(x: Int, lo: Int, hi: Int) -> Int
  requires lo <= hi
  ensures result >= lo, result <= hi
{
  if x < lo {
    return lo
  }
  if x > hi {
    return hi
  }
  return x
}
```

Contracts are checked at run time in debug builds and tests; `jig prove` proves them.
