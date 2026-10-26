//! Native back end tests: every `tests/run/*.jig` program is compiled to C, built with the
//! system C compiler and run; its output must match the interpreter's `.out` file. Skipped
//! when no C compiler is installed. Release builds drop contract checks; a `.release.out` file
//! holds the expected output when that changes it.

use jig::codegen::build::{build, BuildOpts};
use jig::sema::load::Sources;
use std::path::{Path, PathBuf};
use std::process::Command;

fn have_cc() -> bool {
    Command::new(jig::codegen::build::c_compiler(false)).arg("--version").output().is_ok_and(|o| o.status.success())
}

/// Turn a native run's stderr and status into the interpreter's `<panic: ...>` / `<exit N>`
/// markers.
fn markers(stderr: &str, code: Option<i32>) -> String {
    if let Some(i) = stderr.find("panic at ") {
        let after = &stderr[i + "panic at ".len()..];
        let msg = after.split_once(": ").map_or(after, |(_, m)| m).trim_end();
        return format!("<panic: {msg}>\n");
    }
    match code {
        Some(0) | None => String::new(),
        Some(c) => format!("<exit {c}>\n"),
    }
}

fn run_one(src: &Path, work: &Path, release: bool) -> Result<(), String> {
    let name = src.file_stem().unwrap().to_string_lossy().to_string();
    let dir = work.join(&name);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(src, dir.join("main.jig")).unwrap();
    let mut prog = jig::sema::analyze(&Sources::Disk, &dir);
    if prog.diags.has_errors() {
        return Err("check failed".into());
    }
    let exe = dir.join(if release { "release" } else { "debug" });
    let opts = BuildOpts { release, build_dir: dir.join("build"), out: exe.clone(), proven: Default::default() };
    build(&mut prog, &opts)?;
    // JIG_MEMCHECK=1 runs debug builds under valgrind: reference counting must neither leak
    // nor touch freed memory.
    let memcheck = !release && std::env::var_os("JIG_MEMCHECK").is_some();
    let mut cmd = if memcheck {
        let mut c = Command::new("valgrind");
        c.args(["-q", "--error-exitcode=99", "--leak-check=full", "--errors-for-leak-kinds=definite"])
            .arg(format!("--log-file={}", dir.join("valgrind.txt").display()))
            .arg(&exe);
        c
    } else {
        Command::new(&exe)
    };
    let out = cmd.current_dir(&dir).output().map_err(|e| e.to_string())?;
    if memcheck && out.status.code() == Some(99) {
        return Err(format!(
            "valgrind found errors:\n{}",
            std::fs::read_to_string(dir.join("valgrind.txt")).unwrap_or_default()
        ));
    }
    let mut actual = String::from_utf8_lossy(&out.stdout).to_string();
    actual.push_str(&markers(&String::from_utf8_lossy(&out.stderr), out.status.code()));
    // Release builds drop contract checks, so a program may have a separate expectation.
    let release_out = src.with_extension("release.out");
    let expected_path = if release && release_out.exists() { release_out } else { src.with_extension("out") };
    let expected = std::fs::read_to_string(expected_path).unwrap_or_default();
    if actual != expected {
        return Err(format!("output differs\n--- expected ---\n{expected}--- actual ---\n{actual}"));
    }
    Ok(())
}

fn run_all(release: bool) {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/run");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(if release { "native-release" } else { "native-debug" });
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "jig"))
        .collect();
    paths.sort();
    let mut failures = Vec::new();
    for p in &paths {
        if let Err(e) = run_one(p, &work, release) {
            failures.push(format!("== {} ==\n{e}", p.display()));
        }
    }
    if !failures.is_empty() {
        panic!("{} of {} native runs failed:\n{}", failures.len(), paths.len(), failures.join("\n"));
    }
}

#[test]
fn native_debug() {
    run_all(false);
}

#[test]
fn native_release() {
    run_all(true);
}
