//! Checking a whole project: every program package (one with `main`) is analyzed as a root,
//! then every library package no program imports. Dead-code findings in a shared package
//! are kept only if every analysis that loaded the package agrees.

use crate::sema::defs::Program;
use crate::sema::load::{find_project_root, Sources};
use crate::util::json::Json;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub struct ProjectDiag {
    pub file: String,
    pub line: u32,
    pub col: u32,
    pub code: String,
    pub is_error: bool,
    pub json: Json,
    pub text: String,
}

pub struct ProjectReport {
    pub diags: Vec<ProjectDiag>,
    pub packages: Vec<PathBuf>,
    pub programs: Vec<PathBuf>,
}

impl ProjectReport {
    pub fn errors(&self) -> usize {
        self.diags.iter().filter(|d| d.is_error).count()
    }
}

/// Directories under `root` that contain `.jig` files (hidden directories and `target`
/// skipped).
pub fn package_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(root, &mut out);
    out.sort();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut has_jig = false;
    let mut subdirs = Vec::new();
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if p.is_dir() {
            if !name.starts_with('.') && name != "target" && name != "node_modules" {
                subdirs.push(p);
            }
        } else if p.extension().is_some_and(|x| x == "jig") {
            has_jig = true;
        }
    }
    if has_jig {
        out.push(dir.to_path_buf());
    }
    subdirs.sort();
    for s in subdirs {
        walk(&s, out);
    }
}

fn has_main(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else { return false };
    rd.flatten().any(|e| {
        let p = e.path();
        p.extension().is_some_and(|x| x == "jig")
            && std::fs::read_to_string(&p).is_ok_and(|s| s.lines().any(|l| l.trim_start().starts_with("fn main(")))
    })
}

fn collect(prog: &Program, out: &mut Vec<(ProjectDiag, Option<PathBuf>)>) {
    for d in &prog.diags.list {
        let (file, line, col, pkg_dir) = match prog.sm.line_col(d.span.lo) {
            Some((fid, lc)) => {
                let path = prog.sm.file(fid).path.clone();
                let dir = path.parent().map(|p| p.to_path_buf());
                (crate::diag::display_path(&path), lc.line, lc.col, dir)
            }
            None => (String::new(), 0, 0, None),
        };
        out.push((
            ProjectDiag {
                file,
                line,
                col,
                code: d.code.name(),
                is_error: d.is_error(),
                json: d.to_json(&prog.sm),
                text: d.render(&prog.sm, false),
            },
            pkg_dir,
        ));
    }
}

fn loaded_dirs(prog: &Program) -> BTreeSet<PathBuf> {
    prog.pkgs.iter().filter_map(|p| p.dir.clone()).collect()
}

/// Check the project containing `start` (or just the package at `start` if there is no
/// `jig.mod`).
pub fn check_project(start: &Path) -> ProjectReport {
    let start = std::fs::canonicalize(start).unwrap_or_else(|_| start.to_path_buf());
    let start_dir = if start.is_file() { start.parent().map(|p| p.to_path_buf()).unwrap_or_default() } else { start.clone() };
    let Some(root) = find_project_root(&Sources::Disk, &start_dir) else {
        let prog = crate::sema::analyze(&Sources::Disk, &start_dir);
        let mut v = Vec::new();
        collect(&prog, &mut v);
        return ProjectReport { diags: v.into_iter().map(|x| x.0).collect(), packages: vec![start_dir.clone()], programs: vec![start_dir] };
    };
    let dirs = package_dirs(&root);
    let programs: Vec<PathBuf> = dirs.iter().filter(|d| has_main(d)).cloned().collect();
    let mut analyses: Vec<(PathBuf, BTreeSet<PathBuf>, Vec<(ProjectDiag, Option<PathBuf>)>)> = Vec::new();
    let mut covered: BTreeSet<PathBuf> = BTreeSet::new();
    for p in &programs {
        let prog = crate::sema::analyze(&Sources::Disk, p);
        let loaded = loaded_dirs(&prog);
        covered.extend(loaded.iter().cloned());
        let mut v = Vec::new();
        collect(&prog, &mut v);
        analyses.push((p.clone(), loaded, v));
    }
    for d in &dirs {
        if covered.contains(d) {
            continue;
        }
        let prog = crate::sema::analyze(&Sources::Disk, d);
        let loaded = loaded_dirs(&prog);
        covered.extend(loaded.iter().cloned());
        let mut v = Vec::new();
        collect(&prog, &mut v);
        analyses.push((d.clone(), loaded, v));
    }
    // Merge.
    let mut seen: BTreeMap<(String, u32, u32, String, String), usize> = BTreeMap::new();
    let mut keep: Vec<ProjectDiag> = Vec::new();
    for (root_dir, _, diags) in &analyses {
        for (d, pkg_dir) in diags {
            let msg = d.json.get("message").and_then(|m| m.as_str()).unwrap_or("").to_string();
            let key = (d.file.clone(), d.line, d.col, d.code.clone(), msg);
            if seen.contains_key(&key) {
                continue;
            }
            let dead = d.code.starts_with("E06");
            if dead && pkg_dir.as_ref() != Some(root_dir) {
                // Only if every analysis that loaded this package reports it.
                let dir = pkg_dir.clone().unwrap_or_default();
                let agree = analyses.iter().filter(|(_, loaded, _)| loaded.contains(&dir)).all(|(_, _, ds)| {
                    ds.iter().any(|(o, _)| o.file == d.file && o.line == d.line && o.code == d.code)
                });
                if !agree {
                    continue;
                }
            }
            seen.insert(key, keep.len());
            keep.push(d.clone());
        }
    }
    keep.sort_by(|a, b| (a.file.as_str(), a.line, a.col).cmp(&(b.file.as_str(), b.line, b.col)));
    ProjectReport { diags: keep, packages: dirs, programs }
}
