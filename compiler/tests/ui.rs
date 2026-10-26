//! UI tests: each `tests/ui/*.jig` file (or directory with `jig.mod`) is checked, and the
//! errors must match the `//~ E0xxx` annotations exactly (same line, same codes).

use jig::sema::load::Sources;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn expected_in(src: &str) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    for (i, line) in src.lines().enumerate() {
        if let Some(pos) = line.find("//~") {
            for code in line[pos + 3..].split_whitespace() {
                if code.starts_with('E') {
                    out.push((i as u32 + 1, code.to_string()));
                }
            }
        }
    }
    out
}

fn run(dir: &Path, sources: &Sources, files: &[(PathBuf, String)]) -> Result<(), String> {
    let prog = jig::sema::analyze(sources, dir);
    let mut expected: BTreeMap<(String, u32, String), usize> = BTreeMap::new();
    for (p, src) in files {
        for (line, code) in expected_in(src) {
            *expected.entry((p.display().to_string(), line, code)).or_default() += 1;
        }
    }
    let mut actual: BTreeMap<(String, u32, String), usize> = BTreeMap::new();
    let mut rendered = String::new();
    for d in prog.diags.list.iter().filter(|d| d.is_error()) {
        let Some((fid, lc)) = prog.sm.line_col(d.span.lo) else {
            rendered.push_str(&d.render(&prog.sm, false));
            *actual.entry(("<nowhere>".into(), 0, d.code.name())).or_default() += 1;
            continue;
        };
        let path = prog.sm.file(fid).path.display().to_string();
        rendered.push_str(&d.render(&prog.sm, false));
        *actual.entry((path, lc.line, d.code.name())).or_default() += 1;
    }
    if expected == actual {
        return Ok(());
    }
    let mut msg = String::new();
    for (k, n) in &expected {
        if actual.get(k) != Some(n) {
            msg.push_str(&format!("  missing: {}:{} {} (x{n})\n", k.0, k.1, k.2));
        }
    }
    for (k, n) in &actual {
        if expected.get(k) != Some(n) {
            msg.push_str(&format!("  unexpected: {}:{} {} (x{n})\n", k.0, k.1, k.2));
        }
    }
    Err(format!("{msg}\n--- diagnostics ---\n{rendered}"))
}

#[test]
fn ui() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/ui");
    let filter = std::env::var("UI_FILTER").ok();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&root).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    let mut failures = Vec::new();
    let mut count = 0;
    for path in entries {
        let name = path.file_stem().unwrap().to_string_lossy().to_string();
        if filter.as_ref().is_some_and(|f| !name.contains(f.as_str())) {
            continue;
        }
        count += 1;
        let result = if path.is_dir() {
            let mut files = Vec::new();
            collect_jig(&path, &mut files);
            let start = if path.join("app").is_dir() { path.join("app") } else { path.clone() };
            run(&start, &Sources::Disk, &files)
        } else if path.extension().is_some_and(|e| e == "jig") {
            let src = std::fs::read_to_string(&path).unwrap();
            let dir = PathBuf::from(format!("/ui/{name}"));
            let file = dir.join("main.jig");
            let mut m = jig::util::FxHashMap::default();
            m.insert(file.clone(), src.clone());
            run(&dir, &Sources::Memory(m), &[(file, src)])
        } else {
            continue;
        };
        if let Err(e) = result {
            failures.push(format!("== {name} ==\n{e}"));
        }
    }
    assert!(count > 0, "no ui tests found");
    if !failures.is_empty() {
        panic!("{} of {count} ui tests failed:\n\n{}", failures.len(), failures.join("\n"));
    }
}

fn collect_jig(dir: &Path, out: &mut Vec<(PathBuf, String)>) {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            collect_jig(&p, out);
        } else if p.extension().is_some_and(|e| e == "jig") {
            let canon = std::fs::canonicalize(&p).unwrap();
            out.push((canon, std::fs::read_to_string(&p).unwrap()));
        }
    }
}
