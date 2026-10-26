//! Prover tests: each `tests/prove/<name>/` package is proved from scratch and the verbose
//! report must equal `tests/prove/<name>.out`. Set `JIG_BLESS=1` to rewrite the expected
//! files. Skipped when no SMT solver is installed.

use jig::sema::load::Sources;
use jig::verify::{prove, render, ProveOpts};
use std::path::{Path, PathBuf};

fn have_solver() -> bool {
    jig::verify::smt::solver_version().is_some()
}

fn report_for(dir: &Path, cache: Option<PathBuf>) -> (String, jig::verify::Report) {
    let mut prog = jig::sema::analyze(&Sources::Disk, dir);
    assert!(!prog.diags.has_errors(), "{} does not check", dir.display());
    let opts = ProveOpts { cache_dir: cache, timeout_ms: 5000, filter: None };
    let r = prove(&mut prog, &opts).expect("prove");
    let text = render(&prog, &r, true);
    (text, r)
}

/// Drop the timing and make paths relative to the test directory.
fn normalize(text: &str, root: &Path) -> String {
    let prefix = format!("{}/", root.display());
    text.lines()
        .map(|l| {
            let l = l.replace(&prefix, "");
            match l.rfind(", ") {
                Some(i) if l.starts_with("proved ") && l.ends_with(" s") => l[..i].to_string(),
                _ => l,
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n"
}

#[test]
fn golden() {
    if !have_solver() {
        eprintln!("skipping: no SMT solver");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/prove");
    let mut dirs: Vec<PathBuf> =
        std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    assert!(!dirs.is_empty());
    let bless = std::env::var_os("JIG_BLESS").is_some();
    let mut failures = Vec::new();
    for d in &dirs {
        let (text, _) = report_for(d, None);
        let actual = normalize(&text, &root);
        let out = d.with_extension("out");
        if bless {
            std::fs::write(&out, &actual).unwrap();
            continue;
        }
        let expected = std::fs::read_to_string(&out).unwrap_or_default();
        if actual != expected {
            failures.push(format!("== {} ==\n--- expected ---\n{expected}--- actual ---\n{actual}", d.display()));
        }
    }
    if !failures.is_empty() {
        panic!("{} prover tests differ:\n{}", failures.len(), failures.join("\n"));
    }
}

/// A one-function change re-proves only that function; callers keep their cached proofs
/// because they depend on its contract, not its body.
#[test]
fn cache_reproves_only_the_changed_function() {
    if !have_solver() {
        eprintln!("skipping: no SMT solver");
        return;
    }
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("prove-cache");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/prove/basics/main.jig");
    let text = std::fs::read_to_string(&src).unwrap();
    std::fs::write(work.join("main.jig"), &text).unwrap();
    let cache = work.join(".jig/proofs");
    let (first, r1) = report_for(&work, Some(cache.clone()));
    assert!(r1.fns.iter().all(|f| !f.cached));
    let (second, r2) = report_for(&work, Some(cache.clone()));
    assert!(r2.fns.iter().all(|f| f.cached), "everything comes from the cache the second time");
    let strip = |s: &str| s.lines().filter(|l| !l.starts_with("proved ")).map(|l| l.to_string()).collect::<Vec<_>>();
    assert_eq!(strip(&normalize(&first, &work)), strip(&normalize(&second, &work)));
    // Change one body (not its contract) and shift every later line.
    let changed = text.replace("  if x > hi {\n    return hi\n  }", "\n  if hi < x {\n    return hi\n  }");
    assert_ne!(changed, text);
    std::fs::write(work.join("main.jig"), &changed).unwrap();
    let (third, r3) = report_for(&work, Some(cache));
    let redone: Vec<&str> = r3.fns.iter().filter(|f| !f.cached).map(|f| f.name.as_str()).collect();
    assert_eq!(redone, vec!["clamp"]);
    assert_eq!(strip(&normalize(&first, &work)).len(), strip(&normalize(&third, &work)).len());
}
