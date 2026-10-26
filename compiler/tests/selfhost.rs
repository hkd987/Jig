//! The self-hosted compiler (`selfhost/`, written in Jig) must agree with the bootstrap
//! compiler. Its lexer and parser run natively over every `.jig` file in the repository and
//! over mutated copies (stray quotes, escapes, Unicode, unbalanced brackets, keywords and
//! operators in odd places, deleted spans); the token and AST dumps must match
//! `jig debug tokens` and `jig debug ast` byte for byte. Its checker runs over every example,
//! UI and run test, the self-hosted compiler itself and mutated copies of them (names, types,
//! literals, operators and lines swapped, dropped or duplicated); every diagnostic and the
//! type of every expression and pattern must match `jig debug check`.

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
            if p.is_dir() && !has_feature_files(&p) {
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
            if p.is_dir() && !has_feature_files(&p) {
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
