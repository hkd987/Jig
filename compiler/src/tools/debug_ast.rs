//! `jig debug ast FILE`: the parser's output as S-expressions, one top-level item per line,
//! then the errors. Every node prints its span (`@lo:hi`); `_` is an absent option. The
//! self-hosted parser (`selfhost/`) prints the same text.

use crate::diag::{FileId, Span};
use crate::syntax::ast::*;
use crate::syntax::parser;
use crate::util::intern::LocalInterner;
use crate::util::List;

/// Strings print in quotes; printable ASCII stays, everything else is `\u{hex}`.
fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if (' '..='~').contains(&c) && c != '"' && c != '\\' {
            out.push(c);
        } else {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        }
    }
    out.push('"');
    out
}

struct P<'a> {
    ast: &'a Ast,
    src: &'a str,
    out: String,
}

fn sp(s: Span) -> String {
    format!("@{}:{}", s.lo, s.hi)
}

fn name(n: Name) -> String {
    format!("{}{}", n.sym.as_str(), sp(n.span))
}

impl P<'_> {
    fn list<T: Copy>(&mut self, xs: &[T], mut f: impl FnMut(&mut Self, T)) {
        self.out.push('[');
        for (i, &x) in xs.iter().enumerate() {
            if i > 0 {
                self.out.push(' ');
            }
            f(self, x);
        }
        self.out.push(']');
    }

    fn opt<T: Copy>(&mut self, x: Option<T>, f: impl FnOnce(&mut Self, T)) {
        match x {
            Some(v) => f(self, v),
            None => self.out.push('_'),
        }
    }

    fn w(&mut self, s: &str) {
        self.out.push_str(s);
    }

    fn exprs(&mut self, l: List<ExprId>) {
        let xs = self.ast.expr_lists.get(l).to_vec();
        self.list(&xs, |p, e| p.expr(e));
    }

    fn names(&mut self, l: List<Name>) {
        let xs = self.ast.names.get(l).to_vec();
        self.list(&xs, |p, n| p.w(&name(n)));
    }

    fn generics(&mut self, l: List<GenericParam>) {
        let xs = self.ast.generics.get(l).to_vec();
        self.list(&xs, |p, g| {
            p.w(&format!("(gp {} ", name(g.name)));
            let bs = p.ast.type_lists.get(g.bounds).to_vec();
            p.list(&bs, |p, t| p.ty(t));
            p.w(")");
        });
    }

    fn params(&mut self, l: List<Param>) {
        let xs = self.ast.params.get(l).to_vec();
        self.list(&xs, |p, prm| {
            p.w(&format!("(param {} ", name(prm.name)));
            p.opt(prm.ty, |p, t| p.ty(t));
            p.w(if prm.mut_ref { " mut " } else { " val " });
            p.opt(prm.refine, |p, e| p.expr(e));
            p.w(&format!(" {})", sp(prm.span)));
        });
    }

    fn fields(&mut self, l: List<FieldDecl>) {
        let xs = self.ast.fields.get(l).to_vec();
        self.list(&xs, |p, f| {
            p.w(&format!("(field {} ", name(f.name)));
            p.ty(f.ty);
            p.w(if f.is_pub { " pub " } else { " priv " });
            p.opt(f.refine, |p, e| p.expr(e));
            p.w(&format!(" {})", sp(f.span)));
        });
    }

    fn item(&mut self, id: ItemId) {
        let it = self.ast.items[id];
        let vis = if it.is_pub { "pub" } else { "priv" };
        let head = format!("{} {} {}", name(it.name), sp(it.span), vis);
        match it.kind {
            ItemKind::Import { path, path_span } => {
                self.w(&format!("(import {head} {} {})", quote(path.as_str()), sp(path_span)));
            }
            ItemKind::Fn(d) => {
                self.w(&format!("(fn {head} "));
                self.opt(d.recv, |p, r| {
                    p.w(&format!("(recv {} ", name(r.ty)));
                    p.generics(r.generics);
                    p.w(")");
                });
                self.w(" ");
                self.opt(d.self_param, |p, s| {
                    p.w(&format!("(self {} {})", if s.mutable { "mut" } else { "val" }, sp(s.span)))
                });
                self.w(" ");
                self.generics(d.generics);
                self.w(" ");
                self.params(d.params);
                self.w(" ");
                self.opt(d.ret, |p, t| p.ty(t));
                self.w(" ");
                self.exprs(d.requires);
                self.w(" ");
                self.exprs(d.ensures);
                self.w(" ");
                self.opt(d.effects, |p, l| p.names(l));
                self.w(&format!(" {} ", sp(d.effects_span)));
                self.opt(d.body, |p, e| p.expr(e));
                self.w(" ");
                self.opt(d.step, |p, (s, span)| p.w(&format!("(step {} {})", quote(s.as_str()), sp(span))));
                self.w(&format!(" {})", sp(d.sig_span)));
            }
            ItemKind::Struct { generics, fields, invariants } => {
                self.w(&format!("(struct {head} "));
                self.generics(generics);
                self.w(" ");
                self.fields(fields);
                self.w(" ");
                self.exprs(invariants);
                self.w(")");
            }
            ItemKind::Enum { generics, variants } => {
                self.w(&format!("(enum {head} "));
                self.generics(generics);
                self.w(" ");
                let vs = self.ast.variants.get(variants).to_vec();
                self.list(&vs, |p, v| {
                    p.w(&format!("(variant {} ", name(v.name)));
                    p.fields(v.fields);
                    p.w(&format!(" {} {})", if v.has_parens { "parens" } else { "bare" }, sp(v.span)));
                });
                self.w(")");
            }
            ItemKind::Interface { generics, methods } => {
                self.w(&format!("(interface {head} "));
                self.generics(generics);
                self.w(" ");
                let ms = self.ast.item_lists.get(methods).to_vec();
                self.list(&ms, |p, m| p.item(m));
                self.w(")");
            }
            ItemKind::TypeAlias { generics, ty } => {
                self.w(&format!("(type {head} "));
                self.generics(generics);
                self.w(" ");
                self.ty(ty);
                self.w(")");
            }
            ItemKind::Const { ty, value } => {
                self.w(&format!("(const {head} "));
                self.ty(ty);
                self.w(" ");
                self.expr(value);
                self.w(")");
            }
            ItemKind::Unit => self.w(&format!("(unit {head})")),
            ItemKind::Test { label, params, requires, body } => {
                self.w(&format!("(test {head} {} ", quote(label.as_str())));
                self.params(params);
                self.w(" ");
                self.exprs(requires);
                self.w(" ");
                self.expr(body);
                self.w(")");
            }
        }
    }

    fn ty(&mut self, id: TypeExprId) {
        let t = self.ast.types[id];
        match t.kind {
            TypeExprKind::Named { pkg, name: n, args, unit } => {
                self.w("(named ");
                self.opt(pkg, |p, k| p.w(&name(k)));
                self.w(&format!(" {} ", name(n)));
                let xs = self.ast.type_lists.get(args).to_vec();
                self.list(&xs, |p, a| p.ty(a));
                self.w(" ");
                self.opt(unit, |p, u| {
                    let fs = p.ast.unit_factors.get(u).to_vec();
                    p.list(&fs, |p, f| p.w(&format!("(uf {} {})", name(f.name), f.exp)));
                });
            }
            TypeExprKind::Tuple(l) => {
                self.w("(tuple-type ");
                let xs = self.ast.type_lists.get(l).to_vec();
                self.list(&xs, |p, a| p.ty(a));
            }
            TypeExprKind::Fn { params, ret, effects } => {
                self.w("(fn-type ");
                let xs = self.ast.type_lists.get(params).to_vec();
                self.list(&xs, |p, a| p.ty(a));
                self.w(" ");
                self.opt(ret, |p, r| p.ty(r));
                self.w(" ");
                self.opt(effects, |p, l| p.names(l));
            }
            TypeExprKind::Refined { base, pred } => {
                self.w("(refined ");
                self.ty(base);
                self.w(" ");
                self.expr(pred);
            }
            TypeExprKind::Error => self.w("(type-error"),
        }
        self.w(&format!(" {})", sp(t.span)));
    }

    fn pat(&mut self, id: PatId) {
        let pt = self.ast.pats[id];
        match pt.kind {
            PatKind::Wild => self.w("(wild"),
            PatKind::Bind(s) => self.w(&format!("(bind {}", s.as_str())),
            PatKind::Lit(e) => {
                self.w("(lit ");
                self.expr(e);
            }
            PatKind::Variant { name: n, args, has_parens } => {
                self.w(&format!("(variant-pat {} ", name(n)));
                let xs = self.ast.pat_lists.get(args).to_vec();
                self.list(&xs, |p, a| p.pat(a));
                self.w(if has_parens { " parens" } else { " bare" });
            }
            PatKind::Tuple(l) => {
                self.w("(tuple-pat ");
                let xs = self.ast.pat_lists.get(l).to_vec();
                self.list(&xs, |p, a| p.pat(a));
            }
            PatKind::Or(l) => {
                self.w("(or ");
                let xs = self.ast.pat_lists.get(l).to_vec();
                self.list(&xs, |p, a| p.pat(a));
            }
            PatKind::Error => self.w("(pat-error"),
        }
        self.w(&format!(" {})", sp(pt.span)));
    }

    fn stmt(&mut self, id: StmtId) {
        let st = self.ast.stmts[id];
        match st.kind {
            StmtKind::Let { pat, ty, init, mutable } => {
                self.w(if mutable { "(var " } else { "(let " });
                self.pat(pat);
                self.w(" ");
                self.opt(ty, |p, t| p.ty(t));
                self.w(" ");
                self.expr(init);
            }
            StmtKind::Assign { target, op, value } => {
                self.w("(assign ");
                self.expr(target);
                self.w(&format!(" {}= ", op.map(|o| o.as_str()).unwrap_or("")));
                self.expr(value);
            }
            StmtKind::Expr(e) => {
                self.w("(expr ");
                self.expr(e);
            }
            StmtKind::Return(v) => {
                self.w("(return ");
                self.opt(v, |p, e| p.expr(e));
            }
            StmtKind::Break => self.w("(break"),
            StmtKind::Continue => self.w("(continue"),
            StmtKind::For { head, invariants, body } => {
                self.w("(for ");
                match head {
                    ForHead::Infinite => self.w("(forever)"),
                    ForHead::Cond(c) => {
                        self.w("(while ");
                        self.expr(c);
                        self.w(")");
                    }
                    ForHead::In { pat, iter } => {
                        self.w("(in ");
                        self.pat(pat);
                        self.w(" ");
                        self.expr(iter);
                        self.w(")");
                    }
                }
                self.w(" ");
                self.exprs(invariants);
                self.w(" ");
                self.expr(body);
            }
        }
        self.w(&format!(" {})", sp(st.span)));
    }

    fn expr(&mut self, id: ExprId) {
        let e = self.ast.exprs[id];
        match e.kind {
            ExprKind::Int(v) => self.w(&format!("(int {v}")),
            ExprKind::Float(_) => {
                let text = &self.src[e.span.lo as usize..e.span.hi as usize];
                self.w(&format!("(float {}", quote(text)));
            }
            ExprKind::Bool(b) => self.w(&format!("(bool {b}")),
            ExprKind::Str(s) => self.w(&format!("(str {}", quote(s.as_str()))),
            ExprKind::Char(c) => self.w(&format!("(char {c}")),
            ExprKind::Name(s) => self.w(&format!("(name {}", s.as_str())),
            ExprKind::Paren(x) => {
                self.w("(paren ");
                self.expr(x);
            }
            ExprKind::Tuple(l) => {
                self.w("(tuple ");
                self.exprs(l);
            }
            ExprKind::List(l) => {
                self.w("(list ");
                self.exprs(l);
            }
            ExprKind::Field { base, name: n } => {
                self.w("(field ");
                self.expr(base);
                self.w(&format!(" {}", name(n)));
            }
            ExprKind::TupleField { base, index, index_span } => {
                self.w("(tuple-field ");
                self.expr(base);
                self.w(&format!(" {index} {}", sp(index_span)));
            }
            ExprKind::Call { callee, args } => {
                self.w("(call ");
                self.expr(callee);
                self.w(" ");
                self.exprs(args);
            }
            ExprKind::MutArg(x) => {
                self.w("(mut-arg ");
                self.expr(x);
            }
            ExprKind::Index { base, index } => {
                self.w("(index ");
                self.expr(base);
                self.w(" ");
                self.expr(index);
            }
            ExprKind::Range { lo, hi } => {
                self.w("(range ");
                self.opt(lo, |p, x| p.expr(x));
                self.w(" ");
                self.opt(hi, |p, x| p.expr(x));
            }
            ExprKind::Unary { op, expr } => {
                self.w(&format!("(unary {} ", op.as_str()));
                self.expr(expr);
            }
            ExprKind::Binary { op, lhs, rhs } => {
                self.w(&format!("(binary {} ", op.as_str()));
                self.expr(lhs);
                self.w(" ");
                self.expr(rhs);
            }
            ExprKind::Cast { expr, ty } => {
                self.w("(cast ");
                self.expr(expr);
                self.w(" ");
                self.ty(ty);
            }
            ExprKind::Try(x) => {
                self.w("(try ");
                self.expr(x);
            }
            ExprKind::If { cond, then, els } => {
                self.w("(if ");
                self.expr(cond);
                self.w(" ");
                self.expr(then);
                self.w(" ");
                self.opt(els, |p, x| p.expr(x));
            }
            ExprKind::Match { scrut, arms } => {
                self.w("(match ");
                self.expr(scrut);
                self.w(" ");
                let xs = self.ast.arms.get(arms).to_vec();
                self.list(&xs, |p, a| {
                    p.w("(arm ");
                    p.pat(a.pat);
                    p.w(" ");
                    p.opt(a.guard, |p, g| p.expr(g));
                    p.w(" ");
                    p.expr(a.body);
                    p.w(&format!(" {})", sp(a.span)));
                });
            }
            ExprKind::Block(l) => {
                self.w("(block ");
                let xs = self.ast.stmt_lists.get(l).to_vec();
                self.list(&xs, |p, s| p.stmt(s));
            }
            ExprKind::StructLit { ty, fields } => {
                self.w("(struct-lit ");
                self.ty(ty);
                self.w(" ");
                let xs = self.ast.field_inits.get(fields).to_vec();
                self.list(&xs, |p, f| {
                    p.w(&format!("(init {} ", name(f.name)));
                    p.expr(f.value);
                    p.w(")");
                });
            }
            ExprKind::Closure { params, ret, body } => {
                self.w("(closure ");
                self.params(params);
                self.w(" ");
                self.opt(ret, |p, t| p.ty(t));
                self.w(" ");
                self.expr(body);
            }
            ExprKind::TypeArgs { base, args } => {
                self.w("(type-args ");
                self.expr(base);
                self.w(" ");
                let xs = self.ast.type_lists.get(args).to_vec();
                self.list(&xs, |p, t| p.ty(t));
            }
            ExprKind::Quant { forall, var, lo, hi, body } => {
                self.w(&format!("({} {} ", if forall { "forall" } else { "exists" }, name(var)));
                self.expr(lo);
                self.w(" ");
                self.expr(hi);
                self.w(" ");
                self.expr(body);
            }
            ExprKind::Error => self.w("(expr-error"),
        }
        self.w(&format!(" {})", sp(e.span)));
    }
}

/// The dump of `src` parsed as one file at offset 0.
pub fn ast(src: &str) -> String {
    let mut ast = Ast::default();
    let mut interner = LocalInterner::default();
    let out = parser::parse_file(&mut ast, FileId(0), src, 0, &mut interner);
    let mut p = P { ast: &ast, src, out: String::new() };
    let items: Vec<ItemId> = ast.top_items().collect();
    for id in items {
        p.item(id);
        p.out.push('\n');
    }
    for d in &out.diags {
        p.out.push_str(&format!("error {} {} {} {}\n", d.code.name(), d.span.lo, d.span.hi, d.message));
    }
    p.out
}
