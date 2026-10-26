//! The abstract syntax tree. One `Ast` holds every file of a package in flat arenas; nodes
//! refer to each other by index, and child lists are ranges into side arenas.

use super::lexer::Comment;
use crate::diag::{FileId, Span};
use crate::util::{IndexVec, List, ListArena, Symbol};

crate::define_idx!(ItemId);
crate::define_idx!(ExprId);
crate::define_idx!(StmtId);
crate::define_idx!(TypeExprId);
crate::define_idx!(PatId);

#[derive(Default)]
pub struct Ast {
    pub items: IndexVec<ItemId, Item>,
    pub exprs: IndexVec<ExprId, Expr>,
    pub stmts: IndexVec<StmtId, Stmt>,
    pub types: IndexVec<TypeExprId, TypeExpr>,
    pub pats: IndexVec<PatId, Pat>,

    pub item_lists: ListArena<ItemId>,
    pub expr_lists: ListArena<ExprId>,
    pub stmt_lists: ListArena<StmtId>,
    pub type_lists: ListArena<TypeExprId>,
    pub pat_lists: ListArena<PatId>,
    pub params: ListArena<Param>,
    pub generics: ListArena<GenericParam>,
    pub fields: ListArena<FieldDecl>,
    pub variants: ListArena<VariantDecl>,
    pub field_inits: ListArena<FieldInit>,
    pub arms: ListArena<Arm>,
    pub names: ListArena<Name>,
    pub unit_factors: ListArena<UnitFactor>,

    pub files: Vec<FileAst>,
}

pub struct FileAst {
    pub file: FileId,
    pub items: List<ItemId>,
    pub comments: Vec<Comment>,
    pub blank_lines: Vec<u32>,
    pub span: Span,
}

/// A name with its source location.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Name {
    pub sym: Symbol,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub kind: ItemKind,
    pub name: Name,
    pub span: Span,
    pub is_pub: bool,
    /// Index into `Ast::files`.
    pub file: u32,
}

#[derive(Clone, Copy, Debug)]
pub enum ItemKind {
    /// `import "path"`; `name` is the binding (last path segment), `path` the full string.
    Import { path: Symbol, path_span: Span },
    Fn(FnDecl),
    Struct { generics: List<GenericParam>, fields: List<FieldDecl>, invariants: List<ExprId> },
    Enum { generics: List<GenericParam>, variants: List<VariantDecl> },
    Interface { generics: List<GenericParam>, methods: List<ItemId> },
    TypeAlias { generics: List<GenericParam>, ty: TypeExprId },
    Const { ty: TypeExprId, value: ExprId },
    Unit,
    Test { label: Symbol, params: List<Param>, requires: List<ExprId>, body: ExprId },
}

#[derive(Clone, Copy, Debug)]
pub struct FnDecl {
    /// `fn Type.name`: the receiver type and the type's generic parameters.
    pub recv: Option<Receiver>,
    pub self_param: Option<SelfParam>,
    pub generics: List<GenericParam>,
    pub params: List<Param>,
    pub ret: Option<TypeExprId>,
    pub requires: List<ExprId>,
    pub ensures: List<ExprId>,
    /// `None`: no clause (pure). `Some(empty)`: explicit `effects {}`.
    pub effects: Option<List<Name>>,
    pub effects_span: Span,
    /// A `Block` expression; `None` for interface method signatures.
    pub body: Option<ExprId>,
    /// `step "pattern"` attached to this function.
    pub step: Option<(Symbol, Span)>,
    pub sig_span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Receiver {
    pub ty: Name,
    pub generics: List<GenericParam>,
}

#[derive(Clone, Copy, Debug)]
pub struct SelfParam {
    pub mutable: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct Param {
    pub name: Name,
    /// `None` only for closure parameters whose type is inferred.
    pub ty: Option<TypeExprId>,
    pub mut_ref: bool,
    /// `x: Int where x > 0`
    pub refine: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct GenericParam {
    pub name: Name,
    pub bounds: List<TypeExprId>,
}

#[derive(Clone, Copy, Debug)]
pub struct FieldDecl {
    pub name: Name,
    pub ty: TypeExprId,
    pub is_pub: bool,
    pub refine: Option<ExprId>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct VariantDecl {
    pub name: Name,
    pub fields: List<FieldDecl>,
    /// Whether the variant was written with parentheses (`Empty` vs `Empty()`).
    pub has_parens: bool,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct UnitFactor {
    pub name: Name,
    pub exp: i8,
}

#[derive(Clone, Copy, Debug)]
pub struct TypeExpr {
    pub kind: TypeExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum TypeExprKind {
    /// `Name`, `pkg.Name`, `Name<A, B>`, `Float<m/s>`.
    Named { pkg: Option<Name>, name: Name, args: List<TypeExprId>, unit: Option<List<UnitFactor>> },
    /// `()` and `(A, B)`.
    Tuple(List<TypeExprId>),
    Fn { params: List<TypeExprId>, ret: Option<TypeExprId>, effects: Option<List<Name>> },
    /// `T where pred` (aliases use `self` in `pred`).
    Refined { base: TypeExprId, pred: ExprId },
    Error,
}

#[derive(Clone, Copy, Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BinOp {
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    Add,
    Sub,
    BitOr,
    BitXor,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

impl BinOp {
    pub fn precedence(self) -> u8 {
        use BinOp::*;
        match self {
            Mul | Div | Rem | Shl | Shr | BitAnd => 5,
            Add | Sub | BitOr | BitXor => 4,
            Eq | Ne | Lt | Le | Gt | Ge => 3,
            And => 2,
            Or => 1,
        }
    }
    pub fn as_str(self) -> &'static str {
        use BinOp::*;
        match self {
            Mul => "*",
            Div => "/",
            Rem => "%",
            Shl => "<<",
            Shr => ">>",
            BitAnd => "&",
            Add => "+",
            Sub => "-",
            BitOr => "|",
            BitXor => "^",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Le => "<=",
            Gt => ">",
            Ge => ">=",
            And => "&&",
            Or => "||",
        }
    }
    pub fn is_comparison(self) -> bool {
        use BinOp::*;
        matches!(self, Eq | Ne | Lt | Le | Gt | Ge)
    }
    pub fn is_arith(self) -> bool {
        use BinOp::*;
        matches!(self, Mul | Div | Rem | Add | Sub)
    }
    pub fn is_bitwise(self) -> bool {
        use BinOp::*;
        matches!(self, Shl | Shr | BitAnd | BitOr | BitXor)
    }
}

impl UnOp {
    pub fn as_str(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "!",
            UnOp::BitNot => "~",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum ExprKind {
    Int(u64),
    Float(f64),
    Bool(bool),
    Str(Symbol),
    Char(u32),
    Name(Symbol),
    /// `(e)`, kept so the formatter preserves the author's grouping.
    Paren(ExprId),
    Tuple(List<ExprId>),
    List(List<ExprId>),
    Field { base: ExprId, name: Name },
    TupleField { base: ExprId, index: u32, index_span: Span },
    Call { callee: ExprId, args: List<ExprId> },
    /// `&mut place`, only valid as a call argument.
    MutArg(ExprId),
    Index { base: ExprId, index: ExprId },
    Range { lo: Option<ExprId>, hi: Option<ExprId> },
    Unary { op: UnOp, expr: ExprId },
    Binary { op: BinOp, lhs: ExprId, rhs: ExprId },
    Cast { expr: ExprId, ty: TypeExprId },
    Try(ExprId),
    If { cond: ExprId, then: ExprId, els: Option<ExprId> },
    Match { scrut: ExprId, arms: List<Arm> },
    Block(List<StmtId>),
    StructLit { ty: TypeExprId, fields: List<FieldInit> },
    Closure { params: List<Param>, ret: Option<TypeExprId>, body: ExprId },
    /// Explicit type arguments in expression position: `List<Int>.new()`.
    TypeArgs { base: ExprId, args: List<TypeExprId> },
    /// `forall i in lo..hi: body` / `exists ...`
    Quant { forall: bool, var: Name, lo: ExprId, hi: ExprId, body: ExprId },
    Error,
}

#[derive(Clone, Copy, Debug)]
pub struct Arm {
    pub pat: PatId,
    pub guard: Option<ExprId>,
    pub body: ExprId,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub struct FieldInit {
    pub name: Name,
    pub value: ExprId,
}

#[derive(Clone, Copy, Debug)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum StmtKind {
    Let { pat: PatId, ty: Option<TypeExprId>, init: ExprId, mutable: bool },
    Assign { target: ExprId, op: Option<BinOp>, value: ExprId },
    Expr(ExprId),
    Return(Option<ExprId>),
    Break,
    Continue,
    For { head: ForHead, invariants: List<ExprId>, body: ExprId },
}

#[derive(Clone, Copy, Debug)]
pub enum ForHead {
    Infinite,
    Cond(ExprId),
    In { pat: PatId, iter: ExprId },
}

#[derive(Clone, Copy, Debug)]
pub struct Pat {
    pub kind: PatKind,
    pub span: Span,
}

#[derive(Clone, Copy, Debug)]
pub enum PatKind {
    Wild,
    Bind(Symbol),
    /// A literal expression: Int, Float, Str, Char, Bool, or a negated number.
    Lit(ExprId),
    Variant { name: Name, args: List<PatId>, has_parens: bool },
    Tuple(List<PatId>),
    Or(List<PatId>),
    Error,
}

impl Ast {
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id]
    }
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id]
    }
    pub fn ty(&self, id: TypeExprId) -> &TypeExpr {
        &self.types[id]
    }
    pub fn pat(&self, id: PatId) -> &Pat {
        &self.pats[id]
    }
    pub fn item(&self, id: ItemId) -> &Item {
        &self.items[id]
    }
    pub fn fn_decl(&self, id: ItemId) -> Option<&FnDecl> {
        match &self.items[id].kind {
            ItemKind::Fn(f) => Some(f),
            _ => None,
        }
    }
    /// All top-level items of every file, in order.
    pub fn top_items(&self) -> impl Iterator<Item = ItemId> + '_ {
        self.files.iter().flat_map(|f| self.item_lists.get(f.items).iter().copied())
    }
}
