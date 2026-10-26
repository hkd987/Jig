//! The program model: packages, item definitions, and the tables the checker fills in.

use super::ty::*;
use crate::diag::{Diags, FileId, SourceMap, Span};
use crate::syntax::ast::{Ast, ExprId, ItemId, PatId};
use crate::util::{FxHashMap, IndexVec, Symbol};
use std::path::PathBuf;

crate::define_idx!(LocalId);
crate::define_idx!(TestId);

pub struct Program {
    pub sm: SourceMap,
    pub pkgs: IndexVec<PkgId, Package>,
    pub tables: IndexVec<PkgId, Tables>,
    pub defs: Defs,
    pub tys: TyCtx,
    pub diags: Diags,
    pub bodies: IndexVec<FnId, Option<Body>>,
    pub test_bodies: IndexVec<TestId, Option<Body>>,
    pub prelude: PkgId,
    pub root: PkgId,
    /// Project root directory (holding `jig.mod`), if any.
    pub project_root: Option<PathBuf>,
    /// Scenarios from `.feature` files, bound to their step functions.
    pub scenarios: Vec<crate::feature::bind::ScenarioDef>,
    /// C libraries to link (`link NAME` lines in `jig.mod`).
    pub links: Vec<String>,
}

pub struct Package {
    pub path: Symbol,
    pub name: Symbol,
    pub dir: Option<PathBuf>,
    pub ast: Ast,
    pub is_std: bool,
    /// From a required module (`jig.mod`): checked, but not held to the project's dead-code
    /// and lint rules, and not tested or proved with it.
    pub external: bool,
    pub feature_files: Vec<FileId>,
    /// Package-level names.
    pub scope: FxHashMap<Symbol, Def>,
    /// Per file (index into `ast.files`): imported package bindings.
    pub imports: Vec<FxHashMap<Symbol, (PkgId, ItemId)>>,
    /// Packages this package imports (deduplicated).
    pub deps: Vec<PkgId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Def {
    Fn(FnId),
    Adt(AdtId),
    Iface(IfaceId),
    Const(ConstId),
    Alias(AliasId),
    Unit,
    Builtin(BuiltinTy),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum BuiltinTy {
    Int(IntTy),
    Float,
    Bool,
    Char,
    Str,
    List,
    Map,
    Set,
    Never,
}

/// Key for method lookup: the receiver's type constructor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TyKey {
    Adt(AdtId),
    Int(IntTy),
    Float,
    Bool,
    Char,
    Str,
    List,
    Map,
    Set,
}

impl TyKey {
    pub fn of(tys: &TyCtx, t: Ty) -> Option<TyKey> {
        Some(match tys.kind(t) {
            TyKind::Adt(a, _) => TyKey::Adt(*a),
            TyKind::Int(k, _) => TyKey::Int(*k),
            TyKind::Float(_) => TyKey::Float,
            TyKind::Bool => TyKey::Bool,
            TyKind::Char => TyKey::Char,
            TyKind::Str => TyKey::Str,
            TyKind::List(_) => TyKey::List,
            TyKind::Map(..) => TyKey::Map,
            TyKind::Set(_) => TyKey::Set,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Bound {
    Iface(IfaceId),
    Eq,
    Ord,
    Hash,
    Show,
}

#[derive(Clone, Debug)]
pub struct GenericDef {
    pub name: Symbol,
    pub bounds: Vec<Bound>,
    pub span: Span,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    Value,
    MutRef,
}

#[derive(Clone, Debug)]
pub struct ParamDef {
    pub name: Symbol,
    pub ty: Ty,
    pub mode: Mode,
    pub span: Span,
    /// The refinement alias the parameter's type names (`amt: Cents`), if any.
    pub refine: Option<AliasId>,
}

pub struct FnDef {
    pub pkg: PkgId,
    pub item: ItemId,
    pub name: Symbol,
    /// For methods and associated functions: the receiver type constructor.
    pub recv: Option<TyKey>,
    /// `Some` for methods taking `self` / `&mut self`.
    pub self_mode: Option<Mode>,
    /// The receiver type in terms of the receiver's generic parameters.
    pub self_ty: Option<Ty>,
    /// Receiver generics first, then the function's own.
    pub generics: Vec<GenericDef>,
    pub n_recv_generics: usize,
    pub params: Vec<ParamDef>,
    pub ret: Ty,
    /// The refinement alias the return type names, if any.
    pub ret_refine: Option<AliasId>,
    pub effects: EffectSet,
    pub is_pub: bool,
    pub has_body: bool,
    /// A C function: declared without a body, outside the standard library, with the `ffi`
    /// effect. Calls go to the C symbol of the same name.
    pub ffi: bool,
    /// For interface method signatures.
    pub iface: Option<IfaceId>,
    pub step: Option<(Symbol, Span)>,
    pub span: Span,
    pub name_span: Span,
}

impl FnDef {
    pub fn display_name(&self, defs: &Defs, tys: &TyCtx) -> String {
        match self.recv {
            Some(k) => format!("{}.{}", tykey_name(k, defs, tys), self.name),
            None => self.name.to_string(),
        }
    }
}

pub fn tykey_name(k: TyKey, defs: &Defs, _tys: &TyCtx) -> String {
    match k {
        TyKey::Adt(a) => defs.adts[a].name.to_string(),
        TyKey::Int(i) => i.name().to_string(),
        TyKey::Float => "Float".into(),
        TyKey::Bool => "Bool".into(),
        TyKey::Char => "Char".into(),
        TyKey::Str => "Str".into(),
        TyKey::List => "List".into(),
        TyKey::Map => "Map".into(),
        TyKey::Set => "Set".into(),
    }
}

pub struct FieldDef {
    pub name: Symbol,
    pub ty: Ty,
    pub is_pub: bool,
    pub span: Span,
    /// The refinement alias the field's type names, if any.
    pub refine: Option<AliasId>,
}

pub struct VariantDef {
    pub name: Symbol,
    pub fields: Vec<FieldDef>,
    pub span: Span,
}

pub enum AdtKind {
    Struct { fields: Vec<FieldDef> },
    Enum { variants: Vec<VariantDef> },
}

pub struct AdtDef {
    pub pkg: PkgId,
    pub item: ItemId,
    pub name: Symbol,
    pub generics: Vec<GenericDef>,
    pub kind: AdtKind,
    pub is_pub: bool,
    /// Enums whose payloads (transitively) contain the enum itself are boxed.
    pub recursive: bool,
}

impl AdtDef {
    pub fn fields(&self) -> &[FieldDef] {
        match &self.kind {
            AdtKind::Struct { fields } => fields,
            AdtKind::Enum { .. } => &[],
        }
    }
    pub fn variants(&self) -> &[VariantDef] {
        match &self.kind {
            AdtKind::Struct { .. } => &[],
            AdtKind::Enum { variants } => variants,
        }
    }
    pub fn is_enum(&self) -> bool {
        matches!(self.kind, AdtKind::Enum { .. })
    }
}

pub struct IfaceDef {
    pub pkg: PkgId,
    pub item: ItemId,
    pub name: Symbol,
    /// Method signatures; their `self_ty` is `Param(0, Self)`.
    pub methods: Vec<FnId>,
    pub is_pub: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ConstVal {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(Symbol),
    Char(u32),
}

pub struct ConstDef {
    pub pkg: PkgId,
    pub item: ItemId,
    pub name: Symbol,
    pub ty: Ty,
    pub value: Option<ConstVal>,
    pub is_pub: bool,
}

pub struct AliasDef {
    pub pkg: PkgId,
    pub item: ItemId,
    pub name: Symbol,
    pub generics: Vec<GenericDef>,
    pub ty: Ty,
    /// The refinement predicate (`self` names the value) if this is a refinement alias.
    pub refine: Option<ExprId>,
    pub is_pub: bool,
}

pub struct TestDef {
    pub pkg: PkgId,
    pub item: ItemId,
    pub label: Symbol,
    pub params: Vec<ParamDef>,
}

#[derive(Default)]
pub struct Defs {
    pub fns: IndexVec<FnId, FnDef>,
    pub adts: IndexVec<AdtId, AdtDef>,
    pub ifaces: IndexVec<IfaceId, IfaceDef>,
    pub consts: IndexVec<ConstId, ConstDef>,
    pub aliases: IndexVec<AliasId, AliasDef>,
    pub tests: IndexVec<TestId, TestDef>,
    pub methods: FxHashMap<(TyKey, Symbol), FnId>,
    /// The function each item declares (for items that are functions).
    pub fn_of_item: FxHashMap<(PkgId, ItemId), FnId>,
    pub option: Option<AdtId>,
    pub result: Option<AdtId>,
    pub main: Option<FnId>,
    /// Imports used by signatures, fields and aliases: (package, file, import name).
    pub sig_imports_used: crate::util::FxHashSet<(PkgId, usize, Symbol)>,
}

impl Defs {
    pub fn method(&self, k: TyKey, name: Symbol) -> Option<FnId> {
        self.methods.get(&(k, name)).copied()
    }
}

/// What a name, field or call in a body refers to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Res {
    #[default]
    None,
    Local(LocalId),
    Fn(FnId),
    /// `recv.method` used as a callee; `autoref` when the receiver is passed as `&mut`.
    Method(FnId),
    /// A method reached through a generic bound: `x.area()` with `x: T` and `T: Area`.
    IfaceMethod(IfaceId, u32),
    Variant(AdtId, u32),
    Field(u32),
    Const(ConstId),
    Pkg(PkgId),
    Type(Ty),
    /// `old(e)` in an ensures clause.
    Old,
    /// `result` in an ensures clause.
    ResultVal,
    /// A builtin bound method (`x.str()`, `x.hash()`) on a generic parameter.
    BoundMethod(Bound),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalKind {
    Let,
    Var,
    Param,
    MutParam,
    SelfValue,
    SelfMut,
    /// Bound by a pattern in `match`, `for`, or `let (a, b)`.
    Pattern,
    /// A quantifier variable in a contract.
    Quant,
}

pub struct LocalDef {
    pub name: Symbol,
    pub ty: Ty,
    pub kind: LocalKind,
    pub span: Span,
    pub used: bool,
    pub mutated: bool,
    /// Closure nesting depth where it was declared (0 = function body).
    pub depth: u32,
}

impl LocalDef {
    pub fn is_mutable(&self) -> bool {
        matches!(self.kind, LocalKind::Var | LocalKind::MutParam | LocalKind::SelfMut)
    }
}

/// Per-function checking results.
#[derive(Default)]
pub struct Body {
    pub locals: IndexVec<LocalId, LocalDef>,
    /// Locals for `self` and each parameter, in order.
    pub param_locals: Vec<LocalId>,
    /// Effects the body performs.
    pub used_effects: EffectSet,
    /// Every function the body references (calls or takes as a value), for reachability.
    pub refs: Vec<FnId>,
    /// Every variant constructed and field read, for dead-code checks.
    pub variants_built: Vec<(AdtId, u32)>,
    pub fields_read: Vec<(AdtId, u32)>,
    pub consts_used: Vec<ConstId>,
    /// Method names called through generic bounds (resolved per instantiation).
    pub dyn_methods: Vec<Symbol>,
    /// Expressions whose type is compared, hashed or shown with derived behavior (which reads
    /// every field).
    pub derived_exprs: Vec<ExprId>,
}

/// Per-package tables filled by the checker, indexed by AST node ids.
#[derive(Default)]
pub struct Tables {
    pub expr_ty: Vec<Ty>,
    pub expr_res: Vec<Res>,
    /// Type arguments for generic functions, methods and variants at each use.
    pub expr_inst: FxHashMap<ExprId, Box<[Ty]>>,
    /// For method calls: whether the receiver is passed by `&mut`.
    pub autoref: FxHashMap<ExprId, bool>,
    pub pat_ty: Vec<Ty>,
    /// Locals bound by binding patterns.
    pub pat_local: FxHashMap<PatId, LocalId>,
    /// For variant patterns: (adt, variant index).
    pub pat_variant: FxHashMap<PatId, (AdtId, u32)>,
    /// Locals bound by `let`/`var` statements with a simple name, keyed by the pattern.
    pub closure_captures: FxHashMap<ExprId, Vec<LocalId>>,
}

impl Tables {
    pub fn new(n_exprs: usize, n_pats: usize, error: Ty) -> Tables {
        Tables {
            expr_ty: vec![error; n_exprs],
            expr_res: vec![Res::None; n_exprs],
            pat_ty: vec![error; n_pats],
            ..Default::default()
        }
    }
}
