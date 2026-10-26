# Phase 1 gate: a 1,000-line program from the reference alone

Gate: "an agent writes a 1,000-line program using only the plugin reference."

The test subject (an agent) had `docs/reference.md`, the tool output (`jig explain`,
`jig search`, `jig reference`) and the frozen binary `/tmp/claude-0/gate/jig`. It read no
compiler source, std source, tests or other examples. Every claim below comes from
running that binary.

## Follow-up (after the run)

The agent used a compiler frozen at the start of its run. Status of its findings on `main`:

| Item | Status |
| --- | --- |
| B1 property tests | Fixed: generated inputs, `requires` as a filter, shrinking (`tests/props`). |
| B2 refinement aliases | Fixed (landed during the run): checked on arguments, returns, construction and assignment. |
| B3 invariants on mutation | Fixed (landed during the run): re-checked after field assignment and for `&mut` parameters on return. |
| B4 `Never` in struct position (C) | Fixed (`tests/run/never.jig`). |
| B5 type-only imports | Fixed (`tests/ui/type_imports`). |
| B6 E0603 coverage | Fixed: structs, enums, interfaces and constants. `gate1` dropped six `pub`s and one on an interface. |
| B7 trap messages | Messages fixed. Native panics still have no call stack. |
| B8 `jig prove` | Exists now. `jig trace` is marked planned in the reference. |
| G1/L3 method and field names | Fixed: a method may share a field's name. |
| G2 assignment arms | Fixed: `Open => open += 1` parses. |
| G4 private `str` | Fixed. |
| G6 return refinements | Fixed: `-> T where pred` means `ensures pred`. A `where` on `let` is still rejected; the reference says to use `assert`. |
| G3, G5, G7–G11 | Documented in the reference. |
| D1 parse cascades | Fixed: one bad arm gives one error, and a package whose recovery dropped code reports only its syntax errors. |
| D3 shadowed prelude function | A note now names the shadowing local. |
| D4 E0105 catch-all | "has no body" is now E0109. Other syntax errors stay E0105 ("expected X, found Y"). |
| D5, D6 values in failures | Fixed: property-test inputs and every value a failed contract mentions are shown. |
| L1 test helpers | Fixed: functions in `*_test.jig` files may be used only by tests. |
| D2, D7, L2, L4, L5 | Open. |

## 1. Summary

**Program.** `examples/gate1/` is an inventory and order ledger. It reads a line-based
command language from stdin (`add-item`, `price`, `restock`, `order ... [with COUPON]`,
`cancel`, `ship`, `coupon`, `limit`, `stock`, `customer`, `orders`, `history`, `report`,
`help`) and prints results, with per-line error messages. It enforces these business
rules:

- An order is atomic: one short line rejects the whole order.
- Repeated SKUs on one order are merged before the stock check.
- Lines of 10 or more units get a 5% bulk discount.
- Coupons give a percentage off and work once per customer; cancelling an order frees
  its coupon.
- Shipping is weight-based, computed with units of measure, and free above $100.
- Customers have a credit limit that counts their unshipped orders.
- Cancelling an order returns its stock; shipped and cancelled orders are frozen.
- Repricing an item never changes orders already placed.
- Every change is recorded in an audit log.

| Package | Files | Total lines | Code lines (non-blank, non-comment, outside tests) | Test lines | Tests |
| --- | --- | ---: | ---: | ---: | ---: |
| `main` (root) | `main.jig` | 215 | 175 | 22 | 2 |
| `model` | `types.jig`, `errors.jig` | 203 | 163 | 0 | 0 |
| `text` | `text.jig` | 144 | 93 | 26 | 4 |
| `parse` | `lexer.jig`, `command.jig` | 386 | 270 | 56 | 7 |
| `ledger` | `ledger.jig`, `pricing.jig`, `report.jig`, `rules_test.jig` | 718 | 395 | 207 | 22 |
| **total** | 10 files, 5 packages | **1,666** | **1,096** | **311** | **35** |

Counting test blocks too, there are 1,407 non-blank, non-comment lines.

**Results with the frozen binary:**

- `jig check examples/gate1`: `ok: no errors`.
- `jig test examples/gate1`: `35 passed, 0 failed`.
- `jig fmt --check examples/gate1`: clean.
- The interpreter and the native build print identical output for `sample.txt`
  (28 commands, 66 output lines):
  - `jig run examples/gate1 < examples/gate1/sample.txt`
  - `jig build examples/gate1 -o /tmp/claude-0/gate/gate1 && /tmp/claude-0/gate/gate1 < examples/gate1/sample.txt`
- `jig build --release` and `jig run --native` also produce identical output.
- An extra edge-case script (huge prices, absurd weights, CRLF input, empty input, bad
  names) also prints the same output from the interpreter and the native build.

**Iterations.** It took about 12 check/edit rounds on the program itself. 9 of them
showed compiler errors; the rest were test, format and native-build rounds. Before and
during that, about 30 small probe programs in a scratch directory explored behaviour
the reference does not pin down.

**Time.** About 20 minutes of wall-clock time by the container clock, from reading the
reference to a green program, plus the time to write this report.

**Language features used:**

- **Contracts:** `requires`, `ensures` (including `old(...)` on `&mut self`), struct
  `invariant`, and a loop `invariant` in a probe.
- **Refinements:** the aliases `type Cents = Int where self >= 0` and `Qty`, and the
  inline field refinement `percent: Int where percent > 0 && percent <= 50`.
- **Units:** `g`, `kg` and `cent`, with `Float<g/kg>`, `Float<cent/kg>` and
  unit-checked arithmetic in `shipping_for` and `parse_weight`.
- **Enums and match:** enums with payloads (`Command`, `ParseError`, `LedgerError`,
  `Event`, `Status`), exhaustive `match`, nested patterns and alternatives.
- **Errors:** `Result` and `Option` with `?` throughout the parser and the ledger.
- **Mutation:** `&mut` parameters and `&mut self` methods (`Args` cursor, `Ledger`,
  `Session`).
- **Collections and closures:** `Map`, `Set` and `List`; closures passed to `map`,
  `filter`, `fold`, `all` and `sort_by`.
- **Generics:** `total_value<T: Valued>` over items, order lines and orders, and
  `top_n<T>` taking a comparator closure.
- **Effects:** `effects {io}` appears only on `main`; everything else is pure, and the
  session driver is tested directly.

## 2. Friction points

Classifications: **COMPILER BUG** (wrong error, crash, wrong output, interpreter/native
mismatch), **REFERENCE GAP**, **DIAGNOSTIC QUALITY**, **LANGUAGE DESIGN**. The bugs come
first, ordered by severity.

### B1. Property tests never bind their parameters: COMPILER BUG

Every generated parameter evaluates to `()`. Property tests therefore fail spuriously or
pass vacuously. The reference's own example fails:

```
fn add(a: Int, b: Int) -> Int {
  return a + b
}
test "add commutes"(x: Int, y: Int) requires x < 1000, y < 1000 {
  assert(add(x, y) == add(y, x))
}
test "no requires"(x: Int) {
  assert(x + 0 == x)
}
```

```
FAILED  add commutes (main)
        precondition failed: x < 1000
          x = ()
FAILED  no requires (main)
        assertion failed: x + 0 == x
          x + 0 = 0
          x = ()
```

- **Expected:** generated `Int` inputs, with inputs that violate `requires` discarded
  rather than reported as failures.
- **Pattern:** a test whose `requires` is only a lower bound (`x >= 0`) sometimes passes,
  but only by accident: `assert(x != 3)` passes and `assert(x < 1_000_000)` fails, with
  `x = ()` both times.
- **Workaround:** I rewrote my `percent_of` property test as an exhaustive nested loop in
  an ordinary test (`ledger/pricing.jig`).

### B2. Refinement aliases are never enforced: COMPILER BUG

```
type Cents = Int where self >= 0
fn show(c: Cents) -> Str {
  return c.str()
}
fn main() effects {io} {
  io.println(show(-5))
  let c: Cents = -7
  io.println(c.str())
}
```

- **What happens:** `jig check` prints `ok: no errors`. `jig run` and the native debug
  build both print `-5` and `-7` and exit 0.
- **Not checked in any position I tried:** parameter, `let` annotation, struct field
  typed `Cents`, return value, `c -= 10` on a `var c: Cents`.
- **The contrast:** an inline refinement on a parameter (`fn pos(x: Int where x > 0)`)
  is enforced (`panic ... refinement failed: x > 0`). So is an inline refinement on a
  struct field at construction.
- **Expected:** per the reference, "An unproven one becomes a runtime check in debug
  builds". A literal `-5` passed as `Cents` should arguably fail `jig check` outright.
- **Impact:** `Cents` and `Qty` in the program are documentation only. I restated each
  one as an explicit `requires price >= 0` and similar.

### B3. Struct invariants and field refinements are not checked on mutation: COMPILER BUG

```
struct Span {
  lo: Int
  hi: Int
  invariant lo <= hi
}
struct Item {
  price: Int where price >= 0
}
fn Span.shrink(&mut self, d: Int) {
  self.hi -= d
}
fn main() effects {io} {
  var s = Span { lo: 0, hi: 2 }
  s.hi = -1                 // no panic
  s.shrink(10)              // no panic
  var it = Item { price: 1 }
  it.price -= 5             // no panic
  io.println(s.str() + " " + it.str())
}
```

- **Output (interpreter and native):** `Span { lo: 0, hi: -11 } Item { price: -4 }`,
  exit 0.
- **Construction is checked:** `Span { lo: 5, hi: 4 }` panics with
  `invariant failed: lo <= hi`.
- **Expected:** the reference says invariants are "checked on construction and
  mutation".
- **Workaround:** `Ledger.adjust_stock` asserts `item.stock + delta >= 0` explicitly.

### B4. Native backend emits invalid C for `Never` in a struct or tuple position: COMPILER BUG

This crashes the build, and it is triggered by the prelude's `unwrap()`:

```
struct P {
  x: Int
}
fn main() effects {io} {
  let o = Some(P { x: 1 })
  io.println(o.unwrap().x.str())
}
```

- **Interpreter:** `jig check` is fine and `jig run` prints `1`.
- **Native build:**

  ```
  error: C compiler failed (cc):
  .../unit1.c:64:10: error: incompatible types when assigning to type 'T2' from type 'jig_unit' {aka 'unsigned char'}
     64 |     l0 = ((jig_unit){0});
  ```

- **Generated C:** in my program, the monomorphized `Option.unwrap<Item>` contains
  `{ T4 v1213 = ((jig_unit){0}); ... }` after the `jig_panic` call in the `None` arm.
  The `Never` value from `panic(...)` is materialized as a unit literal and assigned to
  the struct-typed result.
- **Same failure with a user function:**
  `fn pick(o: Option<(Int, Int)>) -> (Int, Int) { return match o { Some(p) => p  None => panic("none") } }`
  fails the same way for struct or tuple result types. It builds for `Int`, `Str` and
  `List<Int>`, which accept the unit value through C's implicit conversions.
- **Expected:** `Never` converts to any type, as the reference says, so this must
  compile.
- **Workaround:** I replaced `self.items.get(sku).unwrap()` with a `match` whose arms
  are unit-typed (`ledger.jig`, `adjust_stock`). Test code still uses `.unwrap()`, which
  is fine because tests run only in the interpreter.

### B5. E0502 "unused import" fires when a package is used only in type positions: COMPILER BUG

```
// shapes/shapes.jig
pub struct Sq {
  pub side: Int
}
pub fn make(n: Int) -> Sq {
  return Sq { side: n }
}

// helper.jig (root package)
import "shapes"
fn area(s: shapes.Sq) -> Int {
  return s.side * s.side
}

// main.jig
import "std/io"
import "shapes"
fn main() effects {io} {
  io.println(area(shapes.make(3)).str())
}
```

- **The catch-22:** `error[E0502]: unused import "shapes"` points at `helper.jig:1`, with
  `fix: remove the import`. Applying the fix gives
  `error[E0201]: cannot find package "shapes"` at `fn area(s: shapes.Sq)`. No program
  that uses a package only in signatures can pass `jig check`.
- **Also hit by type aliases:** `type Err2 = m.E` alone also triggers E0502.
- **In gate1:** `main.jig` used `model` only in signatures (`model.Order`,
  `model.LedgerError`).
- **Workaround:** I moved the weight formatter into `model` (`model.format_weight`) so
  `main` uses `model` at value level.

### B6. E0603 "unused export" covers functions and methods but not other items: COMPILER BUG

- **Missed:** in a library package used by `main`, these pass `jig check` even though no
  other package uses them:
  - `pub const LIMIT`
  - `pub interface Sized`
  - `pub struct Box` with `pub n`
  - `pub fn Box.size` (reached only through an interface bound inside the package)
- **Flagged:** in the same package, `pub fn helper()` and `pub fn Box.twice` are flagged
  (`error[E0603]: "helper" is exported but no other package uses it`).
- **Expected:** "unused exports ... fail the build" should apply to every kind of item,
  or the reference should say it covers functions only. In gate1 the six `pub const`s in
  `ledger/pricing.jig` are still `pub`, and nothing flags them.

### B7. Interpreter and native differ on trap messages and backtraces: COMPILER BUG (minor)

```
fn main() effects {io} {
  let f = "1e300".to_float().unwrap_or(0.0)
  io.println((f as Int).str())
}
```

- **Interpreter:** `panic at main.jig:3:15: cannot convert 1e300 to "Int": out of range`.
- **Native:** `panic at main.jig:3:15: numeric conversion out of range`.
- **Same exit code:** both exit 101.
- **Backtraces:** native panics also omit the `in <fn> at <file:line>` call stack that
  the interpreter prints, for contract failures as well. stdout matched in every case I
  tried, but stderr does not.

### B8. `jig prove` and `jig trace` are documented but do not exist: REFERENCE GAP

Both print the usage text and exit 2. The reference describes `jig prove` as the checker
for contracts and for `unwrap()` proof obligations. Without it, every contract is a
runtime check only, which matters given B2 and B3. If this is intentional for Phase 1,
the reference should say so.

### Reference gaps

- **G1. A method cannot share a name with a field, even a private one.**
  `fn Item.name(self) -> Str` on a struct with a `name` field gives
  `error[E0202]: "Item" already has a field named "name"`. In the ledger,
  `fn Ledger.items(self)` and `fn Ledger.orders(...)` collided with private fields, so I
  renamed them `all_items` and `find_orders`. The reference doesn't say methods and
  fields share a namespace. Because interfaces are structural, a getter-style interface
  (`fn name(self) -> Str`) can never be satisfied by a struct that stores `name`
  (see L3).
- **G2. Assignment is a statement, not an expression, so it can't be a match arm body.**
  `Open => open += 1` gives
  `error[E0105]: expected a line break or "}" after the match arm, found "+="`. The fix
  is `Open => { open += 1 }`. The reference says match arms yield expressions but never
  says assignments aren't expressions.
- **G3. Shadowing is an error.** `let weight = ...` inside an arm that bound `weight`
  gives `error[E0505]: "weight" shadows another variable`. The reference doesn't say so.
- **G4. A custom `str` must be `pub` to work across packages.**
  `fn StockErr.str(self) -> Str` (private) makes `e.str()` in another package fail with
  `error[E0205]: method "str" is private`. It does not fall back to the built-in `Show`.
  The reference only says "Define `fn T.str(self) -> Str` to customize `Show`".
- **G5. Unit scoping is undocumented.**
  - Units are program-global. A private `unit g` in `model` is usable as `Float<g>` in
    `parse` and `main` without qualification.
  - `Float<model.g>` is a parse error:
    `error[E0105]: expected "*", "/" or ">" in a unit, found "."`.
  - The same unit can be declared in two packages without conflict, and an unused unit
    is not flagged.
  - `Int<g>` is accepted, but the reference only shows `Float<...>`.
- **G6. Where `T where pred` may appear is undocumented.** It works on parameters, struct
  fields and aliases. It is rejected on `let` (`let c: Int where c >= 0 = n` gives
  `E0105 expected "=" and an initial value`) and on return types
  (`fn mk(x: Int) -> Int where result >= 0 {` gives `E0105 function "mk" has no body` and
  `E0108 expected an item, found "where"`).
- **G7. Imports are per-file.** The reference says all files in a package share one
  namespace. Imports do not: each file must import what it uses (`helper.jig` in the B5
  repro).
- **G8. Dead-code roots are worded misleadingly.** The reference says "Roots are `main`,
  the `pub` API of a library package, ...". In a program with `main`, an imported
  package's `pub` items are not roots. Anything `pub` that no other package uses is
  E0603, and only `jig explain E0603` clarifies ("its package is not the root of a
  library"). I removed `pub` from 15 items.
- **G9. Std behaviour I had to discover by probing:**
  - `io.read_line()` strips `\n` and `\r\n` and returns the final unterminated line.
  - `Str.to_int` accepts `"+12"` and `" 5"`, so I wrote a strict `digits()` helper.
  - `Str.to_float` accepts `"1e300"`, `"nan"`, `"inf"` and `" 2.5"`.
  - `"a  b".split(" ")` yields empty strings.
  - `sort_by` takes a `less: fn(T, T) -> Bool`, and `sort` and `sort_by` work in place.
  - `Map.insert` on an existing key keeps its original position.
  - `Char.is_lower`, `is_upper` and `is_alnum`, and `Set.is_empty`, exist but are not
    listed.
  - Most of this came from `jig search`, which shows real signatures; the semantics came
    from probes.
- **G10. Variants can be constructed through a type alias, but the reference doesn't say
  so.** `type E2 = m.E` then `E2.A(1)` works. I learned this only after writing 43
  fully qualified `model.Type.Variant(...)` constructions (23 in program code, 20 in
  tests).
- **G11. Prelude names cannot be reused.** `pub enum Err` in a user package is
  `error[E0202]: "Err" is defined by the prelude and cannot be redefined`. That's
  reasonable, but undocumented; see D2 for the resulting cascade.

### Diagnostic quality

- **D1. A parse error in one match arm cascades into unrelated errors.** One
  `Open => open += 1` arm in `report.jig` produced 12 errors:
  - `unused variable` for `open`, `shipped`, `cancelled` and `o`, reported before the
    parse error itself;
  - `cannot find "Shipped" in this scope` and `cannot find "Cancelled" in this scope`;
  - `cannot find "_" in this scope`;
  - `missing "return"`;
  - `expected an item, found "let"`.

  Linting code that failed to parse, and resynchronizing at the wrong brace, buried the
  one real error. There is no fix suggestion either: "wrap the assignment in `{ }`" is
  the obvious one.
- **D2. One redefinition produced 19 errors.** Naming an enum `Err` gave one real error
  plus 18 follow-ons in the same file and its users, including
  `cannot find "self" in this scope`,
  `mismatched types: expected "Int", found "Str"` on string arms, and
  `this match arm can never match`. Once a type fails to declare, everything that
  mentions it should be suppressed.
- **D3. A parameter that shadows a prelude function gives a confusing error.** A
  parameter named `max` makes `max(len, 1)` report `error[E0303]: "Int" is not a
  function`, with no mention that `max` is a parameter shadowing the prelude function.
  This is also inconsistent with G3: shadowing a local is an error, but shadowing a
  prelude function is silently allowed.
- **D4. E0105 is a catch-all.** It covered "function has no body", "expected `=` and an
  initial value", "expected a line break after the match arm" and "expected `*`, `/` or
  `>` in a unit". A stable code per diagnostic is promised, but this one can't be looked
  up usefully: `jig explain E0105` can't explain all four. The "has no body" message for
  `-> Int where ... {` is actively misleading.
- **D5. Property-test counterexamples print `x = ()`.** Part of B1, but even a correct
  generator would need the message to show the generated value.
- **D6. Postcondition failures don't print values.** Precondition failures print the
  variables involved (`b = 10`, `a = 3`); postcondition failures do not
  (`postcondition failed: result.is_err() || ...` with no values).
- **D7. `jig search` indexes only prelude methods.**
  - `() -> Option<Str>` doesn't find `io.read_line`.
  - Name-like queries (`read_line`, `io`) return a dump of unrelated prelude methods
    instead of nothing.
  - `Int -> Str` doesn't find `Int.str()`.

  Search was otherwise the most useful tool: it gave the exact `sort_by` and
  `Map.insert` signatures that the reference only names.

### Language design

- **L1. Test-only helpers count as dead code, so tests can't share fixtures.**
  `fn shop() -> Ledger` (three `add_item` calls) and `fn stock_of(...)` gave
  `error[E0601]: "shop" is never reached from an entrypoint ... only tests use it; tests do not justify code`.
  18 of the 19 ledger rule tests now repeat their setup inline, about 45 extra lines,
  and the two session tests repeat their driver loop. "Tests do not justify code" is
  defensible for production code, but there is no documented way to write a test helper
  (a `test fn`, or helpers allowed in `*_test.jig`).
- **L2. No string interpolation, and the formatter makes concatenations tall.** The
  formatter breaks a long `+` chain into one operand per line. Running `jig fmt` grew
  the program from 1,312 to 1,422 code lines. One `added ...` message became 12 lines.
  I wrote `text.fill("{} of {}", [a, b])`, which is about 10 lines of Jig, to get
  readable output code. A `format`/`fill` in std, or interpolation, would remove most
  of this.
- **L3. Methods and fields share a namespace, which clashes with structural
  interfaces.** See G1. A struct can't expose a stored field through an interface method
  of the same name. You either rename the field or name every getter `get_x`.
- **L4. Updating a struct inside a Map is verbose.** With value semantics and no `&mut`
  into map entries, updating one field takes four lines:

  ```
  var item = self.item(sku)?
  item.stock += qty
  self.items.insert(sku, item)
  ```

  It's consistent with the model, just verbose; it appears 6 times in `ledger.jig`. List
  elements, by contrast, can be mutated in place (`xs[0].qty += 1`, and
  `self.orders[id - 1] = order`).
- **L5. Variants of an imported enum must be fully qualified.** Every construction is
  `model.LedgerError.OutOfStock(...)` (43 sites). Patterns are unqualified, which is
  pleasant, but construction has no `use`. A type alias helps (G10), but nobody would
  find that from the reference.

### What worked well

These were not friction points, but they matter for judging the gate:

- **Units:** algebra worked first try (`Float<kg> * Float<cent/kg>` gives `Float<cent>`,
  and `Float<g> / Float<g/kg>` gives `Float<kg>`).
- **Generics:** structural interface bounds work across packages; `model.OrderLine` and
  `model.Item` satisfy `ledger.Valued` with no declaration.
- **Patterns:** nested patterns such as `Err(Unknown(k))` and tuple patterns with
  guards both work.
- **Errors:** `?` on both `Option` and `Result`.
- **Contracts:** `requires`, and `ensures` with `old(...)` on `&mut self`, are enforced
  at runtime in both backends.
- **Other:** `Map` iteration order is stable, and `jig fmt` is idempotent.
- **Tools:** `jig check` takes milliseconds and the test runner's output is clear.
- **Native backend:** once B4 was worked around, it matched the interpreter
  byte-for-byte on stdout for every input I tried, including `--release`.

## 3. Verdict

**The gate passes, with significant caveats.** I wrote the full program from the
reference plus tool output alone, without opening compiler or std source. It meets every
requested check:

- zero `jig check` errors;
- 35 of 35 tests passing;
- `jig fmt --check` clean;
- identical interpreter and native output on the sample session.

The reference was enough to write idiomatic code. Most friction came from undocumented
rules (G1–G11) that the compiler reported quickly and clearly enough to fix in one or
two rounds.

The caveats are real. Three bugs blocked the program as first written and needed
workarounds:

- **B1:** property tests are unusable.
- **B4:** the native build fails on `Option<Struct>.unwrap()`.
- **B5:** imports used only in type positions are impossible.

Two more bugs are silent:

- **B2:** refinement aliases are never checked.
- **B3:** invariants are not checked on mutation.

An agent that trusted the reference would ship code whose `Cents >= 0` guarantees are
not enforced at all, and nothing in `check`, `test` or `run` would say so. I only found
B2 and B3 because I probed runtime behaviour on purpose. Before Phase 1 is declared
done, I recommend fixing B1–B5 and either implementing `jig prove` and `jig trace` or
removing them from the reference.
