//! The first pass: give every comment a slot in the innermost context that contains it.
//!
//! Contexts are claimed bottom-up, so a nested context (a closure body inside a call
//! argument) takes its comments before the enclosing one (the call's argument list) sees
//! them. The file context claims whatever is left, so no comment goes unclaimed. The
//! printer looks slots up by the same keys and element offsets used here.

use super::{Key, Printer, Slot};
use crate::syntax::ast::*;

impl Printer<'_> {
    pub(super) fn claim_file(&mut self) {
        let ast = self.ast;
        let items = ast.item_lists.get(self.file.items);
        let mut elems = Vec::with_capacity(items.len());
        for &id in items {
            self.walk_item(id);
            let sp = ast.item(id).span;
            elems.push((sp.lo, sp.hi));
        }
        self.claim(Key::File, 0, u32::MAX, elems);
    }

    /// Assign the unclaimed comments with offsets in `lo..hi` to slots of `key`, whose
    /// elements span `elems`.
    fn claim(&mut self, key: Key, lo: u32, hi: u32, mut elems: Vec<(u32, u32)>) {
        elems.sort_unstable_by_key(|e| e.0);
        let a = self.comments.partition_point(|c| c.lo < lo);
        let b = self.comments.partition_point(|c| c.lo < hi);
        let mut first = true;
        let mut trailed = None;
        for ci in a..b {
            if self.claimed[ci] {
                continue;
            }
            let c = self.comments[ci];
            let j = elems.partition_point(|e| e.0 <= c.lo);
            let prev = j.checked_sub(1).map(|p| elems[p]);
            let slot = match prev {
                // Inside an element: hoisted to lead it.
                Some((plo, phi)) if c.lo < phi => Slot::Lead(plo),
                Some((plo, phi)) if trailed != Some(plo) && self.same_line(phi, c.lo) => {
                    trailed = Some(plo);
                    Slot::Trail(plo)
                }
                None if first && c.trailing && key != Key::File => Slot::Open,
                _ => match elems.get(j) {
                    Some(&(nlo, _)) => Slot::Lead(nlo),
                    None => Slot::End,
                },
            };
            first = false;
            self.claimed[ci] = true;
            self.slots.entry((key, slot)).or_default().push(ci as u32);
            self.has.insert(key);
        }
    }

    fn walk_item(&mut self, id: ItemId) {
        let ast = self.ast;
        let it = ast.item(id);
        match it.kind {
            ItemKind::Fn(f) => self.walk_fn(id, &f),
            ItemKind::Struct { fields, invariants, .. } => {
                let mut elems = Vec::new();
                for fd in ast.fields.get(fields) {
                    if let Some(r) = fd.refine {
                        self.walk_expr(r);
                    }
                    elems.push((fd.span.lo, fd.span.hi));
                }
                for &inv in ast.expr_lists.get(invariants) {
                    self.walk_expr(inv);
                    let sp = ast.expr(inv).span;
                    elems.push((sp.lo, sp.hi));
                }
                self.claim(Key::Item(id), it.name.span.hi, it.span.hi, elems);
            }
            ItemKind::Enum { variants, .. } => {
                let elems = ast.variants.get(variants).iter().map(|v| (v.span.lo, v.span.hi)).collect();
                self.claim(Key::Item(id), it.name.span.hi, it.span.hi, elems);
            }
            ItemKind::Interface { methods, .. } => {
                let ms = ast.item_lists.get(methods);
                for &m in ms {
                    self.walk_item(m);
                }
                let elems = ms.iter().map(|&m| (ast.item(m).span.lo, ast.item(m).span.hi)).collect();
                self.claim(Key::Item(id), it.name.span.hi, it.span.hi, elems);
            }
            ItemKind::Const { value, .. } => self.walk_expr(value),
            ItemKind::Test { params, requires, body, .. } => {
                let ps = ast.params.get(params);
                self.walk_params(ps);
                let lp = self.skip_trivia(it.name.span.hi, false);
                if self.byte(lp) == b'(' {
                    let (lo, hi) = self.paren_range(lp, ps.last().map(|p| p.span.hi));
                    self.claim(Key::Item(id), lo, hi, ps.iter().map(|p| (p.span.lo, p.span.hi)).collect());
                }
                for &r in ast.expr_lists.get(requires) {
                    self.walk_expr(r);
                }
                self.walk_expr(body);
            }
            ItemKind::Import { .. } | ItemKind::TypeAlias { .. } | ItemKind::Unit => {}
        }
    }

    fn walk_fn(&mut self, id: ItemId, f: &FnDecl) {
        let ast = self.ast;
        let it = ast.item(id);
        let ps = ast.params.get(f.params);
        self.walk_params(ps);
        let mut elems = Vec::with_capacity(ps.len() + 1);
        if let Some(sp) = f.self_param {
            elems.push((sp.span.lo, sp.span.hi));
        }
        elems.extend(ps.iter().map(|p| (p.span.lo, p.span.hi)));
        let lp = self.find_byte(it.name.span.hi, b'(').unwrap_or(it.name.span.hi);
        let (lo, hi) = self.paren_range(lp, elems.last().map(|e| e.1));
        self.claim(Key::Item(id), lo, hi, elems);

        let mut clauses = Vec::new();
        for &e in ast.expr_lists.get(f.requires).iter().chain(ast.expr_lists.get(f.ensures)) {
            self.walk_expr(e);
            let sp = ast.expr(e).span;
            clauses.push((sp.lo, sp.hi));
        }
        if f.effects.is_some() {
            clauses.push((f.effects_span.lo, f.effects_span.hi));
        }
        let end = f.body.map(|b| ast.expr(b).span.lo).unwrap_or(it.span.hi);
        self.claim(Key::Clauses(id), hi, end, clauses);
        if let Some(b) = f.body {
            self.walk_expr(b);
        }
    }

    fn walk_params(&mut self, ps: &[Param]) {
        for p in ps {
            if let Some(r) = p.refine {
                self.walk_expr(r);
            }
        }
    }

    fn walk_stmt(&mut self, s: StmtId) {
        match self.ast.stmt(s).kind {
            StmtKind::Let { init, .. } => self.walk_expr(init),
            StmtKind::Assign { target, value, .. } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
            StmtKind::Expr(e) | StmtKind::Return(Some(e)) => self.walk_expr(e),
            StmtKind::For { head, invariants, body } => {
                match head {
                    ForHead::Cond(c) => self.walk_expr(c),
                    ForHead::In { iter, .. } => self.walk_expr(iter),
                    ForHead::Infinite => {}
                }
                for &inv in self.ast.expr_lists.get(invariants) {
                    self.walk_expr(inv);
                }
                self.walk_expr(body);
            }
            StmtKind::Return(None) | StmtKind::Break | StmtKind::Continue => {}
        }
    }

    /// Walk an expression, iterating down left spines (binary chains, postfix chains) so long
    /// chains do not recurse deeply.
    fn walk_expr(&mut self, mut e: ExprId) {
        let ast = self.ast;
        loop {
            let ex = ast.expr(e);
            match ex.kind {
                ExprKind::Paren(x) | ExprKind::MutArg(x) | ExprKind::Try(x) => e = x,
                ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => e = expr,
                ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } | ExprKind::TypeArgs { base, .. } => {
                    e = base
                }
                ExprKind::Index { base, index } => {
                    self.walk_expr(index);
                    e = base;
                }
                ExprKind::Binary { lhs, rhs, .. } => {
                    self.walk_expr(rhs);
                    e = lhs;
                }
                ExprKind::Range { lo, hi } => {
                    if let Some(h) = hi {
                        self.walk_expr(h);
                    }
                    match lo {
                        Some(l) => e = l,
                        None => return,
                    }
                }
                ExprKind::Call { callee, args } => {
                    let xs = ast.expr_lists.get(args);
                    for &a in xs {
                        self.walk_expr(a);
                    }
                    let elems = xs.iter().map(|&a| (ast.expr(a).span.lo, ast.expr(a).span.hi)).collect();
                    self.claim(Key::Expr(e), ast.expr(callee).span.hi, ex.span.hi, elems);
                    e = callee;
                }
                ExprKind::Tuple(l) | ExprKind::List(l) => {
                    let xs = ast.expr_lists.get(l);
                    for &a in xs {
                        self.walk_expr(a);
                    }
                    let elems = xs.iter().map(|&a| (ast.expr(a).span.lo, ast.expr(a).span.hi)).collect();
                    self.claim(Key::Expr(e), ex.span.lo, ex.span.hi, elems);
                    return;
                }
                ExprKind::If { cond, then, els } => {
                    self.walk_expr(cond);
                    self.walk_expr(then);
                    match els {
                        Some(x) => e = x,
                        None => return,
                    }
                }
                ExprKind::Match { scrut, arms } => {
                    self.walk_expr(scrut);
                    let arms = ast.arms.get(arms);
                    for a in arms {
                        if let Some(g) = a.guard {
                            self.walk_expr(g);
                        }
                        self.walk_expr(a.body);
                    }
                    let elems = arms.iter().map(|a| (a.span.lo, a.span.hi)).collect();
                    self.claim(Key::Expr(e), ast.expr(scrut).span.hi, ex.span.hi, elems);
                    return;
                }
                ExprKind::Block(stmts) => {
                    let ss = ast.stmt_lists.get(stmts);
                    for &s in ss {
                        self.walk_stmt(s);
                    }
                    let elems = ss.iter().map(|&s| (ast.stmt(s).span.lo, ast.stmt(s).span.hi)).collect();
                    self.claim(Key::Expr(e), ex.span.lo, ex.span.hi, elems);
                    return;
                }
                ExprKind::StructLit { ty, fields } => {
                    let fs = ast.field_inits.get(fields);
                    for fi in fs {
                        self.walk_expr(fi.value);
                    }
                    let elems = fs.iter().map(|fi| (fi.name.span.lo, ast.expr(fi.value).span.hi)).collect();
                    self.claim(Key::Expr(e), ast.ty(ty).span.hi, ex.span.hi, elems);
                    return;
                }
                ExprKind::Closure { params, body, .. } => {
                    let ps = ast.params.get(params);
                    self.walk_params(ps);
                    let lp = self.skip_trivia(ex.span.lo + 2, false);
                    let (lo, hi) = self.paren_range(lp, ps.last().map(|p| p.span.hi));
                    self.claim(Key::Expr(e), lo, hi, ps.iter().map(|p| (p.span.lo, p.span.hi)).collect());
                    e = body;
                }
                ExprKind::Quant { lo, hi, body, .. } => {
                    self.walk_expr(lo);
                    self.walk_expr(hi);
                    e = body;
                }
                ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Bool(_)
                | ExprKind::Str(_)
                | ExprKind::Char(_)
                | ExprKind::Name(_)
                | ExprKind::Error => return,
            }
        }
    }
}
