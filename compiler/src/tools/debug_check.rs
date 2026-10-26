//! `jig debug check DIR`: what the checker concludes about the package in DIR and everything
//! it imports. One line per diagnostic (`error CODE FILE LO HI MESSAGE`), then the type of
//! every expression and pattern of the project's own packages (`expr FILE LO HI TYPE`,
//! `pat ...`). Files are named relative to the project root (standard library files as
//! `<std>/NAME.jig`) and byte ranges are relative to the file. The self-hosted checker
//! (`selfhost/`) prints the same lines.

use crate::diag::Span;
use crate::sema::defs::Program;
use crate::sema::load::Sources;
use std::fmt::Write;
use std::path::Path;

/// Each file's name in the dump: relative to the project root.
fn file_names(prog: &Program, root: &Path) -> Vec<String> {
    prog.sm
        .files
        .iter()
        .map(|f| f.path.strip_prefix(root).unwrap_or(&f.path).to_string_lossy().replace('\\', "/"))
        .collect()
}

fn place(out: &mut String, prog: &Program, names: &[String], span: Span) {
    match prog.sm.file_of(span.lo) {
        Some(fid) if !span.is_dummy() => {
            let f = prog.sm.file(fid);
            let _ = write!(out, "{} {} {}", names[fid.0 as usize], span.lo - f.base, span.hi.saturating_sub(f.base));
        }
        _ => out.push_str("- 0 0"),
    }
}

pub fn check(dir: &Path) -> String {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let prog = crate::sema::analyze_on(&Sources::Disk, &dir, Some(1));
    let root = prog.project_root.clone().unwrap_or_else(|| dir.clone());
    let names = file_names(&prog, &root);
    let mut out = String::new();
    for d in &prog.diags.list {
        let _ = write!(out, "error {} ", d.code.name());
        place(&mut out, &prog, &names, d.span);
        let _ = writeln!(out, " {}", d.message);
    }
    for (pid, pkg) in prog.pkgs.iter_enumerated() {
        if pkg.is_std {
            continue;
        }
        let t = &prog.tables[pid];
        for (what, spans, tys) in [
            ("expr", pkg.ast.exprs.iter().map(|e| e.span).collect::<Vec<_>>(), &t.expr_ty),
            ("pat", pkg.ast.pats.iter().map(|p| p.span).collect::<Vec<_>>(), &t.pat_ty),
        ] {
            for (i, span) in spans.into_iter().enumerate() {
                out.push_str(what);
                out.push(' ');
                place(&mut out, &prog, &names, span);
                out.push(' ');
                out.push_str(&prog.tys.display(tys[i]));
                out.push('\n');
            }
        }
    }
    out
}
