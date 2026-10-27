//! The self-hosted compiler (`selfhost/`, written in Jig) must agree with the bootstrap
//! compiler. Its lexer and parser run natively over every `.jig` file in the repository and
//! over mutated copies (stray quotes, escapes, Unicode, unbalanced brackets, keywords and
//! operators in odd places, deleted spans); the token and AST dumps must match
//! `jig debug tokens` and `jig debug ast` byte for byte. Its checker runs over every example,
//! UI and run test, the self-hosted compiler itself and mutated copies of them (names, types,
//! literals, operators and lines swapped, dropped or duplicated); every diagnostic and the
//! type of every expression and pattern must match `jig debug check`. Its MIR lowering and C
//! back end run over every example, run test, the agent benchmark's Jig solutions and `jigc`
//! itself; the C must match `jig debug c` byte for byte. And the checked-in bootstrap
//! (`bootstrap/jigc.c`, D6) must be what `jigc` makes of its own sources.

use jig::codegen::build::{build, BuildOpts};
use jig::sema::load::Sources;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

fn have_cc() -> bool {
    let cc = jig::codegen::build::c_compiler(false);
    Command::new(cc).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn jig_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let name = p.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with('.') || name == "target" {
            continue;
        }
        if p.is_dir() {
            jig_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "jig") {
            out.push(p);
        }
    }
}

/// xorshift64*: deterministic mutations.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// `jigc`, built once for every test in this file.
fn jigc() -> &'static Path {
    static EXE: OnceLock<PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost-jigc");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let mut prog = jig::sema::analyze(&Sources::Disk, &repo_root().join("selfhost"));
        assert!(!prog.diags.has_errors(), "selfhost does not check");
        let exe = dir.join("jigc");
        let opts =
            BuildOpts { release: true, build_dir: dir.join("build"), out: exe.clone(), proven: Default::default() };
        build(&mut prog, &opts).expect("build jigc");
        exe
    })
}

#[test]
fn jigc_matches_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let exe = jigc().to_path_buf();

    let mut files = Vec::new();
    for d in ["compiler", "examples", "selfhost", "bench"] {
        jig_files(&root.join(d), &mut files);
    }
    assert!(files.len() > 50);
    let mut cases: Vec<(String, String)> = Vec::new();
    for f in &files {
        cases.push((f.display().to_string(), std::fs::read_to_string(f).unwrap()));
    }
    let pieces = [
        "\"",
        "'",
        "\\",
        "\\u{",
        "}",
        "\\u{1F600}",
        "\\u{D800}",
        "\\q",
        "\n",
        "\n\n",
        "//",
        "///",
        "////",
        "0x",
        "1e",
        "1.5",
        ".",
        "..",
        "&&",
        "||",
        "<<=",
        ">>=",
        "$",
        "é",
        "€",
        "😀",
        ";",
        "(",
        ")",
        "[",
        "]",
        "{",
        "}",
        "12abc",
        "0b101",
        "'ab'",
        "''",
        "\t",
        "\r\n",
        " ",
        "return\n",
        "fn ",
        "fn f() {\n",
        "struct ",
        "enum E {\n",
        "match x {\n",
        " => ",
        "if ",
        "else ",
        "for ",
        " in ",
        "let ",
        "var ",
        "pub ",
        "import ",
        "test \"t\" ",
        "step \"s\" ",
        "requires ",
        "ensures ",
        "effects {",
        "invariant ",
        " where ",
        "type ",
        "const ",
        "unit ",
        "<",
        ">",
        ">>",
        ">=",
        "->",
        "&mut ",
        "&",
        "|",
        "_",
        "?",
        " as ",
        ":",
        ",",
        "=",
        "+=",
        "-",
        "!",
        "Point { x: 1 }",
        "List<Int>.new()",
        "forall i in 0..n: ",
        "x.0",
        ".1",
        "99999999999999999999",
        "0xZZ",
        "Float<m/s^2>",
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for k in 0..400 {
        let (_, base) = &cases[rng.below(files.len())];
        let mut s = base.clone();
        for _ in 0..1 + rng.below(8) {
            let mut i = rng.below(s.len() + 1);
            while !s.is_char_boundary(i) {
                i -= 1;
            }
            if rng.below(4) == 0 {
                // Delete a short span.
                let mut j = (i + 1 + rng.below(24)).min(s.len());
                while !s.is_char_boundary(j) {
                    j += 1;
                }
                s.replace_range(i..j, "");
            } else {
                s.insert_str(i, pieces[rng.below(pieces.len())]);
            }
        }
        cases.push((format!("mutation {k}"), s));
    }
    let mut failures = Vec::new();
    for (i, (name, src)) in cases.iter().enumerate() {
        let path = work.join(format!("case{i}.jig"));
        std::fs::write(&path, src).unwrap();
        for (cmd, expected) in [("tokens", jig::tools::debug::tokens(src)), ("ast", jig::tools::debug_ast::ast(src))] {
            let out = Command::new(&exe).arg(cmd).arg(&path).output().unwrap();
            let actual = String::from_utf8_lossy(&out.stdout);
            if actual != expected {
                let first = expected.lines().zip(actual.lines()).find(|(a, b)| a != b);
                let stderr = String::from_utf8_lossy(&out.stderr);
                failures.push(format!("{cmd} {name} ({}): first difference {first:?} {stderr}", path.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let p = e.unwrap().path();
        let name = p.file_name().unwrap();
        if name == ".jig" {
            continue; // build and proof caches
        }
        if p.is_dir() {
            copy_dir(&p, &to.join(name));
        } else {
            std::fs::copy(&p, to.join(name)).unwrap();
        }
    }
}

fn has_feature_files(dir: &Path) -> bool {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "feature") {
                return true;
            }
        }
    }
    false
}

/// The byte ranges of the identifiers in `s`.
fn idents(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_alphabetic() || b[i] == b'_' {
            let j = (i..b.len()).find(|&j| !(b[j].is_ascii_alphanumeric() || b[j] == b'_')).unwrap_or(b.len());
            out.push((i, j));
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Change the program's meaning without (usually) breaking its syntax.
fn mutate(s: &str, rng: &mut Rng) -> String {
    let ids = idents(s);
    let words = [
        "Int", "Str", "Float", "Bool", "U8", "List", "Option", "Some", "None", "Ok", "Err", "self", "result", "x",
        "len", "push", "str", "main", "io", "true", "pub", "let", "var",
    ];
    let ops = [" + ", " - ", " * ", " / ", " % ", " == ", " != ", " < ", " >= ", " && ", " || ", " | ", " << "];
    let lits = ["0", "1", "-1", "300", "1.5", "\"s\"", "'c'", "true", "()", "[]", "[1, 2]", "(1, \"a\")"];
    let lines: Vec<&str> = s.lines().collect();
    let mut s = s.to_string();
    match rng.below(9) {
        // Another identifier of the file, or a common one.
        0 | 1 if !ids.is_empty() => {
            let (i, j) = ids[rng.below(ids.len())];
            let w = if rng.below(2) == 0 {
                let (a, b) = ids[rng.below(ids.len())];
                s[a..b].to_string()
            } else {
                words[rng.below(words.len())].to_string()
            };
            s.replace_range(i..j, &w);
        }
        // An operator.
        2 => {
            let found: Vec<usize> = ops.iter().flat_map(|o| s.match_indices(o).map(|(i, _)| i)).collect();
            if let Some(&i) = found.get(rng.below(found.len().max(1))) {
                let old = ops.iter().find(|o| s[i..].starts_with(*o)).unwrap();
                s.replace_range(i..i + old.len(), ops[rng.below(ops.len())]);
            }
        }
        // A literal for a number.
        3 => {
            let b = s.as_bytes();
            let nums: Vec<usize> = (0..b.len())
                .filter(|&i| b[i].is_ascii_digit() && (i == 0 || !b[i - 1].is_ascii_alphanumeric()))
                .collect();
            if let Some(&i) = nums.get(rng.below(nums.len().max(1))) {
                let j = (i..b.len()).find(|&j| !b[j].is_ascii_alphanumeric() && b[j] != b'.').unwrap_or(b.len());
                s.replace_range(i..j, lits[rng.below(lits.len())]);
            }
        }
        // Lines: delete, duplicate or swap.
        4..=6 if lines.len() > 2 => {
            let k = rng.below(lines.len() - 1);
            let mut v: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
            match rng.below(3) {
                0 => {
                    v.remove(k);
                }
                1 => v.insert(k, lines[k].to_string()),
                _ => v.swap(k, k + 1),
            }
            s = v.join("\n") + "\n";
        }
        // Keywords that change mutability, visibility and effects.
        _ => {
            let edits = [
                ("let ", "var "),
                ("var ", "let "),
                ("&mut ", ""),
                ("pub ", ""),
                ("return ", ""),
                ("effects {", "effects {io, "),
            ];
            let (from, to) = edits[rng.below(edits.len())];
            let found: Vec<usize> = s.match_indices(from).map(|(i, _)| i).collect();
            if let Some(&i) = found.get(rng.below(found.len().max(1))) {
                s.replace_range(i..i + from.len(), to);
            }
        }
    }
    s
}

/// Compare `jigc check` with `jig debug check` on a package directory.
fn compare_check(exe: &Path, std_dir: &Path, dir: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).unwrap();
    let expected = jig::tools::debug_check::check(&dir);
    let out = Command::new(exe).arg("check").arg(&dir).arg(std_dir).output().unwrap();
    let actual = String::from_utf8_lossy(&out.stdout);
    let mut e: Vec<&str> = expected.lines().collect();
    let mut a: Vec<&str> = actual.lines().collect();
    e.sort();
    a.sort();
    if e == a && out.status.success() {
        return None;
    }
    let missing: Vec<&&str> = e.iter().filter(|l| !a.contains(l)).take(3).collect();
    let extra: Vec<&&str> = a.iter().filter(|l| !e.contains(l)).take(3).collect();
    Some(format!(
        "{}: bootstrap only {missing:?}, jigc only {extra:?} {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    ))
}

#[test]
fn jigc_checker_matches_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = root.join("compiler/std");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost-check");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    // Cases: (project directory, the package to check inside it).
    let mut cases: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut singles: Vec<PathBuf> = Vec::new();
    for d in ["compiler/tests/ui", "compiler/tests/run"] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join(d)).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                let entry = if p.join("app").is_dir() { p.join("app") } else { p.clone() };
                cases.push((p.clone(), entry));
            } else if p.extension().is_some_and(|x| x == "jig") {
                singles.push(p);
            }
        }
    }
    for d in ["examples", "bench/agents/reference/jig"] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join(d)).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                cases.push((p.clone(), p));
            }
        }
    }
    cases.push((root.join("selfhost"), root.join("selfhost")));
    for (i, f) in singles.iter().enumerate() {
        let d = work.join(format!("single{i}"));
        std::fs::create_dir_all(&d).unwrap();
        std::fs::copy(f, d.join("main.jig")).unwrap();
        cases.push((d.clone(), d));
    }
    let originals = cases.len();
    let mut failures: Vec<String> = Vec::new();
    for (_, entry) in &cases {
        failures.extend(compare_check(exe, &std_dir, entry));
    }
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    for k in 0..300 {
        let (proj, entry) = &cases[rng.below(originals)];
        let copy = work.join(format!("mut{k}"));
        copy_dir(proj, &copy);
        let entry = copy.join(entry.strip_prefix(proj).unwrap());
        let mut files = Vec::new();
        jig_files(&copy, &mut files);
        let f = &files[rng.below(files.len())];
        let mut src = std::fs::read_to_string(f).unwrap();
        for _ in 0..1 + rng.below(2) {
            src = mutate(&src, &mut rng);
        }
        std::fs::write(f, src).unwrap();
        failures.extend(compare_check(exe, &std_dir, &entry).map(|m| format!("mutation {k} of {}: {m}", f.display())));
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

/// The C program `jigc c` generates for DIR, compared with `jig debug c`. Both name source
/// files relative to the working directory.
fn compare_c(exe: &Path, std_dir: &Path, dir: &Path) -> Option<String> {
    let dir = std::fs::canonicalize(dir).unwrap();
    let Ok(expected) = jig::tools::debug_check::c(&dir) else { return None };
    let cwd = std::env::current_dir().unwrap();
    let out = Command::new(exe).arg("c").arg(&dir).arg(std_dir).env("PWD", &cwd).output().unwrap();
    let actual = String::from_utf8_lossy(&out.stdout);
    if actual == expected && out.status.success() {
        return None;
    }
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let first = e.iter().zip(&a).position(|(x, y)| x != y).unwrap_or(e.len().min(a.len()));
    Some(format!(
        "{}: line {} differs (bootstrap {:?}, jigc {:?}) {}",
        dir.display(),
        first + 1,
        e.get(first),
        a.get(first),
        String::from_utf8_lossy(&out.stderr)
    ))
}

#[test]
fn jigc_back_end_matches_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = root.join("compiler/std");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost-c");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut cases: Vec<PathBuf> = Vec::new();
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(root.join("compiler/tests/run")).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for (i, p) in entries.iter().enumerate() {
        if p.is_dir() {
            cases.push(if p.join("app").is_dir() { p.join("app") } else { p.clone() });
        } else if p.extension().is_some_and(|x| x == "jig") {
            let d = work.join(format!("single{i}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::copy(p, d.join("main.jig")).unwrap();
            cases.push(d);
        }
    }
    for d in ["examples", "bench/agents/reference/jig"] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join(d)).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        cases.extend(entries.into_iter().filter(|p| p.is_dir()));
    }
    cases.push(root.join("selfhost"));
    let failures: Vec<String> = cases.iter().filter_map(|d| compare_c(exe, &std_dir, d)).collect();
    assert!(failures.is_empty(), "{} programs differ:\n{}", failures.len(), failures.join("\n"));
}

/// `bootstrap/jigc.c` is what `jigc cc selfhost compiler/std` prints, run from the
/// repository root (D6). `bootstrap/update.sh` regenerates it.
#[test]
fn bootstrap_is_current() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = std::fs::canonicalize(repo_root()).unwrap();
    let out = Command::new(jigc())
        .args(["cc", "selfhost", "compiler/std"])
        .current_dir(&root)
        .env("PWD", &root)
        .output()
        .unwrap();
    assert!(out.status.success(), "jigc cc failed: {}", String::from_utf8_lossy(&out.stderr));
    let checked_in = std::fs::read(root.join("bootstrap/jigc.c")).unwrap_or_default();
    assert!(out.stdout == checked_in, "bootstrap/jigc.c is stale: run bootstrap/update.sh");
}

/// A mutation of a `.feature` file: words replaced by other values, keywords swapped, lines
/// inserted, deleted, duplicated or swapped.
fn mutate_feature(s: &str, rng: &mut Rng) -> String {
    let heads = [
        "Given ",
        "When ",
        "Then ",
        "And ",
        "But ",
        "Scenario: ",
        "Scenario Outline: ",
        "Scenario:",
        "Examples:",
        "Feature: ",
        "Background:",
        "Rule: r",
        "@slow",
        "| a | b |",
        "| 1 |",
        "|",
        "\"\"\"",
        "# note",
        "Given",
        "",
    ];
    let vals = [
        "0",
        "-5",
        "+7",
        "1.5",
        "1e3",
        ".5",
        "1.",
        "inf",
        "NaN",
        "true",
        "false",
        "\"q\"",
        "\"a\\\"b\"",
        "\"\\",
        "\"",
        "99999999999999999999",
        "300",
        "-0",
        "1_000",
        "<start>",
        "<nope>",
        "x y",
        "é",
        "{n}",
        "|",
    ];
    let mut v: Vec<String> = s.lines().map(|l| l.to_string()).collect();
    if v.is_empty() {
        v.push(String::new());
    }
    let k = rng.below(v.len());
    match rng.below(7) {
        0 | 1 => {
            let words: Vec<(usize, usize)> = {
                let b = v[k].as_bytes();
                let mut out = Vec::new();
                let mut i = 0;
                while i < b.len() {
                    if b[i] == b' ' {
                        i += 1;
                        continue;
                    }
                    let j = (i..b.len()).find(|&j| b[j] == b' ').unwrap_or(b.len());
                    out.push((i, j));
                    i = j;
                }
                out
            };
            if let Some(&(i, j)) = words.get(rng.below(words.len().max(1))) {
                v[k].replace_range(i..j, vals[rng.below(vals.len())]);
            }
        }
        2 => {
            let line = v[k].clone();
            let indent = line.len() - line.trim_start().len();
            let rest = line.trim_start();
            let word = rest.find(' ').map(|i| i + 1).unwrap_or(rest.len());
            v[k] = format!("{}{}{}", &line[..indent], heads[rng.below(heads.len())], &rest[word..]);
        }
        3 => {
            let words = ["we have 5 items", "a refund is issued", "\"A\" pays 40 to \"B\"", "x"];
            let line = format!("    {}{}", heads[rng.below(heads.len())], words[rng.below(words.len())]);
            v.insert(k, line);
        }
        4 => {
            v.remove(k);
        }
        5 => {
            let l = v[k].clone();
            v.insert(k, l);
        }
        _ if v.len() > 1 => {
            let j = rng.below(v.len() - 1);
            v.swap(j, j + 1);
        }
        _ => {}
    }
    v.join("\n") + if rng.below(4) == 0 { "" } else { "\n" }
}

/// A mutation of a step function's pattern.
fn mutate_step_pattern(s: &str, rng: &mut Rng) -> String {
    let starts: Vec<usize> = s.match_indices("step \"").map(|(i, _)| i + 6).collect();
    let Some(&i) = starts.get(rng.below(starts.len().max(1))) else { return s.to_string() };
    let j = i + s[i..].find('"').unwrap_or(0);
    let edits = ["{n}", "{}", "{n", "{a}{b}", "{x y}", "{count}", "{who}", " items", "", "\\\"bob\\\"", "{n} {n}"];
    let mut pat = s[i..j].to_string();
    let holes: Vec<(usize, usize)> =
        pat.match_indices('{').filter_map(|(a, _)| pat[a..].find('}').map(|b| (a, a + b + 1))).collect();
    let edit = edits[rng.below(edits.len())];
    match (rng.below(2), holes.get(rng.below(holes.len().max(1)))) {
        (0, Some(&(a, b))) => pat.replace_range(a..b, edit),
        _ => {
            let at = rng.below(pat.len() + 1);
            if pat.is_char_boundary(at) {
                pat.insert_str(at, edit);
            }
        }
    }
    format!("{}{}{}", &s[..i], pat, &s[j..])
}

#[test]
fn jigc_features_match_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = root.join("compiler/std");
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost-features");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut cases: Vec<PathBuf> = Vec::new();
    for d in ["compiler/tests/ui", "examples"] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join(d)).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        cases.extend(entries.into_iter().filter(|p| p.is_dir() && has_feature_files(p)));
    }
    assert!(cases.len() >= 3, "feature packages: {cases:?}");
    let mut failures: Vec<String> = Vec::new();
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for k in 0..300 {
        let proj = &cases[rng.below(cases.len())];
        let copy = work.join(format!("mut{k}"));
        copy_dir(proj, &copy);
        let mut features: Vec<PathBuf> = std::fs::read_dir(&copy)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "feature"))
            .collect();
        features.sort();
        let mut steps: Vec<PathBuf> = Vec::new();
        jig_files(&copy, &mut steps);
        steps.retain(|f| std::fs::read_to_string(f).unwrap().contains("step \""));
        for _ in 0..1 + rng.below(3) {
            if rng.below(4) != 0 || steps.is_empty() {
                let f = &features[rng.below(features.len())];
                let src = std::fs::read_to_string(f).unwrap();
                std::fs::write(f, mutate_feature(&src, &mut rng)).unwrap();
            } else {
                let f = &steps[rng.below(steps.len())];
                let src = std::fs::read_to_string(f).unwrap();
                std::fs::write(f, mutate_step_pattern(&src, &mut rng)).unwrap();
            }
        }
        failures.extend(compare_check(exe, &std_dir, &copy).map(|m| format!("mutation {k}: {m}")));
        failures.extend(compare_c(exe, &std_dir, &copy).map(|m| format!("mutation {k} (C): {m}")));
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

fn write_file(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Both compilers' check dumps (sorted) and C programs for DIR, with the module cache under
/// `home`.
fn compare_with_modules(exe: &Path, std_dir: &Path, dir: &Path, home: &Path) -> Option<String> {
    let run = |cmd: &mut Command| {
        let out = cmd.env("JIG_HOME", home).env("PWD", dir).current_dir(dir).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    };
    let jig = env!("CARGO_BIN_EXE_jig");
    let (_, expected) = run(Command::new(jig).args(["debug", "check"]).arg(dir));
    let (_, actual) = run(Command::new(exe).arg("check").arg(dir).arg(std_dir));
    let mut e: Vec<&str> = expected.lines().collect();
    let mut a: Vec<&str> = actual.lines().collect();
    e.sort();
    a.sort();
    if e != a {
        let missing: Vec<&&str> = e.iter().filter(|l| !a.contains(l)).take(3).collect();
        let extra: Vec<&&str> = a.iter().filter(|l| !e.contains(l)).take(3).collect();
        return Some(format!("{}: bootstrap only {missing:?}, jigc only {extra:?}", dir.display()));
    }
    let (ok, expected) = run(Command::new(jig).args(["debug", "c"]).arg(dir));
    if ok {
        let (_, actual) = run(Command::new(exe).arg("c").arg(dir).arg(std_dir));
        if actual != expected {
            return Some(format!("{}: the C programs differ", dir.display()));
        }
    }
    None
}

/// Projects that require modules: missing downloads, versions raised by other modules,
/// `replace`, a module whose `jig.mod` names another, bad `jig.mod` lines, and lints and
/// errors inside modules.
#[test]
fn jigc_modules_match_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = root.join("compiler/std");
    let work = std::fs::canonicalize(PathBuf::from(env!("CARGO_TARGET_TMPDIR"))).unwrap().join("selfhost-modules");
    let _ = std::fs::remove_dir_all(&work);
    let home = work.join("home");
    let cache = home.join("mod");
    let greet = |dir: &Path, word: &str| {
        write_file(&dir.join("jig.mod"), "module example.com/greetlib\n");
        write_file(
            &dir.join("greet/greet.jig"),
            &format!(
                "import \"util\"\n\npub fn hello(name: Str) -> Str {{\n  return \"{word}, \" + util.shout(name)\n}}\n\npub fn unused_api() -> Int {{\n  let x = 1\n  return 2\n}}\n"
            ),
        );
        write_file(&dir.join("util/util.jig"), "pub fn shout(s: Str) -> Str {\n  return s.to_upper() + \"!\"\n}\n");
    };
    greet(&cache.join("example.com/greetlib@v1.0.0"), "hello");
    greet(&cache.join("example.com/greetlib@v1.10.0"), "hi");
    let mid = cache.join("example.com/middle@v0.1.0");
    write_file(&mid.join("jig.mod"), "module example.com/middle\nrequire example.com/greetlib v1.10.0\n");
    write_file(
        &mid.join("m/m.jig"),
        "import \"example.com/greetlib/greet\"\n\npub fn twice(s: Str) -> Str {\n  return greet.hello(s) + \" \" + greet.hello(s)\n}\n",
    );
    let wrong = cache.join("example.com/wrong@v1.0.0");
    write_file(&wrong.join("jig.mod"), "module example.com/other\n");
    write_file(&wrong.join("w/w.jig"), "pub fn w() -> Int {\n  return \"no\"\n}\n");
    let local = work.join("local-greetlib");
    write_file(&local.join("jig.mod"), "module example.com/greetlib\n");
    write_file(&local.join("greet/greet.jig"), "pub fn hello(name: Str) -> Str {\n  return \"local \" + name\n}\n");
    let main_greet = "import \"example.com/greetlib/greet\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(greet.hello(\"jig\"))\n}\n";
    let cases: [(&str, &str, &str); 7] = [
        ("downloaded", "module app\nrequire example.com/greetlib v1.0.0\n", main_greet),
        ("missing", "module app\nrequire example.com/greetlib v2.0.0\n", main_greet),
        (
            "raised",
            "module app\nrequire example.com/greetlib v1.0.0\nrequire example.com/middle v0.1.0\n",
            "import \"example.com/greetlib/greet\"\nimport \"example.com/middle/m\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(greet.hello(\"a\") + \" / \" + m.twice(\"b\"))\n}\n",
        ),
        (
            "replaced",
            "module app\nrequire example.com/greetlib v9.9.9\nreplace example.com/greetlib => ../local-greetlib\n",
            main_greet,
        ),
        (
            "wrong",
            "module app\nrequire example.com/wrong v1.0.0\n",
            "import \"example.com/wrong/w\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(w.w().str())\n}\n",
        ),
        ("bad_lines", "module app\nrequire example.com/greetlib\nlink -x\nreplace a => \nrequire example.com/greetlib v1.0.0\n", main_greet),
        (
            "unknown_package",
            "module app\nrequire example.com/greetlib v1.0.0\n",
            "import \"example.com/greetlib/nope\"\nimport \"std/io\"\n\nfn main() effects {io} {\n  io.println(nope.x())\n}\n",
        ),
    ];
    let mut failures: Vec<String> = Vec::new();
    for (name, jig_mod, main) in cases {
        let app = work.join(name);
        write_file(&app.join("jig.mod"), jig_mod);
        write_file(&app.join("main.jig"), main);
        failures.extend(compare_with_modules(exe, &std_dir, &app, &home).map(|m| format!("{name}: {m}")));
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

/// A mutation that changes a file's layout and little else: indentation stripped or added,
/// lines joined or split where a line break is insignificant, spacing changed, identifiers
/// made long enough to force breaks, comments and blank lines inserted.
fn mutate_layout(s: &str, rng: &mut Rng) -> String {
    let mut lines: Vec<String> = s.lines().map(|l| l.to_string()).collect();
    if lines.is_empty() {
        return s.to_string();
    }
    let k = rng.below(lines.len());
    match rng.below(10) {
        0 => {
            for l in lines.iter_mut() {
                *l = l.trim_start().to_string();
            }
        }
        1 => {
            let n = 1 + rng.below(6);
            lines[k] = format!("{}{}", " ".repeat(n), lines[k]);
        }
        // Join a line ending in an opener or a comma with the next one.
        2 => {
            let ends = |l: &str| {
                let t = l.trim_end();
                (t.ends_with('(') || t.ends_with('[') || t.ends_with(',')) && !t.contains("//")
            };
            let cands: Vec<usize> = (0..lines.len().saturating_sub(1)).filter(|&i| ends(&lines[i])).collect();
            if let Some(&i) = cands.get(rng.below(cands.len().max(1))) {
                let next = lines.remove(i + 1);
                lines[i] = format!("{} {}", lines[i].trim_end(), next.trim_start());
            }
        }
        // Break after an opener or a comma.
        3 => {
            let l = lines[k].clone();
            let cuts: Vec<usize> = l.match_indices(['(', '[', ',']).map(|(i, _)| i + 1).collect();
            if !l.contains("//") && !l.contains('"') {
                if let Some(&c) = cuts.get(rng.below(cuts.len().max(1))) {
                    lines[k] = l[..c].to_string();
                    lines.insert(k + 1, l[c..].to_string());
                }
            }
        }
        // A long name.
        4 => {
            let ids = idents(&lines[k]);
            if let Some(&(i, j)) = ids.get(rng.below(ids.len().max(1))) {
                let w = &lines[k][i..j];
                if !matches!(w, "fn" | "let" | "var" | "if" | "else" | "for" | "in" | "return" | "match") {
                    let long = format!("{w}_{}", "very_long_name_".repeat(1 + rng.below(4)));
                    let w = w.to_string();
                    for l in lines.iter_mut() {
                        let ids = idents(l);
                        for &(a, b) in ids.iter().rev() {
                            if l[a..b] == w {
                                l.replace_range(a..b, &long);
                            }
                        }
                    }
                }
            }
        }
        5 => {
            let c = ["// a note", "/// docs", "// TODO: more", "//"][rng.below(4)];
            let indent = lines[k].len() - lines[k].trim_start().len();
            lines.insert(k, format!("{}{c}", " ".repeat(indent)));
        }
        6 => {
            if !lines[k].contains("//") && !lines[k].contains('"') {
                lines[k].push_str(" // trailing");
            }
        }
        7 => lines.insert(k, String::new()),
        8 => {
            if lines[k].trim().is_empty() {
                lines.remove(k);
            }
        }
        _ => {
            let edits = [(", ", ","), (" = ", "="), ("(", "( "), (" {", "{"), (": ", ":"), (" + ", "+")];
            let (from, to) = edits[rng.below(edits.len())];
            if !lines[k].contains('"') && !lines[k].contains("//") {
                lines[k] = lines[k].replacen(from, to, 1);
            }
        }
    }
    lines.join("\n") + "\n"
}

/// `jigc fmt` writes what `jig fmt` writes: on every `.jig` file in the repository and on 600
/// copies with their layout mutated (or broken: then both refuse).
#[test]
fn jigc_formatter_matches_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let mut files = Vec::new();
    for d in ["compiler", "examples", "selfhost", "bench"] {
        jig_files(&root.join(d), &mut files);
    }
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost-fmt");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let compare = |src: &str, name: &str| -> Option<String> {
        let path = work.join(name);
        std::fs::write(&path, src).unwrap();
        let out = Command::new(exe).arg("fmt").arg(&path).output().unwrap();
        let actual = out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string());
        let expected = jig::fmt::format_source(src).ok();
        if actual == expected {
            return None;
        }
        let (e, a) = (expected.unwrap_or_default(), actual.unwrap_or_default());
        let (el, al): (Vec<&str>, Vec<&str>) = (e.lines().collect(), a.lines().collect());
        let first = el.iter().zip(&al).position(|(x, y)| x != y).unwrap_or(el.len().min(al.len()));
        Some(format!(
            "{name}: line {} differs (bootstrap {:?}, jigc {:?}) {}",
            first + 1,
            el.get(first),
            al.get(first),
            String::from_utf8_lossy(&out.stderr)
        ))
    };
    let mut failures: Vec<String> = Vec::new();
    let sources: Vec<String> = files.iter().map(|f| std::fs::read_to_string(f).unwrap()).collect();
    for (i, src) in sources.iter().enumerate() {
        failures.extend(compare(src, &format!("file{i}.jig")));
    }
    let mut rng = Rng(0xd1b5_4a32_d192_ed03);
    for k in 0..600 {
        let mut src = sources[rng.below(sources.len())].clone();
        for _ in 0..1 + rng.below(4) {
            src = mutate_layout(&src, &mut rng);
        }
        failures.extend(compare(&src, &format!("mut{k}.jig")));
    }
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

/// Run a command, giving up after `secs` seconds: its success and output, or `None`.
fn output_within(cmd: &mut Command, secs: u64) -> Option<(bool, String)> {
    let mut child = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).spawn().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut stdout, &mut s);
        s
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return Some((status.success(), reader.join().unwrap()));
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// `jigc test DIR` against `jig test --json DIR`, both run from the repository root. A package
/// whose tests the bootstrap does not finish quickly (a mutation that loops) is skipped.
fn compare_tests(exe: &Path, std_dir: &Path, dir: &Path) -> Option<String> {
    let root = std::fs::canonicalize(repo_root()).unwrap();
    let jig = env!("CARGO_BIN_EXE_jig");
    let (_, expected) = output_within(Command::new(jig).args(["test", "--json"]).arg(dir).current_dir(&root), 20)?;
    let out =
        output_within(Command::new(exe).arg("test").arg(dir).arg(std_dir).current_dir(&root).env("PWD", &root), 120);
    let actual = match out {
        Some((_, a)) => a,
        None => return Some(format!("{}: jigc test timed out", dir.display())),
    };
    if actual == expected {
        return None;
    }
    let (e, a): (Vec<&str>, Vec<&str>) = (expected.lines().collect(), actual.lines().collect());
    let first = e.iter().zip(&a).position(|(x, y)| x != y).unwrap_or(e.len().min(a.len()));
    Some(format!(
        "{}: line {} differs (bootstrap {:?}, jigc {:?})",
        dir.display(),
        first + 1,
        e.get(first),
        a.get(first)
    ))
}

/// jigc's interpreter and test runner agree with the bootstrap's: `jigc test` prints what
/// `jig test --json` prints (results, failure messages, locations, backtraces and captured
/// output) for every package with tests or scenarios, every run test turned into a test of its
/// `main`, and mutated copies of them (code, `.feature` files and step patterns); and
/// `jigc sim` gives every run test's expected output. C calls included.
#[test]
fn jigc_tests_match_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = std::fs::canonicalize(root.join("compiler/std")).unwrap();
    let work = std::fs::canonicalize(PathBuf::from(env!("CARGO_TARGET_TMPDIR"))).unwrap().join("selfhost-tests");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut failures: Vec<String> = Vec::new();
    let mut cases: Vec<PathBuf> = Vec::new();
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(root.join("compiler/tests/run")).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries.iter().filter(|p| p.extension().is_some_and(|x| x == "jig")) {
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        let src = std::fs::read_to_string(p).unwrap();
        let d = work.join(format!("run-{name}"));
        write_file(&d.join("main.jig"), &src);
        let expected = std::fs::read_to_string(p.with_extension("out")).unwrap_or_default();
        let out = Command::new(exe).arg("sim").arg(&d).arg(&std_dir).output().unwrap();
        if String::from_utf8_lossy(&out.stdout) != expected {
            failures.push(format!("jigc sim {name}: {:?}", String::from_utf8_lossy(&out.stdout)));
        }
        write_file(&d.join("main.jig"), &format!("{src}\ntest \"main runs\" {{\n  main()\n}}\n"));
        cases.push(d);
    }
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(root.join("examples")).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    cases.extend(entries.into_iter().filter(|p| p.is_dir()));
    cases.push(root.join("compiler/tests/props"));
    cases.push(root.join("selfhost"));
    let originals = cases.len();
    for d in &cases {
        failures.extend(compare_tests(exe, &std_dir, d));
    }
    // Mutations that still check: tests that fail in new places and ways.
    let mut rng = Rng(0x5851_f42d_4c95_7f2d);
    let mut compared = 0;
    for k in 0..600 {
        if compared == 150 {
            break;
        }
        let proj = &cases[rng.below(originals - 1)];
        let copy = work.join(format!("mut{k}"));
        copy_dir(proj, &copy);
        let mut files = Vec::new();
        jig_files(&copy, &mut files);
        let features: Vec<PathBuf> = std::fs::read_dir(&copy)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|x| x == "feature"))
            .collect();
        for _ in 0..1 + rng.below(2) {
            if !features.is_empty() && rng.below(2) == 0 {
                let f = &features[rng.below(features.len())];
                let src = std::fs::read_to_string(f).unwrap();
                std::fs::write(f, mutate_feature(&src, &mut rng)).unwrap();
            } else {
                let f = &files[rng.below(files.len())];
                let src = std::fs::read_to_string(f).unwrap();
                std::fs::write(f, mutate(&src, &mut rng)).unwrap();
            }
        }
        if jig::sema::analyze(&Sources::Disk, &copy).diags.has_errors() {
            continue;
        }
        compared += 1;
        failures.extend(compare_tests(exe, &std_dir, &copy).map(|m| format!("mutation {k}: {m}")));
    }
    assert!(compared >= 80, "only {compared} mutations checked");
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

/// `jigc vc DIR` against `jig debug vc DIR` (with `std`, the standard library's functions).
fn compare_vc(exe: &Path, std_dir: &Path, dir: &Path, std: bool) -> Option<String> {
    let dir = std::fs::canonicalize(dir).unwrap();
    let expected = jig::tools::debug_check::vc(&dir, std).ok();
    let mut cmd = Command::new(exe);
    cmd.arg("vc").arg(&dir).arg(std_dir);
    if std {
        cmd.arg("std");
    }
    let out = cmd.output().unwrap();
    let actual = out.status.success().then(|| String::from_utf8_lossy(&out.stdout).to_string());
    if actual == expected {
        return None;
    }
    let (e, a) = (expected.unwrap_or_default(), actual.unwrap_or_default());
    let (el, al): (Vec<&str>, Vec<&str>) = (e.lines().collect(), a.lines().collect());
    let first = el.iter().zip(&al).position(|(x, y)| x != y).unwrap_or(el.len().min(al.len()));
    Some(format!(
        "{}: line {} differs (bootstrap {:?}, jigc {:?}) {}",
        dir.display(),
        first + 1,
        el.get(first),
        al.get(first),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// jigc's prover generates the bootstrap's verification conditions byte for byte: every
/// declaration, obligation, path fact and goal, the counterexample terms and the replayable
/// inputs, for every example, run test, prover test, agent benchmark solution and the standard
/// library, and for mutated copies that still check.
#[test]
fn jigc_vcs_match_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = std::fs::canonicalize(root.join("compiler/std")).unwrap();
    let work = std::fs::canonicalize(PathBuf::from(env!("CARGO_TARGET_TMPDIR"))).unwrap().join("selfhost-vc");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut cases: Vec<PathBuf> = Vec::new();
    let mut entries: Vec<PathBuf> =
        std::fs::read_dir(root.join("compiler/tests/run")).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for p in entries.iter().filter(|p| p.extension().is_some_and(|x| x == "jig")) {
        let d = work.join(format!("run-{}", p.file_stem().unwrap().to_string_lossy()));
        write_file(&d.join("main.jig"), &std::fs::read_to_string(p).unwrap());
        cases.push(d);
    }
    for dir in ["examples", "compiler/tests/prove", "bench/agents/reference/jig"] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join(dir)).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        cases.extend(entries.into_iter().filter(|p| p.is_dir()));
    }
    let mut failures: Vec<String> = Vec::new();
    for d in &cases {
        failures.extend(compare_vc(exe, &std_dir, d, false));
    }
    failures.extend(compare_vc(exe, &std_dir, &work.join("run-std_all"), true));
    let originals = cases.len();
    let mut rng = Rng(0x6a09_e667_f3bc_c908);
    let mut compared = 0;
    for k in 0..800 {
        if compared == 200 {
            break;
        }
        let proj = &cases[rng.below(originals)];
        let copy = work.join(format!("mut{k}"));
        copy_dir(proj, &copy);
        let mut files = Vec::new();
        jig_files(&copy, &mut files);
        for _ in 0..1 + rng.below(2) {
            let f = &files[rng.below(files.len())];
            let src = std::fs::read_to_string(f).unwrap();
            std::fs::write(f, mutate(&src, &mut rng)).unwrap();
        }
        if jig::sema::analyze(&Sources::Disk, &copy).diags.has_errors() {
            continue;
        }
        compared += 1;
        failures.extend(compare_vc(exe, &std_dir, &copy, false).map(|m| format!("mutation {k}: {m}")));
    }
    assert!(compared >= 100, "only {compared} mutations checked");
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}

/// `jigc prove DIR --json` against `jig prove --json --no-cache DIR` (timings left out): every
/// obligation's status, counterexample, replay and hint, and the summary.
fn compare_prove(exe: &Path, std_dir: &Path, dir: &Path) -> Option<String> {
    let root = std::fs::canonicalize(repo_root()).unwrap();
    let jig = env!("CARGO_BIN_EXE_jig");
    let strip = |s: String| -> String {
        s.lines()
            .map(|l| match l.find(",\"millis\":") {
                Some(i) => format!("{}}}", &l[..i]),
                None => l.to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let (_, expected) =
        output_within(Command::new(jig).args(["prove", "--json", "--no-cache"]).arg(dir).current_dir(&root), 120)?;
    let out = output_within(
        Command::new(exe).arg("prove").arg(dir).arg(std_dir).arg("--json").current_dir(&root).env("PWD", &root),
        120,
    );
    let Some((_, actual)) = out else { return Some(format!("{}: jigc prove timed out", dir.display())) };
    let (e, a) = (strip(expected), strip(actual));
    if e == a {
        return None;
    }
    let (el, al): (Vec<&str>, Vec<&str>) = (e.lines().collect(), a.lines().collect());
    let first = el.iter().zip(&al).position(|(x, y)| x != y).unwrap_or(el.len().min(al.len()));
    Some(format!(
        "{}: line {} differs (bootstrap {:?}, jigc {:?})",
        dir.display(),
        first + 1,
        el.get(first),
        al.get(first)
    ))
}

/// jigc proves what the bootstrap proves: the same obligations come back proved, failed (with
/// the same counterexamples, and confirmed or not by replay on the interpreter) or unknown, for
/// the prover tests, examples, agent solutions and mutated copies of them that still check.
#[test]
fn jigc_proofs_match_bootstrap() {
    if !have_cc() || jig::verify::smt::solver_version().is_none() {
        eprintln!("skipping: no C compiler or no SMT solver");
        return;
    }
    let root = repo_root();
    let exe = jigc();
    let std_dir = std::fs::canonicalize(root.join("compiler/std")).unwrap();
    let work = std::fs::canonicalize(PathBuf::from(env!("CARGO_TARGET_TMPDIR"))).unwrap().join("selfhost-prove");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut cases: Vec<PathBuf> = Vec::new();
    for dir in ["compiler/tests/prove", "examples", "bench/agents/reference/jig"] {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(root.join(dir)).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        cases.extend(entries.into_iter().filter(|p| p.is_dir()));
    }
    let mut failures: Vec<String> = Vec::new();
    for d in &cases {
        failures.extend(compare_prove(exe, &std_dir, d));
    }
    let originals = cases.len();
    let mut rng = Rng(0xbb67_ae85_84ca_a73b);
    let mut compared = 0;
    for k in 0..400 {
        if compared == 60 {
            break;
        }
        let proj = &cases[rng.below(originals)];
        let copy = work.join(format!("mut{k}"));
        copy_dir(proj, &copy);
        let mut files = Vec::new();
        jig_files(&copy, &mut files);
        let f = &files[rng.below(files.len())];
        let src = std::fs::read_to_string(f).unwrap();
        std::fs::write(f, mutate(&src, &mut rng)).unwrap();
        if jig::sema::analyze(&Sources::Disk, &copy).diags.has_errors() {
            continue;
        }
        compared += 1;
        failures.extend(compare_prove(exe, &std_dir, &copy).map(|m| format!("mutation {k}: {m}")));
    }
    assert!(compared >= 30, "only {compared} mutations checked");
    assert!(failures.is_empty(), "{} cases differ:\n{}", failures.len(), failures.join("\n"));
}
