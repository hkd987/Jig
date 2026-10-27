//! Modules: projects that other projects depend on. A project's `jig.mod` names it
//! (`module NAME`), links C libraries (`link NAME`), requires modules by path and version
//! (`require github.com/x/y v1.2.0`, a git tag) and may point one at a local directory
//! (`replace github.com/x/y => ../y`). `jig get` downloads every required version into the
//! module cache (`$JIG_HOME/mod`, by default `~/.jig/mod`) and records its commit in `jig.sum`;
//! checking and building only read the cache, so they never touch the network.
//!
//! One version of each module is used: the highest any module in the build requires.

use crate::mir::interp::os_error;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A parsed `jig.mod`.
#[derive(Default, Debug)]
pub struct ModFile {
    pub module: Option<String>,
    pub links: Vec<String>,
    pub requires: Vec<(String, String)>,
    pub replaces: Vec<(String, String)>,
    /// Lines that are not understood, with their line numbers.
    pub errors: Vec<(usize, String)>,
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "_-.".contains(c))
}

fn is_module_path(s: &str) -> bool {
    !s.is_empty() && s.split('/').all(|seg| is_name(seg) && seg != "." && seg != "..")
}

pub fn parse(text: &str) -> ModFile {
    let mut m = ModFile::default();
    for (i, line) in text.lines().enumerate() {
        let line = line.split("//").next().unwrap_or("").trim();
        let words: Vec<&str> = line.split_whitespace().collect();
        match words.as_slice() {
            [] => {}
            ["module", name] => m.module = Some(name.to_string()),
            ["link", name] if is_name(name) => m.links.push(name.to_string()),
            ["require", path, version] if is_module_path(path) && is_name(version) => {
                m.requires.push((path.to_string(), version.to_string()))
            }
            ["replace", path, "=>", dir] if is_module_path(path) => {
                m.replaces.push((path.to_string(), dir.to_string()))
            }
            _ => m.errors.push((
                i + 1,
                format!(
                    "`{line}`: expected `module NAME`, `link NAME`, `require PATH VERSION` or `replace PATH => DIR`"
                ),
            )),
        }
    }
    m
}

/// Where downloaded modules live.
pub fn cache_dir() -> PathBuf {
    if let Some(h) = std::env::var_os("JIG_HOME") {
        return PathBuf::from(h).join("mod");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".jig").join("mod")
}

/// The directory of one downloaded version.
pub fn version_dir(path: &str, version: &str) -> PathBuf {
    cache_dir().join(format!("{path}@{version}"))
}

/// Order versions: `v1.10.0` after `v1.9.2`; numbers compare as numbers.
pub fn version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let parts = |v: &str| -> Vec<Result<u64, String>> {
        v.trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u64>().map_err(|_| p.to_string())).collect()
    };
    let (pa, pb) = (parts(a), parts(b));
    for (x, y) in pa.iter().zip(&pb) {
        let o = match (x, y) {
            (Ok(x), Ok(y)) => x.cmp(y),
            (Ok(_), Err(_)) => std::cmp::Ordering::Greater,
            (Err(_), Ok(_)) => std::cmp::Ordering::Less,
            (Err(x), Err(y)) => x.cmp(y),
        };
        if o != std::cmp::Ordering::Equal {
            return o;
        }
    }
    pa.len().cmp(&pb.len())
}

/// One module in the build.
#[derive(Clone, Debug)]
pub struct Resolved {
    pub path: String,
    pub version: String,
    pub dir: PathBuf,
    /// Pointed at a local directory by `replace`.
    pub replaced: bool,
    /// Not downloaded yet (`jig get`).
    pub missing: bool,
}

/// Every module the project at `root` needs, one version each.
pub fn resolve(root: &Path) -> (Vec<Resolved>, Vec<String>) {
    let mut errors = Vec::new();
    let Ok(text) = std::fs::read_to_string(root.join("jig.mod")) else { return (Vec::new(), errors) };
    let top = parse(&text);
    let replaces: Vec<(String, PathBuf)> = top.replaces.iter().map(|(p, d)| (p.clone(), root.join(d))).collect();
    let mut chosen: Vec<Resolved> = Vec::new();
    let mut pending: Vec<(String, String)> = top.requires.clone();
    // Raise versions until nothing changes; each newly chosen version's own requirements count.
    while let Some((path, version)) = pending.pop() {
        let at = chosen.iter().position(|r| r.path == path);
        if let Some(i) = at {
            if version_cmp(&version, &chosen[i].version) != std::cmp::Ordering::Greater {
                continue;
            }
        }
        let (dir, replaced) = match replaces.iter().find(|(p, _)| *p == path) {
            Some((_, d)) => (d.clone(), true),
            None => (version_dir(&path, &version), false),
        };
        let missing = !dir.join("jig.mod").exists();
        let r = Resolved { path: path.clone(), version: version.clone(), dir: dir.clone(), replaced, missing };
        match at {
            Some(i) => chosen[i] = r,
            None => chosen.push(r),
        }
        if missing {
            continue;
        }
        match std::fs::read_to_string(dir.join("jig.mod")) {
            Ok(t) => {
                let m = parse(&t);
                if m.module.as_deref() != Some(path.as_str()) {
                    errors.push(format!(
                        "{} says `module {}`, but it is required as `{path}`",
                        dir.join("jig.mod").display(),
                        m.module.as_deref().unwrap_or("?")
                    ));
                }
                pending.extend(m.requires);
            }
            Err(e) => errors.push(format!("cannot read {}: {e}", dir.join("jig.mod").display())),
        }
    }
    chosen.sort_by(|a, b| a.path.cmp(&b.path));
    (chosen, errors)
}

/// The module an import path belongs to: the longest module path it starts with.
pub fn module_of<'a>(mods: &[Resolved], import: &'a str) -> Option<(usize, &'a str)> {
    let mut best: Option<(usize, &str)> = None;
    for (i, m) in mods.iter().enumerate() {
        let rest = if import == m.path {
            Some("")
        } else {
            import.strip_prefix(m.path.as_str()).and_then(|r| r.strip_prefix('/'))
        };
        if let Some(rest) = rest {
            if best.is_none_or(|(b, _)| mods[b].path.len() < m.path.len()) {
                best = Some((i, rest));
            }
        }
    }
    best
}

fn git(args: &[&str], dir: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    let out = c.args(args).output().map_err(|e| format!("cannot run git: {e}"))?;
    if !out.status.success() {
        return Err(format!("git {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `jig.sum`: the commit of every downloaded module version.
fn read_sum(root: &Path) -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(root.join("jig.sum")).unwrap_or_default();
    text.lines()
        .filter_map(|l| {
            let w: Vec<&str> = l.split_whitespace().collect();
            match w.as_slice() {
                [p, v, c] => Some((p.to_string(), v.to_string(), c.to_string())),
                _ => None,
            }
        })
        .collect()
}

/// Download every required module version that is not in the cache, then record the commit
/// of each in `jig.sum`. A version whose commit differs from the one `jig.sum` recorded is an
/// error: the tag moved, or the download is not what was used before. Modules come from
/// `https://PATH` (or `$JIG_GIT_BASE` followed by the path).
pub fn get(root: &Path) -> Result<String, String> {
    let base = std::env::var("JIG_GIT_BASE").unwrap_or_else(|_| "https://".into());
    let mut log = String::new();
    loop {
        let (mods, errors) = resolve(root);
        if let Some(e) = errors.first() {
            return Err(e.clone());
        }
        let missing: Vec<Resolved> = mods.iter().filter(|m| m.missing && !m.replaced).cloned().collect();
        if missing.is_empty() {
            break;
        }
        for m in missing {
            let url = format!("{base}{}", m.path);
            let parent = m.dir.parent().ok_or("bad module cache path")?;
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("cannot create {}: {}", parent.display(), os_error(&e)))?;
            let tmp = parent.join(format!(
                ".tmp-{}-{}",
                std::process::id(),
                m.dir.file_name().unwrap_or_default().to_string_lossy()
            ));
            let _ = std::fs::remove_dir_all(&tmp);
            let tmp_s = tmp.to_string_lossy().to_string();
            git(&["clone", "--quiet", "--depth", "1", "--branch", &m.version, &url, &tmp_s], None)?;
            std::fs::rename(&tmp, &m.dir)
                .map_err(|e| format!("cannot move {} into the module cache: {}", m.path, os_error(&e)))?;
            log.push_str(&format!("downloaded {} {}\n", m.path, m.version));
        }
    }
    let (mods, _) = resolve(root);
    let old = read_sum(root);
    let mut sum = String::new();
    for m in mods.iter().filter(|m| !m.replaced) {
        let commit = git(&["rev-parse", "HEAD"], Some(&m.dir))?;
        if let Some((_, _, c)) = old.iter().find(|(p, v, _)| *p == m.path && *v == m.version) {
            if *c != commit {
                return Err(format!(
                    "{} {} is commit {commit}, but jig.sum recorded {c}: the tag moved or the download was changed",
                    m.path, m.version
                ));
            }
        }
        sum.push_str(&format!("{} {} {commit}\n", m.path, m.version));
    }
    if !sum.is_empty() || root.join("jig.sum").exists() {
        std::fs::write(root.join("jig.sum"), &sum).map_err(|e| format!("cannot write jig.sum: {}", os_error(&e)))?;
    }
    if log.is_empty() {
        log.push_str("all modules are downloaded\n");
    }
    Ok(log)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_orders() {
        let m =
            parse("module app\nlink m\nrequire github.com/a/b v1.2.0 // why\nreplace github.com/a/b => ../b\nbogus\n");
        assert_eq!(m.module.as_deref(), Some("app"));
        assert_eq!(m.links, ["m"]);
        assert_eq!(m.requires, [("github.com/a/b".to_string(), "v1.2.0".to_string())]);
        assert_eq!(m.replaces, [("github.com/a/b".to_string(), "../b".to_string())]);
        assert_eq!(m.errors.len(), 1);
        assert_eq!(version_cmp("v1.10.0", "v1.9.2"), std::cmp::Ordering::Greater);
        assert_eq!(version_cmp("v2.0.0", "v2.0.0"), std::cmp::Ordering::Equal);
        assert_eq!(version_cmp("v1.0", "v1.0.1"), std::cmp::Ordering::Less);
    }

    #[test]
    fn longest_module_wins() {
        let r = |p: &str| Resolved {
            path: p.into(),
            version: "v1".into(),
            dir: PathBuf::new(),
            replaced: false,
            missing: false,
        };
        let mods = [r("example.com/a"), r("example.com/a/b")];
        assert_eq!(module_of(&mods, "example.com/a/b/c"), Some((1, "c")));
        assert_eq!(module_of(&mods, "example.com/a/x"), Some((0, "x")));
        assert_eq!(module_of(&mods, "example.com/a"), Some((0, "")));
        assert_eq!(module_of(&mods, "example.com/ab"), None);
    }
}
