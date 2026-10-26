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

/// Proofs remove runtime checks from native builds without changing behavior; unproven
/// checks stay and still fire.
#[test]
fn proofs_remove_checks_from_builds() {
    let cc = jig::codegen::build::c_compiler(false);
    let have_cc = std::process::Command::new(&cc).arg("--version").output().is_ok_and(|o| o.status.success());
    if !have_solver() || !have_cc {
        eprintln!("skipping: needs an SMT solver and a C compiler");
        return;
    }
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("prove-elide");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/prove/semantics/main.jig");
    std::fs::copy(&src, work.join("main.jig")).unwrap();
    let cache = work.join(".jig/proofs");
    let build = |name: &str, proven: jig::verify::Proven| -> (String, String) {
        let mut prog = jig::sema::analyze(&Sources::Disk, &work);
        let dir = work.join(format!("build-{name}"));
        let opts =
            jig::codegen::build::BuildOpts { release: false, build_dir: dir.clone(), out: work.join(name), proven };
        jig::codegen::build::build(&mut prog, &opts).expect("build");
        let out = std::process::Command::new(work.join(name)).output().unwrap();
        let c: String = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|e| e == "c") && !p.ends_with("jig_rt.c"))
            .map(|p| std::fs::read_to_string(p).unwrap())
            .collect();
        (format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)), c)
    };
    let (plain_out, plain_c) = build("plain", Default::default());
    let _ = report_for(&work, Some(cache.clone()));
    let prog = jig::sema::analyze(&Sources::Disk, &work);
    let proven = jig::verify::cached_proven(&prog, &cache);
    assert!(!proven.contract.is_empty() && !proven.safety.is_empty());
    let (proved_out, proved_c) = build("proved", proven);
    assert_eq!(plain_out, proved_out);
    assert!(proved_out.contains("`Span` requires lo <= hi"), "the unproven invariant still fires: {proved_out}");
    let count = |c: &str, pat: &str| c.matches(pat).count();
    assert!(count(&proved_c, "failed: ") < count(&plain_c, "failed: "));
    assert!(count(&proved_c, "jig_add_i64") < count(&plain_c, "jig_add_i64"));
}
