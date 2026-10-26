//! `jig trace`: which scenarios exercise which code. The call graph is complete (no
//! reflection or dynamic loading), so reachability from a scenario's steps is exact up to
//! which branches a run takes.

use crate::sema::defs::Program;
use crate::sema::ty::FnId;
use crate::util::json::Json;
use crate::util::{FxHashSet, Symbol};

pub struct Trace {
    /// Per scenario: its label and every user function its steps (and world) can reach.
    pub scenarios: Vec<(String, FxHashSet<FnId>)>,
    /// Reachable from `main` (or a library's public API).
    pub from_entry: FxHashSet<FnId>,
    /// Every user function with a body.
    pub all: Vec<FnId>,
}

fn reachable(prog: &Program, starts: impl IntoIterator<Item = FnId>) -> FxHashSet<FnId> {
    let mut seen: FxHashSet<FnId> = FxHashSet::default();
    let mut dyn_names: FxHashSet<Symbol> = FxHashSet::default();
    let mut stack: Vec<FnId> = starts.into_iter().collect();
    loop {
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            if let Some(b) = prog.bodies[f].as_ref() {
                stack.extend(b.refs.iter().copied());
                dyn_names.extend(b.dyn_methods.iter().copied());
            }
        }
        // Calls through generic bounds reach every method with that name.
        let more: Vec<FnId> = prog
            .defs
            .methods
            .iter()
            .filter(|((_, n), f)| dyn_names.contains(n) && !seen.contains(f))
            .map(|(_, &f)| f)
            .collect();
        if more.is_empty() {
            break;
        }
        stack.extend(more);
    }
    seen
}

pub fn trace(prog: &Program) -> Trace {
    let user = |f: &FnId| !prog.pkgs[prog.defs.fns[*f].pkg].is_std;
    let mut scenarios = Vec::new();
    for sc in &prog.scenarios {
        let mut starts: Vec<FnId> = sc.steps.iter().map(|s| s.f).collect();
        if let Some((_, new)) = sc.world {
            starts.push(new);
        }
        let r: FxHashSet<FnId> = reachable(prog, starts).into_iter().filter(user).collect();
        scenarios.push((format!("{}: {}", sc.feature, sc.name), r));
    }
    let entry: Vec<FnId> = match prog.defs.main {
        Some(m) => vec![m],
        None => prog
            .defs
            .fns
            .iter_enumerated()
            .filter(|(_, fd)| fd.pkg == prog.root && fd.is_pub && fd.iface.is_none())
            .map(|(f, _)| f)
            .collect(),
    };
    let from_entry: FxHashSet<FnId> = reachable(prog, entry).into_iter().filter(user).collect();
    let mut all: Vec<FnId> = prog
        .defs
        .fns
        .iter_enumerated()
        .filter(|(f, fd)| user(f) && fd.has_body && fd.iface.is_none())
        .map(|(f, _)| f)
        .collect();
    all.sort_by_key(|f| prog.defs.fns[*f].span.lo);
    Trace { scenarios, from_entry, all }
}

fn name(prog: &Program, f: FnId) -> String {
    prog.defs.fns[f].display_name(&prog.defs, &prog.tys)
}

fn loc(prog: &Program, f: FnId) -> String {
    crate::verify::location(prog, prog.defs.fns[f].name_span)
}

fn fn_json(prog: &Program, f: FnId) -> Json {
    Json::obj().with("function", name(prog, f).as_str()).with("location", loc(prog, f).as_str())
}

pub enum Query {
    /// Coverage per scenario, and what no scenario covers.
    Summary,
    /// Scenarios that exercise functions whose name contains this.
    Function(String),
    /// What one scenario (matched by name) exercises, and what dies without it.
    Scenario(String),
    /// Code reachable from the entrypoint that no scenario exercises.
    Uncovered,
}

pub fn run(prog: &Program, q: &Query) -> (String, Json) {
    let t = trace(prog);
    let covered: FxHashSet<FnId> = t.scenarios.iter().flat_map(|(_, r)| r.iter().copied()).collect();
    let uncovered: Vec<FnId> =
        t.all.iter().copied().filter(|f| t.from_entry.contains(f) && !covered.contains(f)).collect();
    match q {
        Query::Summary => {
            let mut text = format!("{} scenarios\n", t.scenarios.len());
            let mut arr = Json::arr();
            for (label, r) in &t.scenarios {
                text.push_str(&format!("  {label}: exercises {} functions\n", r.len()));
                arr.push(Json::obj().with("scenario", label.as_str()).with("functions", r.len() as i64));
            }
            text.push_str(&format!(
                "{} of {} functions reachable from the entrypoint are covered by no scenario{}\n",
                uncovered.len(),
                t.all.iter().filter(|f| t.from_entry.contains(f)).count(),
                if uncovered.is_empty() { "" } else { " (jig trace uncovered)" }
            ));
            let j = Json::obj().with("scenarios", arr).with("uncovered", uncovered.len() as i64);
            (text, j)
        }
        Query::Uncovered => {
            let mut text = String::new();
            for &f in &uncovered {
                text.push_str(&format!("{}  {}\n", name(prog, f), loc(prog, f)));
            }
            if text.is_empty() {
                text.push_str("every function reachable from the entrypoint is covered by a scenario\n");
            }
            (text, Json::Arr(uncovered.iter().map(|&f| fn_json(prog, f)).collect()))
        }
        Query::Function(q) => {
            let fs: Vec<FnId> = t.all.iter().copied().filter(|&f| name(prog, f).contains(q.as_str())).collect();
            let mut text = String::new();
            let mut arr = Json::arr();
            for f in fs {
                let scs: Vec<&str> =
                    t.scenarios.iter().filter(|(_, r)| r.contains(&f)).map(|(l, _)| l.as_str()).collect();
                text.push_str(&format!("{} ({}):\n", name(prog, f), loc(prog, f)));
                if scs.is_empty() {
                    text.push_str("  no scenario exercises it\n");
                }
                for s in &scs {
                    text.push_str(&format!("  {s}\n"));
                }
                arr.push(fn_json(prog, f).with("scenarios", Json::Arr(scs.iter().map(|s| Json::from(*s)).collect())));
            }
            if text.is_empty() {
                text = format!("no function matches `{q}`\n");
            }
            (text, arr)
        }
        Query::Scenario(q) => {
            let Some(i) = t.scenarios.iter().position(|(l, _)| l.contains(q.as_str())) else {
                return (format!("no scenario matches `{q}`\n"), Json::Null);
            };
            let (label, mine) = &t.scenarios[i];
            // What survives without it: the entrypoint and every other scenario.
            let mut others: FxHashSet<FnId> = t.from_entry.clone();
            for (j, (_, r)) in t.scenarios.iter().enumerate() {
                if j != i {
                    others.extend(r.iter().copied());
                }
            }
            let mut exercised: Vec<FnId> = mine.iter().copied().collect();
            exercised.sort_by_key(|f| prog.defs.fns[*f].span.lo);
            let dies: Vec<FnId> = exercised.iter().copied().filter(|f| !others.contains(f)).collect();
            let mut text = format!("{label}\n  exercises {} functions\n", exercised.len());
            if dies.is_empty() {
                text.push_str("  deleting it leaves no dead code\n");
            } else {
                text.push_str("  deleting it makes these dead:\n");
                for &f in &dies {
                    text.push_str(&format!("    {}  {}\n", name(prog, f), loc(prog, f)));
                }
            }
            let j = Json::obj()
                .with("scenario", label.as_str())
                .with("exercises", Json::Arr(exercised.iter().map(|&f| fn_json(prog, f)).collect()))
                .with("dies_without_it", Json::Arr(dies.iter().map(|&f| fn_json(prog, f)).collect()));
            (text, j)
        }
    }
}
