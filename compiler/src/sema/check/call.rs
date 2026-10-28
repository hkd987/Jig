//! Calls, method resolution, field access, type paths, places and `&mut` rules.

use super::expr::closest;
use super::{Ctx, Cx};
use crate::diag::{codes, Diagnostic, Span};
use crate::sema::collect::builtin_type;
use crate::sema::defs::*;
use crate::sema::ty::*;
use crate::syntax::ast::{self, ExprId, ExprKind, TypeExprKind};
use crate::util::{sym, List, Symbol};

/// A projection step in a place path, for `&mut` aliasing checks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Proj {
    Field(Symbol),
    Tuple(u32),
    Index,
}

/// The receiver of a method call, when it is passed as `&mut`.
struct RecvPlace {
    expr: ExprId,
}

impl Cx<'_> {
    // ---------- type expressions inside bodies ----------

    /// Resolve a type annotation written inside a body.
    pub(crate) fn resolve_type_expr(&mut self, te: ast::TypeExprId) -> Ty {
        self.resolve_type_expr_mode(te, false)
    }

    /// Like `resolve_type_expr`, but missing type arguments of a generic struct become
    /// inference variables (for struct literals and type paths).
    pub(crate) fn resolve_type_expr_infer(&mut self, te: ast::TypeExprId) -> Ty {
        self.resolve_type_expr_mode(te, true)
    }

    fn resolve_type_expr_mode(&mut self, te: ast::TypeExprId, infer_args: bool) -> Ty {
        let t = *self.ast.ty(te);
        match t.kind {
            TypeExprKind::Error => self.tys.error,
            TypeExprKind::Tuple(list) => {
                let ids: Vec<ast::TypeExprId> = self.ast.type_lists.get(list).to_vec();
                let ts: Vec<Ty> = ids.into_iter().map(|x| self.resolve_type_expr(x)).collect();
                self.tys.tuple(ts)
            }
            TypeExprKind::Fn { params, ret, effects } => {
                let ids: Vec<ast::TypeExprId> = self.ast.type_lists.get(params).to_vec();
                let ps: Vec<Ty> = ids.into_iter().map(|x| self.resolve_type_expr(x)).collect();
                let r = match ret {
                    Some(r) => self.resolve_type_expr(r),
                    None => self.tys.unit,
                };
                let eff = match effects {
                    Some(list) => self.effects_of(list),
                    None => EffectSet::PURE,
                };
                self.tys.func(ps, r, eff)
            }
            TypeExprKind::Refined { base, .. } => self.resolve_type_expr_mode(base, infer_args),
            TypeExprKind::Named { pkg, name, args, unit } => {
                let arg_ids: Vec<ast::TypeExprId> = self.ast.type_lists.get(args).to_vec();
                let mut targs: Vec<Ty> = arg_ids.into_iter().map(|x| self.resolve_type_expr(x)).collect();
                let def = match pkg {
                    Some(p) => {
                        let Some(&(target, _)) = self.file_imports().get(&p.sym) else {
                            self.err(codes::E0201, p.span, format!("cannot find package `{}`", p.sym));
                            return self.tys.error;
                        };
                        self.imports_used.insert((self.file, p.sym));
                        match self.pkgs[target].scope.get(&name.sym).copied() {
                            Some(d) => {
                                if !self.def_is_pub(d) {
                                    self.err(
                                        codes::E0205,
                                        name.span,
                                        format!("`{}` is private to package `{}`", name.sym, self.pkgs[target].path),
                                    );
                                }
                                d
                            }
                            None => {
                                self.err(
                                    codes::E0201,
                                    name.span,
                                    format!("package `{}` has no type `{}`", self.pkgs[target].path, name.sym),
                                );
                                return self.tys.error;
                            }
                        }
                    }
                    None => {
                        if let Some(i) = self.generics.iter().rposition(|g| g.name == name.sym) {
                            return self.tys.param(i as u32, name.sym);
                        }
                        if let Some(b) = builtin_type(name.sym) {
                            let unit_id = match unit {
                                Some(f) => self.unit_of_factors(f),
                                None => NO_UNIT,
                            };
                            let want = match b {
                                BuiltinTy::List | BuiltinTy::Set => 1,
                                BuiltinTy::Map => 2,
                                _ => 0,
                            };
                            if targs.is_empty() && want > 0 && infer_args {
                                targs = (0..want).map(|_| self.fresh()).collect();
                            }
                            if targs.len() != want {
                                self.err(
                                    codes::E0313,
                                    t.span,
                                    format!(
                                        "`{}` takes {want} type argument{}",
                                        name.sym,
                                        if want == 1 { "" } else { "s" }
                                    ),
                                );
                                return self.tys.error;
                            }
                            return match b {
                                BuiltinTy::Int(k) => self.tys.int_ty(k, unit_id),
                                BuiltinTy::Float => self.tys.intern(TyKind::Float(unit_id)),
                                BuiltinTy::Bool => self.tys.bool,
                                BuiltinTy::Char => self.tys.char,
                                BuiltinTy::Str => self.tys.str,
                                BuiltinTy::Never => self.tys.never,
                                BuiltinTy::List => self.tys.list(targs[0]),
                                BuiltinTy::Set => self.tys.set(targs[0]),
                                BuiltinTy::Map => self.tys.map_ty(targs[0], targs[1]),
                            };
                        }
                        match self.lookup_def(name.sym) {
                            Some(d) => d,
                            None => {
                                self.err(codes::E0201, name.span, format!("cannot find type `{}`", name.sym));
                                return self.tys.error;
                            }
                        }
                    }
                };
                match def {
                    Def::Adt(a) => {
                        let n = self.defs.adts[a].generics.len();
                        if targs.is_empty() && n > 0 && infer_args {
                            targs = (0..n).map(|_| self.fresh()).collect();
                        }
                        if targs.len() != n {
                            self.err(
                                codes::E0313,
                                t.span,
                                format!(
                                    "`{}` takes {n} type argument{}, found {}",
                                    name.sym,
                                    if n == 1 { "" } else { "s" },
                                    targs.len()
                                ),
                            );
                            return self.tys.error;
                        }
                        self.tys.adt(a, targs)
                    }
                    Def::Alias(al) => {
                        let n = self.defs.aliases[al].generics.len();
                        if targs.is_empty() && n > 0 && infer_args {
                            targs = (0..n).map(|_| self.fresh()).collect();
                        }
                        if targs.len() != n {
                            self.err(codes::E0313, t.span, format!("`{}` takes {n} type arguments", name.sym));
                            return self.tys.error;
                        }
                        let at = self.defs.aliases[al].ty;
                        self.tys.subst(at, &targs)
                    }
                    Def::Iface(_) => {
                        self.err(
                            codes::E0325,
                            name.span,
                            format!("interface `{}` is not a type; use it as a bound", name.sym),
                        );
                        self.tys.error
                    }
                    _ => {
                        self.err(codes::E0325, name.span, format!("`{}` is not a type", name.sym));
                        self.tys.error
                    }
                }
            }
        }
    }

    fn unit_of_factors(&mut self, f: List<ast::UnitFactor>) -> UnitId {
        let fs: Vec<ast::UnitFactor> = self.ast.unit_factors.get(f).to_vec();
        let mut out = Vec::new();
        for x in fs {
            let known = self.pkgs.iter().any(|p| p.scope.get(&x.name.sym) == Some(&Def::Unit));
            if !known {
                self.err(
                    codes::E0201,
                    x.name.span,
                    format!("cannot find unit `{}`; declare it with `unit {}`", x.name.sym, x.name.sym),
                );
            }
            out.push((x.name.sym, x.exp));
        }
        self.tys.unit_id(&out)
    }

    fn effects_of(&mut self, list: List<ast::Name>) -> EffectSet {
        let names: Vec<ast::Name> = self.ast.names.get(list).to_vec();
        let mut set = EffectSet::PURE;
        for n in names {
            match EffectSet::from_name(n.sym.as_str()) {
                Some(e) => set = set.union(e),
                None => self.err(codes::E0402, n.span, format!("unknown effect `{}`", n.sym)),
            }
        }
        set
    }

    pub(crate) fn def_is_pub(&self, def: Def) -> bool {
        match def {
            Def::Fn(f) => self.defs.fns[f].is_pub,
            Def::Adt(a) => self.defs.adts[a].is_pub,
            Def::Iface(i) => self.defs.ifaces[i].is_pub,
            Def::Const(c) => self.defs.consts[c].is_pub,
            Def::Alias(a) => self.defs.aliases[a].is_pub,
            Def::Unit | Def::Builtin(_) => true,
        }
    }

    /// If `e` names a type (`Shape`, `geom.Shape`, `List<Int>`), return it. Never reports.
    pub(crate) fn try_type_path(&mut self, e: ExprId) -> Option<Ty> {
        match self.ast.exprs[e].kind {
            ExprKind::Name(n) => {
                if self.lookup_local(n).is_some() {
                    return None;
                }
                if let Some(i) = self.generics.iter().rposition(|g| g.name == n) {
                    return Some(self.tys.param(i as u32, n));
                }
                if let Some(b) = builtin_type(n) {
                    return Some(match b {
                        BuiltinTy::Int(k) => self.tys.int_ty(k, NO_UNIT),
                        BuiltinTy::Float => self.tys.float,
                        BuiltinTy::Bool => self.tys.bool,
                        BuiltinTy::Char => self.tys.char,
                        BuiltinTy::Str => self.tys.str,
                        BuiltinTy::Never => self.tys.never,
                        BuiltinTy::List => {
                            let v = self.fresh();
                            self.tys.list(v)
                        }
                        BuiltinTy::Set => {
                            let v = self.fresh();
                            self.tys.set(v)
                        }
                        BuiltinTy::Map => {
                            let (k, v) = (self.fresh(), self.fresh());
                            self.tys.map_ty(k, v)
                        }
                    });
                }
                match self.lookup_def(n)? {
                    Def::Adt(a) => {
                        let n = self.defs.adts[a].generics.len();
                        let args: Vec<Ty> = (0..n).map(|_| self.fresh()).collect();
                        Some(self.tys.adt(a, args))
                    }
                    Def::Alias(al) => {
                        let n = self.defs.aliases[al].generics.len();
                        let args: Vec<Ty> = (0..n).map(|_| self.fresh()).collect();
                        let t = self.defs.aliases[al].ty;
                        Some(self.tys.subst(t, &args))
                    }
                    _ => None,
                }
            }
            ExprKind::Field { base, name } => {
                let ExprKind::Name(p) = self.ast.exprs[base].kind else { return None };
                if self.lookup_local(p).is_some() {
                    return None;
                }
                let (target, _) = *self.file_imports().get(&p)?;
                let def = self.pkgs[target].scope.get(&name.sym).copied()?;
                let t = match def {
                    Def::Adt(a) => {
                        let n = self.defs.adts[a].generics.len();
                        let args: Vec<Ty> = (0..n).map(|_| self.fresh()).collect();
                        self.tys.adt(a, args)
                    }
                    Def::Alias(al) => {
                        let n = self.defs.aliases[al].generics.len();
                        let args: Vec<Ty> = (0..n).map(|_| self.fresh()).collect();
                        let t = self.defs.aliases[al].ty;
                        self.tys.subst(t, &args)
                    }
                    _ => return None,
                };
                self.imports_used.insert((self.file, p));
                if !self.def_is_pub(def) {
                    self.err(
                        codes::E0205,
                        name.span,
                        format!("`{}` is private to package `{}`", name.sym, self.pkgs[target].path),
                    );
                }
                Some(t)
            }
            ExprKind::TypeArgs { base, args } => {
                // Build the type with explicit arguments.
                let bt = self.try_type_path(base)?;
                let ids: Vec<ast::TypeExprId> = self.ast.type_lists.get(args).to_vec();
                let targs: Vec<Ty> = ids.into_iter().map(|x| self.resolve_type_expr(x)).collect();
                let r = self.resolve(bt);
                let built = match self.tys.kind(r).clone() {
                    TyKind::Adt(a, old) if old.len() == targs.len() => self.tys.adt(a, targs),
                    TyKind::List(_) if targs.len() == 1 => self.tys.list(targs[0]),
                    TyKind::Set(_) if targs.len() == 1 => self.tys.set(targs[0]),
                    TyKind::Map(..) if targs.len() == 2 => self.tys.map_ty(targs[0], targs[1]),
                    _ => {
                        let span = self.ast.exprs[e].span;
                        self.err(codes::E0313, span, "wrong number of type arguments");
                        self.tys.error
                    }
                };
                Some(built)
            }
            _ => None,
        }
    }

    fn type_args_of(&self, t: Ty) -> Vec<Ty> {
        match self.tys.kind(t) {
            TyKind::Adt(_, args) => args.to_vec(),
            TyKind::List(x) | TyKind::Set(x) => vec![*x],
            TyKind::Map(k, v) => vec![*k, *v],
            _ => vec![],
        }
    }

    // ---------- field access ----------

    pub(crate) fn check_field(&mut self, e: ExprId, base: ExprId, name: ast::Name, expected: Option<Ty>) -> Ty {
        let span = self.ast.exprs[e].span;
        // Package member.
        if let ExprKind::Name(p) = self.ast.exprs[base].kind {
            if self.lookup_local(p).is_none() {
                if let Some(&(target, _)) = self.file_imports().get(&p) {
                    self.imports_used.insert((self.file, p));
                    self.set_res(base, Res::Pkg(target));
                    return match self.pkgs[target].scope.get(&name.sym).copied() {
                        Some(def) => {
                            if !self.def_is_pub(def) {
                                self.err(
                                    codes::E0205,
                                    name.span,
                                    format!("`{}` is private to package `{}`", name.sym, self.pkgs[target].path),
                                );
                            }
                            self.value_of_def(e, def, name.sym, span, None, expected)
                        }
                        None => {
                            let cands: Vec<&str> = self.pkgs[target].scope.keys().map(|s| s.as_str()).collect();
                            let mut d = Diagnostic::error(
                                codes::E0201,
                                name.span,
                                format!("package `{}` has no item `{}`", self.pkgs[target].path, name.sym),
                            );
                            if let Some(b) = closest(name.sym.as_str(), cands.into_iter()) {
                                d = d.fix(format!("did you mean `{b}`?"), name.span, b);
                            }
                            self.push(d);
                            self.tys.error
                        }
                    };
                }
            }
        }
        // Type path: `Shape.Empty`, `Shape.Circle` (constructor value).
        if let Some(t) = self.try_type_path(base) {
            self.record(base, t);
            self.set_res(base, Res::Type(t));
            let r = self.resolve(t);
            if let TyKind::Adt(a, args) = self.tys.kind(r).clone() {
                if let Some(vi) = self.defs.adts[a].variants().iter().position(|v| v.name == name.sym) {
                    return self.variant_value(e, a, vi as u32, Some(args.to_vec()), expected);
                }
            }
            if let Some(key) = TyKey::of(self.tys, r) {
                if let Some(f) = self.defs.method(key, name.sym) {
                    if self.defs.fns[f].self_mode.is_none() {
                        let recv = self.type_args_of(r);
                        let inst = self.instantiate(f, &recv, None, span);
                        let fd = &self.defs.fns[f];
                        let ptys: Vec<Ty> = fd.params.iter().map(|p| p.ty).collect();
                        let (ret, eff) = (fd.ret, fd.effects);
                        let ptys: Vec<Ty> = ptys.into_iter().map(|t| self.tys.subst(t, &inst)).collect();
                        let ret = self.tys.subst(ret, &inst);
                        self.set_res(e, Res::Fn(f));
                        self.set_inst(e, inst.into_boxed_slice());
                        self.add_ref(f);
                        return self.tys.func(ptys, ret, eff);
                    }
                }
            }
            let ts = self.show(t);
            self.err(codes::E0207, name.span, format!("`{ts}` has no variant or associated function `{}`", name.sym));
            return self.tys.error;
        }
        // Value field.
        let bt = self.check_expr(base, None);
        let r = self.resolve(bt);
        match self.tys.kind(r).clone() {
            TyKind::Adt(a, args) if !self.defs.adts[a].is_enum() => {
                let ad = &self.defs.adts[a];
                if let Some(i) = ad.fields().iter().position(|f| f.name == name.sym) {
                    let f = &ad.fields()[i];
                    if ad.pkg != self.pkg && !f.is_pub {
                        let sname = ad.name;
                        self.err(codes::E0206, name.span, format!("field `{}` of `{sname}` is private", name.sym));
                    }
                    let ft = f.ty;
                    self.set_res(e, Res::Field(i as u32));
                    self.note_field_read(a, i as u32);
                    return self.tys.subst(ft, &args);
                }
                self.no_field(r, name)
            }
            TyKind::Error => self.tys.error,
            TyKind::Infer(_) => {
                self.err(codes::E0305, span, "type annotations needed: the value's type is not known here");
                self.tys.error
            }
            _ => self.no_field(r, name),
        }
    }

    fn no_field(&mut self, t: Ty, name: ast::Name) -> Ty {
        let ts = self.show(t);
        let is_method = TyKey::of(self.tys, t).and_then(|k| self.defs.method(k, name.sym)).is_some();
        let mut d = Diagnostic::error(codes::E0206, name.span, format!("`{ts}` has no field `{}`", name.sym));
        if is_method {
            d = d.note(format!("`{}` is a method; call it: `{}()`", name.sym, name.sym));
        } else if let TyKind::Adt(a, _) = self.tys.kind(t) {
            let names: Vec<&str> = self.defs.adts[*a].fields().iter().map(|f| f.name.as_str()).collect();
            if let Some(b) = closest(name.sym.as_str(), names.into_iter()) {
                d = d.fix(format!("did you mean `{b}`?"), name.span, b);
            }
        }
        self.push(d);
        self.tys.error
    }

    // ---------- calls ----------

    pub(crate) fn check_call(&mut self, e: ExprId, callee: ExprId, args: List<ExprId>, expected: Option<Ty>) -> Ty {
        let span = self.ast.exprs[e].span;
        let arg_ids: Vec<ExprId> = self.ast.expr_lists.get(args).to_vec();
        match self.ast.exprs[callee].kind {
            ExprKind::Field { base, name } => {
                return self.check_field_call(e, callee, base, name, &arg_ids, expected);
            }
            ExprKind::Name(n) if self.lookup_local(n).is_none() => {
                if n == sym::OLD {
                    if self.ctx != Ctx::Ensures {
                        self.err(codes::E0320, span, "`old(...)` is only available in `ensures` clauses");
                    }
                    self.set_res(callee, Res::Old);
                    if arg_ids.len() != 1 {
                        self.err(codes::E0302, span, "`old` takes one argument");
                        return self.tys.error;
                    }
                    let t = self.check_expr(arg_ids[0], expected);
                    self.record(callee, t);
                    return t;
                }
                if !(n == sym::RESULT && self.ctx == Ctx::Ensures) {
                    match self.lookup_def(n) {
                        Some(Def::Fn(f)) => {
                            self.set_res(callee, Res::Fn(f));
                            let t = self.call_fn(e, callee, f, Vec::new(), None, &arg_ids, None, expected);
                            return t;
                        }
                        Some(Def::Adt(_) | Def::Alias(_)) => {
                            self.push(
                                Diagnostic::error(codes::E0326, span, format!("`{n}` is a type and cannot be called"))
                                    .note(format!("construct a struct with `{n} {{ field: value }}`")),
                            );
                            for a in arg_ids {
                                self.check_expr(a, None);
                            }
                            return self.tys.error;
                        }
                        _ => {}
                    }
                    if let Some((adt, v)) = self.prelude_variant(n) {
                        return self.construct_variant(e, callee, adt, v, None, &arg_ids, expected);
                    }
                }
            }
            ExprKind::TypeArgs { base, args: targs } => {
                if let ExprKind::Name(n) = self.ast.exprs[base].kind {
                    if self.lookup_local(n).is_none() {
                        if let Some(Def::Fn(f)) = self.lookup_def(n) {
                            let ids: Vec<ast::TypeExprId> = self.ast.type_lists.get(targs).to_vec();
                            let explicit: Vec<Ty> = ids.into_iter().map(|x| self.resolve_type_expr(x)).collect();
                            self.set_res(callee, Res::Fn(f));
                            self.set_res(base, Res::Fn(f));
                            return self.call_fn(e, callee, f, Vec::new(), Some(explicit), &arg_ids, None, expected);
                        }
                    }
                }
            }
            _ => {}
        }
        let ct = self.check_expr(callee, None);
        self.call_value(e, ct, &arg_ids, span)
    }

    fn call_value(&mut self, e: ExprId, ct: Ty, args: &[ExprId], span: Span) -> Ty {
        let r = self.resolve(ct);
        match self.tys.kind(r).clone() {
            TyKind::Fn(ps, ret, eff) => {
                if ps.len() != args.len() {
                    self.err(
                        codes::E0302,
                        span,
                        format!(
                            "this function takes {} argument{}, but {} {} given",
                            ps.len(),
                            if ps.len() == 1 { "" } else { "s" },
                            args.len(),
                            if args.len() == 1 { "was" } else { "were" }
                        ),
                    );
                }
                for (i, &a) in args.iter().enumerate() {
                    if let ExprKind::MutArg(inner) = self.ast.exprs[a].kind {
                        let asp = self.ast.exprs[a].span;
                        self.err(codes::E0314, asp, "function values take their arguments by value; remove `&mut`");
                        self.check_expr(inner, None);
                        continue;
                    }
                    let pt = ps.get(i).copied();
                    self.check_expr(a, pt);
                }
                self.use_effects(eff, span, "calling this function value");
                ret
            }
            TyKind::Error => {
                for &a in args {
                    self.check_expr(a, None);
                }
                self.tys.error
            }
            _ => {
                let s = self.show(ct);
                let mut d = Diagnostic::error(codes::E0303, span, format!("`{s}` is not a function"));
                // `max(a, b)` where a parameter or variable named `max` hides the prelude.
                let callee = match self.ast.exprs[e].kind {
                    ExprKind::Call { callee, .. } => Some(callee),
                    _ => None,
                };
                if let Some(ExprKind::Name(n)) = callee.map(|c| self.ast.exprs[c].kind) {
                    if self.pkgs[self.prelude].scope.contains_key(&n) {
                        d = d.note(format!(
                            "the local `{n}` hides the prelude function `{n}`; rename the local to call it"
                        ));
                    }
                }
                self.diags.push(d);
                for &a in args {
                    self.check_expr(a, None);
                }
                self.tys.error
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn construct_variant(
        &mut self,
        e: ExprId,
        callee: ExprId,
        adt: AdtId,
        v: u32,
        targs: Option<Vec<Ty>>,
        args: &[ExprId],
        expected: Option<Ty>,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        let n = self.defs.adts[adt].generics.len();
        let targs: Vec<Ty> = match targs {
            Some(t) => t,
            None => (0..n).map(|_| self.fresh()).collect(),
        };
        let ty = self.tys.adt(adt, targs.clone());
        if let Some(exp) = expected {
            let _ = self.unify(ty, exp);
        }
        self.set_res(callee, Res::Variant(adt, v));
        self.set_inst(callee, targs.clone().into_boxed_slice());
        self.note_variant_built(adt, v);
        let fields: Vec<Ty> = self.defs.adts[adt].variants()[v as usize].fields.iter().map(|f| f.ty).collect();
        let vname = self.defs.adts[adt].variants()[v as usize].name;
        if fields.is_empty() {
            self.err(codes::E0303, span, format!("variant `{vname}` has no fields; write it without parentheses"));
        } else if fields.len() != args.len() {
            self.err(
                codes::E0302,
                span,
                format!(
                    "variant `{vname}` takes {} value{}, but {} {} given",
                    fields.len(),
                    if fields.len() == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
            );
        }
        for (i, &a) in args.iter().enumerate() {
            let ft = fields.get(i).map(|&t| self.tys.subst(t, &targs));
            self.check_expr(a, ft);
        }
        let ct = {
            let ps: Vec<Ty> = fields.iter().map(|&t| self.tys.subst(t, &targs)).collect();
            self.tys.func(ps, ty, EffectSet::PURE)
        };
        self.record(callee, ct);
        ty
    }

    fn check_field_call(
        &mut self,
        e: ExprId,
        callee: ExprId,
        base: ExprId,
        name: ast::Name,
        args: &[ExprId],
        expected: Option<Ty>,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        // `pkg.func(...)`
        if let ExprKind::Name(p) = self.ast.exprs[base].kind {
            if self.lookup_local(p).is_none() {
                if let Some(&(target, _)) = self.file_imports().get(&p) {
                    self.imports_used.insert((self.file, p));
                    self.set_res(base, Res::Pkg(target));
                    match self.pkgs[target].scope.get(&name.sym).copied() {
                        Some(Def::Fn(f)) => {
                            if !self.defs.fns[f].is_pub {
                                self.err(
                                    codes::E0205,
                                    name.span,
                                    format!("`{}` is private to package `{}`", name.sym, self.pkgs[target].path),
                                );
                            }
                            self.set_res(callee, Res::Fn(f));
                            return self.call_fn(e, callee, f, Vec::new(), None, args, None, expected);
                        }
                        Some(_) => {
                            // Not a function: fall through to a value call (e.g. a constant).
                        }
                        None => {
                            let cands: Vec<&str> = self.pkgs[target].scope.keys().map(|s| s.as_str()).collect();
                            let mut d = Diagnostic::error(
                                codes::E0201,
                                name.span,
                                format!("package `{}` has no function `{}`", self.pkgs[target].path, name.sym),
                            );
                            if let Some(b) = closest(name.sym.as_str(), cands.into_iter()) {
                                d = d.fix(format!("did you mean `{b}`?"), name.span, b);
                            }
                            self.push(d);
                            for &a in args {
                                self.check_expr(a, None);
                            }
                            return self.tys.error;
                        }
                    }
                }
            }
        }
        // `Type.assoc(...)` / `Enum.Variant(...)`
        if let Some(t) = self.try_type_path(base) {
            self.record(base, t);
            self.set_res(base, Res::Type(t));
            let r = self.resolve(t);
            if let TyKind::Adt(a, targs) = self.tys.kind(r).clone() {
                if let Some(vi) = self.defs.adts[a].variants().iter().position(|v| v.name == name.sym) {
                    return self.construct_variant(e, callee, a, vi as u32, Some(targs.to_vec()), args, expected);
                }
            }
            if let Some(key) = TyKey::of(self.tys, r) {
                if let Some(f) = self.defs.method(key, name.sym) {
                    if self.defs.fns[f].self_mode.is_some() {
                        let ts = self.show(t);
                        self.push(Diagnostic::error(
                            codes::E0327,
                            name.span,
                            format!("`{}` is a method of `{ts}`; call it on a value: `x.{}(...)`", name.sym, name.sym),
                        ));
                        return self.tys.error;
                    }
                    let recv = self.type_args_of(r);
                    self.set_res(callee, Res::Fn(f));
                    return self.call_fn(e, callee, f, recv, None, args, None, expected);
                }
            }
            let ts = self.show(t);
            let mut d = Diagnostic::error(
                codes::E0207,
                name.span,
                format!("`{ts}` has no variant or associated function `{}`", name.sym),
            );
            if let TyKind::Adt(a, _) = self.tys.kind(r) {
                let names: Vec<&str> = self.defs.adts[*a].variants().iter().map(|v| v.name.as_str()).collect();
                if let Some(b) = closest(name.sym.as_str(), names.into_iter()) {
                    d = d.fix(format!("did you mean `{b}`?"), name.span, b);
                }
            }
            self.push(d);
            for &a in args {
                self.check_expr(a, None);
            }
            return self.tys.error;
        }
        // Method call on a value.
        let bt = self.check_expr(base, None);
        let r = self.resolve(bt);
        let kind = self.tys.kind(r).clone();
        if matches!(kind, TyKind::Error) {
            // Already reported; avoid follow-on lints about the receiver.
            if let Some(root) = self.place_root(base) {
                self.body.locals[root].mutated = true;
            }
            for &a in args {
                self.check_expr(a, None);
            }
            return self.tys.error;
        }
        let kind = match kind {
            TyKind::Infer(v) if self.vars[v as usize].1 != super::VarKind::General => {
                // A numeric literal: default it, as it would be at the end of the body.
                let d = if self.vars[v as usize].1 == super::VarKind::Int { self.tys.int } else { self.tys.float };
                let _ = self.unify(r, d);
                self.tys.kind(d).clone()
            }
            k => k,
        };
        let r = self.resolve(r);
        if matches!(kind, TyKind::Infer(_)) {
            self.push(
                Diagnostic::error(
                    codes::E0305,
                    self.ast.exprs[base].span,
                    format!("type annotations needed before calling `.{}()`", name.sym),
                )
                .note("annotate the variable, e.g. `let xs: List<Int> = []`"),
            );
            for &a in args {
                self.check_expr(a, None);
            }
            return self.tys.error;
        }
        // Numbers with units: look up on the plain type, re-apply the unit to the result.
        let unit = self.tys.unit_of(r).unwrap_or(NO_UNIT);
        let lookup_ty = if unit != NO_UNIT { self.tys.with_unit(r, NO_UNIT) } else { r };
        if let Some(key) = TyKey::of(self.tys, lookup_ty) {
            if let Some(f) = self.defs.method(key, name.sym) {
                let fd = &self.defs.fns[f];
                if fd.self_mode.is_none() {
                    let ts = self.show(r);
                    self.err(
                        codes::E0327,
                        name.span,
                        format!("`{}` is an associated function; call it as `{ts}.{}(...)`", name.sym, name.sym),
                    );
                    return self.tys.error;
                }
                // A custom `str` customizes `Show` everywhere, like the derived one it replaces.
                if !fd.is_pub && fd.pkg != self.pkg && name.sym != sym::STR_METHOD {
                    self.err(codes::E0205, name.span, format!("method `{}` is private", name.sym));
                }
                let recv_place = if fd.self_mode == Some(Mode::MutRef) {
                    self.require_mut_place(base, "call a `&mut self` method on");
                    self.tables.autoref.insert(callee, true);
                    Some(RecvPlace { expr: base })
                } else {
                    None
                };
                let recv_args = self.type_args_of(lookup_ty);
                self.set_res(callee, Res::Method(f));
                let t = self.call_fn(e, callee, f, recv_args, None, args, recv_place, expected);
                if unit != NO_UNIT {
                    let rt = self.resolve(t);
                    if rt == lookup_ty {
                        let fname = name.sym.as_str();
                        if fname == "sqrt" {
                            return self.sqrt_unit(r, span);
                        }
                        return r;
                    }
                }
                return t;
            }
        }
        // Through a generic bound.
        if let TyKind::Param(i, pname) = kind {
            let bounds = self.generics.get(i as usize).map(|g| g.bounds.clone()).unwrap_or_default();
            for b in &bounds {
                if let Bound::Iface(iid) = b {
                    let ms = self.defs.ifaces[*iid].methods.clone();
                    if let Some((mi, &mf)) = ms.iter().enumerate().find(|(_, &m)| self.defs.fns[m].name == name.sym) {
                        self.set_res(callee, Res::IfaceMethod(*iid, mi as u32));
                        if !self.body.dyn_methods.contains(&name.sym) {
                            self.body.dyn_methods.push(name.sym);
                        }
                        if self.defs.fns[mf].self_mode == Some(Mode::MutRef) {
                            self.require_mut_place(base, "call a `&mut self` method on");
                            self.tables.autoref.insert(callee, true);
                        }
                        return self.call_sig(e, mf, vec![r], args, span);
                    }
                }
            }
            if name.sym == sym::STR_METHOD && bounds.contains(&Bound::Show) {
                self.set_res(callee, Res::BoundMethod(Bound::Show));
                if !self.body.dyn_methods.contains(&name.sym) {
                    self.body.dyn_methods.push(name.sym);
                }
                if !args.is_empty() {
                    self.err(codes::E0302, span, "`str` takes no arguments");
                }
                return self.tys.str;
            }
            let mut d = Diagnostic::error(
                codes::E0207,
                name.span,
                format!("type parameter `{pname}` has no method `{}`", name.sym),
            );
            if name.sym == sym::STR_METHOD {
                d = d.note(format!("add a bound: `{pname}: Show`"));
            } else {
                d = d.note(format!("add an interface bound that declares `{}`", name.sym));
            }
            self.push(d);
            return self.tys.error;
        }
        // Built-in `.str()`.
        if name.sym == sym::STR_METHOD {
            if !args.is_empty() {
                self.err(codes::E0302, span, "`str` takes no arguments");
            }
            if let Err(why) = self.satisfies(r, Bound::Show, &mut Default::default()) {
                let ts = self.show(r);
                self.push(Diagnostic::error(codes::E0312, span, format!("`{ts}` cannot be shown")).note(why));
            }
            self.set_res(callee, Res::BoundMethod(Bound::Show));
            self.body.derived_exprs.push(base);
            return self.tys.str;
        }
        // A field holding a function.
        if let TyKind::Adt(a, targs) = kind.clone() {
            if let Some(i) = self.defs.adts[a].fields().iter().position(|f| f.name == name.sym) {
                let ft = self.defs.adts[a].fields()[i].ty;
                let ft = self.tys.subst(ft, &targs);
                if matches!(self.tys.kind(ft), TyKind::Fn(..)) {
                    self.set_res(callee, Res::Field(i as u32));
                    self.note_field_read(a, i as u32);
                    self.record(callee, ft);
                    return self.call_value(e, ft, args, span);
                }
            }
        }
        let ts = self.show(r);
        let mut d = Diagnostic::error(codes::E0207, name.span, format!("`{ts}` has no method `{}`", name.sym));
        if let Some(key) = TyKey::of(self.tys, lookup_ty) {
            let names: Vec<&str> =
                self.defs.methods.keys().filter(|(k, _)| *k == key).map(|(_, n)| n.as_str()).collect();
            if let Some(b) = closest(name.sym.as_str(), names.into_iter()) {
                d = d.fix(format!("did you mean `{b}`?"), name.span, b);
            }
        }
        self.push(d);
        for &a in args {
            self.check_expr(a, None);
        }
        self.tys.error
    }

    fn sqrt_unit(&mut self, t: Ty, span: Span) -> Ty {
        let u = self.tys.unit_of(t).unwrap_or(NO_UNIT);
        let f: Vec<(Symbol, i8)> = self.tys.unit_factors(u).to_vec();
        if f.iter().any(|(_, e)| e % 2 != 0) {
            let ts = self.show(t);
            self.err(codes::E0317, span, format!("the square root of `{ts}` has no whole-number unit"));
            return self.tys.error;
        }
        let half: Vec<(Symbol, i8)> = f.into_iter().map(|(s, e)| (s, e / 2)).collect();
        let nu = self.tys.unit_id(&half);
        self.tys.with_unit(t, nu)
    }

    /// Call through an interface method signature with `Self := recv[0]`.
    fn call_sig(&mut self, _e: ExprId, f: FnId, recv: Vec<Ty>, args: &[ExprId], span: Span) -> Ty {
        let fd = &self.defs.fns[f];
        let params: Vec<(Ty, Mode)> = fd.params.iter().map(|p| (p.ty, p.mode)).collect();
        let (ret, eff) = (fd.ret, fd.effects);
        let name = fd.name;
        if params.len() != args.len() {
            self.err(
                codes::E0302,
                span,
                format!(
                    "`{name}` takes {} argument{}, but {} {} given",
                    params.len(),
                    if params.len() == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" }
                ),
            );
        }
        let mut places = Vec::new();
        for (i, &a) in args.iter().enumerate() {
            let Some(&(pt, mode)) = params.get(i) else {
                self.check_expr(a, None);
                continue;
            };
            let pt = self.tys.subst(pt, &recv);
            self.check_arg(a, pt, mode, &mut places);
        }
        self.check_disjoint(&places);
        let eff = self.call_effects(eff, args);
        self.use_effects(eff, span, &format!("`{name}`"));
        self.tys.subst(ret, &recv)
    }

    /// The type a call expects for argument `i`: for an effect parameter, its function type
    /// admitting any effects.
    fn arg_expectation(&mut self, pt: Ty, i: usize, eff: EffectSet) -> Ty {
        if !eff.contains(EffectSet::param(i)) {
            return pt;
        }
        match self.tys.kind(pt).clone() {
            TyKind::Fn(ps, r, pe) => {
                self.tys.func(ps.to_vec(), r, pe.minus(EffectSet::param(i)).union(EffectSet::OPEN))
            }
            _ => pt,
        }
    }

    /// Check the argument for an effect parameter: a closure against `pt`, which gives its
    /// parameters their types; anything else on its own, so that its type keeps its effects.
    fn check_effect_arg(&mut self, a: ExprId, pt: Ty, mode: Mode, places: &mut Vec<ExprId>) {
        if mode == Mode::MutRef || matches!(self.ast.exprs[a].kind, ExprKind::Closure { .. } | ExprKind::MutArg(_)) {
            self.check_arg(a, pt, mode, places);
            return;
        }
        let t = self.check_expr(a, None);
        if !self.coerce(t, pt) {
            let sp = self.ast.exprs[a].span;
            self.mismatch(sp, pt, t);
        }
    }

    /// A call's effects: the callee's own, and for each effect parameter those of the
    /// function passed for it.
    fn call_effects(&mut self, eff: EffectSet, args: &[ExprId]) -> EffectSet {
        let mut out = EffectSet(eff.0 & EffectSet::ALL.0);
        for k in eff.params() {
            let Some(&a) = args.get(k) else { continue };
            let t = self.tables.expr_ty[a.0 as usize];
            if let TyKind::Fn(_, _, ae) = self.kind_of(t) {
                out = out.union(ae.minus(EffectSet::OPEN));
            }
        }
        out
    }

    fn check_arg(&mut self, a: ExprId, pt: Ty, mode: Mode, places: &mut Vec<ExprId>) {
        let asp = self.ast.exprs[a].span;
        match (mode, self.ast.exprs[a].kind) {
            (Mode::MutRef, ExprKind::MutArg(place)) => {
                let t = self.check_expr_quiet(place);
                self.require_mut_place(place, "pass as `&mut`");
                if !self.unify(t, pt) {
                    let psp = self.ast.exprs[place].span;
                    self.mismatch(psp, pt, t);
                }
                self.record(a, t);
                places.push(place);
            }
            (Mode::MutRef, _) => {
                let snippet = self.snippet(asp);
                let mut d =
                    Diagnostic::error(codes::E0314, asp, "this parameter is `&mut`: pass a mutable place with `&mut`");
                if !snippet.is_empty() {
                    d = d.fix("add `&mut`", asp, format!("&mut {snippet}"));
                }
                self.push(d);
                self.check_expr(a, Some(pt));
            }
            (Mode::Value, ExprKind::MutArg(inner)) => {
                let isp = self.ast.exprs[inner].span;
                let snippet = self.snippet(isp);
                let mut d =
                    Diagnostic::error(codes::E0314, asp, "this parameter takes its argument by value; remove `&mut`");
                if !snippet.is_empty() {
                    d = d.fix("remove `&mut`", asp, snippet);
                }
                self.push(d);
                let t = self.check_expr(inner, Some(pt));
                self.record(a, t);
            }
            (Mode::Value, _) => {
                self.check_expr(a, Some(pt));
            }
        }
    }

    /// Type-check an expression used as a place without counting it as a read.
    fn check_expr_quiet(&mut self, e: ExprId) -> Ty {
        let root = self.place_root(e);
        let was_used = root.map(|r| self.body.locals[r].used);
        let t = self.check_expr(e, None);
        if let (Some(r), Some(u)) = (root, was_used) {
            self.body.locals[r].used = u || true;
        }
        t
    }

    #[allow(clippy::too_many_arguments)]
    fn call_fn(
        &mut self,
        e: ExprId,
        callee: ExprId,
        f: FnId,
        recv: Vec<Ty>,
        explicit: Option<Vec<Ty>>,
        args: &[ExprId],
        recv_place: Option<RecvPlace>,
        expected: Option<Ty>,
    ) -> Ty {
        let span = self.ast.exprs[e].span;
        let inst = self.instantiate(f, &recv, explicit, span);
        self.set_inst(callee, inst.clone().into_boxed_slice());
        self.add_ref(f);
        let fd = &self.defs.fns[f];
        let params: Vec<(Ty, Mode, Symbol)> = fd.params.iter().map(|p| (p.ty, p.mode, p.name)).collect();
        let (ret, eff) = (fd.ret, fd.effects);
        let display = fd.display_name(self.defs, self.tys);
        let ret = self.tys.subst(ret, &inst);
        if let Some(exp) = expected {
            if self.tys.has_vars(ret) {
                let _ = self.unify(ret, exp);
            }
        }
        if params.len() != args.len() {
            let sig: Vec<String> = params
                .iter()
                .map(|(t, m, n)| {
                    let t = self.tys.subst(*t, &inst);
                    let ts = self.show(t);
                    format!("{n}: {}{ts}", if *m == Mode::MutRef { "&mut " } else { "" })
                })
                .collect();
            self.push(
                Diagnostic::error(
                    codes::E0302,
                    span,
                    format!(
                        "`{display}` takes {} argument{}, but {} {} given",
                        params.len(),
                        if params.len() == 1 { "" } else { "s" },
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .note(format!("signature: {display}({})", sig.join(", "))),
            );
        }
        let mut places: Vec<ExprId> = Vec::new();
        if let Some(rp) = recv_place {
            places.push(rp.expr);
        }
        for (i, &a) in args.iter().enumerate() {
            let Some(&(pt, mode, _)) = params.get(i) else {
                self.check_expr(a, None);
                continue;
            };
            let pt = self.tys.subst(pt, &inst);
            if eff.contains(EffectSet::param(i)) {
                let pt = self.arg_expectation(pt, i, eff);
                self.check_effect_arg(a, pt, mode, &mut places);
            } else {
                self.check_arg(a, pt, mode, &mut places);
            }
        }
        self.check_disjoint(&places);
        if self.ctx != Ctx::Code && params.iter().any(|p| p.1 == Mode::MutRef) {
            self.err(codes::E0321, span, format!("contracts must be pure, but `{display}` mutates its arguments"));
        }
        let call_eff = self.call_effects(eff, args);
        self.use_effects(call_eff, span, &format!("`{display}`"));
        // The callee as this call uses it: the effects its arguments lend it.
        let callee_ty = {
            let mut ps: Vec<Ty> = params.iter().map(|p| self.tys.subst(p.0, &inst)).collect();
            for k in eff.params() {
                if let (Some(p), Some(&a)) = (ps.get_mut(k), args.get(k)) {
                    *p = self.tables.expr_ty[a.0 as usize];
                }
            }
            self.tys.func(ps, ret, call_eff)
        };
        self.record(callee, callee_ty);
        ret
    }

    // ---------- places ----------

    /// The local at the root of a place expression (`a.b[i].c` -> `a`).
    pub(crate) fn place_root(&self, e: ExprId) -> Option<LocalId> {
        match self.ast.exprs[e].kind {
            ExprKind::Name(n) => self.lookup_local(n),
            ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } | ExprKind::Index { base, .. } => {
                self.place_root(base)
            }
            ExprKind::Paren(inner) => self.place_root(inner),
            _ => None,
        }
    }

    fn place_path(&self, e: ExprId, out: &mut Vec<Proj>) -> Option<LocalId> {
        match self.ast.exprs[e].kind {
            ExprKind::Name(n) => self.lookup_local(n),
            ExprKind::Field { base, name } => {
                let r = self.place_path(base, out);
                out.push(Proj::Field(name.sym));
                r
            }
            ExprKind::TupleField { base, index, .. } => {
                let r = self.place_path(base, out);
                out.push(Proj::Tuple(index));
                r
            }
            ExprKind::Index { base, .. } => {
                let r = self.place_path(base, out);
                out.push(Proj::Index);
                r
            }
            ExprKind::Paren(inner) => self.place_path(inner, out),
            _ => None,
        }
    }

    /// Require `e` to be a mutable place (for assignment, `&mut` arguments, `&mut self`).
    pub(crate) fn require_mut_place(&mut self, e: ExprId, what: &str) {
        let span = self.ast.exprs[e].span;
        if !self.is_place(e) {
            self.err(codes::E0314, span, format!("can only {what} a variable, a field or a list element"));
            return;
        }
        if self.ctx != Ctx::Code {
            self.err(codes::E0321, span, "contracts must be pure and cannot mutate anything");
            return;
        }
        if self.is_str_index(e) {
            self.err(codes::E0307, span, "strings are immutable; build a new `Str` instead");
            if let Some(root) = self.place_root(e) {
                self.body.locals[root].mutated = true;
            }
            return;
        }
        let Some(root) = self.place_root(e) else { return };
        let l = &self.body.locals[root];
        let (name, kind, lspan, depth) = (l.name, l.kind, l.span, l.depth);
        if depth < self.closure_depth() {
            self.push(
                Diagnostic::error(codes::E0307, span, format!("cannot mutate `{name}` inside a closure"))
                    .note("closures capture copies of variables; return a new value instead"),
            );
            return;
        }
        if !self.body.locals[root].is_mutable() {
            let mut d = Diagnostic::error(codes::E0307, span, format!("cannot {what} `{name}`, which is not mutable"));
            match kind {
                LocalKind::Let => {
                    d = d.label(lspan, "declared with `let`").fix(
                        "declare it with `var`",
                        Span::new(lspan.lo, lspan.lo),
                        "",
                    );
                    d.fixes.clear();
                    d = d.note("declare it with `var` instead of `let`");
                }
                LocalKind::Param => {
                    d = d.note("parameters are immutable; take it as `&mut` or copy it into a `var`");
                }
                LocalKind::SelfValue => {
                    d = d.note("declare the method with `&mut self` to mutate the receiver");
                }
                LocalKind::Pattern | LocalKind::Quant => {
                    d = d.note("pattern bindings are immutable; copy into a `var` to modify");
                }
                _ => {}
            }
            self.push(d);
            return;
        }
        self.body.locals[root].mutated = true;
    }

    fn is_place(&self, e: ExprId) -> bool {
        match self.ast.exprs[e].kind {
            ExprKind::Name(n) => self.lookup_local(n).is_some(),
            ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } | ExprKind::Index { base, .. } => {
                self.is_place(base)
            }
            ExprKind::Paren(inner) => self.is_place(inner),
            _ => false,
        }
    }

    fn is_str_index(&self, e: ExprId) -> bool {
        if let ExprKind::Index { base, .. } = self.ast.exprs[e].kind {
            let t = self.tables.expr_ty[base.0 as usize];
            return matches!(self.tys.kind(self.resolve(t)), TyKind::Str);
        }
        false
    }

    fn check_disjoint(&mut self, places: &[ExprId]) {
        if places.len() < 2 {
            return;
        }
        let paths: Vec<(Option<LocalId>, Vec<Proj>)> = places
            .iter()
            .map(|&p| {
                let mut v = Vec::new();
                let r = self.place_path(p, &mut v);
                (r, v)
            })
            .collect();
        for i in 0..paths.len() {
            for j in 0..i {
                let (ri, pi) = &paths[i];
                let (rj, pj) = &paths[j];
                if ri.is_none() || ri != rj {
                    continue;
                }
                let n = pi.len().min(pj.len());
                let overlap =
                    pi[..n].iter().zip(pj[..n].iter()).all(|(a, b)| a == b || (*a == Proj::Index && *b == Proj::Index));
                if overlap {
                    let (si, sj) = (self.ast.exprs[places[i]].span, self.ast.exprs[places[j]].span);
                    self.push(
                        Diagnostic::error(codes::E0315, si, "this `&mut` argument overlaps another one")
                            .label(sj, "overlapping `&mut` argument")
                            .note("pass non-overlapping places, or restructure so one function call mutates one place"),
                    );
                }
            }
        }
    }
}
