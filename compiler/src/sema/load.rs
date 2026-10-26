//! Loading packages: discover files, parse them, follow imports, detect cycles.

use super::defs::*;
use super::ty::*;
use crate::diag::{codes, Diagnostic, Diags, FileId, SourceMap, Span};
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
    ("std/task", "task.jig", include_str!("../../std/task.jig")),
    ("std/sim", "sim.jig", include_str!("../../std/sim.jig")),
    ("std/net", "net.jig", include_str!("../../std/net.jig")),
    ("std/lists", "lists.jig", include_str!("../../std/lists.jig")),
    ("std/strings", "strings.jig", include_str!("../../std/strings.jig")),
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
    /// Packages found but not parsed yet, with their `.jig` files (already in `sm`).
    queue: Vec<(PkgId, Vec<FileId>)>,
    /// Each package's imports in source order: (target, import path span), for cycle detection.
    edges: Vec<Vec<(PkgId, Span)>>,
    diags: Diags,
    project_root: Option<PathBuf>,
    /// Required modules (`jig.mod`), and the module each package belongs to (`None`: the
    /// project itself or the standard library).
    modules: Vec<crate::modules::Resolved>,
    pkg_module: Vec<Option<usize>>,
}

/// Load the package in `dir` and everything it imports. Packages are parsed in waves: every
/// package found so far is parsed (in parallel when there is enough source), then their
/// imports name the next wave.
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
        queue: Vec::new(),
        edges: Vec::new(),
        diags: Diags::default(),
        project_root: project_root.clone(),
        modules: Vec::new(),
        pkg_module: Vec::new(),
    };
    let mut links = Vec::new();
    if let Some(r) = &project_root {
        if let Some(text) = sources.read(&r.join("jig.mod")) {
            let m = crate::modules::parse(&text);
            for (line, e) in m.errors {
                ld.diags.push(Diagnostic::error(codes::E0211, Span::DUMMY, format!("jig.mod line {line}: {e}")));
            }
            links = m.links;
            if matches!(sources, Sources::Disk) && !m.requires.is_empty() {
                let (mods, errors) = crate::modules::resolve(r);
                for e in errors {
                    ld.diags.push(Diagnostic::error(codes::E0211, Span::DUMMY, e));
                }
                ld.modules = mods;
            }
        }
    }
    let prelude = ld.add_embedded(PRELUDE_PATH, "prelude.jig", PRELUDE_SOURCE);
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
    let root = ld.add_dir_package(&root_path, &dir, None, None);
    loop {
        let wave = std::mem::take(&mut ld.queue);
        if wave.is_empty() {
            break;
        }
        ld.parse_wave(&wave);
        for (pid, _) in &wave {
            ld.resolve_imports(*pid);
        }
    }
    ld.report_cycles(&[Some(prelude), root]);
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
        scenarios: Vec::new(),
        links,
    }
}

/// Parse one package's files into its AST.
fn parse_package(sm: &SourceMap, ast: &mut Ast, files: &[FileId]) -> Vec<Diagnostic> {
    let mut interner = LocalInterner::default();
    let mut diags = Vec::new();
    for &fid in files {
        let file = sm.file(fid);
        diags.extend(parser::parse_file(ast, fid, &file.text, file.base, &mut interner).diags);
    }
    diags
}

impl Loader<'_> {
    fn new_package(&mut self, path: &str, dir: Option<PathBuf>, is_std: bool, module: Option<usize>) -> PkgId {
        let name = path.rsplit('/').next().unwrap_or(path).trim_start_matches('<').trim_end_matches('>');
        let pkg = Package {
            path: Symbol::intern(path),
            name: Symbol::intern(name),
            dir,
            ast: Ast::default(),
            is_std,
            external: module.is_some(),
            feature_files: Vec::new(),
            scope: FxHashMap::default(),
            imports: Vec::new(),
            deps: Vec::new(),
        };
        let id = self.pkgs.push(pkg);
        self.by_path.insert(Symbol::intern(path), id);
        self.edges.push(Vec::new());
        self.pkg_module.push(module);
        id
    }

    fn add_embedded(&mut self, path: &str, file: &str, src: &str) -> PkgId {
        let pid = self.new_package(path, None, true, None);
        let fid = self.sm.add(PathBuf::from(format!("<std>/{file}")), src.to_string());
        self.queue.push((pid, vec![fid]));
        pid
    }

    /// Register the package in `dir` and read its files; it is parsed with the next wave.
    fn add_dir_package(
        &mut self,
        path: &str,
        dir: &Path,
        import_span: Option<Span>,
        module: Option<usize>,
    ) -> Option<PkgId> {
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
        let pid = self.new_package(path, Some(dir.to_path_buf()), false, module);
        let mut sources = Vec::new();
        for f in &files {
            let ext = f.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "jig" && ext != "feature" {
                continue;
            }
            let Some(text) = self.sources.read(f) else {
                self.diags.push(Diagnostic::error(codes::E0902, Span::DUMMY, format!("cannot read {}", f.display())));
                continue;
            };
            let fid = self.sm.add(f.clone(), text);
            if ext == "jig" {
                sources.push(fid);
            } else {
                self.pkgs[pid].feature_files.push(fid);
            }
        }
        self.queue.push((pid, sources));
        Some(pid)
    }

    /// Parse a wave of packages, spread over threads when there is enough source to pay for
    /// them. Diagnostics are reported in wave order either way.
    fn parse_wave(&mut self, wave: &[(PkgId, Vec<FileId>)]) {
        let size = |files: &[FileId]| files.iter().map(|&f| self.sm.file(f).text.len()).sum::<usize>();
        let total: usize = wave.iter().map(|(_, fs)| size(fs)).sum();
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get()).min(wave.len());
        let mut results: Vec<Vec<Diagnostic>> = Vec::with_capacity(wave.len());
        if threads <= 1 || total < 256 * 1024 {
            for (pid, files) in wave {
                let d = parse_package(&self.sm, &mut self.pkgs[*pid].ast, files);
                results.push(d);
            }
        } else {
            // Largest packages first, each to the least loaded thread.
            let mut order: Vec<usize> = (0..wave.len()).collect();
            order.sort_by_key(|&i| std::cmp::Reverse(size(&wave[i].1)));
            let mut load = vec![0usize; threads];
            let mut jobs: Vec<Vec<usize>> = vec![Vec::new(); threads];
            for i in order {
                let t = (0..threads).min_by_key(|&t| load[t]).unwrap_or(0);
                load[t] += size(&wave[i].1);
                jobs[t].push(i);
            }
            // Move each wave package's AST out so threads own what they fill.
            let mut asts: Vec<Option<Ast>> =
                wave.iter().map(|(pid, _)| Some(std::mem::take(&mut self.pkgs[*pid].ast))).collect();
            let mut slots: Vec<Option<(Ast, Vec<Diagnostic>)>> = (0..wave.len()).map(|_| None).collect();
            let sm = &self.sm;
            std::thread::scope(|sc| {
                let mut handles = Vec::new();
                for job in &jobs {
                    let mine: Vec<(usize, Ast)> =
                        job.iter().map(|&i| (i, asts[i].take().unwrap_or_default())).collect();
                    handles.push(sc.spawn(move || {
                        mine.into_iter()
                            .map(|(i, mut ast)| {
                                let d = parse_package(sm, &mut ast, &wave[i].1);
                                (i, ast, d)
                            })
                            .collect::<Vec<_>>()
                    }));
                }
                for h in handles {
                    for (i, ast, d) in h.join().unwrap_or_default() {
                        slots[i] = Some((ast, d));
                    }
                }
            });
            for (k, slot) in slots.into_iter().enumerate() {
                let (ast, d) = slot.unwrap_or_default();
                self.pkgs[wave[k].0].ast = ast;
                results.push(d);
            }
        }
        for d in results.into_iter().flatten() {
            self.diags.push(d);
        }
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
                let from = self.pkg_module[pid.0 as usize];
                let Some(target) = self.import(path.as_str(), path_span, from) else { continue };
                if target == pid {
                    self.diags.push(Diagnostic::error(codes::E0204, path_span, "a package cannot import itself"));
                    continue;
                }
                self.edges[pid.0 as usize].push((target, path_span));
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

    /// Import cycles, found by walking imports depth first from `roots`: an import of a
    /// package still being walked closes a cycle.
    fn report_cycles(&mut self, roots: &[Option<PkgId>]) {
        let n = self.pkgs.len();
        let mut done = vec![false; n];
        let mut stack: Vec<PkgId> = Vec::new();
        // Explicit stack of (package, next edge index).
        for &root in roots.iter().flatten() {
            if done[root.0 as usize] {
                continue;
            }
            let mut work: Vec<(PkgId, usize)> = vec![(root, 0)];
            stack.push(root);
            while let Some(&mut (p, ref mut next)) = work.last_mut() {
                let Some(&(target, span)) = self.edges[p.0 as usize].get(*next) else {
                    done[p.0 as usize] = true;
                    stack.pop();
                    work.pop();
                    continue;
                };
                *next += 1;
                if done[target.0 as usize] {
                    continue;
                }
                if stack.contains(&target) {
                    let mut chain: Vec<String> =
                        stack.iter().skip_while(|&&q| q != target).map(|&q| self.pkgs[q].path.to_string()).collect();
                    chain.push(self.pkgs[target].path.to_string());
                    self.diags.push(
                        Diagnostic::error(codes::E0204, span, format!("import cycle: {}", chain.join(" -> ")))
                            .note("move the shared code into a package that both can import"),
                    );
                    continue;
                }
                stack.push(target);
                work.push((target, 0));
            }
        }
    }

    /// The package an import names, loading it if needed. `from` is the module of the
    /// importing package: its own packages are named relative to its root.
    fn import(&mut self, path: &str, span: Span, from: Option<usize>) -> Option<PkgId> {
        if let Some(&(p, file, src)) = STD_SOURCES.iter().find(|(p, _, _)| *p == path) {
            if let Some(&id) = self.by_path.get(&Symbol::intern(p)) {
                return Some(id);
            }
            return Some(self.add_embedded(p, file, src));
        }
        if path.starts_with("std/") || path == "std" {
            let known: Vec<&str> = STD_SOURCES.iter().map(|(p, _, _)| *p).collect();
            self.diags.push(
                Diagnostic::error(codes::E0203, span, format!("no standard library package `{path}`"))
                    .note(format!("standard packages: {}", known.join(", "))),
            );
            return None;
        }
        if path.split('/').any(|seg| seg.is_empty() || seg == "." || seg == "..") {
            self.diags.push(Diagnostic::error(codes::E0203, span, format!("invalid import path `{path}`")));
            return None;
        }
        // A required module's package, a package of the importing module, or the project's.
        let (full, dir, module) = if let Some((mi, rest)) = crate::modules::module_of(&self.modules, path) {
            let m = &self.modules[mi];
            if m.missing {
                self.diags.push(
                    Diagnostic::error(
                        codes::E0211,
                        span,
                        format!("module `{}` {} is not downloaded", m.path, m.version),
                    )
                    .note("run `jig get` to download the modules `jig.mod` requires"),
                );
                return None;
            }
            (path.to_string(), m.dir.join(rest), Some(mi))
        } else if let Some(mi) = from {
            let m = &self.modules[mi];
            (format!("{}/{path}", m.path), m.dir.join(path), Some(mi))
        } else {
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
            (path.to_string(), root.join(path), None)
        };
        if let Some(&p) = self.by_path.get(&Symbol::intern(&full)) {
            return Some(p);
        }
        self.add_dir_package(&full, &dir, Some(span), module)
    }
}
