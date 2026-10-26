//! Internal dumps for differential testing against the self-hosted compiler (`selfhost/`).

use std::fmt::Write;

/// The lexer's output for `src` (at offset 0): tokens, comments, blank lines and errors, one per
/// line, in the format `jigc tokens` prints.
pub fn tokens(src: &str) -> String {
    let lx = crate::syntax::lexer::lex(src, 0);
    let mut out = String::new();
    for t in &lx.tokens {
        let _ = writeln!(out, "token {:?} {} {}", t.kind, t.lo, t.hi);
    }
    for c in &lx.comments {
        let _ = writeln!(out, "comment {} {} {} {}", c.lo, c.hi, c.doc, c.trailing);
    }
    for b in &lx.blank_lines {
        let _ = writeln!(out, "blank {b}");
    }
    for d in &lx.diags {
        let _ = writeln!(out, "error {} {} {} {}", d.code.name(), d.span.lo, d.span.hi, d.message);
    }
    out
}
