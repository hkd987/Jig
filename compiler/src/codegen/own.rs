//! Ownership analyses over MIR that let code generation skip reference-count traffic.
//!
//! Values are reference counted and copied on write, so every copy of a local is a retain
//! and every dead local a release. Three facts remove most of that work:
//! - a *borrowed* local aliases part of a value that stays alive and unchanged while the
//!   local is used (a by-value parameter, or a place nothing writes meanwhile), so it needs
//!   no count of its own;
//! - the *last use* of an owned local can move the value out instead of retaining it (the
//!   local is cleared, so its later release does nothing);
//! - a *unique* list, made unique where it is defined, stays unique (see [`unique_lists`]).

use crate::mir::intrinsics::Op;
use crate::mir::*;
use crate::sema::defs::Mode;
use crate::sema::ty::{TyCtx, TyKind};
use crate::util::{FxHashMap, FxHashSet};

fn place_uses(p: &Place, out: &mut Vec<Local>) {
    out.push(p.local);
    for e in &p.proj {
        if let PlaceElem::Index(op, _) = e {
            operand_uses(op, out);
        }
    }
}

fn operand_uses(op: &Operand, out: &mut Vec<Local>) {
    if let Operand::Copy(p) = op {
        place_uses(p, out);
    }
}

/// The locals a statement reads (with repeats), and the local it assigns whole, if any.
fn stmt_uses(stmt: &Stmt, out: &mut Vec<Local>) -> Option<Local> {
    match stmt {
        Stmt::Assign(p, rv) => {
            if !p.proj.is_empty() {
                // Writing part of a local reads the rest of it.
                place_uses(p, out);
            }
            match rv {
                Rvalue::Use(a) | Rvalue::Unary(_, a, _) | Rvalue::Cast(a, _, _) | Rvalue::Inc(a, _) => {
                    operand_uses(a, out)
                }
                Rvalue::Binary(_, a, b, _) => {
                    operand_uses(a, out);
                    operand_uses(b, out);
                }
                Rvalue::Aggregate(_, _, ops) | Rvalue::Closure(_, ops) => ops.iter().for_each(|o| operand_uses(o, out)),
                Rvalue::Discriminant(q) => place_uses(q, out),
                Rvalue::Call(callee, args, _) => {
                    if let Callee::Value(f) = callee {
                        operand_uses(f, out);
                    }
                    for a in args {
                        match a {
                            Arg::Value(o) => operand_uses(o, out),
                            Arg::MutRef(q) => place_uses(q, out),
                        }
                    }
                }
            }
            p.proj.is_empty().then_some(p.local)
        }
        Stmt::Check { cond, values, .. } => {
            operand_uses(cond, out);
            values.iter().for_each(|(_, o, _)| operand_uses(o, out));
            None
        }
        Stmt::Drop(_) => None,
    }
}

fn term_uses(t: &Terminator, out: &mut Vec<Local>) {
    match t {
        Terminator::If(c, _, _) | Terminator::Switch(c, _, _) => operand_uses(c, out),
        Terminator::Return => out.push(Local(0)),
        Terminator::Goto(_) | Terminator::Unreachable => {}
    }
}

fn successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto(b) => vec![*b],
        Terminator::If(_, a, b) => vec![*a, *b],
        Terminator::Switch(_, cases, d) => cases.iter().map(|(_, b)| *b).chain([*d]).collect(),
        Terminator::Return | Terminator::Unreachable => vec![],
    }
}

/// Locals that alias what they were copied from instead of counting their own reference:
/// match scrutinees and pattern bindings, typically. Each is only ever assigned whole, from
/// a place (never written in part or passed `&mut`). It is borrowed when what it was copied
/// from (its *root*, through other borrowed locals) stays alive and unchanged while it is
/// used: always for a by-value parameter, which the caller keeps alive and nobody changes;
/// for any other root (`rc` locals only), when no path from a copy to a use of the local
/// writes, drops or may move the root. Each borrowed local maps to its roots.
pub fn borrowed_locals(mf: &MFn, rc: &FxHashSet<Local>) -> FxHashMap<Local, Vec<Local>> {
    let params: FxHashSet<Local> = mf.params.iter().map(|(l, _)| *l).collect();
    let by_value: FxHashSet<Local> = mf.params.iter().filter(|(_, m)| *m != Mode::MutRef).map(|(l, _)| *l).collect();
    let mut sources: FxHashMap<Local, Vec<Option<Local>>> = FxHashMap::default();
    let mut bad: FxHashSet<Local> = FxHashSet::default();
    for block in mf.blocks.iter() {
        for (stmt, _) in &block.stmts {
            let Stmt::Assign(p, rv) = stmt else { continue };
            if !p.proj.is_empty() {
                bad.insert(p.local);
            } else {
                let src = match rv {
                    Rvalue::Use(Operand::Copy(q)) if q.local != p.local => Some(q.local),
                    _ => None,
                };
                sources.entry(p.local).or_default().push(src);
            }
            if let Rvalue::Call(_, args, _) = rv {
                for a in args {
                    if let Arg::MutRef(q) = a {
                        bad.insert(q.local);
                    }
                }
            }
        }
    }
    // Candidates: not the return slot or a parameter, never written in part, every
    // assignment a copy of a place.
    let mut cands: Vec<(Local, Vec<Local>)> = sources
        .iter()
        .filter(|(l, srcs)| l.0 != 0 && !params.contains(l) && !bad.contains(l) && srcs.iter().all(|s| s.is_some()))
        .map(|(l, srcs)| (*l, srcs.iter().map(|s| s.unwrap()).collect()))
        .collect();
    cands.sort_by_key(|(l, _)| *l);
    let is_cand: FxHashSet<Local> = cands.iter().map(|(l, _)| *l).collect();
    let mut wcx: Option<WindowCx> = None;
    // Each local's roots once decided (`None`: not borrowed); a candidate is decided once
    // its sources are.
    let mut roots: FxHashMap<Local, Option<Vec<Local>>> = FxHashMap::default();
    loop {
        let mut progress = false;
        for (l, srcs) in &cands {
            if roots.contains_key(l) || srcs.iter().any(|s| is_cand.contains(s) && !roots.contains_key(s)) {
                continue;
            }
            let mut rs: Vec<Local> = Vec::new();
            for s in srcs {
                match roots.get(s) {
                    Some(Some(sr)) => rs.extend(sr.iter().copied()),
                    _ => rs.push(*s),
                }
            }
            rs.sort();
            rs.dedup();
            let stable: Vec<Local> = rs.iter().copied().filter(|r| !by_value.contains(r)).collect();
            let ok = stable.is_empty()
                || (rc.contains(l) && {
                    let cx = wcx.get_or_insert_with(|| WindowCx::new(mf, &is_cand, rc));
                    window_clean(mf, cx, &params, *l, &stable)
                });
            roots.insert(*l, ok.then_some(rs));
            progress = true;
        }
        if !progress {
            break;
        }
    }
    roots.into_iter().filter_map(|(l, r)| r.map(|r| (l, r))).collect()
}

/// Whether a statement may write, drop or move one of `roots`: assign it (in whole or part),
/// pass it `&mut`, drop it, or read it whole (a last use moves the value out; a parameter is
/// never moved).
fn writes_any(stmt: &Stmt, roots: &[Local], params: &FxHashSet<Local>) -> bool {
    let whole = |op: &Operand| matches!(op, Operand::Copy(q) if q.proj.is_empty() && !params.contains(&q.local) && roots.contains(&q.local));
    match stmt {
        Stmt::Assign(p, rv) => {
            roots.contains(&p.local)
                || match rv {
                    Rvalue::Use(a) | Rvalue::Unary(_, a, _) | Rvalue::Cast(a, _, _) | Rvalue::Inc(a, _) => whole(a),
                    Rvalue::Binary(_, a, b, _) => whole(a) || whole(b),
                    Rvalue::Aggregate(_, _, ops) | Rvalue::Closure(_, ops) => ops.iter().any(whole),
                    Rvalue::Discriminant(_) => false,
                    Rvalue::Call(callee, args, _) => {
                        matches!(callee, Callee::Value(f) if whole(f))
                            || args.iter().any(|a| match a {
                                Arg::Value(o) => whole(o),
                                Arg::MutRef(q) => roots.contains(&q.local),
                            })
                    }
                }
        }
        Stmt::Check { .. } => false,
        Stmt::Drop(l) => roots.contains(l),
    }
}

/// What `window_clean` needs, built once per function when a candidate first needs it: where
/// each `rc` candidate is read (statement index, or the block's length for its terminator),
/// and each block's predecessors.
struct WindowCx {
    uses_at: FxHashMap<Local, Vec<(usize, usize)>>,
    preds: Vec<Vec<usize>>,
}

impl WindowCx {
    fn new(mf: &MFn, cands: &FxHashSet<Local>, rc: &FxHashSet<Local>) -> WindowCx {
        let mut uses_at: FxHashMap<Local, Vec<(usize, usize)>> = FxHashMap::default();
        let mut preds: Vec<Vec<usize>> = vec![Vec::new(); mf.blocks.len()];
        let mut us = Vec::new();
        for (b, block) in mf.blocks.iter().enumerate() {
            for (k, (stmt, _)) in block.stmts.iter().enumerate() {
                us.clear();
                stmt_uses(stmt, &mut us);
                for &l in &us {
                    if cands.contains(&l) && rc.contains(&l) {
                        uses_at.entry(l).or_default().push((b, k));
                    }
                }
            }
            us.clear();
            term_uses(&block.term, &mut us);
            for &l in &us {
                if cands.contains(&l) && rc.contains(&l) {
                    uses_at.entry(l).or_default().push((b, block.stmts.len()));
                }
            }
            for s in successors(&block.term) {
                preds[s.0 as usize].push(b);
            }
        }
        WindowCx { uses_at, preds }
    }
}

/// Whether no path from an assignment of `l` to a use of it passes a statement that writes
/// one of `roots` (a statement that both uses `l` and writes a root counts too). Searches
/// backwards from each use until the assignments.
fn window_clean(mf: &MFn, cx: &WindowCx, params: &FxHashSet<Local>, l: Local, roots: &[Local]) -> bool {
    let stmt = |b: usize, k: usize| &mf.blocks[BlockId(b as u32)].stmts[k].0;
    let mut visited = vec![false; mf.blocks.len()];
    // (block, k): look at the statements before the k-th.
    let mut work: Vec<(usize, usize)> = Vec::new();
    for &(b, k) in cx.uses_at.get(&l).map(|v| v.as_slice()).unwrap_or(&[]) {
        if k < mf.blocks[BlockId(b as u32)].stmts.len() && writes_any(stmt(b, k), roots, params) {
            return false;
        }
        work.push((b, k));
    }
    while let Some((b, k)) = work.pop() {
        let mut i = k;
        let mut defined = false;
        while i > 0 {
            i -= 1;
            if matches!(stmt(b, i), Stmt::Assign(p, _) if p.local == l) {
                defined = true;
                break;
            }
            if writes_any(stmt(b, i), roots, params) {
                return false;
            }
        }
        if !defined {
            for &p in &cx.preds[b] {
                if !visited[p] {
                    visited[p] = true;
                    work.push((p, mf.blocks[BlockId(p as u32)].stmts.len()));
                }
            }
        }
    }
    true
}

/// For each statement, the locals whose value it may move out: read exactly once there and
/// dead afterwards. `owners` are the locals that own their values (not parameters or
/// borrowed locals, not the return slot).
pub fn last_uses(mf: &MFn, owners: &FxHashSet<Local>) -> Vec<Vec<FxHashSet<Local>>> {
    let n = mf.blocks.len();
    let mut uses_b: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut defs_b: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    for (b, block) in mf.blocks.iter_enumerated() {
        let (u, d) = (&mut uses_b[b.0 as usize], &mut defs_b[b.0 as usize]);
        // Backward: a use before (above) a def is a use of the incoming value.
        let mut t = Vec::new();
        term_uses(&block.term, &mut t);
        u.extend(t);
        for (stmt, _) in block.stmts.iter().rev() {
            let mut us = Vec::new();
            if let Some(l) = stmt_uses(stmt, &mut us) {
                u.remove(&l);
                d.insert(l);
            }
            u.extend(us);
        }
    }
    let succ: Vec<Vec<BlockId>> = mf.blocks.iter().map(|b| successors(&b.term)).collect();
    let mut live_in: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            let mut out: FxHashSet<Local> = FxHashSet::default();
            for s in &succ[b] {
                out.extend(live_in[s.0 as usize].iter().copied());
            }
            let mut inn: FxHashSet<Local> = out.into_iter().filter(|l| !defs_b[b].contains(l)).collect();
            inn.extend(uses_b[b].iter().copied());
            if inn.len() != live_in[b].len() {
                live_in[b] = inn;
                changed = true;
            }
        }
    }
    let mut result = Vec::with_capacity(n);
    for (b, block) in mf.blocks.iter_enumerated() {
        let mut live: FxHashSet<Local> = FxHashSet::default();
        for s in &succ[b.0 as usize] {
            live.extend(live_in[s.0 as usize].iter().copied());
        }
        let mut t = Vec::new();
        term_uses(&block.term, &mut t);
        live.extend(t);
        let mut per = vec![FxHashSet::default(); block.stmts.len()];
        for (i, (stmt, _)) in block.stmts.iter().enumerate().rev() {
            let mut us = Vec::new();
            let def = stmt_uses(stmt, &mut us);
            let mut counts: FxHashMap<Local, usize> = FxHashMap::default();
            for &l in &us {
                *counts.entry(l).or_insert(0) += 1;
            }
            per[i] = counts
                .iter()
                .filter(|(l, c)| **c == 1 && owners.contains(l) && !live.contains(l))
                .map(|(l, _)| *l)
                .collect();
            if let Some(l) = def {
                live.remove(&l);
            }
            live.extend(us);
        }
        result.push(per);
    }
    result
}

/// For each statement, the owned locals to release after it: the ones it reads or assigns
/// that are dead afterwards, where reading a borrowed local reads its roots too (a root
/// stays alive while what borrows from it is used). Values then die at their last use, so
/// a value they shared is not copied when it is changed next.
pub fn dead_after(mf: &MFn, owners: &FxHashSet<Local>, roots: &FxHashMap<Local, Vec<Local>>) -> Vec<Vec<Vec<Local>>> {
    let with_roots = |us: &mut Vec<Local>| {
        for i in 0..us.len() {
            if let Some(rs) = roots.get(&us[i]) {
                us.extend(rs.iter().copied());
            }
        }
    };
    let n = mf.blocks.len();
    let mut uses_b: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut defs_b: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut term_b: Vec<Vec<Local>> = Vec::with_capacity(n);
    for (b, block) in mf.blocks.iter_enumerated() {
        let (u, d) = (&mut uses_b[b.0 as usize], &mut defs_b[b.0 as usize]);
        let mut t = Vec::new();
        term_uses(&block.term, &mut t);
        with_roots(&mut t);
        u.extend(t.iter().copied());
        term_b.push(t);
        for (stmt, _) in block.stmts.iter().rev() {
            let mut us = Vec::new();
            let def = stmt_uses(stmt, &mut us);
            with_roots(&mut us);
            if let Some(l) = def {
                u.remove(&l);
                d.insert(l);
            }
            u.extend(us);
        }
    }
    let succ: Vec<Vec<BlockId>> = mf.blocks.iter().map(|b| successors(&b.term)).collect();
    let mut live_in: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for b in (0..n).rev() {
            let mut out: FxHashSet<Local> = FxHashSet::default();
            for s in &succ[b] {
                out.extend(live_in[s.0 as usize].iter().copied());
            }
            let mut inn: FxHashSet<Local> = out.into_iter().filter(|l| !defs_b[b].contains(l)).collect();
            inn.extend(uses_b[b].iter().copied());
            if inn.len() != live_in[b].len() {
                live_in[b] = inn;
                changed = true;
            }
        }
    }
    let mut result = Vec::with_capacity(n);
    for (b, block) in mf.blocks.iter_enumerated() {
        let mut live: FxHashSet<Local> = FxHashSet::default();
        for s in &succ[b.0 as usize] {
            live.extend(live_in[s.0 as usize].iter().copied());
        }
        live.extend(term_b[b.0 as usize].iter().copied());
        let mut per = vec![Vec::new(); block.stmts.len()];
        for (i, (stmt, _)) in block.stmts.iter().enumerate().rev() {
            let mut us = Vec::new();
            let def = stmt_uses(stmt, &mut us);
            with_roots(&mut us);
            let mut dead: Vec<Local> =
                us.iter().copied().chain(def).filter(|l| owners.contains(l) && !live.contains(l)).collect();
            dead.sort();
            dead.dedup();
            per[i] = dead;
            if let Some(l) = def {
                live.remove(&l);
            }
            live.extend(us);
        }
        result.push(per);
    }
    result
}

/// For each block, the owned locals that may hold a value when its terminator runs: assigned
/// (or passed `&mut`) on some path since they were last released. Every local starts empty,
/// so a function's return releases only these.
pub fn held_at_end(mf: &MFn, owners: &FxHashSet<Local>, dead: &[Vec<Vec<Local>>]) -> Vec<FxHashSet<Local>> {
    let n = mf.blocks.len();
    let succ: Vec<Vec<BlockId>> = mf.blocks.iter().map(|b| successors(&b.term)).collect();
    let mut held_in: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut held_out: Vec<FxHashSet<Local>> = vec![FxHashSet::default(); n];
    let mut changed = true;
    while changed {
        changed = false;
        for b in 0..n {
            let mut h = held_in[b].clone();
            for (i, (stmt, _)) in mf.blocks[BlockId(b as u32)].stmts.iter().enumerate() {
                match stmt {
                    Stmt::Assign(p, rv) => {
                        if owners.contains(&p.local) {
                            h.insert(p.local);
                        }
                        if let Rvalue::Call(_, args, _) = rv {
                            for a in args {
                                if let Arg::MutRef(q) = a {
                                    if owners.contains(&q.local) {
                                        h.insert(q.local);
                                    }
                                }
                            }
                        }
                    }
                    Stmt::Drop(l) => {
                        h.remove(l);
                    }
                    Stmt::Check { .. } => {}
                }
                for l in &dead[b][i] {
                    h.remove(l);
                }
            }
            // Sets only grow, so a change shows in the size.
            if h.len() != held_out[b].len() {
                for s in &succ[b] {
                    held_in[s.0 as usize].extend(h.iter().copied());
                }
                held_out[b] = h;
                changed = true;
            }
        }
    }
    held_out
}

/// Lists that stay unique once made so: they are written through indices, and otherwise used
/// only by intrinsics that read them in place or keep them unique (`len`, `push`, `swap`, ...).
/// Nothing copies one whole or passes it `&mut` to a function that could replace it, so after
/// it is made unique where it is defined (at entry for `&mut` parameters, after each
/// assignment for locals) its element writes can skip the copy-on-write check.
pub fn unique_lists(mf: &MFn, tys: &TyCtx) -> FxHashSet<Local> {
    fn whole(op: &Operand, bad: &mut FxHashSet<Local>) {
        if let Operand::Copy(p) = op {
            if p.proj.is_empty() {
                bad.insert(p.local);
            }
        }
    }
    let reads_in_place =
        |op: Op| matches!(op, Op::ListLen | Op::ListIsEmpty | Op::Get | Op::ListSlice | Op::ListRepeat | Op::Show);
    let mut written: FxHashSet<Local> = FxHashSet::default();
    let mut bad: FxHashSet<Local> = FxHashSet::default();
    for block in mf.blocks.iter() {
        for (stmt, _) in &block.stmts {
            let Stmt::Assign(p, rv) = stmt else { continue };
            if matches!(p.proj.first(), Some(PlaceElem::Index(..))) {
                written.insert(p.local);
            }
            match rv {
                Rvalue::Use(a) | Rvalue::Unary(_, a, _) | Rvalue::Cast(a, _, _) | Rvalue::Inc(a, _) => {
                    whole(a, &mut bad)
                }
                Rvalue::Binary(_, a, b, _) => {
                    whole(a, &mut bad);
                    whole(b, &mut bad);
                }
                Rvalue::Aggregate(_, _, ops) | Rvalue::Closure(_, ops) => ops.iter().for_each(|o| whole(o, &mut bad)),
                Rvalue::Discriminant(_) => {}
                Rvalue::Call(callee, args, _) => {
                    let intr = match callee {
                        Callee::Intrinsic(i) => Some(i.op),
                        Callee::Value(f) => {
                            whole(f, &mut bad);
                            None
                        }
                        Callee::Fn(_) | Callee::Ffi(_) => None,
                    };
                    for a in args {
                        match (a, intr) {
                            (Arg::Value(_), Some(op)) if reads_in_place(op) => {}
                            (Arg::Value(o), _) => whole(o, &mut bad),
                            // List intrinsics taking `&mut` keep a unique list unique.
                            (Arg::MutRef(_), Some(_)) => {}
                            (Arg::MutRef(p), None) if p.proj.is_empty() => {
                                bad.insert(p.local);
                            }
                            (Arg::MutRef(p), None) => {
                                if matches!(p.proj.first(), Some(PlaceElem::Index(..))) {
                                    written.insert(p.local);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let by_value: FxHashSet<Local> = mf.params.iter().filter(|(_, m)| *m != Mode::MutRef).map(|(l, _)| *l).collect();
    written
        .into_iter()
        .filter(|l| {
            l.0 != 0
                && !bad.contains(l)
                && !by_value.contains(l)
                && matches!(tys.kind(mf.locals[*l].ty), TyKind::List(_))
        })
        .collect()
}
