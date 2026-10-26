//! Formatter tests: the syntax tour, golden cases of messy input, and properties that hold for
//! every input (re-parsing, idempotence, comment preservation, every target width).

use super::*;

const TOUR: &str = include_str!("../../tests/fixtures/syntax_tour.jig");

/// Drop the newline that starts each raw string in [`GOLDEN`].
fn text(s: &'static str) -> &'static str {
    s.strip_prefix('\n').unwrap_or(s)
}

fn golden(name: &str) -> (&'static str, &'static str) {
    let &(_, input, want) = GOLDEN.iter().find(|g| g.0 == name).expect("no such golden case");
    (text(input), text(want))
}

/// Every test input: the tour and the golden inputs.
fn inputs() -> Vec<(&'static str, &'static str)> {
    let mut v = vec![("tour", TOUR)];
    v.extend(GOLDEN.iter().map(|&(name, input, _)| (name, text(input))));
    v
}

/// Comment texts in source order.
fn comments_of(src: &str) -> Vec<String> {
    lexer::lex(src, 0).comments.iter().map(|c| src[c.lo as usize..c.hi as usize].to_string()).collect()
}

fn reformat(out: &str) -> String {
    format_source(out).unwrap_or_else(|e| panic!("formatted output does not parse:\n{e}\n{out}"))
}

/// Format `input`, expect `want`, and check that the output parses cleanly, formats to itself,
/// keeps every comment, and stays within the width except for lines that cannot break.
fn check(input: &str, want: &str) {
    let out = format_source(input).unwrap_or_else(|e| panic!("input does not parse:\n{e}"));
    assert_eq!(out, want, "\n--- got ---\n{out}\n--- want ---\n{want}");
    assert_eq!(reformat(&out), out, "formatting is not idempotent");
    assert_eq!(comments_of(&out), comments_of(input), "comments changed");
    for line in out.lines() {
        assert!(
            width_of(line) <= WIDTH || line.contains("//") || line.contains('"'),
            "line over {WIDTH} columns: {line}"
        );
    }
}

fn width_of(s: &str) -> usize {
    s.chars().count()
}

#[test]
fn tour_is_canonical() {
    let out = format_source(TOUR).unwrap_or_else(|e| panic!("the tour does not parse:\n{e}"));
    assert_eq!(out, TOUR, "the tour fixture is not in canonical form");
}

#[test]
fn tour_round_trips() {
    let out = format_source(TOUR).unwrap();
    assert_eq!(reformat(&out), out);
    assert_eq!(comments_of(&out), comments_of(TOUR));
}

#[test]
fn spacing_and_indentation() {
    let (input, want) = golden("spacing");
    check(input, want);
}

#[test]
fn comments_in_every_position() {
    let (input, want) = golden("comments");
    check(input, want);
}

#[test]
fn long_lists_and_operators_break() {
    let (input, want) = golden("breaking");
    check(input, want);
}

#[test]
fn method_chains_break_before_dots() {
    let (input, want) = golden("chains");
    check(input, want);
}

#[test]
fn contract_clauses_and_effects() {
    let (input, want) = golden("clauses");
    check(input, want);
}

#[test]
fn control_flow_and_else_on_next_line() {
    let (input, want) = golden("control");
    check(input, want);
}

#[test]
fn closures_inline_hugged_and_nested() {
    let (input, want) = golden("closures");
    check(input, want);
}

#[test]
fn comments_inside_expressions_and_lists() {
    let (input, want) = golden("placement");
    check(input, want);
}

#[test]
fn literals_types_and_patterns() {
    let (input, want) = golden("literals");
    check(input, want);
}

#[test]
fn blank_lines_between_items() {
    let (input, want) = golden("items");
    check(input, want);
}

#[test]
fn empty_and_comment_only_files() {
    check("", "");
    check("\n\n  \n", "");
    check("// only\n\n\n// comments\n", "// only\n\n// comments\n");
    check("fn f() {}", "fn f() {}\n");
}

#[test]
fn comments_are_preserved() {
    for (name, input) in inputs() {
        let out = format_source(input).unwrap();
        let (a, b) = (comments_of(input), comments_of(&out));
        let (mut sa, mut sb) = (a.clone(), b.clone());
        sa.sort();
        sb.sort();
        assert_eq!(sa, sb, "{name}: comments lost or duplicated");
        // These inputs also keep every comment's relative order.
        assert_eq!(a, b, "{name}: comments reordered");
    }
}

/// Narrow widths force every broken layout. Each result must parse, be a fixed point at its
/// width, keep the comments, and mean the same program: formatting it at the real width gives
/// the canonical text (blank lines aside, since breaking a short item changes the item-run
/// blank-line rule).
#[test]
fn every_width_reparses_to_the_same_program() {
    let non_blank = |s: &str| s.lines().filter(|l| !l.is_empty()).collect::<Vec<_>>().join("\n");
    for (name, input) in inputs() {
        let canon = format_source(input).unwrap();
        for w in (8..=WIDTH).step_by(4) {
            let out = format_source_width(input, w).unwrap();
            let again = format_source_width(&out, w)
                .unwrap_or_else(|e| panic!("{name} at width {w} does not parse:\n{e}\n{out}"));
            assert_eq!(again, out, "{name} at width {w} is not idempotent");
            assert_eq!(comments_of(&out), comments_of(input), "{name} at width {w}: comments changed");
            assert_eq!(non_blank(&reformat(&out)), non_blank(&canon), "{name} at width {w} changed meaning");
        }
    }
}

#[test]
fn rejects_unparsable_input() {
    let err = format_source("fn f( {\n").unwrap_err();
    assert!(err.contains("error[E0105]"), "{err}");
}

#[test]
fn formats_one_file_of_many() {
    let a = "fn a() {}\n";
    let b = "// b\nfn  b(x:Int)->Int{return x}\n";
    let mut sm = SourceMap::new();
    let fa = sm.add("a.jig".into(), a.to_string());
    let fb = sm.add("b.jig".into(), b.to_string());
    let mut ast = Ast::default();
    let mut interner = LocalInterner::default();
    for f in [fa, fb] {
        let file = sm.file(f);
        assert!(parser::parse_file(&mut ast, f, &file.text, file.base, &mut interner).diags.is_empty());
    }
    let out = format_file(&ast, 1, b, sm.file(fb).base);
    assert_eq!(out, "// b\nfn b(x: Int) -> Int {\n  return x\n}\n");
    assert_eq!(format_file(&ast, 0, a, sm.file(fa).base), a);
}

/// Golden cases: (name, messy input, canonical output).
const GOLDEN: &[(&str, &str, &str)] = &[
    (
        "spacing",
        r##"



import   "std/io"
import "std/math"



import "geom"
const  A :Int=1
const B: Int = 0xff_ff


const C: Float = 1e-9
type Pair<T>=(T,T)
type One = (Int,)
unit    m
   pub struct Point<T:Ord+Hash>{pub x:T
y : T


   invariant x<=y,y>=x
}
enum E{A,B(Int,Str),C(),D(r:Float)}
fn   f( a :Int,b: &mut   List<Int> ) ->Int{
let x=a+  b.len( )*2


    var y :Int=- x
      y+=1
  y <<= 2
    let p=Point{x:1,y:2}
    let e = Point {}
  bump(&mut  y)


  return(x+y)
}
fn g( ){}
fn h()->Float<1/s>{return 1.0 as Float<1/s>}
fn k(t: Float<kg*m/s^2>) -> fn(Int, Str) -> Bool effects {io,fs} { return check }
fn Point < T > . norm2 ( self ) -> T { return self . x * self . x + self . y * self . y }
"##,
        r##"
import "std/io"
import "std/math"

import "geom"

const A: Int = 1
const B: Int = 0xff_ff

const C: Float = 1e-9

type Pair<T> = (T, T)
type One = (Int,)

unit m

pub struct Point<T: Ord + Hash> {
  pub x: T
  y: T

  invariant x <= y
  invariant y >= x
}

enum E {
  A
  B(Int, Str)
  C()
  D(r: Float)
}

fn f(a: Int, b: &mut List<Int>) -> Int {
  let x = a + b.len() * 2

  var y: Int = -x
  y += 1
  y <<= 2
  let p = Point { x: 1, y: 2 }
  let e = Point {}
  bump(&mut y)

  return (x + y)
}

fn g() {}

fn h() -> Float<1/s> {
  return 1.0 as Float<1/s>
}

fn k(t: Float<kg*m/s^2>) -> fn(Int, Str) -> Bool effects {io, fs} {
  return check
}

fn Point<T>.norm2(self) -> T {
  return self.x * self.x + self.y * self.y
}
"##,
    ),
    (
        "comments",
        r##"
// header comment

// second header line
import "std/io" // trailing import
// between imports
import "std/math"
/// Doc for P.
struct P { // open struct
  // lead x
  x: Int // trail x

  // lead y
  y: Int
  // end of struct
}
enum E {
  A // trail A
  // lead B
  B
  // end of enum
}
pub interface Shape {
  // area
  fn area(self) -> Float // trailing method

  fn scale(&mut self, k: Float) requires k > 0.0
}
fn f(x: Int) -> Int { // open body
  // lead let
  let a = x // trail let


  // after blank
  let b = match a { // open match
    // lead arm 1
    1 => 2 // trail arm 1
    // between arms

    _ => 3
    // end of match
  }
  let p = P { x: 1, // trail field x
    y: 2 }
  let q = foo(a, // after a
    b)
  let r = a + // inside binary
    b
  return a
  // end of block
}
// between items
fn g() {}

fn empty() { // nothing here
}
fn empty2() {
  // todo
}
// end of file
"##,
        r##"
// header comment

// second header line
import "std/io" // trailing import
// between imports
import "std/math"

/// Doc for P.
struct P { // open struct
  // lead x
  x: Int // trail x

  // lead y
  y: Int
  // end of struct
}

enum E {
  A // trail A
  // lead B
  B
  // end of enum
}

pub interface Shape {
  // area
  fn area(self) -> Float // trailing method

  fn scale(&mut self, k: Float)
    requires k > 0.0
}

fn f(x: Int) -> Int { // open body
  // lead let
  let a = x // trail let

  // after blank
  let b = match a { // open match
    // lead arm 1
    1 => 2 // trail arm 1
    // between arms

    _ => 3
    // end of match
  }
  let p = P {
    x: 1 // trail field x
    y: 2
  }
  let q = foo(
    a, // after a
    b,
  )
  // inside binary
  let r = a + b
  return a
  // end of block
}

// between items
fn g() {}

fn empty() { // nothing here
}

fn empty2() {
  // todo
}
// end of file
"##,
    ),
    (
        "breaking",
        r##"
fn process(first_argument: Int, second_argument: Str, third_argument: List<Int>, fourth: Bool) -> Int {
  let total = compute_something(first_argument, second_argument, third_argument, fourth, 1234567)
  let n = compute_something(first_argument, compute_something(first_argument, second_argument, x, y, z), 1)
  let xs = [first_argument, first_argument + 1, first_argument + 2, first_argument + 3, first_argument + 4]
  let t = (first_argument, second_argument, third_argument, fourth, first_argument, first_argument, 1)
  let ok = first_argument > 0 && second_argument.len() > 0 && third_argument.len() > 0 && fourth && ok
  let msg = "first: " + first_argument.str() + ", second: " + second_argument + ", fourth: " + "xxxxxx"
  if first_argument > 1000000 && second_argument.len() > 1000000 || third_argument.len() > 10000000 {
    return 1
  }
  let mixed = first_argument * 1000 + second_argument.len() * 2000 - third_argument.len() * 3000 + 400000
  let cmp = compute_something(first_argument, second_argument, third_argument) > threshold_for(fourth, 1)
  let p = Point { x: first_argument * 1000000, y: second_argument.len() * 1000000, z: third_argument }
  return total
}
"##,
        r##"
fn process(
  first_argument: Int,
  second_argument: Str,
  third_argument: List<Int>,
  fourth: Bool,
) -> Int {
  let total = compute_something(first_argument, second_argument, third_argument, fourth, 1234567)
  let n = compute_something(
    first_argument,
    compute_something(first_argument, second_argument, x, y, z),
    1,
  )
  let xs = [
    first_argument,
    first_argument + 1,
    first_argument + 2,
    first_argument + 3,
    first_argument + 4,
  ]
  let t = (
    first_argument,
    second_argument,
    third_argument,
    fourth,
    first_argument,
    first_argument,
    1,
  )
  let ok = first_argument > 0 &&
    second_argument.len() > 0 &&
    third_argument.len() > 0 &&
    fourth &&
    ok
  let msg = "first: " +
    first_argument.str() +
    ", second: " +
    second_argument +
    ", fourth: " +
    "xxxxxx"
  if first_argument > 1000000 && second_argument.len() > 1000000 ||
    third_argument.len() > 10000000 {
    return 1
  }
  let mixed = first_argument * 1000 +
    second_argument.len() * 2000 -
    third_argument.len() * 3000 +
    400000
  let cmp = compute_something(first_argument, second_argument, third_argument) >
    threshold_for(fourth, 1)
  let p = Point {
    x: first_argument * 1000000
    y: second_argument.len() * 1000000
    z: third_argument
  }
  return total
}
"##,
    ),
    (
        "chains",
        r##"
fn chains(input: Str) -> Result<Int, Str> {
  let port = input.trim().to_int().ok_or("not a number: " + input + " (expected digits only, base 10)")?
  let names = accounts.values().filter(fn(a) { a.bal > 0 }).map(fn(a) { a.name.to_upper() }).join(", ")
  let total = accounts.values().fold(0, fn(acc, a) {
    let fee = a.bal / 100
    acc + a.bal - fee
  })
  let deep = self.items.get(index).unwrap_or(default_item_value_for_missing_entries_in_the_table).name
  io.println("the total balance of all the accounts in the whole ledger is: " + total.str() + " cents")
  let w = World.new().with_accounts(starting_accounts).with_clock(fake_clock).with_seed(12345678).build()
  let n = xs
      .map(fn(x) { x * 2 })
          .len()
  return Ok(port)
}
"##,
        r##"
fn chains(input: Str) -> Result<Int, Str> {
  let port = input
    .trim()
    .to_int()
    .ok_or("not a number: " + input + " (expected digits only, base 10)")?
  let names = accounts
    .values()
    .filter(fn(a) { a.bal > 0 })
    .map(fn(a) { a.name.to_upper() })
    .join(", ")
  let total = accounts.values().fold(0, fn(acc, a) {
    let fee = a.bal / 100
    acc + a.bal - fee
  })
  let deep = self.items
    .get(index)
    .unwrap_or(default_item_value_for_missing_entries_in_the_table).name
  io.println(
    "the total balance of all the accounts in the whole ledger is: " + total.str() + " cents",
  )
  let w = World
    .new()
    .with_accounts(starting_accounts)
    .with_clock(fake_clock)
    .with_seed(12345678)
    .build()
  let n = xs.map(fn(x) { x * 2 }).len()
  return Ok(port)
}
"##,
    ),
    (
        "clauses",
        r##"
fn contract(x: Int, y: Int) -> Int requires x > 0, y > 0, x < 1000000000, y < 1000000000, x + y < 1000000000, x * y < 10000000000000000 ensures result > 0 ensures result < 2000000000 effects {io} { return x + y }
fn pure_marker() effects {} {}
fn eff() effects {io} {
  io.println("hi")
}
fn short(a: Int) -> Int requires a > 0 { return a }
fn order(a: Int) -> Int
  effects {}
  ensures result > a
  requires a > 0
{
  return a + 1
}
test "long property"(first: Int, second: Int, third: Int) requires first > 0, second > 0, third > 0, first + second + third < 1000000 {
  assert(first + second + third > 0)
}
test "longer property"(first: Int, second: Int, third: Int) requires first > 0, second > 0, third > 0, first + second + third < 1000000, first * second * third < 1000000000 {
  assert(true)
}
fn loop_inv(n: Int) {
  for i in 0..n invariant i >= 0 {
    work(i)
  }
  for i in 0..n invariant some_long_invariant_condition(i, n), another_long_condition(i, n, n, n, n) {
    work(i)
  }
  for i in 0..n invariant some_long_invariant_condition(i, n, n, n, n, n, n, n), another_long_condition(i, n, n, n, n) {
    work(i)
  }
}
"##,
        r##"
fn contract(x: Int, y: Int) -> Int
  requires x > 0
  requires y > 0
  requires x < 1000000000
  requires y < 1000000000
  requires x + y < 1000000000
  requires x * y < 10000000000000000
  ensures result > 0, result < 2000000000
  effects {io}
{
  return x + y
}

fn pure_marker() effects {} {}

fn eff() effects {io} {
  io.println("hi")
}

fn short(a: Int) -> Int
  requires a > 0
{
  return a
}

fn order(a: Int) -> Int
  requires a > 0
  ensures result > a
  effects {}
{
  return a + 1
}

test "long property"(first: Int, second: Int, third: Int)
  requires first > 0, second > 0, third > 0, first + second + third < 1000000
{
  assert(first + second + third > 0)
}

test "longer property"(first: Int, second: Int, third: Int)
  requires first > 0,
    second > 0,
    third > 0,
    first + second + third < 1000000,
    first * second * third < 1000000000
{
  assert(true)
}

fn loop_inv(n: Int) {
  for i in 0..n invariant i >= 0 {
    work(i)
  }
  for i in 0..n
    invariant some_long_invariant_condition(i, n), another_long_condition(i, n, n, n, n)
  {
    work(i)
  }
  for i in 0..n
    invariant some_long_invariant_condition(i, n, n, n, n, n, n, n),
      another_long_condition(i, n, n, n, n)
  {
    work(i)
  }
}
"##,
    ),
    (
        "control",
        r##"
fn f(m: Map<Str, Int>, a: Int, b: Int) -> Int {
  if a > b {
    return 1
  }
  else if a < b {
    return 2
  }
  else {
    return 3
  }
  if a > b { work() }
  if a > b {} else { work() }
  let big = if a > b { "big" }
  else { "small" }
  let v = if a > b { compute_something_long(a, b, c, d) } else { compute_something_else_long(a, b, c) }
  let w = match a { 1 => { 2 }
  2 => { let q = 3
    q }, _ => 0 }
  let z = match a {}
  for (k, v) in m { total += v }
  for i in 0..10 { continue }
  for a < b { break }
  for x in xs {}
  for {}
  for {
    return a
  }
  return
}
"##,
        r##"
fn f(m: Map<Str, Int>, a: Int, b: Int) -> Int {
  if a > b {
    return 1
  } else if a < b {
    return 2
  } else {
    return 3
  }
  if a > b {
    work()
  }
  if a > b {} else {
    work()
  }
  let big = if a > b { "big" } else { "small" }
  let v = if a > b {
    compute_something_long(a, b, c, d)
  } else {
    compute_something_else_long(a, b, c)
  }
  let w = match a {
    1 => { 2 }
    2 => {
      let q = 3
      q
    }
    _ => 0
  }
  let z = match a {}
  for (k, v) in m {
    total += v
  }
  for i in 0..10 {
    continue
  }
  for a < b {
    break
  }
  for x in xs {}
  for {}
  for {
    return a
  }
  return
}
"##,
    ),
    (
        "closures",
        r##"
fn f(xs: List<Int>) -> Int {
  let a = xs.map(fn(x) { let y = x * 2
    y + 1 })
  let b = xs.iter().map(fn(x) {
    let y = x * 2
    y
  })
  let c = xs.filter(fn(x) { x > 2 && x < 1000000 && some_predicate_function(x, 1000000, 2000000, 300000) })
  let d = xs.map(fn(x) { xs.map(fn(y) { x + y }) })
  let e = xs.map(fn(x) { xs.map(fn(y) {
    let z = x + y
    z
  }) })
  xs.each(fn(x) {
    io.println(x.str())
  })
  let r = xs.map(fn(x) {
    // double it
    x * 2
  }).filter(fn(x) { x > 0 })
  let g = fn(x: Int) -> Int { return x }
  let h = fn() {}
  let k = fn(x: Int) -> Bool { x > 0 }
  return a
}
"##,
        r##"
fn f(xs: List<Int>) -> Int {
  let a = xs.map(fn(x) {
    let y = x * 2
    y + 1
  })
  let b = xs.iter().map(fn(x) {
    let y = x * 2
    y
  })
  let c = xs.filter(fn(x) {
    x > 2 && x < 1000000 && some_predicate_function(x, 1000000, 2000000, 300000)
  })
  let d = xs.map(fn(x) { xs.map(fn(y) { x + y }) })
  let e = xs.map(fn(x) {
    xs.map(fn(y) {
      let z = x + y
      z
    })
  })
  xs.each(fn(x) { io.println(x.str()) })
  let r = xs
    .map(fn(x) {
      // double it
      x * 2
    })
    .filter(fn(x) { x > 0 })
  let g = fn(x: Int) -> Int {
    return x
  }
  let h = fn() {}
  let k = fn(x: Int) -> Bool { x > 0 }
  return a
}
"##,
    ),
    (
        "placement",
        r##"
type T = // the type
  Int
fn params(a: Int, // first
  b: Int) -> Int // after sig
  requires a > 0 // positive
  // ensures next
  ensures result > 0
{
  return a
}
fn f() effects { io, // console
  fs } {
  if a {
    x()
  } // after then
  else {
    y()
  }
  let r = xs
    // before map
    .map(f)
    .filter(g)
  let m = match a {
    A => // after arrow
      1
  }
  let k = fn(a, // first param
    b) { a + b }
  foo(
    // nothing
  )
  bar( // open paren
    1, 2)
  let p = Point {
    // lead x

    x: 1

    // lead y
    y: 2 }
  let q = [ // open list
    1, 2 // two
    // end of list
  ]
  return
}
fn g<T: Ord, // bound
  U>() {}
"##,
        r##"
// the type
type T = Int

fn params(
  a: Int, // first
  b: Int,
) -> Int // after sig
  requires a > 0 // positive
  // ensures next
  ensures result > 0
{
  return a
}

fn f()
  // console
  effects {io, fs}
{
  // after then
  if a {
    x()
  } else {
    y()
  }
  // before map
  let r = xs.map(f).filter(g)
  let m = match a {
    // after arrow
    A => 1
  }
  let k = fn(
    a, // first param
    b,
  ) {
    a + b
  }
  foo(
    // nothing
  )
  bar( // open paren
    1,
    2,
  )
  let p = Point {
    // lead x

    x: 1

    // lead y
    y: 2
  }
  let q = [ // open list
    1,
    2, // two
    // end of list
  ]
  return
}

// bound
fn g<T: Ord, U>() {}
"##,
    ),
    (
        "literals",
        r##"
fn strings() {
  let s = "tab\there \"quoted\" \u{1F600} \u{7} back\\slash"
  let c = '\''
  let d = '"'
  let e = '\n'
  let n = 1_000_000 + 0b1010 + 0o17 + 0xFF + 1.5e3 + 2.0
}
step "the {thing} happens"
pub fn thing_happens(w: &mut World, thing: Str) {
  w.things.push(thing)
}
test "no params" {
  let r = parse("80")?.port.unwrap()
  let s = foo(bar(baz(1)))[0].0
  let t = List<Int>.new()
  let u = xs[..2]
  let v = forall i in 0..n: xs[i] > 0
  let w = exists i in 0..n: xs[i] == 0
  let neg = -(-x)
  let nn = !!b
  let bits = ~x
  let o: Option<fn(Int) -> Int effects {io}> = None
  let unit_val: () = ()
  let one = (x,)
}
fn arms(s: Shape) -> Float {
  return match s {
    Circle(r) if r > 0.0 => {
      // positive
      r * r
    }
    Rect(w, h) | Square(w, h) => w * h
    Pair((a, b), _) => a
    Single((a,)) => a
    Lit(-1) => 0.0
    Empty() => 0.0
    _ => 0.0
  }
}
"##,
        r##"
fn strings() {
  let s = "tab\there \"quoted\" 😀 \u{7} back\\slash"
  let c = '\''
  let d = '"'
  let e = '\n'
  let n = 1_000_000 + 0b1010 + 0o17 + 0xFF + 1.5e3 + 2.0
}

step "the {thing} happens"
pub fn thing_happens(w: &mut World, thing: Str) {
  w.things.push(thing)
}

test "no params" {
  let r = parse("80")?.port.unwrap()
  let s = foo(bar(baz(1)))[0].0
  let t = List<Int>.new()
  let u = xs[..2]
  let v = forall i in 0..n: xs[i] > 0
  let w = exists i in 0..n: xs[i] == 0
  let neg = -(-x)
  let nn = !!b
  let bits = ~x
  let o: Option<fn(Int) -> Int effects {io}> = None
  let unit_val: () = ()
  let one = (x,)
}

fn arms(s: Shape) -> Float {
  return match s {
    Circle(r) if r > 0.0 => {
      // positive
      r * r
    }
    Rect(w, h) | Square(w, h) => w * h
    Pair((a, b), _) => a
    Single((a,)) => a
    Lit(-1) => 0.0
    Empty() => 0.0
    _ => 0.0
  }
}
"##,
    ),
    (
        "items",
        r##"
import "a"

import "b"
import "c"
const X: Int = 1
const Y: Int = 2

const Z: Int = 3
unit m

unit s
type A = Int
type B = Int
struct S {}
struct T {}
pub fn one() {}
fn two() {}
const LONG: List<Int> = [1000000, 2000000, 3000000, 4000000, 5000000, 6000000, 7000000, 8000000, 9000000]
const AFTER: Int = 4
// about B2
const B2: Int = 5
"##,
        r##"
import "a"

import "b"
import "c"

const X: Int = 1
const Y: Int = 2

const Z: Int = 3

unit m

unit s

type A = Int
type B = Int

struct S {}

struct T {}

pub fn one() {}

fn two() {}

const LONG: List<Int> = [
  1000000,
  2000000,
  3000000,
  4000000,
  5000000,
  6000000,
  7000000,
  8000000,
  9000000,
]

const AFTER: Int = 4
// about B2
const B2: Int = 5
"##,
    ),
];
