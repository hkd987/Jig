//! Semantic analysis: loading, collection, body checking, lints and dead code.

pub mod check;
pub mod collect;
pub mod dead;
pub mod defs;
pub mod exhaust;
pub mod load;
pub mod ty;

use defs::Program;
use load::Sources;
use std::path::Path;

/// Load and fully check the package in `dir` (and everything it imports).
pub fn analyze(sources: &Sources, dir: &Path) -> Program {
    let timing = std::env::var_os("JIG_TIME").is_some();
    let t0 = std::time::Instant::now();
    let mut prog = load::load_program(sources, dir);
    let t1 = std::time::Instant::now();
    collect::collect(&mut prog);
    let t2 = std::time::Instant::now();
    check::check_bodies(&mut prog);
    let t3 = std::time::Instant::now();
    prog.diags.sort();
    dead::check_dead_code(&mut prog);
    let t4 = std::time::Instant::now();
    if timing {
        let ms = |a: std::time::Instant, b: std::time::Instant| (b - a).as_secs_f64() * 1000.0;
        let lines: usize = prog.sm.files.iter().map(|f| f.line_starts.len()).sum();
        eprintln!(
            "jig: {lines} lines; load+parse {:.1} ms, collect {:.1} ms, check {:.1} ms, dead code {:.1} ms",
            ms(t0, t1),
            ms(t1, t2),
            ms(t2, t3),
            ms(t3, t4)
        );
    }
    prog
}
