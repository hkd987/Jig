//! Match exhaustiveness and redundancy (Maranget's usefulness algorithm), with a witness
//! for the first missing case.

use super::defs::*;
use super::ty::*;
use crate::syntax::ast::{Ast, ExprKind, PatId, PatKind, UnOp};
use crate::util::Symbol;

#[derive(Clone, PartialEq, Debug)]
enum Ctor {
    Variant(u32),
    Bool(bool),
    Tuple,
    Int(i128),
    Str(Symbol),
    Char(u32),
    /// Something we cannot reason about (floats, errors): matches nothing specific.
    Opaque,
}

#[derive(Clone, Debug)]
enum P {
    Wild,
    Ctor(Ctor, Vec<P>),
    Or(Vec<P>),
}

pub struct Report {
    /// A rendering of a value no arm matches.
    pub missing: Option<String>,
    /// Indices of arms that can never match.
    pub unreachable: Vec<usize>,
}

struct Cx<'a> {
    ast: &'a Ast,
    tables: &'a Tables,
    defs: &'a Defs,
    tys: &'a mut TyCtx,
    budget: u32,
}

pub fn check(ast: &Ast, tables: &Tables, defs: &Defs, tys: &mut TyCtx, scrut: Ty, arms: &[(PatId, bool)]) -> Report {
    let mut cx = Cx { ast, tables, defs, tys, budget: 100_000 };
    let pats: Vec<P> = arms.iter().map(|(p, _)| cx.lower(*p)).collect();
    let mut rows: Vec<Vec<P>> = Vec::new();
    let mut unreachable = Vec::new();
    for (i, (p, guarded)) in pats.iter().zip(arms.iter().map(|a| a.1)).enumerate() {
        if cx.useful(&rows, std::slice::from_ref(p), &[scrut]).is_none() {
            unreachable.push(i);
        }
        if !guarded {
            rows.push(vec![p.clone()]);
        }
    }
    let missing = cx.useful(&rows, &[P::Wild], &[scrut]).map(|w| cx.render(&w[0], scrut));
    if cx.budget == 0 {
        return Report { missing: None, unreachable: Vec::new() };
    }
    Report { missing, unreachable }
}

impl Cx<'_> {
    fn lower(&self, p: PatId) -> P {
        let pat = self.ast.pats[p];
        match pat.kind {
            PatKind::Wild | PatKind::Bind(_) | PatKind::Error => P::Wild,
            PatKind::Lit(e) => match self.ast.exprs[e].kind {
                ExprKind::Int(v) => P::Ctor(Ctor::Int(v as i128), vec![]),
                ExprKind::Unary { op: UnOp::Neg, expr } => match self.ast.exprs[expr].kind {
                    ExprKind::Int(v) => P::Ctor(Ctor::Int(-(v as i128)), vec![]),
                    _ => P::Ctor(Ctor::Opaque, vec![]),
                },
                ExprKind::Bool(b) => P::Ctor(Ctor::Bool(b), vec![]),
                ExprKind::Str(s) => P::Ctor(Ctor::Str(s), vec![]),
                ExprKind::Char(c) => {
                    let t = self.tables.pat_ty[p.0 as usize];
                    if matches!(self.tys.kind(t), TyKind::Int(..)) {
                        P::Ctor(Ctor::Int(c as i128), vec![])
                    } else {
                        P::Ctor(Ctor::Char(c), vec![])
                    }
                }
                _ => P::Ctor(Ctor::Opaque, vec![]),
            },
            PatKind::Variant { args, .. } => match self.tables.pat_variant.get(&p) {
                Some(&(_, vi)) => {
                    let subs = self.ast.pat_lists.get(args).iter().map(|&a| self.lower(a)).collect();
                    P::Ctor(Ctor::Variant(vi), subs)
                }
                None => P::Ctor(Ctor::Opaque, vec![]),
            },
            PatKind::Tuple(list) => {
                P::Ctor(Ctor::Tuple, self.ast.pat_lists.get(list).iter().map(|&a| self.lower(a)).collect())
            }
            PatKind::Or(list) => P::Or(self.ast.pat_lists.get(list).iter().map(|&a| self.lower(a)).collect()),
        }
    }

    /// All constructors of a type, if finitely many.
    fn all_ctors(&self, t: Ty) -> Option<Vec<Ctor>> {
        match self.tys.kind(t) {
            TyKind::Bool => Some(vec![Ctor::Bool(false), Ctor::Bool(true)]),
            TyKind::Tuple(_) => Some(vec![Ctor::Tuple]),
            TyKind::Adt(a, _) if self.defs.adts[*a].is_enum() => {
                Some((0..self.defs.adts[*a].variants().len() as u32).map(Ctor::Variant).collect())
            }
            _ => None,
        }
    }

    fn field_tys(&mut self, c: &Ctor, t: Ty) -> Vec<Ty> {
        match (c, self.tys.kind(t).clone()) {
            (Ctor::Tuple, TyKind::Tuple(ts)) => ts.to_vec(),
            (Ctor::Variant(vi), TyKind::Adt(a, args)) => {
                let fs: Vec<Ty> = self.defs.adts[a].variants()[*vi as usize].fields.iter().map(|f| f.ty).collect();
                fs.into_iter().map(|f| self.tys.subst(f, &args)).collect()
            }
            _ => vec![],
        }
    }

    fn arity(&mut self, c: &Ctor, t: Ty) -> usize {
        self.field_tys(c, t).len()
    }

    /// Rows whose first column matches constructor `c`, with that column replaced by the
    /// constructor's fields.
    fn specialize(&mut self, rows: &[Vec<P>], c: &Ctor, arity: usize) -> Vec<Vec<P>> {
        let mut out = Vec::new();
        for row in rows {
            self.specialize_row(row, c, arity, &mut out);
        }
        out
    }

    fn specialize_row(&self, row: &[P], c: &Ctor, arity: usize, out: &mut Vec<Vec<P>>) {
        match &row[0] {
            P::Wild => {
                let mut r = vec![P::Wild; arity];
                r.extend_from_slice(&row[1..]);
                out.push(r);
            }
            P::Ctor(c2, args) => {
                if c2 == c {
                    let mut r = args.clone();
                    r.resize(arity, P::Wild);
                    r.extend_from_slice(&row[1..]);
                    out.push(r);
                }
            }
            P::Or(alts) => {
                for a in alts {
                    let mut r = vec![a.clone()];
                    r.extend_from_slice(&row[1..]);
                    self.specialize_row(&r, c, arity, out);
                }
            }
        }
    }

    fn head_ctors(&self, rows: &[Vec<P>], out: &mut Vec<Ctor>) {
        for row in rows {
            Self::collect_ctors(&row[0], out);
        }
    }

    fn collect_ctors(p: &P, out: &mut Vec<Ctor>) {
        match p {
            P::Wild => {}
            P::Ctor(c, _) => {
                if !out.contains(c) {
                    out.push(c.clone())
                }
            }
            P::Or(alts) => {
                for a in alts {
                    Self::collect_ctors(a, out);
                }
            }
        }
    }

    fn default_rows(&self, rows: &[Vec<P>]) -> Vec<Vec<P>> {
        let mut out = Vec::new();
        for row in rows {
            Self::default_row(row, &mut out);
        }
        out
    }

    fn default_row(row: &[P], out: &mut Vec<Vec<P>>) {
        match &row[0] {
            P::Wild => out.push(row[1..].to_vec()),
            P::Ctor(..) => {}
            P::Or(alts) => {
                for a in alts {
                    let mut r = vec![a.clone()];
                    r.extend_from_slice(&row[1..]);
                    Self::default_row(&r, out);
                }
            }
        }
    }

    /// Is `q` useful with respect to `rows`? Returns a witness vector if so.
    fn useful(&mut self, rows: &[Vec<P>], q: &[P], tys: &[Ty]) -> Option<Vec<P>> {
        if self.budget == 0 {
            return None;
        }
        self.budget -= 1;
        if q.is_empty() {
            return if rows.is_empty() { Some(vec![]) } else { None };
        }
        let t = tys[0];
        match &q[0] {
            P::Or(alts) => {
                for a in alts {
                    let mut q2 = vec![a.clone()];
                    q2.extend_from_slice(&q[1..]);
                    if let Some(w) = self.useful(rows, &q2, tys) {
                        return Some(w);
                    }
                }
                None
            }
            P::Ctor(c, args) => {
                let ftys = self.field_tys(c, t);
                let arity = ftys.len().max(args.len());
                let spec = self.specialize(rows, c, arity);
                let mut q2 = args.clone();
                q2.resize(arity, P::Wild);
                q2.extend_from_slice(&q[1..]);
                let mut t2 = ftys;
                t2.resize(arity, t);
                t2.extend_from_slice(&tys[1..]);
                let w = self.useful(&spec, &q2, &t2)?;
                Some(rebuild(c.clone(), arity, w))
            }
            P::Wild => {
                let mut used = Vec::new();
                self.head_ctors(rows, &mut used);
                let all = self.all_ctors(t);
                if let Some(all) = &all {
                    let complete = all.iter().all(|c| used.contains(c));
                    if complete {
                        for c in all.clone() {
                            let ftys = self.field_tys(&c, t);
                            let arity = ftys.len();
                            let spec = self.specialize(rows, &c, arity);
                            let mut q2 = vec![P::Wild; arity];
                            q2.extend_from_slice(&q[1..]);
                            let mut t2 = ftys;
                            t2.extend_from_slice(&tys[1..]);
                            if let Some(w) = self.useful(&spec, &q2, &t2) {
                                return Some(rebuild(c, arity, w));
                            }
                        }
                        return None;
                    }
                }
                let def = self.default_rows(rows);
                let w = self.useful(&def, &q[1..], &tys[1..])?;
                let head = match &all {
                    Some(all) => {
                        let missing = all.iter().find(|c| !used.contains(c)).cloned().unwrap();
                        let n = self.arity(&missing, t);
                        P::Ctor(missing, vec![P::Wild; n])
                    }
                    None => P::Wild,
                };
                let mut out = vec![head];
                out.extend(w);
                Some(out)
            }
        }
    }

    fn render(&mut self, p: &P, t: Ty) -> String {
        match p {
            P::Wild => "_".to_string(),
            P::Or(alts) => alts.iter().map(|a| self.render(a, t)).collect::<Vec<_>>().join(" | "),
            P::Ctor(c, args) => {
                let ftys = self.field_tys(c, t);
                let sub: Vec<String> = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| {
                        let ft = ftys.get(i).copied().unwrap_or(t);
                        self.render(a, ft)
                    })
                    .collect();
                match c {
                    Ctor::Variant(vi) => {
                        let name = match self.tys.kind(t) {
                            TyKind::Adt(a, _) => self.defs.adts[*a].variants()[*vi as usize].name.to_string(),
                            _ => "?".into(),
                        };
                        if sub.is_empty() { name } else { format!("{name}({})", sub.join(", ")) }
                    }
                    Ctor::Tuple => format!("({})", sub.join(", ")),
                    Ctor::Bool(b) => b.to_string(),
                    Ctor::Int(v) => v.to_string(),
                    Ctor::Str(s) => format!("{:?}", s.as_str()),
                    Ctor::Char(c) => format!("{:?}", char::from_u32(*c).unwrap_or('?')),
                    Ctor::Opaque => "_".into(),
                }
            }
        }
    }
}

fn rebuild(c: Ctor, arity: usize, mut w: Vec<P>) -> Vec<P> {
    let rest = w.split_off(arity.min(w.len()));
    let mut out = vec![P::Ctor(c, w)];
    out.extend(rest);
    out
}
