//! Documentation stays true: cookbook snippets type-check, and the plugin ships the current
//! reference.

use jig::sema::load::Sources;
use std::path::{Path, PathBuf};

fn code_blocks(md: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    for line in md.lines() {
        if line.trim_start().starts_with("```") {
            match cur.take() {
                Some(b) => out.push(b),
                None => cur = Some(String::new()),
            }
            continue;
        }
        if let Some(b) = cur.as_mut() {
            b.push_str(line);
            b.push('\n');
        }
    }
    out
}

#[test]
fn cookbook_snippets_check() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let md = std::fs::read_to_string(root.join("plugin/skills/jig/cookbook.md")).unwrap();
    let blocks = code_blocks(&md);
    assert!(blocks.len() > 8);
    // Imports first, then the rest.
    let mut imports = std::collections::BTreeSet::new();
    let mut body = String::new();
    for b in &blocks {
        for line in b.lines() {
            if line.starts_with("import ") {
                imports.insert(line.to_string());
            } else {
                body.push_str(line);
                body.push('\n');
            }
        }
        body.push('\n');
    }
    let src = imports.into_iter().collect::<Vec<_>>().join("\n") + "\n\n" + &body;
    let dir = PathBuf::from("/cookbook");
    let mut m = jig::util::FxHashMap::default();
    m.insert(dir.join("main.jig"), src);
    let prog = jig::sema::analyze(&Sources::Memory(m), &dir);
    // Snippets are fragments, so dead code is expected; nothing else is.
    let errs: Vec<String> = prog
        .diags
        .list
        .iter()
        .filter(|d| d.is_error() && !d.code.name().starts_with("E06"))
        .map(|d| d.render(&prog.sm, false))
        .collect();
    assert!(errs.is_empty(), "cookbook snippets have errors:\n{}", errs.join("\n"));
}

#[test]
fn plugin_reference_is_current() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let a = std::fs::read_to_string(root.join("docs/reference.md")).unwrap();
    let b = std::fs::read_to_string(root.join("plugin/skills/jig/reference.md")).unwrap();
    assert!(a == b, "plugin/skills/jig/reference.md is stale: copy docs/reference.md over it");
}
