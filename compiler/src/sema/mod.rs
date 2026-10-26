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

/// The whole standard library, checked: a program that imports every standard package (for
/// `jig prove --std`).
pub fn analyze_std() -> Program {
    let dir = std::path::PathBuf::from("/std");
    let mut src: String = load::STD_SOURCES.iter().map(|(p, _, _)| format!("import \"{p}\"\n")).collect();
    src.push_str("\nfn main() {\n}\n");
    let mut files = crate::util::FxHashMap::default();
    files.insert(dir.join("main.jig"), src);
    let mut prog = analyze(&Sources::Memory(files), &dir);
    // The imports are there to load the packages.
    prog.diags.list.retain(|d| d.code != crate::diag::codes::E0502);
    prog
}

pub fn analyze(sources: &Sources, dir: &Path) -> Program {
    analyze_on(sources, dir, None)
}

/// A required module's code answers to its own project's lints, not to this one's.
fn drop_lints_in_modules(prog: &mut Program) {
    let external: crate::util::FxHashSet<crate::diag::FileId> =
        prog.pkgs.iter().filter(|p| p.external).flat_map(|p| p.ast.files.iter().map(|f| f.file)).collect();
    if external.is_empty() {
        return;
    }
    let sm = &prog.sm;
    prog.diags.list.retain(|d| {
        let lint = (500..700).contains(&d.code.0);
        !lint || sm.file_of(d.span.lo).is_none_or(|f| !external.contains(&f))
    });
}

/// `analyze`, checking bodies on `threads` threads (`None`: decide from the program's size and
/// `JIG_THREADS`). The result does not depend on it.
pub fn analyze_on(sources: &Sources, dir: &Path, threads: Option<usize>) -> Program {
    let timing = std::env::var_os("JIG_TIME").is_some();
    let t0 = std::time::Instant::now();
    let mut prog = load::load_program(sources, dir);
    let t1 = std::time::Instant::now();
    collect::collect(&mut prog);
    let t2 = std::time::Instant::now();
    match threads {
        Some(n) => check::check_bodies_on(&mut prog, n),
        None => check::check_bodies(&mut prog),
    }
    prog.scenarios = crate::feature::bind::bind(&mut prog);
    let t3 = std::time::Instant::now();
    quiet_after_syntax_errors(&mut prog);
    drop_lints_in_modules(&mut prog);
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
