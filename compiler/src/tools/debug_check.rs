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

/// `jig debug c DIR [proofs]`: the C program `jig build --release` compiles for the package in
/// DIR, without proofs (no check is removed) or, with `proofs`, with the cached proofs of
/// `jig prove` (in the project's `.jig/proofs`) removing the checks they discharge. The
/// self-hosted back end prints the same text.
pub fn c(dir: &Path, proofs: bool) -> Result<String, String> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut prog = crate::sema::analyze_on(&Sources::Disk, &dir, Some(1));
    if prog.diags.has_errors() {
        return Err(format!("{} has errors", dir.display()));
    }
    let proven = if proofs {
        let root = crate::sema::load::find_project_root(&Sources::Disk, &dir).unwrap_or_else(|| dir.clone());
        crate::verify::cached_proven(&prog, &root.join(".jig").join("proofs"))
    } else {
        Default::default()
    };
    let mut lw = crate::mir::lower::Lowerer::new(&mut prog, crate::mir::LowerOpts { check_contracts: true });
    lw.mir.elide = proven;
    let c = crate::codegen::c::generate(&mut lw)?;
    Ok(c.unity())
}

fn base_name(b: &crate::verify::vc::Base, tys: &crate::sema::ty::TyCtx) -> String {
    use crate::verify::vc::Base;
    match b {
        Base::Bool => "bool".into(),
        Base::Int(k) => format!("int {}", k.name()),
        Base::Char => "char".into(),
        Base::Tag(n) => format!("tag {n}"),
        Base::Str => "str".into(),
        Base::Float => "float".into(),
        Base::Opaque(t) => format!("opaque {}", tys.display(*t)),
    }
}

/// `jig debug vc DIR [std]`: the verification conditions `jig prove` gives the solver for each
/// function of the package in DIR (with `std`, the standard library's): its declarations,
/// then each obligation with its path facts and goal, then the terms counterexamples show and
/// the inputs they can be replayed with. The self-hosted prover prints the same text.
pub fn vc(dir: &Path, std: bool) -> Result<String, String> {
    let dir = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut prog = crate::sema::analyze_on(&Sources::Disk, &dir, Some(1));
    if prog.diags.has_errors() {
        return Err(format!("{} has errors", dir.display()));
    }
    let root = prog.project_root.clone().unwrap_or_else(|| dir.clone());
    let names = file_names(&prog, &root);
    let mut fns = crate::verify::targets(&prog, None, std);
    fns.sort_by_key(|&f| prog.defs.fns[f].span.lo);
    let titles: Vec<String> = fns
        .iter()
        .map(|&f| {
            let mut t = format!("fn {} ", prog.defs.fns[f].display_name(&prog.defs, &prog.tys));
            place(&mut t, &prog, &names, prog.defs.fns[f].span);
            t
        })
        .collect();
    let frames = crate::verify::frame::compute_all(&prog);
    let reads = crate::verify::frame::compute_reads(&prog);
    let mut out = String::new();
    let mut places: Vec<Vec<String>> = Vec::new();
    let mut all = Vec::new();
    {
        let mut gen = crate::verify::vc::Gen::new(&mut prog);
        gen.frames = frames;
        gen.reads = reads;
        for &f in &fns {
            all.push(gen.function(f));
        }
    }
    for vcs in all.iter().flatten() {
        places.push(
            vcs.obligations
                .iter()
                .map(|o| {
                    let mut p = String::new();
                    place(&mut p, &prog, &names, o.span);
                    p
                })
                .collect(),
        );
    }
    let mut pi = 0;
    for (i, vcs) in all.into_iter().enumerate() {
        let Some(vcs) = vcs else { continue };
        out.push_str(&titles[i]);
        out.push('\n');
        for d in &vcs.decls {
            let _ = writeln!(out, "decl {d}");
        }
        for (k, o) in vcs.obligations.iter().enumerate() {
            let _ = writeln!(out, "ob {} {} {} {}", o.kind.name(), places[pi][k], o.elidable as u8, o.msg);
            for p in &o.pc {
                let _ = writeln!(out, "pc {p}");
            }
            let _ = writeln!(out, "goal {}", o.goal);
        }
        for (label, term, b) in &vcs.show {
            let _ = writeln!(out, "show {label} {term} {}", base_name(b, &prog.tys));
        }
        match &vcs.inputs {
            Some(ins) => {
                for (t, leaves) in ins {
                    let ls: Vec<String> =
                        leaves.iter().map(|(t, b)| format!("{t}:{}", base_name(b, &prog.tys))).collect();
                    let _ = writeln!(out, "input {} {}", prog.tys.display(*t), ls.join(" "));
                }
            }
            None => out.push_str("inputs none\n"),
        }
        pi += 1;
    }
    Ok(out)
}
