//! Body checking: name resolution inside bodies, bidirectional type inference with
//! unification, effects, mutability and `&mut` rules, divergence, and lints that need types.

mod call;
mod expr;
mod stmt;

use super::defs::*;
use super::ty::*;
use crate::diag::{codes, Code, Diagnostic, Diags, Span};
use crate::syntax::ast::{self, Ast, ExprId, ItemKind};
use crate::util::{FxHashMap, FxHashSet, IndexVec, Symbol};

/// What kind of code is being checked.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ctx {
    /// Ordinary code.
    Code,
    /// `requires`, invariants, refinements: pure, read-only.
    Pure,
    /// `ensures`: pure, and `result`/`old` are available.
    Ensures,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum VarKind {
    General,
    Int,
    Float,
}

pub(crate) struct ClosureFrame {
    ret: Ty,
    /// Effects the closure may perform (`None`: inferred, anything goes).
    allowed: Option<EffectSet>,
    used: EffectSet,
    captures: Vec<LocalId>,
    saved_loop_depth: u32,
    expr: ExprId,
}

pub struct Cx<'a> {
    pub sm: &'a crate::diag::SourceMap,
    pub pkgs: &'a IndexVec<PkgId, Package>,
    pub defs: &'a Defs,
    pub tys: &'a mut TyCtx,
    pub tables: &'a mut Tables,
    pub diags: &'a mut Diags,
    pub prelude: PkgId,
    pub pkg: PkgId,
    pub ast: &'a Ast,
    pub file: usize,
    /// Generic parameters of the item being checked.
    pub generics: Vec<GenericDef>,
    pub ret_ty: Ty,
    pub declared_effects: EffectSet,
    pub ctx: Ctx,
    vars: Vec<(Option<Ty>, VarKind)>,
    trail: Vec<u32>,
    pub(crate) body: Body,
    scope: Vec<(Symbol, LocalId)>,
    scope_marks: Vec<usize>,
    loop_depth: u32,
    /// For each enclosing loop, whether it contains a `break`.
    loop_breaks: Vec<bool>,
    closures: Vec<ClosureFrame>,
    pending_bounds: Vec<(Span, Ty, Bound)>,
    int_lits: Vec<(ExprId, u128, bool)>,
    touched: Vec<ExprId>,
    /// Patterns and instantiations recorded by this body (finalized in `finish`).
    pub(crate) touched_pats: Vec<crate::syntax::ast::PatId>,
    pub(crate) touched_insts: Vec<ExprId>,
    pub(crate) diverges: bool,
    pub imports_used: &'a mut FxHashSet<(usize, Symbol)>,
    /// Report "cannot infer" once per variable.
    reported_vars: FxHashSet<u32>,
    errors_at_start: usize,
    /// Span of the item being checked (signature included).
    item_span: Span,
}

pub fn check_bodies(prog: &mut Program) {
    let Program { sm, pkgs, tables, defs, tys, diags, bodies, test_bodies, prelude, .. } = prog;
    let mut imports_used: Vec<FxHashSet<(usize, Symbol)>> = (0..pkgs.len()).map(|_| FxHashSet::default()).collect();
    // Functions.
    for i in 0..defs.fns.len() {
        let fid = FnId(i as u32);
        let fd = &defs.fns[fid];
        if fd.iface.is_some() {
            continue;
        }
        let pid = fd.pkg;
        let body = {
            let mut cx =
                Cx::new(sm, pkgs, defs, tys, &mut tables[pid], diags, *prelude, pid, &mut imports_used[pid.0 as usize]);
            cx.check_fn(fid)
        };
        bodies[fid] = Some(body);
    }
    // Tests.
    for i in 0..defs.tests.len() {
        let tid = TestId(i as u32);
        let pid = defs.tests[tid].pkg;
        let body = {
            let mut cx =
                Cx::new(sm, pkgs, defs, tys, &mut tables[pid], diags, *prelude, pid, &mut imports_used[pid.0 as usize]);
            cx.check_test(tid)
        };
        test_bodies[tid] = Some(body);
    }
    // Struct invariants, field refinements and alias refinements.
    for i in 0..defs.adts.len() {
        let aid = AdtId(i as u32);
        let pid = defs.adts[aid].pkg;
        let mut cx =
            Cx::new(sm, pkgs, defs, tys, &mut tables[pid], diags, *prelude, pid, &mut imports_used[pid.0 as usize]);
        cx.check_adt_predicates(aid);
    }
    for i in 0..defs.aliases.len() {
        let al = AliasId(i as u32);
        let pid = defs.aliases[al].pkg;
        let mut cx =
            Cx::new(sm, pkgs, defs, tys, &mut tables[pid], diags, *prelude, pid, &mut imports_used[pid.0 as usize]);
        cx.check_alias_predicate(al);
    }
    // Unused imports.
    for (pi, pkg) in pkgs.iter_enumerated() {
        if pkg.is_std {
            continue;
        }
        for (fi, imports) in pkg.imports.iter().enumerate() {
            for (name, &(_, item)) in imports {
                if !imports_used[pi.0 as usize].contains(&(fi, *name)) {
                    let span = pkg.ast.items[item].span;
                    diags.push(
                        Diagnostic::error(
                            codes::E0502,
                            span,
                            format!("unused import `{}`", pkg.ast.items[item].name.sym),
                        )
                        .fix("remove the import", span, ""),
                    );
                }
            }
        }
    }
}

impl<'a> Cx<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        sm: &'a crate::diag::SourceMap,
        pkgs: &'a IndexVec<PkgId, Package>,
        defs: &'a Defs,
        tys: &'a mut TyCtx,
        tables: &'a mut Tables,
        diags: &'a mut Diags,
        prelude: PkgId,
        pkg: PkgId,
        imports_used: &'a mut FxHashSet<(usize, Symbol)>,
    ) -> Cx<'a> {
        let unit = tys.unit;
        let errors_at_start = diags.error_count();
        Cx {
            sm,
            pkgs,
            defs,
            tys,
            tables,
            diags,
            prelude,
            pkg,
            ast: &pkgs[pkg].ast,
            file: 0,
            generics: Vec::new(),
            ret_ty: unit,
            declared_effects: EffectSet::PURE,
            ctx: Ctx::Code,
            vars: Vec::new(),
            trail: Vec::new(),
            body: Body::default(),
            scope: Vec::new(),
            scope_marks: Vec::new(),
            loop_depth: 0,
            loop_breaks: Vec::new(),
            closures: Vec::new(),
            pending_bounds: Vec::new(),
            int_lits: Vec::new(),
            touched: Vec::new(),
            touched_pats: Vec::new(),
            touched_insts: Vec::new(),
            diverges: false,
            imports_used,
            reported_vars: FxHashSet::default(),
            errors_at_start,
            item_span: Span::DUMMY,
        }
    }

    // ---------- diagnostics ----------

    pub(crate) fn err(&mut self, code: Code, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(code, span, msg));
    }

    pub(crate) fn push(&mut self, d: Diagnostic) {
        self.diags.push(d);
    }

    pub(crate) fn show(&mut self, t: Ty) -> String {
        let t = self.zonk(t);
        self.tys.display(t)
    }

    pub(crate) fn mismatch(&mut self, span: Span, expected: Ty, found: Ty) {
        let (e, f) = (self.show(expected), self.show(found));
        let mut d = Diagnostic::error(codes::E0301, span, format!("mismatched types: expected `{e}`, found `{f}`"))
            .primary(format!("expected `{e}`"));
        // Suggestions for common conversions.
        let ek = self.tys.kind(self.resolve(expected)).clone();
        let fk = self.tys.kind(self.resolve(found)).clone();
        let snippet = self.snippet(span);
        match (&ek, &fk) {
            (TyKind::Float(_), TyKind::Int(..))
            | (TyKind::Int(..), TyKind::Float(_))
            | (TyKind::Int(..), TyKind::Int(..)) => {
                if !snippet.is_empty() {
                    d = d.fix(format!("convert with `as {e}`"), span, format!("({snippet}) as {e}"));
                }
            }
            (TyKind::Str, _) if !snippet.is_empty() && !matches!(fk, TyKind::Error) => {
                d = d.fix("convert to a string with `.str()`", span, format!("{snippet}.str()"));
            }
            (TyKind::Adt(a, _), _) if Some(*a) == self.defs.option && !snippet.is_empty() => {
                d = d.fix("wrap the value in `Some(...)`", span, format!("Some({snippet})"));
            }
            (TyKind::Adt(a, _), _) if Some(*a) == self.defs.result && !snippet.is_empty() => {
                d = d.fix("wrap the value in `Ok(...)`", span, format!("Ok({snippet})"));
            }
            _ => {}
        }
        self.push(d);
    }

    pub(crate) fn snippet(&self, span: Span) -> String {
        let s = self.sm.snippet(span);
        if s.len() > 80 || s.contains('\n') {
            String::new()
        } else {
            s.to_string()
        }
    }

    // ---------- inference variables ----------

    pub(crate) fn new_var(&mut self, kind: VarKind) -> Ty {
        let v = self.vars.len() as u32;
        self.vars.push((None, kind));
        self.tys.infer(v)
    }

    pub(crate) fn fresh(&mut self) -> Ty {
        self.new_var(VarKind::General)
    }

    /// Follow variable bindings at the top level.
    pub(crate) fn resolve(&self, mut t: Ty) -> Ty {
        loop {
            match *self.tys.kind(t) {
                TyKind::Infer(v) => match self.vars.get(v as usize).and_then(|x| x.0) {
                    Some(b) => t = b,
                    None => return t,
                },
                _ => return t,
            }
        }
    }

    pub(crate) fn kind_of(&self, t: Ty) -> TyKind {
        let r = self.resolve(t);
        self.tys.kind(r).clone()
    }

    /// Deep-resolve inference variables.
    pub(crate) fn zonk(&mut self, t: Ty) -> Ty {
        let r = self.resolve(t);
        match self.tys.kind(r).clone() {
            TyKind::Tuple(ts) => {
                let v: Vec<Ty> = ts.iter().map(|&x| self.zonk(x)).collect();
                self.tys.tuple(v)
            }
            TyKind::Adt(a, ts) => {
                let v: Vec<Ty> = ts.iter().map(|&x| self.zonk(x)).collect();
                self.tys.adt(a, v)
            }
            TyKind::List(x) => {
                let x = self.zonk(x);
                self.tys.list(x)
            }
            TyKind::Set(x) => {
                let x = self.zonk(x);
                self.tys.set(x)
            }
            TyKind::Map(k, v) => {
                let k = self.zonk(k);
                let v = self.zonk(v);
                self.tys.map_ty(k, v)
            }
            TyKind::Fn(ps, ret, e) => {
                let v: Vec<Ty> = ps.iter().map(|&x| self.zonk(x)).collect();
                let ret = self.zonk(ret);
                self.tys.func(v, ret, e)
            }
            _ => r,
        }
    }

    /// Default unresolved literal variables (Int, Float) at the end of a body.
    fn default_vars(&mut self) {
        for i in 0..self.vars.len() {
            if self.vars[i].0.is_none() {
                match self.vars[i].1 {
                    VarKind::Int => self.vars[i].0 = Some(self.tys.int),
                    VarKind::Float => self.vars[i].0 = Some(self.tys.float),
                    VarKind::General => {}
                }
            }
        }
    }

    pub(crate) fn snapshot(&self) -> usize {
        self.trail.len()
    }

    pub(crate) fn rollback(&mut self, snap: usize) {
        while self.trail.len() > snap {
            let v = self.trail.pop().unwrap();
            self.vars[v as usize].0 = None;
        }
    }

    fn bind(&mut self, v: u32, t: Ty) {
        self.vars[v as usize].0 = Some(t);
        self.trail.push(v);
    }

    fn occurs(&self, v: u32, t: Ty) -> bool {
        let t = self.resolve(t);
        match self.tys.kind(t) {
            TyKind::Infer(w) => *w == v,
            TyKind::Tuple(ts) | TyKind::Adt(_, ts) => ts.iter().any(|&x| self.occurs(v, x)),
            TyKind::List(x) | TyKind::Set(x) => self.occurs(v, *x),
            TyKind::Map(k, x) => self.occurs(v, *k) || self.occurs(v, *x),
            TyKind::Fn(ps, r, _) => ps.iter().any(|&x| self.occurs(v, x)) || self.occurs(v, *r),
            _ => false,
        }
    }

    /// Make two types equal. Rolls back partial bindings on failure.
    pub(crate) fn unify(&mut self, a: Ty, b: Ty) -> bool {
        let snap = self.snapshot();
        let ok = self.unify_inner(a, b, false);
        if !ok {
            self.rollback(snap);
        }
        ok
    }

    /// `actual` may be used where `expected` is required: equality, except that `Never`
    /// converts to anything and a function with fewer effects fits a type with more.
    pub(crate) fn coerce(&mut self, actual: Ty, expected: Ty) -> bool {
        let snap = self.snapshot();
        let ok = self.unify_inner(actual, expected, true);
        if !ok {
            self.rollback(snap);
        }
        ok
    }

    fn unify_inner(&mut self, a: Ty, b: Ty, coerce: bool) -> bool {
        let a = self.resolve(a);
        let b = self.resolve(b);
        if a == b {
            return true;
        }
        let ka = self.tys.kind(a).clone();
        let kb = self.tys.kind(b).clone();
        match (&ka, &kb) {
            (TyKind::Error, _) | (_, TyKind::Error) => true,
            (TyKind::Never, _) if coerce => true,
            (TyKind::Infer(v), TyKind::Infer(w)) => {
                let (kv, kw) = (self.vars[*v as usize].1, self.vars[*w as usize].1);
                match (kv, kw) {
                    (VarKind::General, _) => self.bind(*v, b),
                    (_, VarKind::General) => self.bind(*w, a),
                    (x, y) if x == y => self.bind(*v, b),
                    _ => return false,
                }
                true
            }
            (TyKind::Infer(v), _) => self.bind_checked(*v, b, &kb),
            (_, TyKind::Infer(w)) => self.bind_checked(*w, a, &ka),
            (TyKind::Tuple(x), TyKind::Tuple(y)) => {
                x.len() == y.len() && x.iter().zip(y.iter()).all(|(&p, &q)| self.unify_inner(p, q, false))
            }
            (TyKind::Adt(p, x), TyKind::Adt(q, y)) => {
                p == q && x.iter().zip(y.iter()).all(|(&s, &t)| self.unify_inner(s, t, false))
            }
            (TyKind::List(x), TyKind::List(y)) | (TyKind::Set(x), TyKind::Set(y)) => self.unify_inner(*x, *y, false),
            (TyKind::Map(k1, v1), TyKind::Map(k2, v2)) => {
                self.unify_inner(*k1, *k2, false) && self.unify_inner(*v1, *v2, false)
            }
            (TyKind::Fn(p1, r1, e1), TyKind::Fn(p2, r2, e2)) => {
                let eff_ok = if coerce { e2.contains(*e1) } else { e1 == e2 };
                eff_ok
                    && p1.len() == p2.len()
                    && p1.iter().zip(p2.iter()).all(|(&x, &y)| self.unify_inner(x, y, false))
                    && self.unify_inner(*r1, *r2, coerce)
            }
            _ => false,
        }
    }

    fn bind_checked(&mut self, v: u32, t: Ty, k: &TyKind) -> bool {
        match self.vars[v as usize].1 {
            VarKind::Int if !matches!(k, TyKind::Int(..)) => return false,
            VarKind::Float if !matches!(k, TyKind::Float(_)) => return false,
            _ => {}
        }
        if matches!(k, TyKind::Never) {
            return false;
        }
        if self.occurs(v, t) {
            return false;
        }
        self.bind(v, t);
        true
    }

    /// Join the types of two branches.
    pub(crate) fn join(&mut self, a: Ty, b: Ty, span: Span) -> Ty {
        let ra = self.resolve(a);
        let rb = self.resolve(b);
        if self.tys.is_never(ra) {
            return b;
        }
        if self.tys.is_never(rb) {
            return a;
        }
        if !self.coerce(b, a) {
            self.mismatch(span, a, b);
        }
        a
    }

    // ---------- scopes and locals ----------

    pub(crate) fn push_scope(&mut self) {
        self.scope_marks.push(self.scope.len());
    }

    pub(crate) fn pop_scope(&mut self) {
        let m = self.scope_marks.pop().unwrap_or(0);
        self.scope.truncate(m);
    }

    pub(crate) fn closure_depth(&self) -> u32 {
        self.closures.len() as u32
    }

    pub(crate) fn declare_local(&mut self, name: Symbol, ty: Ty, kind: LocalKind, span: Span) -> LocalId {
        if name.as_str() != "_" && !matches!(kind, LocalKind::SelfValue | LocalKind::SelfMut) {
            if let Some(&(_, prev)) = self.scope.iter().rev().find(|(n, _)| *n == name) {
                let prev_span = self.body.locals[prev].span;
                self.push(
                    Diagnostic::error(codes::E0505, span, format!("`{name}` shadows another variable"))
                        .label(prev_span, "first declared here")
                        .note("choose a different name"),
                );
            }
            check_snake(self, name, span, "variable");
        }
        let id = self.body.locals.push(LocalDef {
            name,
            ty,
            kind,
            span,
            used: false,
            mutated: false,
            depth: self.closure_depth(),
        });
        self.scope.push((name, id));
        id
    }

    pub(crate) fn lookup_local(&self, name: Symbol) -> Option<LocalId> {
        self.scope.iter().rev().find(|(n, _)| *n == name).map(|&(_, id)| id)
    }

    /// Record a use of a local, capturing it into enclosing closures if needed.
    pub(crate) fn use_local(&mut self, id: LocalId) {
        self.body.locals[id].used = true;
        let depth = self.body.locals[id].depth as usize;
        for frame in self.closures.iter_mut().skip(depth) {
            if !frame.captures.contains(&id) {
                frame.captures.push(id);
            }
        }
    }

    // ---------- effects ----------

    /// Perform effects at `span` (a call or a function value invocation).
    pub(crate) fn use_effects(&mut self, eff: EffectSet, span: Span, what: &str) {
        if eff.is_pure() {
            return;
        }
        if self.ctx != Ctx::Code {
            self.push(Diagnostic::error(
                codes::E0321,
                span,
                format!("contracts must be pure, but {what} has effects {}", eff.render()),
            ));
            return;
        }
        if let Some(frame) = self.closures.last_mut() {
            frame.used = frame.used.union(eff);
            if let Some(allowed) = frame.allowed {
                let missing = eff.minus(allowed);
                if !missing.is_pure() {
                    // Count it as used by the function too, so we don't also report the
                    // function's declaration as unused.
                    self.body.used_effects = self.body.used_effects.union(missing);
                    self.push(
                        Diagnostic::error(
                            codes::E0401,
                            span,
                            format!(
                                "{what} performs {} but the closure's type allows only {}",
                                missing.render(),
                                allowed.render()
                            ),
                        )
                        .note("a function value's effects are part of its type"),
                    );
                }
            }
            return;
        }
        self.body.used_effects = self.body.used_effects.union(eff);
        let missing = eff.minus(self.declared_effects);
        if !missing.is_pure() {
            let names = missing.names();
            let mut want = self.declared_effects.union(missing).names();
            want.sort_by_key(|n| super::ty::EFFECT_NAMES.iter().position(|x| x == n));
            self.push(
                Diagnostic::error(
                    codes::E0401,
                    span,
                    format!(
                        "{what} needs effect{} `{}`, which this function does not declare",
                        if names.len() > 1 { "s" } else { "" },
                        names.join("`, `")
                    ),
                )
                .note(format!("declare it: `effects {{{}}}`", want.join(", "))),
            );
        }
    }

    // ---------- bounds ----------

    pub(crate) fn require_bound(&mut self, span: Span, ty: Ty, b: Bound) {
        self.pending_bounds.push((span, ty, b));
    }

    fn check_pending_bounds(&mut self) {
        let pending = std::mem::take(&mut self.pending_bounds);
        for (span, ty, b) in pending {
            let t = self.zonk(ty);
            if self.tys.has_infer(t) {
                continue; // reported as an inference failure
            }
            if let Err(why) = self.satisfies(t, b, &mut FxHashSet::default()) {
                let tn = self.tys.display(t);
                let bn = bound_name(self.defs, b);
                self.push(Diagnostic::error(codes::E0312, span, format!("`{tn}` does not satisfy `{bn}`")).note(why));
            }
        }
    }

    /// Does `t` satisfy bound `b`? `Err` explains why not.
    pub(crate) fn satisfies(&mut self, t: Ty, b: Bound, seen: &mut FxHashSet<(Ty, Bound)>) -> Result<(), String> {
        if !seen.insert((t, b)) {
            return Ok(());
        }
        let k = self.tys.kind(t).clone();
        match (&k, b) {
            (TyKind::Error, _) | (TyKind::Never, _) => Ok(()),
            (TyKind::Param(i, name), _) => {
                let bounds = self.generics.get(*i as usize).map(|g| g.bounds.clone()).unwrap_or_default();
                if bounds.contains(&b) {
                    Ok(())
                } else {
                    Err(format!("add the bound to the type parameter: `{name}: {}`", bound_name(self.defs, b)))
                }
            }
            (TyKind::Fn(..), _) => {
                Err("functions cannot be compared, hashed, shown or used as interface values".into())
            }
            (_, Bound::Iface(iid)) => self.satisfies_iface(t, iid),
            (TyKind::Float(_), Bound::Ord | Bound::Hash) => {
                Err("`Float` is not totally ordered; use `sort_by` with an explicit comparison".into())
            }
            (TyKind::Int(..) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::Str, _) => Ok(()),
            (TyKind::Tuple(ts), _) => {
                for &x in ts.iter() {
                    self.satisfies(x, b, seen)?;
                }
                Ok(())
            }
            (TyKind::List(x) | TyKind::Set(x), _) => self.satisfies(*x, b, seen),
            (TyKind::Map(kk, v), _) => {
                self.satisfies(*kk, b, seen)?;
                self.satisfies(*v, b, seen)
            }
            (TyKind::Adt(a, args), _) => {
                if b == Bound::Show && self.defs.method(TyKey::Adt(*a), crate::util::sym::STR_METHOD).is_some() {
                    return Ok(());
                }
                let a = *a;
                let field_tys: Vec<Ty> = match &self.defs.adts[a].kind {
                    AdtKind::Struct { fields } => fields.iter().map(|f| f.ty).collect(),
                    AdtKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| f.ty)).collect(),
                };
                for ft in field_tys {
                    let ft = self.tys.subst(ft, args);
                    if let Err(e) = self.satisfies(ft, b, seen) {
                        return Err(format!("a field of `{}` does not: {e}", self.defs.adts[a].name));
                    }
                }
                Ok(())
            }
            (TyKind::Infer(_), _) => Ok(()),
        }
    }

    fn satisfies_iface(&mut self, t: Ty, iid: IfaceId) -> Result<(), String> {
        let iface = &self.defs.ifaces[iid];
        let iname = iface.name;
        let Some(key) = TyKey::of(self.tys, t) else {
            return Err(format!("`{}` has no methods", self.tys.display(t)));
        };
        let targs: Vec<Ty> = match self.tys.kind(t).clone() {
            TyKind::Adt(_, args) => args.to_vec(),
            TyKind::List(x) | TyKind::Set(x) => vec![x],
            TyKind::Map(k, v) => vec![k, v],
            _ => vec![],
        };
        for &m in &iface.methods {
            let md = &self.defs.fns[m];
            let Some(cf) = self.defs.method(key, md.name) else {
                return Err(format!("missing method `{}` required by `{iname}`", md.name));
            };
            let cd = &self.defs.fns[cf];
            if cd.self_mode != md.self_mode
                || cd.params.len() != md.params.len()
                || cd.generics.len() != cd.n_recv_generics
            {
                return Err(format!("method `{}` does not match the signature required by `{iname}`", md.name));
            }
            // Compare signatures with Self := t (iface) and the receiver params := targs.
            let n = cd.n_recv_generics;
            if targs.len() != n {
                return Err(format!("method `{}` does not match `{iname}`", md.name));
            }
            let self_sub = [t];
            for (cp, mp) in cd.params.iter().zip(md.params.iter()) {
                let a = self.tys.subst(cp.ty, &targs);
                let b = self.tys.subst(mp.ty, &self_sub);
                if a != b || cp.mode != mp.mode {
                    return Err(format!("parameter `{}` of `{}` does not match `{iname}`", cp.name, md.name));
                }
            }
            let ra = self.tys.subst(cd.ret, &targs);
            let rb = self.tys.subst(md.ret, &self_sub);
            if ra != rb || !md.effects.contains(cd.effects) {
                return Err(format!(
                    "method `{}` does not match the return type or effects required by `{iname}`",
                    md.name
                ));
            }
        }
        Ok(())
    }

    // ---------- finishing ----------

    fn finish(&mut self) {
        self.default_vars();
        // Record final types for every expression this body touched.
        let touched = std::mem::take(&mut self.touched);
        for e in &touched {
            let t = self.tables.expr_ty[e.0 as usize];
            let z = self.zonk(t);
            if self.tys.has_infer(z) && !self.item_has_errors() {
                if let TyKind::Infer(v) = *self.tys.kind(self.first_var(z)) {
                    if self.reported_vars.insert(v) {
                        let span = self.ast.exprs[*e].span;
                        self.push(
                            Diagnostic::error(codes::E0305, span, "type annotations needed")
                                .note("add a type annotation, e.g. `let xs: List<Int> = []`"),
                        );
                    }
                }
            }
            self.tables.expr_ty[e.0 as usize] = z;
        }
        self.touched = touched;
        let pats = std::mem::take(&mut self.touched_pats);
        for p in pats {
            let t = self.tables.pat_ty[p.0 as usize];
            if self.tys.has_infer(t) {
                self.tables.pat_ty[p.0 as usize] = self.zonk(t);
            }
        }
        for id in self.body.locals.indices().collect::<Vec<_>>() {
            let t = self.body.locals[id].ty;
            self.body.locals[id].ty = self.zonk(t);
        }
        let insts = std::mem::take(&mut self.touched_insts);
        for e in insts {
            let args = self.tables.expr_inst[&e].clone();
            let z: Vec<Ty> = args.iter().map(|&t| self.zonk(t)).collect();
            self.tables.expr_inst.insert(e, z.into_boxed_slice());
        }
        // Integer literal ranges.
        let lits = std::mem::take(&mut self.int_lits);
        for (e, v, neg) in lits {
            let t = self.tables.expr_ty[e.0 as usize];
            if let TyKind::Int(k, _) = *self.tys.kind(t) {
                let val = if neg { -(v as i128) } else { v as i128 };
                if val < k.min() || val > k.max() {
                    let span = self.ast.exprs[e].span;
                    self.err(
                        codes::E0104,
                        span,
                        format!("literal `{val}` does not fit in `{}` ({}..={})", k.name(), k.min(), k.max()),
                    );
                }
            }
        }
        self.check_pending_bounds();
    }

    /// Has an error been reported inside this item (body or signature)? Then unresolved
    /// types are follow-on noise.
    fn item_has_errors(&self) -> bool {
        if self.diags.error_count() != self.errors_at_start {
            return true;
        }
        let s = self.item_span;
        self.diags.list.iter().any(|d| d.is_error() && d.span.lo >= s.lo && d.span.hi <= s.hi && !s.is_dummy())
    }

    fn first_var(&self, t: Ty) -> Ty {
        match self.tys.kind(t) {
            TyKind::Infer(_) => t,
            TyKind::Tuple(ts) | TyKind::Adt(_, ts) => ts
                .iter()
                .map(|&x| self.first_var(x))
                .find(|&x| matches!(self.tys.kind(x), TyKind::Infer(_)))
                .unwrap_or(t),
            TyKind::List(x) | TyKind::Set(x) => self.first_var(*x),
            TyKind::Map(k, v) => {
                let a = self.first_var(*k);
                if matches!(self.tys.kind(a), TyKind::Infer(_)) {
                    a
                } else {
                    self.first_var(*v)
                }
            }
            TyKind::Fn(ps, r, _) => ps
                .iter()
                .map(|&x| self.first_var(x))
                .find(|&x| matches!(self.tys.kind(x), TyKind::Infer(_)))
                .unwrap_or_else(|| self.first_var(*r)),
            _ => t,
        }
    }

    /// Unused variables, needless `var`, needless `&mut`.
    fn lint_locals(&mut self) {
        let ids: Vec<LocalId> = self.body.locals.indices().collect();
        for id in ids {
            let l = &self.body.locals[id];
            if l.name.as_str().starts_with('_') {
                continue;
            }
            let (name, span, used, mutated, kind) = (l.name, l.span, l.used, l.mutated, l.kind);
            match kind {
                LocalKind::SelfValue => {}
                LocalKind::SelfMut => {
                    if !mutated {
                        self.push(Diagnostic::error(codes::E0510, span, "`&mut self` is never mutated").fix(
                            "take `self` by value",
                            span,
                            "self",
                        ));
                    }
                }
                _ if !used => {
                    let what =
                        if matches!(kind, LocalKind::Param | LocalKind::MutParam) { "parameter" } else { "variable" };
                    self.push(
                        Diagnostic::error(codes::E0501, span, format!("unused {what} `{name}`"))
                            .note(format!("remove it, or rename it to `_{name}` if it must exist")),
                    );
                }
                LocalKind::Var if !mutated => {
                    self.push(
                        Diagnostic::error(codes::E0503, span, format!("`{name}` is declared `var` but never mutated"))
                            .note("use `let`"),
                    );
                }
                LocalKind::MutParam if !mutated => {
                    self.push(
                        Diagnostic::error(
                            codes::E0510,
                            span,
                            format!("parameter `{name}` is `&mut` but never mutated"),
                        )
                        .note("take it by value"),
                    );
                }
                _ => {}
            }
        }
    }

    // ---------- entry points ----------

    fn setup_params(&mut self, fd_params: &[ParamDef], self_info: Option<(Mode, Ty, Span)>) {
        if let Some((mode, ty, span)) = self_info {
            let kind = if mode == Mode::MutRef { LocalKind::SelfMut } else { LocalKind::SelfValue };
            let id = self.declare_local(crate::util::sym::SELF_LOWER, ty, kind, span);
            self.body.param_locals.push(id);
        }
        for p in fd_params {
            let kind = if p.mode == Mode::MutRef { LocalKind::MutParam } else { LocalKind::Param };
            let id = self.declare_local(p.name, p.ty, kind, p.span);
            self.body.param_locals.push(id);
        }
    }

    pub fn check_fn(&mut self, fid: FnId) -> Body {
        let fd = &self.defs.fns[fid];
        let item = self.ast.items[fd.item];
        self.file = item.file as usize;
        self.generics = fd.generics.clone();
        self.ret_ty = fd.ret;
        self.declared_effects = fd.effects;
        let ItemKind::Fn(f) = item.kind else { unreachable!() };
        self.item_span = item.span;
        check_snake(self, item.name.sym, item.name.span, "function");
        for g in &fd.generics {
            if g.name.as_str() != "Self" {
                check_camel(self, g.name, g.span, "type parameter");
            }
        }
        let self_info = match (fd.self_mode, fd.self_ty) {
            (Some(m), Some(t)) => Some((m, t, f.self_param.map(|s| s.span).unwrap_or(item.name.span))),
            _ => None,
        };
        let params = fd.params.clone();
        self.push_scope();
        self.setup_params(&params, self_info);
        // Parameter refinements and requires.
        let ast_params: Vec<ast::Param> = self.ast.params.get(f.params).to_vec();
        self.ctx = Ctx::Pure;
        for p in &ast_params {
            if let Some(pred) = p.refine {
                let b = self.tys.bool;
                self.check_expr(pred, Some(b));
            }
        }
        for &r in self.ast.expr_lists.get(f.requires) {
            let b = self.tys.bool;
            self.check_expr(r, Some(b));
        }
        self.ctx = Ctx::Code;
        if let Some(body) = f.body {
            self.diverges = false;
            self.check_block_stmts(body);
            let ret = self.ret_ty;
            let ret_unit = self.tys.is_unit(ret);
            if !self.diverges && !ret_unit {
                let span = self.ast.exprs[body].span;
                let close = Span::new(span.hi.saturating_sub(1), span.hi);
                let rt = self.show(ret);
                self.push(
                    Diagnostic::error(
                        codes::E0308,
                        close,
                        format!("missing `return`: this function must return `{rt}` on every path"),
                    )
                    .note("Jig function bodies do not return their last expression; write `return <value>`"),
                );
            }
        }
        // Ensures.
        self.ctx = Ctx::Ensures;
        for &e in self.ast.expr_lists.get(f.ensures) {
            let b = self.tys.bool;
            self.check_expr(e, Some(b));
        }
        self.ctx = Ctx::Code;
        self.pop_scope();
        self.finish();
        if f.body.is_some() {
            self.lint_locals();
            let unused = self.declared_effects.minus(self.body.used_effects);
            if !unused.is_pure() && fd.iface.is_none() {
                let span = if f.effects_span.is_dummy() { item.name.span } else { f.effects_span };
                self.push(
                    Diagnostic::error(
                        codes::E0403,
                        span,
                        format!(
                            "declared effect{} {} never used",
                            if unused.names().len() > 1 { "s" } else { "" },
                            unused.names().iter().map(|n| format!("`{n}`")).collect::<Vec<_>>().join(", ")
                        ),
                    )
                    .note("remove unused effects so callers do not have to declare them"),
                );
            }
        }
        std::mem::take(&mut self.body)
    }

    pub fn check_test(&mut self, tid: TestId) -> Body {
        let td = &self.defs.tests[tid];
        let item = self.ast.items[td.item];
        self.file = item.file as usize;
        self.ret_ty = self.tys.unit;
        // Tests run against deterministic fakes and may use any effect.
        self.declared_effects = EffectSet::ALL;
        let ItemKind::Test { requires, body, .. } = item.kind else { unreachable!() };
        let params = td.params.clone();
        self.push_scope();
        self.setup_params(&params, None);
        self.ctx = Ctx::Pure;
        for &r in self.ast.expr_lists.get(requires) {
            let b = self.tys.bool;
            self.check_expr(r, Some(b));
        }
        self.ctx = Ctx::Code;
        self.diverges = false;
        self.check_block_stmts(body);
        self.pop_scope();
        self.finish();
        self.lint_locals();
        std::mem::take(&mut self.body)
    }

    fn check_adt_predicates(&mut self, aid: AdtId) {
        let ad = &self.defs.adts[aid];
        let item = self.ast.items[ad.item];
        self.file = item.file as usize;
        self.generics = ad.generics.clone();
        check_camel(self, item.name.sym, item.name.span, if ad.is_enum() { "enum" } else { "struct" });
        self.ctx = Ctx::Pure;
        match item.kind {
            ItemKind::Struct { fields, invariants, .. } => {
                let fs: Vec<ast::FieldDecl> = self.ast.fields.get(fields).to_vec();
                let ftys: Vec<Ty> = ad.fields().iter().map(|f| f.ty).collect();
                for f in &fs {
                    check_snake(self, f.name.sym, f.name.span, "field");
                }
                for (i, f) in fs.iter().enumerate() {
                    if let Some(pred) = f.refine {
                        self.push_scope();
                        let t = ftys.get(i).copied().unwrap_or(self.tys.error);
                        let id = self.declare_local(f.name.sym, t, LocalKind::Param, f.name.span);
                        self.body.locals[id].used = true;
                        let b = self.tys.bool;
                        self.check_expr(pred, Some(b));
                        self.pop_scope();
                    }
                }
                let invs: Vec<ExprId> = self.ast.expr_lists.get(invariants).to_vec();
                if !invs.is_empty() {
                    self.push_scope();
                    for (i, f) in fs.iter().enumerate() {
                        let t = ftys.get(i).copied().unwrap_or(self.tys.error);
                        let id = self.declare_local(f.name.sym, t, LocalKind::Param, f.name.span);
                        self.body.locals[id].used = true;
                    }
                    for e in invs {
                        let b = self.tys.bool;
                        self.check_expr(e, Some(b));
                    }
                    self.pop_scope();
                }
            }
            ItemKind::Enum { variants, .. } => {
                let vs: Vec<ast::VariantDecl> = self.ast.variants.get(variants).to_vec();
                for v in vs {
                    check_camel(self, v.name.sym, v.name.span, "variant");
                    for f in self.ast.fields.get(v.fields).to_vec() {
                        if !f.name.sym.as_str().starts_with('_') {
                            check_snake(self, f.name.sym, f.name.span, "field");
                        }
                    }
                }
            }
            _ => {}
        }
        self.ctx = Ctx::Code;
        self.finish();
    }

    fn check_alias_predicate(&mut self, al: AliasId) {
        let ad = &self.defs.aliases[al];
        let item = self.ast.items[ad.item];
        self.file = item.file as usize;
        check_camel(self, item.name.sym, item.name.span, "type alias");
        self.generics = ad.generics.clone();
        if let Some(pred) = ad.refine {
            let base = ad.ty;
            self.ctx = Ctx::Pure;
            self.push_scope();
            let id = self.declare_local(crate::util::sym::SELF_LOWER, base, LocalKind::SelfValue, item.name.span);
            self.body.locals[id].used = true;
            let b = self.tys.bool;
            self.check_expr(pred, Some(b));
            self.pop_scope();
            self.ctx = Ctx::Code;
        }
        self.finish();
    }

    // ---------- shared helpers ----------

    pub(crate) fn record(&mut self, e: ExprId, t: Ty) -> Ty {
        self.tables.expr_ty[e.0 as usize] = t;
        self.touched.push(e);
        t
    }

    pub(crate) fn set_inst(&mut self, e: ExprId, args: Box<[Ty]>) {
        self.tables.expr_inst.insert(e, args);
        self.touched_insts.push(e);
    }

    pub(crate) fn set_pat_ty(&mut self, p: crate::syntax::ast::PatId, t: Ty) {
        self.tables.pat_ty[p.0 as usize] = t;
        self.touched_pats.push(p);
    }

    pub(crate) fn set_res(&mut self, e: ExprId, r: Res) {
        self.tables.expr_res[e.0 as usize] = r;
    }

    pub(crate) fn add_ref(&mut self, f: FnId) {
        if !self.body.refs.contains(&f) {
            self.body.refs.push(f);
        }
    }

    pub(crate) fn in_loop(&self) -> bool {
        self.loop_depth > 0
    }

    pub(crate) fn file_imports(&self) -> &FxHashMap<Symbol, (PkgId, ast::ItemId)> {
        &self.pkgs[self.pkg].imports[self.file]
    }
}

pub fn bound_name(defs: &Defs, b: Bound) -> String {
    match b {
        Bound::Eq => "Eq".into(),
        Bound::Ord => "Ord".into(),
        Bound::Hash => "Hash".into(),
        Bound::Show => "Show".into(),
        Bound::Iface(i) => defs.ifaces[i].name.to_string(),
    }
}

pub fn is_snake(s: &str) -> bool {
    !s.is_empty()
        && s.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !s.starts_with(|c: char| c.is_ascii_digit())
}

pub fn is_camel(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_uppercase()) && !s.contains('_')
}

pub fn to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

pub fn to_camel(s: &str) -> String {
    s.split('_')
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut cs = p.chars();
            match cs.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + cs.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

pub(crate) fn check_snake(cx: &mut Cx, name: Symbol, span: Span, what: &str) {
    let s = name.as_str();
    let t = s.trim_start_matches('_');
    if t.is_empty() || is_snake(t) {
        return;
    }
    let fixed = to_snake(s);
    cx.push(Diagnostic::error(codes::E0504, span, format!("{what} `{s}` should be snake_case")).fix(
        format!("rename to `{fixed}`"),
        span,
        fixed,
    ));
}

pub(crate) fn check_camel(cx: &mut Cx, name: Symbol, span: Span, what: &str) {
    let s = name.as_str();
    if is_camel(s) {
        return;
    }
    let fixed = to_camel(s);
    if fixed == s {
        return;
    }
    cx.push(Diagnostic::error(codes::E0504, span, format!("{what} `{s}` should be UpperCamel")).fix(
        format!("rename to `{fixed}`"),
        span,
        fixed,
    ));
}
