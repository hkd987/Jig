//! Ownership analyses over MIR that let code generation skip reference-count traffic.
//!
//! Values are reference counted and copied on write, so every copy of a local is a retain
//! and every dead local a release. Three facts remove most of that work:
//! - a *borrowed* local only ever holds a part of a by-value parameter, which the caller
//!   keeps alive and nobody changes for the whole call, so it needs no count of its own;
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

/// Locals that only ever hold a part of a by-value parameter (or of another such local):
/// match scrutinees and pattern bindings, typically. They are never written in part or
/// passed `&mut`, so they can alias what they were copied from without a count.
pub fn borrowed_locals(mf: &MFn) -> FxHashSet<Local> {
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
                    Rvalue::Use(Operand::Copy(q)) => Some(q.local),
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
    let mut ok: FxHashSet<Local> =
        by_value.iter().copied().filter(|l| !sources.contains_key(l) && !bad.contains(l)).collect();
    let mut borrowed: FxHashSet<Local> = FxHashSet::default();
    loop {
        let more: Vec<Local> = sources
            .iter()
            .filter(|(l, srcs)| {
                l.0 != 0
                    && !bad.contains(l)
                    && !ok.contains(l)
                    && srcs.iter().all(|s| s.is_some_and(|r| ok.contains(&r) && r != **l))
            })
            .map(|(l, _)| *l)
            .collect();
        if more.is_empty() {
            return borrowed;
        }
        for l in more {
            ok.insert(l);
            borrowed.insert(l);
        }
    }
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
