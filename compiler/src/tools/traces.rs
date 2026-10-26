//! Opt-in trace collection, the PRD's corpus for teaching models Jig. With the user's consent
//! (`jig traces on`), the edit hook records every edit of a `.jig` file (its new text and the
//! errors `jig check` then reported), and the commit hook records the project's files whenever
//! a commit passes its checks and tests. Nothing leaves the machine: traces stay in
//! `~/.jig/traces` (or `$JIG_HOME/traces`) until the user runs `jig traces export FILE` and
//! shares that file. Projects are named by a hash of their path, not the path.

use crate::util::json::Json;
use std::io::Write;
use std::path::{Path, PathBuf};

const CONSENT: &str = "CONSENT";

/// Where traces are kept.
pub fn dir() -> PathBuf {
    match std::env::var_os("JIG_HOME") {
        Some(h) => PathBuf::from(h).join("traces"),
        None => std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join(".jig/traces"),
    }
}

pub fn enabled_in(dir: &Path) -> bool {
    dir.join(CONSENT).exists()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// A stable name for a project that does not reveal its path.
fn project_id(root: &Path) -> String {
    let s = root.to_string_lossy();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn append(dir: &Path, root: &Path, event: Json) {
    let id = project_id(root);
    let event = event.with("project", id.as_str()).with("time", now_ms());
    let _ = std::fs::create_dir_all(dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join(format!("{id}.jsonl"))) {
        let _ = writeln!(f, "{}", event.to_string_compact());
    }
}

fn rel(root: &Path, file: &Path) -> String {
    file.strip_prefix(root).unwrap_or(file).to_string_lossy().replace('\\', "/")
}

/// An edit: the file's text after it and the errors the check reported (code and message).
pub fn record_edit(dir: &Path, root: &Path, file: &Path, errors: &[(String, String)]) {
    if !enabled_in(dir) {
        return;
    }
    let text = std::fs::read_to_string(file).unwrap_or_default();
    let errs: Vec<Json> =
        errors.iter().map(|(c, m)| Json::obj().with("code", c.as_str()).with("message", m.as_str())).collect();
    append(
        dir,
        root,
        Json::obj()
            .with("kind", "edit")
            .with("file", rel(root, file))
            .with("text", text)
            .with("errors", Json::Arr(errs)),
    );
}

/// A verified state: every `.jig` and `.feature` file of a project that checks and tests
/// cleanly.
pub fn record_verified(dir: &Path, root: &Path, files: &[PathBuf]) {
    if !enabled_in(dir) {
        return;
    }
    let fs: Vec<Json> = files
        .iter()
        .filter_map(|f| {
            let text = std::fs::read_to_string(f).ok()?;
            Some(Json::obj().with("path", rel(root, f)).with("text", text))
        })
        .collect();
    append(dir, root, Json::obj().with("kind", "verified").with("files", Json::Arr(fs)));
}

/// `jig traces on|off|status|export FILE`.
pub fn command(args: &[String]) -> i32 {
    let d = dir();
    match args.iter().map(|s| s.as_str()).collect::<Vec<_>>().as_slice() {
        ["on"] => {
            let _ = std::fs::create_dir_all(&d);
            let note = "Jig records your edits and verified programs in this directory, for a corpus to\n\
                        train models on Jig. Nothing is sent anywhere; share an export only if you choose\n\
                        to. Remove this file (`jig traces off`) to stop.\n";
            match std::fs::write(d.join(CONSENT), note) {
                Ok(()) => {
                    println!("trace collection is on; traces are kept in {}", d.display());
                    0
                }
                Err(e) => {
                    eprintln!("error: cannot write {}: {e}", d.join(CONSENT).display());
                    1
                }
            }
        }
        ["off"] => {
            let _ = std::fs::remove_file(d.join(CONSENT));
            println!("trace collection is off (recorded traces stay in {})", d.display());
            0
        }
        [] | ["status"] => {
            let n = std::fs::read_dir(&d)
                .map(|rd| {
                    rd.filter_map(|e| e.ok()).filter(|e| e.path().extension().is_some_and(|x| x == "jsonl")).count()
                })
                .unwrap_or(0);
            println!(
                "trace collection is {}; {n} project(s) recorded in {}",
                if enabled_in(&d) { "on" } else { "off" },
                d.display()
            );
            0
        }
        ["export", out] => match export(&d, Path::new(out)) {
            Ok(n) => {
                println!("wrote {n} events to {out}");
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        },
        _ => {
            eprintln!("usage: jig traces on|off|status|export FILE");
            2
        }
    }
}

/// Every recorded event, one JSON object per line.
pub fn export(dir: &Path, out: &Path) -> Result<usize, String> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| format!("no traces in {}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    let mut all = String::new();
    for f in files {
        all.push_str(&std::fs::read_to_string(&f).map_err(|e| e.to_string())?);
    }
    std::fs::write(out, &all).map_err(|e| format!("cannot write {}: {e}", out.display()))?;
    Ok(all.lines().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_only_with_consent() {
        let base = std::env::temp_dir().join(format!("jig-traces-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let root = base.join("proj");
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("main.jig");
        std::fs::write(&file, "fn main() {\n}\n").unwrap();
        let d = base.join("traces");
        record_edit(&d, &root, &file, &[]);
        assert!(!d.exists(), "recorded without consent");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(CONSENT), "yes").unwrap();
        record_edit(&d, &root, &file, &[("E0201".into(), "cannot find `x`".into())]);
        record_verified(&d, &root, &[file]);
        let out = base.join("export.jsonl");
        assert_eq!(export(&d, &out).unwrap(), 2);
        let text = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<Json> = text.lines().map(|l| Json::parse(l).unwrap()).collect();
        assert_eq!(lines[0].get("kind").and_then(|k| k.as_str()), Some("edit"));
        assert_eq!(lines[0].get("file").and_then(|k| k.as_str()), Some("main.jig"));
        assert_eq!(lines[1].get("kind").and_then(|k| k.as_str()), Some("verified"));
        assert!(!text.contains(&*root.to_string_lossy()), "the project's path leaked");
        let _ = std::fs::remove_dir_all(&base);
    }
}
