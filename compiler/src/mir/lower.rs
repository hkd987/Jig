//! Lowering typed bodies to MIR, one monomorphized instance at a time.

use super::intrinsics::{self, Intrinsic, Op};
use super::*;
use crate::diag::SourceMap;
use crate::sema::defs::*;
use crate::sema::ty::*;
use crate::syntax::ast::{self, Ast, ExprId, ExprKind, ForHead, ItemKind, PatId, PatKind, StmtId, StmtKind};
use crate::util::{sym, FxHashMap, IndexVec, List, Symbol};

pub struct Lowerer<'a> {
    pub defs: &'a Defs,
    pub pkgs: &'a IndexVec<PkgId, Package>,
    pub tables: &'a IndexVec<PkgId, Tables>,
    pub bodies: &'a IndexVec<FnId, Option<Body>>,
    pub test_bodies: &'a IndexVec<TestId, Option<Body>>,
    pub sm: &'a SourceMap,
    pub tys: &'a mut TyCtx,
    pub mir: Mir,
}

impl<'a> Lowerer<'a> {
    pub fn new(prog: &'a mut Program, opts: LowerOpts) -> Lowerer<'a> {
        let Program { defs, pkgs, tables, bodies, test_bodies, sm, tys, .. } = prog;
        Lowerer { defs, pkgs, tables, bodies, test_bodies, sm, tys, mir: Mir::new(opts) }
    }

    /// The MIR function for an instance, created on first request (lowered later).
    pub fn instance(&mut self, f: FnId, args: Box<[Ty]>) -> MFnId {
        if let Some(&m) = self.mir.instances.get(&(f, args.clone())) {
            return m;
        }
        let fd = &self.defs.fns[f];
        let mut name = fd.display_name(self.defs, self.tys);
        if !args.is_empty() {
            let a: Vec<String> = args.iter().map(|&t| self.tys.display(t)).collect();
            name = format!("{name}<{}>", a.join(", "));
        }
        let intrinsic = if fd.has_body {
            None
        } else {
            let path = self.pkgs[fd.pkg].path.as_str();
            intrinsics::lookup(path, fd.recv, fd.name.as_str()).map(|(op, int)| Intrinsic {
                op,
                tys: args.clone(),
                int,
            })
        };
        let ret = self.tys.subst(fd.ret, &args);
        let m = self.mir.fns.push(MFn {
            name,
            source: Some(f),
            type_args: args.clone(),
            params: Vec::new(),
            n_captures: 0,
            ret,
            locals: IndexVec::new(),
            blocks: IndexVec::new(),
            lowered: intrinsic.is_some() || !fd.has_body,
            intrinsic,
            span: fd.span,
        });
        self.mir.instances.insert((f, args), m);
        if !self.mir.fns[m].lowered {
            self.mir.pending.push(m);
        }
        m
    }

    /// Lower every requested instance.
    pub fn lower_pending(&mut self) {
        while let Some(m) = self.mir.pending.pop() {
            self.lower_instance(m);
        }
    }

    pub fn ensure_lowered(&mut self, m: MFnId) {
        if !self.mir.fns[m].lowered {
            self.lower_instance(m);
        }
    }

    pub fn main(&mut self) -> Option<MFnId> {
        let m = self.defs.main?;
        Some(self.instance(m, Box::new([])))
    }

    pub fn test_fn(&mut self, tid: TestId) -> MFnId {
        if let Some(&m) = self.mir.tests.get(&tid) {
            return m;
        }
        let td = &self.defs.tests[tid];
        let item = self.pkgs[td.pkg].ast.items[td.item];
        let m = self.mir.fns.push(MFn {
            name: format!("test \"{}\"", td.label),
            source: None,
            type_args: Box::new([]),
            params: Vec::new(),
            n_captures: 0,
            ret: self.tys.unit,
            locals: IndexVec::new(),
            blocks: IndexVec::new(),
            intrinsic: None,
            span: item.span,
            lowered: false,
        });
        self.mir.tests.insert(tid, m);
        let body = self.test_bodies[tid].as_ref().expect("test checked");
        let ItemKind::Test { body: bexpr, requires, .. } = item.kind else { unreachable!() };
        let mut b = FnBuilder::new(self, td.pkg, body, Box::new([]), m);
        b.params_from_body(&td.params.iter().map(|p| p.mode).collect::<Vec<_>>(), false);
        if b.lw.mir.opts.check_contracts {
            for &r in b.ast.expr_lists.get(requires).to_vec().iter() {
                b.check(r, CheckKind::Precondition);
            }
        }
        b.lower_body_block(bexpr);
        b.finish();
        m
    }

    fn lower_instance(&mut self, m: MFnId) {
        let f = self.mir.fns[m].source.expect("instance has a source");
        let args = self.mir.fns[m].type_args.clone();
        let fd = &self.defs.fns[f];
        let pkg = fd.pkg;
        let item = self.pkgs[pkg].ast.items[fd.item];
        let ItemKind::Fn(decl) = item.kind else { unreachable!() };
        let Some(body) = self.bodies[f].as_ref() else {
            self.mir.fns[m].lowered = true;
            return;
        };
        let mut modes: Vec<Mode> = Vec::new();
        if let Some(sm) = fd.self_mode {
            modes.push(sm);
        }
        modes.extend(fd.params.iter().map(|p| p.mode));
        let mut b = FnBuilder::new(self, pkg, body, args, m);
        b.params_from_body(&modes, false);
        b.ret_ty = b.lw.mir.fns[m].ret;
        b.locals[Local(0)].ty = b.ret_ty;
        let check = b.lw.mir.opts.check_contracts;
        if check {
            // Parameter refinements, then preconditions.
            let ps: Vec<ast::Param> = b.ast.params.get(decl.params).to_vec();
            for p in &ps {
                if let Some(pred) = p.refine {
                    b.check(pred, CheckKind::Refinement);
                }
            }
            let fd = &b.lw.defs.fns[f];
            let offset = fd.self_mode.is_some() as usize;
            let refined: Vec<(usize, AliasId)> =
                fd.params.iter().enumerate().filter_map(|(i, p)| p.refine.map(|al| (i + offset, al))).collect();
            b.ret_refine = fd.ret_refine;
            for (i, al) in refined {
                let l = b.params[i].0;
                b.check_alias(al, Place::local(l));
            }
            for &r in b.ast.expr_lists.get(decl.requires).to_vec().iter() {
                b.check(r, CheckKind::Precondition);
            }
            // `&mut` parameters must satisfy their struct predicates again on return.
            let mut_params: Vec<Local> = b.params.iter().filter(|(_, m)| *m == Mode::MutRef).map(|(l, _)| *l).collect();
            b.mut_params = mut_params;
            // Snapshot `old(...)` values for the postconditions.
            for &en in b.ast.expr_lists.get(decl.ensures).to_vec().iter() {
                b.snapshot_olds(en);
            }
            b.ensures = b.ast.expr_lists.get(decl.ensures).to_vec();
        }
        if let Some(bexpr) = decl.body {
            b.lower_body_block(bexpr);
        }
        b.finish();
    }
}

struct LoopFrame {
    cont: BlockId,
    brk: BlockId,
    scope_depth: usize,
}

struct FnBuilder<'l, 'a> {
    lw: &'l mut Lowerer<'a>,
    pkg: PkgId,
    ast: &'a Ast,
    tables: &'a Tables,
    body: &'a Body,
    inst: Box<[Ty]>,
    mfn: MFnId,
    locals: IndexVec<Local, LocalDecl>,
    blocks: IndexVec<BlockId, Block>,
    cur: BlockId,
    local_map: FxHashMap<LocalId, Local>,
    loops: Vec<LoopFrame>,
    /// Locals declared in each open scope (dropped at scope exit).
    scopes: Vec<Vec<Local>>,
    exit: BlockId,
    ret_ty: Ty,
    olds: FxHashMap<ExprId, Local>,
    ensures: Vec<ExprId>,
    /// Checked on return: the return type's refinement and `&mut` parameters' predicates.
    ret_refine: Option<AliasId>,
    mut_params: Vec<Local>,
    params: Vec<(Local, Mode)>,
    n_captures: usize,
    cur_span: crate::diag::Span,
}

impl<'l, 'a> FnBuilder<'l, 'a> {
    fn new(lw: &'l mut Lowerer<'a>, pkg: PkgId, body: &'a Body, inst: Box<[Ty]>, mfn: MFnId) -> Self {
        let ast: &'a Ast = &lw.pkgs[pkg].ast;
        let tables: &'a Tables = &lw.tables[pkg];
        let ret_ty = lw.mir.fns[mfn].ret;
        let mut locals = IndexVec::new();
        locals.push(LocalDecl { ty: ret_ty, name: None });
        let mut blocks: IndexVec<BlockId, Block> = IndexVec::new();
        let span = lw.mir.fns[mfn].span;
        let entry = blocks.push(Block { stmts: Vec::new(), term: Terminator::Unreachable, term_span: span });
        let exit = blocks.push(Block { stmts: Vec::new(), term: Terminator::Return, term_span: span });
        FnBuilder {
            lw,
            pkg,
            ast,
            tables,
            body,
            inst,
            mfn,
            locals,
            blocks,
            cur: entry,
            local_map: FxHashMap::default(),
            loops: Vec::new(),
            scopes: vec![Vec::new()],
            exit,
            ret_ty,
            olds: FxHashMap::default(),
            ensures: Vec::new(),
            ret_refine: None,
            mut_params: Vec::new(),
            params: Vec::new(),
            n_captures: 0,
            cur_span: span,
        }
    }

    // ---------- infrastructure ----------

    fn subst(&mut self, t: Ty) -> Ty {
        self.lw.tys.subst(t, &self.inst)
    }

    fn ty(&mut self, e: ExprId) -> Ty {
        let t = self.tables.expr_ty[e.0 as usize];
        self.subst(t)
    }

    fn new_local(&mut self, ty: Ty, name: Option<Symbol>) -> Local {
        self.locals.push(LocalDecl { ty, name })
    }

    fn temp(&mut self, ty: Ty) -> Local {
        self.new_local(ty, None)
    }

    fn new_block(&mut self) -> BlockId {
        self.blocks.push(Block { stmts: Vec::new(), term: Terminator::Unreachable, term_span: self.cur_span })
    }

    fn push(&mut self, s: Stmt) {
        let span = self.cur_span;
        self.blocks[self.cur].stmts.push((s, span));
    }

    fn assign(&mut self, p: Place, rv: Rvalue) {
        self.push(Stmt::Assign(p, rv));
    }

    fn terminate(&mut self, t: Terminator) {
        let span = self.cur_span;
        let b = &mut self.blocks[self.cur];
        b.term = t;
        b.term_span = span;
    }

    fn goto(&mut self, target: BlockId) {
        self.terminate(Terminator::Goto(target));
    }

    /// End the current block with `t` and continue in a fresh (unreachable) block.
    fn terminate_and_continue(&mut self, t: Terminator) {
        self.terminate(t);
        self.cur = self.new_block();
    }

    fn sema_local(&mut self, id: LocalId) -> Local {
        if let Some(&l) = self.local_map.get(&id) {
            return l;
        }
        let d = &self.body.locals[id];
        let (ty, name) = (d.ty, d.name);
        let ty = self.subst(ty);
        let l = self.new_local(ty, Some(name));
        self.local_map.insert(id, l);
        if let Some(s) = self.scopes.last_mut() {
            s.push(l);
        }
        l
    }

    fn params_from_body(&mut self, modes: &[Mode], _closure: bool) {
        let ids: Vec<LocalId> = self.body.param_locals.clone();
        for (i, id) in ids.into_iter().enumerate() {
            let l = self.sema_local(id);
            let mode = modes.get(i).copied().unwrap_or(Mode::Value);
            self.params.push((l, mode));
        }
        // Parameters are not dropped by scopes.
        if let Some(s) = self.scopes.last_mut() {
            s.clear();
        }
    }

    fn finish(mut self) {
        // Fall off the end of the body into the exit block.
        self.drop_scopes_to(0);
        self.goto(self.exit);
        // Postconditions in the exit block.
        let ensures = std::mem::take(&mut self.ensures);
        let mut_params = std::mem::take(&mut self.mut_params);
        let ret_refine = self.ret_refine.take();
        if !ensures.is_empty() || !mut_params.is_empty() || ret_refine.is_some() {
            let exit = self.exit;
            let body_start = self.new_block();
            self.cur = exit;
            // The exit block checks then returns; re-route: exit -> checks -> return.
            let ret_block = body_start;
            self.blocks[ret_block].term = Terminator::Return;
            if let Some(al) = ret_refine {
                self.check_alias(al, Place::local(Local(0)));
            }
            for l in mut_params {
                let t = self.locals[l].ty;
                if let TyKind::Adt(adt, _) = *self.lw.tys.kind(t) {
                    if !self.lw.defs.adts[adt].is_enum() {
                        self.check_struct_predicates(adt, Place::local(l));
                    }
                }
            }
            for en in ensures {
                self.check(en, CheckKind::Postcondition);
            }
            self.goto(ret_block);
        }
        let m = self.mfn;
        let f = &mut self.lw.mir.fns[m];
        f.locals = self.locals;
        f.blocks = self.blocks;
        f.params = self.params;
        f.n_captures = self.n_captures;
        f.lowered = true;
    }

    fn src(&self, e: ExprId) -> String {
        let s = self.lw.sm.snippet(self.ast.exprs[e].span);
        if s.len() > 120 {
            format!("{}…", &s[..s.char_indices().take(117).last().map(|(i, c)| i + c.len_utf8()).unwrap_or(0)])
        } else {
            s.to_string()
        }
    }

    /// Emit a runtime check of a boolean expression, showing operand values on failure.
    fn check(&mut self, cond: ExprId, kind: CheckKind) {
        let saved = self.cur_span;
        self.cur_span = self.ast.exprs[cond].span;
        let text = self.src(cond);
        let mut values = Vec::new();
        let op = match self.ast.exprs[cond].kind {
            ExprKind::Binary { op, lhs, rhs } if op.is_comparison() => {
                let lt = self.ty(lhs);
                let a = self.expr_to_temp(lhs);
                let b = self.expr_to_temp(rhs);
                let (ls, rs) = (self.src(lhs), self.src(rhs));
                let t = self.temp(self.lw.tys.bool);
                self.assign(Place::local(t), Rvalue::Binary(op, Operand::local(a), Operand::local(b), lt));
                if !matches!(
                    self.ast.exprs[lhs].kind,
                    ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_)
                ) {
                    values.push((Symbol::intern(&ls), Operand::local(a), lt));
                }
                if !matches!(
                    self.ast.exprs[rhs].kind,
                    ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_)
                ) {
                    values.push((Symbol::intern(&rs), Operand::local(b), lt));
                }
                Operand::local(t)
            }
            _ => self.expr(cond),
        };
        let span = self.cur_span;
        // The variables, fields, `result` and `old(...)` values the condition mentions,
        // read only when it fails.
        let mut atoms = Vec::new();
        self.atoms(cond, &mut atoms);
        let shown: Vec<String> = values.iter().map(|(n, _, _)| n.as_str().to_string()).collect();
        let mut texts: Vec<String> = Vec::new();
        atoms.retain(|&a| {
            let t = self.src(a);
            if shown.contains(&t) || texts.contains(&t) {
                return false;
            }
            texts.push(t);
            true
        });
        atoms.truncate(8);
        if atoms.is_empty() {
            self.push(Stmt::Check { cond: op, kind, msg: Symbol::intern(&text), values, span });
        } else {
            let ok_bb = self.new_block();
            let fail_bb = self.new_block();
            self.terminate(Terminator::If(op, ok_bb, fail_bb));
            self.cur = fail_bb;
            for a in atoms {
                let t = self.ty(a);
                let l = self.expr_to_temp(a);
                values.push((Symbol::intern(&self.src(a)), Operand::local(l), t));
            }
            self.cur_span = span;
            self.push(Stmt::Check { cond: Operand::bool(false), kind, msg: Symbol::intern(&text), values, span });
            self.goto(ok_bb);
            self.cur = ok_bb;
        }
        self.cur_span = saved;
    }

    /// Side-effect-free leaves of a condition worth showing: locals, field paths, `result`
    /// and `old(...)`. Indexing is left out (it could trap again).
    fn atoms(&self, e: ExprId, out: &mut Vec<ExprId>) {
        let k = self.ast.exprs[e].kind;
        match k {
            ExprKind::Name(_) => {
                if matches!(self.tables.expr_res[e.0 as usize], Res::Local(_) | Res::ResultVal) {
                    out.push(e);
                }
                return;
            }
            ExprKind::Field { base, .. } => {
                if matches!(self.tables.expr_res[e.0 as usize], Res::Field(_)) && self.field_chain(base) {
                    out.push(e);
                    return;
                }
            }
            ExprKind::TupleField { base, .. } if self.field_chain(base) => {
                out.push(e);
                return;
            }
            ExprKind::Call { callee, .. } if self.tables.expr_res[callee.0 as usize] == Res::Old => {
                out.push(e);
                return;
            }
            ExprKind::Closure { .. } | ExprKind::Quant { .. } => return,
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Char(_) => return,
            _ => {}
        }
        let mut kids = Vec::new();
        visit_children(self.ast, e, &mut kids);
        for c in kids {
            self.atoms(c, out);
        }
    }

    fn field_chain(&self, e: ExprId) -> bool {
        match self.ast.exprs[e].kind {
            ExprKind::Name(_) => matches!(self.tables.expr_res[e.0 as usize], Res::Local(_) | Res::ResultVal),
            ExprKind::Field { base, .. } => {
                matches!(self.tables.expr_res[e.0 as usize], Res::Field(_)) && self.field_chain(base)
            }
            ExprKind::TupleField { base, .. } | ExprKind::Paren(base) => self.field_chain(base),
            _ => false,
        }
    }

    fn snapshot_olds(&mut self, e: ExprId) {
        // Find `old(x)` calls and evaluate `x` now.
        let mut stack = vec![e];
        while let Some(x) = stack.pop() {
            let k = self.ast.exprs[x].kind;
            if let ExprKind::Call { callee, args } = k {
                if self.tables.expr_res[callee.0 as usize] == Res::Old {
                    if let Some(&arg) = self.ast.expr_lists.get(args).first() {
                        let t = self.ty(arg);
                        let l = self.temp(t);
                        self.expr_into(arg, Place::local(l));
                        self.olds.insert(x, l);
                    }
                    continue;
                }
            }
            visit_children(self.ast, x, &mut stack);
        }
    }

    // ---------- scopes ----------

    fn push_scope(&mut self) {
        self.scopes.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        if let Some(s) = self.scopes.pop() {
            for &l in s.iter().rev() {
                self.push(Stmt::Drop(l));
            }
        }
    }

    /// Emit drops for all scopes deeper than `depth` (for early exits).
    fn drop_scopes_to(&mut self, depth: usize) {
        let mut drops = Vec::new();
        for s in self.scopes[depth..].iter().rev() {
            for &l in s.iter().rev() {
                drops.push(l);
            }
        }
        for l in drops {
            self.push(Stmt::Drop(l));
        }
    }

    // ---------- statements ----------

    fn lower_body_block(&mut self, block: ExprId) {
        if let ExprKind::Block(stmts) = self.ast.exprs[block].kind {
            self.push_scope();
            for &s in self.ast.stmt_lists.get(stmts).to_vec().iter() {
                self.stmt(s);
            }
            self.pop_scope();
        }
    }

    fn stmt(&mut self, s: StmtId) {
        let st = self.ast.stmts[s];
        self.cur_span = st.span;
        match st.kind {
            StmtKind::Let { pat, init, .. } => self.let_pat(pat, init),
            StmtKind::Assign { target, op, value } => {
                let p = self.place(target);
                let checked = p.clone();
                match op {
                    None => self.expr_into(value, p),
                    Some(op) => {
                        let tt = self.ty(target);
                        let v = self.expr(value);
                        let rv = if matches!(self.lw.tys.kind(tt), TyKind::Str) {
                            Rvalue::Call(
                                Callee::Intrinsic(Intrinsic::new(Op::StrConcat, vec![])),
                                vec![Arg::Value(Operand::Copy(p.clone())), Arg::Value(v)],
                                tt,
                            )
                        } else {
                            Rvalue::Binary(op, Operand::Copy(p.clone()), v, tt)
                        };
                        let t = self.temp(tt);
                        self.assign(Place::local(t), rv);
                        self.assign(p, Rvalue::Use(Operand::Copy(Place::local(t))));
                    }
                }
                if self.lw.mir.opts.check_contracts && !checked.proj.is_empty() {
                    self.check_mutated(checked);
                }
            }
            StmtKind::Expr(e) => {
                let _ = self.expr(e);
            }
            StmtKind::Return(v) => {
                if let Some(e) = v {
                    self.expr_into(e, Place::local(Local(0)));
                }
                self.drop_scopes_to(1);
                let exit = self.exit;
                self.terminate_and_continue(Terminator::Goto(exit));
            }
            StmtKind::Break | StmtKind::Continue => {
                let Some(fr) = self.loops.last() else { return };
                let (target, depth) = if matches!(st.kind, StmtKind::Break) {
                    (fr.brk, fr.scope_depth)
                } else {
                    (fr.cont, fr.scope_depth)
                };
                self.drop_scopes_to(depth);
                self.terminate_and_continue(Terminator::Goto(target));
            }
            StmtKind::For { head, invariants, body } => self.lower_for(head, invariants, body),
        }
    }

    fn let_pat(&mut self, pat: PatId, init: ExprId) {
        match self.ast.pats[pat].kind {
            PatKind::Bind(_) | PatKind::Variant { .. } => match self.tables.pat_local.get(&pat) {
                Some(&id) => {
                    let l = self.sema_local(id);
                    self.expr_into(init, Place::local(l));
                }
                None => {
                    let _ = self.expr(init);
                }
            },
            PatKind::Wild => {
                let _ = self.expr(init);
            }
            _ => {
                let t = self.ty(init);
                let tmp = self.temp(t);
                self.expr_into(init, Place::local(tmp));
                self.bind_irrefutable(pat, Place::local(tmp));
            }
        }
    }

    fn bind_irrefutable(&mut self, pat: PatId, place: Place) {
        match self.ast.pats[pat].kind {
            PatKind::Bind(_) | PatKind::Variant { .. } => {
                if let Some(&id) = self.tables.pat_local.get(&pat) {
                    let l = self.sema_local(id);
                    self.assign(Place::local(l), Rvalue::Use(Operand::Copy(place)));
                }
            }
            PatKind::Tuple(list) => {
                for (i, &sp) in self.ast.pat_lists.get(list).to_vec().iter().enumerate() {
                    self.bind_irrefutable(sp, place.field(i as u32));
                }
            }
            _ => {}
        }
    }

    fn lower_for(&mut self, head: ForHead, invariants: List<ExprId>, body: ExprId) {
        let invs: Vec<ExprId> = self.ast.expr_lists.get(invariants).to_vec();
        let check = self.lw.mir.opts.check_contracts;
        self.push_scope();
        let head_bb = self.new_block();
        let body_bb = self.new_block();
        let latch_bb = self.new_block();
        let exit_bb = self.new_block();
        let int = self.lw.tys.int;
        match head {
            ForHead::Infinite | ForHead::Cond(_) => {
                self.goto(head_bb);
                self.cur = head_bb;
                if let ForHead::Cond(c) = head {
                    let op = self.expr(c);
                    self.terminate(Terminator::If(op, body_bb, exit_bb));
                } else {
                    self.goto(body_bb);
                }
                self.cur = latch_bb;
                self.goto(head_bb);
            }
            ForHead::In { pat, iter } => {
                // Ranges iterate an integer; collections iterate a snapshot by index.
                let (elem_kind, idx, limit, coll) = if let ExprKind::Range { lo: Some(lo), hi: Some(hi) } =
                    self.ast.exprs[iter].kind
                {
                    let t = self.ty(lo);
                    let i = self.temp(t);
                    self.expr_into(lo, Place::local(i));
                    let h = self.temp(t);
                    self.expr_into(hi, Place::local(h));
                    (Some(t), i, h, None)
                } else {
                    let it = self.ty(iter);
                    let src = self.expr(iter);
                    let (list_ty, op) = match self.lw.tys.kind(it).clone() {
                        TyKind::List(_) => (it, None),
                        TyKind::Set(e) => (self.lw.tys.list(e), Some((Op::Items, vec![e]))),
                        TyKind::Map(k, v) => {
                            let tup = self.lw.tys.tuple(vec![k, v]);
                            (self.lw.tys.list(tup), Some((Op::Entries, vec![k, v])))
                        }
                        _ => (it, None),
                    };
                    let coll = self.temp(list_ty);
                    match op {
                        None => self.assign(Place::local(coll), Rvalue::Use(src)),
                        Some((op, tys)) => self.assign(
                            Place::local(coll),
                            Rvalue::Call(Callee::Intrinsic(Intrinsic::new(op, tys)), vec![Arg::Value(src)], list_ty),
                        ),
                    }
                    let elem = match self.lw.tys.kind(list_ty) {
                        TyKind::List(e) => *e,
                        _ => self.lw.tys.error,
                    };
                    let n = self.temp(int);
                    self.assign(
                        Place::local(n),
                        Rvalue::Call(
                            Callee::Intrinsic(Intrinsic::new(Op::ListLen, vec![elem])),
                            vec![Arg::Value(Operand::local(coll))],
                            int,
                        ),
                    );
                    let i = self.temp(int);
                    self.assign(Place::local(i), Rvalue::Use(Operand::int(0)));
                    (None, i, n, Some(coll))
                };
                self.goto(head_bb);
                self.cur = head_bb;
                let it = elem_kind.unwrap_or(int);
                let c = self.temp(self.lw.tys.bool);
                self.assign(
                    Place::local(c),
                    Rvalue::Binary(ast::BinOp::Lt, Operand::local(idx), Operand::local(limit), it),
                );
                self.terminate(Terminator::If(Operand::local(c), body_bb, exit_bb));
                self.cur = body_bb;
                match coll {
                    None => self.bind_irrefutable(pat, Place::local(idx)),
                    Some(coll) => self.bind_irrefutable(pat, Place::local(coll).index(Operand::local(idx))),
                }
                let after_bind = self.cur;
                let next = self.new_block();
                self.goto(next);
                let _ = after_bind;
                let body_start = next;
                self.cur = latch_bb;
                let one = Operand::Const(Const::Int(
                    1,
                    match self.lw.tys.kind(it) {
                        TyKind::Int(k, _) => *k,
                        _ => IntTy::I64,
                    },
                ));
                self.assign(Place::local(idx), Rvalue::Binary(ast::BinOp::Add, Operand::local(idx), one, it));
                self.goto(head_bb);
                // The body proper starts after the binding.
                self.cur = body_start;
                self.lower_loop_body(&invs, check, body, latch_bb, exit_bb);
                self.cur = exit_bb;
                self.pop_scope();
                return;
            }
        }
        self.cur = body_bb;
        self.lower_loop_body(&invs, check, body, latch_bb, exit_bb);
        self.cur = exit_bb;
        self.pop_scope();
    }

    fn lower_loop_body(&mut self, invs: &[ExprId], check: bool, body: ExprId, latch: BlockId, exit: BlockId) {
        if check {
            for &inv in invs {
                self.check(inv, CheckKind::Invariant);
            }
        }
        self.loops.push(LoopFrame { cont: latch, brk: exit, scope_depth: self.scopes.len() });
        self.lower_body_block(body);
        self.loops.pop();
        self.goto(latch);
    }

    // ---------- places ----------

    fn is_place_expr(&self, e: ExprId) -> bool {
        match self.ast.exprs[e].kind {
            ExprKind::Name(_) => matches!(self.tables.expr_res[e.0 as usize], Res::Local(_)),
            ExprKind::Field { base, .. } => {
                matches!(self.tables.expr_res[e.0 as usize], Res::Field(_)) && self.is_place_expr(base)
            }
            ExprKind::TupleField { base, .. } => self.is_place_expr(base),
            ExprKind::Index { base, index } => {
                !matches!(self.ast.exprs[index].kind, ExprKind::Range { .. })
                    && matches!(
                        self.lw.tys.kind(self.tables.expr_ty[base.0 as usize]),
                        TyKind::List(_) | TyKind::Param(..)
                    )
                    && self.is_place_expr(base)
            }
            ExprKind::Paren(x) => self.is_place_expr(x),
            _ => false,
        }
    }

    /// A place for a place expression; other expressions are evaluated into a temporary.
    fn place(&mut self, e: ExprId) -> Place {
        match self.ast.exprs[e].kind {
            ExprKind::Name(_) => {
                if let Res::Local(id) = self.tables.expr_res[e.0 as usize] {
                    return Place::local(self.sema_local(id));
                }
            }
            ExprKind::Field { base, .. } => {
                if let Res::Field(i) = self.tables.expr_res[e.0 as usize] {
                    let b = self.place(base);
                    return b.field(i);
                }
            }
            ExprKind::TupleField { base, index, .. } => {
                let b = self.place(base);
                return b.field(index);
            }
            ExprKind::Index { base, index } if !matches!(self.ast.exprs[index].kind, ExprKind::Range { .. }) => {
                let bt = self.ty(base);
                if matches!(self.lw.tys.kind(bt), TyKind::List(_)) {
                    let b = self.place(base);
                    let i = self.expr(index);
                    return b.index(i);
                }
            }
            ExprKind::Paren(x) => return self.place(x),
            _ => {}
        }
        let t = self.ty(e);
        let tmp = self.temp(t);
        self.expr_into(e, Place::local(tmp));
        Place::local(tmp)
    }

    // ---------- expressions ----------

    fn expr_to_temp(&mut self, e: ExprId) -> Local {
        let t = self.ty(e);
        let l = self.temp(t);
        self.expr_into(e, Place::local(l));
        l
    }

    /// Evaluate an expression to an operand.
    fn expr(&mut self, e: ExprId) -> Operand {
        if self.is_place_expr(e) {
            let p = self.place(e);
            return Operand::Copy(p);
        }
        match self.ast.exprs[e].kind {
            ExprKind::Int(v) => {
                let t = self.ty(e);
                return self.int_const(v as i128, t);
            }
            ExprKind::Unary { op: ast::UnOp::Neg, expr } if matches!(self.ast.exprs[expr].kind, ExprKind::Int(_)) => {
                let ExprKind::Int(v) = self.ast.exprs[expr].kind else { unreachable!() };
                let t = self.ty(e);
                return self.int_const(-(v as i128), t);
            }
            ExprKind::Float(v) => return Operand::Const(Const::Float(v)),
            ExprKind::Bool(b) => return Operand::Const(Const::Bool(b)),
            ExprKind::Str(s) => return Operand::Const(Const::Str(s)),
            ExprKind::Char(c) => {
                let t = self.ty(e);
                if let TyKind::Int(k, _) = self.lw.tys.kind(t) {
                    return Operand::Const(Const::Int(c as i128, *k));
                }
                return Operand::Const(Const::Char(c));
            }
            ExprKind::Paren(x) => return self.expr(x),
            _ => {}
        }
        if let Res::Const(c) = self.tables.expr_res[e.0 as usize] {
            return self.const_val(c);
        }
        let t = self.ty(e);
        if self.lw.tys.is_unit(t) {
            self.expr_into_unit(e);
            return Operand::unit();
        }
        let l = self.temp(t);
        self.expr_into(e, Place::local(l));
        Operand::local(l)
    }

    fn expr_into_unit(&mut self, e: ExprId) {
        let t = self.ty(e);
        let l = self.temp(t);
        self.expr_into(e, Place::local(l));
    }

    fn int_const(&mut self, v: i128, t: Ty) -> Operand {
        match *self.lw.tys.kind(t) {
            TyKind::Int(k, _) => Operand::Const(Const::Int(v, k)),
            TyKind::Float(_) => Operand::Const(Const::Float(v as f64)),
            _ => Operand::Const(Const::Int(v, IntTy::I64)),
        }
    }

    fn const_val(&mut self, c: ConstId) -> Operand {
        let ty = self.lw.defs.consts[c].ty;
        match &self.lw.defs.consts[c].value {
            Some(ConstVal::Int(v)) => {
                let v = *v;
                self.int_const(v, ty)
            }
            Some(ConstVal::Float(f)) => Operand::Const(Const::Float(*f)),
            Some(ConstVal::Bool(b)) => Operand::Const(Const::Bool(*b)),
            Some(ConstVal::Str(s)) => Operand::Const(Const::Str(*s)),
            Some(ConstVal::Char(ch)) => Operand::Const(Const::Char(*ch)),
            None => Operand::unit(),
        }
    }

    fn inst_of(&mut self, e: ExprId) -> Box<[Ty]> {
        let args: Vec<Ty> = self.tables.expr_inst.get(&e).map(|a| a.to_vec()).unwrap_or_default();
        args.into_iter().map(|t| self.subst(t)).collect::<Vec<_>>().into_boxed_slice()
    }

    /// Evaluate `e` and store its value into `dest`.
    fn expr_into(&mut self, e: ExprId, dest: Place) {
        let ex = self.ast.exprs[e];
        let saved = self.cur_span;
        self.cur_span = ex.span;
        let t = self.ty(e);
        match ex.kind {
            ExprKind::Name(_)
            | ExprKind::Field { .. }
            | ExprKind::TupleField { .. }
            | ExprKind::Index { .. }
            | ExprKind::TypeArgs { .. }
                if !self.is_place_expr(e) =>
            {
                self.value_expr_into(e, dest);
            }
            ExprKind::Name(_) | ExprKind::Field { .. } | ExprKind::TupleField { .. } | ExprKind::Index { .. } => {
                let p = self.place(e);
                self.assign(dest, Rvalue::Use(Operand::Copy(p)));
            }
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Char(_) => {
                let op = self.expr(e);
                self.assign(dest, Rvalue::Use(op));
            }
            ExprKind::Paren(x) => self.expr_into(x, dest),
            ExprKind::Tuple(list) => {
                let ops: Vec<Operand> =
                    self.ast.expr_lists.get(list).to_vec().into_iter().map(|x| self.expr(x)).collect();
                self.assign(dest, Rvalue::Aggregate(AggKind::Tuple, t, ops));
            }
            ExprKind::List(list) => {
                let ops: Vec<Operand> =
                    self.ast.expr_lists.get(list).to_vec().into_iter().map(|x| self.expr(x)).collect();
                self.assign(dest, Rvalue::Aggregate(AggKind::List, t, ops));
            }
            ExprKind::Call { callee, args } => self.lower_call(e, callee, args, dest),
            ExprKind::Unary { op, expr } => {
                if op == ast::UnOp::Neg && matches!(self.ast.exprs[expr].kind, ExprKind::Int(_)) {
                    let o = self.expr(e);
                    self.assign(dest, Rvalue::Use(o));
                } else {
                    let o = self.expr(expr);
                    let it = self.ty(expr);
                    self.assign(dest, Rvalue::Unary(op, o, it));
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.lower_binary(op, lhs, rhs, t, dest),
            ExprKind::Cast { expr, .. } => {
                let from = self.ty(expr);
                let o = self.expr(expr);
                self.assign(dest, Rvalue::Cast(o, from, t));
            }
            ExprKind::Try(inner) => self.lower_try(inner, t, dest),
            ExprKind::If { cond, then, els } => self.lower_if(cond, then, els, Some(dest)),
            ExprKind::Match { scrut, arms } => self.lower_match(scrut, arms, Some(dest)),
            ExprKind::Block(_) => self.block_into(e, Some(dest)),
            ExprKind::StructLit { fields, .. } => self.lower_struct_lit(e, fields, t, dest),
            ExprKind::Closure { params, body, .. } => self.lower_closure(e, params, body, t, dest),
            ExprKind::Quant { forall, var, lo, hi, body } => self.lower_quant(e, forall, var, lo, hi, body, dest),
            ExprKind::MutArg(x) => self.expr_into(x, dest),
            ExprKind::Range { .. } | ExprKind::Error => {}
            ExprKind::TypeArgs { .. } => self.value_expr_into(e, dest),
        }
        self.cur_span = saved;
    }

    /// Names and fields that are not places: functions, constants, variants, packages.
    fn value_expr_into(&mut self, e: ExprId, dest: Place) {
        let t = self.ty(e);
        match self.tables.expr_res[e.0 as usize] {
            Res::Const(c) => {
                let o = self.const_val(c);
                self.assign(dest, Rvalue::Use(o));
            }
            Res::Fn(f) => {
                let inst = self.inst_of(e);
                let m = self.lw.instance(f, inst);
                self.assign(dest, Rvalue::Use(Operand::Const(Const::Fn(m))));
            }
            Res::Variant(adt, v) => {
                let nfields = self.lw.defs.adts[adt].variants()[v as usize].fields.len();
                if nfields == 0 {
                    self.assign(dest, Rvalue::Aggregate(AggKind::Variant(adt, v), t, Vec::new()));
                } else {
                    let m = self.variant_ctor(adt, v, t);
                    self.assign(dest, Rvalue::Use(Operand::Const(Const::Fn(m))));
                }
            }
            Res::ResultVal => {
                self.assign(dest, Rvalue::Use(Operand::local(Local(0))));
            }
            Res::Field(i) => {
                if let ExprKind::Field { base, .. } = self.ast.exprs[e].kind {
                    let b = self.place(base);
                    self.assign(dest, Rvalue::Use(Operand::Copy(b.field(i))));
                }
            }
            _ => match self.ast.exprs[e].kind {
                ExprKind::TupleField { base, index, .. } => {
                    let b = self.place(base);
                    self.assign(dest, Rvalue::Use(Operand::Copy(b.field(index))));
                }
                ExprKind::Index { base, index } => self.lower_index(base, index, t, dest),
                ExprKind::TypeArgs { base, .. } => self.value_expr_into(base, dest),
                _ => {}
            },
        }
    }

    /// A function constructing an enum variant from its fields (`xs.map(Some)`).
    fn variant_ctor(&mut self, adt: AdtId, v: u32, fn_ty: Ty) -> MFnId {
        let TyKind::Fn(ps, ret, _) = self.lw.tys.kind(fn_ty).clone() else { unreachable!() };
        let mut locals: IndexVec<Local, LocalDecl> = IndexVec::new();
        locals.push(LocalDecl { ty: ret, name: None });
        let params: Vec<(Local, Mode)> =
            ps.iter().map(|&p| (locals.push(LocalDecl { ty: p, name: None }), Mode::Value)).collect();
        let ops: Vec<Operand> = params.iter().map(|(l, _)| Operand::local(*l)).collect();
        let span = self.cur_span;
        let mut blocks = IndexVec::new();
        blocks.push(Block {
            stmts: vec![(
                Stmt::Assign(Place::local(Local(0)), Rvalue::Aggregate(AggKind::Variant(adt, v), ret, ops)),
                span,
            )],
            term: Terminator::Return,
            term_span: span,
        });
        let name = format!("{}.{}", self.lw.defs.adts[adt].name, self.lw.defs.adts[adt].variants()[v as usize].name);
        self.lw.mir.fns.push(MFn {
            name,
            source: None,
            type_args: Box::new([]),
            params,
            n_captures: 0,
            ret,
            locals,
            blocks,
            intrinsic: None,
            span,
            lowered: true,
        })
    }

    fn lower_index(&mut self, base: ExprId, index: ExprId, t: Ty, dest: Place) {
        let bt = self.ty(base);
        let int = self.lw.tys.int;
        if let ExprKind::Range { lo, hi } = self.ast.exprs[index].kind {
            let b = self.expr(base);
            let lo_op = match lo {
                Some(l) => self.expr(l),
                None => Operand::int(0),
            };
            let is_str = matches!(self.lw.tys.kind(bt), TyKind::Str);
            let hi_op = match hi {
                Some(h) => self.expr(h),
                None => {
                    let n = self.temp(int);
                    let (op, tys) = if is_str {
                        (Op::StrLen, vec![])
                    } else {
                        let el = match self.lw.tys.kind(bt) {
                            TyKind::List(e) => *e,
                            _ => bt,
                        };
                        (Op::ListLen, vec![el])
                    };
                    self.assign(
                        Place::local(n),
                        Rvalue::Call(Callee::Intrinsic(Intrinsic::new(op, tys)), vec![Arg::Value(b.clone())], int),
                    );
                    Operand::local(n)
                }
            };
            let op = if is_str { Op::StrSlice } else { Op::ListSlice };
            self.assign(
                dest,
                Rvalue::Call(
                    Callee::Intrinsic(Intrinsic::new(op, vec![t])),
                    vec![Arg::Value(b), Arg::Value(lo_op), Arg::Value(hi_op)],
                    t,
                ),
            );
            return;
        }
        match self.lw.tys.kind(bt).clone() {
            TyKind::Str => {
                let b = self.expr(base);
                let i = self.expr(index);
                self.assign(
                    dest,
                    Rvalue::Call(
                        Callee::Intrinsic(Intrinsic::new(Op::StrByte, vec![])),
                        vec![Arg::Value(b), Arg::Value(i)],
                        t,
                    ),
                );
            }
            _ => {
                let b = self.place(base);
                let i = self.expr(index);
                self.assign(dest, Rvalue::Use(Operand::Copy(b.index(i))));
            }
        }
    }

    fn lower_binary(&mut self, op: ast::BinOp, lhs: ExprId, rhs: ExprId, t: Ty, dest: Place) {
        use ast::BinOp::*;
        match op {
            And | Or => {
                // Short-circuit.
                self.expr_into(lhs, dest.clone());
                let rhs_bb = self.new_block();
                let join = self.new_block();
                let c = Operand::Copy(dest.clone());
                if op == And {
                    self.terminate(Terminator::If(c, rhs_bb, join));
                } else {
                    self.terminate(Terminator::If(c, join, rhs_bb));
                }
                self.cur = rhs_bb;
                self.expr_into(rhs, dest);
                self.goto(join);
                self.cur = join;
            }
            Add if matches!(self.lw.tys.kind(t), TyKind::Str) => {
                let a = self.expr(lhs);
                let b = self.expr(rhs);
                self.assign(
                    dest,
                    Rvalue::Call(
                        Callee::Intrinsic(Intrinsic::new(Op::StrConcat, vec![])),
                        vec![Arg::Value(a), Arg::Value(b)],
                        t,
                    ),
                );
            }
            _ => {
                let lt = self.ty(lhs);
                let a = self.expr(lhs);
                let b = self.expr(rhs);
                self.assign(dest, Rvalue::Binary(op, a, b, lt));
            }
        }
    }

    fn lower_try(&mut self, inner: ExprId, t: Ty, dest: Place) {
        let it = self.ty(inner);
        let tmp = self.temp(it);
        self.expr_into(inner, Place::local(tmp));
        let TyKind::Adt(adt, args) = self.lw.tys.kind(it).clone() else { return };
        let d = self.temp(self.lw.tys.int);
        self.assign(Place::local(d), Rvalue::Discriminant(Place::local(tmp)));
        let ok_bb = self.new_block();
        let err_bb = self.new_block();
        self.terminate(Terminator::Switch(Operand::local(d), vec![(0, ok_bb)], err_bb));
        // Failure: return the error (or None) from this function.
        self.cur = err_bb;
        let ret = self.ret_ty;
        let is_result = Some(adt) == self.lw.defs.result;
        if is_result {
            let e = Place::local(tmp).variant_field(1, 0);
            self.assign(
                Place::local(Local(0)),
                Rvalue::Aggregate(AggKind::Variant(adt, 1), ret, vec![Operand::Copy(e)]),
            );
        } else {
            self.assign(Place::local(Local(0)), Rvalue::Aggregate(AggKind::Variant(adt, 1), ret, vec![]));
        }
        let _ = args;
        self.drop_scopes_to(1);
        let exit = self.exit;
        self.goto(exit);
        self.cur = ok_bb;
        let _ = t;
        self.assign(dest, Rvalue::Use(Operand::Copy(Place::local(tmp).variant_field(0, 0))));
    }

    fn block_into(&mut self, block: ExprId, dest: Option<Place>) {
        let ExprKind::Block(stmts) = self.ast.exprs[block].kind else {
            match dest {
                Some(d) => self.expr_into(block, d),
                None => {
                    let _ = self.expr(block);
                }
            }
            return;
        };
        self.push_scope();
        let ids: Vec<StmtId> = self.ast.stmt_lists.get(stmts).to_vec();
        let n = ids.len();
        for (i, s) in ids.into_iter().enumerate() {
            if i + 1 == n {
                if let (StmtKind::Expr(e), Some(d)) = (self.ast.stmts[s].kind, dest.as_ref()) {
                    self.cur_span = self.ast.stmts[s].span;
                    self.expr_into(e, d.clone());
                    continue;
                }
            }
            self.stmt(s);
        }
        self.pop_scope();
    }

    fn lower_if(&mut self, cond: ExprId, then: ExprId, els: Option<ExprId>, dest: Option<Place>) {
        let c = self.expr(cond);
        let then_bb = self.new_block();
        let else_bb = self.new_block();
        let join = self.new_block();
        self.terminate(Terminator::If(c, then_bb, else_bb));
        self.cur = then_bb;
        self.block_into(then, dest.clone());
        self.goto(join);
        self.cur = else_bb;
        if let Some(el) = els {
            match self.ast.exprs[el].kind {
                ExprKind::If { cond, then, els } => self.lower_if(cond, then, els, dest),
                _ => self.block_into(el, dest),
            }
        }
        self.goto(join);
        self.cur = join;
    }

    fn lower_match(&mut self, scrut: ExprId, arms: List<ast::Arm>, dest: Option<Place>) {
        let st = self.ty(scrut);
        let s = self.temp(st);
        self.expr_into(scrut, Place::local(s));
        let join = self.new_block();
        let arms: Vec<ast::Arm> = self.ast.arms.get(arms).to_vec();
        for arm in arms {
            let next = self.new_block();
            self.push_scope();
            self.test_pat(arm.pat, Place::local(s), next);
            if let Some(g) = arm.guard {
                let gop = self.expr(g);
                let body_bb = self.new_block();
                self.terminate(Terminator::If(gop, body_bb, next));
                self.cur = body_bb;
            }
            self.cur_span = arm.span;
            match &dest {
                Some(d) => {
                    let d = d.clone();
                    match self.ast.exprs[arm.body].kind {
                        ExprKind::Block(_) => self.block_into(arm.body, Some(d)),
                        _ => self.expr_into(arm.body, d),
                    }
                }
                None => match self.ast.exprs[arm.body].kind {
                    ExprKind::Block(_) => self.block_into(arm.body, None),
                    _ => {
                        let _ = self.expr(arm.body);
                    }
                },
            }
            self.pop_scope();
            self.goto(join);
            self.cur = next;
        }
        self.terminate(Terminator::Unreachable);
        self.cur = join;
    }

    /// Test `pat` against `place`; continue in the current block on success, jump to `fail`
    /// otherwise. Bindings are assigned as the test proceeds.
    fn test_pat(&mut self, pat: PatId, place: Place, fail: BlockId) {
        match self.ast.pats[pat].kind {
            PatKind::Wild | PatKind::Error => {}
            PatKind::Bind(_) => {
                if let Some(&id) = self.tables.pat_local.get(&pat) {
                    let l = self.sema_local(id);
                    self.assign(Place::local(l), Rvalue::Use(Operand::Copy(place)));
                }
            }
            PatKind::Lit(e) => {
                let pt = self.tables.pat_ty[pat.0 as usize];
                let pt = self.subst(pt);
                let lit = self.expr(e);
                let c = self.temp(self.lw.tys.bool);
                self.assign(Place::local(c), Rvalue::Binary(ast::BinOp::Eq, Operand::Copy(place), lit, pt));
                let ok = self.new_block();
                self.terminate(Terminator::If(Operand::local(c), ok, fail));
                self.cur = ok;
            }
            PatKind::Variant { args, .. } => {
                let Some(&(_, vi)) = self.tables.pat_variant.get(&pat) else { return };
                let d = self.temp(self.lw.tys.int);
                self.assign(Place::local(d), Rvalue::Discriminant(place.clone()));
                let ok = self.new_block();
                self.terminate(Terminator::Switch(Operand::local(d), vec![(vi as i128, ok)], fail));
                self.cur = ok;
                for (i, &sp) in self.ast.pat_lists.get(args).to_vec().iter().enumerate() {
                    self.test_pat(sp, place.variant_field(vi, i as u32), fail);
                }
            }
            PatKind::Tuple(list) => {
                for (i, &sp) in self.ast.pat_lists.get(list).to_vec().iter().enumerate() {
                    self.test_pat(sp, place.field(i as u32), fail);
                }
            }
            PatKind::Or(list) => {
                let alts: Vec<PatId> = self.ast.pat_lists.get(list).to_vec();
                let success = self.new_block();
                let n = alts.len();
                for (i, a) in alts.into_iter().enumerate() {
                    let alt_fail = if i + 1 == n { fail } else { self.new_block() };
                    self.test_pat(a, place.clone(), alt_fail);
                    self.goto(success);
                    self.cur = alt_fail;
                }
                self.cur = success;
            }
        }
    }

    fn lower_struct_lit(&mut self, e: ExprId, fields: List<ast::FieldInit>, t: Ty, dest: Place) {
        let TyKind::Adt(adt, args) = self.lw.tys.kind(t).clone() else { return };
        let inits: Vec<ast::FieldInit> = self.ast.field_inits.get(fields).to_vec();
        let decl: Vec<Symbol> = self.lw.defs.adts[adt].fields().iter().map(|f| f.name).collect();
        // Evaluate in source order, assemble in declaration order.
        let mut vals: Vec<Option<Operand>> = vec![None; decl.len()];
        for init in inits {
            let op = self.expr(init.value);
            let op = match op {
                Operand::Copy(p) if !p.proj.is_empty() || p.local.0 != 0 => {
                    // Freeze the value now so later field initializers can't observe changes.
                    let ft = self.ty(init.value);
                    let l = self.temp(ft);
                    self.assign(Place::local(l), Rvalue::Use(Operand::Copy(p)));
                    Operand::local(l)
                }
                o => o,
            };
            if let Some(i) = decl.iter().position(|&n| n == init.name.sym) {
                vals[i] = Some(op);
            }
        }
        let ops: Vec<Operand> = vals.into_iter().map(|o| o.unwrap_or(Operand::unit())).collect();
        self.assign(dest.clone(), Rvalue::Aggregate(AggKind::Struct(adt), t, ops));
        let _ = (e, args);
        if self.lw.mir.opts.check_contracts {
            self.check_struct_predicates(adt, dest);
        }
    }

    /// After an assignment through a projection, re-check the predicates of every struct on
    /// the path and the refinement of the assigned field.
    fn check_mutated(&mut self, p: Place) {
        let mut t = self.locals[p.local].ty;
        let mut prefix = Place::local(p.local);
        let n = p.proj.len();
        for (k, el) in p.proj.iter().enumerate() {
            let parent = t;
            t = match el {
                PlaceElem::Field(i) => match self.lw.tys.kind(parent).clone() {
                    TyKind::Adt(adt, args) => {
                        let ft = self.lw.defs.adts[adt].fields()[*i as usize].ty;
                        self.lw.tys.subst(ft, &args)
                    }
                    TyKind::Tuple(ts) => ts[*i as usize],
                    _ => return,
                },
                PlaceElem::Index(_) => match self.lw.tys.kind(parent) {
                    TyKind::List(e) => *e,
                    _ => return,
                },
                PlaceElem::VariantField(..) => return,
            };
            prefix.proj.push(el.clone());
            if let (PlaceElem::Field(i), TyKind::Adt(adt, _)) = (el, self.lw.tys.kind(parent).clone()) {
                if k + 1 == n {
                    if let Some(al) = self.lw.defs.adts[adt].fields()[*i as usize].refine {
                        self.check_alias(al, prefix.clone());
                    }
                }
            }
        }
        // Struct predicates, innermost first.
        for k in (0..n).rev() {
            let q = Place { local: p.local, proj: p.proj[..k].to_vec() };
            let qt = self.place_ty(&q);
            if let TyKind::Adt(adt, _) = *self.lw.tys.kind(qt) {
                if !self.lw.defs.adts[adt].is_enum() {
                    self.check_struct_predicates(adt, q);
                }
            }
        }
    }

    fn place_ty(&mut self, p: &Place) -> Ty {
        let mut t = self.locals[p.local].ty;
        for el in &p.proj {
            t = match (el, self.lw.tys.kind(t).clone()) {
                (PlaceElem::Field(i), TyKind::Adt(adt, args)) => {
                    let ft = self.lw.defs.adts[adt].fields()[*i as usize].ty;
                    self.lw.tys.subst(ft, &args)
                }
                (PlaceElem::Field(i), TyKind::Tuple(ts)) => ts[*i as usize],
                (PlaceElem::Index(_), TyKind::List(e)) => e,
                _ => return self.lw.tys.error,
            };
        }
        t
    }

    /// Check a refinement alias's predicate (`self` names the value).
    fn check_alias(&mut self, al: AliasId, value: Place) {
        let ad = &self.lw.defs.aliases[al];
        let Some(pred) = ad.refine else { return };
        let pkg = ad.pkg;
        let t = self.place_ty(&value);
        let l = self.temp(t);
        self.assign(Place::local(l), Rvalue::Use(Operand::Copy(value)));
        let owner = ad.name;
        self.in_package(pkg, |b| b.check_with_names(pred, &[(sym::SELF_LOWER, l)], CheckKind::Refinement, owner));
    }

    /// Run `f` with the AST and tables of another package (for predicates written there).
    fn in_package(&mut self, pkg: PkgId, f: impl FnOnce(&mut Self)) {
        let (ast, tables) = (self.ast, self.tables);
        self.ast = &self.lw.pkgs[pkg].ast;
        self.tables = &self.lw.tables[pkg];
        f(self);
        self.ast = ast;
        self.tables = tables;
    }

    /// Field refinements and struct invariants at construction.
    fn check_struct_predicates(&mut self, adt: AdtId, value: Place) {
        let ad = &self.lw.defs.adts[adt];
        let pkg = ad.pkg;
        let aliases: Vec<(u32, AliasId)> =
            ad.fields().iter().enumerate().filter_map(|(i, f)| f.refine.map(|al| (i as u32, al))).collect();
        for (i, al) in aliases {
            self.check_alias(al, value.field(i));
        }
        self.in_package(pkg, |b| b.check_struct_predicates_here(adt, value));
    }

    fn check_struct_predicates_here(&mut self, adt: AdtId, value: Place) {
        let ad = &self.lw.defs.adts[adt];
        let item = self.ast.items[ad.item];
        let ItemKind::Struct { fields, invariants, .. } = item.kind else { return };
        let fdecls: Vec<ast::FieldDecl> = self.ast.fields.get(fields).to_vec();
        let invs: Vec<ExprId> = self.ast.expr_lists.get(invariants).to_vec();
        let has = !invs.is_empty() || fdecls.iter().any(|f| f.refine.is_some());
        if !has {
            return;
        }
        // Predicates refer to fields by name: bind each to a fresh local, then check.
        let mut bound: Vec<(Symbol, Local)> = Vec::new();
        for (i, f) in fdecls.iter().enumerate() {
            let ft = self.lw.defs.adts[adt].fields()[i].ty;
            let l = self.temp(ft);
            self.assign(Place::local(l), Rvalue::Use(Operand::Copy(value.field(i as u32))));
            bound.push((f.name.sym, l));
        }
        let mut preds: Vec<ExprId> = fdecls.iter().filter_map(|f| f.refine).collect();
        preds.extend(invs);
        // The checker resolved these names to its own locals; map by name.
        for pred in preds {
            let owner = self.lw.defs.adts[adt].name;
            self.check_with_names(pred, &bound, CheckKind::Invariant, owner);
        }
    }

    /// Check a predicate whose free names are given by `names` (struct predicates are checked
    /// outside any function body, so their locals are matched by name).
    /// The check is reported at the current site (construction, assignment, call), naming the
    /// type whose predicate failed and the values involved.
    fn check_with_names(&mut self, pred: ExprId, names: &[(Symbol, Local)], kind: CheckKind, owner: Symbol) {
        let text = self.src(pred);
        let span = self.cur_span;
        let c = self.temp(self.lw.tys.bool);
        let ok = self.eval_pred_names(pred, names, Place::local(c));
        if ok {
            let mut values = Vec::new();
            for &(n, l) in names {
                if text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).any(|w| w == n.as_str()) {
                    values.push((n, Operand::local(l), self.locals[l].ty));
                }
            }
            self.push(Stmt::Check {
                cond: Operand::local(c),
                kind,
                msg: Symbol::intern(&format!("`{owner}` requires {text}")),
                values,
                span,
            });
        }
    }

    /// A small evaluator for struct predicates: names, literals, comparisons, arithmetic,
    /// logic, field access and `.len()`. Returns false if the predicate uses something else.
    fn eval_pred_names(&mut self, e: ExprId, names: &[(Symbol, Local)], dest: Place) -> bool {
        let k = self.ast.exprs[e].kind;
        match k {
            ExprKind::Name(n) => match names.iter().find(|(x, _)| *x == n) {
                Some(&(_, l)) => {
                    self.assign(dest, Rvalue::Use(Operand::local(l)));
                    true
                }
                None => false,
            },
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Bool(_) | ExprKind::Str(_) | ExprKind::Char(_) => {
                let op = self.expr(e);
                self.assign(dest, Rvalue::Use(op));
                true
            }
            ExprKind::Paren(x) => self.eval_pred_names(x, names, dest),
            ExprKind::Unary { op, expr } => {
                let t = self.ty(expr);
                let l = self.temp(t);
                if !self.eval_pred_names(expr, names, Place::local(l)) {
                    return false;
                }
                self.assign(dest, Rvalue::Unary(op, Operand::local(l), t));
                true
            }
            ExprKind::Binary { op: op @ (ast::BinOp::And | ast::BinOp::Or), lhs, rhs } => {
                // Short-circuit, as in ordinary code.
                if !self.eval_pred_names(lhs, names, dest.clone()) {
                    return false;
                }
                let rhs_bb = self.new_block();
                let join = self.new_block();
                let c = Operand::Copy(dest.clone());
                if op == ast::BinOp::And {
                    self.terminate(Terminator::If(c, rhs_bb, join));
                } else {
                    self.terminate(Terminator::If(c, join, rhs_bb));
                }
                self.cur = rhs_bb;
                let ok = self.eval_pred_names(rhs, names, dest);
                self.goto(join);
                self.cur = join;
                ok
            }
            ExprKind::Binary { op, lhs, rhs } => {
                let t = self.ty(lhs);
                let a = self.temp(t);
                let b = self.temp(t);
                if !self.eval_pred_names(lhs, names, Place::local(a))
                    || !self.eval_pred_names(rhs, names, Place::local(b))
                {
                    return false;
                }
                if matches!(op, ast::BinOp::Add) && matches!(self.lw.tys.kind(t), TyKind::Str) {
                    return false;
                }
                self.assign(dest, Rvalue::Binary(op, Operand::local(a), Operand::local(b), t));
                true
            }
            _ => false,
        }
    }

    fn lower_closure(&mut self, e: ExprId, params: List<ast::Param>, body: ExprId, t: Ty, dest: Place) {
        let captures: Vec<LocalId> = self.tables.closure_captures.get(&e).cloned().unwrap_or_default();
        let TyKind::Fn(_, ret, _) = self.lw.tys.kind(t).clone() else { return };
        let span = self.ast.exprs[e].span;
        let m = self.lw.mir.fns.push(MFn {
            name: format!("{}::closure", self.lw.mir.fns[self.mfn].name),
            source: None,
            type_args: self.inst.clone(),
            params: Vec::new(),
            n_captures: captures.len(),
            ret,
            locals: IndexVec::new(),
            blocks: IndexVec::new(),
            intrinsic: None,
            span,
            lowered: false,
        });
        let cap_ops: Vec<Operand> = captures.iter().map(|&id| Operand::local(self.sema_local(id))).collect();
        // Build the closure body in its own builder.
        {
            let body_ref = self.body;
            let inst = self.inst.clone();
            let pkg = self.pkg;
            let mut b = FnBuilder::new(self.lw, pkg, body_ref, inst, m);
            b.ret_ty = ret;
            for &id in &captures {
                let l = b.sema_local(id);
                b.params.push((l, Mode::Value));
            }
            b.n_captures = captures.len();
            // Closure parameters: the checker declared them as locals in order; find them by
            // matching the parameter names declared at this closure's depth.
            let ps: Vec<ast::Param> = b.ast.params.get(params).to_vec();
            for p in &ps {
                let id = b.body.locals.iter_enumerated().find(|(_, d)| d.span == p.name.span).map(|(i, _)| i);
                if let Some(id) = id {
                    let l = b.sema_local(id);
                    b.params.push((l, Mode::Value));
                }
            }
            if let Some(s) = b.scopes.last_mut() {
                s.clear();
            }
            let ret_place = Place::local(Local(0));
            b.block_into(body, Some(ret_place));
            b.finish();
        }
        self.assign(dest, Rvalue::Closure(m, cap_ops));
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_quant(
        &mut self,
        e: ExprId,
        forall: bool,
        var: ast::Name,
        lo: ExprId,
        hi: ExprId,
        body: ExprId,
        dest: Place,
    ) {
        let it = self.ty(lo);
        let id = self
            .body
            .locals
            .iter_enumerated()
            .find(|(_, d)| d.span == var.span && d.kind == LocalKind::Quant)
            .map(|(i, _)| i);
        let i = match id {
            Some(id) => self.sema_local(id),
            None => self.temp(it),
        };
        self.expr_into(lo, Place::local(i));
        let h = self.temp(it);
        self.expr_into(hi, Place::local(h));
        self.assign(dest.clone(), Rvalue::Use(Operand::bool(forall)));
        let head = self.new_block();
        let body_bb = self.new_block();
        let found = self.new_block();
        let latch = self.new_block();
        let exit = self.new_block();
        self.goto(head);
        self.cur = head;
        let c = self.temp(self.lw.tys.bool);
        self.assign(Place::local(c), Rvalue::Binary(ast::BinOp::Lt, Operand::local(i), Operand::local(h), it));
        self.terminate(Terminator::If(Operand::local(c), body_bb, exit));
        self.cur = body_bb;
        let b = self.expr(body);
        if forall {
            self.terminate(Terminator::If(b, latch, found));
        } else {
            self.terminate(Terminator::If(b, found, latch));
        }
        self.cur = found;
        self.assign(dest, Rvalue::Use(Operand::bool(!forall)));
        self.goto(exit);
        self.cur = latch;
        let one = Operand::Const(Const::Int(
            1,
            match self.lw.tys.kind(it) {
                TyKind::Int(k, _) => *k,
                _ => IntTy::I64,
            },
        ));
        self.assign(Place::local(i), Rvalue::Binary(ast::BinOp::Add, Operand::local(i), one, it));
        self.goto(head);
        self.cur = exit;
        let _ = e;
    }

    // ---------- calls ----------

    fn lower_call(&mut self, e: ExprId, callee: ExprId, args: List<ExprId>, dest: Place) {
        let arg_ids: Vec<ExprId> = self.ast.expr_lists.get(args).to_vec();
        let ret = self.ty(e);
        let res = self.tables.expr_res[callee.0 as usize];
        match res {
            Res::Old => {
                if let Some(&l) = self.olds.get(&e) {
                    self.assign(dest, Rvalue::Use(Operand::local(l)));
                } else if let Some(&a) = arg_ids.first() {
                    self.expr_into(a, dest);
                }
            }
            Res::Variant(adt, v) => {
                let ops: Vec<Operand> = arg_ids.iter().map(|&a| self.expr(a)).collect();
                self.assign(dest, Rvalue::Aggregate(AggKind::Variant(adt, v), ret, ops));
            }
            Res::Fn(f) | Res::Method(f) => {
                // assert(cond) becomes a check that reports operand values; the condition is
                // evaluated exactly once, by the check.
                let fdef = &self.lw.defs.fns[f];
                if fdef.name == sym::ASSERT && !fdef.has_body && self.lw.pkgs[fdef.pkg].path.as_str() == "<prelude>" {
                    if let Some(&c) = arg_ids.first() {
                        self.check(c, CheckKind::Assert);
                    }
                    self.assign(dest, Rvalue::Use(Operand::unit()));
                    return;
                }
                let inst = self.inst_of(callee);
                let fd = &self.lw.defs.fns[f];
                let mut call_args: Vec<Arg> = Vec::new();
                if let (Res::Method(_), ExprKind::Field { base, .. }) = (res, self.ast.exprs[callee].kind) {
                    if fd.self_mode == Some(Mode::MutRef) {
                        let p = self.place(base);
                        call_args.push(Arg::MutRef(p));
                    } else {
                        let o = self.expr(base);
                        call_args.push(Arg::Value(o));
                    }
                }
                let modes: Vec<Mode> = self.lw.defs.fns[f].params.iter().map(|p| p.mode).collect();
                self.push_args(&arg_ids, &modes, &mut call_args);
                let m = self.lw.instance(f, inst);
                let callee_op = match self.lw.mir.fns[m].intrinsic.clone() {
                    Some(i) => Callee::Intrinsic(i),
                    None => Callee::Fn(m),
                };
                self.assign(dest, Rvalue::Call(callee_op, call_args, ret));
                if self.lw.tys.is_never(ret) {
                    self.terminate_and_continue(Terminator::Unreachable);
                }
            }
            Res::IfaceMethod(iid, mi) => {
                let ExprKind::Field { base, .. } = self.ast.exprs[callee].kind else { return };
                let rt = self.ty(base);
                let mf = self.lw.defs.ifaces[iid].methods[mi as usize];
                let mname = self.lw.defs.fns[mf].name;
                let key = TyKey::of(self.lw.tys, rt);
                let Some(f) = key.and_then(|k| self.lw.defs.method(k, mname)) else { return };
                let recv_args: Vec<Ty> = match self.lw.tys.kind(rt).clone() {
                    TyKind::Adt(_, a) => a.to_vec(),
                    TyKind::List(x) | TyKind::Set(x) => vec![x],
                    TyKind::Map(k, v) => vec![k, v],
                    _ => vec![],
                };
                let mut call_args = Vec::new();
                if self.lw.defs.fns[f].self_mode == Some(Mode::MutRef) {
                    let p = self.place(base);
                    call_args.push(Arg::MutRef(p));
                } else {
                    let o = self.expr(base);
                    call_args.push(Arg::Value(o));
                }
                let modes: Vec<Mode> = self.lw.defs.fns[f].params.iter().map(|p| p.mode).collect();
                self.push_args(&arg_ids, &modes, &mut call_args);
                let m = self.lw.instance(f, recv_args.into_boxed_slice());
                let callee_op = match self.lw.mir.fns[m].intrinsic.clone() {
                    Some(i) => Callee::Intrinsic(i),
                    None => Callee::Fn(m),
                };
                self.assign(dest, Rvalue::Call(callee_op, call_args, ret));
            }
            Res::BoundMethod(_) => {
                let ExprKind::Field { base, .. } = self.ast.exprs[callee].kind else { return };
                let rt = self.ty(base);
                let o = self.expr(base);
                // A user-defined `str` wins over the derived one.
                let user = TyKey::of(self.lw.tys, rt).and_then(|k| self.lw.defs.method(k, sym::STR_METHOD));
                if let (Some(f), TyKind::Adt(_, a)) = (user, self.lw.tys.kind(rt).clone()) {
                    let m = self.lw.instance(f, a);
                    self.assign(dest, Rvalue::Call(Callee::Fn(m), vec![Arg::Value(o)], ret));
                } else {
                    self.assign(
                        dest,
                        Rvalue::Call(Callee::Intrinsic(Intrinsic::new(Op::Show, vec![rt])), vec![Arg::Value(o)], ret),
                    );
                }
            }
            Res::Field(i) => {
                let ExprKind::Field { base, .. } = self.ast.exprs[callee].kind else { return };
                let b = self.place(base);
                let mut call_args = Vec::new();
                self.push_args(&arg_ids, &vec![Mode::Value; arg_ids.len()], &mut call_args);
                self.assign(dest, Rvalue::Call(Callee::Value(Operand::Copy(b.field(i))), call_args, ret));
            }
            _ => {
                let f = self.expr(callee);
                let mut call_args = Vec::new();
                self.push_args(&arg_ids, &vec![Mode::Value; arg_ids.len()], &mut call_args);
                self.assign(dest, Rvalue::Call(Callee::Value(f), call_args, ret));
                if self.lw.tys.is_never(ret) {
                    self.terminate_and_continue(Terminator::Unreachable);
                }
            }
        }
    }

    fn push_args(&mut self, args: &[ExprId], modes: &[Mode], out: &mut Vec<Arg>) {
        for (i, &a) in args.iter().enumerate() {
            let mode = modes.get(i).copied().unwrap_or(Mode::Value);
            match (mode, self.ast.exprs[a].kind) {
                (Mode::MutRef, ExprKind::MutArg(place)) => {
                    let p = self.place(place);
                    out.push(Arg::MutRef(p));
                }
                _ => {
                    let o = self.expr(a);
                    // Freeze place operands so later arguments can't change them.
                    let o = match o {
                        Operand::Copy(p) if !p.proj.is_empty() => {
                            let t = self.ty(a);
                            let l = self.temp(t);
                            self.assign(Place::local(l), Rvalue::Use(Operand::Copy(p)));
                            Operand::local(l)
                        }
                        o => o,
                    };
                    out.push(Arg::Value(o));
                }
            }
        }
    }
}

/// Push the direct sub-expressions of `e`.
pub fn visit_children(ast: &Ast, e: ExprId, out: &mut Vec<ExprId>) {
    match ast.exprs[e].kind {
        ExprKind::Paren(x) | ExprKind::Try(x) | ExprKind::MutArg(x) => out.push(x),
        ExprKind::Unary { expr, .. } | ExprKind::Cast { expr, .. } => out.push(expr),
        ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } | ExprKind::TypeArgs { base, .. } => {
            out.push(base)
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            out.push(lhs);
            out.push(rhs);
        }
        ExprKind::Index { base, index } => {
            out.push(base);
            out.push(index);
        }
        ExprKind::Range { lo, hi } => {
            out.extend(lo);
            out.extend(hi);
        }
        ExprKind::Call { callee, args } => {
            out.push(callee);
            out.extend(ast.expr_lists.get(args).iter().copied());
        }
        ExprKind::Tuple(l) | ExprKind::List(l) => out.extend(ast.expr_lists.get(l).iter().copied()),
        ExprKind::If { cond, then, els } => {
            out.push(cond);
            out.push(then);
            out.extend(els);
        }
        ExprKind::Match { scrut, arms } => {
            out.push(scrut);
            for a in ast.arms.get(arms) {
                out.extend(a.guard);
                out.push(a.body);
            }
        }
        ExprKind::Block(stmts) => {
            for &s in ast.stmt_lists.get(stmts) {
                match ast.stmts[s].kind {
                    StmtKind::Let { init, .. } => out.push(init),
                    StmtKind::Assign { target, value, .. } => {
                        out.push(target);
                        out.push(value);
                    }
                    StmtKind::Expr(x) => out.push(x),
                    StmtKind::Return(v) => out.extend(v),
                    StmtKind::For { head, invariants, body } => {
                        match head {
                            ForHead::Cond(c) => out.push(c),
                            ForHead::In { iter, .. } => out.push(iter),
                            ForHead::Infinite => {}
                        }
                        out.extend(ast.expr_lists.get(invariants).iter().copied());
                        out.push(body);
                    }
                    _ => {}
                }
            }
        }
        ExprKind::StructLit { fields, .. } => out.extend(ast.field_inits.get(fields).iter().map(|f| f.value)),
        ExprKind::Closure { body, .. } => out.push(body),
        ExprKind::Quant { lo, hi, body, .. } => {
            out.push(lo);
            out.push(hi);
            out.push(body);
        }
        _ => {}
    }
}
