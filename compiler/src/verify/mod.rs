//! `jig prove`: per-function verification conditions discharged by an SMT solver, with a
//! content-addressed proof cache so re-proving after a one-function change only redoes that
//! function (callers depend on its contract, not its body).

pub mod frame;
pub mod smt;
pub mod vc;

use crate::diag::Span;
use crate::sema::defs::Program;
use crate::sema::ty::FnId;
use crate::syntax::ast::{self, ExprKind};
use crate::util::fx::{hash_hex, StableHasher};
use crate::util::json::Json;
use crate::util::{FxHashMap, FxHashSet};
use smt::{Sat, Solver};
use std::path::PathBuf;
use vc::{Base, ObKind};

/// Bump when the encoding changes, so stale cached proofs are not reused.
const VC_VERSION: &str = "vc-10";

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Proved,
    /// The solver found inputs that break it (under the modular abstraction).
    Failed(Vec<(String, String)>),
    Unknown(String),
}

pub struct ObResult {
    pub kind: ObKind,
    pub span: Span,
    pub msg: String,
    pub status: Status,
    pub elidable: bool,
    /// For failed contracts: what running the function on the interpreter showed.
    pub replay: Option<Replay>,
    /// The counterexample's parameter values, when they can be rebuilt (not cached).
    pub model: Option<Vec<Vec<i128>>>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Replay {
    /// Running the function on these inputs fails the same check: a real bug.
    Confirmed(String),
    /// The counterexample's inputs pass: the prover is missing a fact.
    NotReproduced,
}

pub struct FnReport {
    pub fid: FnId,
    pub name: String,
    pub obligations: Vec<ObResult>,
    pub cached: bool,
}

pub struct Report {
    pub fns: Vec<FnReport>,
    pub solver: String,
    pub millis: u128,
}

#[derive(Clone, Debug)]
pub struct ProveOpts {
    /// Where cached proofs live (`.jig/proofs` under the project); `None` disables caching.
    pub cache_dir: Option<PathBuf>,
    pub timeout_ms: u32,
    /// Only functions whose name contains this.
    pub filter: Option<String>,
    /// Prove the standard library's functions instead of the program's.
    pub std: bool,
}

/// Whether a function states a contract: `requires`, `ensures` or a refined parameter or
/// result.
pub fn has_contract(prog: &Program, f: FnId) -> bool {
    let fd = &prog.defs.fns[f];
    if fd.ret_refine.is_some() || fd.params.iter().any(|p| p.refine.is_some()) {
        return true;
    }
    let pkg = &prog.pkgs[fd.pkg];
    match pkg.ast.items[fd.item].kind {
        ast::ItemKind::Fn(d) => !d.requires.is_empty() || !d.ensures.is_empty(),
        _ => false,
    }
}

impl Report {
    /// Functions with contracts, and how many of them prove completely (every obligation in
    /// them, contracts and traps).
    pub fn contract_coverage(&self, prog: &Program) -> (usize, usize) {
        let with: Vec<&FnReport> = self.fns.iter().filter(|f| has_contract(prog, f.fid)).collect();
        let proved = with.iter().filter(|f| f.obligations.iter().all(|o| o.status == Status::Proved)).count();
        (with.len(), proved)
    }

    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let mut c = (0, 0, 0, 0);
        for f in &self.fns {
            for o in &f.obligations {
                if o.kind.is_contract() {
                    c.1 += 1;
                    c.0 += (o.status == Status::Proved) as usize;
                } else {
                    c.3 += 1;
                    c.2 += (o.status == Status::Proved) as usize;
                }
            }
        }
        c
    }

    pub fn failed_contracts(&self) -> usize {
        self.fns
            .iter()
            .flat_map(|f| &f.obligations)
            .filter(|o| o.kind.is_contract() && matches!(o.status, Status::Failed(_)))
            .count()
    }

    /// Spans whose runtime checks are fully discharged: every obligation recorded there is
    /// proved. Preconditions are never removed (they are checked in the callee, which must
    /// stay safe for every caller).
    pub fn proven(&self) -> Proven {
        let mut contract: FxHashMap<Span, bool> = FxHashMap::default();
        let mut safety: FxHashMap<Span, bool> = FxHashMap::default();
        let mut bounds: FxHashMap<Span, bool> = FxHashMap::default();
        for f in &self.fns {
            for o in &f.obligations {
                let ok = o.elidable && o.status == Status::Proved;
                match o.kind {
                    ObKind::Precondition => {}
                    k if k.is_contract() => *contract.entry(o.span).or_insert(true) &= ok,
                    ObKind::Overflow | ObKind::DivZero | ObKind::Shift => *safety.entry(o.span).or_insert(true) &= ok,
                    ObKind::Bounds => *bounds.entry(o.span).or_insert(true) &= ok,
                    _ => {}
                }
            }
        }
        let keep = |m: FxHashMap<Span, bool>| m.into_iter().filter(|(_, ok)| *ok).map(|(s, _)| s).collect();
        Proven { contract: keep(contract), safety: keep(safety), bounds: keep(bounds), entries: FxHashSet::default() }
    }
}

/// Discharged obligations, consulted by lowering and code generation to drop runtime checks.
#[derive(Default, Clone, Debug)]
pub struct Proven {
    /// Contract checks (asserts, postconditions, invariants, refinements) at these spans.
    pub contract: FxHashSet<Span>,
    /// Arithmetic traps (overflow, division by zero, shift range) at these spans.
    pub safety: FxHashSet<Span>,
    /// List indexing proved in bounds at these spans.
    pub bounds: FxHashSet<Span>,
    /// Functions whose entry checks (`requires`, parameter refinements) are proved at every
    /// call, so the function itself need not check them.
    pub entries: FxHashSet<FnId>,
}

impl Proven {
    pub fn is_empty(&self) -> bool {
        self.contract.is_empty() && self.safety.is_empty() && self.bounds.is_empty() && self.entries.is_empty()
    }
}

/// The functions `jig prove` checks: non-generic user functions with bodies.
/// Whether `span` is in a `*_test.jig` file (test helpers, which run only in tests).
fn in_test_file(prog: &Program, span: Span) -> bool {
    prog.sm.file_of(span.lo).is_some_and(|f| prog.sm.file(f).path.to_string_lossy().ends_with("_test.jig"))
}

/// The functions to prove: the program's functions with bodies (generic ones with their type
/// parameters left abstract), or the standard library's, except test helpers (tests are not
/// proved; their assertions are checked when they run).
pub fn targets(prog: &Program, filter: Option<&str>, std: bool) -> Vec<FnId> {
    prog.defs
        .fns
        .iter_enumerated()
        .filter(|(f, fd)| {
            prog.pkgs[fd.pkg].is_std == std
                && !prog.pkgs[fd.pkg].external
                && !in_test_file(prog, fd.span)
                && fd.has_body
                && fd.iface.is_none()
                && prog.bodies[*f].is_some()
                && filter.is_none_or(|q| fd.name.as_str().contains(q))
        })
        .map(|(f, _)| f)
        .collect()
}

/// The text of a function's signature and contract (what its callers rely on).
fn contract_text(prog: &Program, f: FnId) -> String {
    let fd = &prog.defs.fns[f];
    let pkg = &prog.pkgs[fd.pkg];
    let item = pkg.ast.items[fd.item];
    let end = match item.kind {
        ast::ItemKind::Fn(d) => d.body.map(|b| pkg.ast.exprs[b].span.lo).unwrap_or(item.span.hi),
        _ => item.span.hi,
    };
    format!("{}|{}", pkg.path, prog.sm.snippet(Span::new(item.span.lo, end)))
}

/// Functions referenced from a function's contract expressions.
fn contract_refs(prog: &Program, f: FnId, out: &mut Vec<FnId>) {
    let fd = &prog.defs.fns[f];
    let pkg = &prog.pkgs[fd.pkg];
    let ast::ItemKind::Fn(d) = pkg.ast.items[fd.item].kind else { return };
    let mut stack: Vec<ast::ExprId> = pkg.ast.expr_lists.get(d.requires).to_vec();
    stack.extend(pkg.ast.expr_lists.get(d.ensures).iter().copied());
    for p in pkg.ast.params.get(d.params) {
        stack.extend(p.refine);
    }
    let tables = &prog.tables[fd.pkg];
    while let Some(e) = stack.pop() {
        if let ExprKind::Call { callee, .. } = pkg.ast.exprs[e].kind {
            if let crate::sema::defs::Res::Fn(g) | crate::sema::defs::Res::Method(g) =
                tables.expr_res[callee.0 as usize]
            {
                out.push(g);
            }
        }
        crate::mir::lower::visit_children(&pkg.ast, e, &mut stack);
    }
}

/// Everything a function's proof depends on: its own text, the contracts of what it calls
/// (transitively through contracts), every type predicate, the solver and the encoding.
type FrameMap = FxHashMap<(FnId, usize), frame::Frame>;

fn cache_key(prog: &Program, f: FnId, global: &str, frames: &FrameMap, reads: &FrameMap) -> String {
    let fd = &prog.defs.fns[f];
    let pkg = &prog.pkgs[fd.pkg];
    let item = pkg.ast.items[fd.item];
    let mut h = StableHasher::new();
    h.str(global).str(pkg.path.as_str()).str(prog.sm.snippet(item.span));
    let mut seen = FxHashSet::default();
    let mut work: Vec<FnId> = prog.bodies[f].as_ref().map(|b| b.refs.clone()).unwrap_or_default();
    let mut deps = Vec::new();
    while let Some(g) = work.pop() {
        if !seen.insert(g) || seen.len() > 512 {
            continue;
        }
        deps.push(g);
        contract_refs(prog, g, &mut work);
    }
    let mut texts: Vec<String> = deps.iter().map(|&g| contract_text(prog, g)).collect();
    // What the pure functions it calls, directly or in contracts, read of their arguments.
    for &g in &deps {
        for i in 0..=prog.defs.fns[g].params.len() {
            if let Some(fr) = reads.get(&(g, i)) {
                texts.push(format!("reads {} {i} {}", contract_text(prog, g), fr.summary()));
            }
        }
    }
    // What direct callees may change in their `&mut` arguments (read off their bodies).
    for &g in prog.bodies[f].as_ref().map(|b| b.refs.as_slice()).unwrap_or(&[]) {
        for i in 0..=prog.defs.fns[g].params.len() {
            if let Some(fr) = frames.get(&(g, i)) {
                texts.push(format!("frame {} {i} {}", contract_text(prog, g), fr.summary()));
            }
        }
    }
    texts.sort();
    for t in &texts {
        h.str(t);
    }
    hash_hex(h.finish())
}

/// Facts shared by every key: the encoding, the solver, the standard library and all type
/// declarations (struct invariants and refinement aliases).
fn global_key(prog: &Program, solver: &str) -> String {
    let mut h = StableHasher::new();
    h.str(VC_VERSION).str(env!("CARGO_PKG_VERSION")).str(solver);
    for (p, f, src) in crate::sema::load::STD_SOURCES {
        h.str(p).str(f).str(src);
    }
    for pkg in prog.pkgs.iter() {
        if pkg.is_std {
            continue;
        }
        for item in pkg.ast.items.iter() {
            if matches!(
                item.kind,
                ast::ItemKind::Struct { .. } | ast::ItemKind::TypeAlias { .. } | ast::ItemKind::Enum { .. }
            ) {
                h.str(prog.sm.snippet(item.span));
            }
        }
    }
    hash_hex(h.finish())
}

fn status_json(s: &Status) -> Json {
    match s {
        Status::Proved => Json::obj().with("status", "proved"),
        Status::Failed(cex) => {
            let mut c = Json::obj();
            for (k, v) in cex {
                c.set(k, v.as_str());
            }
            Json::obj().with("status", "failed").with("counterexample", c)
        }
        Status::Unknown(why) => Json::obj().with("status", "unknown").with("reason", why.as_str()),
    }
}

fn load_cached(path: &PathBuf, base: u32) -> Option<Vec<ObResult>> {
    let src = std::fs::read_to_string(path).ok()?;
    let j = Json::parse(&src).ok()?;
    let mut out = Vec::new();
    for o in j.get("obligations")?.as_arr()? {
        let kind = ObKind::parse(o.get("kind")?.as_str()?)?;
        let lo = o.get("lo")?.as_i64()? as u32 + base;
        let hi = o.get("hi")?.as_i64()? as u32 + base;
        let msg = o.get("msg")?.as_str()?.to_string();
        let elidable = o.get("elidable")?.as_bool()?;
        let status = match o.get("status")?.as_str()? {
            "proved" => Status::Proved,
            "failed" => {
                let mut cex = Vec::new();
                if let Some(Json::Obj(kv)) = o.get("counterexample") {
                    for (k, v) in kv {
                        cex.push((k.clone(), v.as_str().unwrap_or("").to_string()));
                    }
                }
                Status::Failed(cex)
            }
            _ => Status::Unknown(o.get("reason").and_then(|r| r.as_str()).unwrap_or("").to_string()),
        };
        let replay = match o.get("replay").and_then(|r| r.as_str()) {
            Some("not reproduced") => Some(Replay::NotReproduced),
            Some(inputs) => Some(Replay::Confirmed(inputs.to_string())),
            None => None,
        };
        out.push(ObResult { kind, span: Span::new(lo, hi), msg, status, elidable, replay, model: None });
    }
    Some(out)
}

fn store_cached(path: &PathBuf, base: u32, obs: &[ObResult]) {
    let mut arr = Json::arr();
    for o in obs {
        let mut j = status_json(&o.status);
        j.set("kind", o.kind.name());
        j.set("lo", (o.span.lo - base) as i64);
        j.set("hi", (o.span.hi - base) as i64);
        j.set("msg", o.msg.as_str());
        j.set("elidable", o.elidable);
        match &o.replay {
            Some(Replay::Confirmed(inputs)) => j.set("replay", inputs.as_str()),
            Some(Replay::NotReproduced) => j.set("replay", "not reproduced"),
            None => {}
        }
        arr.push(j);
    }
    let j = Json::obj().with("obligations", arr);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    if std::fs::write(&tmp, j.to_string_compact()).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// Render a model value for a counterexample.
fn show_value(v: &smt::Sexp, base: &Base) -> String {
    match base {
        Base::Bool => v.as_bool().map(|b| b.to_string()).unwrap_or_else(|| "?".into()),
        Base::Char => {
            v.as_int().and_then(|c| char::from_u32(c as u32)).map(|c| format!("{c:?}")).unwrap_or_else(|| "?".into())
        }
        _ => v.as_int().map(|i| i.to_string()).unwrap_or_else(|| "?".into()),
    }
}

/// The cached proof results, if every target function has a fresh entry (for builds).
pub fn cached_proven(prog: &Program, cache_dir: &std::path::Path) -> Proven {
    let solver = smt::solver_version().unwrap_or_default();
    let global = global_key(prog, &solver);
    let frames = frame::compute_all(prog);
    let reads = frame::compute_reads(prog);
    let mut fns = Vec::new();
    for f in targets(prog, None, false) {
        let key = cache_key(prog, f, &global, &frames, &reads);
        let base = prog.pkgs[prog.defs.fns[f].pkg].ast.items[prog.defs.fns[f].item].span.lo;
        if let Some(obs) = load_cached(&cache_dir.join(format!("{key}.json")), base) {
            fns.push(FnReport { fid: f, name: String::new(), obligations: obs, cached: true });
        }
    }
    let report = Report { fns, solver, millis: 0 };
    let mut proven = report.proven();
    proven.entries = proven_entries(prog, &report);
    proven
}

/// Functions whose entry checks hold at every call. The program is closed (no dynamic loading
/// or reflection), so when each call site has its preconditions proved, the callee need not
/// check them again. A function used as a value or reached through a generic bound has calls
/// the prover cannot see, and one with no calls at all is an entry point; neither qualifies.
/// Calls in tests and test helpers don't count: they run in the interpreter, which keeps every
/// check.
fn proven_entries(prog: &Program, report: &Report) -> FxHashSet<FnId> {
    let mut sites: FxHashMap<FnId, Vec<Span>> = FxHashMap::default();
    let mut as_value: FxHashSet<FnId> = FxHashSet::default();
    for (pid, pkg) in prog.pkgs.iter_enumerated() {
        let ast = &pkg.ast;
        let tests: Vec<Span> =
            ast.items.iter().filter(|it| matches!(it.kind, ast::ItemKind::Test { .. })).map(|it| it.span).collect();
        let in_test = |sp: Span| tests.iter().any(|t| t.lo <= sp.lo && sp.hi <= t.hi) || in_test_file(prog, sp);
        let mut callee_of: FxHashMap<u32, Span> = FxHashMap::default();
        for e in ast.exprs.iter() {
            if let ExprKind::Call { callee, .. } = e.kind {
                callee_of.insert(callee.0, e.span);
            }
        }
        let res = &prog.tables[pid].expr_res;
        for (i, e) in ast.exprs.iter().enumerate() {
            let f = match res.get(i) {
                Some(crate::sema::defs::Res::Fn(f) | crate::sema::defs::Res::Method(f)) => *f,
                _ => continue,
            };
            match callee_of.get(&(i as u32)) {
                Some(&call) if in_test(call) => {}
                Some(&call) => sites.entry(f).or_default().push(call),
                None if in_test(e.span) => {}
                None => {
                    as_value.insert(f);
                }
            }
        }
    }
    let dyn_names: FxHashSet<crate::util::Symbol> =
        prog.bodies.iter().flatten().flat_map(|b| b.dyn_methods.iter().copied()).collect();
    // Per call site: whether every precondition obligation there was proved.
    let mut at: FxHashMap<Span, bool> = FxHashMap::default();
    for f in &report.fns {
        for o in f.obligations.iter().filter(|o| o.kind == ObKind::Precondition) {
            *at.entry(o.span).or_insert(true) &= o.elidable && o.status == Status::Proved;
        }
    }
    let mut out = FxHashSet::default();
    for (f, calls) in sites {
        let fd = &prog.defs.fns[f];
        if as_value.contains(&f) || fd.step.is_some() || (fd.recv.is_some() && dyn_names.contains(&fd.name)) {
            continue;
        }
        if calls.iter().all(|sp| at.get(sp) == Some(&true)) {
            out.insert(f);
        }
    }
    out
}

pub fn prove(prog: &mut Program, opts: &ProveOpts) -> Result<Report, String> {
    let start = std::time::Instant::now();
    let solver_name = smt::solver_version()
        .ok_or_else(|| format!("cannot run the SMT solver `{}` (install z3 or set JIG_Z3)", smt::solver_path()))?;
    let global = global_key(prog, &solver_name);
    let fns = targets(prog, opts.filter.as_deref(), opts.std);
    let frames = frame::compute_all(prog);
    let reads = frame::compute_reads(prog);
    // Keys and cached results first (needs only shared access).
    let mut plan = Vec::new();
    for &f in &fns {
        let key = cache_key(prog, f, &global, &frames, &reads);
        let fd = &prog.defs.fns[f];
        let base = prog.pkgs[fd.pkg].ast.items[fd.item].span.lo;
        let name = fd.display_name(&prog.defs, &prog.tys);
        let cached = opts.cache_dir.as_ref().and_then(|d| load_cached(&d.join(format!("{key}.json")), base));
        plan.push((f, key, base, name, cached));
    }
    let debug = std::env::var_os("JIG_PROVE_DEBUG").is_some();
    // Generate every uncached function's conditions (needs the program), then solve them on
    // one solver process per core.
    let mut reports: Vec<Option<FnReport>> = Vec::new();
    let mut work: Vec<(usize, String, u32, vc::FnVcs)> = Vec::new();
    {
        let mut gen = vc::Gen::new(prog);
        gen.frames = frames;
        gen.reads = reads;
        for (f, key, base, name, cached) in plan {
            if let Some(obs) = cached {
                reports.push(Some(FnReport { fid: f, name, obligations: obs, cached: true }));
                continue;
            }
            let Some(vcs) = gen.function(f) else { continue };
            work.push((reports.len(), key, base, vcs));
            reports.push(Some(FnReport { fid: f, name, obligations: Vec::new(), cached: false }));
        }
    }
    let jobs =
        if debug { 1 } else { std::thread::available_parallelism().map_or(1, |n| n.get()).min(work.len().max(1)) };
    // Deal functions round-robin so every worker gets a similar mix.
    let mut buckets: Vec<Vec<(usize, String, u32, vc::FnVcs)>> = (0..jobs).map(|_| Vec::new()).collect();
    for (i, w) in work.into_iter().enumerate() {
        buckets[i % jobs].push(w);
    }
    let names: Vec<String> = reports.iter().map(|r| r.as_ref().map(|r| r.name.clone()).unwrap_or_default()).collect();
    let results: Vec<Result<Vec<Solved>, String>> = std::thread::scope(|sc| {
        let handles: Vec<_> = buckets
            .into_iter()
            .filter(|b| !b.is_empty())
            .map(|bucket| {
                let names = &names;
                sc.spawn(move || -> Result<Vec<Solved>, String> {
                    let mut s = Solver::start(opts.timeout_ms)?;
                    let mut out = Vec::new();
                    for (idx, key, base, vcs) in bucket {
                        if debug {
                            eprintln!(";; ---- {} ----", names[idx]);
                        }
                        out.push((idx, key, base, solve_fn(&mut s, vcs, debug)));
                    }
                    Ok(out)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("solver worker panicked".into()))).collect()
    });
    let mut solved = Vec::new();
    for r in results {
        solved.extend(r?);
    }
    replay_failures(prog, &mut solved, &reports);
    {
        for (idx, key, base, obs) in solved {
            if let Some(dir) = &opts.cache_dir {
                // Timeouts are not cached: a faster machine (or a longer timeout) may prove them.
                if !obs.iter().any(|o| matches!(&o.status, Status::Unknown(r) if r == "timeout")) {
                    store_cached(&dir.join(format!("{key}.json")), base, &obs);
                }
            }
            if let Some(rep) = reports[idx].as_mut() {
                rep.obligations = obs;
            }
        }
    }
    let reports: Vec<FnReport> = reports.into_iter().flatten().collect();
    Ok(Report { fns: reports, solver: solver_name, millis: start.elapsed().as_millis() })
}

/// Run functions whose contracts failed on the interpreter: first with the counterexample
/// (when its inputs can be rebuilt), then with generated inputs. A run that fails the same
/// check confirms a real bug; a counterexample that passes means a missing fact.
fn replay_failures(prog: &mut Program, solved: &mut [Solved], reports: &[Option<FnReport>]) {
    let todo: Vec<usize> = (0..solved.len())
        .filter(|&i| solved[i].3.iter().any(|o| o.kind.is_contract() && matches!(o.status, Status::Failed(_))))
        .collect();
    if todo.is_empty() {
        return;
    }
    let mut lw = crate::mir::lower::Lowerer::new(prog, crate::mir::LowerOpts { check_contracts: true });
    for i in todo {
        let Some(rep) = reports[solved[i].0].as_ref() else { continue };
        let f = rep.fid;
        let ptys: Vec<crate::sema::ty::Ty> = match lw.bodies[f].as_ref() {
            Some(b) => b.param_locals.iter().map(|&l| b.locals[l].ty).collect(),
            None => continue,
        };
        let names: Vec<String> = match lw.bodies[f].as_ref() {
            Some(b) => b.param_locals.iter().map(|&l| b.locals[l].name.to_string()).collect(),
            None => continue,
        };
        // The function's own entry checks reject inputs instead of failing them.
        let own = own_entry_spans(&lw, f);
        let m = lw.instance(f, Box::new([]));
        let obs = &mut solved[i].3;
        for o in obs.iter_mut().filter(|o| o.kind.is_contract() && matches!(o.status, Status::Failed(_))) {
            if let Some(model) = &o.model {
                let mut vals = model.iter().map(|v| v.iter().copied());
                let args: Option<Vec<crate::mir::interp::Value>> =
                    ptys.iter().map(|&t| build_value(lw.tys, lw.defs, t, &mut vals.next()?)).collect();
                if let Some(args) = args {
                    o.replay = match run_matches(&mut lw, m, args.clone(), o, &own) {
                        Some(true) => Some(Replay::Confirmed(show_args(&mut lw, &args, &ptys, &names))),
                        Some(false) => Some(Replay::NotReproduced),
                        None => None,
                    };
                    continue;
                }
            }
            // No rebuildable counterexample: try generated inputs, seeded by the function's
            // name so the inputs don't change when unrelated functions are added.
            let seed =
                rep.name.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3));
            let mut rng = crate::proptest::Rng::new(seed);
            for case in 0..200u64 {
                let args: Option<Vec<crate::mir::interp::Value>> = ptys
                    .iter()
                    .map(|&t| crate::proptest::generate(lw.tys, lw.defs, t, &mut rng, 2 + case * 5, 0))
                    .collect();
                let Some(args) = args else { break };
                if run_matches(&mut lw, m, args.clone(), o, &own) == Some(true) {
                    let args = shrink_replay(&mut lw, m, &ptys, args, o, &own);
                    o.replay = Some(Replay::Confirmed(show_args(&mut lw, &args, &ptys, &names)));
                    break;
                }
            }
        }
    }
}

/// The smallest inputs found that still fail the obligation's check.
fn shrink_replay(
    lw: &mut crate::mir::lower::Lowerer,
    m: crate::mir::MFnId,
    ptys: &[crate::sema::ty::Ty],
    mut args: Vec<crate::mir::interp::Value>,
    o: &ObResult,
    own: &[Span],
) -> Vec<crate::mir::interp::Value> {
    let mut budget = 300;
    'outer: while budget > 0 {
        for i in 0..args.len() {
            for c in crate::proptest::shrink(lw.tys, lw.defs, ptys[i], &args[i]) {
                budget -= 1;
                if budget == 0 {
                    break 'outer;
                }
                let mut trial = args.clone();
                trial[i] = c;
                if run_matches(lw, m, trial.clone(), o, own) == Some(true) {
                    args = trial;
                    continue 'outer;
                }
            }
        }
        break;
    }
    args
}

/// Spans of a function's own entry checks (`requires`, parameter refinements).
fn own_entry_spans(lw: &crate::mir::lower::Lowerer, f: FnId) -> Vec<Span> {
    let fd = &lw.defs.fns[f];
    let pkg = &lw.pkgs[fd.pkg];
    let mut out: Vec<Span> = fd.params.iter().map(|p| p.span).collect();
    if let ast::ItemKind::Fn(d) = pkg.ast.items[fd.item].kind {
        out.extend(pkg.ast.expr_lists.get(d.requires).iter().map(|&e| pkg.ast.exprs[e].span));
        for p in pkg.ast.params.get(d.params) {
            if let Some(r) = p.refine {
                out.push(pkg.ast.exprs[r].span);
            }
        }
    }
    out
}

/// Run `m(args)`: `Some(true)` if it fails the obligation's check, `Some(false)` if it runs
/// to completion, `None` if it stops elsewhere first (rejected by the function's own entry
/// checks, or another failure), which says nothing about this obligation.
fn run_matches(
    lw: &mut crate::mir::lower::Lowerer,
    m: crate::mir::MFnId,
    args: Vec<crate::mir::interp::Value>,
    o: &ObResult,
    own: &[Span],
) -> Option<bool> {
    use crate::mir::interp::{Effects, Interp, Trap};
    let mut it = Interp::new(lw, Effects::fake(1));
    it.fuel = 5_000_000;
    match it.call(m, args) {
        Err(Trap::Panic { span, msg, .. }) => {
            if own.contains(&span) {
                return None;
            }
            let pred = o.msg.rsplit_once("requires ").map(|(_, p)| p).unwrap_or(&o.msg);
            let pred = pred.trim_end_matches(" (on entry)").trim_end_matches(" (preserved)");
            if span == o.span || (o.kind == ObKind::Precondition && msg.contains(pred)) {
                Some(true)
            } else {
                None
            }
        }
        Err(Trap::Exit(_)) => None,
        Ok(_) => Some(false),
    }
}

fn show_args(
    lw: &mut crate::mir::lower::Lowerer,
    args: &[crate::mir::interp::Value],
    tys: &[crate::sema::ty::Ty],
    names: &[String],
) -> String {
    let mut it = crate::mir::interp::Interp::new(lw, crate::mir::interp::Effects::fake(1));
    args.iter()
        .zip(tys)
        .zip(names)
        .map(|((v, &t), n)| format!("{n} = {}", it.show(v, t, true).unwrap_or_else(|_| "?".into())))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A value of a scalar-tree type from model values in leaf order.
fn build_value(
    tys: &mut crate::sema::ty::TyCtx,
    defs: &crate::sema::defs::Defs,
    t: crate::sema::ty::Ty,
    vals: &mut impl Iterator<Item = i128>,
) -> Option<crate::mir::interp::Value> {
    use crate::mir::interp::Value;
    use crate::sema::ty::{IntTy, TyKind};
    Some(match tys.kind(t).clone() {
        TyKind::Int(IntTy::U64, _) => Value::UInt(vals.next()? as u64),
        TyKind::Int(..) => Value::Int(vals.next()? as i64),
        TyKind::Bool => Value::Bool(vals.next()? != 0),
        TyKind::Char => Value::Char(vals.next()? as u32),
        TyKind::Tuple(ts) if ts.is_empty() => Value::Unit,
        TyKind::Tuple(ts) => {
            let mut xs = Vec::new();
            for &x in ts.iter() {
                xs.push(build_value(tys, defs, x, vals)?);
            }
            Value::Tuple(std::rc::Rc::new(xs))
        }
        TyKind::Adt(a, args) if !defs.adts[a].is_enum() => {
            let fts: Vec<crate::sema::ty::Ty> = defs.adts[a].fields().iter().map(|f| f.ty).collect();
            let mut xs = Vec::new();
            for ft in fts {
                let ft = tys.subst(ft, &args);
                xs.push(build_value(tys, defs, ft, vals)?);
            }
            Value::Tuple(std::rc::Rc::new(xs))
        }
        _ => return None,
    })
}

/// A solved function: (report index, cache key, span base, results).
type Solved = (usize, String, u32, Vec<ObResult>);

/// Discharge one function's obligations on a solver.
fn solve_fn(s: &mut Solver, vcs: vc::FnVcs, debug: bool) -> Vec<ObResult> {
    s.reset();
    s.send("(push 1)");
    for d in &vcs.decls {
        s.send(d);
        if debug {
            eprintln!("{d}");
        }
    }
    let mut obs = Vec::new();
    for ob in vcs.obligations {
        let mut model = None;
        let status = if ob.goal == "true" || ob.pc.iter().any(|p| p == "false") {
            Status::Proved
        } else {
            s.last_error = None;
            s.send("(push 1)");
            for p in &ob.pc {
                s.send(&format!("(assert {p})"));
            }
            s.send(&format!("(assert (not {}))", ob.goal));
            if debug {
                let pcs: Vec<String> = ob.pc.iter().map(|p| format!("(assert {p})")).collect();
                eprintln!(";; {} {}\n{}\n(assert (not {}))", ob.kind.name(), ob.msg, pcs.join("\n"), ob.goal);
            }
            let st = match s.check() {
                Sat::Unsat => Status::Proved,
                Sat::Sat => {
                    let terms: Vec<String> = vcs.show.iter().map(|(_, t, _)| t.clone()).collect();
                    let vals = s.values(&terms);
                    let mut cex = Vec::new();
                    for ((label, _, b), v) in vcs.show.iter().zip(vals) {
                        if let Some(v) = v {
                            cex.push((label.clone(), show_value(&v, b)));
                        }
                    }
                    if ob.kind.is_contract() {
                        model = vcs.inputs.as_ref().and_then(|ins| {
                            let mut out = Vec::new();
                            for (_, leaves) in ins {
                                let terms: Vec<String> = leaves.iter().map(|(t, _)| t.clone()).collect();
                                let vals = s.values(&terms);
                                let mut vs = Vec::new();
                                for v in vals {
                                    let v = v?;
                                    vs.push(v.as_int().or_else(|| v.as_bool().map(|b| b as i128))?);
                                }
                                out.push(vs);
                            }
                            Some(out)
                        });
                    }
                    Status::Failed(cex)
                }
                Sat::Unknown => Status::Unknown(match s.last_error.take() {
                    Some(e) => format!("solver error: {e}"),
                    None => "timeout".into(),
                }),
            };
            if debug {
                eprintln!(";; => {st:?}");
            }
            s.send("(pop 1)");
            st
        };
        obs.push(ObResult {
            kind: ob.kind,
            span: ob.span,
            msg: ob.msg,
            status,
            elidable: ob.elidable,
            replay: None,
            model,
        });
    }
    s.send("(pop 1)");
    obs
}

/// How to make a failed or unknown obligation go through.
pub fn hint(o: &ObResult) -> &'static str {
    let possible = "If the counterexample can happen, the code is wrong; otherwise add the missing fact as a `requires`, an `ensures` on a callee or a loop `invariant`.";
    match o.kind {
        ObKind::Precondition => "Establish it before the call (an `if`, or an `assert` that is proved), or add it to this function's `requires` so callers must.",
        ObKind::Postcondition => "If the counterexample can happen, fix the code or the contract. Otherwise the prover is missing a fact (it sees only this body, its `requires` and its callees' contracts): add it as a `requires`, an `ensures` on a callee, or a loop `invariant`.",
        ObKind::LoopInvariant => {
            if o.msg.ends_with("(on entry)") {
                "It must hold before the first iteration: establish it before the loop."
            } else {
                "The body must re-establish it. The prover knows only the invariants at the top of an iteration, so add the facts the body relies on to the invariant."
            }
        }
        ObKind::StructInvariant | ObKind::Refinement => "Show the new value satisfies the predicate: check it first, or require it of the inputs it is computed from.",
        ObKind::Assert => possible,
        ObKind::Overflow => "Bound the operands (`requires x <= 1_000_000`, a loop invariant), or use `wrapping_add` if wrapping is intended.",
        ObKind::DivZero => "Check the divisor first, or add `requires d != 0`.",
        ObKind::Bounds => "Show `0 <= i < xs.len()`: loop over `0..xs.len()`, check the index first, or add it to `requires`.",
        ObKind::Unwrap => "Match on the value instead, or have its producer `ensures` it is present.",
        ObKind::Panic => "Show this `panic` is unreachable: the branch condition must be impossible given the `requires`.",
        ObKind::Cast | ObKind::Shift => "Bound the value first (a check or a `requires`).",
    }
}

pub fn location(prog: &Program, span: Span) -> String {
    match prog.sm.line_col(span.lo) {
        Some((fid, lc)) => format!("{}:{}:{}", crate::diag::display_path(&prog.sm.file(fid).path), lc.line, lc.col),
        None => "<unknown>".into(),
    }
}

pub fn obligation_json(prog: &Program, f: &FnReport, o: &ObResult) -> Json {
    let mut j = status_json(&o.status);
    j.set("kind", "obligation");
    j.set("obligation", o.kind.name());
    j.set("contract", o.kind.is_contract());
    j.set("function", f.name.as_str());
    j.set("location", location(prog, o.span).as_str());
    j.set("message", o.msg.as_str());
    if o.status != Status::Proved {
        j.set("hint", hint(o));
    }
    match &o.replay {
        Some(Replay::Confirmed(inputs)) => j.set("confirmed_with", inputs.as_str()),
        Some(Replay::NotReproduced) => j.set("reproduced", false),
        None => {}
    }
    j
}

/// The text report: failures and unknown contracts in full, safety obligations counted.
pub fn render(prog: &Program, r: &Report, verbose: bool) -> String {
    let mut out = String::new();
    for f in &r.fns {
        for o in &f.obligations {
            let show = match &o.status {
                Status::Proved => false,
                _ => o.kind.is_contract() || verbose,
            };
            if !show {
                continue;
            }
            let (word, detail) = match &o.status {
                Status::Failed(cex) => {
                    let c: Vec<String> = cex.iter().map(|(k, v)| format!("{k} = {v}")).collect();
                    let word = if o.kind.is_contract() { "FAILED" } else { "may trap" };
                    (word, if c.is_empty() { String::new() } else { format!("\n  counterexample: {}", c.join(", ")) })
                }
                Status::Unknown(why) => ("unknown", format!(" ({why})")),
                Status::Proved => ("proved", String::new()),
            };
            out.push_str(&format!(
                "{word} {} in `{}` at {}: {}{detail}\n",
                o.kind.name(),
                f.name,
                location(prog, o.span),
                o.msg
            ));
            match &o.replay {
                Some(Replay::Confirmed(inputs)) if inputs.is_empty() => {
                    out.push_str("  confirmed: running it fails this check\n")
                }
                Some(Replay::Confirmed(inputs)) => {
                    out.push_str(&format!("  confirmed: running it with {inputs} fails this check\n"))
                }
                Some(Replay::NotReproduced) => out.push_str(
                    "  not reproduced: the counterexample passes at run time, so the prover is missing a fact\n",
                ),
                None => {}
            }
            if o.kind.is_contract() {
                out.push_str(&format!("  hint: {}\n", hint(o)));
            }
        }
    }
    let (cp, ct, sp, stot) = r.counts();
    let cached = r.fns.iter().filter(|f| f.cached).count();
    out.push_str(&format!(
        "proved {cp} of {ct} contract obligations and {sp} of {stot} safety obligations in {} functions ({cached} from cache), {:.2} s\n",
        r.fns.len(),
        r.millis as f64 / 1000.0
    ));
    let (with, proved) = r.contract_coverage(prog);
    if with > 0 {
        out.push_str(&format!(
            "{with} of {} functions have contracts; {proved} of those prove completely\n",
            r.fns.len()
        ));
    }
    out
}
