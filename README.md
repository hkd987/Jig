# Jig

Jig is a systems language that agents write and humans can trust: Go's small surface,
Rust-class performance, and verification built into the compiler. Humans write feature
scenarios and contracts. Agents write the implementation. The compiler proves, or failing
that tests, that the implementation meets them.

```
fn transfer(from: &mut Acct, to: &mut Acct, amt: Cents)
  requires amt > 0, from.bal >= amt
  ensures from.bal + to.bal == old(from.bal + to.bal)
  effects {}
{
  from.bal -= amt
  to.bal += amt
}
```

**Status:** pre-alpha, Phase 1 of 4. See [the plan](docs/plan.md).

- [Language reference](docs/reference.md)
- [Build plan](docs/plan.md)
- [Decisions](docs/decisions.md)

## Quick start

```
cargo build --release                         # the compiler: target/release/jig
jig check examples/bank                       # types, effects, lints, dead code (milliseconds)
jig test examples/bank                        # unit tests on the deterministic runner
jig run examples/hello                        # run main on the interpreter
jig build examples/hello --release            # native binary through C and clang -O2
```

Native builds need a C compiler (`cc`; `clang` for release builds).

## License

MIT
