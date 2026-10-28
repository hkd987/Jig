# Jig language reference

Jig is a strict, Go-shaped systems language with verification built in. Humans write
feature scenarios and contracts; agents write implementations; the compiler proves the
contracts or, failing that, tests them. No nil, no zero values, no implicit conversions,
no GC, no warnings: every lint is an error.

## Files and packages

- A package is a directory of `.jig` files. All files in it share one namespace, but
  imports are per file: each file imports the packages it names.
- A project root holds a `jig.mod` file starting `module <name>`. Import paths are
  relative to the root: `import "geom"` loads `<root>/geom/`. `std/...` is the standard
  library. Without `jig.mod`, a package can import only `std`.
- Other projects are modules: `require github.com/x/y v1.2.0` in `jig.mod` (a git tag of
  `https://github.com/x/y`), then `import "github.com/x/y/pkg"`. `jig get` downloads them and
  records their commits in `jig.sum`; check and build never fetch. The highest version any
  module requires is used. `replace github.com/x/y => ../y` uses a local directory.
- The imported package is named by its last path segment: `geom.area(p)`.
- Import cycles and unused imports are errors.
- Items are private to their package unless marked `pub`. Struct fields are private unless
  marked `pub`.

## Lexical rules

- Comments: `// line`, `/// doc comment`. No block comments.
- Statements end at a newline when the line's last token is an identifier, a literal,
  `return`, `break`, `continue`, `)`, `]`, `}`, `>` or `?`. Put binary operators at the
  end of a line to continue it.
- Literals: `42`, `1_000`, `0xff`, `0b1010`, `3.14`, `1e-9`, `true`, `false`, `'a'`
  (`Char`, or `U8` when a `U8` is expected), `"text\n"` (escapes `\n \t \r \0 \\ \" \'
  \u{1F600}`).
- Naming is enforced: types, variants and interfaces `UpperCamel`; functions, variables,
  fields and packages `snake_case`; constants `UPPER_SNAKE`.

## Types

| Type | Meaning |
| --- | --- |
| `Int` | 64-bit signed integer |
| `I32`, `U8`, `U32`, `U64` | sized integers |
| `Float` | 64-bit IEEE float |
| `Bool`, `Char` | boolean, Unicode scalar value |
| `Str` | immutable UTF-8 string |
| `()` | unit |
| `(A, B)` | tuple; fields `t.0`, `t.1` |
| `List<T>` | growable array |
| `Map<K, V>`, `Set<T>` | hash map/set, iterate in insertion order |
| `Option<T>` | `Some(value)` or `None` |
| `Result<T, E>` | `Ok(value)` or `Err(error)` |
| `fn(A) -> B effects {io}` | function value |
| `Never` | type of `panic(...)`; converts to any type |

There is no nil and no default value: every variable and field is initialized explicitly.
Numbers never convert implicitly; use `as`: `n as Float`, `f as Int` (truncates),
`x as U8` (traps if out of range), `c as U32`.

### Structs, enums, aliases, constants

```
pub struct Point {
  pub x: Int
  pub y: Int
}

pub struct Span {
  lo: Int
  hi: Int
  invariant lo <= hi            // checked on construction and mutation
}

pub enum Shape {
  Circle(r: Float)
  Rect(w: Float, h: Float)
  Empty
}

pub enum Tree<T> {              // recursive payloads are boxed automatically
  Leaf
  Node(left: Tree<T>, value: T, right: Tree<T>)
}

type Cents = Int where self >= 0   // refinement alias
type Grid = List<List<Int>>        // plain alias
const MAX_USERS: Int = 1_000
```

Construct with all fields named: `Point { x: 1, y: 2 }`. Variants are qualified when
constructed: `Shape.Circle(1.0)`, `Shape.Empty`, or through an alias
(`type E = model.LedgerError` then `E.OutOfStock(n)`). The prelude variants `Some`, `None`,
`Ok` and `Err` need no qualifier. Prelude names (`Option`, `Result`, `Some`, `Err`,
`min`, ...) cannot be redefined.

### Units of measure

```
unit m
unit s
fn speed(d: Float<m>, t: Float<s>) -> Float<m/s> {
  return d / t
}
```

`+`, `-` and comparisons need equal units; `*` and `/` combine them. Literals take the unit
their context expects. `x as Float<m>` adds a unit; `d as Float` removes it. Units work on
`Int` too (`Int<cent>`). Unit names are program-wide: declare each once, in any package,
and use it unqualified everywhere.

## Values and memory

Jig has **mutable value semantics**. Assignment and argument passing copy the value
(copies of `Str`, `List`, `Map` and `Set` are cheap: they share storage until one is
mutated). Two variables never observe each other's changes. There are no pointers.

- `let x = …` is immutable; `var x = …` is mutable. A `var` that is never mutated is an error.
- `&mut` lets a function mutate the caller's variable. It appears only on parameters and at
  the call site, and cannot be stored:

```
fn bump(n: &mut Int) {
  n += 1
}
var count = 0
bump(&mut count)
```

- Two `&mut` arguments to one call must be different places (`f(&mut a, &mut a.x)` is an
  error).
- For graphs and cyclic data, store nodes in a `List` and refer to them by index.

## Functions

```
pub fn transfer(from: &mut Acct, to: &mut Acct, amt: Cents)
  requires amt > 0, from.bal >= amt
  ensures from.bal + to.bal == old(from.bal + to.bal)
  effects {}
{
  from.bal -= amt
  to.bal += amt
}
```

- Every parameter and the return type are annotated. A missing `-> T` means `()`.
- Function bodies return with an explicit `return`. Every path of a non-`()` function
  must return.
- Generics: `fn first<T>(xs: List<T>) -> Option<T>`. Type arguments are inferred at call
  sites. Bounds name interfaces: `fn largest<T: Ord>(xs: List<T>) -> Option<T>`.
- Methods are declared on a type of the same package and called with a dot:

```
fn Point.norm2(self) -> Int {
  return self.x * self.x + self.y * self.y
}
fn Acct.deposit(&mut self, amt: Cents) {
  self.bal += amt
}
fn Point.origin() -> Point {                 // associated function: Point.origin()
  return Point { x: 0, y: 0 }
}
```

- A method may share a field's name: `x.name` reads the field, `x.name()` calls the
  method. A custom `fn T.str(self) -> Str` replaces the derived `Show` everywhere, even
  when it is not `pub`.
- Closures capture copies of variables. Their body yields its last expression:
  `xs.map(fn(x) { x * 2 })`. Parameter types may be omitted when the expected type is known.
- `if` and `match` are expressions; their blocks also yield their last expression.

### Interfaces

Interfaces are structural: a type satisfies one if it has the methods. They are used as
generic bounds and are resolved at compile time.

```
pub interface Shape {
  fn area(self) -> Float
}
fn total<T: Shape>(xs: List<T>) -> Float { … }
```

Built-in interfaces: `Eq` (`==`, `!=`), `Ord` (`<`, `<=`, `>`, `>=`), `Hash`, `Show`
(`.str()`); `Ord` and `Hash` imply `Eq`. Every type except functions satisfies `Eq` and `Show`; every type except
functions and `Float` satisfies `Hash` and `Ord`. Structs and enums compare field by field
in declaration order. Define `fn T.str(self) -> Str` to customize `Show`.

## Effects

A function is pure unless it declares effects. A function may only call functions whose
effects are a subset of its own.

| Effect | Grants |
| --- | --- |
| `io` | console input and output |
| `fs` | files and directories |
| `net` | network |
| `clock` | wall and monotonic time |
| `rand` | randomness |
| `env` | process arguments, environment variables, exit |
| `spawn` | threads |
| `ffi` | C functions |

```
import "std/io"
fn main() effects {io} {
  io.println("hello")
}
```

Function types carry effects: `fn(Int) -> Int effects {io}`. A pure function value cannot
perform effects. Tests run effects against deterministic fakes.

An `effects` clause may name a parameter of function type: the parameter then takes a
function with any effects, and each call performs that function's effects. Such a function
is called, not passed as a value. The built-in methods that take a function (`map`,
`filter`, `fold`, `any`, `all`, `sort_by`, `Option.map`, `Result.map`, `map_err`, and
`lists.find`, `position`, `count`, `partition`) work this way, so `xs.map(f)` has `f`'s
effects.

```
fn twice(f: fn() -> Int) -> Int effects {f} {
  return f() + f()
}
```

C functions are declared without a body and with the `ffi` effect; calls go to the C symbol
of the same name (which need not be snake_case). Parameters and results are `Int`, `I32`, `U8`, `U32`, `U64`, `Float`,
`Bool` or `Str` (a NUL-terminated `const char *`; a returned one is copied, `NULL` is `""`),
at most 6 integer-sized and 8 `Float` arguments. A parameter may also be a byte buffer:
`List<U8>` is passed as a `const uint8_t *` to its bytes, and `&mut List<U8>` as a
`uint8_t *` that C may write, as many bytes as the list holds (size it first); pass the
length as an `Int` yourself. Pointers C hands out are `Int`s, and a struct passed by pointer
is a `&mut List<U8>` of its size, read by byte offset. C's `long` is `Int` on Linux and macOS
but 32 bits on Windows, where it is `I32`. C cannot call back into Jig. Their
contracts are checked at run time. Link libraries with `link NAME` lines in `jig.mod`
(`libc` and `libm` are always there).

```
fn erf(x: Float) -> Float effects {ffi}
fn read(fd: I32, buf: &mut List<U8>, n: Int) -> Int effects {ffi}
```

Parallelism is fork-join through `std/task` and the `spawn` effect: `task.map(items, f)`,
`task.all(jobs)` and `task.both(f, g)` run their functions in parallel and return when all
have finished, results in order. Task functions have type `fn() -> T effects {spawn}`: they
may start tasks but do nothing else, so results never depend on scheduling (tests run them
in order). Write a list of jobs as `var jobs: List<fn() -> Int effects {spawn}> = []`.

```
let sizes = task.map(files, fn(f) { parse(f).len() })
```

## Statements and expressions

```
let p = Point { x: 1, y: 2 }
var total = 0
total += p.x
let (q, r) = divmod(17, 5)
let big = if total > 10 { "big" } else { "small" }

for i in 0..10 { … }               // half-open range
for x in xs { … }                  // each element, by value
for (k, v) in m { … }              // map entries in insertion order
for i in 0..n invariant total >= 0 { … }
for queue.len() > 0 { … }          // while
for { … }                          // loop; leave with break or return
```

Operators, tightest first: postfix (`f()`, `x[i]`, `x.f`, `?`), `as`, unary (`-`, `!`, `~`),
`* / % << >> &`, `+ - | ^`, comparisons, `&&`, `||`. Integer overflow, division by zero,
out-of-range shifts and out-of-bounds indexing trap unless the compiler proves they cannot
happen. A call statement whose result is not `()` is an error; write `let _ = f()`.
Assignment is a statement, not an expression. Declaring a name that is already in scope
(shadowing) is an error.

### Match

`match` must be exhaustive; an arm that can never match is an error. Patterns: `_`, a binding
name, literals, variants with sub-patterns (unqualified), tuples, alternatives `A | B`,
guards `if cond`. An arm's body is an expression, a block, or a single statement
(`Open => n += 1`, `Done => return total`).

```
fn area(s: Shape) -> Float {
  return match s {
    Circle(r) => 3.14159 * r * r
    Rect(w, h) => w * h
    Empty => 0.0
  }
}
```

### Errors

Use `Result` and `Option`; `?` returns early with the `Err` or `None`. The error types must
match exactly.

```
fn parse_port(s: Str) -> Result<Int, Str> {
  let n = s.to_int().ok_or("not a number")?
  if n < 1 || n > 65535 {
    return Err("out of range: " + n.str())
  }
  return Ok(n)
}
```

`panic(msg)` aborts the program. `assert(cond)` is a proof obligation and a runtime check.

## Contracts and refinements

- `requires` lists preconditions; callers must establish them.
- `ensures` lists postconditions; `result` is the return value and `old(e)` is `e` on entry.
- `invariant` on a `for` loop must hold before each iteration; on a struct, whenever a
  value is constructed or mutated.
- `T where pred` refines a parameter (`amt: Int where amt > 0`), a struct or variant field,
  an alias (`self` names the value) or the return type (`-> Int where result >= 0`, the same
  as `ensures result >= 0`). It is checked wherever a value enters the type: arguments,
  returns, construction and assignment; the prover assumes it wherever the value is used
  (a variant's, in the arm that matches it). A variant with refined fields is called, not
  passed as a function. For a local, use `assert`.
- Contract expressions are pure. They may call pure functions, and use
  `forall i in a..b: pred` and `exists i in a..b: pred`.

`jig prove` checks each function against its own contract and its callees' contracts, never
their bodies, and also proves the absence of traps (overflow, division by zero, bounds,
`unwrap`, reachable `panic`); it knows a list or string holds at most 2^60 items, so `i + 1`
cannot overflow for an index `i`. Integer arithmetic is exact, and so are shifts by a
constant (`x >> 3` divides by 8 rounding down; `x << 3` multiplies by 8, wrapping within the
type), masks of the form `x & (2^k - 1)` (`x` modulo `2^k`) and `~x`; other bit operations are
known by bounds (`x & y` is between 0 and `x` when `x >= 0`, `x | y` at least both when both
are non-negative). Strings are known by their length: `s.starts_with(p)`, `ends_with` and
`contains` imply `p.len() <= s.len()`. Two things are read off a callee's body. What it can write:
after `f(&mut x)` or `x.m()` with `&mut self`, fields `f` never assigns keep their values.
And what a pure function reads: a call to one that reads only scalars (not lists) is a
function of them, so `self.at(i)` twice on the same fields gives the same result, even if
other fields changed in between. Results are cached per function: changing a body re-proves
only that function and, if what it can write or read changed, its callers. A failed contract comes with a
counterexample in source terms, and is replayed on the interpreter: "confirmed" means
those inputs really fail, "not reproduced" means the prover is missing a fact. An unproven
obligation keeps its runtime check in every build; a proved one has none, and a `requires`
proved at every call is not checked in the callee either. To prove facts after a loop,
state them as loop invariants; to prove facts about a call's result, put them in the
callee's `ensures`.

## Tests

`test` blocks live next to the code they test and run in a deterministic runner with fake
effects. A `test` with parameters is a property test: it runs 100 generated inputs (edge
values first, growing in size), skips inputs that fail its own `requires`, and shrinks a
failure to a minimal counterexample.

```
test "transfer keeps the total" {
  var a = Acct { id: 1, bal: 100 }
  var b = Acct { id: 2, bal: 0 }
  transfer(&mut a, &mut b, 40)
  assert(a.bal == 60 && b.bal == 40)
}

test "add commutes"(x: Int, y: Int) requires x < 1000, y < 1000 {
  assert(add(x, y) == add(y, x))
}
```

Tests do not justify code: a function called only from tests is dead code, unless it is in
a file whose name ends in `_test.jig` (a test helper).

Tests and scenarios run against a simulated world: output is captured, stdin is empty,
files live in memory, the clock starts at a fixed time and moves 1 ms per reading (and by
`time.sleep`), randomness is seeded, and the network is simulated (`sim.reply` queues a
server's answer to `net.request`; `sim.connect` queues a request for `net.serve`, which
answers what is queued and returns). `std/sim` sets this world up and inspects it; code
that `main` reaches may not use it (E0607).

```
test "the copy is stamped" {
  sim.set_file("a.txt", "hi")
  sim.set_time(1000)
  sim.set_args(["stamp", "a.txt", "b.txt"])
  main()
  assert(sim.stdout() == "stamped b.txt\n" && sim.file("b.txt") == Some("1500\nhi"))
}
```

## Features and steps

Scenarios are Gherkin `.feature` files in the package directory (`Feature`, `Scenario`,
`Scenario Outline` with `Examples`, `Given`/`When`/`Then`/`And`/`But`). Steps bind to
functions at compile time; `{name}` placeholders bind to parameters of the same name and
are type-checked (`Str` matches a quoted string, numbers match literals).

```
Feature: Transfers
  Scenario: transfer moves funds
    Given an account "A" with balance 100
    When "A" transfers 40 to "B"
    Then "A" has balance 60
```

```
pub struct World {
  accts: Map<Str, Acct>
}
fn World.new() -> World { … }

step "an account {name} with balance {amt}"
fn given_account(w: &mut World, name: Str, amt: Cents) { … }

step "{name} has balance {amt}"
fn then_balance(w: World, name: Str, amt: Int) { … }
```

Each scenario starts from `World.new()` and runs its steps in order: a step takes the world
first (`&mut World` to change it, `World` to read it), then one parameter per placeholder.
Placeholders are `Str` (quoted), numbers or `Bool`. A `Scenario Outline` runs once per
`Examples` row with `<column>` replaced. `jig test` runs every scenario. A missing (the error
suggests a step), ambiguous or mistyped step is a compile error, and so is a step no
scenario uses. Scenarios are roots for dead code: code they reach is justified.

## Dead code and traceability

Roots are `main` and step functions used by a scenario; a package without `main` (a
library) also has its root package's `pub` items as roots. Unreachable functions, unused
fields, variants and constants, and impossible match arms fail the build. So does an
unused export: a `pub` item of a non-root package that no other package uses (remove the
`pub`). `jig trace` answers which scenarios exercise a function (`jig trace fn transfer`),
what is reachable but covered by no scenario (`jig trace uncovered`), and what becomes dead
if a scenario is deleted (`jig trace scenario overdraft`).

## Standard library

Prelude (no import): `assert(c)`, `panic(msg) -> Never`, `min(a, b)`, `max(a, b)`.

| Type | Methods |
| --- | --- |
| integers | `str() abs() min(b) max(b) pow(n) wrapping_add(b) wrapping_sub(b) wrapping_mul(b)` |
| `Float` | `str() abs() sqrt() floor() ceil() round() is_nan()` |
| `Char` | `str() is_digit() is_alpha() is_space()` |
| `Str` | `len() is_empty() s[i] -> U8, s[a..b] -> Str, + contains(t) starts_with(t) ends_with(t) find(t) -> Option<Int> split(sep) -> List<Str> lines() trim() replace(a, b) repeat(n) to_upper() to_lower() to_int() -> Option<Int> to_float() -> Option<Float> bytes() -> List<U8> chars() -> List<Char>` |
| `List<T>` | `List.new() [a, b] len() is_empty() xs[i] xs[a..b] push(x) pop() -> Option<T> get(i) -> Option<T> first() last() insert(i, x) remove(i) -> T clear() extend(ys) reverse() swap(i, j) truncate(n) repeat(n) contains(x) index_of(x) -> Option<Int> sort() sort_by(f) map(f) filter(f) fold(init, f) any(f) all(f) join(sep)` (`join` on `List<Str>`; `[0].repeat(n)` is n zeros) |
| `Map<K, V>` | `Map.new() len() is_empty() get(k) -> Option<V> insert(k, v) remove(k) -> Option<V> contains(k) keys() values() entries()` |
| `Set<T>` | `Set.new() len() insert(x) remove(x) contains(x) items()` |
| `Option<T>` | `is_some() is_none() unwrap() unwrap_or(d) ok_or(e) map(f)` |
| `Result<T, E>` | `is_ok() is_err() unwrap() unwrap_or(d) ok() map(f) map_err(f)` |

`unwrap()` requires `is_some()`/`is_ok()`: it is a proof obligation like any `requires`.
The prover knows the prelude's contracts (all proved, `jig prove --std`): `map` keeps the
length and `filter` does not grow it, `index_of` returns an index in bounds, `first()` and
`last()` are `Some` exactly when the list is not empty, `Option`/`Result` `map`, `ok_or` and
`ok` keep whether a value is present, and `min`/`max` return one of their arguments.
Also: `Char.is_upper() is_lower() is_alnum() to_upper() to_lower()` and `Set.is_empty()`.
Behavior worth knowing: `io.read_line()` strips `\n` or `\r\n` and returns a final
unterminated line; `"a  b".split(" ")` keeps empty pieces; `sort_by(less)` takes a strict
`fn(T, T) -> Bool` and sorts in place, like `sort()`; `Map.insert` on an existing key keeps
its position; `Str.to_int` and `to_float` ignore surrounding spaces and accept a sign.

| Package | Functions |
| --- | --- |
| `std/io` (`io`) | `print(s) println(s) eprintln(s) read_line() -> Option<Str> read_all() -> Str read(n) -> Str` (`read`: up to `n` bytes, fewer only at the end) |
| `std/fs` (`fs`) | `read(path) -> Result<Str, Str> write(path, s) -> Result<(), Str> exists(path) -> Bool list(dir) -> Result<List<Str>, Str>` |
| `std/os` (`env`) | `args() -> List<Str> env(name) -> Option<Str> exit(code) -> Never` |
| `std/time` (`clock`) | `now_ms() -> Int now_ns() -> Int sleep(ms)`; pure, for UTC dates in years 0 to 9999 (`MIN_MS` to `MAX_MS`): `utc(ms) -> DateTime` (`year month day hour minute second ms`) `unix_ms(d) iso(ms) http_date(ms) -> Str parse_iso(s) -> Option<Int> weekday(ms)` (1 is Monday) `is_leap(year) days_in_month(year, month)` |
| `std/rand` (`rand`) | `int(lo, hi) -> Int float() -> Float` |
| `std/math` (pure) | `PI E sin(x) cos(x) tan(x) exp(x) ln(x) log10(x) pow(x, y) gcd(a, b) clamp(x, lo, hi) sign(x) isqrt(n) pow_mod(b, e, m)` |
| `std/lists` (pure) | `sum(xs) max_of(xs) min_of(xs) -> Option<T> reversed(xs) take(xs, n) drop(xs, n) zip(xs, ys) enumerate(xs) flatten(xss) find(xs, f) -> Option<T> position(xs, f) -> Option<Int> count(xs, f) partition(xs, f) -> (List<T>, List<T>) dedup(xs) unique(xs) chunks(xs, n) binary_search(xs, x) -> Option<Int>` |
| `std/strings` (pure) | `split_once(s, sep) -> Option<(Str, Str)> strip_prefix(s, p) strip_suffix(s, p) -> Option<Str> trim_start(s) trim_end(s) is_blank(s) pad_left(s, width, c) pad_right(s, width, c) count(s, part) words(s) -> List<Str>` |
| `std/html` (pure) | `Node` (`Text(text)`, `Element(tag, attrs, children)`), `text(s) el(tag, attrs, children) tag(name, children) render(n) document(title, head, body) escape(s) is_void(tag)`: text and attribute values are escaped when rendered |
| `std/json` (pure) | `Json` (`Null Bool(value) Int(value) Float(value) String(value) Array(items) Object(fields)`), `parse(s) -> Result<Json, Str>` (errors give line and column; nesting is limited to `MAX_DEPTH`) `stringify(j) pretty(j) quote(s) -> Str`, `j.get(key) j.at(i) -> Option<Json> j.as_str() as_int() as_float() as_bool() as_list() as_fields()` |
| `std/hash` (pure) | `sha256(s) -> Str sha256_bytes(bytes) -> List<U8> hmac_sha256(key, msg) -> Str hmac_sha256_bytes(key, msg) -> List<U8> hex(bytes) -> Str equal(a, b) -> Bool` (hex digests; `equal` takes the same time wherever texts differ) |
| `std/regex` (pure) | `compile(pattern) -> Result<Regex, Str>`, `re.find(s) -> Option<Match> re.find_all(s) -> List<Match> re.is_match(s) re.full_match(s) -> Bool re.replace_all(s, with) -> Str re.split(s) -> List<Str>`, `m.start m.end m.text(s) m.group(s, g) -> Option<Str>`, `escape(s) -> Str`. Literals, `.`, `[a-z]` `[^...]` `\d \w \s` (and `\D \W \S`), `^ $ \b \B`, `* + ? {m} {m,} {m,n}` (lazy with a `?` after), `( )` and `(?: )`, `\|`; `$1` and `$$` in `with`. Matching takes time linear in the text; positions are byte offsets |
| `std/net` (`net`) | `request(addr, req) -> Result<Str, Str> serve(addr, state, handle, limit) -> Result<S, Str>` with `handle: fn(S, Str) -> (S, Str)`, effects `{net}` and `handle`'s; one request per connection, read until the client closes its side; `serve_http` (same arguments) ends a request after its headers and `Content-Length` body, for browsers and HTTP clients (raw request in, raw reply out); `addr` is `host:port` or `:port`. HTTP client: `get(url) post(url, content_type, body) send(method, url, headers, body) -> Result<Response, Str>` (`Response`: `status headers body`, `r.header(name)`; `get` follows up to five redirects), over `fetch(addr, request, tls) -> Result<Str, Str>`; `https://` is TLS through the system's OpenSSL, certificates checked (`SSL_CERT_FILE` adds roots). `parse_url(url) -> Result<Url, Str> request_text(method, url, headers, body) parse_response(raw) -> Result<Response, Str>`; for handlers, `parse_request(raw) -> Result<Request, Str>` (`method path headers body`, `q.header(name)`) and `reply(status, content_type, body) -> Str`. `serve_http_parallel(addr, handle, limit) -> Result<Int, Str>` with a stateless `handle: fn(Str) -> Str` answers several requests at once natively (`JIG_THREADS` workers, else twice the processors) and returns how many it answered |
| `std/sim` (tests only) | `stdout() stderr() -> Str set_stdin(s) set_args(xs) set_env(k, v) set_file(path, s) file(path) -> Option<Str> remove_file(path) set_time(ms) advance(ms) seed(n) reply(addr, s) requests(addr) -> List<Str> connect(addr, req) answers(addr) -> List<Str>` |
| `std/task` (`spawn`) | `all(jobs) -> List<T> map(items, f) -> List<U> both(f, g) -> (A, B)` |

## Toolchain

| Command | Does |
| --- | --- |
| `jig check [--json]` | parse, types, effects, lints, dead code; milliseconds |
| `jig prove [filter] [--json] [--verbose] [--std]` | SMT proofs per function, cached in `.jig/proofs` (`--std`: the standard library's) |
| `jig test [--json] [filter]` | unit tests, property tests, scenarios |
| `jig get` | download the modules `jig.mod` requires |
| `jig build [--release] [-o out]` | native binary (debug: checks on; release: `clang -O2`, contracts off) |
| `jig run [--native] [-- args]` | run `main` (interpreter, or a native debug build) |
| `jig fmt [--check]` | canonical formatting; no options |
| `jig trace [fn F \| scenario S \| uncovered] [--json]` | traceability between scenarios and code |
| `jig explain <code>` | long explanation of a diagnostic code |
| `jig lsp` | a language server for editors over stdio: diagnostics as you type, types and signatures on hover, go to definition, formatting (`editors/vscode` is the VS Code extension) |

Every diagnostic has a stable code (`E0301`), a span, a message, and where possible a
counterexample and candidate fixes. `--json` prints one JSON object per line.
