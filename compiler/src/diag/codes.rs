//! The registry of stable diagnostic codes. Codes never change meaning once released; retired
//! codes are kept so old reports stay explainable.
//!
//! Ranges: E01 syntax · E02 names and packages · E03 types · E04 effects · E045 matches ·
//! E05 lints · E06 dead code · E07 verification · E08 features · E09 tests and tools.

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct Code(pub u16);

pub struct CodeInfo {
    pub code: Code,
    pub slug: &'static str,
    pub summary: &'static str,
    pub explain: &'static str,
}

impl Code {
    pub fn name(self) -> String {
        format!("E{:04}", self.0)
    }
    pub fn info(self) -> Option<&'static CodeInfo> {
        ALL.iter().find(|c| c.code == self)
    }
    pub fn parse(s: &str) -> Option<Code> {
        let s = s.trim().trim_start_matches(['E', 'e']);
        s.parse::<u16>().ok().map(Code).filter(|c| c.info().is_some())
    }
}

macro_rules! codes {
    ($($name:ident = $num:literal, $slug:literal, $summary:literal, $explain:literal;)*) => {
        $(pub const $name: Code = Code($num);)*
        pub static ALL: &[CodeInfo] = &[
            $(CodeInfo { code: Code($num), slug: $slug, summary: $summary, explain: $explain },)*
        ];
    };
}

codes! {
    // ---- E01: syntax ----
    E0101 = 101, "unexpected-char", "unexpected character",
        "The lexer found a character that cannot start any token. Jig source is UTF-8; \
         identifiers are ASCII letters, digits and `_`. Remove the character or put it inside a \
         string literal.";
    E0102 = 102, "unterminated-string", "unterminated string literal",
        "A string literal is missing its closing `\"`. Strings cannot span lines; use `\\n`.";
    E0103 = 103, "invalid-escape", "invalid escape sequence",
        "Valid escapes are `\\n \\t \\r \\0 \\\\ \\\" \\'` and `\\u{XXXX}` with 1 to 6 hex digits \
         naming a Unicode scalar value.";
    E0104 = 104, "invalid-number", "invalid number literal",
        "Integer literals are decimal, `0x` hex or `0b` binary, with optional `_` separators. \
         Float literals need digits on both sides of the `.` (`1.0`, not `1.`), and an optional \
         exponent (`1e9`, `2.5e-3`). A literal must fit its type.";
    E0105 = 105, "unexpected-token", "unexpected token",
        "The parser expected something else at this point. The message lists what was expected. \
         Common causes: a binary operator at the start of a line (put it at the end of the \
         previous line instead), a missing `{`, or `else` on its own line after a missing `}`.";
    E0106 = 106, "invalid-char", "invalid character literal",
        "A character literal holds exactly one Unicode scalar value: `'a'`, `'\\n'`, `'\\u{e9}'`.";
    E0107 = 107, "semicolon", "semicolons are not used",
        "Jig ends statements at line breaks. Put each statement on its own line and remove the `;`.";
    E0108 = 108, "invalid-item", "expected an item",
        "Only `import`, `fn`, `struct`, `enum`, `interface`, `type`, `const`, `unit`, `test` and \
         `step` declarations may appear at the top level of a file.";

    // ---- E02: names and packages ----
    E0201 = 201, "unresolved-name", "cannot find name",
        "No variable, function, type or package with this name is in scope. Check the spelling, \
         add the missing `import`, or declare the item. Names from another package are written \
         `pkg.name` and must be `pub`.";
    E0202 = 202, "duplicate-definition", "name defined more than once",
        "Each name may be declared once per package (functions, types, constants) or once per \
         type (methods, fields, variants). Rename one of them.";
    E0203 = 203, "unresolved-import", "cannot find package",
        "The import path does not name a directory under the project root (the directory with \
         `jig.mod`) or a standard library package. Standard library packages are `std/io`, \
         `std/fs`, `std/os`, `std/time`, `std/rand` and `std/math`.";
    E0204 = 204, "import-cycle", "import cycle",
        "Packages form a directed acyclic graph. Move the shared code into a third package that \
         both can import.";
    E0205 = 205, "private-item", "item is private",
        "The item exists but is not marked `pub` in its package. Mark it `pub` if it is part of \
         the package's API, or use a public function instead.";
    E0206 = 206, "unknown-field", "no such field",
        "The type has no field with this name, or the field is private to another package.";
    E0207 = 207, "unknown-method", "no such method",
        "The type has no method with this name. Methods are declared as `fn Type.name(self, ...)` \
         in the type's own package; see the reference for built-in methods on `Str`, `List`, \
         `Map`, `Set`, `Option` and `Result`.";
    E0208 = 208, "unknown-variant", "no such variant",
        "The enum has no variant with this name. Patterns name variants without the enum \
         (`Circle(r)`); expressions qualify them (`Shape.Circle(1.0)`).";
    E0209 = 209, "name-conflict", "name conflict",
        "Two imports, or an import and a declaration, bind the same name.";
    E0210 = 210, "no-project-root", "package import outside a project",
        "Importing a non-standard package needs a project root: a directory containing a \
         `jig.mod` file with the line `module <name>`.";

    // ---- E03: types ----
    E0301 = 301, "type-mismatch", "mismatched types",
        "The expression has a different type from the one required here. Jig has no implicit \
         conversions: convert numbers with `as`, wrap values in `Some(...)` or `Ok(...)` \
         explicitly, and call `.str()` to turn values into strings.";
    E0302 = 302, "arg-count", "wrong number of arguments",
        "The call passes a different number of arguments from the function's parameters.";
    E0303 = 303, "not-callable", "not callable",
        "Only functions, methods, closures and enum variants with fields can be called.";
    E0304 = 304, "missing-fields", "missing fields in struct literal",
        "Jig has no zero values: a struct literal must give every field a value.";
    E0305 = 305, "cannot-infer", "type annotations needed",
        "The compiler cannot work out a type here. Add an annotation, e.g. \
         `let xs: List<Int> = []` or `let m: Map<Str, Int> = Map.new()`.";
    E0306 = 306, "bad-operands", "invalid operand types",
        "The operator is not defined for these types. Arithmetic needs two numbers of the same \
         type (and unit); `+` also joins two `Str`s; `&&` and `||` need `Bool`s; comparisons need \
         equal types that satisfy `Eq` or `Ord`.";
    E0307 = 307, "immutable", "cannot mutate an immutable value",
        "Only `var` variables, `&mut` parameters and `&mut self` receivers can be assigned or \
         mutated. Declare the variable with `var`, or take the parameter as `&mut T`.";
    E0308 = 308, "missing-return", "missing return",
        "A function with a return type must end every path with `return`. Jig function bodies \
         do not return their last expression.";
    E0309 = 309, "bad-try", "invalid use of `?`",
        "`?` works on `Result<T, E>` inside a function returning `Result<_, E>` with the same `E`, \
         and on `Option<T>` inside a function returning `Option<_>`. Convert errors explicitly \
         with `.map_err(...)` or `.ok_or(...)`.";
    E0310 = 310, "bad-cast", "invalid cast",
        "`as` converts between numeric types, between `Char` and integers, and adds or removes \
         units of measure. Other conversions are methods, e.g. `n.str()` or `s.to_int()`.";
    E0311 = 311, "not-indexable", "cannot index this value",
        "Only `List<T>` (by `Int`, giving `T`) and `Str` (by `Int`, giving the byte as `U8`) can be \
         indexed, and both can be sliced with a range `a..b`. Use `m.get(k)` for maps.";
    E0312 = 312, "bound-not-met", "type does not satisfy a bound",
        "A generic parameter requires an interface the type does not implement. User interfaces \
         are satisfied by declaring each method (`fn T.method(self, ...)`). Functions never \
         satisfy `Eq`; `Float` does not satisfy `Ord` or `Hash`.";
    E0313 = 313, "type-arg-count", "wrong number of type arguments",
        "The generic type or function takes a different number of type arguments.";
    E0314 = 314, "bad-mut-arg", "invalid `&mut` argument",
        "A `&mut T` parameter needs `&mut place` at the call site, where the place is a `var`, a \
         `&mut` parameter, or a field or element of one. `&mut` may only appear on parameters and \
         call arguments; it cannot be stored in variables, fields or return types.";
    E0315 = 315, "aliased-mut", "overlapping `&mut` arguments",
        "Two `&mut` arguments to one call refer to the same place, or one contains the other. \
         Jig forbids this so a function can reason about its parameters locally.";
    E0316 = 316, "break-outside-loop", "`break` or `continue` outside a loop",
        "`break` and `continue` may only appear inside a `for` loop (and not inside a closure \
         defined in it).";
    E0317 = 317, "unit-mismatch", "mismatched units of measure",
        "`+`, `-` and comparisons need operands with the same unit. Multiply or divide to combine \
         units, or convert explicitly with `as Float<unit>`.";
    E0318 = 318, "bad-main", "invalid `main` function",
        "`main` takes no parameters, has no type parameters, and returns `()`. Read arguments \
         with `os.args()` and set the exit code with `os.exit(code)`.";
    E0319 = 319, "infinite-type", "recursive struct has infinite size",
        "A struct contains itself by value. Store the nested values in a `List` or an `Option` of \
         an enum, or model the structure as an enum (whose recursive payloads are boxed \
         automatically).";
    E0320 = 320, "contract-context", "contract syntax used out of place",
        "`result` and `old(...)` are only meaningful in `ensures` clauses; `forall` and `exists` \
         only in contracts, invariants, refinements and assertions.";
    E0321 = 321, "impure-contract", "contract is not pure",
        "Contracts, invariants and refinements must be pure: they may not call functions with \
         effects, mutate anything, or panic.";
    E0322 = 322, "duplicate-member", "duplicate field, variant or parameter",
        "A struct's fields, an enum's variants and a function's parameters must have distinct \
         names.";
    E0323 = 323, "bad-pattern", "pattern does not match the type",
        "The pattern's shape (variant, tuple arity, literal type) does not fit the type of the \
         value being matched.";
    E0324 = 324, "not-a-struct", "not a struct type",
        "Struct literal syntax `Name { field: value }` needs a struct type. Enum variants are \
         constructed with `Enum.Variant(args)`.";
    E0325 = 325, "value-in-type", "expected a type",
        "A value or package was used where a type is required.";
    E0326 = 326, "type-in-value", "expected a value",
        "A type was used where a value is required. Construct a struct with `Name { ... }` or call \
         an associated function with `Name.func(...)`.";
    E0327 = 327, "bad-receiver", "invalid method receiver",
        "Methods take `self` or `&mut self` as their first parameter. Calling a `&mut self` \
         method needs a mutable receiver (a `var`, or a `&mut` parameter).";
    E0328 = 328, "foreign-method", "method on a type from another package",
        "Methods can only be declared in the package that declares the type.";
    E0329 = 329, "const-expr", "not a constant expression",
        "`const` values must be literals, or arithmetic and comparisons on other constants.";
    E0330 = 330, "never-returns", "unreachable expression type",
        "This expression can never produce a value (for example `panic(...)` or `return`) in a \
         position that needs one.";

    // ---- E04: effects ----
    E0401 = 401, "undeclared-effect", "effect not declared",
        "The function performs an effect (directly or by calling a function that declares it) \
         that its own `effects {...}` clause does not list. Add the effect to the clause, or \
         restructure so the effectful call happens in the caller.";
    E0402 = 402, "unknown-effect", "unknown effect",
        "Effects are `io`, `fs`, `net`, `clock`, `rand`, `env`, `spawn` and `ffi`.";
    E0403 = 403, "unused-effect", "declared effect is never used",
        "The function declares an effect it never performs. Remove it so callers do not have to \
         declare it too.";

    // ---- E045: match ----
    E0451 = 451, "non-exhaustive", "non-exhaustive match",
        "Some values of the matched type reach no arm. The message gives an example. Add arms \
         for the missing cases, or a final `_ =>` arm if they should share one behavior.";
    E0452 = 452, "unreachable-arm", "match arm can never match",
        "Every value this arm matches is already handled by earlier arms. Remove it or reorder \
         the arms.";

    // ---- E05: lints (always errors) ----
    E0501 = 501, "unused-variable", "unused variable",
        "A variable or parameter is never read. Remove it, or name it `_` if a pattern requires \
         a binding.";
    E0502 = 502, "unused-import", "unused import",
        "The imported package is never used. Remove the import.";
    E0503 = 503, "needless-var", "variable does not need to be `var`",
        "The variable is declared with `var` but never mutated. Use `let`.";
    E0504 = 504, "naming", "naming convention",
        "Types, variants and interfaces are `UpperCamel`; functions, variables, fields and \
         packages are `snake_case`; constants are `UPPER_SNAKE`. Units are lower case.";
    E0505 = 505, "shadowing", "name shadows another binding",
        "A local binding reuses the name of another local in an enclosing scope of the same \
         function. Choose a different name.";
    E0506 = 506, "unreachable-code", "unreachable code",
        "This code comes after a `return`, `break`, `continue` or `panic` and can never run.";
    E0507 = 507, "unused-result", "unused result",
        "The call returns a value that is ignored. Use it, or discard it explicitly with \
         `let _ = ...` when ignoring it is intended.";
    E0508 = 508, "self-comparison", "comparison or assignment of a value with itself",
        "The expression compares or assigns a value to itself, which is always true, always \
         false, or does nothing.";
    E0509 = 509, "constant-condition", "condition is always the same",
        "The condition is a literal `true` or `false`. Use `for { }` for an infinite loop and \
         remove dead branches.";
    E0510 = 510, "needless-mut", "parameter does not need `&mut`",
        "The parameter is taken as `&mut` but never mutated. Take it by value.";

    // ---- E06: dead code ----
    E0601 = 601, "dead-function", "function is never reached",
        "No entrypoint reaches this function. Entrypoints are `main`, the `pub` API of a library \
         package, and step functions used by a scenario. Tests do not justify code. Delete the \
         function, or use it.";
    E0602 = 602, "unused-field", "field is never read",
        "No reachable code reads this field. Remove it.";
    E0603 = 603, "unused-export", "exported item is never used",
        "The item is `pub` but no other package in the program uses it, and its package is not \
         the root of a library. Remove `pub`, or remove the item.";
    E0604 = 604, "unused-variant", "variant is never constructed",
        "No reachable code constructs this enum variant, so matches on it are impossible. Remove \
         the variant.";
    E0605 = 605, "unused-step", "step is never used",
        "No scenario in any `.feature` file uses this step function.";
    E0606 = 606, "unused-item", "item is never used",
        "This type, constant, interface or unit is never used by reachable code.";

    // ---- E07: verification ----
    E0701 = 701, "precondition", "precondition may not hold",
        "The caller cannot show that the callee's `requires` holds here. Strengthen the caller's \
         own `requires`, add a check before the call, or weaken the callee's precondition.";
    E0702 = 702, "postcondition", "postcondition may not hold",
        "Some path through the function ends in a state that violates its `ensures`.";
    E0703 = 703, "invariant-entry", "loop invariant may not hold on entry",
        "The loop invariant is false before the first iteration.";
    E0704 = 704, "invariant-preserved", "loop invariant may not be preserved",
        "An iteration that starts with the invariant true may end with it false.";
    E0705 = 705, "assertion", "assertion may fail",
        "The `assert` condition can be false.";
    E0706 = 706, "overflow", "arithmetic may overflow",
        "The operation can exceed the range of its integer type. Bound the inputs with \
         `requires` or refinements, or use `wrapping_add` and friends if wrap-around is intended.";
    E0707 = 707, "bounds", "index may be out of bounds",
        "The index can be negative or at least the length. Check it, iterate with `for x in xs`, \
         or add a `requires` relating the index to `xs.len()`.";
    E0708 = 708, "div-zero", "division by zero possible",
        "The divisor can be zero. Check it first or require it to be non-zero.";
    E0709 = 709, "refinement", "refinement may not hold",
        "A value flowing into a refined type (`T where pred`) may not satisfy the predicate.";
    E0710 = 710, "struct-invariant", "struct invariant may not hold",
        "Constructing or mutating the struct can break its `invariant`.";
    E0711 = 711, "unwrap", "unwrap may fail",
        "`unwrap()` is called on a value that may be `None` or `Err`.";
    E0712 = 712, "panic-reachable", "panic is reachable",
        "Some input reaches this `panic`.";
    E0713 = 713, "unknown", "proof inconclusive",
        "The solver timed out or gave up. The obligation is checked at run time in debug builds \
         and covered by generated property tests. Simplify the code, add intermediate `assert`s, \
         or add loop invariants.";
    E0714 = 714, "refuted", "contract violated",
        "A concrete input violates the contract; the counterexample was replayed and confirmed. \
         This is a real bug in the implementation or the contract.";
    E0715 = 715, "solver-missing", "SMT solver not available",
        "`jig prove` needs Z3 on the PATH (or `JIG_Z3` set to its path). Install the pinned \
         version with `pip install z3-solver==5.3.2.0`.";

    // ---- E08: features ----
    E0801 = 801, "gherkin-syntax", "invalid feature file",
        "Feature files use the Gherkin subset: `Feature:`, `Scenario:`, `Scenario Outline:` with \
         `Examples:` tables, and steps starting with `Given`, `When`, `Then`, `And` or `But`. \
         Hooks, tags and backgrounds are not supported.";
    E0802 = 802, "no-step", "no step matches",
        "No `step` function's pattern matches this line. Write one, e.g. \
         `step \"an account {name} with balance {amt}\" fn ...`.";
    E0803 = 803, "ambiguous-step", "several steps match",
        "More than one step pattern matches this line. Make the patterns more specific.";
    E0804 = 804, "step-type", "step argument does not fit the parameter type",
        "A placeholder's text does not parse as the parameter's type: `Str` needs a quoted string, \
         integer and float types need numeric literals, `Bool` needs `true` or `false`.";
    E0805 = 805, "bad-step", "invalid step function",
        "Every `{placeholder}` must name a parameter, and every parameter except an optional \
         first `w: &mut World` must appear as a placeholder. Step functions return `()`.";
    E0806 = 806, "uncovered-entrypoint", "entrypoint is not covered by any scenario",
        "Every entrypoint must be exercised by at least one scenario.";

    // ---- E09: tests and tools ----
    E0901 = 901, "test-failed", "test failed",
        "A test, property test or scenario failed. The report gives the failing assertion and \
         the input values.";
    E0902 = 902, "io-error", "cannot read or write a file",
        "The compiler could not read a source file or write an output file.";
    E0903 = 903, "build-failed", "native build failed",
        "The linker or a backend tool failed. The message includes its output.";
    E0904 = 904, "unsupported", "not supported yet",
        "This construct parses and type-checks but is not supported by this compiler stage yet.";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_unique_and_parse() {
        let mut seen = std::collections::HashSet::new();
        for c in ALL {
            assert!(seen.insert(c.code), "duplicate code {}", c.code.name());
        }
        assert_eq!(Code::parse("E0301"), Some(E0301));
        assert_eq!(Code::parse("0301"), Some(E0301));
        assert_eq!(E0301.name(), "E0301");
        assert_eq!(Code::parse("E9999"), None);
    }
}
