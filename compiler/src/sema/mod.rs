//! Semantic analysis: loading, collection, body checking, lints and dead code.

pub mod check;
pub mod collect;
pub mod defs;
pub mod exhaust;
pub mod load;
pub mod ty;

use defs::Program;
use load::Sources;
use std::path::Path;

/// Load and fully check the package in `dir` (and everything it imports).
pub fn analyze(sources: &Sources, dir: &Path) -> Program {
    let mut prog = load::load_program(sources, dir);
    collect::collect(&mut prog);
    check::check_bodies(&mut prog);
    prog.diags.sort();
    prog
}
