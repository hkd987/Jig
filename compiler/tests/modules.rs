//! Modules: `jig get` downloads required versions (git tags) from local repositories standing
//! in for remote ones, imports resolve through them, the highest required version wins,
//! `replace` points a module at a directory, and `jig.sum` catches a tag that moved.
//! Skipped when `git` is not installed.

use std::path::{Path, PathBuf};
use std::process::Command;

fn have_git() -> bool {
    Command::new("git").arg("--version").output().is_ok_and(|o| o.status.success())
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.email=t@t", "-c", "user.name=t"])
        .args(args)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok, "git {args:?} in {}", dir.display());
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Run the compiler's command line with the module cache and git base of this test.
fn jig(work: &Path, args: &[&str]) -> (bool, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_jig"))
        .args(args)
        .env("JIG_HOME", work.join("home"))
        .env("JIG_GIT_BASE", format!("file://{}/remotes/", work.display()))
        .output()
        .unwrap();
    (out.status.success(), format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)))
}

#[test]
fn modules_resolve_download_and_verify() {
    if !have_git() {
        eprintln!("skipping: no git");
        return;
    }
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("modules");
    let _ = std::fs::remove_dir_all(&work);
    // A library with two packages; its own imports are relative to its root.
    let lib = work.join("remotes/example.com/greetlib");
    write(&lib.join("jig.mod"), "module example.com/greetlib\n");
    write(
        &lib.join("greet/greet.jig"),
        "import \"util\"\n\npub fn hello(name: Str) -> Str {\n  return \"hello, \" + util.shout(name)\n}\n\npub fn unused_api() -> Int {\n  let x = 1\n  return 2\n}\n",
    );
    write(&lib.join("util/util.jig"), "pub fn shout(s: Str) -> Str {\n  return s.to_upper() + \"!\"\n}\n");
    git(&lib, &["init", "-q"]);
    git(&lib, &["add", "-A"]);
    git(&lib, &["commit", "-qm", "v1"]);
    git(&lib, &["tag", "v1.0.0"]);
    std::fs::write(
        lib.join("greet/greet.jig"),
        std::fs::read_to_string(lib.join("greet/greet.jig")).unwrap().replace("hello, ", "hi, "),
    )
    .unwrap();
    git(&lib, &["commit", "-qam", "v1.1"]);
    git(&lib, &["tag", "v1.1.0"]);
    // A second module that needs the newer version.
    let mid = work.join("remotes/example.com/middle");
    write(&mid.join("jig.mod"), "module example.com/middle\nrequire example.com/greetlib v1.1.0\n");
    write(
        &mid.join("m/m.jig"),
        "import \"example.com/greetlib/greet\"\n\npub fn twice(s: Str) -> Str {\n  return greet.hello(s) + \" \" + greet.hello(s)\n}\n",
    );
    git(&mid, &["init", "-q"]);
    git(&mid, &["add", "-A"]);
    git(&mid, &["commit", "-qm", "m"]);
    git(&mid, &["tag", "v0.1.0"]);

    let app = work.join("app");
    write(&app.join("jig.mod"), "module app\nrequire example.com/greetlib v1.0.0\n");
    write(
        &app.join("main.jig"),
        "import \"example.com/greetlib/greet\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(greet.hello(\"jig\"))\n}\n",
    );
    let a = app.to_str().unwrap();
    let (ok, out) = jig(&work, &["check", a]);
    assert!(!ok && out.contains("E0211") && out.contains("jig get"), "{out}");
    let (ok, out) = jig(&work, &["get", a]);
    assert!(ok && out.contains("downloaded example.com/greetlib v1.0.0"), "{out}");
    let sum = std::fs::read_to_string(app.join("jig.sum")).unwrap();
    assert!(sum.starts_with("example.com/greetlib v1.0.0 "), "{sum}");
    // The dependency's unused export and variable are its own business.
    let (ok, out) = jig(&work, &["check", a]);
    assert!(ok, "{out}");
    let (ok, out) = jig(&work, &["run", a]);
    assert!(ok && out == "hello, JIG!\n", "{out}");

    // Requiring a module that needs v1.1.0 raises the version for everyone.
    write(&app.join("jig.mod"), "module app\nrequire example.com/greetlib v1.0.0\nrequire example.com/middle v0.1.0\n");
    write(
        &app.join("main.jig"),
        "import \"example.com/greetlib/greet\"\nimport \"example.com/middle/m\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(greet.hello(\"a\") + \" / \" + m.twice(\"b\"))\n}\n",
    );
    let (ok, out) = jig(&work, &["get", a]);
    assert!(ok && out.contains("example.com/middle v0.1.0") && out.contains("example.com/greetlib v1.1.0"), "{out}");
    let (ok, out) = jig(&work, &["run", a]);
    assert!(ok && out == "hi, A! / hi, B! hi, B!\n", "{out}");

    // A recorded commit that does not match is refused.
    let sum = std::fs::read_to_string(app.join("jig.sum")).unwrap();
    let tampered: String = sum
        .lines()
        .map(|l| {
            if l.starts_with("example.com/greetlib") {
                "example.com/greetlib v1.1.0 0000000000000000000000000000000000000000".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(app.join("jig.sum"), tampered).unwrap();
    let (ok, out) = jig(&work, &["get", a]);
    assert!(!ok && out.contains("jig.sum recorded"), "{out}");
    std::fs::write(app.join("jig.sum"), sum).unwrap();

    // `replace` uses a local directory instead of a download.
    let local = work.join("local-greetlib");
    write(&local.join("jig.mod"), "module example.com/greetlib\n");
    write(&local.join("greet/greet.jig"), "pub fn hello(name: Str) -> Str {\n  return \"local \" + name\n}\n");
    write(
        &app.join("jig.mod"),
        "module app\nrequire example.com/greetlib v9.9.9\nreplace example.com/greetlib => ../local-greetlib\n",
    );
    write(
        &app.join("main.jig"),
        "import \"example.com/greetlib/greet\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(greet.hello(\"x\"))\n}\n",
    );
    let (ok, out) = jig(&work, &["run", a]);
    assert!(ok && out == "local x\n", "{out}");
}
