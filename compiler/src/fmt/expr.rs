//! Expressions, statements, blocks, types and patterns.

use super::{Blanks, Key, LineElem, Printer, FLAT_LIMIT};
use crate::syntax::ast::*;
use crate::syntax::lexer;
use crate::util::{List, Symbol};

/// The width in columns of a single-line string.
pub(super) fn width(s: &str) -> usize {
    s.chars().count()
}

/// The column after printing `s` from column `col`.
pub(super) fn end_col(col: usize, s: &str) -> usize {
    match s.rfind('\n') {
        Some(i) => width(&s[i + 1..]),
        None => col + width(s),
    }
}

pub(super) fn indent(s: &mut String, n: usize) {
    s.extend(std::iter::repeat_n(' ', n));
}

pub(super) fn newline(s: &mut String, n: usize) {
    s.push('\n');
    indent(s, n);
}

pub(super) fn push_str_lit(o: &mut String, s: Symbol) {
    o.push('"');
    o.push_str(&lexer::escape(s.as_str(), '"'));
    o.push('"');
}

fn push_char_lit(o: &mut String, c: u32) {
    let ch = char::from_u32(c).unwrap_or('\u{fffd}');
    o.push('\'');
    o.push_str(&lexer::escape(ch.encode_utf8(&mut [0; 4]), '\''));
    o.push('\'');
}

/// An element of a parenthesized or bracketed list.
#[derive(Clone, Copy)]
pub(super) enum Elem {
    Expr(ExprId),
    Param(Param),
    SelfParam(SelfParam),
}

/// One postfix operation of a chain `root.a(x).b[i]?`.
#[derive(Clone, Copy)]
enum Op {
    Field(Name),
    TupleField(u32),
    /// The call expression and its arguments.
    Call(ExprId, List<ExprId>),
    Index(ExprId),
    Try,
    TypeArgs(List<TypeExprId>),
}

/// Indices of the ops that start a method segment: a `.name` followed by a call.
fn segment_starts(ops: &[Op]) -> Vec<usize> {
    let mut v = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        if let Op::Field(_) = op {
            let mut j = i + 1;
            if matches!(ops.get(j), Some(Op::TypeArgs(_))) {
                j += 1;
            }
            if matches!(ops.get(j), Some(Op::Call(..))) {
                v.push(i);
            }
        }
    }
    v
}

impl Printer<'_> {
    // ---------- flat layouts ----------

    /// The single-line layout of `e`, or `None` if it has none (a multi-statement block, a
    /// comment to place) or it is far too long to fit anyway.
    pub(super) fn flat(&self, e: ExprId) -> Option<String> {
        let mut o = String::new();
        self.flat_into(e, &mut o).then_some(o)
    }

    fn flat_into(&self, e: ExprId, o: &mut String) -> bool {
        if o.len() > FLAT_LIMIT {
            return false;
        }
        let ast = self.ast;
        let ex = ast.expr(e);
        match ex.kind {
            // Numbers print as written: `0xff`, `1_000`, `1e-9`.
            ExprKind::Int(_) | ExprKind::Float(_) | ExprKind::Error => o.push_str(self.span_text(ex.span)),
            ExprKind::Bool(b) => o.push_str(if b { "true" } else { "false" }),
            ExprKind::Str(s) => push_str_lit(o, s),
            ExprKind::Char(c) => push_char_lit(o, c),
            ExprKind::Name(s) => o.push_str(s.as_str()),
            ExprKind::Paren(x) => {
                o.push('(');
                if !self.flat_into(x, o) {
                    return false;
                }
                o.push(')');
            }
            ExprKind::Tuple(l) => {
                let xs = ast.expr_lists.get(l);
                if self.has(Key::Expr(e)) {
                    return false;
                }
                o.push('(');
                if !self.flat_list_into(xs, o) {
                    return false;
                }
                if xs.len() == 1 {
                    o.push(',');
                }
                o.push(')');
            }
            ExprKind::List(l) => {
                if self.has(Key::Expr(e)) {
                    return false;
                }
                o.push('[');
                if !self.flat_list_into(ast.expr_lists.get(l), o) {
                    return false;
                }
                o.push(']');
            }
            ExprKind::Field { base, name } => {
                if !self.flat_into(base, o) {
                    return false;
                }
                o.push('.');
                o.push_str(name.sym.as_str());
            }
            ExprKind::TupleField { base, index, .. } => {
                if !self.flat_into(base, o) {
                    return false;
                }
                o.push('.');
                o.push_str(&index.to_string());
            }
            ExprKind::Call { callee, args } => {
                if self.has(Key::Expr(e)) || !self.flat_into(callee, o) {
                    return false;
                }
                o.push('(');
                if !self.flat_list_into(ast.expr_lists.get(args), o) {
                    return false;
                }
                o.push(')');
            }
            ExprKind::MutArg(x) => {
                o.push_str("&mut ");
                return self.flat_into(x, o);
            }
            ExprKind::Index { base, index } => {
                if !self.flat_into(base, o) {
                    return false;
                }
                o.push('[');
                if !self.flat_into(index, o) {
                    return false;
                }
                o.push(']');
            }
            ExprKind::Range { lo, hi } => {
                if let Some(l) = lo {
                    if !self.flat_into(l, o) {
                        return false;
                    }
                }
                o.push_str("..");
                if let Some(h) = hi {
                    return self.flat_into(h, o);
                }
            }
            ExprKind::Unary { op, expr } => {
                o.push_str(op.as_str());
                return self.flat_into(expr, o);
            }
            ExprKind::Binary { .. } => return self.flat_binary_into(e, o),
            ExprKind::Cast { expr, ty } => {
                if !self.flat_into(expr, o) {
                    return false;
                }
                o.push_str(" as ");
                self.ty_into(ty, o);
            }
            ExprKind::Try(x) => {
                if !self.flat_into(x, o) {
                    return false;
                }
                o.push('?');
            }
            ExprKind::If { cond, then, els } => {
                o.push_str("if ");
                if !self.flat_into(cond, o) {
                    return false;
                }
                o.push(' ');
                if !self.flat_block_into(then, o) {
                    return false;
                }
                if let Some(x) = els {
                    o.push_str(" else ");
                    let ok = match ast.expr(x).kind {
                        ExprKind::If { .. } => self.flat_into(x, o),
                        _ => self.flat_block_into(x, o),
                    };
                    return ok;
                }
            }
            ExprKind::Match { scrut, arms } => {
                // Arms always print one per line.
                if !arms.is_empty() || self.has(Key::Expr(e)) {
                    return false;
                }
                o.push_str("match ");
                if !self.flat_into(scrut, o) {
                    return false;
                }
                o.push_str(" {}");
            }
            ExprKind::Block(_) => {
                // A block of statements is multi-line; inline `{ e }` is for if/else, arm and
                // closure bodies only.
                if !matches!(self.inline_body(e), Some(None)) {
                    return false;
                }
                o.push_str("{}");
            }
            ExprKind::StructLit { ty, fields } => {
                if self.has(Key::Expr(e)) {
                    return false;
                }
                self.ty_into(ty, o);
                let fs = ast.field_inits.get(fields);
                if fs.is_empty() {
                    o.push_str(" {}");
                    return true;
                }
                o.push_str(" { ");
                for (i, fi) in fs.iter().enumerate() {
                    if i > 0 {
                        o.push_str(", ");
                    }
                    o.push_str(fi.name.sym.as_str());
                    o.push_str(": ");
                    if !self.flat_into(fi.value, o) {
                        return false;
                    }
                }
                o.push_str(" }");
            }
            ExprKind::Closure { params, ret, body } => {
                if self.has(Key::Expr(e)) {
                    return false;
                }
                o.push_str("fn(");
                if !self.flat_params_into(params, o) {
                    return false;
                }
                o.push(')');
                if let Some(r) = ret {
                    o.push_str(" -> ");
                    self.ty_into(r, o);
                }
                o.push(' ');
                return self.flat_block_into(body, o);
            }
            ExprKind::TypeArgs { base, args } => {
                if !self.flat_into(base, o) {
                    return false;
                }
                self.type_args_into(args, o);
            }
            ExprKind::Quant { forall, var, lo, hi, body } => {
                o.push_str(if forall { "forall " } else { "exists " });
                o.push_str(var.sym.as_str());
                o.push_str(" in ");
                if !self.flat_into(lo, o) {
                    return false;
                }
                o.push_str("..");
                if !self.flat_into(hi, o) {
                    return false;
                }
                o.push_str(": ");
                return self.flat_into(body, o);
            }
        }
        true
    }

    /// A binary expression, iterating down its left spine.
    fn flat_binary_into(&self, e: ExprId, o: &mut String) -> bool {
        let mut spine = Vec::new();
        let mut cur = e;
        while let ExprKind::Binary { op, lhs, rhs } = self.ast.expr(cur).kind {
            spine.push((op, rhs));
            cur = lhs;
        }
        if !self.flat_into(cur, o) {
            return false;
        }
        for &(op, rhs) in spine.iter().rev() {
            o.push(' ');
            o.push_str(op.as_str());
            o.push(' ');
            if !self.flat_into(rhs, o) {
                return false;
            }
        }
        true
    }

    fn flat_list_into(&self, xs: &[ExprId], o: &mut String) -> bool {
        for (i, &x) in xs.iter().enumerate() {
            if i > 0 {
                o.push_str(", ");
            }
            if !self.flat_into(x, o) {
                return false;
            }
        }
        true
    }

    /// The conditions of a clause, comma-separated on one line.
    pub(super) fn flat_joined(&self, xs: &[ExprId]) -> Option<String> {
        let mut o = String::new();
        self.flat_list_into(xs, &mut o).then_some(o)
    }

    fn flat_params_into(&self, params: List<Param>, o: &mut String) -> bool {
        for (i, p) in self.ast.params.get(params).iter().enumerate() {
            if i > 0 {
                o.push_str(", ");
            }
            if !self.elem_flat_into(Elem::Param(*p), o) {
                return false;
            }
        }
        true
    }

    /// `Some(None)`: the block is empty; `Some(Some(e))`: it holds just the expression `e`.
    /// `None`: it must print one statement per line.
    fn inline_body(&self, b: ExprId) -> Option<Option<ExprId>> {
        let ExprKind::Block(stmts) = self.ast.expr(b).kind else { return None };
        if self.has(Key::Expr(b)) {
            return None;
        }
        match self.ast.stmt_lists.get(stmts) {
            [] => Some(None),
            [s] => match self.ast.stmt(*s).kind {
                StmtKind::Expr(e) => Some(Some(e)),
                _ => None,
            },
            _ => None,
        }
    }

    /// `{}` or `{ e }`.
    fn flat_block_into(&self, b: ExprId, o: &mut String) -> bool {
        match self.inline_body(b) {
            Some(None) => o.push_str("{}"),
            Some(Some(e)) => {
                o.push_str("{ ");
                if !self.flat_into(e, o) {
                    return false;
                }
                o.push_str(" }");
            }
            None => return false,
        }
        true
    }

    // ---------- broken layouts ----------

    /// The layout of `e`: flat if it fits, broken otherwise.
    pub(super) fn render(&self, e: ExprId, ind: usize, col: usize, trail: usize) -> String {
        if let Some(f) = self.flat(e) {
            if col + width(&f) + trail <= self.width {
                return f;
            }
        }
        self.broken(e, ind, col, trail)
    }

    fn broken(&self, e: ExprId, ind: usize, col: usize, trail: usize) -> String {
        let ast = self.ast;
        match ast.expr(e).kind {
            ExprKind::Paren(x) => format!("({})", self.render(x, ind, col + 1, trail + 1)),
            ExprKind::Tuple(l) => {
                let xs: Vec<Elem> = ast.expr_lists.get(l).iter().map(|&x| Elem::Expr(x)).collect();
                self.delimited("(", ")", Key::Expr(e), &xs, ind, col, trail, true)
            }
            ExprKind::List(l) => {
                let xs: Vec<Elem> = ast.expr_lists.get(l).iter().map(|&x| Elem::Expr(x)).collect();
                self.delimited("[", "]", Key::Expr(e), &xs, ind, col, trail, false)
            }
            ExprKind::Field { .. }
            | ExprKind::TupleField { .. }
            | ExprKind::Call { .. }
            | ExprKind::Index { .. }
            | ExprKind::Try(_)
            | ExprKind::TypeArgs { .. } => self.postfix(e, ind, col, trail),
            ExprKind::MutArg(x) => format!("&mut {}", self.render(x, ind, col + 5, trail)),
            ExprKind::Range { lo, hi } => {
                let mut s = String::new();
                if let Some(l) = lo {
                    s += &self.render(l, ind, col, 2);
                }
                s.push_str("..");
                if let Some(h) = hi {
                    let c = end_col(col, &s);
                    s += &self.render(h, ind, c, trail);
                }
                s
            }
            ExprKind::Unary { op, expr } => {
                format!("{}{}", op.as_str(), self.render(expr, ind, col + op.as_str().len(), trail))
            }
            ExprKind::Binary { .. } => self.binary(e, ind, col, trail),
            ExprKind::Cast { expr, ty } => {
                let t = format!(" as {}", self.ty(ty));
                let mut s = self.render(expr, ind, col, trail + width(&t));
                s += &t;
                s
            }
            ExprKind::If { .. } => self.if_broken(e, ind, col),
            ExprKind::Match { .. } => self.match_expr(e, ind, col),
            ExprKind::Block(_) => self.block(e, ind),
            ExprKind::StructLit { ty, fields } => {
                let mut s = self.ty(ty);
                s.push(' ');
                let fs = ast.field_inits.get(fields);
                let elems: Vec<LineElem> = fs
                    .iter()
                    .map(|fi| {
                        let mut text = format!("{}: ", fi.name.sym);
                        let c = ind + 2 + width(&text);
                        text += &self.render(fi.value, ind + 2, c, 0);
                        LineElem { lo: fi.name.span.lo, hi: ast.expr(fi.value).span.hi, text }
                    })
                    .collect();
                s += &self.braced(Key::Expr(e), &elems, ind);
                s
            }
            ExprKind::Closure { params, ret, body } => {
                let r = ret.map(|t| format!(" -> {}", self.ty(t))).unwrap_or_default();
                let elems: Vec<Elem> = ast.params.get(params).iter().map(|&p| Elem::Param(p)).collect();
                let mut s = String::from("fn");
                s += &self.delimited("(", ")", Key::Expr(e), &elems, ind, col + 2, width(&r) + 2, false);
                s += &r;
                s.push(' ');
                s += &self.block(body, ind);
                s
            }
            ExprKind::Quant { forall, var, lo, hi, body } => {
                let mut s = format!("{} {} in ", if forall { "forall" } else { "exists" }, var.sym);
                s += &self.render(lo, ind, col + width(&s), 2);
                s.push_str("..");
                s += &self.render(hi, ind, end_col(col, &s), 1);
                s.push_str(": ");
                s += &self.render(body, ind, end_col(col, &s), trail);
                s
            }
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Char(_)
            | ExprKind::Name(_)
            | ExprKind::Error => {
                let mut o = String::new();
                self.flat_into(e, &mut o);
                o
            }
        }
    }

    /// A chain of binary operators of one precedence, broken after each operator: the operator
    /// ends its line so the line break cannot end the statement.
    fn binary(&self, e: ExprId, ind: usize, col: usize, trail: usize) -> String {
        let ExprKind::Binary { op, .. } = self.ast.expr(e).kind else { unreachable!() };
        let prec = op.precedence();
        let mut ops = Vec::new();
        let mut operands = Vec::new();
        let mut cur = e;
        while let ExprKind::Binary { op, lhs, rhs } = self.ast.expr(cur).kind {
            if op.precedence() != prec {
                break;
            }
            ops.push(op);
            operands.push(rhs);
            cur = lhs;
        }
        operands.push(cur);
        operands.reverse();
        ops.reverse();
        let mut s = String::new();
        for (i, &x) in operands.iter().enumerate() {
            let t = match ops.get(i) {
                Some(op) => 1 + op.as_str().len(),
                None => trail,
            };
            if i == 0 {
                s += &self.render(x, ind, col, t);
            } else {
                newline(&mut s, ind + 2);
                s += &self.render(x, ind + 2, ind + 2, t);
            }
            if let Some(op) = ops.get(i) {
                s.push(' ');
                s.push_str(op.as_str());
            }
        }
        s
    }

    // ---------- lists ----------

    /// `open elems close` on one line if it fits and holds no comments; otherwise one element
    /// per line with trailing commas. `tuple`: a single element keeps its comma.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn delimited(
        &self,
        open: &str,
        close: &str,
        key: Key,
        elems: &[Elem],
        ind: usize,
        col: usize,
        trail: usize,
        tuple: bool,
    ) -> String {
        if !self.has(key) {
            let mut f = String::from(open);
            let mut ok = true;
            for (i, &el) in elems.iter().enumerate() {
                if i > 0 {
                    f.push_str(", ");
                }
                if !self.elem_flat_into(el, &mut f) {
                    ok = false;
                    break;
                }
            }
            if ok {
                if tuple && elems.len() == 1 {
                    f.push(',');
                }
                f.push_str(close);
                if col + width(&f) + trail <= self.width {
                    return f;
                }
            }
        }
        self.delimited_broken(open, close, key, elems, ind)
    }

    fn delimited_broken(&self, open: &str, close: &str, key: Key, elems: &[Elem], ind: usize) -> String {
        let lines: Vec<LineElem> = elems
            .iter()
            .map(|&el| {
                let (lo, hi) = self.elem_span(el);
                let mut text = self.elem_render(el, ind + 2, ind + 2, 1);
                text.push(',');
                LineElem { lo, hi, text }
            })
            .collect();
        self.wrapped(open, close, key, &lines, ind, Blanks::Never)
    }

    fn elem_span(&self, el: Elem) -> (u32, u32) {
        match el {
            Elem::Expr(x) => {
                let sp = self.ast.expr(x).span;
                (sp.lo, sp.hi)
            }
            Elem::Param(p) => (p.span.lo, p.span.hi),
            Elem::SelfParam(sp) => (sp.span.lo, sp.span.hi),
        }
    }

    fn elem_flat_into(&self, el: Elem, o: &mut String) -> bool {
        match el {
            Elem::Expr(x) => self.flat_into(x, o),
            Elem::SelfParam(sp) => {
                o.push_str(if sp.mutable { "&mut self" } else { "self" });
                true
            }
            Elem::Param(p) => {
                self.param_head_into(&p, o);
                match p.refine {
                    Some(r) => {
                        o.push_str(" where ");
                        self.flat_into(r, o)
                    }
                    None => true,
                }
            }
        }
    }

    fn elem_render(&self, el: Elem, ind: usize, col: usize, trail: usize) -> String {
        match el {
            Elem::Expr(x) => self.render(x, ind, col, trail),
            Elem::SelfParam(_) => {
                let mut o = String::new();
                self.elem_flat_into(el, &mut o);
                o
            }
            Elem::Param(p) => {
                let mut o = String::new();
                self.param_head_into(&p, &mut o);
                if let Some(r) = p.refine {
                    o.push_str(" where ");
                    let c = col + width(&o);
                    o += &self.render(r, ind, c, trail);
                }
                o
            }
        }
    }

    /// `name`, `name: T` or `name: &mut T`.
    fn param_head_into(&self, p: &Param, o: &mut String) {
        o.push_str(p.name.sym.as_str());
        if let Some(t) = p.ty {
            o.push_str(": ");
            if p.mut_ref {
                o.push_str("&mut ");
            }
            self.ty_into(t, o);
        }
    }

    // ---------- calls and chains ----------

    /// Split a postfix chain into its root and operations, innermost first.
    fn chain(&self, e: ExprId) -> (ExprId, Vec<Op>) {
        let mut ops = Vec::new();
        let mut cur = e;
        loop {
            match self.ast.expr(cur).kind {
                ExprKind::Field { base, name } => {
                    ops.push(Op::Field(name));
                    cur = base;
                }
                ExprKind::TupleField { base, index, .. } => {
                    ops.push(Op::TupleField(index));
                    cur = base;
                }
                ExprKind::Call { callee, args } => {
                    ops.push(Op::Call(cur, args));
                    cur = callee;
                }
                ExprKind::Index { base, index } => {
                    ops.push(Op::Index(index));
                    cur = base;
                }
                ExprKind::Try(x) => {
                    ops.push(Op::Try);
                    cur = x;
                }
                ExprKind::TypeArgs { base, args } => {
                    ops.push(Op::TypeArgs(args));
                    cur = base;
                }
                _ => break,
            }
        }
        ops.reverse();
        (cur, ops)
    }

    /// A postfix chain that does not fit flat. With two or more method calls it breaks before
    /// each `.name(...)` (a line starting with `.` continues the previous one); otherwise only
    /// call arguments and indexes break.
    fn postfix(&self, e: ExprId, ind: usize, col: usize, trail: usize) -> String {
        let (root, ops) = self.chain(e);
        let starts = segment_starts(&ops);
        if starts.len() >= 2 {
            if let Some(s) = self.chain_hug(root, &ops, ind, col) {
                return s;
            }
            let head = &ops[..starts[0]];
            let mut s = self.render(root, ind, col, self.ops_trail(head, 0));
            let c = end_col(col, &s);
            s += &self.ops_inline(head, ind, c, 0);
            for (n, &st) in starts.iter().enumerate() {
                let end = starts.get(n + 1).copied().unwrap_or(ops.len());
                let t = if end == ops.len() { trail } else { 0 };
                newline(&mut s, ind + 2);
                s += &self.ops_inline(&ops[st..end], ind + 2, ind + 2, t);
            }
            return s;
        }
        let mut s = self.render(root, ind, col, self.ops_trail(&ops, trail));
        let c = end_col(col, &s);
        s += &self.ops_inline(&ops, ind, c, trail);
        s
    }

    /// The width that follows on the same line after a chain's root, up to the next point
    /// where it may break.
    fn ops_trail(&self, ops: &[Op], trail: usize) -> usize {
        let mut w = 0;
        for op in ops {
            match *op {
                Op::Field(n) => w += 1 + width(n.sym.as_str()),
                Op::TupleField(i) => w += 1 + i.to_string().len(),
                Op::Try => w += 1,
                Op::TypeArgs(ts) => {
                    let mut o = String::new();
                    self.type_args_into(ts, &mut o);
                    w += width(&o);
                }
                Op::Index(_) | Op::Call(..) => return w + 1,
            }
        }
        w + trail
    }

    /// Print `ops` on the current line; only call arguments and indexes may break.
    fn ops_inline(&self, ops: &[Op], ind: usize, col: usize, trail: usize) -> String {
        let mut s = String::new();
        let mut c = col;
        for (i, op) in ops.iter().enumerate() {
            let rest = self.ops_trail(&ops[i + 1..], trail);
            let part = match *op {
                Op::Field(n) => format!(".{}", n.sym),
                Op::TupleField(k) => format!(".{k}"),
                Op::Try => "?".to_string(),
                Op::TypeArgs(ts) => {
                    let mut o = String::new();
                    self.type_args_into(ts, &mut o);
                    o
                }
                Op::Index(x) => format!("[{}]", self.render(x, ind, c + 1, rest + 1)),
                Op::Call(call, args) => self.call_args(call, args, ind, c, rest),
            };
            c = end_col(c, &part);
            s += &part;
        }
        s
    }

    fn op_flat_into(&self, op: Op, o: &mut String) -> bool {
        match op {
            Op::Field(n) => {
                o.push('.');
                o.push_str(n.sym.as_str());
            }
            Op::TupleField(k) => {
                o.push('.');
                o.push_str(&k.to_string());
            }
            Op::Try => o.push('?'),
            Op::TypeArgs(ts) => self.type_args_into(ts, o),
            Op::Index(x) => {
                o.push('[');
                if !self.flat_into(x, o) {
                    return false;
                }
                o.push(']');
            }
            Op::Call(call, args) => {
                if self.has(Key::Expr(call)) {
                    return false;
                }
                o.push('(');
                if !self.flat_list_into(self.ast.expr_lists.get(args), o) {
                    return false;
                }
                o.push(')');
            }
        }
        true
    }

    /// A whole chain on one line except for a multi-line closure that ends the last call:
    /// `xs.iter().map(fn(x) {` ... `})`.
    fn chain_hug(&self, root: ExprId, ops: &[Op], ind: usize, col: usize) -> Option<String> {
        let k = ops.iter().rposition(|op| matches!(op, Op::Call(..)))?;
        if ops[k + 1..].iter().any(|op| matches!(op, Op::Index(_))) {
            return None;
        }
        let mut s = self.flat(root)?;
        for &op in &ops[..k] {
            if !self.op_flat_into(op, &mut s) {
                return None;
            }
        }
        let Op::Call(call, args) = ops[k] else { return None };
        let c = col + width(&s);
        s += &self.hug(call, self.ast.expr_lists.get(args), ind, c, true)?;
        for &op in &ops[k + 1..] {
            self.op_flat_into(op, &mut s);
        }
        Some(s)
    }

    /// The parenthesized arguments of `call`: flat, else hugging a final closure, else one per
    /// line.
    fn call_args(&self, call: ExprId, args: List<ExprId>, ind: usize, col: usize, trail: usize) -> String {
        let key = Key::Expr(call);
        let xs = self.ast.expr_lists.get(args);
        if !self.has(key) {
            let mut f = String::from("(");
            if self.flat_list_into(xs, &mut f) {
                f.push(')');
                if col + width(&f) + trail <= self.width {
                    return f;
                }
            }
            if let Some(h) = self.hug(call, xs, ind, col, false) {
                return h;
            }
        }
        let elems: Vec<Elem> = xs.iter().map(|&x| Elem::Expr(x)).collect();
        self.delimited_broken("(", ")", key, &elems, ind)
    }

    /// `(a, b, fn(x) {` ... `})`: the other arguments flat on the call's line, the final
    /// closure's body on the following lines. With `multiline_only`, only for a closure whose
    /// body cannot be inline anyway.
    fn hug(&self, call: ExprId, args: &[ExprId], ind: usize, col: usize, multiline_only: bool) -> Option<String> {
        let (&last, rest) = args.split_last()?;
        let ExprKind::Closure { params, ret, body } = self.ast.expr(last).kind else { return None };
        if self.has(Key::Expr(call)) || self.has(Key::Expr(last)) {
            return None;
        }
        match self.inline_body(body) {
            Some(None) => return None,
            Some(Some(_)) if multiline_only => return None,
            _ => {}
        }
        let mut s = String::from("(");
        for &a in rest {
            if !self.flat_into(a, &mut s) {
                return None;
            }
            s.push_str(", ");
        }
        s.push_str("fn(");
        if !self.flat_params_into(params, &mut s) {
            return None;
        }
        s.push(')');
        if let Some(r) = ret {
            s.push_str(" -> ");
            self.ty_into(r, &mut s);
        }
        s.push(' ');
        if col + width(&s) + 1 > self.width {
            return None;
        }
        s += &self.block(body, ind);
        s.push(')');
        Some(s)
    }

    // ---------- blocks and control flow ----------

    /// A block: `{}` when empty, else one statement per line at `ind + 2` and `}` at `ind`.
    pub(super) fn block(&self, b: ExprId, ind: usize) -> String {
        let ExprKind::Block(stmts) = self.ast.expr(b).kind else { return self.render(b, ind, ind, 0) };
        let elems: Vec<LineElem> = self
            .ast
            .stmt_lists
            .get(stmts)
            .iter()
            .map(|&s| {
                let sp = self.ast.stmt(s).span;
                LineElem { lo: sp.lo, hi: sp.hi, text: self.stmt(s, ind + 2) }
            })
            .collect();
        self.braced(Key::Expr(b), &elems, ind)
    }

    /// An `if` with every branch on its own lines.
    fn if_broken(&self, e: ExprId, ind: usize, col: usize) -> String {
        let ExprKind::If { cond, then, els } = self.ast.expr(e).kind else { unreachable!() };
        let mut s = String::from("if ");
        s += &self.render(cond, ind, col + 3, 2);
        s.push(' ');
        s += &self.block(then, ind);
        if let Some(x) = els {
            s.push_str(" else ");
            let c = end_col(col, &s);
            match self.ast.expr(x).kind {
                ExprKind::If { .. } => s += &self.if_broken(x, ind, c),
                _ => s += &self.block(x, ind),
            }
        }
        s
    }

    fn match_expr(&self, e: ExprId, ind: usize, col: usize) -> String {
        let ExprKind::Match { scrut, arms } = self.ast.expr(e).kind else { unreachable!() };
        let mut s = String::from("match ");
        s += &self.render(scrut, ind, col + 6, 2);
        s.push(' ');
        let elems: Vec<LineElem> = self
            .ast
            .arms
            .get(arms)
            .iter()
            .map(|a| LineElem { lo: a.span.lo, hi: a.span.hi, text: self.arm(a, ind + 2) })
            .collect();
        s += &self.braced(Key::Expr(e), &elems, ind);
        s
    }

    /// `pat if guard => body`.
    fn arm(&self, a: &Arm, ind: usize) -> String {
        let mut s = self.pat(a.pat);
        if let Some(g) = a.guard {
            s.push_str(" if ");
            let c = ind + width(&s);
            s += &self.render(g, ind, c, 3);
        }
        s.push_str(" => ");
        let c = end_col(ind, &s);
        // `Open => open += 1`: a brace-less assignment arm stays one statement.
        if let ExprKind::Block(stmts) = self.ast.expr(a.body).kind {
            let ids = self.ast.stmt_lists.get(stmts);
            if ids.len() == 1
                && self.ast.stmts[ids[0]].span.lo == self.ast.expr(a.body).span.lo
                && matches!(self.ast.stmts[ids[0]].kind, StmtKind::Assign { .. })
                && !self.has(Key::Expr(a.body))
            {
                s += &self.stmt(ids[0], ind);
                return s;
            }
        }
        if let ExprKind::Block(_) = self.ast.expr(a.body).kind {
            let mut f = String::new();
            if self.flat_block_into(a.body, &mut f) && c + width(&f) <= self.width {
                s += &f;
            } else {
                s += &self.block(a.body, ind);
            }
        } else {
            s += &self.render(a.body, ind, c, 0);
        }
        s
    }

    /// `head kw a, b {body}`, for the single clause of a `for` (`invariant`) or a `test`
    /// (`requires`): on one line if it fits, else the clause on its own line and `{` on the
    /// next, else one condition per line.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn with_clause(
        &self,
        head: String,
        col: usize,
        kw: &str,
        conds: &[ExprId],
        body: ExprId,
        ind: usize,
    ) -> String {
        let mut s = head;
        let blk = self.block(body, ind);
        if conds.is_empty() {
            s.push(' ');
            s += &blk;
            return s;
        }
        let end = end_col(col, &s);
        if let Some(j) = self.flat_joined(conds) {
            if end + kw.len() + width(&j) + 4 <= self.width {
                s += &format!(" {kw} {j} {blk}");
                return s;
            }
            if ind + 2 + kw.len() + 1 + width(&j) <= self.width {
                newline(&mut s, ind + 2);
                s += &format!("{kw} {j}");
                newline(&mut s, ind);
                s += &blk;
                return s;
            }
        }
        newline(&mut s, ind + 2);
        s.push_str(kw);
        s.push(' ');
        for (i, &c) in conds.iter().enumerate() {
            let t = if i + 1 < conds.len() { 1 } else { 0 };
            if i == 0 {
                s += &self.render(c, ind + 2, ind + 3 + kw.len(), t);
            } else {
                s.push(',');
                newline(&mut s, ind + 4);
                s += &self.render(c, ind + 4, ind + 4, t);
            }
        }
        newline(&mut s, ind);
        s += &blk;
        s
    }

    // ---------- statements ----------

    /// A statement whose first line starts at column `ind`.
    pub(super) fn stmt(&self, id: StmtId, ind: usize) -> String {
        let ast = self.ast;
        match ast.stmt(id).kind {
            StmtKind::Let { pat, ty, init, mutable } => {
                let mut s = String::from(if mutable { "var " } else { "let " });
                self.pat_into(pat, &mut s);
                if let Some(t) = ty {
                    s.push_str(": ");
                    self.ty_into(t, &mut s);
                }
                s.push_str(" = ");
                let c = ind + width(&s);
                s += &self.render(init, ind, c, 0);
                s
            }
            StmtKind::Assign { target, op, value } => {
                let mut s = self.render(target, ind, ind, 0);
                s.push(' ');
                if let Some(op) = op {
                    s.push_str(op.as_str());
                }
                s.push_str("= ");
                let c = end_col(ind, &s);
                s += &self.render(value, ind, c, 0);
                s
            }
            StmtKind::Expr(e) => match ast.expr(e).kind {
                // A statement `if` always spans lines; inline `if` is for values.
                ExprKind::If { .. } => self.if_broken(e, ind, ind),
                _ => self.render(e, ind, ind, 0),
            },
            StmtKind::Return(None) => "return".to_string(),
            StmtKind::Return(Some(e)) => format!("return {}", self.render(e, ind, ind + 7, 0)),
            StmtKind::Break => "break".to_string(),
            StmtKind::Continue => "continue".to_string(),
            StmtKind::For { head, invariants, body } => {
                let invs = ast.expr_lists.get(invariants);
                let t = if invs.is_empty() { 2 } else { 0 };
                let mut s = String::from("for");
                match head {
                    ForHead::Infinite => {}
                    ForHead::Cond(c) => {
                        s.push(' ');
                        s += &self.render(c, ind, ind + 4, t);
                    }
                    ForHead::In { pat, iter } => {
                        s.push(' ');
                        self.pat_into(pat, &mut s);
                        s.push_str(" in ");
                        let c = ind + width(&s);
                        s += &self.render(iter, ind, c, t);
                    }
                }
                self.with_clause(s, ind, "invariant", invs, body, ind)
            }
        }
    }

    // ---------- types ----------

    pub(super) fn ty(&self, t: TypeExprId) -> String {
        let mut o = String::new();
        self.ty_into(t, &mut o);
        o
    }

    pub(super) fn ty_into(&self, t: TypeExprId, o: &mut String) {
        let ast = self.ast;
        let te = ast.ty(t);
        match te.kind {
            TypeExprKind::Named { pkg, name, args, unit } => {
                if let Some(p) = pkg {
                    o.push_str(p.sym.as_str());
                    o.push('.');
                }
                o.push_str(name.sym.as_str());
                if let Some(u) = unit {
                    o.push('<');
                    self.unit_into(u, o);
                    o.push('>');
                } else {
                    self.type_args_into(args, o);
                }
            }
            TypeExprKind::Tuple(l) => {
                let ts = ast.type_lists.get(l);
                o.push('(');
                self.types_into(ts, o);
                if ts.len() == 1 {
                    o.push(',');
                }
                o.push(')');
            }
            TypeExprKind::Fn { params, ret, effects } => {
                o.push_str("fn(");
                self.types_into(ast.type_lists.get(params), o);
                o.push(')');
                if let Some(r) = ret {
                    o.push_str(" -> ");
                    self.ty_into(r, o);
                }
                if let Some(es) = effects {
                    o.push(' ');
                    self.effects_into(es, o);
                }
            }
            TypeExprKind::Refined { base, pred } => {
                self.ty_into(base, o);
                o.push_str(" where ");
                match self.flat(pred) {
                    Some(f) => o.push_str(&f),
                    None => o.push_str(&self.render(pred, 0, 0, 0)),
                }
            }
            TypeExprKind::Error => o.push_str(self.span_text(te.span)),
        }
    }

    fn types_into(&self, ts: &[TypeExprId], o: &mut String) {
        for (i, &t) in ts.iter().enumerate() {
            if i > 0 {
                o.push_str(", ");
            }
            self.ty_into(t, o);
        }
    }

    /// `<A, B>`, or nothing for no arguments.
    fn type_args_into(&self, args: List<TypeExprId>, o: &mut String) {
        let ts = self.ast.type_lists.get(args);
        if !ts.is_empty() {
            o.push('<');
            self.types_into(ts, o);
            o.push('>');
        }
    }

    /// A unit expression: `m`, `m/s^2`, `kg*m`, `1/s`.
    fn unit_into(&self, factors: List<UnitFactor>, o: &mut String) {
        let fs = self.ast.unit_factors.get(factors);
        if fs.is_empty() {
            o.push('1');
        }
        for (i, f) in fs.iter().enumerate() {
            match (i, f.exp < 0) {
                (0, true) => o.push_str("1/"),
                (0, false) => {}
                (_, true) => o.push('/'),
                (_, false) => o.push('*'),
            }
            o.push_str(f.name.sym.as_str());
            let m = f.exp.unsigned_abs();
            if m != 1 {
                o.push('^');
                o.push_str(&m.to_string());
            }
        }
    }

    /// `effects {io, fs}`.
    pub(super) fn effects_into(&self, es: List<Name>, o: &mut String) {
        o.push_str("effects {");
        for (i, n) in self.ast.names.get(es).iter().enumerate() {
            if i > 0 {
                o.push_str(", ");
            }
            o.push_str(n.sym.as_str());
        }
        o.push('}');
    }

    /// `<T: Ord + Hash, U>`, or nothing.
    pub(super) fn generics_into(&self, gs: List<GenericParam>, o: &mut String) {
        let gs = self.ast.generics.get(gs);
        if gs.is_empty() {
            return;
        }
        o.push('<');
        for (i, g) in gs.iter().enumerate() {
            if i > 0 {
                o.push_str(", ");
            }
            o.push_str(g.name.sym.as_str());
            for (j, &b) in self.ast.type_lists.get(g.bounds).iter().enumerate() {
                o.push_str(if j == 0 { ": " } else { " + " });
                self.ty_into(b, o);
            }
        }
        o.push('>');
    }

    // ---------- patterns ----------

    fn pat(&self, p: PatId) -> String {
        let mut o = String::new();
        self.pat_into(p, &mut o);
        o
    }

    fn pat_into(&self, p: PatId, o: &mut String) {
        let ast = self.ast;
        let pt = ast.pat(p);
        match pt.kind {
            PatKind::Wild => o.push('_'),
            PatKind::Bind(s) => o.push_str(s.as_str()),
            PatKind::Lit(e) => {
                self.flat_into(e, o);
            }
            PatKind::Variant { name, args, has_parens } => {
                o.push_str(name.sym.as_str());
                if has_parens {
                    o.push('(');
                    self.pats_into(ast.pat_lists.get(args), ", ", o);
                    o.push(')');
                }
            }
            PatKind::Tuple(l) => {
                let ps = ast.pat_lists.get(l);
                o.push('(');
                self.pats_into(ps, ", ", o);
                if ps.len() == 1 {
                    o.push(',');
                }
                o.push(')');
            }
            PatKind::Or(l) => self.pats_into(ast.pat_lists.get(l), " | ", o),
            PatKind::Error => o.push_str(self.span_text(pt.span)),
        }
    }

    fn pats_into(&self, ps: &[PatId], sep: &str, o: &mut String) {
        for (i, &p) in ps.iter().enumerate() {
            if i > 0 {
                o.push_str(sep);
            }
            self.pat_into(p, o);
        }
    }
}
