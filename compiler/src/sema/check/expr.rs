//! Expression checking.

use super::{Ctx, Cx, VarKind};
use crate::diag::{codes, Diagnostic, Span};
use crate::sema::defs::*;
use crate::sema::ty::*;
use crate::syntax::ast::{self, BinOp, ExprId, ExprKind, UnOp};
use crate::util::{sym, List, Symbol};

impl Cx<'_> {
    /// Check an expression in value position against an optional expected type.
    pub(crate) fn check_expr(&mut self, e: ExprId, expected: Option<Ty>) -> Ty {
        let t = self.check_expr_inner(e, expected);
        self.record(e, t);
        if let Some(exp) = expected {
            if !self.coerce(t, exp) {
                let span = self.ast.exprs[e].span;
                self.mismatch(span, exp, t);
                return exp;
            }
        }
        t
    }

    /// Check an expression in statement position: its value is discarded.
    pub(crate) fn check_expr_stmt(&mut self, e: ExprId) {
        let ex = self.ast.exprs[e];
        match ex.kind {
            ExprKind::If { cond, then, els } => {
                let t = self.check_if(e, cond, then, els, None, false);
                self.record(e, t);
            }
            ExprKind::Match { scrut, arms } => {
                let t = self.check_match(e, scrut, arms, None, false);
                self.record(e, t);
            }
            ExprKind::Block(_) => {
                self.check_block_stmts(e);
                let t = if self.diverges { self.tys.never } else { self.tys.unit };
                self.record(e, t);
            }
            _ => {
                let t = self.check_expr(e, None);
                let r = self.resolve(t);
                if self.tys.is_unit(r) || self.tys.is_never(r) || self.tys.is_error(r) {
                    if self.tys.is_never(r) {
                        self.diverges = true;
                    }
                    return;
                }
                let ts = self.show(r);
                let is_call = matches!(ex.kind, ExprKind::Call { .. });
                let msg = if is_call {
                    format!("unused result of type `{ts}`")
                } else {
                    format!("unused value of type `{ts}`")
                };
                let snippet = self.snippet(ex.span);
                let mut d = Diagnostic::error(codes::E0507, ex.span, msg);
                if !snippet.is_empty() {
                    d = d.fix("discard it explicitly", ex.span, format!("let _ = {snippet}"));
                }
                self.push(d);
            }
        }
    }

    fn check_expr_inner(&mut self, e: ExprId, expected: Option<Ty>) -> Ty {
        let ex = self.ast.exprs[e];
        match ex.kind {
            ExprKind::Int(v) => self.check_int_lit(e, v, false, expected),
            ExprKind::Float(_) => {
                if let Some(exp) = expected {
                    let r = self.resolve(exp);
                    if matches!(self.tys.kind(r), TyKind::Float(_)) {
                        return r;
                    }
                    if matches!(self.tys.kind(r), TyKind::Int(..)) {
                        let en = self.show(r);
                        self.push(Diagnostic::error(
                            codes::E0301,
                            ex.span,
                            format!("mismatched types: expected `{en}`, found a float literal"),
                        ));
                        return r;
                    }
                }
                self.new_var(VarKind::Float)
            }
            ExprKind::Bool(_) => self.tys.bool,
            ExprKind::Str(_) => self.tys.str,
            ExprKind::Char(c) => {
                if let Some(exp) = expected {
                    let r = self.resolve(exp);
                    if let TyKind::Int(IntTy::U8, _) = self.tys.kind(r) {
                        if c < 128 {
                            return r;
                        }
                        self.err(codes::E0104, ex.span, "only ASCII characters fit in a `U8` literal");
                        return r;
                    }
                }
                self.tys.char
            }
            ExprKind::Name(n) => self.check_name(e, n, expected),
            ExprKind::Paren(inner) => self.check_expr(inner, expected),
            ExprKind::Tuple(list) => {
                let elems: Vec<ExprId> = self.ast.expr_lists.get(list).to_vec();
                let exp_elems: Option<Vec<Ty>> = expected.and_then(|t| match self.kind_of(t) {
                    TyKind::Tuple(ts) if ts.len() == elems.len() => Some(ts.to_vec()),
                    _ => None,
                });
                let mut tys = Vec::with_capacity(elems.len());
                for (i, &el) in elems.iter().enumerate() {
                    let et = exp_elems.as_ref().map(|v| v[i]);
                    tys.push(self.check_expr(el, et));
                }
                self.tys.tuple(tys)
            }
            ExprKind::List(list) => {
                let elems: Vec<ExprId> = self.ast.expr_lists.get(list).to_vec();
                let elem_ty = match expected.map(|t| self.kind_of(t)) {
                    Some(TyKind::List(t)) => t,
                    _ => self.fresh(),
                };
                for el in elems {
                    self.check_expr(el, Some(elem_ty));
                }
                self.tys.list(elem_ty)
            }
            ExprKind::Field { base, name } => self.check_field(e, base, name, expected),
            ExprKind::TupleField { base, index, index_span } => {
                let bt = self.check_expr(base, None);
                match self.kind_of(bt) {
                    TyKind::Tuple(ts) => match ts.get(index as usize) {
                        Some(&t) => t,
                        None => {
                            self.err(codes::E0206, index_span, format!("tuple has no field `{index}` (it has {} fields)", ts.len()));
                            self.tys.error
                        }
                    },
                    TyKind::Error => self.tys.error,
                    TyKind::Infer(_) => {
                        self.err(codes::E0305, ex.span, "type annotations needed: the tuple's type is not known here");
                        self.tys.error
                    }
                    _ => {
                        let bs = self.show(bt);
                        self.err(codes::E0206, index_span, format!("`{bs}` is not a tuple"));
                        self.tys.error
                    }
                }
            }
            ExprKind::Call { callee, args } => self.check_call(e, callee, args, expected),
            ExprKind::MutArg(inner) => {
                self.err(codes::E0314, ex.span, "`&mut` is only allowed in call arguments");
                self.check_expr(inner, expected)
            }
            ExprKind::Index { base, index } => self.check_index(e, base, index),
            ExprKind::Range { .. } => {
                self.push(
                    Diagnostic::error(codes::E0306, ex.span, "a range is only allowed in `for`, slices and quantifiers")
                        .note("build a list with a loop, e.g. `for i in 0..n { xs.push(i) }`"),
                );
                self.tys.error
            }
            ExprKind::Unary { op, expr } => self.check_unary(e, op, expr, expected),
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(e, op, lhs, rhs, expected),
            ExprKind::Cast { expr, ty } => self.check_cast(e, expr, ty),
            ExprKind::Try(inner) => self.check_try(e, inner),
            ExprKind::If { cond, then, els } => self.check_if(e, cond, then, els, expected, true),
            ExprKind::Match { scrut, arms } => self.check_match(e, scrut, arms, expected, true),
            ExprKind::Block(_) => self.check_block_value(e, expected),
            ExprKind::StructLit { ty, fields } => self.check_struct_lit(e, ty, fields, expected),
            ExprKind::Closure { params, ret, body } => self.check_closure(e, params, ret, body, expected),
            ExprKind::TypeArgs { base, args } => self.check_type_args_value(e, base, args, expected),
            ExprKind::Quant { var, lo, hi, body, .. } => {
                let lt = self.check_expr(lo, None);
                let ht = self.check_expr(hi, Some(lt));
                let it = self.resolve(ht);
                if !self.tys.is_int(it) && !matches!(self.tys.kind(it), TyKind::Infer(_) | TyKind::Error) {
                    let s = self.show(it);
                    self.err(codes::E0306, ex.span, format!("quantifier ranges must be integers, found `{s}`"));
                }
                self.push_scope();
                let id = self.declare_local(var.sym, lt, LocalKind::Quant, var.span);
                self.body.locals[id].used = true;
                let b = self.tys.bool;
                self.check_expr(body, Some(b));
                self.pop_scope();
                self.tys.bool
            }
            ExprKind::Error => self.tys.error,
        }
    }

    pub(crate) fn check_int_lit(&mut self, e: ExprId, v: u64, neg: bool, expected: Option<Ty>) -> Ty {
        self.int_lits.push((e, v as u128, neg));
        if let Some(exp) = expected {
            let r = self.resolve(exp);
            match self.tys.kind(r).clone() {
                TyKind::Int(..) => return r,
                TyKind::Float(_) => {
                    let span = self.ast.exprs[e].span;
                    let en = self.show(r);
                    let text = format!("{}{v}.0", if neg { "-" } else { "" });
                    self.push(
                        Diagnostic::error(codes::E0301, span, format!("mismatched types: expected `{en}`, found an integer literal"))
                            .fix("write a float literal", span, text),
                    );
                    return r;
                }
                _ => {}
            }
        }
        self.new_var(VarKind::Int)
    }

    // ---------- names ----------

    pub(crate) fn lookup_def(&self, name: Symbol) -> Option<Def> {
        if let Some(d) = self.pkgs[self.pkg].scope.get(&name) {
            return Some(*d);
        }
        if self.pkg != self.prelude {
            if let Some(d) = self.pkgs[self.prelude].scope.get(&name) {
                return Some(*d);
            }
        }
        None
    }

    pub(crate) fn prelude_variant(&self, name: Symbol) -> Option<(AdtId, u32)> {
        let (opt, res) = (self.defs.option?, self.defs.result?);
        Some(match name.as_str() {
            "Some" => (opt, 0),
            "None" => (opt, 1),
            "Ok" => (res, 0),
            "Err" => (res, 1),
            _ => return None,
        })
    }

    fn check_name(&mut self, e: ExprId, name: Symbol, expected: Option<Ty>) -> Ty {
        let span = self.ast.exprs[e].span;
        if let Some(id) = self.lookup_local(name) {
            self.use_local(id);
            self.set_res(e, Res::Local(id));
            return self.body.locals[id].ty;
        }
        if name == sym::RESULT && self.ctx == Ctx::Ensures {
            self.set_res(e, Res::ResultVal);
            let r = self.ret_ty;
            if self.tys.is_unit(r) {
                self.err(codes::E0320, span, "`result` is not available: this function returns nothing");
                return self.tys.error;
            }
            return r;
        }
        if let Some(def) = self.lookup_def(name) {
            return self.value_of_def(e, def, name, span, None, expected);
        }
        if let Some((adt, v)) = self.prelude_variant(name) {
            return self.variant_value(e, adt, v, None, expected);
        }
        if self.file_imports().contains_key(&name) {
            self.imports_used.insert((self.file, name));
            self.err(codes::E0326, span, format!("package `{name}` is not a value; use `{name}.<item>`"));
            return self.tys.error;
        }
        self.unresolved(span, name);
        self.tys.error
    }

    pub(crate) fn unresolved(&mut self, span: Span, name: Symbol) {
        let mut cands: Vec<Symbol> = self.scope.iter().map(|(n, _)| *n).collect();
        cands.extend(self.pkgs[self.pkg].scope.keys().copied());
        cands.extend(self.pkgs[self.prelude].scope.keys().copied());
        cands.extend(self.file_imports().keys().copied());
        let mut d = Diagnostic::error(codes::E0201, span, format!("cannot find `{name}` in this scope"));
        if let Some(best) = closest(name.as_str(), cands.iter().map(|s| s.as_str())) {
            d = d.fix(format!("did you mean `{best}`?"), span, best);
        } else if super::super::load::STD_SOURCES.iter().any(|(p, _, _)| p.rsplit('/').next() == Some(name.as_str())) {
            d = d.note(format!("add `import \"std/{name}\"`"));
        }
        self.push(d);
    }

    /// The value a package-level definition denotes when used as an expression.
    pub(crate) fn value_of_def(
        &mut self,
        e: ExprId,
        def: Def,
        name: Symbol,
        span: Span,
        explicit: Option<Vec<Ty>>,
        _expected: Option<Ty>,
    ) -> Ty {
        match def {
            Def::Fn(f) => self.fn_value(e, f, explicit, span),
            Def::Const(c) => {
                self.set_res(e, Res::Const(c));
                if !self.body.consts_used.contains(&c) {
                    self.body.consts_used.push(c);
                }
                self.defs.consts[c].ty
            }
            Def::Adt(_) | Def::Alias(_) | Def::Builtin(_) => {
                self.push(
                    Diagnostic::error(codes::E0326, span, format!("`{name}` is a type, not a value"))
                        .note(format!("construct a struct with `{name} {{ field: value }}`, or call an associated function with `{name}.function()`")),
                );
                self.tys.error
            }
            Def::Iface(_) | Def::Unit => {
                self.err(codes::E0326, span, format!("`{name}` is not a value"));
                self.tys.error
            }
        }
    }

    /// A function item used as a value: instantiate its generics.
    pub(crate) fn fn_value(&mut self, e: ExprId, f: FnId, explicit: Option<Vec<Ty>>, span: Span) -> Ty {
        let fd = &self.defs.fns[f];
        if fd.params.iter().any(|p| p.mode == Mode::MutRef) {
            let n = fd.name;
            self.err(codes::E0314, span, format!("`{n}` takes `&mut` parameters and cannot be used as a function value"));
        }
        let inst = self.instantiate(f, &[], explicit, span);
        let fd = &self.defs.fns[f];
        let ptys: Vec<Ty> = fd.params.iter().map(|p| p.ty).collect();
        let (ret, eff) = (fd.ret, fd.effects);
        let ptys: Vec<Ty> = ptys.into_iter().map(|t| self.tys.subst(t, &inst)).collect();
        let ret = self.tys.subst(ret, &inst);
        self.set_res(e, Res::Fn(f));
        self.tables.expr_inst.insert(e, inst.into_boxed_slice());
        self.add_ref(f);
        self.tys.func(ptys, ret, eff)
    }

    /// Fresh type arguments for a function: receiver arguments first, then explicit or fresh.
    pub(crate) fn instantiate(&mut self, f: FnId, recv: &[Ty], explicit: Option<Vec<Ty>>, span: Span) -> Vec<Ty> {
        let fd = &self.defs.fns[f];
        let n = fd.generics.len();
        let n_recv = fd.n_recv_generics;
        let mut inst: Vec<Ty> = Vec::with_capacity(n);
        for i in 0..n_recv {
            match recv.get(i) {
                Some(&t) => inst.push(t),
                None => {
                    let v = self.fresh();
                    inst.push(v)
                }
            }
        }
        let own = n - n_recv;
        match explicit {
            Some(ex) => {
                if ex.len() != own {
                    let name = self.defs.fns[f].name;
                    self.err(codes::E0313, span, format!("`{name}` takes {own} type argument{}, found {}", if own == 1 { "" } else { "s" }, ex.len()));
                    for _ in 0..own {
                        let v = self.fresh();
                        inst.push(v);
                    }
                } else {
                    inst.extend(ex);
                }
            }
            None => {
                for _ in 0..own {
                    let v = self.fresh();
                    inst.push(v);
                }
            }
        }
        // Record bound obligations.
        let bounds: Vec<(usize, Vec<Bound>)> =
            self.defs.fns[f].generics.iter().enumerate().map(|(i, g)| (i, g.bounds.clone())).collect();
        for (i, bs) in bounds {
            for b in bs {
                self.require_bound(span, inst[i], b);
            }
        }
        inst
    }

    /// A variant used as a value: nullary variants are values; others are constructor
    /// functions.
    pub(crate) fn variant_value(
        &mut self,
        e: ExprId,
        adt: AdtId,
        v: u32,
        targs: Option<Vec<Ty>>,
        expected: Option<Ty>,
    ) -> Ty {
        let n = self.defs.adts[adt].generics.len();
        let args: Vec<Ty> = match targs {
            Some(a) => a,
            None => (0..n).map(|_| self.fresh()).collect(),
        };
        let ty = self.tys.adt(adt, args.clone());
        if let Some(exp) = expected {
            let _ = self.unify(ty, exp);
        }
        self.set_res(e, Res::Variant(adt, v));
        self.tables.expr_inst.insert(e, args.clone().into_boxed_slice());
        let field_tys: Vec<Ty> = self.defs.adts[adt].variants()[v as usize].fields.iter().map(|f| f.ty).collect();
        if field_tys.is_empty() {
            self.note_variant_built(adt, v);
            return ty;
        }
        let ps: Vec<Ty> = field_tys.into_iter().map(|t| self.tys.subst(t, &args)).collect();
        self.note_variant_built(adt, v);
        self.tys.func(ps, ty, EffectSet::PURE)
    }

    pub(crate) fn note_variant_built(&mut self, adt: AdtId, v: u32) {
        if !self.body.variants_built.contains(&(adt, v)) {
            self.body.variants_built.push((adt, v));
        }
    }

    pub(crate) fn note_field_read(&mut self, adt: AdtId, i: u32) {
        if !self.body.fields_read.contains(&(adt, i)) {
            self.body.fields_read.push((adt, i));
        }
    }

    fn check_type_args_value(&mut self, e: ExprId, base: ExprId, args: List<ast::TypeExprId>, expected: Option<Ty>) -> Ty {
        let targs: Vec<Ty> = self.ast.type_lists.get(args).to_vec().into_iter().map(|t| self.resolve_type_expr(t)).collect();
        let span = self.ast.exprs[e].span;
        match self.ast.exprs[base].kind {
            ExprKind::Name(n) if self.lookup_local(n).is_none() => match self.lookup_def(n) {
                Some(Def::Fn(f)) => {
                    let t = self.fn_value(e, f, Some(targs), span);
                    self.record(base, t);
                    t
                }
                _ => {
                    let _ = expected;
                    self.err(codes::E0326, span, "explicit type arguments here must follow a generic function or type");
                    self.tys.error
                }
            },
            _ => {
                self.err(codes::E0326, span, "explicit type arguments here must follow a generic function or type");
                self.tys.error
            }
        }
    }

    // ---------- operators ----------

    fn check_unary(&mut self, e: ExprId, op: UnOp, inner: ExprId, expected: Option<Ty>) -> Ty {
        let span = self.ast.exprs[e].span;
        match op {
            UnOp::Neg => {
                if let ExprKind::Int(v) = self.ast.exprs[inner].kind {
                    let t = self.check_int_lit(inner, v, true, expected);
                    self.record(inner, t);
                    let r = self.resolve(t);
                    if let TyKind::Int(k, _) = self.tys.kind(r) {
                        if !k.signed() {
                            self.err(codes::E0306, span, format!("cannot negate an unsigned `{}`", k.name()));
                        }
                    }
                    return t;
                }
                let t = self.check_expr(inner, expected.filter(|&t| self.tys.is_numeric(self.resolve(t))));
                let r = self.resolve(t);
                match self.tys.kind(r).clone() {
                    TyKind::Int(k, _) if !k.signed() => {
                        self.err(codes::E0306, span, format!("cannot negate an unsigned `{}`", k.name()));
                        t
                    }
                    TyKind::Int(..) | TyKind::Float(_) | TyKind::Error => t,
                    TyKind::Infer(v) if self.vars[v as usize].1 != VarKind::General => t,
                    _ => {
                        let s = self.show(t);
                        self.err(codes::E0306, span, format!("cannot negate a value of type `{s}`"));
                        self.tys.error
                    }
                }
            }
            UnOp::Not => {
                let b = self.tys.bool;
                self.check_expr(inner, Some(b));
                b
            }
            UnOp::BitNot => {
                let t = self.check_expr(inner, expected);
                let r = self.resolve(t);
                match self.tys.kind(r) {
                    TyKind::Int(_, u) if *u == NO_UNIT => t,
                    TyKind::Infer(v) if self.vars[*v as usize].1 == VarKind::Int => t,
                    TyKind::Error => t,
                    _ => {
                        let s = self.show(t);
                        self.err(codes::E0306, span, format!("`~` needs an integer without a unit, found `{s}`"));
                        self.tys.error
                    }
                }
            }
        }
    }

    pub(crate) fn check_binary(&mut self, e: ExprId, op: BinOp, lhs: ExprId, rhs: ExprId, expected: Option<Ty>) -> Ty {
        let span = self.ast.exprs[e].span;
        match op {
            BinOp::And | BinOp::Or => {
                let b = self.tys.bool;
                self.check_expr(lhs, Some(b));
                self.check_expr(rhs, Some(b));
                b
            }
            _ if op.is_comparison() => {
                self.lint_self_compare(e, lhs, rhs);
                let lt = self.check_expr(lhs, None);
                let rt = self.check_expr_no_report(rhs, Some(lt));
                if !self.unify(lt, rt) {
                    self.operand_mismatch(span, op, lt, rt);
                    return self.tys.bool;
                }
                let r = self.resolve(lt);
                self.body.derived_exprs.push(lhs);
                let is_float = matches!(self.tys.kind(r), TyKind::Float(_));
                match op {
                    BinOp::Eq | BinOp::Ne => self.require_bound(span, lt, Bound::Eq),
                    _ if is_float => {}
                    _ => self.require_bound(span, lt, Bound::Ord),
                }
                self.tys.bool
            }
            BinOp::Shl | BinOp::Shr => {
                let lt = self.check_expr(lhs, expected);
                let rt = self.check_expr(rhs, None);
                for (t, side) in [(lt, lhs), (rt, rhs)] {
                    let r = self.resolve(t);
                    let ok = matches!(self.tys.kind(r), TyKind::Int(_, u) if *u == NO_UNIT)
                        || matches!(self.tys.kind(r), TyKind::Error)
                        || matches!(self.tys.kind(r), TyKind::Infer(v) if self.vars[*v as usize].1 == VarKind::Int);
                    if !ok {
                        let s = self.show(t);
                        let sp = self.ast.exprs[side].span;
                        self.err(codes::E0306, sp, format!("shifts need integers without units, found `{s}`"));
                    }
                }
                lt
            }
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor => {
                let lt = self.check_expr(lhs, expected);
                let rt = self.check_expr_no_report(rhs, Some(lt));
                if !self.unify(lt, rt) {
                    self.operand_mismatch(span, op, lt, rt);
                    return self.tys.error;
                }
                let r = self.resolve(lt);
                let ok = matches!(self.tys.kind(r), TyKind::Int(_, u) if *u == NO_UNIT)
                    || matches!(self.tys.kind(r), TyKind::Error)
                    || matches!(self.tys.kind(r), TyKind::Infer(v) if self.vars[*v as usize].1 == VarKind::Int);
                if !ok {
                    let s = self.show(lt);
                    self.err(codes::E0306, span, format!("`{}` needs integers without units, found `{s}`", op.as_str()));
                }
                lt
            }
            _ => self.check_arith(e, op, lhs, rhs, expected),
        }
    }

    /// Check without reporting a mismatch against `expected` (the caller reports).
    fn check_expr_no_report(&mut self, e: ExprId, expected: Option<Ty>) -> Ty {
        let t = self.check_expr_inner(e, expected);
        self.record(e, t)
    }

    fn operand_mismatch(&mut self, span: Span, op: BinOp, lt: Ty, rt: Ty) {
        let (a, b) = (self.show(lt), self.show(rt));
        let (ra, rb) = (self.resolve(lt), self.resolve(rt));
        let same_kind = matches!((self.tys.kind(ra), self.tys.kind(rb)), (TyKind::Float(_), TyKind::Float(_)))
            || matches!((self.tys.kind(ra), self.tys.kind(rb)), (TyKind::Int(x, _), TyKind::Int(y, _)) if x == y);
        if same_kind {
            self.push(
                Diagnostic::error(codes::E0317, span, format!("mismatched units: `{a}` {} `{b}`", op.as_str()))
                    .note("add and compare values with the same unit; convert with `as` if needed"),
            );
        } else {
            let mut d = Diagnostic::error(codes::E0306, span, format!("`{}` cannot combine `{a}` and `{b}`", op.as_str()));
            if self.tys.is_numeric(ra) && self.tys.is_numeric(rb) {
                d = d.note(format!("Jig has no implicit numeric conversions; convert one side with `as {a}` or `as {b}`"));
            }
            self.push(d);
        }
    }

    fn check_arith(&mut self, e: ExprId, op: BinOp, lhs: ExprId, rhs: ExprId, expected: Option<Ty>) -> Ty {
        let span = self.ast.exprs[e].span;
        let additive = matches!(op, BinOp::Add | BinOp::Sub | BinOp::Rem);
        let lt = self.check_expr(lhs, if additive { expected } else { None });
        let rt = if additive { self.check_expr_no_report(rhs, Some(lt)) } else { self.check_expr(rhs, None) };
        let (ra, rb) = (self.resolve(lt), self.resolve(rt));
        let (ka, kb) = (self.tys.kind(ra).clone(), self.tys.kind(rb).clone());
        if matches!(ka, TyKind::Error) || matches!(kb, TyKind::Error) {
            return self.tys.error;
        }
        // String concatenation.
        if op == BinOp::Add && (matches!(ka, TyKind::Str) || matches!(kb, TyKind::Str)) {
            if !(matches!(ka, TyKind::Str) && matches!(kb, TyKind::Str)) {
                let (a, b) = (self.show(lt), self.show(rt));
                let other = if matches!(ka, TyKind::Str) { rhs } else { lhs };
                let osp = self.ast.exprs[other].span;
                let snippet = self.snippet(osp);
                let mut d = Diagnostic::error(codes::E0306, span, format!("`+` cannot join `{a}` and `{b}`"));
                if !snippet.is_empty() {
                    d = d.fix("convert to a string with `.str()`", osp, format!("{snippet}.str()"));
                }
                self.push(d);
                return self.tys.str;
            }
            return self.tys.str;
        }
        let numeric = |cx: &Self, k: &TyKind| match k {
            TyKind::Int(..) | TyKind::Float(_) => true,
            TyKind::Infer(v) => cx.vars[*v as usize].1 != VarKind::General,
            _ => false,
        };
        if !numeric(self, &ka) || !numeric(self, &kb) {
            // Generic variables might be numbers later; force the issue now.
            if matches!(ka, TyKind::Infer(_)) && self.unify(lt, rt) && numeric(self, &kb) {
                return rt;
            }
            let (a, b) = (self.show(lt), self.show(rt));
            let bad = if numeric(self, &ka) { b } else { a };
            self.err(codes::E0306, span, format!("`{}` needs numbers, found `{bad}`", op.as_str()));
            return self.tys.error;
        }
        if additive {
            if !self.unify(lt, rt) {
                self.operand_mismatch(span, op, lt, rt);
                return self.tys.error;
            }
            return lt;
        }
        // * and /: units combine. A literal operand is dimensionless.
        let (ra, rb) = (self.default_lit_to(lt, rt), self.default_lit_to(rt, lt));
        let (ka, kb) = (self.tys.kind(ra).clone(), self.tys.kind(rb).clone());
        match (&ka, &kb) {
            (TyKind::Int(x, u), TyKind::Int(y, w)) if x == y => {
                let unit = self.tys.unit_mul(*u, *w, if op == BinOp::Mul { 1 } else { -1 });
                self.tys.int_ty(*x, unit)
            }
            (TyKind::Float(u), TyKind::Float(w)) => {
                let unit = self.tys.unit_mul(*u, *w, if op == BinOp::Mul { 1 } else { -1 });
                self.tys.intern(TyKind::Float(unit))
            }
            (TyKind::Infer(_), TyKind::Infer(_)) => {
                if !self.unify(ra, rb) {
                    self.operand_mismatch(span, op, lt, rt);
                }
                ra
            }
            _ => {
                self.operand_mismatch(span, op, lt, rt);
                self.tys.error
            }
        }
    }

    /// If `t` is an unresolved literal variable and `other` is a concrete number, bind `t` to
    /// the dimensionless version of `other`'s type.
    fn default_lit_to(&mut self, t: Ty, other: Ty) -> Ty {
        let r = self.resolve(t);
        if let TyKind::Infer(_) = self.tys.kind(r) {
            let o = self.resolve(other);
            if self.tys.is_numeric(o) {
                let plain = self.tys.with_unit(o, NO_UNIT);
                if self.unify(r, plain) {
                    return plain;
                }
            }
        }
        r
    }

    fn lint_self_compare(&mut self, e: ExprId, lhs: ExprId, rhs: ExprId) {
        if let (ExprKind::Name(a), ExprKind::Name(b)) = (self.ast.exprs[lhs].kind, self.ast.exprs[rhs].kind) {
            if a == b {
                let span = self.ast.exprs[e].span;
                self.err(codes::E0508, span, format!("`{a}` is compared with itself"));
            }
        }
    }

    fn check_cast(&mut self, e: ExprId, inner: ExprId, ty: ast::TypeExprId) -> Ty {
        let span = self.ast.exprs[e].span;
        let target = self.resolve_type_expr(ty);
        let src = self.check_expr(inner, None);
        let rs = self.resolve(src);
        let (ks, kt) = (self.tys.kind(rs).clone(), self.tys.kind(target).clone());
        let ok = match (&ks, &kt) {
            (TyKind::Error, _) | (_, TyKind::Error) => true,
            (TyKind::Int(..) | TyKind::Float(_), TyKind::Int(..) | TyKind::Float(_)) => true,
            (TyKind::Infer(v), TyKind::Int(..) | TyKind::Float(_)) => self.vars[*v as usize].1 != VarKind::General,
            (TyKind::Char, TyKind::Int(..)) | (TyKind::Int(..), TyKind::Char) => true,
            (TyKind::Infer(v), TyKind::Char) => self.vars[*v as usize].1 == VarKind::Int,
            _ => false,
        };
        if !ok {
            let (a, b) = (self.show(src), self.show(target));
            self.push(
                Diagnostic::error(codes::E0310, span, format!("cannot cast `{a}` to `{b}`"))
                    .note("`as` converts between numbers and between `Char` and integers; use methods such as `.str()` or `.to_int()` for other conversions"),
            );
        }
        target
    }

    fn check_try(&mut self, e: ExprId, inner: ExprId) -> Ty {
        let span = self.ast.exprs[e].span;
        if self.ctx != Ctx::Code {
            self.err(codes::E0321, span, "`?` cannot be used in a contract");
        }
        let t = self.check_expr(inner, None);
        let r = self.resolve(t);
        let ret = match self.closures.last() {
            Some(f) => f.ret,
            None => self.ret_ty,
        };
        let (opt, res) = (self.defs.option, self.defs.result);
        match self.tys.kind(r).clone() {
            TyKind::Adt(a, args) if Some(a) == res => {
                let err_ty = args[1];
                let want = {
                    let v = self.fresh();
                    self.tys.adt(a, vec![v, err_ty])
                };
                if !self.unify(ret, want) {
                    let (rs, es) = (self.show(ret), self.show(err_ty));
                    self.push(
                        Diagnostic::error(codes::E0309, span, format!("`?` on `Result<_, {es}>` needs the function to return `Result<_, {es}>`, but it returns `{rs}`"))
                            .note("convert the error first with `.map_err(...)`"),
                    );
                }
                args[0]
            }
            TyKind::Adt(a, args) if Some(a) == opt => {
                let want = {
                    let v = self.fresh();
                    self.tys.adt(a, vec![v])
                };
                if !self.unify(ret, want) {
                    let rs = self.show(ret);
                    self.push(
                        Diagnostic::error(codes::E0309, span, format!("`?` on an `Option` needs the function to return an `Option`, but it returns `{rs}`"))
                            .note("convert with `.ok_or(error)` to use `?` in a function returning `Result`"),
                    );
                }
                args[0]
            }
            TyKind::Error => self.tys.error,
            _ => {
                let s = self.show(t);
                self.err(codes::E0309, span, format!("`?` needs a `Result` or `Option`, found `{s}`"));
                self.tys.error
            }
        }
    }

    fn check_index(&mut self, e: ExprId, base: ExprId, index: ExprId) -> Ty {
        let span = self.ast.exprs[e].span;
        let bt = self.check_expr(base, None);
        let r = self.resolve(bt);
        let is_range = matches!(self.ast.exprs[index].kind, ExprKind::Range { .. });
        if is_range {
            if let ExprKind::Range { lo, hi } = self.ast.exprs[index].kind {
                let int = self.tys.int;
                if let Some(l) = lo {
                    self.check_expr(l, Some(int));
                }
                if let Some(h) = hi {
                    self.check_expr(h, Some(int));
                }
                self.record(index, int);
            }
        }
        match self.tys.kind(r).clone() {
            TyKind::List(elem) => {
                if is_range {
                    bt
                } else {
                    let int = self.tys.int;
                    self.check_expr(index, Some(int));
                    elem
                }
            }
            TyKind::Str => {
                if is_range {
                    self.tys.str
                } else {
                    let int = self.tys.int;
                    self.check_expr(index, Some(int));
                    self.tys.u8
                }
            }
            TyKind::Map(..) => {
                self.push(
                    Diagnostic::error(codes::E0311, span, "maps cannot be indexed")
                        .note("use `m.get(key)`, which returns an `Option`"),
                );
                self.tys.error
            }
            TyKind::Error => self.tys.error,
            TyKind::Infer(_) => {
                self.err(codes::E0305, span, "type annotations needed: the indexed value's type is not known here");
                self.tys.error
            }
            _ => {
                let s = self.show(bt);
                self.err(codes::E0311, span, format!("`{s}` cannot be indexed"));
                self.tys.error
            }
        }
    }

    // ---------- control flow ----------

    pub(crate) fn check_if(
        &mut self,
        e: ExprId,
        cond: ExprId,
        then: ExprId,
        els: Option<ExprId>,
        expected: Option<Ty>,
        as_value: bool,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        let b = self.tys.bool;
        self.check_expr(cond, Some(b));
        if let ExprKind::Bool(v) = self.ast.exprs[cond].kind {
            let cs = self.ast.exprs[cond].span;
            self.err(codes::E0509, cs, format!("condition is always `{v}`"));
        }
        self.diverges = false;
        let t1 = if as_value { self.check_block_value(then, expected) } else {
            self.check_block_stmts(then);
            self.tys.unit
        };
        self.record(then, t1);
        let d1 = self.diverges;
        let (t2, d2) = match els {
            Some(el) => {
                self.diverges = false;
                let t = if as_value {
                    self.check_expr(el, expected)
                } else {
                    match self.ast.exprs[el].kind {
                        ExprKind::If { cond, then, els } => {
                            let t = self.check_if(el, cond, then, els, None, false);
                            self.record(el, t)
                        }
                        _ => {
                            self.check_block_stmts(el);
                            self.record(el, self.tys.unit)
                        }
                    }
                };
                (t, self.diverges)
            }
            None => {
                let rt = self.resolve(t1);
                if as_value && !self.tys.is_unit(rt) && !self.tys.is_never(rt) && !self.tys.is_error(rt) {
                    self.push(
                        Diagnostic::error(codes::E0301, span, "`if` without `else` has no value on the false path")
                            .note("add an `else` branch"),
                    );
                    self.diverges = false;
                    return self.tys.error;
                }
                (self.tys.unit, false)
            }
        };
        self.diverges = d1 && d2;
        if !as_value {
            return if self.diverges { self.tys.never } else { self.tys.unit };
        }
        let sp = els.map(|x| self.ast.exprs[x].span).unwrap_or(span);
        let t = self.join(t1, t2, sp);
        if self.diverges { self.tys.never } else { t }
    }

    // ---------- struct literals ----------

    fn check_struct_lit(
        &mut self,
        e: ExprId,
        ty: ast::TypeExprId,
        fields: List<ast::FieldInit>,
        expected: Option<Ty>,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        let t = self.resolve_type_expr_infer(ty);
        if let Some(exp) = expected {
            let _ = self.unify(t, exp);
        }
        let r = self.resolve(t);
        let (adt, args) = match self.tys.kind(r).clone() {
            TyKind::Adt(a, args) if !self.defs.adts[a].is_enum() => (a, args),
            TyKind::Error => {
                for f in self.ast.field_inits.get(fields).to_vec() {
                    self.check_expr(f.value, None);
                }
                return self.tys.error;
            }
            _ => {
                let s = self.show(t);
                self.push(
                    Diagnostic::error(codes::E0324, span, format!("`{s}` is not a struct"))
                        .note("enum variants are constructed with `Enum.Variant(args)`"),
                );
                return self.tys.error;
            }
        };
        let inits: Vec<ast::FieldInit> = self.ast.field_inits.get(fields).to_vec();
        let ad = &self.defs.adts[adt];
        let foreign = ad.pkg != self.pkg;
        let decl: Vec<(Symbol, Ty, bool)> = ad.fields().iter().map(|f| (f.name, f.ty, f.is_pub)).collect();
        let sname = ad.name;
        let mut seen: Vec<Symbol> = Vec::new();
        for init in &inits {
            match decl.iter().position(|(n, _, _)| *n == init.name.sym) {
                Some(i) => {
                    if seen.contains(&init.name.sym) {
                        self.err(codes::E0322, init.name.span, format!("field `{}` is given twice", init.name.sym));
                    }
                    seen.push(init.name.sym);
                    if foreign && !decl[i].2 {
                        self.err(codes::E0206, init.name.span, format!("field `{}` of `{sname}` is private", init.name.sym));
                    }
                    let ft = self.tys.subst(decl[i].1, &args);
                    self.check_expr(init.value, Some(ft));
                }
                None => {
                    let mut d = Diagnostic::error(codes::E0206, init.name.span, format!("`{sname}` has no field `{}`", init.name.sym));
                    if let Some(best) = closest(init.name.sym.as_str(), decl.iter().map(|(n, _, _)| n.as_str())) {
                        d = d.fix(format!("did you mean `{best}`?"), init.name.span, best);
                    }
                    self.push(d);
                    self.check_expr(init.value, None);
                }
            }
        }
        let missing: Vec<String> =
            decl.iter().filter(|(n, _, _)| !seen.contains(n)).map(|(n, _, _)| format!("`{n}`")).collect();
        if !missing.is_empty() {
            self.push(
                Diagnostic::error(codes::E0304, span, format!("missing field{} {} in `{sname}`", if missing.len() > 1 { "s" } else { "" }, missing.join(", ")))
                    .note("Jig has no default values: every field needs a value"),
            );
        }
        r
    }

    // ---------- closures ----------

    fn check_closure(
        &mut self,
        e: ExprId,
        params: List<ast::Param>,
        ret: Option<ast::TypeExprId>,
        body: ExprId,
        expected: Option<Ty>,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        let exp_fn = expected.and_then(|t| match self.kind_of(t) {
            TyKind::Fn(ps, r, eff) => Some((ps.to_vec(), r, eff)),
            _ => None,
        });
        let ps: Vec<ast::Param> = self.ast.params.get(params).to_vec();
        if let Some((eps, _, _)) = &exp_fn {
            if eps.len() != ps.len() {
                self.err(codes::E0302, span, format!("this closure takes {} parameter{}, but {} {} expected", ps.len(), if ps.len() == 1 { "" } else { "s" }, eps.len(), if eps.len() == 1 { "is" } else { "are" }));
            }
        }
        let mut ptys = Vec::with_capacity(ps.len());
        for (i, p) in ps.iter().enumerate() {
            if p.mut_ref {
                self.err(codes::E0314, p.span, "closure parameters cannot be `&mut`");
            }
            let t = match p.ty {
                Some(te) => {
                    let t = self.resolve_type_expr(te);
                    if let Some((eps, _, _)) = &exp_fn {
                        if let Some(&et) = eps.get(i) {
                            if !self.unify(t, et) {
                                self.mismatch(p.span, et, t);
                            }
                        }
                    }
                    t
                }
                None => match exp_fn.as_ref().and_then(|(eps, _, _)| eps.get(i).copied()) {
                    Some(t) => t,
                    None => self.fresh(),
                },
            };
            ptys.push(t);
        }
        let ret_ty = match ret {
            Some(te) => self.resolve_type_expr(te),
            None => match &exp_fn {
                Some((_, r, _)) => *r,
                None => self.fresh(),
            },
        };
        let allowed = exp_fn.as_ref().map(|(_, _, eff)| *eff);
        self.closures.push(super::ClosureFrame {
            ret: ret_ty,
            allowed,
            used: EffectSet::PURE,
            captures: Vec::new(),
            saved_loop_depth: self.loop_depth,
            expr: e,
        });
        self.loop_depth = 0;
        let saved_div = self.diverges;
        self.diverges = false;
        self.push_scope();
        for (p, &t) in ps.iter().zip(ptys.iter()) {
            self.declare_local(p.name.sym, t, LocalKind::Param, p.name.span);
        }
        let vt = self.check_block_value(body, Some(ret_ty));
        self.record(body, vt);
        self.pop_scope();
        self.diverges = saved_div;
        let frame = self.closures.pop().unwrap();
        self.loop_depth = frame.saved_loop_depth;
        let _ = frame.expr;
        self.tables.closure_captures.insert(e, frame.captures);
        let eff = allowed.unwrap_or(frame.used);
        self.tys.func(ptys, ret_ty, eff)
    }
}

/// The closest candidate by edit distance, if reasonably close.
pub fn closest<'a>(name: &str, cands: impl Iterator<Item = &'a str>) -> Option<String> {
    let mut best: Option<(usize, &str)> = None;
    for c in cands {
        if c == name || c.is_empty() {
            continue;
        }
        let d = edit_distance(name, c);
        let limit = (name.len().max(c.len()) / 3).max(1);
        if d <= limit && best.is_none_or(|(bd, _)| d < bd) {
            best = Some((d, c));
        }
    }
    best.map(|(_, c)| c.to_string())
}

pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}
