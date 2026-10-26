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
/// A package that failed to parse reports only its syntax errors: recovery drops code, and
/// what follows (unknown names, unused variables, missing returns) is noise that buries the
/// one real mistake.
fn quiet_after_syntax_errors(prog: &mut defs::Program) {
    let is_syntax = |d: &crate::diag::Diagnostic| (100..200).contains(&d.code.0);
    // Errors whose recovery skips source text (a bad literal or a stray `;` loses nothing).
    let drops_code = |d: &crate::diag::Diagnostic| matches!(d.code.0, 101 | 102 | 105 | 108);
    let bad_files: Vec<crate::diag::FileId> =
        prog.diags.list.iter().filter(|d| drops_code(d)).filter_map(|d| prog.sm.file_of(d.span.lo)).collect();
    if bad_files.is_empty() {
        return;
    }
    let mut quiet: crate::util::FxHashSet<crate::diag::FileId> = crate::util::FxHashSet::default();
    for pkg in prog.pkgs.iter() {
        let files: Vec<crate::diag::FileId> = pkg.ast.files.iter().map(|f| f.file).collect();
        if files.iter().any(|f| bad_files.contains(f)) {
            quiet.extend(files);
        }
    }
    let sm = &prog.sm;
    prog.diags.list.retain(|d| is_syntax(d) || sm.file_of(d.span.lo).is_none_or(|f| !quiet.contains(&f)));
}

pub fn analyze(sources: &Sources, dir: &Path) -> Program {
    let timing = std::env::var_os("JIG_TIME").is_some();
    let t0 = std::time::Instant::now();
    let mut prog = load::load_program(sources, dir);
    let t1 = std::time::Instant::now();
    collect::collect(&mut prog);
    let t2 = std::time::Instant::now();
    check::check_bodies(&mut prog);
    prog.scenarios = crate::feature::bind::bind(&mut prog);
    let t3 = std::time::Instant::now();
    quiet_after_syntax_errors(&mut prog);
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
