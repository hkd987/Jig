//! Statements, blocks, loops, patterns and `match`.

use super::{Ctx, Cx};
use crate::diag::{codes, Diagnostic};
use crate::sema::defs::*;
use crate::sema::exhaust;
use crate::sema::ty::*;
use crate::syntax::ast::{self, BinOp, ExprId, ExprKind, ForHead, PatId, PatKind, StmtId, StmtKind};
use crate::util::{List, Symbol};

impl Cx<'_> {
    /// Check a block whose value is discarded.
    pub(crate) fn check_block_stmts(&mut self, block: ExprId) {
        let ExprKind::Block(stmts) = self.ast.exprs[block].kind else {
            self.check_expr_stmt(block);
            return;
        };
        self.push_scope();
        let ids: Vec<StmtId> = self.ast.stmt_lists.get(stmts).to_vec();
        let mut reported = false;
        for s in ids {
            self.unreachable_check(s, &mut reported);
            self.check_stmt(s);
        }
        self.pop_scope();
        let t = if self.diverges { self.tys.never } else { self.tys.unit };
        self.record(block, t);
    }

    /// Check a block whose last expression statement is its value.
    pub(crate) fn check_block_value(&mut self, block: ExprId, expected: Option<Ty>) -> Ty {
        let ExprKind::Block(stmts) = self.ast.exprs[block].kind else {
            return self.check_expr(block, expected);
        };
        self.push_scope();
        let ids: Vec<StmtId> = self.ast.stmt_lists.get(stmts).to_vec();
        let mut reported = false;
        let mut value = self.tys.unit;
        let n = ids.len();
        for (i, s) in ids.into_iter().enumerate() {
            self.unreachable_check(s, &mut reported);
            if i + 1 == n {
                if let StmtKind::Expr(e) = self.ast.stmts[s].kind {
                    value = self.check_expr(e, expected);
                    let r = self.resolve(value);
                    if self.tys.is_never(r) {
                        self.diverges = true;
                    }
                    continue;
                }
            }
            self.check_stmt(s);
        }
        self.pop_scope();
        if self.diverges {
            return self.tys.never;
        }
        if n == 0 || !matches!(self.ast.stmts[*self.ast.stmt_lists.get(stmts).last().unwrap()].kind, StmtKind::Expr(_)) {
            if let Some(exp) = expected {
                let r = self.resolve(exp);
                if !self.tys.is_unit(r) && !matches!(self.tys.kind(r), TyKind::Infer(_) | TyKind::Error) {
                    let span = self.ast.exprs[block].span;
                    let es = self.show(exp);
                    self.push(
                        Diagnostic::error(codes::E0301, span, format!("this block has no value, but `{es}` is expected"))
                            .note("end the block with an expression"),
                    );
                    return exp;
                }
            }
        }
        value
    }

    fn unreachable_check(&mut self, s: StmtId, reported: &mut bool) {
        if self.diverges && !*reported {
            *reported = true;
            let span = self.ast.stmts[s].span;
            self.push(
                Diagnostic::error(codes::E0506, span, "unreachable code")
                    .note("it comes after a `return`, `break`, `continue` or `panic`"),
            );
        }
    }

    pub(crate) fn check_stmt(&mut self, s: StmtId) {
        let st = self.ast.stmts[s];
        match st.kind {
            StmtKind::Let { pat, ty, init, mutable } => {
                let ann = ty.map(|t| self.resolve_type_expr(t));
                let t = self.check_expr(init, ann);
                let t = ann.unwrap_or(t);
                let r = self.resolve(t);
                if self.tys.is_never(r) {
                    self.diverges = true;
                }
                if self.tys.is_unit(r) && matches!(self.ast.pats[pat].kind, PatKind::Bind(_)) {
                    let span = self.ast.exprs[init].span;
                    self.push(
                        Diagnostic::error(codes::E0301, span, "this expression has no value (its type is `()`)")
                            .note("call it as a statement instead of binding its result"),
                    );
                }
                let kind = if mutable { LocalKind::Var } else { LocalKind::Let };
                self.check_pat_irrefutable(pat, t, kind);
            }
            StmtKind::Assign { target, op, value } => {
                let tt = self.check_place_for_write(target, op.is_some());
                match op {
                    None => {
                        self.check_expr(value, Some(tt));
                        if let (ExprKind::Name(a), ExprKind::Name(b)) = (self.ast.exprs[target].kind, self.ast.exprs[value].kind) {
                            if a == b {
                                self.err(codes::E0508, st.span, format!("`{a}` is assigned to itself"));
                            }
                        }
                    }
                    Some(op) => self.check_compound(st.span, op, target, tt, value),
                }
            }
            StmtKind::Expr(e) => self.check_expr_stmt(e),
            StmtKind::Return(v) => {
                if self.ctx != Ctx::Code {
                    self.err(codes::E0321, st.span, "`return` cannot appear in a contract");
                }
                let ret = match self.closures.last() {
                    Some(f) => f.ret,
                    None => self.ret_ty,
                };
                match v {
                    Some(e) => {
                        self.check_expr(e, Some(ret));
                    }
                    None => {
                        let r = self.resolve(ret);
                        if !self.tys.is_unit(r) && !matches!(self.tys.kind(r), TyKind::Infer(_) | TyKind::Error) {
                            let rs = self.show(ret);
                            self.err(codes::E0308, st.span, format!("`return` needs a value of type `{rs}`"));
                        } else if matches!(self.tys.kind(r), TyKind::Infer(_)) {
                            let u = self.tys.unit;
                            let _ = self.unify(r, u);
                        }
                    }
                }
                self.diverges = true;
            }
            StmtKind::Break | StmtKind::Continue => {
                if !self.in_loop() {
                    let kw = if matches!(st.kind, StmtKind::Break) { "break" } else { "continue" };
                    self.err(codes::E0316, st.span, format!("`{kw}` outside of a loop"));
                } else if matches!(st.kind, StmtKind::Break) {
                    if let Some(b) = self.loop_breaks.last_mut() {
                        *b = true;
                    }
                }
                self.diverges = true;
            }
            StmtKind::For { head, invariants, body } => self.check_for(s, head, invariants, body),
        }
    }

    /// Check the target of an assignment. Plain `=` does not read the target.
    fn check_place_for_write(&mut self, target: ExprId, reads: bool) -> Ty {
        let root = self.place_root(target);
        let was_used = root.map(|r| self.body.locals[r].used);
        let t = self.check_expr(target, None);
        if !reads {
            // Writing through a place reads its base unless the place is the variable itself.
            if let (Some(r), Some(u)) = (root, was_used) {
                if matches!(self.ast.exprs[target].kind, ExprKind::Name(_)) {
                    self.body.locals[r].used = u;
                }
            }
        }
        self.require_mut_place(target, "assign to");
        t
    }

    fn check_compound(&mut self, span: crate::diag::Span, op: BinOp, target: ExprId, tt: Ty, value: ExprId) {
        let r = self.resolve(tt);
        match op {
            BinOp::Add if matches!(self.tys.kind(r), TyKind::Str) => {
                let s = self.tys.str;
                self.check_expr(value, Some(s));
            }
            BinOp::Add | BinOp::Sub | BinOp::Rem | BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let numeric = self.is_numeric_or_lit(r);
                if !numeric {
                    let ts = self.show(tt);
                    self.err(codes::E0306, span, format!("`{}=` needs a number, found `{ts}`", op.as_str()));
                }
                self.check_expr(value, Some(tt));
                if op.is_bitwise() && matches!(self.tys.kind(r), TyKind::Float(_)) {
                    self.err(codes::E0306, span, "bitwise operators need integers");
                }
            }
            BinOp::Mul | BinOp::Div => {
                // The value must be dimensionless for the target's unit to stay the same.
                let plain = self.tys.with_unit(r, NO_UNIT);
                if !self.is_numeric_or_lit(r) {
                    let ts = self.show(tt);
                    self.err(codes::E0306, span, format!("`{}=` needs a number, found `{ts}`", op.as_str()));
                }
                self.check_expr(value, Some(plain));
            }
            BinOp::Shl | BinOp::Shr => {
                let vt = self.check_expr(value, None);
                let vr = self.resolve(vt);
                if !self.is_int_or_lit(r) || !self.is_int_or_lit(vr) {
                    self.err(codes::E0306, span, "shifts need integers");
                }
            }
            _ => {
                self.check_expr(value, None);
            }
        }
        let _ = target;
    }

    /// A number, a still-unresolved numeric literal, or an error (already reported).
    pub(crate) fn is_numeric_or_lit(&self, r: Ty) -> bool {
        match self.tys.kind(r) {
            TyKind::Int(..) | TyKind::Float(_) | TyKind::Error => true,
            TyKind::Infer(v) => self.vars[*v as usize].1 != super::VarKind::General,
            _ => false,
        }
    }

    pub(crate) fn is_int_or_lit(&self, r: Ty) -> bool {
        match self.tys.kind(r) {
            TyKind::Int(..) | TyKind::Error => true,
            TyKind::Infer(v) => self.vars[*v as usize].1 == super::VarKind::Int,
            _ => false,
        }
    }

    fn check_for(&mut self, _s: StmtId, head: ForHead, invariants: List<ExprId>, body: ExprId) {
        self.push_scope();
        let infinite = matches!(head, ForHead::Infinite);
        match head {
            ForHead::Infinite => {}
            ForHead::Cond(c) => {
                let b = self.tys.bool;
                self.check_expr(c, Some(b));
                if let ExprKind::Bool(v) = self.ast.exprs[c].kind {
                    let cs = self.ast.exprs[c].span;
                    let mut d = Diagnostic::error(codes::E0509, cs, format!("condition is always `{v}`"));
                    if v {
                        d = d.note("write `for { ... }` for an infinite loop");
                    }
                    self.push(d);
                }
            }
            ForHead::In { pat, iter } => {
                let elem = self.check_iterable(iter);
                self.check_pat_irrefutable(pat, elem, LocalKind::Pattern);
            }
        }
        let saved_ctx = self.ctx;
        self.ctx = Ctx::Pure;
        for &inv in self.ast.expr_lists.get(invariants) {
            let b = self.tys.bool;
            self.check_expr(inv, Some(b));
        }
        self.ctx = saved_ctx;
        self.loop_depth += 1;
        self.loop_breaks.push(false);
        let saved_div = self.diverges;
        self.diverges = false;
        self.check_block_stmts(body);
        self.loop_depth -= 1;
        let broke = self.loop_breaks.pop().unwrap_or(false);
        self.diverges = saved_div || (infinite && !broke);
        self.pop_scope();
    }

    fn check_iterable(&mut self, iter: ExprId) -> Ty {
        let span = self.ast.exprs[iter].span;
        if let ExprKind::Range { lo, hi } = self.ast.exprs[iter].kind {
            let (Some(lo), Some(hi)) = (lo, hi) else {
                self.err(codes::E0306, span, "a `for` range needs both ends: `for i in a..b`");
                return self.tys.error;
            };
            let lt = self.check_expr(lo, None);
            let ht = self.check_expr(hi, Some(lt));
            self.record(iter, ht);
            let r = self.resolve(lt);
            let ok = self.tys.is_int(r)
                || matches!(self.tys.kind(r), TyKind::Error)
                || matches!(self.tys.kind(r), TyKind::Infer(v) if self.vars[*v as usize].1 == super::VarKind::Int);
            if !ok {
                let ts = self.show(lt);
                self.err(codes::E0306, span, format!("ranges need integers, found `{ts}`"));
            }
            return lt;
        }
        let t = self.check_expr(iter, None);
        let r = self.resolve(t);
        match self.tys.kind(r).clone() {
            TyKind::List(e) | TyKind::Set(e) => e,
            TyKind::Map(k, v) => self.tys.tuple(vec![k, v]),
            TyKind::Error => self.tys.error,
            TyKind::Str => {
                self.push(
                    Diagnostic::error(codes::E0306, span, "a `Str` is not directly iterable")
                        .note("iterate over `s.chars()` or `s.bytes()`"),
                );
                self.tys.error
            }
            _ => {
                let ts = self.show(t);
                self.err(codes::E0306, span, format!("cannot iterate over `{ts}`; `for` works on ranges, `List`, `Set` and `Map`"));
                self.tys.error
            }
        }
    }

    // ---------- patterns ----------

    /// Patterns in `let` and `for`: bindings, `_` and tuples only.
    fn check_pat_irrefutable(&mut self, p: PatId, t: Ty, kind: LocalKind) {
        let pat = self.ast.pats[p];
        self.tables.pat_ty[p.0 as usize] = t;
        match pat.kind {
            PatKind::Wild => {}
            PatKind::Bind(name) => {
                let id = self.declare_local(name, t, kind, pat.span);
                self.tables.pat_local.insert(p, id);
            }
            PatKind::Tuple(list) => {
                let subs: Vec<PatId> = self.ast.pat_lists.get(list).to_vec();
                let r = self.resolve(t);
                let elems: Vec<Ty> = match self.tys.kind(r).clone() {
                    TyKind::Tuple(ts) if ts.len() == subs.len() => ts.to_vec(),
                    TyKind::Infer(_) => {
                        let vs: Vec<Ty> = subs.iter().map(|_| self.fresh()).collect();
                        let tt = self.tys.tuple(vs.clone());
                        let _ = self.unify(r, tt);
                        vs
                    }
                    TyKind::Error => subs.iter().map(|_| self.tys.error).collect(),
                    _ => {
                        let ts = self.show(t);
                        self.err(codes::E0323, pat.span, format!("expected a {}-tuple, found `{ts}`", subs.len()));
                        subs.iter().map(|_| self.tys.error).collect()
                    }
                };
                for (sp, et) in subs.into_iter().zip(elems) {
                    self.check_pat_irrefutable(sp, et, kind);
                }
            }
            PatKind::Error => {}
            PatKind::Variant { name, args, has_parens: false } if args.is_empty() && self.prelude_variant(name.sym).is_none() => {
                // `let Total = ...`: a binding with a bad name, not a variant.
                let id = self.declare_local(name.sym, t, kind, pat.span);
                self.tables.pat_local.insert(p, id);
            }
            _ => {
                self.push(
                    Diagnostic::error(codes::E0323, pat.span, "this pattern can fail to match; `let` and `for` need irrefutable patterns")
                        .note("use `match` to handle each case"),
                );
            }
        }
    }

    /// Check a `match` pattern against the scrutinee type, declaring its bindings. With
    /// `rebind`, bindings must already exist (later alternatives of an `|` pattern).
    fn check_pat(&mut self, p: PatId, t: Ty, rebind: Option<&mut Vec<(Symbol, LocalId)>>) {
        let mut binds: Vec<(Symbol, LocalId)> = Vec::new();
        let is_rebind = rebind.is_some();
        let target = match rebind {
            Some(r) => r,
            None => &mut binds,
        };
        self.check_pat_inner(p, t, is_rebind, target);
    }

    fn check_pat_inner(&mut self, p: PatId, t: Ty, rebind: bool, binds: &mut Vec<(Symbol, LocalId)>) {
        let pat = self.ast.pats[p];
        self.tables.pat_ty[p.0 as usize] = t;
        let r = self.resolve(t);
        match pat.kind {
            PatKind::Wild | PatKind::Error => {}
            PatKind::Bind(name) => {
                if rebind {
                    match binds.iter().find(|(n, _)| *n == name) {
                        Some(&(_, id)) => {
                            let lt = self.body.locals[id].ty;
                            if !self.unify(lt, t) {
                                self.mismatch(pat.span, lt, t);
                            }
                            self.tables.pat_local.insert(p, id);
                        }
                        None => {
                            self.err(codes::E0323, pat.span, format!("`{name}` must be bound in every alternative of the pattern"));
                        }
                    }
                } else {
                    let id = self.declare_local(name, t, LocalKind::Pattern, pat.span);
                    self.tables.pat_local.insert(p, id);
                    binds.push((name, id));
                }
            }
            PatKind::Lit(e) => {
                let et = self.check_expr(e, Some(t));
                let er = self.resolve(et);
                if matches!(self.tys.kind(er), TyKind::Float(_)) {
                    self.err(codes::E0323, pat.span, "floats cannot be matched exactly; compare with a tolerance in a guard");
                }
                if let ExprKind::Str(_) | ExprKind::Int(_) | ExprKind::Char(_) | ExprKind::Bool(_) | ExprKind::Unary { .. } | ExprKind::Float(_) = self.ast.exprs[e].kind {
                } else {
                    self.err(codes::E0323, pat.span, "patterns may only contain literals");
                }
            }
            PatKind::Variant { name, args, has_parens } => {
                let (adt, targs) = match self.tys.kind(r).clone() {
                    TyKind::Adt(a, targs) if self.defs.adts[a].is_enum() => (a, targs),
                    TyKind::Error => return,
                    TyKind::Infer(_) => {
                        // Infer from the variant name for the prelude enums.
                        if let Some((a, _)) = self.prelude_variant(name.sym) {
                            let n = self.defs.adts[a].generics.len();
                            let vs: Vec<Ty> = (0..n).map(|_| self.fresh()).collect();
                            let at = self.tys.adt(a, vs.clone());
                            let _ = self.unify(r, at);
                            (a, vs.into_boxed_slice())
                        } else {
                            self.err(codes::E0305, pat.span, "type annotations needed: the matched value's type is not known");
                            return;
                        }
                    }
                    _ => {
                        let ts = self.show(t);
                        self.err(codes::E0323, pat.span, format!("variant pattern `{}` cannot match a value of type `{ts}`", name.sym));
                        return;
                    }
                };
                let variants = self.defs.adts[adt].variants();
                let Some(vi) = variants.iter().position(|v| v.name == name.sym) else {
                    let ename = self.defs.adts[adt].name;
                    let names: Vec<&str> = variants.iter().map(|v| v.name.as_str()).collect();
                    let mut d = Diagnostic::error(codes::E0208, name.span, format!("`{ename}` has no variant `{}`", name.sym));
                    if let Some(b) = super::expr::closest(name.sym.as_str(), names.into_iter()) {
                        d = d.fix(format!("did you mean `{b}`?"), name.span, b);
                    }
                    self.push(d);
                    return;
                };
                self.tables.pat_variant.insert(p, (adt, vi as u32));
                let ftys: Vec<Ty> = variants[vi].fields.iter().map(|f| f.ty).collect();
                let subs: Vec<PatId> = self.ast.pat_lists.get(args).to_vec();
                if !has_parens && !ftys.is_empty() {
                    let wild = vec!["_"; ftys.len()].join(", ");
                    self.push(
                        Diagnostic::error(codes::E0323, pat.span, format!("variant `{}` has {} field{}", name.sym, ftys.len(), if ftys.len() == 1 { "" } else { "s" }))
                            .fix("match its fields", pat.span, format!("{}({wild})", name.sym)),
                    );
                    return;
                }
                if has_parens && ftys.len() != subs.len() {
                    self.err(codes::E0323, pat.span, format!("variant `{}` has {} field{}, but the pattern has {}", name.sym, ftys.len(), if ftys.len() == 1 { "" } else { "s" }, subs.len()));
                    return;
                }
                for (sp, ft) in subs.into_iter().zip(ftys) {
                    let ft = self.tys.subst(ft, &targs);
                    self.check_pat_inner(sp, ft, rebind, binds);
                }
            }
            PatKind::Tuple(list) => {
                let subs: Vec<PatId> = self.ast.pat_lists.get(list).to_vec();
                let elems: Vec<Ty> = match self.tys.kind(r).clone() {
                    TyKind::Tuple(ts) if ts.len() == subs.len() => ts.to_vec(),
                    TyKind::Error => subs.iter().map(|_| self.tys.error).collect(),
                    _ => {
                        let ts = self.show(t);
                        self.err(codes::E0323, pat.span, format!("expected a {}-tuple, found `{ts}`", subs.len()));
                        return;
                    }
                };
                for (sp, et) in subs.into_iter().zip(elems) {
                    self.check_pat_inner(sp, et, rebind, binds);
                }
            }
            PatKind::Or(list) => {
                let alts: Vec<PatId> = self.ast.pat_lists.get(list).to_vec();
                let mut first = true;
                let before = binds.len();
                for a in alts {
                    if first && !rebind {
                        self.check_pat_inner(a, t, false, binds);
                        first = false;
                    } else {
                        let mut sub: Vec<(Symbol, LocalId)> = binds[before..].to_vec();
                        let n_sub = sub.len();
                        self.check_pat_inner(a, t, true, &mut sub);
                        let _ = n_sub;
                    }
                }
                self.tables.pat_ty[p.0 as usize] = t;
            }
        }
    }

    pub(crate) fn check_match(
        &mut self,
        e: ExprId,
        scrut: ExprId,
        arms: List<ast::Arm>,
        expected: Option<Ty>,
        as_value: bool,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        let st = self.check_expr(scrut, None);
        let arms_v: Vec<ast::Arm> = self.ast.arms.get(arms).to_vec();
        let mut result: Option<Ty> = expected;
        let mut all_diverge = !arms_v.is_empty();
        for arm in &arms_v {
            self.push_scope();
            self.check_pat(arm.pat, st, None);
            if let Some(g) = arm.guard {
                let b = self.tys.bool;
                self.check_expr(g, Some(b));
            }
            self.diverges = false;
            if as_value {
                let t = self.check_expr(arm.body, result);
                if result.is_none() {
                    let r = self.resolve(t);
                    if !self.tys.is_never(r) {
                        result = Some(t);
                    }
                }
            } else {
                self.check_expr_stmt(arm.body);
            }
            all_diverge &= self.diverges;
            self.pop_scope();
        }
        self.diverges = all_diverge;
        // Exhaustiveness and redundancy.
        let st_z = self.zonk(st);
        let pats: Vec<(PatId, bool)> = arms_v.iter().map(|a| (a.pat, a.guard.is_some())).collect();
        let report = exhaust::check(self.ast, self.tables, self.defs, self.tys, st_z, &pats);
        if let Some(missing) = report.missing {
            self.push(
                Diagnostic::error(codes::E0451, span, format!("non-exhaustive match: `{missing}` is not handled"))
                    .note("add an arm for it, or a final `_ => ...` arm"),
            );
        }
        for i in report.unreachable {
            let aspan = arms_v[i].span;
            self.push(Diagnostic::error(codes::E0452, aspan, "this match arm can never match").note("earlier arms already cover every value it matches"));
        }
        if !as_value {
            return if all_diverge { self.tys.never } else { self.tys.unit };
        }
        if all_diverge {
            return self.tys.never;
        }
        result.unwrap_or(self.tys.unit)
    }
}
