//! Items and the file.

use super::expr::{newline, push_str_lit, width, Elem};
use super::{Blanks, Key, LineElem, Printer, Slot};
use crate::syntax::ast::*;

impl Printer<'_> {
    pub(super) fn print_file(&self) -> String {
        let ast = self.ast;
        let items = ast.item_lists.get(self.file.items);
        let elems: Vec<LineElem> = items
            .iter()
            .map(|&id| {
                let sp = ast.item(id).span;
                LineElem { lo: sp.lo, hi: sp.hi, text: self.item(id, 0) }
            })
            .collect();
        // Consecutive single-line items of one of these kinds keep the author's blank lines.
        let kinds: Vec<Option<u8>> = items
            .iter()
            .zip(&elems)
            .map(|(&id, el)| {
                let kind = match ast.item(id).kind {
                    ItemKind::Import { .. } => 1,
                    ItemKind::Const { .. } => 2,
                    ItemKind::Unit => 3,
                    ItemKind::TypeAlias { .. } => 4,
                    _ => return None,
                };
                (!el.text.contains('\n')).then_some(kind)
            })
            .collect();
        self.line_list(Key::File, &elems, 0, Blanks::Items(&kinds))
    }

    /// An item whose first line starts at column `ind`.
    fn item(&self, id: ItemId, ind: usize) -> String {
        let ast = self.ast;
        let it = ast.item(id);
        let mut s = String::from(if it.is_pub { "pub " } else { "" });
        match it.kind {
            ItemKind::Import { path, .. } => {
                s.push_str("import ");
                push_str_lit(&mut s, path);
            }
            ItemKind::Fn(f) => return self.fn_item(id, &f, ind, it.is_pub),
            ItemKind::Struct { generics, fields, invariants } => {
                s.push_str("struct ");
                s.push_str(it.name.sym.as_str());
                self.generics_into(generics, &mut s);
                s.push(' ');
                let mut elems: Vec<LineElem> = ast
                    .fields
                    .get(fields)
                    .iter()
                    .map(|fd| LineElem { lo: fd.span.lo, hi: fd.span.hi, text: self.field_decl(fd, ind + 2, ind + 2) })
                    .collect();
                for &inv in ast.expr_lists.get(invariants) {
                    let sp = ast.expr(inv).span;
                    let text = format!("invariant {}", self.render(inv, ind + 2, ind + 12, 0));
                    elems.push(LineElem { lo: sp.lo, hi: sp.hi, text });
                }
                s += &self.braced(Key::Item(id), &elems, ind);
            }
            ItemKind::Enum { generics, variants } => {
                s.push_str("enum ");
                s.push_str(it.name.sym.as_str());
                self.generics_into(generics, &mut s);
                s.push(' ');
                let elems: Vec<LineElem> = ast
                    .variants
                    .get(variants)
                    .iter()
                    .map(|v| LineElem { lo: v.span.lo, hi: v.span.hi, text: self.variant(v, ind + 2) })
                    .collect();
                s += &self.braced(Key::Item(id), &elems, ind);
            }
            ItemKind::Interface { generics, methods } => {
                s.push_str("interface ");
                s.push_str(it.name.sym.as_str());
                self.generics_into(generics, &mut s);
                s.push(' ');
                let elems: Vec<LineElem> = ast
                    .item_lists
                    .get(methods)
                    .iter()
                    .filter_map(|&m| {
                        let ItemKind::Fn(f) = ast.item(m).kind else { return None };
                        let sp = ast.item(m).span;
                        Some(LineElem { lo: sp.lo, hi: sp.hi, text: self.fn_item(m, &f, ind + 2, false) })
                    })
                    .collect();
                s += &self.braced(Key::Item(id), &elems, ind);
            }
            ItemKind::TypeAlias { generics, ty } => {
                s.push_str("type ");
                s.push_str(it.name.sym.as_str());
                self.generics_into(generics, &mut s);
                s.push_str(" = ");
                self.ty_into(ty, &mut s);
            }
            ItemKind::Const { ty, value } => {
                s.push_str("const ");
                s.push_str(it.name.sym.as_str());
                s.push_str(": ");
                self.ty_into(ty, &mut s);
                s.push_str(" = ");
                let c = ind + width(&s);
                s += &self.render(value, ind, c, 0);
            }
            ItemKind::Unit => {
                s.push_str("unit ");
                s.push_str(it.name.sym.as_str());
            }
            ItemKind::Test { label, params, requires, body } => {
                s.push_str("test ");
                push_str_lit(&mut s, label);
                let key = Key::Item(id);
                let ps = ast.params.get(params);
                let conds = ast.expr_lists.get(requires);
                if !ps.is_empty() || self.has(key) {
                    let elems: Vec<Elem> = ps.iter().map(|&p| Elem::Param(p)).collect();
                    let c = ind + width(&s);
                    let t = if conds.is_empty() { 2 } else { 0 };
                    s += &self.delimited("(", ")", key, &elems, ind, c, t, false);
                }
                return self.with_clause(s, ind, "requires", conds, body, ind);
            }
        }
        s
    }

    /// `[pub ]name: T[ where pred]`, starting at column `col` of a line indented `ind`.
    fn field_decl(&self, fd: &FieldDecl, ind: usize, col: usize) -> String {
        let mut s = String::from(if fd.is_pub { "pub " } else { "" });
        s.push_str(fd.name.sym.as_str());
        s.push_str(": ");
        self.ty_into(fd.ty, &mut s);
        if let Some(r) = fd.refine {
            s.push_str(" where ");
            let c = col + width(&s);
            s += &self.render(r, ind, c, 0);
        }
        s
    }

    /// `Name`, `Name()` or `Name(r: Float, Int)`; positional fields print as bare types.
    fn variant(&self, v: &VariantDecl, ind: usize) -> String {
        let mut s = v.name.sym.to_string();
        if v.has_parens {
            s.push('(');
            for (i, fd) in self.ast.fields.get(v.fields).iter().enumerate() {
                if i > 0 {
                    s.push_str(", ");
                }
                // The parser names positional fields `_0`, `_1`, ... with the type's span.
                if fd.name.span == fd.span && fd.name.sym.as_str().starts_with('_') {
                    self.ty_into(fd.ty, &mut s);
                } else {
                    let c = ind + width(&s);
                    s += &self.field_decl(fd, ind, c);
                }
            }
            s.push(')');
        }
        s
    }

    /// `[step "..."\n][pub ]fn [Recv.]name<G>(params) -> Ret`, its clauses one per line, and
    /// its body.
    fn fn_item(&self, id: ItemId, f: &FnDecl, ind: usize, is_pub: bool) -> String {
        let name = self.ast.item(id).name;
        let mut s = String::new();
        if let Some((pat, _)) = f.step {
            s.push_str("step ");
            push_str_lit(&mut s, pat);
            newline(&mut s, ind);
        }
        let line = s.len();
        if is_pub {
            s.push_str("pub ");
        }
        s.push_str("fn ");
        if let Some(r) = f.recv {
            s.push_str(r.ty.sym.as_str());
            self.generics_into(r.generics, &mut s);
            s.push('.');
        }
        s.push_str(name.sym.as_str());
        self.generics_into(f.generics, &mut s);
        let ret = f.ret.map(|t| format!(" -> {}", self.ty(t))).unwrap_or_default();
        // A lone `effects` clause stays on the signature line when it fits:
        // `fn main() effects {io} {`.
        let effects_inline = if f.requires.is_empty() && f.ensures.is_empty() && !self.has(Key::Clauses(id)) {
            f.effects.map(|es| {
                let mut o = String::from(" ");
                self.effects_into(es, &mut o);
                o
            })
        } else {
            None
        };
        let has_clauses = !f.requires.is_empty() || !f.ensures.is_empty() || f.effects.is_some() || self.has(Key::Clauses(id));
        if let Some(eff) = &effects_inline {
            let mut one = s.clone();
            let mut elems = Vec::new();
            if let Some(sp) = f.self_param {
                elems.push(Elem::SelfParam(sp));
            }
            elems.extend(self.ast.params.get(f.params).iter().map(|&p| Elem::Param(p)));
            let tail = width(&ret) + width(eff) + if f.body.is_some() { 2 } else { 0 };
            let col = ind + width(&one[line..]);
            one += &self.delimited("(", ")", Key::Item(id), &elems, ind, col, tail, false);
            let last = one.rsplit('\n').next().unwrap_or("");
            let start_col = if one[line..].contains('\n') { 0 } else { ind };
            if !one[line..].contains('\n') && start_col + width(last) + width(&ret) + width(eff) + 2 <= self.width {
                one += &ret;
                one += eff;
                if let Some(b) = f.body {
                    one.push(' ');
                    one += &self.block(b, ind);
                }
                return one;
            }
        }
        let tail = width(&ret) + if f.body.is_some() && !has_clauses { 2 } else { 0 };
        let mut elems = Vec::new();
        if let Some(sp) = f.self_param {
            elems.push(Elem::SelfParam(sp));
        }
        elems.extend(self.ast.params.get(f.params).iter().map(|&p| Elem::Param(p)));
        let col = ind + width(&s[line..]);
        s += &self.delimited("(", ")", Key::Item(id), &elems, ind, col, tail, false);
        s += &ret;
        if has_clauses {
            s += &self.clauses(id, f, ind);
            if let Some(b) = f.body {
                newline(&mut s, ind);
                s += &self.block(b, ind);
            }
        } else if let Some(b) = f.body {
            s.push(' ');
            s += &self.block(b, ind);
        }
        s
    }

    /// The clause lines after a signature: `requires`, `ensures`, `effects`, each on its own
    /// line at `ind + 2`. Conditions of one clause share a line when it fits; each gets its own
    /// line otherwise, and whenever the clauses hold comments.
    fn clauses(&self, id: ItemId, f: &FnDecl, ind: usize) -> String {
        let ast = self.ast;
        let key = Key::Clauses(id);
        let ci = ind + 2;
        let groups = [("requires", ast.expr_lists.get(f.requires)), ("ensures", ast.expr_lists.get(f.ensures))];
        let effects = f.effects.map(|es| {
            let mut o = String::new();
            self.effects_into(es, &mut o);
            o
        });
        let mut s = String::new();
        if self.has(key) {
            for &c in self.slot(key, Slot::Open) {
                s.push(' ');
                s.push_str(self.comment(c));
            }
            let mut lines = Vec::new();
            for (kw, conds) in groups {
                for &e in conds {
                    let sp = ast.expr(e).span;
                    let text = format!("{kw} {}", self.render(e, ci, ci + kw.len() + 1, 0));
                    lines.push(LineElem { lo: sp.lo, hi: sp.hi, text });
                }
            }
            if let Some(text) = effects {
                lines.push(LineElem { lo: f.effects_span.lo, hi: f.effects_span.hi, text });
            }
            let body = self.line_list(key, &lines, ci, Blanks::Never);
            if !body.is_empty() {
                s.push('\n');
                s += &body;
            }
            return s;
        }
        for (kw, conds) in groups {
            if conds.is_empty() {
                continue;
            }
            if let Some(j) = self.flat_joined(conds) {
                if ci + kw.len() + 1 + width(&j) <= self.width {
                    newline(&mut s, ci);
                    s += &format!("{kw} {j}");
                    continue;
                }
            }
            for &e in conds {
                newline(&mut s, ci);
                s += &format!("{kw} {}", self.render(e, ci, ci + kw.len() + 1, 0));
            }
        }
        if let Some(text) = effects {
            newline(&mut s, ci);
            s += &text;
        }
        s
    }
}
