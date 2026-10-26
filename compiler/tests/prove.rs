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
    let opts = ProveOpts { cache_dir: cache, timeout_ms: 5000, filter: None, std: false };
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

/// A precondition proved at every call site is not checked again in the callee; one unproven
/// call, or a use of the function as a value, keeps the check.
#[test]
fn preconditions_proved_at_every_call_leave_the_callee() {
    let cc = jig::codegen::build::c_compiler(false);
    let have_cc = std::process::Command::new(&cc).arg("--version").output().is_ok_and(|o| o.status.success());
    if !have_solver() || !have_cc {
        eprintln!("skipping: needs an SMT solver and a C compiler");
        return;
    }
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("prove-entries");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/prove/entries/main.jig");
    std::fs::copy(&src, work.join("main.jig")).unwrap();
    let cache = work.join(".jig/proofs");
    let _ = report_for(&work, Some(cache.clone()));
    let prog = jig::sema::analyze(&Sources::Disk, &work);
    let proven = jig::verify::cached_proven(&prog, &cache);
    let mut names: Vec<&str> = proven.entries.iter().map(|&f| prog.defs.fns[f].name.as_str()).collect();
    names.sort();
    assert_eq!(names, vec!["half"]);
    let mut prog = jig::sema::analyze(&Sources::Disk, &work);
    let dir = work.join("build");
    let opts = jig::codegen::build::BuildOpts { release: true, build_dir: dir.clone(), out: work.join("exe"), proven };
    jig::codegen::build::build(&mut prog, &opts).expect("build");
    let out = std::process::Command::new(work.join("exe")).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout, "5 2\n3\n2 3\n");
    // Release builds keep unproven contract checks: the bad call to `third` still stops.
    assert!(String::from_utf8_lossy(&out.stderr).contains("precondition failed: x >= 0"));
    let c = std::fs::read_to_string(dir.join("exe.c")).unwrap();
    assert_eq!(c.matches("precondition failed: x >= 0").count(), 2, "third and quarter keep their checks");
}

/// Builds do not check the standard library's postconditions (D22), so every one of them must
/// prove, along with every trap in the standard library's Jig code.
#[test]
fn std_proves_completely() {
    if !have_solver() {
        eprintln!("skipping: no SMT solver");
        return;
    }
    let dir = PathBuf::from("/std-proof");
    let mut m = jig::util::FxHashMap::default();
    m.insert(dir.join("main.jig"), "fn main() {\n}\n".to_string());
    let mut prog = jig::sema::analyze(&Sources::Memory(m), &dir);
    assert!(!prog.diags.has_errors());
    let opts = ProveOpts { cache_dir: None, timeout_ms: 5000, filter: None, std: true };
    let r = prove(&mut prog, &opts).expect("prove");
    let text = render(&prog, &r, false);
    let (cp, ct, sp, st) = r.counts();
    assert!(ct > 20 && cp == ct && sp == st, "the standard library does not prove:\n{text}");
    let (with, proved) = r.contract_coverage(&prog);
    assert_eq!(with, proved, "{text}");
    assert!(with * 10 >= r.fns.len() * 8, "fewer than 80% of std functions have contracts:\n{text}");
}
