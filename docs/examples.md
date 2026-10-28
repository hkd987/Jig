# Examples

Each directory under [`examples/`](../examples) is a small, complete project: it checks,
its tests pass (CI runs both, with the Rust compiler and with jigc), and most of them say
in their first comment what part of the language they show. Run any of them from the
repository root:

```
jig check examples/http        # types, effects, lints, dead code
jig test examples/http         # its tests, on the simulator
jig prove examples/stack       # its contracts
jig run examples/http -- :8080 # run it (then: curl localhost:8080)
```

| Example | What it shows | Try |
| --- | --- | --- |
| [hello](../examples/hello) | The smallest program | `jig run examples/hello` |
| [http](../examples/http) | A web server in 30 lines: `net.serve_http`, simulated requests in tests | `jig run examples/http -- :8080` |
| [kv](../examples/kv) | A key-value store over HTTP; the store is the server's state | `jig run examples/kv -- :8080` |
| [postgres](../examples/postgres) | Notes in a Postgres table, through libpq behind the `ffi` effect; SQL built and tested as pure code | `jig run examples/postgres -- list` |
| [guestbook](../examples/guestbook) | A web app on Postgres: an HTTP handler that queries the database, tested against fakes | `jig test examples/guestbook` |
| [greet](../examples/greet) | TCP client and server with `net.request` and `net.serve` | `jig test examples/greet` |
| [wordfreq](../examples/wordfreq) | Reading standard input, maps, sorting | `echo "a b a" \| jig run examples/wordfreq` |
| [calc](../examples/calc) | A tokenizer, a recursive-descent parser and an evaluator | `echo "2 * (3 + 4)" \| jig run examples/calc` |
| [todo](../examples/todo) | A command-line app on files, every effect in the signatures, tested on simulated files | `jig run examples/todo -- add buy milk` |
| [stamp](../examples/stamp) | Files and the clock, tested with simulated time | `jig test examples/stamp` |
| [stack](../examples/stack) | A data structure whose contracts all prove, so the build keeps no checks for them | `jig prove examples/stack` |
| [vending](../examples/vending) | A state machine: an exhaustive `match`, and a refinement the prover asked for | `jig prove examples/vending` |
| [bank](../examples/bank) | A ledger with the PRD's `transfer` contract and `.feature` scenarios | `jig test examples/bank` |
| [cart](../examples/cart) | Behaviour written first as Gherkin scenarios, bound to steps at compile time | `jig test examples/cart` |
| [units](../examples/units) | Units of measure: metres, seconds and kilograms checked through arithmetic | `jig run examples/units` |
| [primes](../examples/primes) | Fork-join parallelism with `std/task`, and a property test against the sequential count | `jig run examples/primes` |
| [normal](../examples/normal) | Calling the C library (`erf`) behind the `ffi` effect | `jig run examples/normal` |
| [gate1](../examples/gate1) | A multi-package program an agent wrote for the Phase 1 gate | `jig test examples/gate1` |

## A web server

`examples/http/main.jig` serves the greeting at `/` and a 404 everywhere else. A request
reaches the handler as text and the reply goes back as text; the state (here a count of
requests) is threaded through each call, so the handler is a pure function and the tests
call it, or `main` itself, on simulated connections.

<!-- file: examples/http/main.jig -->
```jig
import "std/io"
import "std/net"
import "std/os"

// `http [ADDR [N]]` serves "Hello, world!" over HTTP on ADDR (default :8080): forever, or N
// requests and then it says how many it served.

/// An HTTP reply with a plain-text body.
fn response(status: Str, body: Str) -> Str
  ensures result.len() > body.len()
{
  return "HTTP/1.1 " +
    status +
    "\r\nContent-Type: text/plain\r\nContent-Length: " +
    body.len().str() +
    "\r\nConnection: close\r\n\r\n" +
    body
}

/// The path in a request line such as `GET /a HTTP/1.1`.
fn path_of(request: Str) -> Str {
  let parts = request.split(" ")
  return if parts.len() > 1 { parts[1] } else { "" }
}

/// One request: the greeting at `/`, 404 anywhere else. The state counts requests.
fn handle(served: Int, request: Str) -> (Int, Str) {
  let n = if served < 1000000000 { served + 1 } else { served }
  if path_of(request) == "/" {
    return (n, response("200 OK", "Hello, world!\n"))
  }
  return (n, response("404 Not Found", "not found\n"))
}

fn main() effects {io, env, net} {
  let args = os.args()
  let addr = if args.len() > 1 { args[1] } else { ":8080" }
  let limit = if args.len() > 2 { max(args[2].to_int().unwrap_or(0), 0) } else { 0 }
  io.println("listening on " + addr)
  match net.serve_http(addr, 0, handle, limit) {
    Ok(n) => io.println("served " + n.str())
    Err(e) => io.eprintln(e)
  }
}
```

Its tests, in `examples/http/main_test.jig`, queue requests with `sim.connect` and compare
the answers; a property test checks that every reply ends with its body. Over a real
socket:

```
$ jig run examples/http -- :8080 &
listening on :8080
$ curl localhost:8080/
Hello, world!
$ curl -i localhost:8080/missing
HTTP/1.1 404 Not Found
...
```

## A proved data structure

`examples/stack/main.jig` states what `push` and `pop` do, and `jig prove` proves it: every
contract obligation and every safety check (bounds, overflow), so a release build keeps none
of their checks.

<!-- file: examples/stack/main.jig -->
```jig
import "std/io"

// A stack of at most `cap` numbers whose contracts all prove: `jig prove` finds no
// obligation it cannot discharge, so the release build keeps no checks for them.
// Reads `push N` and `pop` lines on standard input.

struct Stack {
  items: List<Int>
  cap: Int
  invariant cap >= 0 && items.len() <= cap
}

fn new_stack(cap: Int) -> Stack
  requires cap >= 0
  ensures result.items.is_empty() && result.cap == cap
{
  return Stack { items: [], cap: cap }
}

/// Push `x` unless the stack is full; says whether it did.
fn Stack.push(&mut self, x: Int) -> Bool
  ensures result == (old(self.items.len()) < self.cap)
  ensures !result || self.items.len() - old(self.items.len()) == 1
  ensures result || self.items.len() == old(self.items.len())
{
  if self.items.len() >= self.cap {
    return false
  }
  self.items.push(x)
  return true
}

/// The top number, removed; `None` when the stack is empty.
fn Stack.pop(&mut self) -> Option<Int>
  ensures result.is_none() == old(self.items.is_empty())
  ensures result.is_none() || old(self.items.len()) - self.items.len() == 1
{
  return self.items.pop()
}

fn main() effects {io} {
  var s = new_stack(3)
  for {
    let line = match io.read_line() {
      Some(l) => l.trim()
      None => {
        break
      }
    }
    if line == "pop" {
      match s.pop() {
        Some(x) => io.println(x.str())
        None => io.println("empty")
      }
    } else if line.starts_with("push ") {
      let x = line[5..line.len()].to_int().unwrap_or(0)
      if !s.push(x) {
        io.println("full")
      }
    }
  }
}
```

```
$ jig prove examples/stack
proved 14 of 14 contract obligations and 4 of 4 safety obligations in 4 functions
3 of 4 functions have contracts; 3 of those prove completely
```

## A database

`examples/postgres` keeps notes in a Postgres table. It declares the eleven libpq functions
it calls (`PQconnectdb`, `PQexec`, `PQgetvalue`, ...) as Jig functions without a body,
behind the `ffi` effect, and its `jig.mod` says `link pq`; a connection and a result are
pointers, held as `Int`. Only `exec` and `main` touch the database. Parsing the command,
building the SQL (each quote in the text doubled, so no text can end a string literal
early) and showing the rows are pure functions, tested without a database, and a property
test checks the quoting on generated text. CI runs it against a Postgres server, on the
interpreter and built natively.

It needs libpq's development files (`apt install libpq-dev` on Debian and Ubuntu) and a
server; the connection string comes from `DATABASE_URL`, or libpq's defaults without it.

```
$ export DATABASE_URL=postgresql://localhost/notes
$ jig run examples/postgres -- add "it's done"
added note 1
$ jig run examples/postgres -- add "x'); DROP TABLE notes; --"
added note 2
$ jig run examples/postgres -- list
1. it's done
2. x'); DROP TABLE notes; --
```

## A web app on a database

`examples/guestbook` puts the two together: an HTTP server whose handler reads and writes
Postgres. `GET /` lists the entries and `POST /` signs the book with the request's body.
The handler takes the database as a function and has whatever effects that function has
(an [effect parameter](reference.md#effects)):

```
fn handle(db: fn(Str) -> Result<List<List<Str>>, Str>, raw: Str) -> Str effects {db}
```

`main` passes a closure that runs the statement through libpq, so serving is
`effects {net, ffi}`; the tests pass fakes (`fn(_) { Err("connection lost") }`) and run
every request, and the server itself on simulated connections, without a database. CI
serves it from a Postgres server and talks to it with curl.

```
$ export DATABASE_URL=postgresql://localhost/guestbook
$ jig run examples/guestbook -- :8080 &
listening on :8080
$ curl -d "it's me" localhost:8080/
signed as entry 1
$ curl localhost:8080/
1. it's me
```

## What the checks catch

Writing these turned up the kind of thing the tools are for:

- `vending`'s property test found that a huge coin overflowed the change computation, and
  printed the inputs that do it; the change is now computed as `c - (PRICE - credit)`.
- `jig prove` found that nothing said a `Paying` credit lies between 0 and the price, and
  replayed the counterexample to confirm it; the variant's field now has a refined type,
  `Credit`, which is checked wherever a `Paying` is built and assumed wherever one is
  matched.
- `primes` needed its divisor loop to say `invariant d >= 2` before the prover would accept
  `n / d`.
