//! Loading packages: discover files, parse them, follow imports, detect cycles.

use super::defs::*;
use super::ty::*;
use crate::diag::{codes, Diagnostic, Diags, SourceMap, Span};
use crate::syntax::ast::{Ast, ItemKind};
use crate::syntax::parser;
use crate::util::intern::LocalInterner;
use crate::util::{FxHashMap, IndexVec, Symbol};
use std::path::{Path, PathBuf};

/// The embedded standard library: (import path, file name, source).
pub const STD_SOURCES: &[(&str, &str, &str)] = &[
    ("std/io", "io.jig", include_str!("../../std/io.jig")),
    ("std/fs", "fs.jig", include_str!("../../std/fs.jig")),
    ("std/os", "os.jig", include_str!("../../std/os.jig")),
    ("std/time", "time.jig", include_str!("../../std/time.jig")),
    ("std/rand", "rand.jig", include_str!("../../std/rand.jig")),
    ("std/math", "math.jig", include_str!("../../std/math.jig")),
];
pub const PRELUDE_SOURCE: &str = include_str!("../../std/prelude.jig");
pub const PRELUDE_PATH: &str = "<prelude>";

/// Where source files come from.
pub enum Sources {
    Disk,
    /// In-memory files, keyed by path (tests, editor buffers).
    Memory(FxHashMap<PathBuf, String>),
}

impl Sources {
    fn list_dir(&self, dir: &Path) -> Option<Vec<PathBuf>> {
        match self {
            Sources::Disk => {
                let rd = std::fs::read_dir(dir).ok()?;
                let mut v: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_file()).collect();
                v.sort();
                Some(v)
            }
            Sources::Memory(m) => {
                let mut v: Vec<PathBuf> = m.keys().filter(|p| p.parent() == Some(dir)).cloned().collect();
                if v.is_empty() && !m.keys().any(|p| p.starts_with(dir)) {
                    return None;
                }
                v.sort();
                Some(v)
            }
        }
    }
    fn read(&self, p: &Path) -> Option<String> {
        match self {
            Sources::Disk => std::fs::read_to_string(p).ok(),
            Sources::Memory(m) => m.get(p).cloned(),
        }
    }
    fn exists(&self, p: &Path) -> bool {
        match self {
            Sources::Disk => p.exists(),
            Sources::Memory(m) => m.contains_key(p),
        }
    }
}

pub fn find_project_root(sources: &Sources, start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(d) = cur {
        if sources.exists(&d.join("jig.mod")) {
            return Some(d.to_path_buf());
        }
        cur = d.parent();
    }
    None
}

struct Loader<'a> {
    sources: &'a Sources,
    sm: SourceMap,
    pkgs: IndexVec<PkgId, Package>,
    by_path: FxHashMap<Symbol, PkgId>,
    /// Packages currently being loaded (for cycle detection), with the import span that led
    /// to each.
    stack: Vec<PkgId>,
    done: Vec<bool>,
    diags: Diags,
    interner: LocalInterner,
    project_root: Option<PathBuf>,
}

/// Load the package in `dir` and everything it imports.
pub fn load_program(sources: &Sources, dir: &Path) -> Program {
    let dir = if matches!(sources, Sources::Disk) {
        std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf())
    } else {
        dir.to_path_buf()
    };
    let project_root = find_project_root(sources, &dir);
    let mut ld = Loader {
        sources,
        sm: SourceMap::new(),
        pkgs: IndexVec::new(),
        by_path: FxHashMap::default(),
        stack: Vec::new(),
        done: Vec::new(),
        diags: Diags::default(),
        interner: LocalInterner::default(),
        project_root: project_root.clone(),
    };
    let prelude = ld.load_embedded(PRELUDE_PATH, "prelude.jig", PRELUDE_SOURCE);
    let root_path = match &project_root {
        Some(r) => {
            let rel = dir.strip_prefix(r).unwrap_or(Path::new(""));
            let s = rel.to_string_lossy().replace('\\', "/");
            if s.is_empty() {
                "main".to_string()
            } else {
                s
            }
        }
        None => "main".to_string(),
    };
    let root = ld.load_dir_package(&root_path, &dir, None);
    let root = root.unwrap_or(prelude);
    let n = ld.pkgs.len();
    let mut tys = TyCtx::new();
    let tables = IndexVec::from_raw(
        ld.pkgs.iter().map(|p| Tables::new(p.ast.exprs.len(), p.ast.pats.len(), tys.error)).collect(),
    );
    let _ = &mut tys;
    debug_assert_eq!(tables.len(), n);
    Program {
        sm: ld.sm,
        pkgs: ld.pkgs,
        tables,
        defs: Defs::default(),
        tys,
        diags: ld.diags,
        bodies: IndexVec::new(),
        test_bodies: IndexVec::new(),
        prelude,
        root,
        project_root,
    }
}

impl Loader<'_> {
    fn new_package(&mut self, path: &str, dir: Option<PathBuf>, is_std: bool) -> PkgId {
        let name = path.rsplit('/').next().unwrap_or(path).trim_start_matches('<').trim_end_matches('>');
        let pkg = Package {
            path: Symbol::intern(path),
            name: Symbol::intern(name),
            dir,
            ast: Ast::default(),
            is_std,
            feature_files: Vec::new(),
            scope: FxHashMap::default(),
            imports: Vec::new(),
            deps: Vec::new(),
        };
        let id = self.pkgs.push(pkg);
        self.by_path.insert(Symbol::intern(path), id);
        self.done.push(false);
        id
    }

    fn parse_into(&mut self, pid: PkgId, path: PathBuf, text: String) {
        let fid = self.sm.add(path, text);
        let Loader { sm, pkgs, interner, .. } = self;
        let file = sm.file(fid);
        let out = parser::parse_file(&mut pkgs[pid].ast, fid, &file.text, file.base, interner);
        for d in out.diags {
            self.diags.push(d);
        }
    }

    fn load_embedded(&mut self, path: &str, file: &str, src: &str) -> PkgId {
        let pid = self.new_package(path, None, true);
        self.parse_into(pid, PathBuf::from(format!("<std>/{file}")), src.to_string());
        self.stack.push(pid);
        self.resolve_imports(pid);
        self.stack.pop();
        self.done[pid.0 as usize] = true;
        pid
    }

    fn load_dir_package(&mut self, path: &str, dir: &Path, import_span: Option<Span>) -> Option<PkgId> {
        let files = match self.sources.list_dir(dir) {
            Some(f) => f,
            None => {
                let span = import_span.unwrap_or(Span::DUMMY);
                self.diags.push(Diagnostic::error(
                    codes::E0203,
                    span,
                    format!("cannot find package `{path}` (no directory {})", dir.display()),
                ));
                return None;
            }
        };
        let jig_files: Vec<&PathBuf> = files.iter().filter(|p| p.extension().is_some_and(|e| e == "jig")).collect();
        if jig_files.is_empty() {
            let span = import_span.unwrap_or(Span::DUMMY);
            self.diags.push(Diagnostic::error(
                codes::E0203,
                span,
                format!("package `{path}` has no .jig files in {}", dir.display()),
            ));
            return None;
        }
        let pid = self.new_package(path, Some(dir.to_path_buf()), false);
        for f in &files {
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "jig" && ext != "feature" {
                continue;
            }
            let Some(text) = self.sources.read(f) else {
                self.diags.push(Diagnostic::error(codes::E0902, Span::DUMMY, format!("cannot read {}", f.display())));
                continue;
            };
            if ext == "jig" {
                self.parse_into(pid, f.clone(), text);
            } else {
                let fid = self.sm.add(f.clone(), text);
                self.pkgs[pid].feature_files.push(fid);
            }
        }
        self.stack.push(pid);
        self.resolve_imports(pid);
        self.stack.pop();
        self.done[pid.0 as usize] = true;
        Some(pid)
    }

    fn resolve_imports(&mut self, pid: PkgId) {
        let nfiles = self.pkgs[pid].ast.files.len();
        let mut per_file: Vec<FxHashMap<Symbol, (PkgId, crate::syntax::ast::ItemId)>> = Vec::with_capacity(nfiles);
        let mut deps: Vec<PkgId> = Vec::new();
        for fi in 0..nfiles {
            let mut map: FxHashMap<Symbol, (PkgId, crate::syntax::ast::ItemId)> = FxHashMap::default();
            let items: Vec<_> = {
                let ast = &self.pkgs[pid].ast;
                ast.item_lists.get(ast.files[fi].items).to_vec()
            };
            for item_id in items {
                let item = self.pkgs[pid].ast.items[item_id];
                let ItemKind::Import { path, path_span } = item.kind else { continue };
                let Some(target) = self.import(path.as_str(), path_span) else { continue };
                if target == pid {
                    self.diags.push(Diagnostic::error(codes::E0204, path_span, "a package cannot import itself"));
                    continue;
                }
                if let Some(&(_, prev)) = map.get(&item.name.sym) {
                    let prev_span = self.pkgs[pid].ast.items[prev].span;
                    self.diags.push(
                        Diagnostic::error(codes::E0209, item.span, format!("`{}` is imported twice", item.name.sym))
                            .label(prev_span, "first imported here"),
                    );
                    continue;
                }
                map.insert(item.name.sym, (target, item_id));
                if !deps.contains(&target) {
                    deps.push(target);
                }
            }
            per_file.push(map);
        }
        self.pkgs[pid].imports = per_file;
        self.pkgs[pid].deps = deps;
    }

    fn import(&mut self, path: &str, span: Span) -> Option<PkgId> {
        let key = Symbol::intern(path);
        if let Some(&p) = self.by_path.get(&key) {
            if !self.done[p.0 as usize] && self.stack.contains(&p) {
                let mut chain: Vec<String> =
                    self.stack.iter().skip_while(|&&q| q != p).map(|&q| self.pkgs[q].path.to_string()).collect();
                chain.push(path.to_string());
                self.diags.push(
                    Diagnostic::error(codes::E0204, span, format!("import cycle: {}", chain.join(" -> ")))
                        .note("move the shared code into a package that both can import"),
                );
                // Keep the binding so the rest of the package still resolves.
                return Some(p);
            }
            return Some(p);
        }
        if let Some(&(p, file, src)) = STD_SOURCES.iter().find(|(p, _, _)| *p == path) {
            return Some(self.load_embedded(p, file, src));
        }
        if path.starts_with("std/") || path == "std" {
            let known: Vec<&str> = STD_SOURCES.iter().map(|(p, _, _)| *p).collect();
            self.diags.push(
                Diagnostic::error(codes::E0203, span, format!("no standard library package `{path}`"))
                    .note(format!("standard packages: {}", known.join(", "))),
            );
            return None;
        }
        let Some(root) = self.project_root.clone() else {
            self.diags.push(
                Diagnostic::error(
                    codes::E0210,
                    span,
                    format!("cannot import `{path}`: no project root (a directory with `jig.mod`)"),
                )
                .note("create `jig.mod` containing `module <name>` in the project's top directory"),
            );
            return None;
        };
        if path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
            self.diags.push(Diagnostic::error(codes::E0203, span, format!("invalid import path `{path}`")));
            return None;
        }
        let dir = root.join(path);
        self.load_dir_package(path, &dir, Some(span))
    }
}
