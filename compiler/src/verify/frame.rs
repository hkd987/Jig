//! Frames: which parts of each `&mut` parameter a function may change.
//!
//! After a call, the prover keeps everything about an `&mut` argument except the parts the
//! callee can write, so callers need no `ensures self.x == old(self.x)` for fields a method
//! never touches. A frame is read off the callee's body: assignments to the parameter's
//! fields, `&mut` methods called on them, and `&mut` arguments passed on to other functions
//! (through their frames). Anything else (recursion, function values, generic calls) changes
//! the whole parameter. A caller's proof depends on its callees' frames, so they are part of
//! its cache key.

use crate::sema::defs::{Mode, Program, Res};
use crate::sema::ty::FnId;
use crate::syntax::ast::{self, ExprId, ExprKind, StmtKind};
use crate::util::{FxHashMap, FxHashSet};

/// The parts of a parameter a function may change: field-index paths from the parameter
/// (the whole subtree under each), or all of it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    Whole,
    Paths(Vec<Vec<u32>>),
}

impl Frame {
    fn add(&mut self, path: Vec<u32>) {
        let Frame::Paths(ps) = self else { return };
        if path.is_empty() {
            *self = Frame::Whole;
            return;
        }
        if ps.iter().any(|p| path.starts_with(p)) {
            return;
        }
        ps.retain(|p| !p.starts_with(&path));
        ps.push(path);
        ps.sort();
    }

    fn add_frame(&mut self, prefix: &[u32], f: &Frame) {
        match f {
            Frame::Whole => self.add(prefix.to_vec()),
            Frame::Paths(ps) => {
                for p in ps {
                    let mut q = prefix.to_vec();
                    q.extend_from_slice(p);
                    self.add(q);
                }
            }
        }
    }

    /// For cache keys.
    pub fn summary(&self) -> String {
        match self {
            Frame::Whole => "*".into(),
            Frame::Paths(ps) => ps
                .iter()
                .map(|p| p.iter().map(|i| i.to_string()).collect::<Vec<_>>().join("."))
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

pub struct Frames<'a> {
    prog: &'a Program,
    done: FxHashMap<(FnId, usize), Frame>,
    active: FxHashSet<FnId>,
}

impl<'a> Frames<'a> {
    pub fn new(prog: &'a Program) -> Frames<'a> {
        Frames { prog, done: FxHashMap::default(), active: FxHashSet::default() }
    }

    /// The frame of parameter `i` of `f` (the receiver of a method is parameter 0).
    pub fn of(&mut self, f: FnId, i: usize) -> Frame {
        if let Some(fr) = self.done.get(&(f, i)) {
            return fr.clone();
        }
        let fr = self.compute(f, i);
        self.done.insert((f, i), fr.clone());
        fr
    }

    fn compute(&mut self, f: FnId, i: usize) -> Frame {
        let prog = self.prog;
        let fd = &prog.defs.fns[f];
        let (Some(body), true) = (prog.bodies[f].as_ref(), fd.has_body) else { return Frame::Whole };
        let Some(&param) = body.param_locals.get(i) else { return Frame::Whole };
        let pkg = &prog.pkgs[fd.pkg];
        let ast::ItemKind::Fn(decl) = pkg.ast.items[fd.item].kind else { return Frame::Whole };
        let Some(root) = decl.body else { return Frame::Whole };
        // Recursion: assume the worst.
        if !self.active.insert(f) {
            return Frame::Whole;
        }
        let ast = &pkg.ast;
        let res = &prog.tables[fd.pkg].expr_res;
        let place = |e: ExprId| -> Option<Vec<u32>> { place_path(ast, res, e, param) };
        let mut frame = Frame::Paths(Vec::new());
        let mut stack = vec![root];
        while let Some(x) = stack.pop() {
            if frame == Frame::Whole {
                break;
            }
            if let ExprKind::Block(stmts) = ast.exprs[x].kind {
                for &s in ast.stmt_lists.get(stmts) {
                    if let StmtKind::Assign { target, .. } = ast.stmts[s].kind {
                        if let Some(p) = place(target) {
                            frame.add(p);
                        }
                    }
                }
            }
            match ast.exprs[x].kind {
                ExprKind::Call { callee, args } => {
                    let args = ast.expr_lists.get(args);
                    let target = match res[callee.0 as usize] {
                        Res::Fn(g) | Res::Method(g) => Some(g),
                        _ => None,
                    };
                    // The receiver of a method taking `&mut self` (known or through a bound).
                    let mut offset = 0;
                    if let ExprKind::Field { base, .. } = ast.exprs[callee].kind {
                        // `Type.method(x, ...)` passes the receiver as the first argument.
                        let on_type = matches!(res[base.0 as usize], Res::Type(_));
                        let method =
                            !on_type && matches!(res[callee.0 as usize], Res::Method(_) | Res::IfaceMethod(..));
                        if method && target.is_none_or(|g| prog.defs.fns[g].self_mode.is_some()) {
                            offset = 1;
                        }
                        if prog.tables[fd.pkg].autoref.get(&callee) == Some(&true) {
                            if let Some(p) = place(base) {
                                match target {
                                    Some(g) => {
                                        let gf = self.of(g, 0);
                                        frame.add_frame(&p, &gf);
                                    }
                                    None => frame.add(p),
                                }
                            }
                        }
                    }
                    for (k, &a) in args.iter().enumerate() {
                        if let ExprKind::MutArg(inner) = ast.exprs[a].kind {
                            if let Some(p) = place(inner) {
                                match target {
                                    Some(g) => {
                                        let gf = self.of(g, k + offset);
                                        frame.add_frame(&p, &gf);
                                    }
                                    None => frame.add(p),
                                }
                            }
                        }
                    }
                }
                ExprKind::Closure { .. } => continue,
                _ => {}
            }
            crate::mir::lower::visit_children(ast, x, &mut stack);
        }
        self.active.remove(&f);
        frame
    }
}

/// The field path from `param` to the place `e`, up to the first index (writing an element
/// may change everything under the list).
fn place_path(ast: &ast::Ast, res: &[Res], e: ExprId, param: crate::sema::defs::LocalId) -> Option<Vec<u32>> {
    match ast.exprs[e].kind {
        ExprKind::Name(_) => match res[e.0 as usize] {
            Res::Local(id) if id == param => Some(Vec::new()),
            _ => None,
        },
        ExprKind::Paren(x) => place_path(ast, res, x, param),
        ExprKind::Field { base, .. } => {
            let Res::Field(i) = res[e.0 as usize] else { return None };
            let mut p = place_path(ast, res, base, param)?;
            p.push(i);
            Some(p)
        }
        ExprKind::TupleField { base, index, .. } => {
            let mut p = place_path(ast, res, base, param)?;
            p.push(index);
            Some(p)
        }
        // Below an index the path stops: the whole list may change.
        ExprKind::Index { base, .. } => place_path(ast, res, base, param),
        _ => None,
    }
}

/// Every function's frames, for each `&mut` parameter (the receiver of a method is 0).
pub fn compute_all(prog: &Program) -> FxHashMap<(FnId, usize), Frame> {
    let mut fr = Frames::new(prog);
    for (f, fd) in prog.defs.fns.iter_enumerated() {
        let recv = fd.self_mode.is_some() as usize;
        if fd.self_mode == Some(Mode::MutRef) {
            fr.of(f, 0);
        }
        for (i, p) in fd.params.iter().enumerate() {
            if p.mode == Mode::MutRef {
                fr.of(f, i + recv);
            }
        }
    }
    fr.done
}
