//! The self-hosted compiler (`selfhost/`, written in Jig) must agree with the bootstrap
//! compiler. Its lexer runs natively over every `.jig` file in the repository and over
//! mutated copies (stray quotes, escapes, Unicode, unbalanced brackets); the token dumps must
//! match `jig debug tokens` byte for byte.

use jig::codegen::build::{build, BuildOpts};
use jig::sema::load::Sources;
use std::path::{Path, PathBuf};
use std::process::Command;

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

#[test]
fn lexer_matches_bootstrap() {
    if !have_cc() {
        eprintln!("skipping: no C compiler");
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let work = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("selfhost");
    let _ = std::fs::remove_dir_all(&work);
    std::fs::create_dir_all(&work).unwrap();
    let mut prog = jig::sema::analyze(&Sources::Disk, &root.join("selfhost"));
    assert!(!prog.diags.has_errors(), "selfhost does not check");
    let exe = work.join("jigc");
    let opts = BuildOpts { release: true, build_dir: work.join("build"), out: exe.clone(), proven: Default::default() };
    build(&mut prog, &opts).expect("build jigc");

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
    ];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
    for k in 0..300 {
        let (_, base) = &cases[rng.below(files.len())];
        let mut s = base.clone();
        for _ in 0..1 + rng.below(8) {
            let mut i = rng.below(s.len() + 1);
            while !s.is_char_boundary(i) {
                i -= 1;
            }
            s.insert_str(i, pieces[rng.below(pieces.len())]);
        }
        cases.push((format!("mutation {k}"), s));
    }
    let mut failures = Vec::new();
    for (i, (name, src)) in cases.iter().enumerate() {
        let path = work.join(format!("case{i}.jig"));
        std::fs::write(&path, src).unwrap();
        let expected = jig::tools::debug::tokens(src);
        let out = Command::new(&exe).arg("tokens").arg(&path).output().unwrap();
        let actual = String::from_utf8_lossy(&out.stdout);
        if actual != expected {
            let first = expected.lines().zip(actual.lines()).find(|(a, b)| a != b);
            failures.push(format!("{name} ({}): first difference {first:?}", path.display()));
        }
    }
    assert!(failures.is_empty(), "{} of {} cases differ:\n{}", failures.len(), cases.len(), failures.join("\n"));
}
