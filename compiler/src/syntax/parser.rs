//! Hand-written recursive-descent parser producing the flat-arena AST.

use super::ast::*;
use super::lexer::{self, Lexed};
use super::token::{Token, TokenKind as T};
use crate::diag::{codes, Diagnostic, FileId, Span};
use crate::util::intern::LocalInterner;
use crate::util::{sym, List, Symbol};

type PResult<V> = Result<V, ()>;

pub struct ParseOutput {
    pub diags: Vec<Diagnostic>,
}

/// Parse one file into `ast`, appending a `FileAst`.
pub fn parse_file(
    ast: &mut Ast,
    file: FileId,
    src: &str,
    base: u32,
    interner: &mut LocalInterner,
) -> ParseOutput {
    let Lexed { tokens, comments, blank_lines, diags } = lexer::lex(src, base);
    let file_index = ast.files.len() as u32;
    let mut p = Parser {
        src,
        base,
        toks: tokens,
        pos: 0,
        ast,
        diags,
        interner,
        no_struct: false,
        file_index,
        prev_hi: base,
    };
    let items = p.parse_items();
    let span = Span::new(base, base + src.len() as u32);
    p.ast.files.push(FileAst { file, items, comments, blank_lines, span });
    ParseOutput { diags: p.diags }
}

struct Parser<'a> {
    src: &'a str,
    base: u32,
    toks: Vec<Token>,
    pos: usize,
    ast: &'a mut Ast,
    diags: Vec<Diagnostic>,
    interner: &'a mut LocalInterner,
    /// Inside `if`/`for`/`match` heads and contract clauses, `Name {` is not a struct literal.
    no_struct: bool,
    file_index: u32,
    prev_hi: u32,
}

/// Heuristic used by the parser: does this identifier name a type (UpperCamel)?
pub fn is_type_like(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() => s.len() == 1 || s.chars().any(|c| c.is_ascii_lowercase()),
        _ => false,
    }
}

impl<'a> Parser<'a> {
    // ---------- token helpers ----------

    #[inline]
    fn tok(&self) -> Token {
        self.toks[self.pos]
    }
    #[inline]
    fn peek(&self) -> T {
        self.toks[self.pos].kind
    }
    #[inline]
    fn peek_at(&self, n: usize) -> T {
        self.toks.get(self.pos + n).map(|t| t.kind).unwrap_or(T::Eof)
    }
    #[inline]
    fn at(&self, k: T) -> bool {
        self.peek() == k
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos];
        if t.kind != T::Eof {
            self.pos += 1;
        }
        if !t.is_inserted_semi() {
            self.prev_hi = t.hi;
        }
        t
    }
    fn eat(&mut self, k: T) -> bool {
        if self.at(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn text(&self, t: Token) -> &'a str {
        &self.src[(t.lo - self.base) as usize..(t.hi - self.base) as usize]
    }
    fn sym(&mut self, t: Token) -> Symbol {
        let s = self.text(t);
        self.interner.intern(s)
    }
    fn tok_is_ident(&self, s: &str) -> bool {
        self.at(T::Ident) && self.text(self.tok()) == s
    }
    fn ident_at(&self, n: usize, s: &str) -> bool {
        self.toks.get(self.pos + n).is_some_and(|t| t.kind == T::Ident && self.text(*t) == s)
    }
    fn span_from(&self, lo: u32) -> Span {
        Span::new(lo, self.prev_hi.max(lo))
    }
    fn skip_semis(&mut self) {
        while self.at(T::Semi) {
            self.bump();
        }
    }
    /// Skip line-break terminators if the next real token is `k`.
    fn skip_semis_before(&mut self, k: T) -> bool {
        let mut n = 0;
        while self.peek_at(n) == T::Semi {
            n += 1;
        }
        if self.peek_at(n) == k {
            for _ in 0..n {
                self.bump();
            }
            true
        } else {
            false
        }
    }
    fn skip_semis_before_any(&mut self, ks: &[T]) -> bool {
        let mut n = 0;
        while self.peek_at(n) == T::Semi {
            n += 1;
        }
        if ks.contains(&self.peek_at(n)) {
            for _ in 0..n {
                self.bump();
            }
            true
        } else {
            false
        }
    }

    fn error_at(&mut self, span: Span, msg: impl Into<String>) {
        // Avoid cascades at one position.
        if self.diags.last().is_some_and(|d| d.span.lo == span.lo) {
            return;
        }
        self.diags.push(Diagnostic::error(codes::E0105, span, msg));
    }

    fn expected(&mut self, what: &str) {
        let t = self.tok();
        let found = match t.kind {
            T::Ident => format!("`{}`", self.text(t)),
            k => k.describe().to_string(),
        };
        let span = if t.kind == T::Semi || t.kind == T::Eof { Span::new(t.lo, t.lo) } else { t.span() };
        self.error_at(span, format!("expected {what}, found {found}"));
    }

    fn expect(&mut self, k: T) -> PResult<Token> {
        if self.at(k) {
            Ok(self.bump())
        } else {
            self.expected(k.describe());
            Err(())
        }
    }

    fn expect_ident(&mut self, what: &str) -> PResult<Name> {
        if self.at(T::Ident) {
            let t = self.bump();
            let s = self.sym(t);
            Ok(Name { sym: s, span: t.span() })
        } else {
            self.expected(what);
            Err(())
        }
    }

    // ---------- node constructors ----------

    fn mk_expr(&mut self, kind: ExprKind, span: Span) -> ExprId {
        self.ast.exprs.push(Expr { kind, span })
    }
    fn mk_type(&mut self, kind: TypeExprKind, span: Span) -> TypeExprId {
        self.ast.types.push(TypeExpr { kind, span })
    }
    fn mk_pat(&mut self, kind: PatKind, span: Span) -> PatId {
        self.ast.pats.push(Pat { kind, span })
    }
    fn mk_stmt(&mut self, kind: StmtKind, span: Span) -> StmtId {
        self.ast.stmts.push(Stmt { kind, span })
    }
    fn err_expr(&mut self, span: Span) -> ExprId {
        self.mk_expr(ExprKind::Error, span)
    }

    // ---------- recovery ----------

    /// Skip to the end of the current statement (a terminator or a closing brace at depth 0).
    fn recover_stmt(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                T::Eof => return,
                T::LBrace | T::LParen | T::LBracket => depth += 1,
                T::RBrace | T::RParen | T::RBracket => {
                    if depth == 0 {
                        return;
                    }
                    depth -= 1;
                }
                T::Semi if depth == 0 => {
                    self.bump();
                    return;
                }
                _ => {}
            }
            self.bump();
        }
    }

    /// Skip to the next token that can start an item at the top level.
    fn recover_item(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.peek() {
                T::Eof => return,
                T::LBrace | T::LParen | T::LBracket => depth += 1,
                T::RBrace | T::RParen | T::RBracket => depth -= 1,
                T::Fn | T::Struct | T::Enum | T::Interface | T::Type | T::Const | T::Unit | T::Test
                | T::Step | T::Import | T::Pub
                    if depth <= 0 =>
                {
                    // only at a line start
                    if self.pos == 0 || self.toks[self.pos - 1].kind == T::Semi {
                        return;
                    }
                }
                _ => {}
            }
            self.bump();
        }
    }

    // ---------- items ----------

    fn parse_items(&mut self) -> List<ItemId> {
        let mark = self.ast.item_lists.mark();
        loop {
            self.skip_semis();
            if self.at(T::Eof) {
                break;
            }
            let before = self.pos;
            match self.parse_item() {
                Ok(id) => self.ast.item_lists.push_scratch(id),
                Err(()) => {
                    if self.pos == before {
                        self.bump();
                    }
                    self.recover_item();
                }
            }
        }
        self.ast.item_lists.finish(mark)
    }

    fn push_item(&mut self, kind: ItemKind, name: Name, lo: u32, is_pub: bool) -> ItemId {
        let span = self.span_from(lo);
        self.ast.items.push(Item { kind, name, span, is_pub, file: self.file_index })
    }

    fn parse_item(&mut self) -> PResult<ItemId> {
        let lo = self.tok().lo;
        let is_pub = self.eat(T::Pub);
        match self.peek() {
            T::Import => {
                self.bump();
                let t = self.expect(T::Str)?;
                let raw = self.text(t);
                let path = lexer::unescape(&raw[1..raw.len().saturating_sub(1).max(1)]);
                let last = path.rsplit('/').next().unwrap_or(&path).to_string();
                let name = Name { sym: self.interner.intern(&last), span: t.span() };
                let path_sym = self.interner.intern(&path);
                let id = self.push_item(ItemKind::Import { path: path_sym, path_span: t.span() }, name, lo, is_pub);
                self.end_item()?;
                Ok(id)
            }
            T::Fn => {
                let id = self.parse_fn(lo, is_pub, None, true)?;
                Ok(id)
            }
            T::Step => {
                self.bump();
                let t = self.expect(T::Str)?;
                let raw = self.text(t);
                let pat = lexer::unescape(&raw[1..raw.len().saturating_sub(1).max(1)]);
                let pat_sym = self.interner.intern(&pat);
                self.skip_semis();
                let fn_lo = self.tok().lo;
                let fn_pub = self.eat(T::Pub);
                if !self.at(T::Fn) {
                    self.expected("`fn` after the step pattern");
                    return Err(());
                }
                self.parse_fn(fn_lo.min(lo), fn_pub || is_pub, Some((pat_sym, t.span())), true)
            }
            T::Struct => self.parse_struct(lo, is_pub),
            T::Enum => self.parse_enum(lo, is_pub),
            T::Interface => self.parse_interface(lo, is_pub),
            T::Type => {
                self.bump();
                let name = self.expect_ident("a type name")?;
                let generics = self.parse_generic_params_opt()?;
                self.expect(T::Eq)?;
                let mut ty = self.parse_type()?;
                if self.at(T::Where) {
                    self.bump();
                    let pred = self.with_no_struct(|p| p.parse_expr())?;
                    let span = self.span_from(self.ast.types[ty].span.lo);
                    ty = self.mk_type(TypeExprKind::Refined { base: ty, pred }, span);
                }
                let id = self.push_item(ItemKind::TypeAlias { generics, ty }, name, lo, is_pub);
                self.end_item()?;
                Ok(id)
            }
            T::Const => {
                self.bump();
                let name = self.expect_ident("a constant name")?;
                self.expect(T::Colon)?;
                let ty = self.parse_type()?;
                self.expect(T::Eq)?;
                let value = self.parse_expr()?;
                let id = self.push_item(ItemKind::Const { ty, value }, name, lo, is_pub);
                self.end_item()?;
                Ok(id)
            }
            T::Unit => {
                self.bump();
                let name = self.expect_ident("a unit name")?;
                let id = self.push_item(ItemKind::Unit, name, lo, is_pub);
                self.end_item()?;
                Ok(id)
            }
            T::Test => {
                self.bump();
                let t = self.expect(T::Str)?;
                let raw = self.text(t);
                let label = lexer::unescape(&raw[1..raw.len().saturating_sub(1).max(1)]);
                let label = self.interner.intern(&label);
                let params =
                    if self.at(T::LParen) { self.parse_params(false)?.1 } else { List::EMPTY };
                let mut requires = List::EMPTY;
                if self.skip_semis_before(T::Requires) {
                    self.bump();
                    requires = self.parse_clause_exprs()?;
                }
                self.skip_semis_before(T::LBrace);
                let body = self.parse_block()?;
                let name = Name { sym: label, span: t.span() };
                let id = self.push_item(ItemKind::Test { label, params, requires, body }, name, lo, is_pub);
                Ok(id)
            }
            _ => {
                let t = self.tok();
                self.diags.push(
                    Diagnostic::error(codes::E0108, t.span(), format!("expected an item, found {}", match t.kind {
                        T::Ident => format!("`{}`", self.text(t)),
                        k => k.describe().to_string(),
                    }))
                    .note("top-level declarations are `import`, `fn`, `struct`, `enum`, `interface`, `type`, `const`, `unit`, `test` and `step`"),
                );
                Err(())
            }
        }
    }

    fn end_item(&mut self) -> PResult<()> {
        if self.at(T::Semi) || self.at(T::Eof) {
            self.skip_semis();
            Ok(())
        } else {
            self.expected("end of line");
            Err(())
        }
    }

    fn with_no_struct<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let save = self.no_struct;
        self.no_struct = true;
        let r = f(self);
        self.no_struct = save;
        r
    }

    fn with_struct<R>(&mut self, f: impl FnOnce(&mut Self) -> R) -> R {
        let save = self.no_struct;
        self.no_struct = false;
        let r = f(self);
        self.no_struct = save;
        r
    }

    fn parse_generic_params_opt(&mut self) -> PResult<List<GenericParam>> {
        if !self.at(T::Lt) {
            return Ok(List::EMPTY);
        }
        self.bump();
        let mark = self.ast.generics.mark();
        loop {
            if self.close_angle() {
                break;
            }
            let name = self.expect_ident("a type parameter")?;
            let mut bounds = List::EMPTY;
            if self.eat(T::Colon) {
                let bm = self.ast.type_lists.mark();
                loop {
                    let b = self.parse_type()?;
                    self.ast.type_lists.push_scratch(b);
                    if !self.eat(T::Plus) {
                        break;
                    }
                }
                bounds = self.ast.type_lists.finish(bm);
            }
            self.ast.generics.push_scratch(GenericParam { name, bounds });
            if self.eat(T::Comma) {
                continue;
            }
            if self.close_angle() {
                break;
            }
            self.expected("`,` or `>`");
            return Err(());
        }
        Ok(self.ast.generics.finish(mark))
    }

    /// Consume a `>` (splitting `>>` if needed). Returns false if not at one.
    fn close_angle(&mut self) -> bool {
        match self.peek() {
            T::Gt => {
                self.bump();
                true
            }
            T::Shr => {
                let t = self.tok();
                self.toks[self.pos] = Token { kind: T::Gt, lo: t.lo + 1, hi: t.hi };
                self.prev_hi = t.lo + 1;
                true
            }
            T::Ge | T::ShrEq => {
                let t = self.tok();
                let kind = if t.kind == T::Ge { T::Eq } else { T::Ge };
                self.toks[self.pos] = Token { kind, lo: t.lo + 1, hi: t.hi };
                self.prev_hi = t.lo + 1;
                true
            }
            _ => false,
        }
    }

    fn parse_fn(
        &mut self,
        lo: u32,
        is_pub: bool,
        step: Option<(Symbol, Span)>,
        need_body: bool,
    ) -> PResult<ItemId> {
        self.expect(T::Fn)?;
        let first = self.expect_ident("a function name")?;
        let first_generics = self.parse_generic_params_opt()?;
        let (recv, name, generics) = if self.at(T::Dot) {
            self.bump();
            let name = self.expect_ident("a method name")?;
            let generics = self.parse_generic_params_opt()?;
            (Some(Receiver { ty: first, generics: first_generics }), name, generics)
        } else {
            (None, first, first_generics)
        };
        let (self_param, params) = self.parse_params(true)?;
        let mut ret = None;
        if self.eat(T::Arrow) {
            ret = Some(self.parse_type()?);
        }
        let sig_span = self.span_from(lo);
        let (requires, ensures, effects, effects_span) = self.parse_clauses()?;
        let mut body = None;
        if self.skip_semis_before(T::LBrace) {
            body = Some(self.parse_block()?);
        } else if need_body {
            self.expected("`{` to start the function body");
            return Err(());
        }
        let decl = FnDecl {
            recv,
            self_param,
            generics,
            params,
            ret,
            requires,
            ensures,
            effects,
            effects_span,
            body,
            step,
            sig_span,
        };
        Ok(self.push_item(ItemKind::Fn(decl), name, lo, is_pub))
    }

    fn parse_clause_exprs(&mut self) -> PResult<List<ExprId>> {
        let mark = self.ast.expr_lists.mark();
        loop {
            let e = self.with_no_struct(|p| p.parse_expr())?;
            self.ast.expr_lists.push_scratch(e);
            if !self.eat(T::Comma) {
                break;
            }
        }
        Ok(self.ast.expr_lists.finish(mark))
    }

    #[allow(clippy::type_complexity)]
    fn parse_clauses(&mut self) -> PResult<(List<ExprId>, List<ExprId>, Option<List<Name>>, Span)> {
        let rm = self.ast.expr_lists.mark();
        let mut req: Vec<ExprId> = Vec::new();
        let mut ens: Vec<ExprId> = Vec::new();
        let mut effects = None;
        let mut effects_span = Span::DUMMY;
        loop {
            if !self.skip_semis_before_any(&[T::Requires, T::Ensures, T::Effects]) {
                break;
            }
            match self.peek() {
                T::Requires | T::Ensures => {
                    let is_req = self.bump().kind == T::Requires;
                    loop {
                        let e = self.with_no_struct(|p| p.parse_expr())?;
                        if is_req {
                            req.push(e)
                        } else {
                            ens.push(e)
                        }
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                }
                T::Effects => {
                    let t = self.bump();
                    let (list, span) = self.parse_effect_set(t.lo)?;
                    if effects.is_some() {
                        self.error_at(span, "duplicate `effects` clause");
                    }
                    effects = Some(list);
                    effects_span = span;
                }
                _ => unreachable!(),
            }
        }
        debug_assert_eq!(rm, self.ast.expr_lists.mark());
        let requires = self.ast.expr_lists.alloc(&req);
        let ensures = self.ast.expr_lists.alloc(&ens);
        Ok((requires, ensures, effects, effects_span))
    }

    /// After the `effects` keyword: `{io, fs}`.
    fn parse_effect_set(&mut self, lo: u32) -> PResult<(List<Name>, Span)> {
        self.expect(T::LBrace)?;
        let mark = self.ast.names.mark();
        loop {
            self.skip_semis();
            if self.eat(T::RBrace) {
                break;
            }
            let n = self.expect_ident("an effect name")?;
            self.ast.names.push_scratch(n);
            self.skip_semis();
            if self.eat(T::Comma) {
                continue;
            }
            self.skip_semis();
            if self.eat(T::RBrace) {
                break;
            }
            self.expected("`,` or `}`");
            return Err(());
        }
        Ok((self.ast.names.finish(mark), self.span_from(lo)))
    }

    /// `(self, a: T, b: &mut U where pred)`. Closures (`allow_self == false` and untyped)
    /// may omit types.
    fn parse_params(&mut self, allow_self: bool) -> PResult<(Option<SelfParam>, List<Param>)> {
        self.expect(T::LParen)?;
        let mut self_param = None;
        let mark = self.ast.params.mark();
        let mut first = true;
        loop {
            if self.eat(T::RParen) {
                break;
            }
            let lo = self.tok().lo;
            // self / &mut self
            if first && allow_self {
                if self.tok_is_ident("self") {
                    let t = self.bump();
                    self_param = Some(SelfParam { mutable: false, span: t.span() });
                } else if self.at(T::Amp) && self.ident_at(1, "mut") && self.ident_at(2, "self") {
                    self.bump();
                    self.bump();
                    self.bump();
                    self_param = Some(SelfParam { mutable: true, span: self.span_from(lo) });
                }
            }
            if self_param.is_none() || !first {
                let name = self.expect_ident("a parameter name")?;
                let mut ty = None;
                let mut mut_ref = false;
                let mut refine = None;
                if self.eat(T::Colon) {
                    if self.at(T::Amp) && self.ident_at(1, "mut") {
                        self.bump();
                        self.bump();
                        mut_ref = true;
                    }
                    ty = Some(self.parse_type()?);
                    if self.eat(T::Where) {
                        refine = Some(self.with_no_struct(|p| p.parse_expr())?);
                    }
                } else if allow_self {
                    self.expected("`:` and a type");
                    return Err(());
                }
                let span = self.span_from(lo);
                self.ast.params.push_scratch(Param { name, ty, mut_ref, refine, span });
            }
            first = false;
            if self.eat(T::Comma) {
                continue;
            }
            if self.eat(T::RParen) {
                break;
            }
            self.expected("`,` or `)`");
            return Err(());
        }
        Ok((self_param, self.ast.params.finish(mark)))
    }

    fn parse_struct(&mut self, lo: u32, is_pub: bool) -> PResult<ItemId> {
        self.bump();
        let name = self.expect_ident("a struct name")?;
        let generics = self.parse_generic_params_opt()?;
        self.expect(T::LBrace)?;
        let fm = self.ast.fields.mark();
        let mut invs: Vec<ExprId> = Vec::new();
        loop {
            self.skip_semis();
            if self.eat(T::RBrace) {
                break;
            }
            if self.at(T::Invariant) {
                self.bump();
                loop {
                    let e = self.with_no_struct(|p| p.parse_expr())?;
                    invs.push(e);
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
            } else {
                let f = self.parse_field_decl(true)?;
                self.ast.fields.push_scratch(f);
            }
            if self.eat(T::Comma) || self.at(T::Semi) {
                continue;
            }
            if self.eat(T::RBrace) {
                break;
            }
            self.expected("a line break or `}`");
            return Err(());
        }
        let fields = self.ast.fields.finish(fm);
        let invariants = self.ast.expr_lists.alloc(&invs);
        let id = self.push_item(ItemKind::Struct { generics, fields, invariants }, name, lo, is_pub);
        self.end_item()?;
        Ok(id)
    }

    fn parse_field_decl(&mut self, allow_pub: bool) -> PResult<FieldDecl> {
        let lo = self.tok().lo;
        let is_pub = allow_pub && self.eat(T::Pub);
        let name = self.expect_ident("a field name")?;
        self.expect(T::Colon)?;
        let ty = self.parse_type()?;
        let mut refine = None;
        if self.eat(T::Where) {
            refine = Some(self.with_no_struct(|p| p.parse_expr())?);
        }
        Ok(FieldDecl { name, ty, is_pub, refine, span: self.span_from(lo) })
    }

    fn parse_enum(&mut self, lo: u32, is_pub: bool) -> PResult<ItemId> {
        self.bump();
        let name = self.expect_ident("an enum name")?;
        let generics = self.parse_generic_params_opt()?;
        self.expect(T::LBrace)?;
        let vm = self.ast.variants.mark();
        loop {
            self.skip_semis();
            if self.eat(T::RBrace) {
                break;
            }
            let vlo = self.tok().lo;
            let vname = self.expect_ident("a variant name")?;
            let mut fields = List::EMPTY;
            let mut has_parens = false;
            if self.eat(T::LParen) {
                has_parens = true;
                let fm = self.ast.fields.mark();
                let mut idx = 0;
                loop {
                    if self.eat(T::RParen) {
                        break;
                    }
                    let flo = self.tok().lo;
                    let f = if self.at(T::Ident) && self.peek_at(1) == T::Colon {
                        self.parse_field_decl(false)?
                    } else {
                        let ty = self.parse_type()?;
                        let sym = self.interner.intern(&format!("_{idx}"));
                        let span = self.span_from(flo);
                        FieldDecl { name: Name { sym, span }, ty, is_pub: false, refine: None, span }
                    };
                    idx += 1;
                    self.ast.fields.push_scratch(f);
                    if self.eat(T::Comma) {
                        continue;
                    }
                    if self.eat(T::RParen) {
                        break;
                    }
                    self.expected("`,` or `)`");
                    return Err(());
                }
                fields = self.ast.fields.finish(fm);
            }
            let span = self.span_from(vlo);
            self.ast.variants.push_scratch(VariantDecl { name: vname, fields, has_parens, span });
            if self.eat(T::Comma) || self.at(T::Semi) {
                continue;
            }
            if self.eat(T::RBrace) {
                break;
            }
            self.expected("a line break or `}`");
            return Err(());
        }
        let variants = self.ast.variants.finish(vm);
        let id = self.push_item(ItemKind::Enum { generics, variants }, name, lo, is_pub);
        self.end_item()?;
        Ok(id)
    }

    fn parse_interface(&mut self, lo: u32, is_pub: bool) -> PResult<ItemId> {
        self.bump();
        let name = self.expect_ident("an interface name")?;
        let generics = self.parse_generic_params_opt()?;
        self.expect(T::LBrace)?;
        let mut methods: Vec<ItemId> = Vec::new();
        loop {
            self.skip_semis();
            if self.eat(T::RBrace) {
                break;
            }
            let mlo = self.tok().lo;
            let m = self.parse_fn(mlo, true, None, false)?;
            methods.push(m);
        }
        let methods = self.ast.item_lists.alloc(&methods);
        let id = self.push_item(ItemKind::Interface { generics, methods }, name, lo, is_pub);
        self.end_item()?;
        Ok(id)
    }

    // ---------- types ----------

    fn parse_type(&mut self) -> PResult<TypeExprId> {
        let lo = self.tok().lo;
        match self.peek() {
            T::LParen => {
                self.bump();
                let mark = self.ast.type_lists.mark();
                let mut trailing_comma = false;
                let mut n = 0;
                loop {
                    if self.eat(T::RParen) {
                        break;
                    }
                    let t = self.parse_type()?;
                    self.ast.type_lists.push_scratch(t);
                    n += 1;
                    trailing_comma = false;
                    if self.eat(T::Comma) {
                        trailing_comma = true;
                        continue;
                    }
                    self.expect(T::RParen)?;
                    break;
                }
                let list = self.ast.type_lists.finish(mark);
                if n == 1 && !trailing_comma {
                    // parenthesized type
                    return Ok(self.ast.type_lists.get(list)[0]);
                }
                Ok(self.mk_type(TypeExprKind::Tuple(list), self.span_from(lo)))
            }
            T::Fn => {
                self.bump();
                self.expect(T::LParen)?;
                let mark = self.ast.type_lists.mark();
                loop {
                    if self.eat(T::RParen) {
                        break;
                    }
                    let t = self.parse_type()?;
                    self.ast.type_lists.push_scratch(t);
                    if self.eat(T::Comma) {
                        continue;
                    }
                    self.expect(T::RParen)?;
                    break;
                }
                let params = self.ast.type_lists.finish(mark);
                let mut ret = None;
                if self.eat(T::Arrow) {
                    ret = Some(self.parse_type()?);
                }
                let mut effects = None;
                if self.at(T::Effects) {
                    let t = self.bump();
                    effects = Some(self.parse_effect_set(t.lo)?.0);
                }
                Ok(self.mk_type(TypeExprKind::Fn { params, ret, effects }, self.span_from(lo)))
            }
            T::Ident => {
                let first = self.expect_ident("a type")?;
                let (pkg, name) = if self.at(T::Dot) && self.peek_at(1) == T::Ident {
                    self.bump();
                    let n = self.expect_ident("a type name")?;
                    (Some(first), n)
                } else {
                    (None, first)
                };
                let mut args = List::EMPTY;
                let mut unit = None;
                if self.at(T::Lt) {
                    self.bump();
                    if pkg.is_none() && is_numeric_type_name(name.sym) {
                        unit = Some(self.parse_unit_expr()?);
                    } else {
                        args = self.parse_type_args_after_lt()?;
                    }
                }
                Ok(self.mk_type(TypeExprKind::Named { pkg, name, args, unit }, self.span_from(lo)))
            }
            T::Amp => {
                let t = self.tok();
                self.diags.push(
                    Diagnostic::error(
                        codes::E0314,
                        t.span(),
                        "`&mut` is only allowed on function parameters",
                    )
                    .note("Jig has no reference types; values are copied, and `&mut` parameters mutate the caller's variable"),
                );
                self.bump();
                if self.tok_is_ident("mut") {
                    self.bump();
                }
                self.parse_type()
            }
            _ => {
                self.expected("a type");
                Err(())
            }
        }
    }

    fn parse_type_args_after_lt(&mut self) -> PResult<List<TypeExprId>> {
        let mark = self.ast.type_lists.mark();
        loop {
            if self.close_angle() {
                break;
            }
            let t = self.parse_type()?;
            self.ast.type_lists.push_scratch(t);
            if self.eat(T::Comma) {
                continue;
            }
            if self.close_angle() {
                break;
            }
            self.expected("`,` or `>`");
            return Err(());
        }
        Ok(self.ast.type_lists.finish(mark))
    }

    /// After `Float<`: `m`, `m/s^2`, `kg*m`, `1/s`, then `>`.
    fn parse_unit_expr(&mut self) -> PResult<List<UnitFactor>> {
        let mut factors: Vec<UnitFactor> = Vec::new();
        let mut sign: i8 = 1;
        let mut first = true;
        loop {
            if self.close_angle() {
                break;
            }
            if !first {
                if self.eat(T::Star) {
                    sign = 1;
                } else if self.eat(T::Slash) {
                    sign = -1;
                } else {
                    self.expected("`*`, `/` or `>` in a unit");
                    return Err(());
                }
            }
            if first && self.at(T::Int) && self.text(self.tok()) == "1" {
                self.bump();
                first = false;
                continue;
            }
            first = false;
            let name = self.expect_ident("a unit name")?;
            let mut exp: i8 = 1;
            if self.eat(T::Caret) {
                let neg = self.eat(T::Minus);
                let t = self.expect(T::Int)?;
                let v: i8 = self.text(t).parse().unwrap_or(1);
                exp = if neg { -v } else { v };
            }
            factors.push(UnitFactor { name, exp: exp * sign });
        }
        Ok(self.ast.unit_factors.alloc(&factors))
    }

    // ---------- statements ----------

    fn parse_block(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        self.expect(T::LBrace)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mark = self.ast.stmt_lists.mark();
        loop {
            self.skip_semis();
            if self.at(T::RBrace) || self.at(T::Eof) {
                break;
            }
            let before = self.pos;
            match self.parse_stmt() {
                Ok(s) => {
                    self.ast.stmt_lists.push_scratch(s);
                    if !(self.at(T::Semi) || self.at(T::RBrace)) {
                        self.expected("end of line");
                        self.recover_stmt();
                    }
                }
                Err(()) => {
                    if self.pos == before {
                        self.bump();
                    }
                    self.recover_stmt();
                }
            }
        }
        self.no_struct = saved;
        let stmts = self.ast.stmt_lists.finish(mark);
        if self.expect(T::RBrace).is_err() {
            return Err(());
        }
        Ok(self.mk_expr(ExprKind::Block(stmts), self.span_from(lo)))
    }

    fn parse_stmt(&mut self) -> PResult<StmtId> {
        let lo = self.tok().lo;
        match self.peek() {
            T::Let | T::Var => {
                let mutable = self.bump().kind == T::Var;
                let pat = self.parse_pat()?;
                let mut ty = None;
                if self.eat(T::Colon) {
                    ty = Some(self.parse_type()?);
                }
                if !self.at(T::Eq) {
                    self.expected("`=` and an initial value (Jig has no default values)");
                    return Err(());
                }
                self.bump();
                let init = self.parse_expr()?;
                Ok(self.mk_stmt(StmtKind::Let { pat, ty, init, mutable }, self.span_from(lo)))
            }
            T::Return => {
                self.bump();
                let value = if self.at(T::Semi) || self.at(T::RBrace) { None } else { Some(self.parse_expr()?) };
                Ok(self.mk_stmt(StmtKind::Return(value), self.span_from(lo)))
            }
            T::Break => {
                self.bump();
                Ok(self.mk_stmt(StmtKind::Break, self.span_from(lo)))
            }
            T::Continue => {
                self.bump();
                Ok(self.mk_stmt(StmtKind::Continue, self.span_from(lo)))
            }
            T::For => self.parse_for(),
            _ => {
                let e = self.parse_expr()?;
                let op = match self.peek() {
                    T::Eq => Some(None),
                    T::PlusEq => Some(Some(BinOp::Add)),
                    T::MinusEq => Some(Some(BinOp::Sub)),
                    T::StarEq => Some(Some(BinOp::Mul)),
                    T::SlashEq => Some(Some(BinOp::Div)),
                    T::PercentEq => Some(Some(BinOp::Rem)),
                    T::AmpEq => Some(Some(BinOp::BitAnd)),
                    T::PipeEq => Some(Some(BinOp::BitOr)),
                    T::CaretEq => Some(Some(BinOp::BitXor)),
                    T::ShlEq => Some(Some(BinOp::Shl)),
                    T::ShrEq => Some(Some(BinOp::Shr)),
                    _ => None,
                };
                if let Some(op) = op {
                    self.bump();
                    let value = self.parse_expr()?;
                    return Ok(self.mk_stmt(StmtKind::Assign { target: e, op, value }, self.span_from(lo)));
                }
                Ok(self.mk_stmt(StmtKind::Expr(e), self.span_from(lo)))
            }
        }
    }

    fn is_for_in(&self) -> bool {
        match self.peek() {
            T::Ident => self.peek_at(1) == T::In,
            T::LParen => {
                let mut depth = 0;
                let mut i = self.pos;
                while i < self.toks.len() {
                    match self.toks[i].kind {
                        T::LParen => depth += 1,
                        T::RParen => {
                            depth -= 1;
                            if depth == 0 {
                                return self.toks.get(i + 1).is_some_and(|t| t.kind == T::In);
                            }
                        }
                        T::LBrace | T::Semi | T::Eof => return false,
                        _ => {}
                    }
                    i += 1;
                }
                false
            }
            _ => false,
        }
    }

    fn parse_for(&mut self) -> PResult<StmtId> {
        let lo = self.tok().lo;
        self.bump();
        let head = if self.at(T::LBrace) || self.at(T::Invariant) {
            ForHead::Infinite
        } else if self.is_for_in() {
            let pat = self.parse_pat()?;
            self.expect(T::In)?;
            let iter = self.with_no_struct(|p| p.parse_range_or_expr())?;
            ForHead::In { pat, iter }
        } else {
            let cond = self.with_no_struct(|p| p.parse_expr())?;
            ForHead::Cond(cond)
        };
        let mut invariants = List::EMPTY;
        if self.skip_semis_before(T::Invariant) {
            self.bump();
            invariants = self.parse_clause_exprs()?;
        }
        self.skip_semis_before(T::LBrace);
        let body = self.parse_block()?;
        Ok(self.mk_stmt(StmtKind::For { head, invariants, body }, self.span_from(lo)))
    }

    /// `a..b` or a plain expression.
    fn parse_range_or_expr(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        if self.eat(T::DotDot) {
            let hi = self.parse_expr()?;
            return Ok(self.mk_expr(ExprKind::Range { lo: None, hi: Some(hi) }, self.span_from(lo)));
        }
        let e = self.parse_expr()?;
        if self.eat(T::DotDot) {
            let hi = self.parse_expr()?;
            return Ok(self.mk_expr(ExprKind::Range { lo: Some(e), hi: Some(hi) }, self.span_from(lo)));
        }
        Ok(e)
    }

    // ---------- patterns ----------

    fn parse_pat(&mut self) -> PResult<PatId> {
        let lo = self.tok().lo;
        let first = self.parse_pat_primary()?;
        if !self.at(T::Pipe) {
            return Ok(first);
        }
        let mark = self.ast.pat_lists.mark();
        self.ast.pat_lists.push_scratch(first);
        while self.eat(T::Pipe) {
            let p = self.parse_pat_primary()?;
            self.ast.pat_lists.push_scratch(p);
        }
        let list = self.ast.pat_lists.finish(mark);
        Ok(self.mk_pat(PatKind::Or(list), self.span_from(lo)))
    }

    fn parse_pat_primary(&mut self) -> PResult<PatId> {
        let lo = self.tok().lo;
        match self.peek() {
            T::Ident => {
                let t = self.bump();
                let text = self.text(t);
                if text == "_" {
                    return Ok(self.mk_pat(PatKind::Wild, t.span()));
                }
                let s = self.sym(t);
                if text.starts_with(|c: char| c.is_ascii_uppercase()) {
                    let name = Name { sym: s, span: t.span() };
                    if self.at(T::Dot) && self.peek_at(1) == T::Ident {
                        self.error_at(
                            t.span(),
                            "patterns name variants without the enum: write `Variant(...)`, not `Enum.Variant(...)`",
                        );
                        self.bump();
                        let n = self.expect_ident("a variant")?;
                        return self.finish_variant_pat(lo, n);
                    }
                    return self.finish_variant_pat(lo, name);
                }
                Ok(self.mk_pat(PatKind::Bind(s), t.span()))
            }
            T::Int | T::Float | T::Str | T::Char | T::True | T::False => {
                let e = self.parse_primary()?;
                Ok(self.mk_pat(PatKind::Lit(e), self.span_from(lo)))
            }
            T::Minus => {
                let mt = self.bump();
                if !matches!(self.peek(), T::Int | T::Float) {
                    self.expected("a number after `-`");
                    return Err(());
                }
                let inner = self.parse_primary()?;
                let span = self.span_from(mt.lo);
                let e = self.mk_expr(ExprKind::Unary { op: UnOp::Neg, expr: inner }, span);
                Ok(self.mk_pat(PatKind::Lit(e), span))
            }
            T::LParen => {
                self.bump();
                let mark = self.ast.pat_lists.mark();
                let mut n = 0;
                let mut trailing = false;
                loop {
                    if self.eat(T::RParen) {
                        break;
                    }
                    let p = self.parse_pat()?;
                    self.ast.pat_lists.push_scratch(p);
                    n += 1;
                    trailing = false;
                    if self.eat(T::Comma) {
                        trailing = true;
                        continue;
                    }
                    self.expect(T::RParen)?;
                    break;
                }
                let list = self.ast.pat_lists.finish(mark);
                if n == 1 && !trailing {
                    return Ok(self.ast.pat_lists.get(list)[0]);
                }
                Ok(self.mk_pat(PatKind::Tuple(list), self.span_from(lo)))
            }
            _ => {
                self.expected("a pattern");
                Err(())
            }
        }
    }

    fn finish_variant_pat(&mut self, lo: u32, name: Name) -> PResult<PatId> {
        let mut args = List::EMPTY;
        let mut has_parens = false;
        if self.eat(T::LParen) {
            has_parens = true;
            let mark = self.ast.pat_lists.mark();
            loop {
                if self.eat(T::RParen) {
                    break;
                }
                let p = self.parse_pat()?;
                self.ast.pat_lists.push_scratch(p);
                if self.eat(T::Comma) {
                    continue;
                }
                self.expect(T::RParen)?;
                break;
            }
            args = self.ast.pat_lists.finish(mark);
        }
        Ok(self.mk_pat(PatKind::Variant { name, args, has_parens }, self.span_from(lo)))
    }

    // ---------- expressions ----------

    pub fn parse_expr(&mut self) -> PResult<ExprId> {
        self.parse_binary(1)
    }

    fn cur_binop(&self) -> Option<BinOp> {
        Some(match self.peek() {
            T::Star => BinOp::Mul,
            T::Slash => BinOp::Div,
            T::Percent => BinOp::Rem,
            T::Shl => BinOp::Shl,
            T::Shr => BinOp::Shr,
            T::Amp => BinOp::BitAnd,
            T::Plus => BinOp::Add,
            T::Minus => BinOp::Sub,
            T::Pipe => BinOp::BitOr,
            T::Caret => BinOp::BitXor,
            T::EqEq => BinOp::Eq,
            T::Ne => BinOp::Ne,
            T::Lt => BinOp::Lt,
            T::Le => BinOp::Le,
            T::Gt => BinOp::Gt,
            T::Ge => BinOp::Ge,
            T::AmpAmp => BinOp::And,
            T::PipePipe => BinOp::Or,
            _ => return None,
        })
    }

    fn parse_binary(&mut self, min_prec: u8) -> PResult<ExprId> {
        let lo = self.tok().lo;
        let mut lhs = self.parse_cast()?;
        while let Some(op) = self.cur_binop() {
            let prec = op.precedence();
            if prec < min_prec {
                break;
            }
            self.bump();
            // An operand is expected: a line break here does not end the statement.
            while self.tok().is_inserted_semi() {
                self.bump();
            }
            let rhs = self.parse_binary(prec + 1)?;
            lhs = self.mk_expr(ExprKind::Binary { op, lhs, rhs }, self.span_from(lo));
        }
        Ok(lhs)
    }

    fn parse_cast(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        let mut e = self.parse_unary()?;
        while self.at(T::As) {
            self.bump();
            let ty = self.parse_type()?;
            e = self.mk_expr(ExprKind::Cast { expr: e, ty }, self.span_from(lo));
        }
        Ok(e)
    }

    fn parse_unary(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        let op = match self.peek() {
            T::Minus => Some(UnOp::Neg),
            T::Bang => Some(UnOp::Not),
            T::Tilde => Some(UnOp::BitNot),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            let e = self.parse_unary()?;
            return Ok(self.mk_expr(ExprKind::Unary { op, expr: e }, self.span_from(lo)));
        }
        let e = self.parse_primary()?;
        self.parse_postfix(e, lo)
    }

    /// Is the expression a path that can name a type (for struct literals)?
    fn expr_as_type_path(&self, e: ExprId) -> bool {
        match self.ast.exprs[e].kind {
            ExprKind::Name(s) => is_type_like(s.as_str()),
            ExprKind::Field { base, name } => {
                matches!(self.ast.exprs[base].kind, ExprKind::Name(_)) && is_type_like(name.sym.as_str())
            }
            ExprKind::TypeArgs { base, .. } => matches!(
                self.ast.exprs[base].kind,
                ExprKind::Name(_) | ExprKind::Field { .. }
            ),
            _ => false,
        }
    }

    fn expr_to_type(&mut self, e: ExprId) -> TypeExprId {
        let span = self.ast.exprs[e].span;
        match self.ast.exprs[e].kind {
            ExprKind::Name(s) => self.mk_type(
                TypeExprKind::Named { pkg: None, name: Name { sym: s, span }, args: List::EMPTY, unit: None },
                span,
            ),
            ExprKind::Field { base, name } => {
                let pkg = match self.ast.exprs[base].kind {
                    ExprKind::Name(p) => Some(Name { sym: p, span: self.ast.exprs[base].span }),
                    _ => None,
                };
                self.mk_type(TypeExprKind::Named { pkg, name, args: List::EMPTY, unit: None }, span)
            }
            ExprKind::TypeArgs { base, args } => {
                let t = self.expr_to_type(base);
                if let TypeExprKind::Named { pkg, name, .. } = self.ast.types[t].kind {
                    self.ast.types[t].kind = TypeExprKind::Named { pkg, name, args, unit: None };
                    self.ast.types[t].span = span;
                }
                t
            }
            _ => self.mk_type(TypeExprKind::Error, span),
        }
    }

    /// C#-style disambiguation: `<` starts type arguments if a balanced `<...>` made only of
    /// type tokens is followed by one of `( . { ) , ] ;` or a line end.
    fn looks_like_type_args(&self) -> bool {
        let mut i = self.pos;
        let mut depth: i32 = 0;
        let mut parens = 0;
        while i < self.toks.len() {
            match self.toks[i].kind {
                T::Lt => depth += 1,
                T::Gt => depth -= 1,
                T::Shr => depth -= 2,
                T::LParen => parens += 1,
                T::RParen => {
                    parens -= 1;
                    if parens < 0 {
                        return false;
                    }
                }
                T::Ident | T::Comma | T::Dot | T::Fn | T::Arrow | T::Slash | T::Caret | T::Star | T::Int => {}
                _ => return false,
            }
            i += 1;
            if depth == 0 {
                break;
            }
            if depth < 0 {
                return false;
            }
        }
        if depth != 0 || parens != 0 {
            return false;
        }
        matches!(
            self.toks.get(i).map(|t| t.kind),
            Some(T::LParen | T::Dot | T::LBrace | T::RParen | T::Comma | T::RBracket | T::Semi | T::RBrace | T::Eof)
        )
    }

    fn parse_postfix(&mut self, mut e: ExprId, lo: u32) -> PResult<ExprId> {
        loop {
            match self.peek() {
                T::LParen => {
                    let args = self.parse_call_args()?;
                    e = self.mk_expr(ExprKind::Call { callee: e, args }, self.span_from(lo));
                }
                T::LBracket => {
                    self.bump();
                    let index = self.with_struct(|p| p.parse_range_or_expr())?;
                    self.expect(T::RBracket)?;
                    e = self.mk_expr(ExprKind::Index { base: e, index }, self.span_from(lo));
                }
                T::Dot => {
                    self.bump();
                    // A line break may separate a chained call from its receiver.
                    match self.peek() {
                        T::Ident => {
                            let name = self.expect_ident("a field or method name")?;
                            e = self.mk_expr(ExprKind::Field { base: e, name }, self.span_from(lo));
                        }
                        T::Int => {
                            let t = self.bump();
                            let index: u32 = self.text(t).parse().unwrap_or(0);
                            e = self.mk_expr(
                                ExprKind::TupleField { base: e, index, index_span: t.span() },
                                self.span_from(lo),
                            );
                        }
                        _ => {
                            self.expected("a field or method name");
                            return Err(());
                        }
                    }
                }
                T::Question => {
                    self.bump();
                    e = self.mk_expr(ExprKind::Try(e), self.span_from(lo));
                }
                T::Lt if matches!(self.ast.exprs[e].kind, ExprKind::Name(_) | ExprKind::Field { .. })
                    && self.looks_like_type_args() =>
                {
                    self.bump();
                    let args = self.parse_type_args_after_lt()?;
                    e = self.mk_expr(ExprKind::TypeArgs { base: e, args }, self.span_from(lo));
                }
                T::LBrace if !self.no_struct && self.expr_as_type_path(e) => {
                    let ty = self.expr_to_type(e);
                    let fields = self.parse_struct_lit_fields()?;
                    e = self.mk_expr(ExprKind::StructLit { ty, fields }, self.span_from(lo));
                }
                _ => return Ok(e),
            }
        }
    }

    fn parse_struct_lit_fields(&mut self) -> PResult<List<FieldInit>> {
        self.expect(T::LBrace)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mark = self.ast.field_inits.mark();
        let r = (|| {
            loop {
                self.skip_semis();
                if self.eat(T::RBrace) {
                    break;
                }
                let name = self.expect_ident("a field name")?;
                if !self.at(T::Colon) {
                    self.expected("`:` and a value (write `field: value`)");
                    return Err(());
                }
                self.bump();
                let value = self.parse_expr()?;
                self.ast.field_inits.push_scratch(FieldInit { name, value });
                if self.eat(T::Comma) || self.at(T::Semi) {
                    continue;
                }
                self.skip_semis();
                if self.eat(T::RBrace) {
                    break;
                }
                self.expected("`,` or `}`");
                return Err(());
            }
            Ok(())
        })();
        self.no_struct = saved;
        r?;
        Ok(self.ast.field_inits.finish(mark))
    }

    fn parse_call_args(&mut self) -> PResult<List<ExprId>> {
        self.expect(T::LParen)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mark = self.ast.expr_lists.mark();
        let r = (|| {
            loop {
                if self.eat(T::RParen) {
                    break;
                }
                let lo = self.tok().lo;
                let arg = if self.at(T::Amp) && self.ident_at(1, "mut") {
                    self.bump();
                    self.bump();
                    let place = self.parse_expr()?;
                    self.mk_expr(ExprKind::MutArg(place), self.span_from(lo))
                } else {
                    self.parse_expr()?
                };
                self.ast.expr_lists.push_scratch(arg);
                if self.eat(T::Comma) {
                    continue;
                }
                if self.eat(T::RParen) {
                    break;
                }
                self.expected("`,` or `)`");
                return Err(());
            }
            Ok(())
        })();
        self.no_struct = saved;
        r?;
        Ok(self.ast.expr_lists.finish(mark))
    }

    fn parse_primary(&mut self) -> PResult<ExprId> {
        let t = self.tok();
        let lo = t.lo;
        match t.kind {
            T::Int => {
                self.bump();
                let text = self.text(t).replace('_', "");
                let v = if let Some(h) = text.strip_prefix("0x") {
                    u64::from_str_radix(h, 16)
                } else if let Some(b) = text.strip_prefix("0b") {
                    u64::from_str_radix(b, 2)
                } else if let Some(o) = text.strip_prefix("0o") {
                    u64::from_str_radix(o, 8)
                } else {
                    text.parse::<u64>()
                };
                let v = match v {
                    Ok(v) => v,
                    Err(_) => {
                        self.diags.push(Diagnostic::error(codes::E0104, t.span(), "integer literal is too large"));
                        0
                    }
                };
                Ok(self.mk_expr(ExprKind::Int(v), t.span()))
            }
            T::Float => {
                self.bump();
                let text = self.text(t).replace('_', "");
                let v: f64 = text.parse().unwrap_or(0.0);
                Ok(self.mk_expr(ExprKind::Float(v), t.span()))
            }
            T::Str => {
                self.bump();
                let raw = self.text(t);
                let body = if raw.len() >= 2 && raw.ends_with('"') { &raw[1..raw.len() - 1] } else { &raw[1..] };
                let s = lexer::unescape(body);
                let s = self.interner.intern(&s);
                Ok(self.mk_expr(ExprKind::Str(s), t.span()))
            }
            T::Char => {
                self.bump();
                let raw = self.text(t);
                let body = if raw.len() >= 2 && raw.ends_with('\'') { &raw[1..raw.len() - 1] } else { &raw[1..] };
                let c = lexer::unescape(body).chars().next().map(|c| c as u32).unwrap_or(0);
                Ok(self.mk_expr(ExprKind::Char(c), t.span()))
            }
            T::True | T::False => {
                self.bump();
                Ok(self.mk_expr(ExprKind::Bool(t.kind == T::True), t.span()))
            }
            T::Ident => {
                let text = self.text(t);
                if (text == "forall" || text == "exists") && self.peek_at(1) == T::Ident && self.peek_at(2) == T::In {
                    return self.parse_quant();
                }
                self.bump();
                let s = self.sym(t);
                Ok(self.mk_expr(ExprKind::Name(s), t.span()))
            }
            T::LParen => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                let r = (|| {
                    let mark = self.ast.expr_lists.mark();
                    let mut n = 0;
                    let mut trailing = false;
                    loop {
                        if self.eat(T::RParen) {
                            break;
                        }
                        let e = self.parse_expr()?;
                        self.ast.expr_lists.push_scratch(e);
                        n += 1;
                        trailing = false;
                        if self.eat(T::Comma) {
                            trailing = true;
                            continue;
                        }
                        self.expect(T::RParen)?;
                        break;
                    }
                    let list = self.ast.expr_lists.finish(mark);
                    if n == 1 && !trailing {
                        let inner = self.ast.expr_lists.get(list)[0];
                        return Ok(self.mk_expr(ExprKind::Paren(inner), self.span_from(lo)));
                    }
                    Ok(self.mk_expr(ExprKind::Tuple(list), self.span_from(lo)))
                })();
                self.no_struct = saved;
                r
            }
            T::LBracket => {
                self.bump();
                let saved = self.no_struct;
                self.no_struct = false;
                let r = (|| {
                    let mark = self.ast.expr_lists.mark();
                    loop {
                        if self.eat(T::RBracket) {
                            break;
                        }
                        let e = self.parse_expr()?;
                        self.ast.expr_lists.push_scratch(e);
                        if self.eat(T::Comma) {
                            continue;
                        }
                        self.expect(T::RBracket)?;
                        break;
                    }
                    let list = self.ast.expr_lists.finish(mark);
                    Ok(self.mk_expr(ExprKind::List(list), self.span_from(lo)))
                })();
                self.no_struct = saved;
                r
            }
            T::Fn => {
                self.bump();
                let (_, params) = self.parse_params(false)?;
                let mut ret = None;
                if self.eat(T::Arrow) {
                    ret = Some(self.parse_type()?);
                }
                let body = self.parse_block()?;
                Ok(self.mk_expr(ExprKind::Closure { params, ret, body }, self.span_from(lo)))
            }
            T::If => self.parse_if(),
            T::Match => self.parse_match(),
            T::LBrace => self.parse_block(),
            T::Amp => {
                self.diags.push(Diagnostic::error(
                    codes::E0314,
                    t.span(),
                    "`&mut` is only allowed in call arguments",
                ));
                self.bump();
                if self.tok_is_ident("mut") {
                    self.bump();
                }
                let inner = self.parse_unary()?;
                Ok(self.mk_expr(ExprKind::MutArg(inner), self.span_from(lo)))
            }
            _ => {
                self.expected("an expression");
                Err(())
            }
        }
    }

    fn parse_quant(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        let kw = self.bump();
        let forall = self.text(kw) == "forall";
        let var = self.expect_ident("a variable")?;
        self.expect(T::In)?;
        let range_lo = self.parse_binary(1)?;
        self.expect(T::DotDot)?;
        let range_hi = self.parse_binary(1)?;
        self.expect(T::Colon)?;
        let body = self.parse_expr()?;
        Ok(self.mk_expr(ExprKind::Quant { forall, var, lo: range_lo, hi: range_hi, body }, self.span_from(lo)))
    }

    fn parse_if(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        self.expect(T::If)?;
        let cond = self.with_no_struct(|p| p.parse_expr())?;
        let then = self.parse_block()?;
        let mut els = None;
        if self.skip_semis_before(T::Else) {
            self.bump();
            if self.at(T::If) {
                els = Some(self.parse_if()?);
            } else {
                els = Some(self.parse_block()?);
            }
        }
        Ok(self.mk_expr(ExprKind::If { cond, then, els }, self.span_from(lo)))
    }

    fn parse_match(&mut self) -> PResult<ExprId> {
        let lo = self.tok().lo;
        self.expect(T::Match)?;
        let scrut = self.with_no_struct(|p| p.parse_expr())?;
        self.expect(T::LBrace)?;
        let saved = self.no_struct;
        self.no_struct = false;
        let mut arms: Vec<Arm> = Vec::new();
        let r = (|| {
            loop {
                self.skip_semis();
                if self.eat(T::RBrace) {
                    break;
                }
                let alo = self.tok().lo;
                let pat = self.parse_pat()?;
                let mut guard = None;
                if self.eat(T::If) {
                    guard = Some(self.with_no_struct(|p| p.parse_expr())?);
                }
                self.expect(T::FatArrow)?;
                let body = self.parse_expr()?;
                arms.push(Arm { pat, guard, body, span: self.span_from(alo) });
                if self.eat(T::Comma) || self.at(T::Semi) {
                    continue;
                }
                if self.eat(T::RBrace) {
                    break;
                }
                self.expected("a line break or `}` after the match arm");
                return Err(());
            }
            Ok(())
        })();
        self.no_struct = saved;
        r?;
        let arms = self.ast.arms.alloc(&arms);
        Ok(self.mk_expr(ExprKind::Match { scrut, arms }, self.span_from(lo)))
    }
}

pub fn is_numeric_type_name(s: Symbol) -> bool {
    s == sym::INT || s == sym::FLOAT || s == sym::I32 || s == sym::U8 || s == sym::U32 || s == sym::U64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diag::SourceMap;

    pub(crate) fn parse_src(src: &str) -> (Ast, Vec<Diagnostic>) {
        let mut sm = SourceMap::new();
        let f = sm.add("t.jig".into(), src.to_string());
        let base = sm.file(f).base;
        let mut ast = Ast::default();
        let mut li = LocalInterner::default();
        let out = parse_file(&mut ast, f, src, base, &mut li);
        (ast, out.diags)
    }

    fn ok(src: &str) -> Ast {
        let (ast, diags) = parse_src(src);
        assert!(diags.is_empty(), "unexpected diagnostics: {:#?}", diags.iter().map(|d| &d.message).collect::<Vec<_>>());
        ast
    }

    #[test]
    fn parses_reference_examples() {
        let src = include_str!("../../tests/fixtures/syntax_tour.jig");
        let ast = ok(src);
        assert!(ast.items.len() > 20);
    }

    #[test]
    fn generic_disambiguation() {
        let ast = ok("fn f() {\n  let a = x < y\n  let b = List<Int>.new()\n  let c = g(a < b, c > d)\n  if i < n {\n  }\n}\n");
        let mut type_args = 0;
        for e in ast.exprs.iter() {
            if matches!(e.kind, ExprKind::TypeArgs { .. }) {
                type_args += 1;
            }
        }
        assert_eq!(type_args, 1);
    }

    #[test]
    fn no_struct_literal_in_if_head() {
        let ast = ok("fn f() {\n  if x == Point {\n  }\n}\n");
        assert!(!ast.exprs.iter().any(|e| matches!(e.kind, ExprKind::StructLit { .. })));
    }

    #[test]
    fn struct_literal_and_nested_generics() {
        let ast = ok("fn f() -> Map<Str, List<Int>> {\n  let p = Point { x: 1, y: 2 }\n  let q = geom.Point {\n    x: 1\n    y: 2\n  }\n  return Map.new()\n}\n");
        let n = ast.exprs.iter().filter(|e| matches!(e.kind, ExprKind::StructLit { .. })).count();
        assert_eq!(n, 2);
    }

    #[test]
    fn reports_and_recovers() {
        let (ast, diags) = parse_src("fn f() {\n  let x = \n  let y = 2\n}\nfn g() {}\n");
        assert_eq!(diags.len(), 1, "{:?}", diags.iter().map(|d| &d.message).collect::<Vec<_>>());
        assert_eq!(ast.top_items().count(), 2);
    }

    #[test]
    fn else_on_next_line_and_chains() {
        ok("fn f() {\n  if a {\n  }\n  else {\n  }\n  let n = xs\n    .map(fn(x) { x * 2 })\n    .len()\n}\n");
    }
}
