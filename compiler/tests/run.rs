//! Execution tests: each `tests/run/*.jig` program runs on the interpreter with faked
//! effects, and its output must equal the sibling `.out` file.

use jig::mir::interp::{Effects, Interp, Trap};
use jig::mir::lower::Lowerer;
use jig::mir::LowerOpts;
use jig::sema::load::Sources;
use std::path::{Path, PathBuf};

fn run_one(path: &Path) -> Result<(), String> {
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let src = std::fs::read_to_string(path).unwrap();
    let dir = PathBuf::from(format!("/run/{name}"));
    let mut m = jig::util::FxHashMap::default();
    m.insert(dir.join("main.jig"), src);
    let mut prog = jig::sema::analyze(&Sources::Memory(m), &dir);
    if prog.diags.has_errors() {
        let s: String = prog.diags.list.iter().map(|d| d.render(&prog.sm, false)).collect();
        return Err(format!("check failed:\n{s}"));
    }
    let expected = std::fs::read_to_string(path.with_extension("out")).unwrap_or_default();
    let out = jig::runner::with_big_stack(move || {
        let mut lw = Lowerer::new(&mut prog, LowerOpts { check_contracts: true });
        let main = lw.main().expect("main");
        let mut it = Interp::new(&mut lw, Effects::fake(42));
        let r = it.call(main, Vec::new());
        let mut out = std::mem::take(&mut it.fx.stdout);
        match r {
            Ok(_) => {}
            Err(Trap::Exit(c)) => out.push_str(&format!("<exit {c}>\n")),
            Err(Trap::Panic { msg, .. }) => out.push_str(&format!("<panic: {msg}>\n")),
        }
        out
    });
    if out != expected {
        return Err(format!("output differs\n--- expected ---\n{expected}--- actual ---\n{out}"));
    }
    Ok(())
}

#[test]
fn run() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run");
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jig"))
        .collect();
    paths.sort();
    let mut failures = Vec::new();
    for p in &paths {
        if let Err(e) = run_one(p) {
            failures.push(format!("== {} ==\n{e}", p.display()));
        }
    }
    assert!(!paths.is_empty());
    if !failures.is_empty() {
        panic!("{} of {} run tests failed:\n{}", failures.len(), paths.len(), failures.join("\n"));
    }
}
