//! Checking bodies on several threads must give exactly what checking on one gives: the same
//! diagnostics in the same order, and the same type ids in every table and body, so lowering,
//! proofs and generated code cannot tell the difference. Every example, the self-hosted
//! compiler and every UI and run test (most of them with errors) are checked both ways.

use jig::sema::defs::Program;
use jig::sema::load::Sources;
use jig::sema::ty::Ty;
use std::path::{Path, PathBuf};

fn fingerprint(p: &Program) -> Vec<String> {
    let mut out = Vec::new();
    let (ntys, nunits) = p.tys.sizes();
    out.push(format!("sizes {ntys} {nunits}"));
    for i in 0..ntys {
        out.push(format!("ty {i} {}", p.tys.display(Ty(i as u32))));
    }
    for d in &p.diags.list {
        out.push(format!("diag {:?} {}..{} {}", d.code.name(), d.span.lo, d.span.hi, d.message));
    }
    for (pi, t) in p.tables.iter_enumerated() {
        out.push(format!("pkg {pi:?} expr_ty {:?}", t.expr_ty));
        out.push(format!("pkg {pi:?} pat_ty {:?}", t.pat_ty));
        out.push(format!("pkg {pi:?} expr_res {:?}", t.expr_res));
        let mut inst: Vec<_> = t.expr_inst.iter().collect();
        inst.sort_by_key(|(e, _)| e.0);
        out.push(format!("pkg {pi:?} expr_inst {inst:?}"));
        let mut auto: Vec<_> = t.autoref.iter().collect();
        auto.sort_by_key(|(e, _)| e.0);
        out.push(format!("pkg {pi:?} autoref {auto:?}"));
    }
    for (f, b) in p.bodies.iter_enumerated() {
        if let Some(b) = b {
            let locals: Vec<_> = b.locals.iter().map(|l| (l.name, l.ty, l.used, l.mutated)).collect();
            out.push(format!("fn {f:?} {locals:?} refs {:?} effects {:?}", b.refs, b.used_effects));
        }
    }
    for (t, b) in p.test_bodies.iter_enumerated() {
        if let Some(b) = b {
            let locals: Vec<_> = b.locals.iter().map(|l| (l.name, l.ty)).collect();
            out.push(format!("test {t:?} {locals:?}"));
        }
    }
    out
}

fn compare(name: &str, sources: &Sources, dir: &Path) -> Option<String> {
    let one = fingerprint(&jig::sema::analyze_on(sources, dir, Some(1)));
    let four = fingerprint(&jig::sema::analyze_on(sources, dir, Some(4)));
    if one == four {
        return None;
    }
    let diff = one.iter().zip(&four).find(|(a, b)| a != b);
    Some(format!("{name}: {} vs {} lines, first difference {diff:?}", one.len(), four.len()))
}

fn single_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    v.sort();
    out.extend(v.into_iter().filter(|p| p.extension().is_some_and(|e| e == "jig")));
}

#[test]
fn threads_do_not_change_the_result() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let mut failures = Vec::new();
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(root.join("examples")).unwrap().filter_map(|e| e.ok()).map(|e| e.path()).collect();
    dirs.sort();
    dirs.push(root.join("selfhost"));
    dirs.push(root.join("compiler/tests/ui/packages"));
    for d in &dirs {
        if let Some(f) = compare(&d.display().to_string(), &Sources::Disk, d) {
            failures.push(f);
        }
    }
    let mut files = Vec::new();
    for d in ["compiler/tests/ui", "compiler/tests/run", "compiler/tests/fixtures"] {
        single_files(&root.join(d), &mut files);
    }
    assert!(files.len() > 20);
    for f in &files {
        let dir = PathBuf::from("/parallel").join(f.file_stem().unwrap());
        let mut m = jig::util::FxHashMap::default();
        m.insert(dir.join("main.jig"), std::fs::read_to_string(f).unwrap());
        if let Some(x) = compare(&f.display().to_string(), &Sources::Memory(m), &dir) {
            failures.push(x);
        }
    }
    assert!(failures.is_empty(), "{} programs differ:\n{}", failures.len(), failures.join("\n"));
}
