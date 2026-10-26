//! Verification conditions by symbolic evaluation of the typed AST.
//!
//! Each function is evaluated once with symbolic parameters. Values are trees of SMT terms
//! (`Sv`): integers are mathematical integers with range facts, strings and floats are
//! uninterpreted, lists are a length plus one SMT array per leaf of the element type, and
//! enums are a tag plus every variant's fields. Branches merge with `ite`, loops are cut
//! with their invariants (modified state is havocked), and calls use only the callee's
//! contract. Every place a run could trap or a contract could fail becomes an obligation
//! under the path condition that reaches it.
//!
//! Anything not modeled evaluates to a fresh unconstrained value, which is always sound:
//! it can only make proofs fail, never succeed wrongly.

use crate::diag::{SourceMap, Span};
use crate::mir::intrinsics::{self, Op};
use crate::sema::defs::*;
use crate::sema::ty::*;
use crate::syntax::ast::{self, Ast, BinOp, ExprId, ExprKind, ForHead, PatId, PatKind, StmtId, StmtKind, UnOp};
use crate::util::{sym, FxHashMap, FxHashSet, IndexVec, Symbol};

/// The most elements a list (or bytes a string) can hold: every element takes at least a
/// byte, and no 64-bit system gives a program 2^60 bytes of address space (x86-64 and
/// AArch64 give at most 2^56). It lets the prover show that `i + 1`, or the sum of two
/// lengths, cannot overflow.
const MAX_LEN: i128 = 1 << 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ObKind {
    /// A callee's `requires` or parameter refinement, at a call site.
    Precondition,
    Postcondition,
    LoopInvariant,
    /// A struct invariant or field refinement, at construction or mutation.
    StructInvariant,
    /// A refinement alias or `where` clause.
    Refinement,
    Assert,
    Overflow,
    DivZero,
    Bounds,
    Unwrap,
    Panic,
    Cast,
    Shift,
}

impl ObKind {
    pub fn name(self) -> &'static str {
        match self {
            ObKind::Precondition => "precondition",
            ObKind::Postcondition => "postcondition",
            ObKind::LoopInvariant => "loop invariant",
            ObKind::StructInvariant => "struct invariant",
            ObKind::Refinement => "refinement",
            ObKind::Assert => "assertion",
            ObKind::Overflow => "overflow",
            ObKind::DivZero => "division by zero",
            ObKind::Bounds => "bounds",
            ObKind::Unwrap => "unwrap",
            ObKind::Panic => "panic",
            ObKind::Cast => "conversion",
            ObKind::Shift => "shift",
        }
    }
    pub fn parse(s: &str) -> Option<ObKind> {
        use ObKind::*;
        [
            Precondition,
            Postcondition,
            LoopInvariant,
            StructInvariant,
            Refinement,
            Assert,
            Overflow,
            DivZero,
            Bounds,
            Unwrap,
            Panic,
            Cast,
            Shift,
        ]
        .into_iter()
        .find(|k| k.name() == s)
    }
    /// Contracts are what people write; safety obligations are the runtime traps.
    pub fn is_contract(self) -> bool {
        matches!(
            self,
            ObKind::Precondition
                | ObKind::Postcondition
                | ObKind::LoopInvariant
                | ObKind::StructInvariant
                | ObKind::Refinement
                | ObKind::Assert
        )
    }
}

pub struct Obligation {
    pub kind: ObKind,
    pub span: Span,
    pub msg: String,
    pub pc: Vec<String>,
    pub goal: String,
    /// Whether proving it may remove the runtime check at `span` (only for obligations from
    /// the function's own code, not from callee contracts or type predicates).
    pub elidable: bool,
}

/// The verification conditions of one function.
pub struct FnVcs {
    pub decls: Vec<String>,
    pub obligations: Vec<Obligation>,
    /// Parameter leaves to show in counterexamples: (label, term, kind).
    pub show: Vec<(String, String, Base)>,
    /// When every parameter is built from integers, booleans and characters: each
    /// parameter's type and leaf terms, so a counterexample can be replayed.
    pub inputs: Option<Vec<ParamLeaves>>,
}

/// A parameter's type and its leaf terms with their kinds.
pub type ParamLeaves = (Ty, Vec<(String, Base)>);

// ---------- symbolic values ----------

#[derive(Clone, Debug, PartialEq)]
pub enum Base {
    Bool,
    Int(IntTy),
    Char,
    /// An enum tag with this many variants.
    Tag(u32),
    Str,
    Float,
    /// An uninterpreted value of this type (maps, sets, functions, recursive enums).
    Opaque(Ty),
}

#[derive(Clone, Debug)]
pub struct Leaf {
    pub t: String,
    pub base: Base,
    /// How many `(Array Int _)` wrappers: list elements are lifted to arrays.
    pub depth: u8,
}

#[derive(Clone, Debug)]
pub enum Sv {
    Unit,
    Leaf(Leaf),
    List { len: Leaf, elems: Box<Sv> },
    Tuple(Vec<Sv>),
    Struct(AdtId, Vec<Sv>),
    Enum { adt: AdtId, tag: Leaf, vars: Vec<Vec<Sv>> },
}

impl Sv {
    fn bool(t: impl Into<String>) -> Sv {
        Sv::Leaf(Leaf { t: t.into(), base: Base::Bool, depth: 0 })
    }
    fn int(t: impl Into<String>, k: IntTy) -> Sv {
        Sv::Leaf(Leaf { t: t.into(), base: Base::Int(k), depth: 0 })
    }
    /// The term of a scalar.
    pub fn term(&self) -> Option<&str> {
        match self {
            Sv::Leaf(l) => Some(&l.t),
            _ => None,
        }
    }
    fn map_leaves(&self, f: &mut impl FnMut(&Leaf) -> Leaf) -> Sv {
        match self {
            Sv::Unit => Sv::Unit,
            Sv::Leaf(l) => Sv::Leaf(f(l)),
            Sv::List { len, elems } => Sv::List { len: f(len), elems: Box::new(elems.map_leaves(f)) },
            Sv::Tuple(xs) => Sv::Tuple(xs.iter().map(|x| x.map_leaves(f)).collect()),
            Sv::Struct(a, xs) => Sv::Struct(*a, xs.iter().map(|x| x.map_leaves(f)).collect()),
            Sv::Enum { adt, tag, vars } => Sv::Enum {
                adt: *adt,
                tag: f(tag),
                vars: vars.iter().map(|v| v.iter().map(|x| x.map_leaves(f)).collect()).collect(),
            },
        }
    }
    /// Combine two values of the same shape leaf by leaf; `None` if the shapes differ.
    fn zip_leaves(&self, other: &Sv, f: &mut impl FnMut(&Leaf, &Leaf) -> Leaf) -> Option<Sv> {
        Some(match (self, other) {
            (Sv::Unit, Sv::Unit) => Sv::Unit,
            (Sv::Leaf(a), Sv::Leaf(b)) => Sv::Leaf(f(a, b)),
            (Sv::List { len: la, elems: ea }, Sv::List { len: lb, elems: eb }) => {
                Sv::List { len: f(la, lb), elems: Box::new(ea.zip_leaves(eb, f)?) }
            }
            (Sv::Tuple(xs), Sv::Tuple(ys)) if xs.len() == ys.len() => {
                Sv::Tuple(xs.iter().zip(ys).map(|(x, y)| x.zip_leaves(y, f)).collect::<Option<_>>()?)
            }
            (Sv::Struct(a, xs), Sv::Struct(b, ys)) if a == b && xs.len() == ys.len() => {
                Sv::Struct(*a, xs.iter().zip(ys).map(|(x, y)| x.zip_leaves(y, f)).collect::<Option<_>>()?)
            }
            (Sv::Enum { adt, tag: ta, vars: va }, Sv::Enum { adt: b, tag: tb, vars: vb })
                if adt == b && va.len() == vb.len() =>
            {
                let mut vars = Vec::new();
                for (x, y) in va.iter().zip(vb) {
                    if x.len() != y.len() {
                        return None;
                    }
                    vars.push(x.iter().zip(y).map(|(p, q)| p.zip_leaves(q, f)).collect::<Option<Vec<_>>>()?);
                }
                Sv::Enum { adt: *adt, tag: f(ta, tb), vars }
            }
            _ => return None,
        })
    }
    fn leaves<'s>(&'s self, out: &mut Vec<&'s Leaf>) {
        match self {
            Sv::Unit => {}
            Sv::Leaf(l) => out.push(l),
            Sv::List { len, elems } => {
                out.push(len);
                elems.leaves(out);
            }
            Sv::Tuple(xs) | Sv::Struct(_, xs) => xs.iter().for_each(|x| x.leaves(out)),
            Sv::Enum { tag, vars, .. } => {
                out.push(tag);
                vars.iter().flatten().for_each(|x| x.leaves(out));
            }
        }
    }
    fn same(&self, other: &Sv) -> bool {
        let (mut a, mut b) = (Vec::new(), Vec::new());
        self.leaves(&mut a);
        other.leaves(&mut b);
        a.len() == b.len() && a.iter().zip(&b).all(|(x, y)| x.t == y.t)
    }
}

/// The leaves of a value made only of integers, booleans and characters (in `fresh` order).
fn scalar_leaves(v: &Sv) -> Option<Vec<(String, Base)>> {
    let mut ls = Vec::new();
    v.leaves(&mut ls);
    if let Sv::List { .. } | Sv::Enum { .. } = v {
        return None;
    }
    let mut out = Vec::new();
    for l in ls {
        match l.base {
            Base::Bool | Base::Int(_) | Base::Char if l.depth == 0 => out.push((l.t.clone(), l.base.clone())),
            _ => return None,
        }
    }
    fn plain(v: &Sv) -> bool {
        match v {
            Sv::Unit | Sv::Leaf(_) => true,
            Sv::Tuple(xs) | Sv::Struct(_, xs) => xs.iter().all(plain),
            _ => false,
        }
    }
    plain(v).then_some(out)
}

pub fn int_lit(v: i128) -> String {
    if v < 0 {
        format!("(- {})", -v)
    } else {
        v.to_string()
    }
}

fn and(xs: &[String]) -> String {
    let xs: Vec<&String> = xs.iter().filter(|x| x.as_str() != "true").collect();
    match xs.len() {
        0 => "true".into(),
        1 => xs[0].clone(),
        _ => format!("(and {})", xs.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(" ")),
    }
}

fn not(x: &str) -> String {
    match x {
        "true" => "false".into(),
        "false" => "true".into(),
        _ => format!("(not {x})"),
    }
}

fn or2(a: &str, b: &str) -> String {
    if a == "true" || b == "true" {
        return "true".into();
    }
    if a == "false" {
        return b.to_string();
    }
    if b == "false" {
        return a.to_string();
    }
    format!("(or {a} {b})")
}

fn range_fact(t: &str, lo: i128, hi: i128) -> String {
    format!("(and (<= {} {t}) (<= {t} {}))", int_lit(lo), int_lit(hi))
}

#[derive(Clone, Default)]
pub struct St {
    pub env: FxHashMap<LocalId, Sv>,
    pub pc: Vec<String>,
}

/// Where a value lives, for writes: a local and a path into it.
#[derive(Clone)]
struct PlaceRef {
    local: LocalId,
    path: Vec<Seg>,
    /// The type at each step: `tys[0]` is the local's type, `tys[i+1]` after `path[i]`.
    tys: Vec<Ty>,
}

#[derive(Clone)]
enum Seg {
    Field(u32),
    Index(String, Span),
}

enum Arg {
    Value(Sv),
    Place(PlaceRef, Sv),
}

impl Arg {
    fn value(&self) -> &Sv {
        match self {
            Arg::Value(v) | Arg::Place(_, v) => v,
        }
    }
}

#[derive(Clone)]
struct Frame<'a> {
    pkg: PkgId,
    body: Option<&'a Body>,
    inst: Box<[Ty]>,
    result: Option<Sv>,
    old: Option<FxHashMap<LocalId, Sv>>,
    /// Names bound by name (struct and alias predicates are checked outside any body).
    names: Vec<(Symbol, Sv)>,
    /// Obligations from this frame may remove runtime checks.
    own: bool,
}

struct LoopFrame {
    breaks: Vec<St>,
    conts: Vec<St>,
}

pub struct Gen<'a> {
    pub defs: &'a Defs,
    pub pkgs: &'a IndexVec<PkgId, Package>,
    pub tables: &'a IndexVec<PkgId, Tables>,
    pub bodies: &'a IndexVec<FnId, Option<Body>>,
    pub sm: &'a SourceMap,
    pub tys: &'a mut TyCtx,
    sorts: Vec<String>,
    funs: Vec<String>,
    decls: Vec<String>,
    declared: FxHashSet<String>,
    axioms: Vec<String>,
    n: usize,
    str_lits: FxHashMap<Symbol, String>,
    float_lits: FxHashMap<u64, String>,
    obligations: Vec<Obligation>,
    returns: Vec<(St, Sv)>,
    loops: Vec<LoopFrame>,
    /// Inside a quantifier body evaluated for its value: no definitions or obligations.
    inline: u32,
    rec_fields: FxHashMap<(String, u32, u32), Sv>,
    prelude: Option<PkgId>,
    /// The return type of the function being verified (for `?`).
    cur_ret: Option<Ty>,
    /// Nesting of callee contracts being evaluated (contracts may call contracted functions).
    depth: u32,
    /// What each function may change in each `&mut` parameter (see `frame.rs`).
    pub frames: FxHashMap<(FnId, usize), super::frame::Frame>,
}

impl<'a> Gen<'a> {
    pub fn new(prog: &'a mut Program) -> Gen<'a> {
        let Program { defs, pkgs, tables, bodies, sm, tys, .. } = prog;
        let prelude = pkgs.iter_enumerated().find(|(_, p)| p.path.as_str() == "<prelude>").map(|(i, _)| i);
        Gen {
            defs,
            pkgs,
            tables,
            bodies,
            sm,
            tys,
            sorts: Vec::new(),
            funs: Vec::new(),
            decls: Vec::new(),
            declared: FxHashSet::default(),
            axioms: Vec::new(),
            n: 0,
            str_lits: FxHashMap::default(),
            float_lits: FxHashMap::default(),
            obligations: Vec::new(),
            returns: Vec::new(),
            loops: Vec::new(),
            inline: 0,
            rec_fields: FxHashMap::default(),
            prelude,
            cur_ret: None,
            depth: 0,
            frames: FxHashMap::default(),
        }
    }

    fn reset(&mut self) {
        self.sorts.clear();
        self.funs.clear();
        self.decls.clear();
        self.declared.clear();
        self.axioms.clear();
        self.n = 0;
        self.str_lits.clear();
        self.float_lits.clear();
        self.obligations.clear();
        self.returns.clear();
        self.loops.clear();
        self.inline = 0;
        self.rec_fields.clear();
        self.depth = 0;
    }

    // ---------- SMT declarations ----------

    fn name(&mut self, hint: &str) -> String {
        self.n += 1;
        let clean: String = hint.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '_').take(24).collect();
        format!("{clean}!{}", self.n)
    }

    fn sort(&mut self, b: &Base, depth: u8) -> String {
        let base = match b {
            Base::Bool => "Bool".to_string(),
            Base::Int(_) | Base::Char | Base::Tag(_) => "Int".to_string(),
            Base::Str => self.opaque_sort("JStr"),
            Base::Float => self.opaque_sort("JFloat"),
            Base::Opaque(t) => self.opaque_sort(&format!("O{}", t.0)),
        };
        let mut s = base;
        for _ in 0..depth {
            s = format!("(Array Int {s})");
        }
        s
    }

    fn opaque_sort(&mut self, name: &str) -> String {
        if self.declared.insert(format!("sort {name}")) {
            self.sorts.push(format!("(declare-sort {name} 0)"));
        }
        name.to_string()
    }

    fn fun(&mut self, name: &str, args: &[String], ret: &str) -> String {
        if self.declared.insert(format!("fun {name}")) {
            self.funs.push(format!("(declare-fun {name} ({}) {ret})", args.join(" ")));
        }
        name.to_string()
    }

    fn str_len(&mut self, t: &str) -> String {
        let s = self.opaque_sort("JStr");
        let f = self.fun("str_len", &[s], "Int");
        format!("({f} {t})")
    }

    fn konst(&mut self, sort: &str, hint: &str) -> String {
        let n = self.name(hint);
        self.decls.push(format!("(declare-const {n} {sort})"));
        n
    }

    /// Name a term (keeps terms small); inline mode returns the term itself.
    fn define(&mut self, sort: &str, term: String) -> String {
        if self.inline > 0 || !term.starts_with('(') {
            return term;
        }
        let n = self.name("v");
        self.decls.push(format!("(define-fun {n} () {sort} {term})"));
        n
    }

    fn define_bool(&mut self, term: String) -> String {
        self.define("Bool", term)
    }

    fn define_int(&mut self, term: String) -> String {
        self.define("Int", term)
    }

    /// Facts about a scalar implied by its type.
    fn wf_fact(&mut self, l: &Leaf) -> Option<String> {
        if l.depth > 0 {
            return None;
        }
        match &l.base {
            Base::Int(k) => Some(range_fact(&l.t, IntTy::min(*k), IntTy::max(*k))),
            Base::Char => Some(range_fact(&l.t, 0, 0x10_FFFF)),
            Base::Tag(n) => Some(range_fact(&l.t, 0, *n as i128 - 1)),
            Base::Str => {
                let len = self.str_len(&l.t);
                Some(range_fact(&len, 0, MAX_LEN))
            }
            _ => None,
        }
    }

    fn wf_facts(&mut self, v: &Sv) -> Vec<String> {
        let mut ls = Vec::new();
        v.leaves(&mut ls);
        let ls: Vec<Leaf> = ls.into_iter().cloned().collect();
        let mut out = Vec::new();
        for l in &ls {
            if let Some(f) = self.wf_fact(l) {
                out.push(f);
            }
        }
        if let Sv::List { len, .. } = v {
            if len.depth == 0 {
                out.push(range_fact(&len.t, 0, MAX_LEN));
            }
        }
        out
    }

    // ---------- types ----------

    fn kind(&self, t: Ty) -> TyKind {
        self.tys.kind(t).clone()
    }

    fn field_tys(&mut self, t: Ty) -> Vec<Ty> {
        match self.kind(t) {
            TyKind::Tuple(ts) => ts.to_vec(),
            TyKind::Adt(a, args) => {
                let fs: Vec<Ty> = self.defs.adts[a].fields().iter().map(|f| f.ty).collect();
                fs.into_iter().map(|f| self.tys.subst(f, &args)).collect()
            }
            _ => vec![],
        }
    }

    fn variant_tys(&mut self, t: Ty) -> Vec<Vec<Ty>> {
        match self.kind(t) {
            TyKind::Adt(a, args) => {
                let vs: Vec<Vec<Ty>> =
                    self.defs.adts[a].variants().iter().map(|v| v.fields.iter().map(|f| f.ty).collect()).collect();
                vs.into_iter().map(|fs| fs.into_iter().map(|f| self.tys.subst(f, &args)).collect()).collect()
            }
            _ => vec![],
        }
    }

    fn elem_ty(&self, t: Ty) -> Ty {
        match self.tys.kind(t) {
            TyKind::List(e) => *e,
            _ => self.tys.error,
        }
    }

    /// A fresh unconstrained value of a type (lifted `depth` times into arrays). Type facts
    /// about scalars at depth 0 become global axioms (the constant is otherwise free).
    fn fresh(&mut self, t: Ty, hint: &str) -> Sv {
        let v = self.fresh_at(t, hint, 0);
        let facts = self.wf_facts_deep(&v);
        self.axioms.extend(facts);
        v
    }

    fn wf_facts_deep(&mut self, v: &Sv) -> Vec<String> {
        let mut out = self.wf_facts(v);
        match v {
            Sv::Tuple(xs) | Sv::Struct(_, xs) => {
                for x in xs {
                    if let Sv::List { .. } = x {
                        out.extend(self.wf_facts_deep(x));
                    }
                }
            }
            Sv::Enum { vars, .. } => {
                for x in vars.iter().flatten() {
                    if let Sv::List { .. } = x {
                        out.extend(self.wf_facts_deep(x));
                    }
                }
            }
            _ => {}
        }
        out.dedup();
        out
    }

    fn leaf(&mut self, base: Base, depth: u8, hint: &str) -> Leaf {
        let sort = self.sort(&base, depth);
        let t = self.konst(&sort, hint);
        Leaf { t, base, depth }
    }

    fn fresh_at(&mut self, t: Ty, hint: &str, depth: u8) -> Sv {
        match self.kind(t) {
            TyKind::Int(k, _) => Sv::Leaf(self.leaf(Base::Int(k), depth, hint)),
            TyKind::Float(_) => Sv::Leaf(self.leaf(Base::Float, depth, hint)),
            TyKind::Bool => Sv::Leaf(self.leaf(Base::Bool, depth, hint)),
            TyKind::Char => Sv::Leaf(self.leaf(Base::Char, depth, hint)),
            TyKind::Str => Sv::Leaf(self.leaf(Base::Str, depth, hint)),
            TyKind::Tuple(ts) if ts.is_empty() => Sv::Unit,
            TyKind::Never | TyKind::Error | TyKind::Infer(_) => Sv::Unit,
            TyKind::Tuple(ts) => Sv::Tuple(
                ts.iter().enumerate().map(|(i, &x)| self.fresh_at(x, &format!("{hint}_{i}"), depth)).collect(),
            ),
            TyKind::Adt(a, _) => {
                let ad = &self.defs.adts[a];
                if ad.recursive {
                    return Sv::Leaf(self.leaf(Base::Opaque(t), depth, hint));
                }
                if ad.is_enum() {
                    let n = ad.variants().len() as u32;
                    let vts = self.variant_tys(t);
                    let tag = self.leaf(Base::Tag(n), depth, &format!("{hint}_tag"));
                    let vars = vts
                        .into_iter()
                        .enumerate()
                        .map(|(vi, fs)| {
                            fs.into_iter()
                                .enumerate()
                                .map(|(i, f)| self.fresh_at(f, &format!("{hint}_{vi}_{i}"), depth))
                                .collect()
                        })
                        .collect();
                    Sv::Enum { adt: a, tag, vars }
                } else {
                    let names: Vec<String> = ad.fields().iter().map(|f| f.name.to_string()).collect();
                    let fts = self.field_tys(t);
                    Sv::Struct(
                        a,
                        fts.into_iter()
                            .zip(names)
                            .map(|(f, n)| self.fresh_at(f, &format!("{hint}_{n}"), depth))
                            .collect(),
                    )
                }
            }
            TyKind::List(e) => {
                let len = self.leaf(Base::Int(IntTy::I64), depth, &format!("{hint}_len"));
                let elems = self.fresh_at(e, &format!("{hint}_e"), depth + 1);
                Sv::List { len, elems: Box::new(elems) }
            }
            TyKind::Map(..) | TyKind::Set(_) | TyKind::Fn(..) | TyKind::Param(..) => {
                Sv::Leaf(self.leaf(Base::Opaque(t), depth, hint))
            }
        }
    }

    // ---------- value operations ----------

    fn select_raw(v: &Sv, i: &str) -> Sv {
        v.map_leaves(&mut |l| Leaf { t: format!("(select {} {i})", l.t), base: l.base.clone(), depth: l.depth - 1 })
    }

    /// Read element `i` of lifted elements, assuming its type facts on this path.
    fn select(&mut self, st: &mut St, v: &Sv, i: &str, elem_ty: Ty) -> Sv {
        let raw = Self::select_raw(v, i);
        let named = raw.map_leaves(&mut |l| l.clone());
        let named = self.name_leaves(&named);
        let facts = self.wf_facts_deep(&named);
        for f in facts {
            self.assume(st, f);
        }
        self.assume_type_preds(st, elem_ty, &named, 0);
        named
    }

    fn name_leaves(&mut self, v: &Sv) -> Sv {
        let mut ls = Vec::new();
        v.leaves(&mut ls);
        let ls: Vec<Leaf> = ls.into_iter().cloned().collect();
        let mut named = Vec::new();
        for l in ls {
            let sort = self.sort(&l.base, l.depth);
            named.push(self.define(&sort, l.t.clone()));
        }
        let mut it = named.into_iter();
        v.map_leaves(&mut |l| Leaf {
            t: it.next().unwrap_or_else(|| l.t.clone()),
            base: l.base.clone(),
            depth: l.depth,
        })
    }

    fn store(&mut self, arr: &Sv, i: &str, val: &Sv) -> Option<Sv> {
        let stored = arr.zip_leaves(val, &mut |a, b| Leaf {
            t: format!("(store {} {i} {})", a.t, b.t),
            base: a.base.clone(),
            depth: a.depth,
        })?;
        Some(self.name_leaves(&stored))
    }

    fn ite(&mut self, c: &str, a: &Sv, b: &Sv, t: Ty) -> Sv {
        if c == "true" {
            return a.clone();
        }
        if c == "false" {
            return b.clone();
        }
        if a.same(b) {
            return a.clone();
        }
        match a.zip_leaves(b, &mut |x, y| {
            if x.t == y.t {
                x.clone()
            } else {
                Leaf { t: format!("(ite {c} {} {})", x.t, y.t), base: x.base.clone(), depth: x.depth }
            }
        }) {
            Some(v) => self.name_leaves(&v),
            None => self.fresh(t, "merge"),
        }
    }

    /// Structural equality as a Bool term; `None` when the values cannot be compared.
    fn eq_term(&mut self, a: &Sv, b: &Sv) -> Option<String> {
        Some(match (a, b) {
            (Sv::Unit, Sv::Unit) => "true".into(),
            (Sv::Leaf(x), Sv::Leaf(y)) => {
                if x.base == Base::Float {
                    let s = self.opaque_sort("JFloat");
                    let f = self.fun("feq", &[s.clone(), s], "Bool");
                    format!("({f} {} {})", x.t, y.t)
                } else if x.t == y.t {
                    "true".into()
                } else {
                    format!("(= {} {})", x.t, y.t)
                }
            }
            (Sv::List { len: la, elems: ea }, Sv::List { len: lb, elems: eb }) => {
                let k = self.name("k");
                let sa = Self::select_raw(ea, &k);
                let sb = Self::select_raw(eb, &k);
                self.inline += 1;
                let inner = self.eq_term(&sa, &sb);
                self.inline -= 1;
                let inner = inner?;
                format!(
                    "(and (= {} {}) (forall (({k} Int)) (=> (and (<= 0 {k}) (< {k} {})) {inner})))",
                    la.t, lb.t, la.t
                )
            }
            (Sv::Tuple(xs), Sv::Tuple(ys)) | (Sv::Struct(_, xs), Sv::Struct(_, ys)) if xs.len() == ys.len() => {
                let mut parts = Vec::new();
                for (x, y) in xs.iter().zip(ys) {
                    parts.push(self.eq_term(x, y)?);
                }
                and(&parts)
            }
            (Sv::Enum { tag: ta, vars: va, .. }, Sv::Enum { tag: tb, vars: vb, .. }) if va.len() == vb.len() => {
                let mut parts = vec![format!("(= {} {})", ta.t, tb.t)];
                for (vi, (x, y)) in va.iter().zip(vb).enumerate() {
                    let mut fs = Vec::new();
                    for (p, q) in x.iter().zip(y) {
                        fs.push(self.eq_term(p, q)?);
                    }
                    let f = and(&fs);
                    if f != "true" {
                        parts.push(format!("(=> (= {} {vi}) {f})", ta.t));
                    }
                }
                and(&parts)
            }
            _ => return None,
        })
    }

    fn fresh_bool(&mut self, hint: &str) -> Sv {
        Sv::Leaf(self.leaf(Base::Bool, 0, hint))
    }

    fn unit_or_fresh(&mut self, t: Ty, hint: &str) -> Sv {
        if self.tys.is_unit(t) {
            Sv::Unit
        } else {
            self.fresh(t, hint)
        }
    }

    // ---------- obligations ----------

    fn assume(&mut self, st: &mut St, fact: String) {
        if fact != "true" {
            st.pc.push(fact);
        }
    }

    fn oblige(&mut self, fr: &Frame, st: &St, kind: ObKind, span: Span, msg: String, goal: String) {
        // Traps inside callee contracts and type predicates are the callee's (or the type's)
        // business; they are checked where that code is verified.
        if self.inline > 0 || (!fr.own && !kind.is_contract()) {
            return;
        }
        let elidable = fr.own;
        self.obligations.push(Obligation { kind, span, msg, pc: st.pc.clone(), goal, elidable });
    }

    fn src(&self, span: Span) -> String {
        let s = self.sm.snippet(span);
        let s: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
        if s.chars().count() > 100 {
            format!("{}…", s.chars().take(97).collect::<String>())
        } else {
            s
        }
    }

    // ---------- predicates ----------

    /// Evaluate a predicate with names bound by name (struct and alias predicates).
    fn pred_named(&mut self, st: &mut St, pkg: PkgId, pred: ExprId, names: Vec<(Symbol, Sv)>) -> Option<String> {
        let fr = Frame { pkg, body: None, inst: Box::new([]), result: None, old: None, names, own: false };
        let v = self.expr(&fr, st, pred)?;
        v.term().map(|s| s.to_string())
    }

    /// The predicates every value of a struct type satisfies: field refinements, field
    /// refinement aliases and invariants. Returns (message, term) pairs.
    fn struct_preds(&mut self, st: &mut St, adt: AdtId, v: &Sv) -> Vec<(String, String)> {
        let Sv::Struct(_, fields) = v else { return vec![] };
        let ad = &self.defs.adts[adt];
        if ad.is_enum() {
            return vec![];
        }
        let pkg = ad.pkg;
        let item = self.pkgs[pkg].ast.items[ad.item];
        let ast::ItemKind::Struct { fields: fdecls, invariants, .. } = item.kind else { return vec![] };
        let ast: &'a Ast = &self.pkgs[pkg].ast;
        let fdecls: Vec<ast::FieldDecl> = ast.fields.get(fdecls).to_vec();
        let invs: Vec<ExprId> = ast.expr_lists.get(invariants).to_vec();
        let aliases: Vec<Option<AliasId>> = ad.fields().iter().map(|f| f.refine).collect();
        let name = ad.name;
        let names: Vec<(Symbol, Sv)> = fdecls.iter().map(|f| f.name.sym).zip(fields.iter().cloned()).collect();
        let mut out = Vec::new();
        for (i, al) in aliases.iter().enumerate() {
            if let Some(al) = al {
                if let Some(t) = self.alias_pred(st, *al, &fields[i]) {
                    out.push((format!("`{name}` field `{}`: {}", fdecls[i].name.sym, t.0), t.1));
                }
            }
        }
        let mut preds: Vec<ExprId> = fdecls.iter().filter_map(|f| f.refine).collect();
        preds.extend(invs);
        for p in preds {
            let text = self.src(ast.exprs[p].span);
            if let Some(t) = self.pred_named(st, pkg, p, names.clone()) {
                out.push((format!("`{name}` requires {text}"), t));
            }
        }
        out
    }

    /// (text, term) of a refinement alias's predicate applied to `v`.
    fn alias_pred(&mut self, st: &mut St, al: AliasId, v: &Sv) -> Option<(String, String)> {
        let ad = &self.defs.aliases[al];
        let pred = ad.refine?;
        let pkg = ad.pkg;
        let name = ad.name;
        let text = self.src(self.pkgs[pkg].ast.exprs[pred].span);
        let t = self.pred_named(st, pkg, pred, vec![(sym::SELF_LOWER, v.clone())])?;
        Some((format!("`{name}` requires {text}"), t))
    }

    /// Assume the predicates of a value's type (structs, recursively through fields).
    fn assume_type_preds(&mut self, st: &mut St, t: Ty, v: &Sv, depth: u32) {
        if depth > 3 {
            return;
        }
        if let (TyKind::Tuple(ts), Sv::Tuple(xs)) = (self.kind(t), v) {
            for (ft, fv) in ts.iter().zip(xs.clone()) {
                self.assume_type_preds(st, *ft, &fv, depth + 1);
            }
            return;
        }
        let TyKind::Adt(adt, _) = self.kind(t) else { return };
        if self.defs.adts[adt].is_enum() {
            // Payloads satisfy their types' predicates when their variant is the active one.
            let Sv::Enum { tag, vars, .. } = v else { return };
            let vts = self.variant_tys(t);
            for (vi, (fs, fts)) in vars.clone().iter().zip(vts).enumerate() {
                for (fv, ft) in fs.iter().zip(fts) {
                    let mut inner = St { env: FxHashMap::default(), pc: Vec::new() };
                    self.assume_type_preds(&mut inner, ft, fv, depth + 1);
                    if !inner.pc.is_empty() {
                        let fact = format!("(=> (= {} {vi}) {})", tag.t, and(&inner.pc));
                        self.assume(st, fact);
                    }
                }
            }
            return;
        }
        let fts = self.field_tys(t);
        if let Sv::Struct(_, fields) = v {
            for (ft, fv) in fts.iter().zip(fields.clone()) {
                self.assume_type_preds(st, *ft, &fv, depth + 1);
            }
        }
        for (_, term) in self.struct_preds(st, adt, v) {
            self.assume(st, term);
        }
    }

    fn oblige_struct_preds(&mut self, fr: &Frame, st: &mut St, t: Ty, v: &Sv, span: Span) {
        let TyKind::Adt(adt, _) = self.kind(t) else { return };
        for (msg, term) in self.struct_preds(st, adt, v) {
            self.oblige(fr, st, ObKind::StructInvariant, span, msg, term.clone());
            self.assume(st, term);
        }
    }

    // ---------- functions ----------

    /// Verification conditions for a non-generic function with a body.
    pub fn function(&mut self, f: FnId) -> Option<FnVcs> {
        self.reset();
        let fd = &self.defs.fns[f];
        let body = self.bodies[f].as_ref()?;
        let pkg = fd.pkg;
        let item = self.pkgs[pkg].ast.items[fd.item];
        let ast::ItemKind::Fn(decl) = item.kind else { return None };
        let bexpr = decl.body?;
        let ast: &'a Ast = &self.pkgs[pkg].ast;
        self.cur_ret = Some(fd.ret);
        let mut fr =
            Frame { pkg, body: Some(body), inst: Box::new([]), result: None, old: None, names: vec![], own: true };
        let mut st = St::default();
        let mut show = Vec::new();
        let mut inputs = Some(Vec::new());
        // Parameters: fresh values satisfying their types, refinements and preconditions.
        let ast_params: Vec<ast::Param> = ast.params.get(decl.params).to_vec();
        let offset = fd.self_mode.is_some() as usize;
        let refines: Vec<Option<AliasId>> = fd.params.iter().map(|p| p.refine).collect();
        for (i, &id) in body.param_locals.iter().enumerate() {
            let ld = &body.locals[id];
            let (t, name) = (ld.ty, ld.name);
            let v = self.fresh(t, name.as_str());
            self.show_leaves(name.as_str(), &v, t, &mut show);
            match (&mut inputs, scalar_leaves(&v)) {
                (Some(ins), Some(ls)) => ins.push((t, ls)),
                _ => inputs = None,
            }
            self.assume_type_preds(&mut st, t, &v, 0);
            if i >= offset {
                if let Some(Some(al)) = refines.get(i - offset) {
                    if let Some((_, term)) = self.alias_pred(&mut st, *al, &v) {
                        self.assume(&mut st, term);
                    }
                }
            }
            st.env.insert(id, v);
        }
        for p in &ast_params {
            if let Some(pred) = p.refine {
                if let Some(v) = self.expr(&fr, &mut st, pred) {
                    if let Some(t) = v.term() {
                        let t = t.to_string();
                        self.assume(&mut st, t);
                    }
                }
            }
        }
        for &r in ast.expr_lists.get(decl.requires) {
            if let Some(v) = self.expr(&fr, &mut st, r) {
                if let Some(t) = v.term() {
                    let t = t.to_string();
                    self.assume(&mut st, t);
                }
            }
        }
        let old_env = st.env.clone();
        let ret_ty = fd.ret;
        let ret_refine = fd.ret_refine;
        let fspan = fd.span;
        let mut_params: Vec<LocalId> = body
            .param_locals
            .iter()
            .enumerate()
            .filter(|(i, _)| {
                if *i < offset {
                    fd.self_mode == Some(Mode::MutRef)
                } else {
                    fd.params[*i - offset].mode == Mode::MutRef
                }
            })
            .map(|(_, &id)| id)
            .collect();
        // The body.
        if let Some(v) = self.block(&fr, &mut st, bexpr) {
            let v = if self.tys.is_unit(ret_ty) { Sv::Unit } else { v };
            self.returns.push((st, v));
        }
        // Postconditions on every return path.
        let returns = std::mem::take(&mut self.returns);
        let ensures: Vec<ExprId> = ast.expr_lists.get(decl.ensures).to_vec();
        fr.old = Some(old_env);
        for (mut rs, rv) in returns {
            if let Some(al) = ret_refine {
                if let Some((msg, term)) = self.alias_pred(&mut rs, al, &rv) {
                    self.oblige(&fr, &rs, ObKind::Refinement, fspan, format!("return value: {msg}"), term);
                }
            }
            for &id in &mut_params {
                let t = body.locals[id].ty;
                if let Some(v) = rs.env.get(&id).cloned() {
                    if let TyKind::Adt(adt, _) = self.kind(t) {
                        for (msg, term) in self.struct_preds(&mut rs, adt, &v) {
                            self.oblige(&fr, &rs, ObKind::StructInvariant, fspan, format!("on return: {msg}"), term);
                        }
                    }
                }
            }
            fr.result = Some(rv);
            for &en in &ensures {
                let span = ast.exprs[en].span;
                let text = self.src(span);
                let mut s2 = rs.clone();
                if let Some(v) = self.expr(&fr, &mut s2, en) {
                    if let Some(t) = v.term() {
                        let t = t.to_string();
                        self.oblige(&fr, &s2, ObKind::Postcondition, span, text, t);
                    }
                }
            }
        }
        let mut decls = std::mem::take(&mut self.sorts);
        decls.append(&mut self.funs);
        decls.append(&mut self.decls);
        let lits: Vec<String> = self.str_lits.values().cloned().collect();
        if lits.len() > 1 {
            decls.push(format!("(assert (distinct {}))", lits.join(" ")));
        }
        for a in std::mem::take(&mut self.axioms) {
            decls.push(format!("(assert {a})"));
        }
        Some(FnVcs { decls, obligations: std::mem::take(&mut self.obligations), show, inputs })
    }

    fn show_leaves(&mut self, label: &str, v: &Sv, t: Ty, out: &mut Vec<(String, String, Base)>) {
        match v {
            Sv::Leaf(l) if l.depth == 0 => match &l.base {
                Base::Str => {
                    let len = self.str_len(&l.t);
                    out.push((format!("{label}.len()"), len, Base::Int(IntTy::I64)));
                }
                Base::Float | Base::Opaque(_) => {}
                b => out.push((label.to_string(), l.t.clone(), b.clone())),
            },
            Sv::List { len, .. } if len.depth == 0 => {
                out.push((format!("{label}.len()"), len.t.clone(), Base::Int(IntTy::I64)));
            }
            Sv::Struct(adt, fs) => {
                let names: Vec<String> = self.defs.adts[*adt].fields().iter().map(|f| f.name.to_string()).collect();
                let fts = self.field_tys(t);
                for ((n, fv), ft) in names.iter().zip(fs).zip(fts) {
                    self.show_leaves(&format!("{label}.{n}"), fv, ft, out);
                }
            }
            Sv::Tuple(xs) => {
                let fts = self.field_tys(t);
                for (i, (x, ft)) in xs.iter().zip(fts).enumerate() {
                    self.show_leaves(&format!("{label}.{i}"), x, ft, out);
                }
            }
            _ => {}
        }
    }

    // ---------- statements ----------

    fn ast(&self, pkg: PkgId) -> &'a Ast {
        &self.pkgs[pkg].ast
    }

    fn ty(&mut self, fr: &Frame, e: ExprId) -> Ty {
        let t = self.tables[fr.pkg].expr_ty[e.0 as usize];
        self.tys.subst(t, &fr.inst)
    }

    fn res(&self, fr: &Frame, e: ExprId) -> Res {
        self.tables[fr.pkg].expr_res[e.0 as usize]
    }

    fn local_ty(&mut self, fr: &Frame, id: LocalId) -> Ty {
        let t = fr.body.map(|b| b.locals[id].ty).unwrap_or(self.tys.error);
        self.tys.subst(t, &fr.inst)
    }

    /// Evaluate a block; `None` if every path leaves it (return, break, panic).
    fn block(&mut self, fr: &Frame, st: &mut St, e: ExprId) -> Option<Sv> {
        let ast = self.ast(fr.pkg);
        let ExprKind::Block(stmts) = ast.exprs[e].kind else { return self.expr(fr, st, e) };
        let ids: Vec<StmtId> = ast.stmt_lists.get(stmts).to_vec();
        let n = ids.len();
        let mut last = Sv::Unit;
        for (i, s) in ids.into_iter().enumerate() {
            if i + 1 == n {
                if let StmtKind::Expr(x) = ast.stmts[s].kind {
                    last = self.expr(fr, st, x)?;
                    continue;
                }
            }
            self.stmt(fr, st, s)?;
        }
        Some(last)
    }

    fn stmt(&mut self, fr: &Frame, st: &mut St, s: StmtId) -> Option<()> {
        let ast = self.ast(fr.pkg);
        let stmt = ast.stmts[s];
        match stmt.kind {
            StmtKind::Let { pat, init, .. } => {
                let v = self.expr(fr, st, init)?;
                let t = self.ty(fr, init);
                self.bind(fr, st, pat, &v, t);
                Some(())
            }
            StmtKind::Assign { target, op, value } => {
                let Some(pl) = self.place(fr, st, target) else {
                    // Not a place we track: evaluate for its obligations only.
                    self.expr(fr, st, value)?;
                    return Some(());
                };
                let tt = *pl.tys.last().unwrap();
                let new = match op {
                    None => self.expr(fr, st, value)?,
                    Some(op) => {
                        let old = self.read_place(st, &pl);
                        let rhs = self.expr(fr, st, value)?;
                        if matches!(self.kind(tt), TyKind::Str) {
                            self.concat(&old, &rhs)
                        } else {
                            self.binary_vals(fr, st, op, &old, &rhs, tt, stmt.span)?
                        }
                    }
                };
                self.write_place(fr, st, &pl, new, stmt.span);
                Some(())
            }
            StmtKind::Expr(e) => self.expr(fr, st, e).map(|_| ()),
            StmtKind::Return(v) => {
                let rv = match v {
                    Some(e) => self.expr(fr, st, e)?,
                    None => Sv::Unit,
                };
                self.returns.push((st.clone(), rv));
                None
            }
            StmtKind::Break => {
                if let Some(l) = self.loops.last_mut() {
                    l.breaks.push(st.clone());
                }
                None
            }
            StmtKind::Continue => {
                if let Some(l) = self.loops.last_mut() {
                    l.conts.push(st.clone());
                }
                None
            }
            StmtKind::For { head, invariants, body } => self.for_loop(fr, st, head, invariants, body),
        }
    }

    /// Bind an irrefutable pattern.
    fn bind(&mut self, fr: &Frame, st: &mut St, pat: PatId, v: &Sv, t: Ty) {
        let ast = self.ast(fr.pkg);
        match ast.pats[pat].kind {
            PatKind::Bind(_) | PatKind::Variant { .. } => {
                if let Some(&id) = self.tables[fr.pkg].pat_local.get(&pat) {
                    st.env.insert(id, v.clone());
                }
            }
            PatKind::Tuple(list) => {
                let subs: Vec<PatId> = ast.pat_lists.get(list).to_vec();
                let fts = self.field_tys(t);
                for (i, sp) in subs.into_iter().enumerate() {
                    let (fv, ft) = match v {
                        Sv::Tuple(xs) if i < xs.len() => (xs[i].clone(), fts.get(i).copied().unwrap_or(self.tys.error)),
                        _ => {
                            let ft = fts.get(i).copied().unwrap_or(self.tys.error);
                            (self.fresh(ft, "pat"), ft)
                        }
                    };
                    self.bind(fr, st, sp, &fv, ft);
                }
            }
            _ => {}
        }
    }

    // ---------- merging ----------

    /// Join states that forked from a common prefix of `base` path facts.
    fn merge(&mut self, base: usize, states: Vec<(St, Sv)>, t: Ty) -> Option<(St, Sv)> {
        let mut it = states.into_iter();
        let (mut acc, mut accv) = it.next()?;
        for (s, v) in it {
            let ga = and(&acc.pc[base.min(acc.pc.len())..]);
            let gb = and(&s.pc[base.min(s.pc.len())..]);
            let ga = self.define_bool(ga);
            let mut env = FxHashMap::default();
            let keys: Vec<LocalId> = acc.env.keys().copied().collect();
            for k in keys {
                let (Some(a), Some(b)) = (acc.env.get(&k), s.env.get(&k)) else { continue };
                if a.same(b) {
                    env.insert(k, a.clone());
                    continue;
                }
                let (a, b) = (a.clone(), b.clone());
                let lt = self.tys.error;
                let m = match a.zip_leaves(&b, &mut |x, y| {
                    if x.t == y.t {
                        x.clone()
                    } else {
                        Leaf { t: format!("(ite {ga} {} {})", x.t, y.t), base: x.base.clone(), depth: x.depth }
                    }
                }) {
                    Some(m) => self.name_leaves(&m),
                    None => self.fresh(lt, "merge"),
                };
                env.insert(k, m);
            }
            let v = if accv.same(&v) { accv.clone() } else { self.ite(&ga, &accv, &v, t) };
            let mut pc: Vec<String> = acc.pc[..base.min(acc.pc.len())].to_vec();
            let joined = or2(&ga, &gb);
            if joined != "true" {
                pc.push(self.define_bool(joined));
            }
            acc = St { env, pc };
            accv = v;
        }
        Some((acc, accv))
    }

    // ---------- places ----------

    fn place(&mut self, fr: &Frame, st: &mut St, e: ExprId) -> Option<PlaceRef> {
        let ast = self.ast(fr.pkg);
        match ast.exprs[e].kind {
            ExprKind::Name(_) => match self.res(fr, e) {
                Res::Local(id) => {
                    let t = self.local_ty(fr, id);
                    Some(PlaceRef { local: id, path: vec![], tys: vec![t] })
                }
                _ => None,
            },
            ExprKind::Paren(x) | ExprKind::MutArg(x) => self.place(fr, st, x),
            ExprKind::Field { base, .. } => {
                let Res::Field(i) = self.res(fr, e) else { return None };
                let mut p = self.place(fr, st, base)?;
                let t = self.ty(fr, e);
                p.path.push(Seg::Field(i));
                p.tys.push(t);
                Some(p)
            }
            ExprKind::TupleField { base, index, .. } => {
                let mut p = self.place(fr, st, base)?;
                let t = self.ty(fr, e);
                p.path.push(Seg::Field(index));
                p.tys.push(t);
                Some(p)
            }
            ExprKind::Index { base, index } => {
                if matches!(ast.exprs[index].kind, ExprKind::Range { .. }) {
                    return None;
                }
                let bt = self.ty(fr, base);
                if !matches!(self.kind(bt), TyKind::List(_)) {
                    return None;
                }
                let mut p = self.place(fr, st, base)?;
                let i = self.expr(fr, st, index)?;
                let it = i.term()?.to_string();
                let t = self.ty(fr, e);
                p.path.push(Seg::Index(it, ast.exprs[e].span));
                p.tys.push(t);
                Some(p)
            }
            _ => None,
        }
    }

    fn read_place(&mut self, st: &mut St, p: &PlaceRef) -> Sv {
        let mut v = match st.env.get(&p.local) {
            Some(v) => v.clone(),
            None => {
                let v = self.fresh(p.tys[0], "local");
                st.env.insert(p.local, v.clone());
                v
            }
        };
        for (k, seg) in p.path.iter().enumerate() {
            let t = p.tys[k + 1];
            v = match (seg, &v) {
                (Seg::Field(i), Sv::Struct(_, xs) | Sv::Tuple(xs)) if (*i as usize) < xs.len() => {
                    xs[*i as usize].clone()
                }
                (Seg::Index(i, _), Sv::List { elems, .. }) => {
                    let elems = (**elems).clone();
                    self.select(st, &elems, i, t)
                }
                _ => self.fresh(t, "part"),
            };
        }
        v
    }

    fn read_place_raw(&mut self, st: &mut St, p: &PlaceRef) -> Sv {
        let mut v = match st.env.get(&p.local) {
            Some(v) => v.clone(),
            None => return self.fresh(*p.tys.last().unwrap(), "part"),
        };
        for (k, seg) in p.path.iter().enumerate() {
            v = match (seg, &v) {
                (Seg::Field(i), Sv::Struct(_, xs) | Sv::Tuple(xs)) if (*i as usize) < xs.len() => {
                    xs[*i as usize].clone()
                }
                (Seg::Index(i, _), Sv::List { elems, .. }) => {
                    let raw = Self::select_raw(elems, i);
                    self.name_leaves(&raw)
                }
                _ => self.fresh(p.tys[k + 1], "part"),
            };
        }
        v
    }

    /// Bounds obligations for each index on the path (the read or write traps otherwise).
    fn oblige_place_bounds(&mut self, fr: &Frame, st: &mut St, p: &PlaceRef) {
        let mut v = st.env.get(&p.local).cloned();
        for (k, seg) in p.path.iter().enumerate() {
            let t = p.tys[k + 1];
            v = match (seg, v) {
                (Seg::Field(i), Some(Sv::Struct(_, xs) | Sv::Tuple(xs))) => xs.get(*i as usize).cloned(),
                (Seg::Index(i, span), Some(Sv::List { len, elems })) => {
                    let goal = format!("(and (<= 0 {i}) (< {i} {}))", len.t);
                    let msg = format!("index in bounds: {}", self.src(*span));
                    self.oblige(fr, st, ObKind::Bounds, *span, msg, goal.clone());
                    self.assume(st, goal);
                    Some(self.select(st, &elems, i, t))
                }
                _ => None,
            };
        }
    }

    fn write_place(&mut self, fr: &Frame, st: &mut St, p: &PlaceRef, new: Sv, span: Span) {
        self.oblige_place_bounds(fr, st, p);
        let root = match st.env.get(&p.local) {
            Some(v) => v.clone(),
            None => self.fresh(p.tys[0], "local"),
        };
        let updated = self.update(st, &root, &p.path, &p.tys, 0, new.clone());
        st.env.insert(p.local, updated);
        if p.path.is_empty() {
            return;
        }
        // Mirror the runtime: the assigned field's refinement alias, then the predicates of
        // every struct on the path, innermost first.
        if let (Some(Seg::Field(i)), TyKind::Adt(adt, _)) = (p.path.last(), self.kind(p.tys[p.tys.len() - 2])) {
            if let Some(al) = self.defs.adts[adt].fields().get(*i as usize).and_then(|f| f.refine) {
                if let Some((msg, term)) = self.alias_pred(st, al, &new) {
                    self.oblige(fr, st, ObKind::StructInvariant, span, msg, term.clone());
                    self.assume(st, term);
                }
            }
        }
        for k in (0..p.path.len()).rev() {
            let q = PlaceRef { local: p.local, path: p.path[..k].to_vec(), tys: p.tys[..=k].to_vec() };
            let t = q.tys[k];
            if let TyKind::Adt(adt, _) = self.kind(t) {
                if !self.defs.adts[adt].is_enum() {
                    // Read without assuming the predicates we are about to check.
                    let v = self.read_place_raw(st, &q);
                    self.oblige_struct_preds(fr, st, t, &v, span);
                }
            }
        }
    }

    fn update(&mut self, st: &mut St, v: &Sv, path: &[Seg], tys: &[Ty], k: usize, new: Sv) -> Sv {
        let Some(seg) = path.get(k) else { return new };
        match (seg, v) {
            (Seg::Field(i), Sv::Struct(a, xs)) if (*i as usize) < xs.len() => {
                let mut xs = xs.clone();
                xs[*i as usize] = self.update(st, &xs[*i as usize], path, tys, k + 1, new);
                Sv::Struct(*a, xs)
            }
            (Seg::Field(i), Sv::Tuple(xs)) if (*i as usize) < xs.len() => {
                let mut xs = xs.clone();
                xs[*i as usize] = self.update(st, &xs[*i as usize], path, tys, k + 1, new);
                Sv::Tuple(xs)
            }
            (Seg::Index(i, _), Sv::List { len, elems }) => {
                let cur = self.select(st, elems, i, tys[k + 1]);
                let inner = self.update(st, &cur, path, tys, k + 1, new);
                match self.store(elems, i, &inner) {
                    Some(e) => Sv::List { len: len.clone(), elems: Box::new(e) },
                    None => {
                        let fresh = self.fresh(tys[k], "list");
                        match fresh {
                            Sv::List { elems, .. } => Sv::List { len: len.clone(), elems },
                            other => other,
                        }
                    }
                }
            }
            _ => self.fresh(tys[k], "part"),
        }
    }

    // ---------- loops ----------

    /// The places a loop body may modify: (local, path of field indices, whether the change
    /// may alter list lengths below that path).
    fn modified(&self, fr: &Frame, e: ExprId, out: &mut Vec<(LocalId, Vec<u32>, bool)>) {
        let ast = self.ast(fr.pkg);
        let mut stack = vec![e];
        while let Some(x) = stack.pop() {
            if let ExprKind::Block(stmts) = ast.exprs[x].kind {
                for &s in ast.stmt_lists.get(stmts) {
                    if let StmtKind::Assign { target, .. } = ast.stmts[s].kind {
                        if let Some(m) = self.place_root(fr, target) {
                            out.push(m);
                        }
                    }
                }
            }
            match ast.exprs[x].kind {
                ExprKind::MutArg(p) => {
                    if let Some((l, path, _)) = self.place_root(fr, p) {
                        out.push((l, path, true));
                    }
                }
                ExprKind::Call { callee, .. } => {
                    if let (Res::Method(f), ExprKind::Field { base, .. }) =
                        (self.res(fr, callee), ast.exprs[callee].kind)
                    {
                        let fd = &self.defs.fns[f];
                        if fd.self_mode == Some(Mode::MutRef) {
                            // Reordering a list in place keeps its length.
                            let reorders = !fd.has_body
                                && Some(fd.pkg) == self.prelude
                                && matches!(fd.name.as_str(), "swap" | "reverse" | "sort" | "sort_by");
                            if let Some((l, path, whole)) = self.place_root(fr, base) {
                                out.push((l, path, whole && !reorders));
                            }
                        }
                    }
                }
                ExprKind::Closure { .. } => continue,
                _ => {}
            }
            crate::mir::lower::visit_children(ast, x, &mut stack);
        }
    }

    /// The root local of a place expression and the field path down to the first index.
    /// The flag says whether the assignment may change a list's length at that path.
    fn place_root(&self, fr: &Frame, e: ExprId) -> Option<(LocalId, Vec<u32>, bool)> {
        let ast = self.ast(fr.pkg);
        match ast.exprs[e].kind {
            ExprKind::Name(_) => match self.res(fr, e) {
                Res::Local(id) => Some((id, vec![], true)),
                _ => None,
            },
            ExprKind::Paren(x) => self.place_root(fr, x),
            ExprKind::Field { base, .. } => {
                let Res::Field(i) = self.res(fr, e) else { return None };
                let (l, mut p, whole) = self.place_root(fr, base)?;
                if whole {
                    p.push(i);
                }
                Some((l, p, whole))
            }
            ExprKind::TupleField { base, index, .. } => {
                let (l, mut p, whole) = self.place_root(fr, base)?;
                if whole {
                    p.push(index);
                }
                Some((l, p, whole))
            }
            ExprKind::Index { base, .. } => {
                // Writing an element keeps the list's length.
                let (l, p, _) = self.place_root(fr, base)?;
                Some((l, p, false))
            }
            _ => None,
        }
    }

    /// Locals a loop body changes only by unconditional top-level `push` statements, with
    /// how many per iteration. Requires a body with no early exit, so every iteration that
    /// starts runs every push: then a list's length at iteration `i` is `len0 + k*(i - lo)`.
    fn push_counts(&self, fr: &Frame, body: ExprId, mods: &[(LocalId, Vec<u32>, bool)]) -> Vec<(LocalId, usize)> {
        let ast = self.ast(fr.pkg);
        let ExprKind::Block(stmts) = ast.exprs[body].kind else { return vec![] };
        // No break, continue, return or `?` anywhere inside.
        let mut stack = vec![body];
        while let Some(x) = stack.pop() {
            match ast.exprs[x].kind {
                ExprKind::Try(_) => return vec![],
                ExprKind::Block(ss) => {
                    for &s in ast.stmt_lists.get(ss) {
                        if matches!(ast.stmts[s].kind, StmtKind::Break | StmtKind::Continue | StmtKind::Return(_)) {
                            return vec![];
                        }
                    }
                }
                ExprKind::Closure { .. } => continue,
                _ => {}
            }
            crate::mir::lower::visit_children(ast, x, &mut stack);
        }
        let mut counts: FxHashMap<LocalId, usize> = FxHashMap::default();
        for &s in ast.stmt_lists.get(stmts) {
            let StmtKind::Expr(e) = ast.stmts[s].kind else { continue };
            let ExprKind::Call { callee, .. } = ast.exprs[e].kind else { continue };
            let ExprKind::Field { base, name } = ast.exprs[callee].kind else { continue };
            let Res::Method(f) = self.res(fr, callee) else { continue };
            let fd = &self.defs.fns[f];
            if name.sym.as_str() != "push" || fd.has_body || Some(fd.pkg) != self.prelude {
                continue;
            }
            if let (ExprKind::Name(_), Res::Local(l)) = (ast.exprs[base].kind, self.res(fr, base)) {
                *counts.entry(l).or_insert(0) += 1;
            }
        }
        // Every modification of the local must be one of those pushes.
        counts.into_iter().filter(|(l, k)| mods.iter().filter(|(m, _, _)| m == l).count() == *k).collect()
    }

    fn havoc(&mut self, fr: &Frame, st: &mut St, mods: &[(LocalId, Vec<u32>, bool)]) {
        for (l, path, whole) in mods {
            let Some(cur) = st.env.get(l).cloned() else { continue };
            let t = self.local_ty(fr, *l);
            let new = self.havoc_at(st, &cur, t, path, *whole);
            st.env.insert(*l, new);
        }
    }

    fn havoc_at(&mut self, st: &mut St, v: &Sv, t: Ty, path: &[u32], whole: bool) -> Sv {
        if let Some((&i, rest)) = path.split_first() {
            let fts = self.field_tys(t);
            return match v {
                Sv::Struct(a, xs) if (i as usize) < xs.len() => {
                    let mut xs = xs.clone();
                    xs[i as usize] = self.havoc_at(st, &xs[i as usize], fts[i as usize], rest, whole);
                    Sv::Struct(*a, xs)
                }
                Sv::Tuple(xs) if (i as usize) < xs.len() => {
                    let mut xs = xs.clone();
                    xs[i as usize] = self.havoc_at(st, &xs[i as usize], fts[i as usize], rest, whole);
                    Sv::Tuple(xs)
                }
                _ => self.fresh_assumed(st, t),
            };
        }
        if !whole {
            // Only list elements change: keep the length.
            if let (Sv::List { len, .. }, TyKind::List(e)) = (v, self.kind(t)) {
                let elems = self.fresh_at(e, "elems", len.depth + 1);
                return Sv::List { len: len.clone(), elems: Box::new(elems) };
            }
        }
        self.fresh_assumed(st, t)
    }

    fn fresh_assumed(&mut self, st: &mut St, t: Ty) -> Sv {
        let v = self.fresh(t, "havoc");
        self.assume_type_preds(st, t, &v, 0);
        v
    }

    fn for_loop(
        &mut self,
        fr: &Frame,
        st: &mut St,
        head: ForHead,
        invariants: crate::util::List<ExprId>,
        body: ExprId,
    ) -> Option<()> {
        let ast = self.ast(fr.pkg);
        let invs: Vec<ExprId> = ast.expr_lists.get(invariants).to_vec();
        let mut mods = Vec::new();
        self.modified(fr, body, &mut mods);
        if let ForHead::Cond(c) = head {
            self.modified(fr, c, &mut mods);
        }
        let base = st.pc.len();
        let mut exits: Vec<(St, Sv)> = Vec::new();
        // The iteration space: an index range, an optional element source and pattern.
        enum Iter {
            Range { lo: String, hi: String, k: IntTy, pat: PatId },
            Coll { list: Sv, n: String, pat: PatId, elem: Ty },
            Cond(Option<ExprId>),
        }
        let iter = match head {
            ForHead::In { pat, iter } => {
                if let ExprKind::Range { lo: Some(lo), hi: Some(hi) } = ast.exprs[iter].kind {
                    let t = self.ty(fr, lo);
                    let k = match self.kind(t) {
                        TyKind::Int(k, _) => k,
                        _ => IntTy::I64,
                    };
                    let l = self.expr(fr, st, lo)?;
                    let h = self.expr(fr, st, hi)?;
                    match (l.term(), h.term()) {
                        (Some(l), Some(h)) => Iter::Range { lo: l.to_string(), hi: h.to_string(), k, pat },
                        _ => return self.opaque_loop(fr, st, &mods, body),
                    }
                } else {
                    let it = self.ty(fr, iter);
                    let v = self.expr(fr, st, iter)?;
                    match (self.kind(it), v) {
                        (TyKind::List(e), Sv::List { len, elems }) => {
                            let n = len.t.clone();
                            Iter::Coll { list: Sv::List { len, elems }, n, pat, elem: e }
                        }
                        (TyKind::Set(e), _) => {
                            let lt = self.tys.list(e);
                            let l = self.fresh(lt, "items");
                            let Sv::List { len, .. } = &l else { return self.opaque_loop(fr, st, &mods, body) };
                            let n = len.t.clone();
                            Iter::Coll { list: l, n, pat, elem: e }
                        }
                        (TyKind::Map(k, v), _) => {
                            let tup = self.tys.tuple(vec![k, v]);
                            let lt = self.tys.list(tup);
                            let l = self.fresh(lt, "entries");
                            let Sv::List { len, .. } = &l else { return self.opaque_loop(fr, st, &mods, body) };
                            let n = len.t.clone();
                            Iter::Coll { list: l, n, pat, elem: tup }
                        }
                        _ => return self.opaque_loop(fr, st, &mods, body),
                    }
                }
            }
            ForHead::Cond(c) => Iter::Cond(Some(c)),
            ForHead::Infinite => Iter::Cond(None),
        };
        // Bind the iteration variable for index `i` in state `s`.
        let bind_iter = |g: &mut Self, s: &mut St, i: &str| match &iter {
            Iter::Range { k, pat, .. } => {
                let t = g.tys.int_ty(*k, NO_UNIT);
                g.bind(fr, s, *pat, &Sv::int(i, *k), t);
            }
            Iter::Coll { list: Sv::List { elems, .. }, pat, elem, .. } => {
                let elems = (**elems).clone();
                let x = g.select(s, &elems, i, *elem);
                g.bind(fr, s, *pat, &x, *elem);
            }
            _ => {}
        };
        let (lo, hi) = match &iter {
            Iter::Range { lo, hi, .. } => (lo.clone(), hi.clone()),
            Iter::Coll { n, .. } => ("0".to_string(), n.clone()),
            Iter::Cond(_) => (String::new(), String::new()),
        };
        let indexed = !matches!(iter, Iter::Cond(_));
        // First iteration: the invariants must hold on entry.
        let mut entry = st.clone();
        if indexed {
            let enter = format!("(< {lo} {hi})");
            let mut skip = st.clone();
            self.assume(&mut skip, not(&enter));
            exits.push((skip, Sv::Unit));
            self.assume(&mut entry, enter);
            bind_iter(self, &mut entry, &lo);
        } else if let Iter::Cond(Some(c)) = iter {
            let cv = self.expr(fr, &mut entry, c)?;
            let ct = cv.term().unwrap_or("true").to_string();
            let mut skip = entry.clone();
            self.assume(&mut skip, not(&ct));
            exits.push((skip, Sv::Unit));
            self.assume(&mut entry, ct);
        }
        for &inv in &invs {
            let span = ast.exprs[inv].span;
            let text = self.src(span);
            let mut s2 = entry.clone();
            if let Some(v) = self.expr(fr, &mut s2, inv) {
                if let Some(t) = v.term() {
                    let t = t.to_string();
                    self.oblige(fr, &s2, ObKind::LoopInvariant, span, format!("{text} (on entry)"), t);
                }
            }
        }
        // Lists the body only pushes to: their length is known at every iteration.
        let counted: Vec<(LocalId, usize, String)> = if indexed {
            self.push_counts(fr, body, &mods)
                .into_iter()
                .filter_map(|(l, k)| match st.env.get(&l) {
                    Some(Sv::List { len, .. }) if len.depth == 0 => Some((l, k, len.t.clone())),
                    _ => None,
                })
                .collect()
        } else {
            Vec::new()
        };
        // An arbitrary iteration: havoc what the loop modifies, assume the invariants.
        let mut it = st.clone();
        self.havoc(fr, &mut it, &mods);
        let idx = if indexed {
            let k = match &iter {
                Iter::Range { k, .. } => *k,
                _ => IntTy::I64,
            };
            let sort = "Int".to_string();
            let i = self.konst(&sort, "i");
            self.assume(&mut it, format!("(and (<= {lo} {i}) (< {i} {hi}))"));
            let _ = k;
            bind_iter(self, &mut it, &i);
            for (l, k, len0) in &counted {
                if let Some(Sv::List { len, .. }) = it.env.get(l) {
                    let fact = format!("(= {} (+ {len0} (* {k} (- {i} {lo}))))", len.t);
                    self.assume(&mut it, fact);
                }
            }
            i
        } else {
            String::new()
        };
        // The invariants hold at the top of every iteration, before the condition runs.
        for &inv in &invs {
            if let Some(v) = self.expr(fr, &mut it, inv) {
                if let Some(t) = v.term() {
                    let t = t.to_string();
                    self.assume(&mut it, t);
                }
            }
        }
        if let Iter::Cond(Some(c)) = iter {
            let cv = self.expr(fr, &mut it, c)?;
            let ct = cv.term().unwrap_or("true").to_string();
            self.assume(&mut it, ct);
        }
        let it_base = it.pc.len();
        self.loops.push(LoopFrame { breaks: Vec::new(), conts: Vec::new() });
        let end = self.block(fr, &mut it, body);
        let frame = self.loops.pop().unwrap_or(LoopFrame { breaks: Vec::new(), conts: Vec::new() });
        let mut latch_states: Vec<(St, Sv)> = frame.conts.into_iter().map(|s| (s, Sv::Unit)).collect();
        if end.is_some() {
            latch_states.push((it, Sv::Unit));
        }
        let unit = self.tys.unit;
        if let Some((latch, _)) = self.merge(it_base, latch_states, unit) {
            // The next iteration (if any) must satisfy the invariants again.
            let mut next = latch.clone();
            let (cont, next_idx) = if indexed {
                let ni = self.define_int(format!("(+ {idx} 1)"));
                (format!("(< {ni} {hi})"), ni)
            } else if let Iter::Cond(Some(c)) = iter {
                let cv = self.expr(fr, &mut next, c)?;
                (cv.term().unwrap_or("true").to_string(), String::new())
            } else {
                ("true".to_string(), String::new())
            };
            let mut again = next.clone();
            self.assume(&mut again, cont.clone());
            if indexed {
                bind_iter(self, &mut again, &next_idx);
            }
            for &inv in &invs {
                let span = ast.exprs[inv].span;
                let text = self.src(span);
                let mut s2 = again.clone();
                if let Some(v) = self.expr(fr, &mut s2, inv) {
                    if let Some(t) = v.term() {
                        let t = t.to_string();
                        self.oblige(fr, &s2, ObKind::LoopInvariant, span, format!("{text} (preserved)"), t);
                    }
                }
            }
            let mut done = next;
            self.assume(&mut done, not(&cont));
            exits.push((done, Sv::Unit));
        }
        exits.extend(frame.breaks.into_iter().map(|s| (s, Sv::Unit)));
        match self.merge(base, exits, unit) {
            Some((s, _)) => {
                *st = s;
                Some(())
            }
            None => None,
        }
    }

    /// A loop we cannot model: havoc what it modifies and continue.
    fn opaque_loop(
        &mut self,
        fr: &Frame,
        st: &mut St,
        mods: &[(LocalId, Vec<u32>, bool)],
        _body: ExprId,
    ) -> Option<()> {
        self.havoc(fr, st, mods);
        Some(())
    }

    // ---------- expressions ----------

    fn str_lit(&mut self, s: Symbol) -> Sv {
        if let Some(t) = self.str_lits.get(&s) {
            return Sv::Leaf(Leaf { t: t.clone(), base: Base::Str, depth: 0 });
        }
        let sort = self.opaque_sort("JStr");
        let t = self.konst(&sort, "str");
        let len = self.str_len(&t);
        self.axioms.push(format!("(= {len} {})", s.as_str().len()));
        self.str_lits.insert(s, t.clone());
        Sv::Leaf(Leaf { t, base: Base::Str, depth: 0 })
    }

    fn float_lit(&mut self, f: f64) -> Sv {
        let bits = f.to_bits();
        if let Some(t) = self.float_lits.get(&bits) {
            return Sv::Leaf(Leaf { t: t.clone(), base: Base::Float, depth: 0 });
        }
        let sort = self.opaque_sort("JFloat");
        let t = self.konst(&sort, "flt");
        self.float_lits.insert(bits, t.clone());
        Sv::Leaf(Leaf { t, base: Base::Float, depth: 0 })
    }

    fn concat(&mut self, a: &Sv, b: &Sv) -> Sv {
        let (Some(x), Some(y)) = (a.term(), b.term()) else {
            let t = self.tys.str;
            return self.fresh(t, "str");
        };
        let s = self.opaque_sort("JStr");
        let f = self.fun("str_cat", &[s.clone(), s.clone()], &s);
        let t = self.define(&s, format!("({f} {x} {y})"));
        let (lt, lx, ly) = (self.str_len(&t), self.str_len(x), self.str_len(y));
        self.axioms.push(format!("(= {lt} (+ {lx} {ly}))"));
        Sv::Leaf(Leaf { t, base: Base::Str, depth: 0 })
    }

    fn lit_of_ty(&mut self, v: i128, t: Ty) -> Sv {
        match self.kind(t) {
            TyKind::Int(k, _) => Sv::int(int_lit(v), k),
            TyKind::Float(_) => self.float_lit(v as f64),
            TyKind::Char => Sv::Leaf(Leaf { t: int_lit(v), base: Base::Char, depth: 0 }),
            _ => Sv::int(int_lit(v), IntTy::I64),
        }
    }

    fn expr(&mut self, fr: &Frame, st: &mut St, e: ExprId) -> Option<Sv> {
        let ast = self.ast(fr.pkg);
        let ex = ast.exprs[e];
        let t = self.ty(fr, e);
        match ex.kind {
            ExprKind::Int(v) => Some(self.lit_of_ty(v as i128, t)),
            ExprKind::Float(f) => Some(self.float_lit(f)),
            ExprKind::Bool(b) => Some(Sv::bool(if b { "true" } else { "false" })),
            ExprKind::Str(s) => Some(self.str_lit(s)),
            ExprKind::Char(c) => Some(self.lit_of_ty(c as i128, t)),
            ExprKind::Paren(x) | ExprKind::MutArg(x) => self.expr(fr, st, x),
            ExprKind::Name(n) => Some(self.name_value(fr, st, e, n, t)),
            ExprKind::Tuple(list) => {
                let ids: Vec<ExprId> = ast.expr_lists.get(list).to_vec();
                if ids.is_empty() {
                    return Some(Sv::Unit);
                }
                let mut xs = Vec::new();
                for x in ids {
                    xs.push(self.expr(fr, st, x)?);
                }
                Some(Sv::Tuple(xs))
            }
            ExprKind::List(list) => {
                let ids: Vec<ExprId> = ast.expr_lists.get(list).to_vec();
                let mut vals = Vec::new();
                for x in &ids {
                    vals.push(self.expr(fr, st, *x)?);
                }
                let elem = self.elem_ty(t);
                let mut elems = self.fresh_at(elem, "lit", 1);
                for (i, v) in vals.iter().enumerate() {
                    match self.store(&elems, &i.to_string(), v) {
                        Some(e) => elems = e,
                        None => return Some(self.fresh(t, "list")),
                    }
                }
                Some(Sv::List {
                    len: Leaf { t: ids.len().to_string(), base: Base::Int(IntTy::I64), depth: 0 },
                    elems: Box::new(elems),
                })
            }
            ExprKind::Field { base, .. } => match self.res(fr, e) {
                Res::Field(i) => {
                    let b = self.expr(fr, st, base)?;
                    Some(match b {
                        Sv::Struct(_, xs) if (i as usize) < xs.len() => xs[i as usize].clone(),
                        _ => self.fresh(t, "field"),
                    })
                }
                _ => Some(self.name_value(fr, st, e, sym::EMPTY, t)),
            },
            ExprKind::TupleField { base, index, .. } => {
                let b = self.expr(fr, st, base)?;
                Some(match b {
                    Sv::Tuple(xs) if (index as usize) < xs.len() => xs[index as usize].clone(),
                    _ => self.fresh(t, "tuple"),
                })
            }
            ExprKind::TypeArgs { base, .. } => self.expr(fr, st, base),
            ExprKind::Call { callee, args } => self.call(fr, st, e, callee, args, t),
            ExprKind::Index { base, index } => self.index(fr, st, e, base, index, t),
            ExprKind::Range { .. } | ExprKind::Error => Some(self.fresh(t, "range")),
            ExprKind::Unary { op, expr } => {
                if op == UnOp::Neg {
                    if let ExprKind::Int(v) = ast.exprs[expr].kind {
                        return Some(self.lit_of_ty(-(v as i128), t));
                    }
                }
                let v = self.expr(fr, st, expr)?;
                self.unary(fr, st, op, &v, t, ex.span)
            }
            ExprKind::Binary { op, lhs, rhs } => self.binary(fr, st, op, lhs, rhs, t, ex.span),
            ExprKind::Cast { expr, .. } => {
                let from = self.ty(fr, expr);
                let v = self.expr(fr, st, expr)?;
                self.cast(fr, st, &v, from, t, ex.span)
            }
            ExprKind::Try(inner) => {
                let v = self.expr(fr, st, inner)?;
                let Sv::Enum { tag, vars, adt } = v else { return Some(self.fresh(t, "try")) };
                let is_result = Some(adt) == self.defs.result;
                // Failure path: return the error (or None) from this function.
                let ok = format!("(= {} 0)", tag.t);
                let mut fail = st.clone();
                self.assume(&mut fail, not(&ok));
                let ret_ty = self.current_ret_ty(fr);
                if let TyKind::Adt(radt, _) = self.kind(ret_ty) {
                    let fields = if is_result { vars.get(1).cloned().unwrap_or_default() } else { vec![] };
                    let rv = self.construct(&mut fail, ret_ty, radt, 1, fields);
                    self.returns.push((fail, rv));
                }
                self.assume(st, ok);
                Some(vars.first().and_then(|v| v.first()).cloned().unwrap_or_else(|| self.fresh(t, "ok")))
            }
            ExprKind::If { cond, then, els } => self.if_expr(fr, st, cond, then, els, t),
            ExprKind::Match { scrut, arms } => self.match_expr(fr, st, scrut, arms, t),
            ExprKind::Block(_) => self.block(fr, st, e),
            ExprKind::StructLit { fields, .. } => {
                let TyKind::Adt(adt, _) = self.kind(t) else { return Some(self.fresh(t, "struct")) };
                let inits: Vec<ast::FieldInit> = ast.field_inits.get(fields).to_vec();
                let decl: Vec<Symbol> = self.defs.adts[adt].fields().iter().map(|f| f.name).collect();
                let fts = self.field_tys(t);
                let mut vals: Vec<Option<Sv>> = vec![None; decl.len()];
                for init in inits {
                    let v = self.expr(fr, st, init.value)?;
                    if let Some(i) = decl.iter().position(|&n| n == init.name.sym) {
                        vals[i] = Some(v);
                    }
                }
                let vals: Vec<Sv> =
                    vals.into_iter().zip(fts).map(|(v, ft)| v.unwrap_or_else(|| self.fresh(ft, "f"))).collect();
                let v = Sv::Struct(adt, vals);
                self.oblige_struct_preds(fr, st, t, &v, ex.span);
                Some(v)
            }
            ExprKind::Closure { .. } => Some(self.fresh(t, "closure")),
            ExprKind::Quant { forall, var, lo, hi, body } => self.quant(fr, st, forall, var, lo, hi, body),
        }
    }

    fn current_ret_ty(&mut self, fr: &Frame) -> Ty {
        // The function being verified is the only frame with returns.
        let _ = fr;
        self.cur_ret.unwrap_or(self.tys.error)
    }

    fn name_value(&mut self, fr: &Frame, st: &mut St, e: ExprId, n: Symbol, t: Ty) -> Sv {
        if fr.body.is_none() && n != sym::EMPTY {
            if let Some((_, v)) = fr.names.iter().rev().find(|(x, _)| *x == n) {
                return v.clone();
            }
        }
        match self.res(fr, e) {
            Res::Local(id) => match st.env.get(&id) {
                Some(v) => v.clone(),
                None => {
                    let v = self.fresh(t, n.as_str());
                    st.env.insert(id, v.clone());
                    v
                }
            },
            Res::Const(c) => match &self.defs.consts[c].value {
                Some(ConstVal::Int(v)) => self.lit_of_ty(*v, t),
                Some(ConstVal::Bool(b)) => Sv::bool(if *b { "true" } else { "false" }),
                Some(ConstVal::Str(s)) => self.str_lit(*s),
                Some(ConstVal::Char(c)) => self.lit_of_ty(*c as i128, t),
                Some(ConstVal::Float(f)) => self.float_lit(*f),
                None => self.fresh(t, "const"),
            },
            Res::Variant(adt, v) if self.defs.adts[adt].variants()[v as usize].fields.is_empty() => {
                self.construct(st, t, adt, v, vec![])
            }
            Res::ResultVal => fr.result.clone().unwrap_or_else(|| self.fresh(t, "result")),
            _ => self.fresh(t, "value"),
        }
    }

    /// Construct an enum value.
    fn construct(&mut self, st: &mut St, t: Ty, adt: AdtId, v: u32, fields: Vec<Sv>) -> Sv {
        let ad = &self.defs.adts[adt];
        let n = ad.variants().len() as u32;
        if ad.recursive {
            let leaf = self.leaf(Base::Opaque(t), 0, "rec");
            let tag = self.rec_tag(t, &leaf.t);
            self.axioms.push(format!("(= {tag} {v})"));
            for (i, f) in fields.iter().enumerate() {
                if let Sv::Leaf(fl) = f {
                    if fl.depth == 0 {
                        let ft = self.rec_field_fn(t, v, i as u32, fl);
                        self.axioms.push(format!("(= ({ft} {}) {})", leaf.t, fl.t));
                    }
                }
                self.rec_fields.insert((leaf.t.clone(), v, i as u32), f.clone());
            }
            let _ = st;
            return Sv::Leaf(leaf);
        }
        let vts = self.variant_tys(t);
        let vars = vts
            .into_iter()
            .enumerate()
            .map(|(vi, fs)| {
                if vi as u32 == v {
                    fields.clone()
                } else {
                    fs.into_iter().map(|f| self.fresh_at(f, "pad", 0)).collect()
                }
            })
            .collect();
        Sv::Enum { adt, tag: Leaf { t: v.to_string(), base: Base::Tag(n), depth: 0 }, vars }
    }

    fn rec_tag(&mut self, t: Ty, x: &str) -> String {
        let s = self.sort(&Base::Opaque(t), 0);
        let f = self.fun(&format!("tag_{}", t.0), &[s], "Int");
        format!("({f} {x})")
    }

    fn rec_field_fn(&mut self, t: Ty, v: u32, i: u32, l: &Leaf) -> String {
        let s = self.sort(&Base::Opaque(t), 0);
        let r = self.sort(&l.base, 0);
        self.fun(&format!("fld_{}_{v}_{i}", t.0), &[s], &r)
    }

    /// Field `i` of variant `v` of a recursive enum value.
    fn rec_field(&mut self, st: &mut St, t: Ty, x: &str, v: u32, i: u32, ft: Ty) -> Sv {
        if let Some(f) = self.rec_fields.get(&(x.to_string(), v, i)) {
            return f.clone();
        }
        let f = self.fresh(ft, "fld");
        if let Sv::Leaf(fl) = &f {
            let fname = self.rec_field_fn(t, v, i, fl);
            let fl = fl.clone();
            self.assume(st, format!("(= ({fname} {x}) {})", fl.t));
        }
        self.assume_type_preds(st, ft, &f, 0);
        self.rec_fields.insert((x.to_string(), v, i), f.clone());
        f
    }

    fn unary(&mut self, fr: &Frame, st: &mut St, op: UnOp, v: &Sv, t: Ty, span: Span) -> Option<Sv> {
        let Some(x) = v.term() else { return Some(self.fresh(t, "un")) };
        let x = x.to_string();
        Some(match (op, self.kind(t)) {
            (UnOp::Not, _) => Sv::bool(self.define_bool(not(&x))),
            (UnOp::Neg, TyKind::Int(k, _)) => {
                let r = self.define_int(format!("(- {x})"));
                self.arith_result(fr, st, &r, k, span, "-");
                Sv::int(r, k)
            }
            (UnOp::Neg, TyKind::Float(_)) => {
                let s = self.opaque_sort("JFloat");
                let f = self.fun("fneg", std::slice::from_ref(&s), &s);
                Sv::Leaf(Leaf { t: self.define(&s, format!("({f} {x})")), base: Base::Float, depth: 0 })
            }
            _ => self.fresh(t, "un"),
        })
    }

    /// The overflow obligation for an arithmetic result, then its range as a fact.
    fn arith_result(&mut self, fr: &Frame, st: &mut St, r: &str, k: IntTy, span: Span, op: &str) {
        let goal = range_fact(r, k.min(), k.max());
        let msg = format!("`{}` does not overflow {}", self.src(span), k.name());
        let _ = op;
        self.oblige(fr, st, ObKind::Overflow, span, msg, goal.clone());
        self.assume(st, goal);
    }

    #[allow(clippy::too_many_arguments)]
    fn binary(
        &mut self,
        fr: &Frame,
        st: &mut St,
        op: BinOp,
        lhs: ExprId,
        rhs: ExprId,
        t: Ty,
        span: Span,
    ) -> Option<Sv> {
        if matches!(op, BinOp::And | BinOp::Or) {
            let a = self.expr(fr, st, lhs)?;
            let at = a.term().unwrap_or("true").to_string();
            // The right side only runs when the left does not decide.
            let base = st.pc.len();
            let mut rs = st.clone();
            let mut skip = st.clone();
            if op == BinOp::And {
                self.assume(&mut rs, at.clone());
                self.assume(&mut skip, not(&at));
            } else {
                self.assume(&mut rs, not(&at));
                self.assume(&mut skip, at.clone());
            }
            let b = match self.expr(fr, &mut rs, rhs) {
                Some(b) => b,
                None => {
                    *st = skip;
                    return Some(Sv::bool(if op == BinOp::And { "false" } else { "true" }));
                }
            };
            let bt = b.term().unwrap_or("true").to_string();
            let val = if op == BinOp::And { format!("(and {at} {bt})") } else { format!("(or {at} {bt})") };
            let unit = self.tys.unit;
            if let Some((s, _)) = self.merge(base, vec![(rs, Sv::Unit), (skip, Sv::Unit)], unit) {
                *st = s;
            }
            return Some(Sv::bool(self.define_bool(val)));
        }
        let lt = self.ty(fr, lhs);
        let a = self.expr(fr, st, lhs)?;
        let b = self.expr(fr, st, rhs)?;
        if op == BinOp::Add && matches!(self.kind(t), TyKind::Str) {
            return Some(self.concat(&a, &b));
        }
        self.binary_vals(fr, st, op, &a, &b, lt, span)
    }

    /// `op` on two values whose operand type is `t`.
    #[allow(clippy::too_many_arguments)]
    fn binary_vals(&mut self, fr: &Frame, st: &mut St, op: BinOp, a: &Sv, b: &Sv, t: Ty, span: Span) -> Option<Sv> {
        use BinOp::*;
        let k = self.kind(t);
        if matches!(op, Eq | Ne) {
            let eq = match self.eq_term(a, b) {
                Some(e) => e,
                None => return Some(self.fresh_bool("eq")),
            };
            let r = if op == Eq { eq } else { not(&eq) };
            return Some(Sv::bool(self.define_bool(r)));
        }
        let (Some(x), Some(y)) = (a.term(), b.term()) else {
            return Some(if op.is_comparison() { self.fresh_bool("cmp") } else { self.fresh(t, "bin") });
        };
        let (x, y) = (x.to_string(), y.to_string());
        if op.is_comparison() {
            let sym = match op {
                Lt => "<",
                Le => "<=",
                Gt => ">",
                _ => ">=",
            };
            return Some(match k {
                TyKind::Int(..) | TyKind::Char => Sv::bool(self.define_bool(format!("({sym} {x} {y})"))),
                TyKind::Bool => self.fresh_bool("cmp"),
                TyKind::Float(_) => {
                    let s = self.opaque_sort("JFloat");
                    let (name, swap) = match op {
                        Lt => ("flt", false),
                        Le => ("fle", false),
                        Gt => ("flt", true),
                        _ => ("fle", true),
                    };
                    let f = self.fun(name, &[s.clone(), s], "Bool");
                    let (p, q) = if swap { (&y, &x) } else { (&x, &y) };
                    Sv::bool(self.define_bool(format!("({f} {p} {q})")))
                }
                _ => self.fresh_bool("cmp"),
            });
        }
        match k {
            TyKind::Int(ik, _) => {
                let r = match op {
                    Add => format!("(+ {x} {y})"),
                    Sub => format!("(- {x} {y})"),
                    Mul => format!("(* {x} {y})"),
                    Div | Rem => {
                        let nz = format!("(not (= {y} 0))");
                        let msg = format!("`{}` divides by a nonzero value", self.src(span));
                        self.oblige(fr, st, ObKind::DivZero, span, msg, nz.clone());
                        self.assume(st, nz);
                        // Truncating division, as in C and Rust.
                        let q = format!(
                            "(ite (>= {x} 0) (ite (> {y} 0) (div {x} {y}) (- (div {x} (- {y})))) (ite (> {y} 0) (- (div (- {x}) {y})) (div (- {x}) (- {y}))))"
                        );
                        let q = self.define_int(q);
                        if op == Div {
                            q
                        } else {
                            // MIN % -1 is 0 at run time; the formula gives 0 too.
                            let r = self.define_int(format!("(- {x} (* {y} {q}))"));
                            return Some(Sv::int(r, ik));
                        }
                    }
                    Shl | Shr => {
                        let ok = range_fact(&y, 0, ik.bits() as i128 - 1);
                        let msg = format!("shift amount in range: {}", self.src(span));
                        self.oblige(fr, st, ObKind::Shift, span, msg, ok.clone());
                        self.assume(st, ok);
                        return Some(self.fresh(t, "shift"));
                    }
                    _ => return Some(self.fresh(t, "bits")),
                };
                let r = self.define_int(r);
                self.arith_result(fr, st, &r, ik, span, op.as_str());
                Some(Sv::int(r, ik))
            }
            TyKind::Float(_) => {
                let s = self.opaque_sort("JFloat");
                let name = match op {
                    Add => "fadd",
                    Sub => "fsub",
                    Mul => "fmul",
                    Div => "fdiv",
                    _ => "frem",
                };
                let f = self.fun(name, &[s.clone(), s.clone()], &s);
                Some(Sv::Leaf(Leaf { t: self.define(&s, format!("({f} {x} {y})")), base: Base::Float, depth: 0 }))
            }
            _ => Some(self.fresh(t, "bin")),
        }
    }

    fn cast(&mut self, fr: &Frame, st: &mut St, v: &Sv, from: Ty, to: Ty, span: Span) -> Option<Sv> {
        let Some(x) = v.term() else { return Some(self.fresh(to, "cast")) };
        let x = x.to_string();
        Some(match (self.kind(from), self.kind(to)) {
            (TyKind::Int(..) | TyKind::Char, TyKind::Int(k, _)) => {
                let goal = range_fact(&x, k.min(), k.max());
                let msg = format!("`{}` fits in {}", self.src(span), k.name());
                self.oblige(fr, st, ObKind::Cast, span, msg, goal.clone());
                self.assume(st, goal);
                Sv::int(x, k)
            }
            (TyKind::Int(..), TyKind::Char) => {
                let goal = format!("(and {} (not (and (<= 55296 {x}) (<= {x} 57343))))", range_fact(&x, 0, 0x10_FFFF));
                let msg = format!("`{}` is a valid character", self.src(span));
                self.oblige(fr, st, ObKind::Cast, span, msg, goal.clone());
                self.assume(st, goal);
                Sv::Leaf(Leaf { t: x, base: Base::Char, depth: 0 })
            }
            (TyKind::Int(..) | TyKind::Char, TyKind::Float(_)) => {
                let s = self.opaque_sort("JFloat");
                let f = self.fun("i2f", &["Int".into()], &s);
                Sv::Leaf(Leaf { t: self.define(&s, format!("({f} {x})")), base: Base::Float, depth: 0 })
            }
            (TyKind::Float(_), TyKind::Float(_)) => v.clone(),
            _ => self.fresh(to, "cast"),
        })
    }

    fn index(&mut self, fr: &Frame, st: &mut St, e: ExprId, base: ExprId, index: ExprId, t: Ty) -> Option<Sv> {
        let ast = self.ast(fr.pkg);
        let span = ast.exprs[e].span;
        let b = self.expr(fr, st, base)?;
        if let ExprKind::Range { lo, hi } = ast.exprs[index].kind {
            let lo_v = match lo {
                Some(l) => self.expr(fr, st, l)?,
                None => Sv::int("0", IntTy::I64),
            };
            let len = match &b {
                Sv::List { len, .. } => Some(len.t.clone()),
                Sv::Leaf(l) if l.base == Base::Str => Some(self.str_len(&l.t)),
                _ => None,
            };
            let hi_v = match hi {
                Some(h) => self.expr(fr, st, h)?,
                None => match &len {
                    Some(l) => Sv::int(l.clone(), IntTy::I64),
                    None => self.fresh(self.tys.int, "hi"),
                },
            };
            let r = self.fresh(t, "slice");
            if let (Some(l), Some(lo), Some(hi)) = (len, lo_v.term(), hi_v.term()) {
                let goal = format!("(and (<= 0 {lo}) (<= {lo} {hi}) (<= {hi} {l}))");
                let msg = format!("slice in bounds: {}", self.src(span));
                self.oblige(fr, st, ObKind::Bounds, span, msg, goal.clone());
                self.assume(st, goal);
                let rlen = match &r {
                    Sv::List { len, .. } => Some(len.t.clone()),
                    Sv::Leaf(x) if x.base == Base::Str => Some(self.str_len(&x.t)),
                    _ => None,
                };
                // A slice that returns (a string slice also panics off a character boundary)
                // has exactly the bytes or elements in its range.
                if let Some(rl) = rlen {
                    self.assume(st, format!("(= {rl} (- {hi} {lo}))"));
                }
            }
            return Some(r);
        }
        let i = self.expr(fr, st, index)?;
        let Some(it) = i.term().map(|s| s.to_string()) else { return Some(self.fresh(t, "idx")) };
        match b {
            Sv::List { len, elems } => {
                let goal = format!("(and (<= 0 {it}) (< {it} {}))", len.t);
                let msg = format!("index in bounds: {}", self.src(span));
                self.oblige(fr, st, ObKind::Bounds, span, msg, goal.clone());
                self.assume(st, goal);
                Some(self.select(st, &elems, &it, t))
            }
            Sv::Leaf(l) if l.base == Base::Str => {
                let len = self.str_len(&l.t);
                let goal = format!("(and (<= 0 {it}) (< {it} {len}))");
                let msg = format!("index in bounds: {}", self.src(span));
                self.oblige(fr, st, ObKind::Bounds, span, msg, goal.clone());
                self.assume(st, goal);
                Some(self.fresh(t, "byte"))
            }
            _ => Some(self.fresh(t, "idx")),
        }
    }

    fn if_expr(
        &mut self,
        fr: &Frame,
        st: &mut St,
        cond: ExprId,
        then: ExprId,
        els: Option<ExprId>,
        t: Ty,
    ) -> Option<Sv> {
        let c = self.expr(fr, st, cond)?;
        let ct = c.term().unwrap_or("true").to_string();
        let base = st.pc.len();
        let mut ts = st.clone();
        self.assume(&mut ts, ct.clone());
        let mut es = st.clone();
        self.assume(&mut es, not(&ct));
        let mut outs = Vec::new();
        if let Some(v) = self.block(fr, &mut ts, then) {
            outs.push((ts, v));
        }
        let ev = match els {
            Some(el) => self.block(fr, &mut es, el),
            None => Some(Sv::Unit),
        };
        if let Some(v) = ev {
            outs.push((es, v));
        }
        let (s, v) = self.merge(base, outs, t)?;
        *st = s;
        Some(if self.tys.is_unit(t) { Sv::Unit } else { v })
    }

    fn match_expr(
        &mut self,
        fr: &Frame,
        st: &mut St,
        scrut: ExprId,
        arms: crate::util::List<ast::Arm>,
        t: Ty,
    ) -> Option<Sv> {
        let ast = self.ast(fr.pkg);
        let stt = self.ty(fr, scrut);
        let s = self.expr(fr, st, scrut)?;
        let arms: Vec<ast::Arm> = ast.arms.get(arms).to_vec();
        let base = st.pc.len();
        let mut outs = Vec::new();
        let mut rest = st.clone();
        for arm in arms {
            let mut a = rest.clone();
            let test = self.test_pat(fr, &mut a, arm.pat, &s, stt);
            let test = self.define_bool(test);
            self.assume(&mut a, test.clone());
            let mut fail = rest.clone();
            if let Some(g) = arm.guard {
                let gv = self.expr(fr, &mut a, g)?;
                let gt = gv.term().unwrap_or("true").to_string();
                self.assume(&mut a, gt.clone());
                // Pattern bindings feed the guard; a failed guard falls through.
                let both = self.define_bool(format!("(and {test} {gt})"));
                self.assume(&mut fail, not(&both));
            } else {
                self.assume(&mut fail, not(&test));
            }
            let v = match ast.exprs[arm.body].kind {
                ExprKind::Block(_) => self.block(fr, &mut a, arm.body),
                _ => self.expr(fr, &mut a, arm.body),
            };
            if let Some(v) = v {
                outs.push((a, v));
            }
            rest = fail;
        }
        let (s2, v) = self.merge(base, outs, t)?;
        *st = s2;
        Some(if self.tys.is_unit(t) { Sv::Unit } else { v })
    }

    /// The condition under which `pat` matches `v`; bindings go into `st`.
    fn test_pat(&mut self, fr: &Frame, st: &mut St, pat: PatId, v: &Sv, t: Ty) -> String {
        let ast = self.ast(fr.pkg);
        match ast.pats[pat].kind {
            PatKind::Wild | PatKind::Error => "true".into(),
            PatKind::Bind(_) => {
                if let Some(&id) = self.tables[fr.pkg].pat_local.get(&pat) {
                    st.env.insert(id, v.clone());
                }
                "true".into()
            }
            PatKind::Lit(e) => {
                let Some(lit) = self.expr(fr, st, e) else { return "false".into() };
                self.eq_term(v, &lit).unwrap_or_else(|| {
                    let b = self.fresh_bool("lit");
                    b.term().unwrap().to_string()
                })
            }
            PatKind::Variant { args, .. } => {
                let Some(&(_, vi)) = self.tables[fr.pkg].pat_variant.get(&pat) else { return "true".into() };
                let subs: Vec<PatId> = ast.pat_lists.get(args).to_vec();
                let vts = self.variant_tys(t);
                let fts = vts.get(vi as usize).cloned().unwrap_or_default();
                match v {
                    Sv::Enum { tag, vars, .. } => {
                        let mut parts = vec![format!("(= {} {vi})", tag.t)];
                        let fields = vars.get(vi as usize).cloned().unwrap_or_default();
                        for (i, sp) in subs.into_iter().enumerate() {
                            let ft = fts.get(i).copied().unwrap_or(self.tys.error);
                            let fv = fields.get(i).cloned().unwrap_or_else(|| self.fresh(ft, "f"));
                            parts.push(self.test_pat(fr, st, sp, &fv, ft));
                        }
                        and(&parts)
                    }
                    Sv::Leaf(l) if matches!(l.base, Base::Opaque(_)) => {
                        let x = l.t.clone();
                        let tag = self.rec_tag(t, &x);
                        let mut parts = vec![format!("(= {tag} {vi})")];
                        for (i, sp) in subs.into_iter().enumerate() {
                            let ft = fts.get(i).copied().unwrap_or(self.tys.error);
                            let fv = self.rec_field(st, t, &x, vi, i as u32, ft);
                            parts.push(self.test_pat(fr, st, sp, &fv, ft));
                        }
                        and(&parts)
                    }
                    _ => {
                        let b = self.fresh_bool("pat");
                        b.term().unwrap().to_string()
                    }
                }
            }
            PatKind::Tuple(list) => {
                let subs: Vec<PatId> = ast.pat_lists.get(list).to_vec();
                let fts = self.field_tys(t);
                let mut parts = Vec::new();
                for (i, sp) in subs.into_iter().enumerate() {
                    let ft = fts.get(i).copied().unwrap_or(self.tys.error);
                    let fv = match v {
                        Sv::Tuple(xs) if i < xs.len() => xs[i].clone(),
                        _ => self.fresh(ft, "f"),
                    };
                    parts.push(self.test_pat(fr, st, sp, &fv, ft));
                }
                and(&parts)
            }
            PatKind::Or(list) => {
                let alts: Vec<PatId> = ast.pat_lists.get(list).to_vec();
                let mut parts = Vec::new();
                for a in alts {
                    parts.push(self.test_pat(fr, st, a, v, t));
                }
                if parts.is_empty() {
                    "false".into()
                } else if parts.len() == 1 {
                    parts.pop().unwrap()
                } else {
                    format!("(or {})", parts.join(" "))
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quant(
        &mut self,
        fr: &Frame,
        st: &mut St,
        forall: bool,
        var: ast::Name,
        lo: ExprId,
        hi: ExprId,
        body: ExprId,
    ) -> Option<Sv> {
        let l = self.expr(fr, st, lo)?;
        let h = self.expr(fr, st, hi)?;
        let (Some(l), Some(h)) = (l.term().map(|s| s.to_string()), h.term().map(|s| s.to_string())) else {
            return Some(self.fresh_bool("quant"));
        };
        let qid = fr.body.and_then(|b| {
            b.locals.iter_enumerated().find(|(_, d)| d.span == var.span && d.kind == LocalKind::Quant).map(|(i, _)| i)
        });
        let lt = self.ty(fr, lo);
        let k = match self.kind(lt) {
            TyKind::Int(k, _) => k,
            _ => IntTy::I64,
        };
        // Obligations inside the body hold for an arbitrary index in range.
        {
            let i = self.konst("Int", var.sym.as_str());
            let mut s2 = st.clone();
            self.assume(&mut s2, format!("(and (<= {l} {i}) (< {i} {h}))"));
            let mut fr2 = fr.clone();
            match qid {
                Some(id) => {
                    s2.env.insert(id, Sv::int(i.clone(), k));
                }
                None => fr2.names.push((var.sym, Sv::int(i.clone(), k))),
            }
            let _ = self.expr(&fr2, &mut s2, body);
        }
        // The value: a real quantifier over the body, evaluated without definitions.
        let b = self.name("q");
        let mut s3 = St { env: st.env.clone(), pc: Vec::new() };
        let mut fr3 = fr.clone();
        match qid {
            Some(id) => {
                s3.env.insert(id, Sv::int(b.clone(), k));
            }
            None => fr3.names.push((var.sym, Sv::int(b.clone(), k))),
        }
        self.inline += 1;
        let v = self.expr(&fr3, &mut s3, body);
        self.inline -= 1;
        let Some(bt) = v.as_ref().and_then(|v| v.term()).map(|s| s.to_string()) else {
            return Some(self.fresh_bool("quant"));
        };
        let facts = and(&s3.pc);
        let range = format!("(and (<= {l} {b}) (< {b} {h}))");
        let term = if forall {
            format!("(forall (({b} Int)) (=> (and {range} {facts}) {bt}))")
        } else {
            format!("(exists (({b} Int)) (and {range} {facts} {bt}))")
        };
        Some(Sv::bool(self.define_bool(term)))
    }

    // ---------- calls ----------

    fn call(
        &mut self,
        fr: &Frame,
        st: &mut St,
        e: ExprId,
        callee: ExprId,
        args: crate::util::List<ExprId>,
        t: Ty,
    ) -> Option<Sv> {
        let ast = self.ast(fr.pkg);
        let span = ast.exprs[e].span;
        let arg_ids: Vec<ExprId> = ast.expr_lists.get(args).to_vec();
        let res = self.res(fr, callee);
        match res {
            Res::Old => {
                let a = *arg_ids.first()?;
                match fr.old.clone() {
                    Some(old) => {
                        let mut s2 = St { env: old, pc: st.pc.clone() };
                        let v = self.expr(fr, &mut s2, a);
                        st.pc = s2.pc;
                        v
                    }
                    None => self.expr(fr, st, a),
                }
            }
            Res::Variant(adt, v) => {
                let mut fields = Vec::new();
                for a in arg_ids {
                    fields.push(self.expr(fr, st, a)?);
                }
                Some(self.construct(st, t, adt, v, fields))
            }
            Res::Fn(f) | Res::Method(f) => {
                let fd = &self.defs.fns[f];
                if fd.name == sym::ASSERT && !fd.has_body && Some(fd.pkg) == self.prelude {
                    let c = *arg_ids.first()?;
                    let v = self.expr(fr, st, c)?;
                    let ct = v.term().unwrap_or("false").to_string();
                    let cspan = ast.exprs[c].span;
                    let text = self.src(cspan);
                    self.oblige(fr, st, ObKind::Assert, cspan, text, ct.clone());
                    self.assume(st, ct);
                    return Some(Sv::Unit);
                }
                let inst: Box<[Ty]> = {
                    let raw: Vec<Ty> =
                        self.tables[fr.pkg].expr_inst.get(&callee).map(|a| a.to_vec()).unwrap_or_default();
                    raw.into_iter().map(|x| self.tys.subst(x, &fr.inst)).collect()
                };
                let mut call_args = Vec::new();
                if let (Res::Method(_), ExprKind::Field { base, .. }) = (res, ast.exprs[callee].kind) {
                    call_args.push(self.arg(fr, st, base, self.defs.fns[f].self_mode == Some(Mode::MutRef))?);
                }
                let modes: Vec<Mode> = self.defs.fns[f].params.iter().map(|p| p.mode).collect();
                for (i, &a) in arg_ids.iter().enumerate() {
                    let m = modes.get(i).copied().unwrap_or(Mode::Value) == Mode::MutRef;
                    call_args.push(self.arg(fr, st, a, m)?);
                }
                self.invoke(fr, st, span, f, inst, call_args, t)
            }
            Res::IfaceMethod(iid, mi) => {
                let ExprKind::Field { base, .. } = ast.exprs[callee].kind else { return Some(self.fresh(t, "call")) };
                let rt = self.ty(fr, base);
                let mf = self.defs.ifaces[iid].methods[mi as usize];
                let mname = self.defs.fns[mf].name;
                let key = TyKey::of(self.tys, rt);
                let Some(f) = key.and_then(|k| self.defs.method(k, mname)) else {
                    for a in arg_ids {
                        self.expr(fr, st, a)?;
                    }
                    return Some(self.unit_or_fresh(t, "call"));
                };
                let recv_args: Box<[Ty]> = match self.kind(rt) {
                    TyKind::Adt(_, a) => a,
                    TyKind::List(x) | TyKind::Set(x) => Box::new([x]),
                    TyKind::Map(k, v) => Box::new([k, v]),
                    _ => Box::new([]),
                };
                let mut call_args = vec![self.arg(fr, st, base, self.defs.fns[f].self_mode == Some(Mode::MutRef))?];
                let modes: Vec<Mode> = self.defs.fns[f].params.iter().map(|p| p.mode).collect();
                for (i, &a) in arg_ids.iter().enumerate() {
                    let m = modes.get(i).copied().unwrap_or(Mode::Value) == Mode::MutRef;
                    call_args.push(self.arg(fr, st, a, m)?);
                }
                self.invoke(fr, st, span, f, recv_args, call_args, t)
            }
            _ => {
                // Function values, bound methods (`x.str()`), fields holding functions.
                if let ExprKind::Field { base, .. } = ast.exprs[callee].kind {
                    self.expr(fr, st, base)?;
                } else {
                    self.expr(fr, st, callee)?;
                }
                for a in arg_ids {
                    self.expr(fr, st, a)?;
                }
                if self.tys.is_never(t) {
                    return None;
                }
                Some(self.unit_or_fresh(t, "call"))
            }
        }
    }

    fn arg(&mut self, fr: &Frame, st: &mut St, a: ExprId, mutref: bool) -> Option<Arg> {
        if mutref {
            if let Some(p) = self.place(fr, st, a) {
                self.oblige_place_bounds(fr, st, &p);
                let v = self.read_place(st, &p);
                return Some(Arg::Place(p, v));
            }
        }
        Some(Arg::Value(self.expr(fr, st, a)?))
    }

    /// A call: the callee's contract (or a model of a builtin) stands in for its body.
    #[allow(clippy::too_many_arguments)]
    fn invoke(
        &mut self,
        fr: &Frame,
        st: &mut St,
        span: Span,
        f: FnId,
        inst: Box<[Ty]>,
        args: Vec<Arg>,
        t: Ty,
    ) -> Option<Sv> {
        let fd = &self.defs.fns[f];
        let pkg_path = self.pkgs[fd.pkg].path.as_str();
        if !fd.has_body {
            if let Some((op, _)) = intrinsics::lookup(pkg_path, fd.recv, fd.name.as_str()) {
                return self.intrinsic(fr, st, span, op, &args, t);
            }
        }
        if Some(fd.pkg) == self.prelude {
            if let Some(v) = self.prelude_model(fr, st, span, f, &args, t) {
                return v;
            }
        }
        self.contract_call(fr, st, span, f, inst, args, t)
    }

    #[allow(clippy::too_many_arguments)]
    fn contract_call(
        &mut self,
        fr: &Frame,
        st: &mut St,
        span: Span,
        f: FnId,
        inst: Box<[Ty]>,
        args: Vec<Arg>,
        t: Ty,
    ) -> Option<Sv> {
        let fd = &self.defs.fns[f];
        let fname = fd.display_name(self.defs, self.tys);
        let Some(body) = self.bodies[f].as_ref() else {
            return self.havoc_call(st, args, t);
        };
        if self.depth > 2 {
            return self.havoc_call(st, args, t);
        }
        self.depth += 1;
        let r = self.contract_call_inner(fr, st, span, f, fname, body, inst, args, t);
        self.depth -= 1;
        r
    }

    #[allow(clippy::too_many_arguments)]
    fn contract_call_inner(
        &mut self,
        fr: &Frame,
        st: &mut St,
        span: Span,
        f: FnId,
        fname: String,
        body: &'a Body,
        inst: Box<[Ty]>,
        args: Vec<Arg>,
        t: Ty,
    ) -> Option<Sv> {
        let fd = &self.defs.fns[f];
        let pkg = fd.pkg;
        let item = self.pkgs[pkg].ast.items[fd.item];
        let ast::ItemKind::Fn(decl) = item.kind else { return self.havoc_call(st, args, t) };
        let ast: &'a Ast = &self.pkgs[pkg].ast;
        let offset = fd.self_mode.is_some() as usize;
        let refines: Vec<Option<AliasId>> = fd.params.iter().map(|p| p.refine).collect();
        let ret_refine = fd.ret_refine;
        let pure = fd.effects == EffectSet::PURE;
        let mut cfr =
            Frame { pkg, body: Some(body), inst: inst.clone(), result: None, old: None, names: vec![], own: false };
        let mut cst = St { env: FxHashMap::default(), pc: std::mem::take(&mut st.pc) };
        for (i, &id) in body.param_locals.iter().enumerate() {
            if let Some(a) = args.get(i) {
                cst.env.insert(id, a.value().clone());
            }
        }
        // Preconditions at the call site.
        for (i, a) in args.iter().enumerate() {
            if i < offset {
                continue;
            }
            if let Some(Some(al)) = refines.get(i - offset) {
                let v = a.value().clone();
                if let Some((msg, term)) = self.alias_pred(&mut cst, *al, &v) {
                    self.oblige(
                        fr,
                        &cst,
                        ObKind::Precondition,
                        span,
                        format!("argument of `{fname}`: {msg}"),
                        term.clone(),
                    );
                    self.assume(&mut cst, term);
                }
            }
        }
        let ast_params: Vec<ast::Param> = ast.params.get(decl.params).to_vec();
        for p in &ast_params {
            if let Some(pred) = p.refine {
                let text = self.src(ast.exprs[pred].span);
                if let Some(v) = self.expr(&cfr, &mut cst, pred) {
                    if let Some(term) = v.term() {
                        let term = term.to_string();
                        self.oblige(
                            fr,
                            &cst,
                            ObKind::Precondition,
                            span,
                            format!("`{fname}` requires {text}"),
                            term.clone(),
                        );
                        self.assume(&mut cst, term);
                    }
                }
            }
        }
        for &r in ast.expr_lists.get(decl.requires) {
            let text = self.src(ast.exprs[r].span);
            if let Some(v) = self.expr(&cfr, &mut cst, r) {
                if let Some(term) = v.term() {
                    let term = term.to_string();
                    self.oblige(
                        fr,
                        &cst,
                        ObKind::Precondition,
                        span,
                        format!("`{fname}` requires {text}"),
                        term.clone(),
                    );
                    self.assume(&mut cst, term);
                }
            }
        }
        // The result: an uninterpreted function of the arguments when the callee is pure
        // and everything is scalar (so equal arguments give equal results), else fresh.
        let has_mut = args.iter().any(|a| matches!(a, Arg::Place(..)));
        let scalar_args: Option<Vec<(String, String)>> = args
            .iter()
            .map(|a| match a.value() {
                Sv::Leaf(l) if l.depth == 0 => Some(l.clone()),
                _ => None,
            })
            .map(|l| l.map(|l| (l.t.clone(), self.sort(&l.base, 0))))
            .collect();
        let result = match (pure && !has_mut, scalar_args, self.fresh_at(t, "r", 0)) {
            (true, Some(sargs), Sv::Leaf(rl)) if inst.is_empty() => {
                let rs = self.sort(&rl.base, 0);
                let sorts: Vec<String> = sargs.iter().map(|(_, s)| s.clone()).collect();
                let fname_smt = self.fun(&format!("fn_{}", f.0), &sorts, &rs);
                let app = if sargs.is_empty() {
                    fname_smt
                } else {
                    format!("({fname_smt} {})", sargs.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join(" "))
                };
                let v = Sv::Leaf(Leaf { t: self.define(&rs, app), base: rl.base, depth: 0 });
                for fct in self.wf_facts(&v) {
                    self.assume(&mut cst, fct);
                }
                v
            }
            (_, _, v) => {
                let facts = self.wf_facts_deep(&v);
                self.axioms.extend(facts);
                v
            }
        };
        self.assume_type_preds(&mut cst, t, &result, 0);
        if let Some(al) = ret_refine {
            if let Some((_, term)) = self.alias_pred(&mut cst, al, &result) {
                self.assume(&mut cst, term);
            }
        }
        // `&mut` arguments come back changed, satisfying their types' predicates.
        let pre_env = cst.env.clone();
        let mut writes = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if let Arg::Place(p, cur) = a {
                let pt = *p.tys.last().unwrap();
                // Only what the callee's body can write changes; the rest is kept.
                let nv = match self.frames.get(&(f, i)).cloned() {
                    Some(super::frame::Frame::Paths(paths)) => {
                        let mut v = cur.clone();
                        for path in &paths {
                            v = self.havoc_at(&mut cst, &v, pt, path, true);
                        }
                        v
                    }
                    _ => self.fresh(pt, "out"),
                };
                self.assume_type_preds(&mut cst, pt, &nv, 0);
                if let Some(&id) = body.param_locals.get(i) {
                    cst.env.insert(id, nv.clone());
                }
                writes.push((p.clone(), nv));
            }
        }
        // Postconditions as facts.
        cfr.result = Some(result.clone());
        cfr.old = Some(pre_env);
        for &en in ast.expr_lists.get(decl.ensures) {
            if let Some(v) = self.expr(&cfr, &mut cst, en) {
                if let Some(term) = v.term() {
                    let term = term.to_string();
                    self.assume(&mut cst, term);
                }
            }
        }
        st.pc = cst.pc;
        for (p, nv) in writes {
            let root = st.env.get(&p.local).cloned().unwrap_or_else(|| self.fresh(p.tys[0], "local"));
            let updated = self.update(st, &root, &p.path, &p.tys, 0, nv);
            st.env.insert(p.local, updated);
        }
        if self.tys.is_never(t) {
            return None;
        }
        Some(result)
    }

    /// An unknown call: fresh result, `&mut` arguments havocked.
    fn havoc_call(&mut self, st: &mut St, args: Vec<Arg>, t: Ty) -> Option<Sv> {
        for a in args {
            if let Arg::Place(p, _) = a {
                let pt = *p.tys.last().unwrap();
                let nv = self.fresh_assumed(st, pt);
                let root = st.env.get(&p.local).cloned().unwrap_or_else(|| self.fresh(p.tys[0], "local"));
                let updated = self.update(st, &root, &p.path, &p.tys, 0, nv);
                st.env.insert(p.local, updated);
            }
        }
        if self.tys.is_never(t) {
            return None;
        }
        Some(self.unit_or_fresh(t, "call"))
    }

    fn write_arg(&mut self, st: &mut St, a: &Arg, v: Sv) {
        if let Arg::Place(p, _) = a {
            let root = st.env.get(&p.local).cloned().unwrap_or_else(|| self.fresh(p.tys[0], "local"));
            let updated = self.update(st, &root, &p.path, &p.tys, 0, v);
            st.env.insert(p.local, updated);
        }
    }

    /// Models of prelude functions written in Jig (`Option.unwrap`, `min`, ...).
    fn prelude_model(
        &mut self,
        fr: &Frame,
        st: &mut St,
        span: Span,
        f: FnId,
        args: &[Arg],
        t: Ty,
    ) -> Option<Option<Sv>> {
        let fd = &self.defs.fns[f];
        let name = fd.name.as_str();
        let is_opt = matches!(fd.recv, Some(TyKey::Adt(a)) if Some(a) == self.defs.option);
        let is_res = matches!(fd.recv, Some(TyKey::Adt(a)) if Some(a) == self.defs.result);
        let recv = args.first().map(|a| a.value().clone());
        if is_opt || is_res {
            let Some(Sv::Enum { tag, vars, .. }) = recv else { return None };
            let ok = format!("(= {} 0)", tag.t);
            let payload = vars.first().and_then(|v| v.first()).cloned();
            return Some(match name {
                "is_some" | "is_ok" => Some(Sv::bool(self.define_bool(ok))),
                "is_none" | "is_err" => Some(Sv::bool(self.define_bool(not(&ok)))),
                "unwrap" => {
                    let what = if is_opt { "`None`" } else { "an `Err`" };
                    self.oblige(fr, st, ObKind::Unwrap, span, format!("not {what}: {}", self.src(span)), ok.clone());
                    self.assume(st, ok);
                    Some(payload.unwrap_or_else(|| self.fresh(t, "unwrap")))
                }
                "unwrap_or" => {
                    let d = args.get(1)?.value().clone();
                    let p = payload.unwrap_or_else(|| self.fresh(t, "p"));
                    Some(self.ite(&ok, &p, &d, t))
                }
                "ok_or" if is_opt => {
                    let err = args.get(1)?.value().clone();
                    let TyKind::Adt(radt, _) = self.kind(t) else { return None };
                    let p = payload.unwrap_or_else(|| {
                        let pt = self.variant_tys(t).first().and_then(|v| v.first().copied()).unwrap_or(t);
                        self.fresh(pt, "p")
                    });
                    let okv = self.construct(st, t, radt, 0, vec![p]);
                    let errv = self.construct(st, t, radt, 1, vec![err]);
                    Some(self.ite(&ok, &okv, &errv, t))
                }
                _ => return None,
            });
        }
        if fd.recv.is_none() && (name == "min" || name == "max") && args.len() == 2 {
            let (a, b) = (args[0].value().clone(), args[1].value().clone());
            if let (Some(x), Some(y), TyKind::Int(..)) = (a.term(), b.term(), self.kind(t)) {
                let c = if name == "min" { format!("(<= {x} {y})") } else { format!("(>= {x} {y})") };
                return Some(Some(self.ite(&c, &a, &b, t)));
            }
        }
        None
    }

    /// Models of builtins: list and string lengths, element access, integer helpers.
    fn intrinsic(&mut self, fr: &Frame, st: &mut St, span: Span, op: Op, args: &[Arg], t: Ty) -> Option<Sv> {
        use Op::*;
        let v0 = args.first().map(|a| a.value().clone());
        let term = |i: usize| args.get(i).and_then(|a| a.value().term().map(|s| s.to_string()));
        match op {
            Panic => {
                self.oblige(fr, st, ObKind::Panic, span, format!("unreachable: {}", self.src(span)), "false".into());
                None
            }
            IntAbs => {
                let x = term(0)?;
                let TyKind::Int(k, _) = self.kind(t) else { return Some(self.fresh(t, "abs")) };
                let r = self.define_int(format!("(ite (< {x} 0) (- {x}) {x})"));
                self.arith_result(fr, st, &r, k, span, "abs");
                Some(Sv::int(r, k))
            }
            IntMin | IntMax => {
                let (x, y) = (term(0)?, term(1)?);
                let TyKind::Int(k, _) = self.kind(t) else { return Some(self.fresh(t, "m")) };
                let c = if op == IntMin { "<=" } else { ">=" };
                Some(Sv::int(self.define_int(format!("(ite ({c} {x} {y}) {x} {y})")), k))
            }
            StrLen => {
                let x = term(0)?;
                let len = self.str_len(&x);
                Some(Sv::int(self.define_int(len), IntTy::I64))
            }
            StrIsEmpty => {
                let x = term(0)?;
                let len = self.str_len(&x);
                Some(Sv::bool(self.define_bool(format!("(= {len} 0)"))))
            }
            StrConcat => {
                let (a, b) = (args.first()?.value().clone(), args.get(1)?.value().clone());
                Some(self.concat(&a, &b))
            }
            // What string searches imply about lengths.
            StartsWith | EndsWith | StrContains => {
                let (x, y) = (term(0)?, term(1)?);
                let (lx, ly) = (self.str_len(&x), self.str_len(&y));
                let r = self.fresh(t, "has");
                let rt = r.term()?.to_string();
                self.assume(st, format!("(=> {rt} (<= {ly} {lx}))"));
                self.assume(st, format!("(=> (= {ly} 0) {rt})"));
                Some(r)
            }
            StrFind => {
                let (x, y) = (term(0)?, term(1)?);
                let (lx, ly) = (self.str_len(&x), self.str_len(&y));
                let r = self.fresh(t, "find");
                if let Sv::Enum { tag, vars, .. } = &r {
                    if let Some(i) = vars.first().and_then(|v| v.first()).and_then(|v| v.term()) {
                        self.assume(st, format!("(=> (= {} 0) (and (>= {i} 0) (<= (+ {i} {ly}) {lx})))", tag.t));
                    }
                }
                Some(r)
            }
            Trim => {
                let x = term(0)?;
                let r = self.fresh(t, "trim");
                if let Some(rt) = r.term() {
                    let (lr, lx) = (self.str_len(rt), self.str_len(&x));
                    self.assume(st, format!("(<= {lr} {lx})"));
                }
                Some(r)
            }
            Bytes | Chars | Split => {
                let x = term(0)?;
                let lx = self.str_len(&x);
                let r = self.fresh(t, "parts");
                if let Sv::List { len, .. } = &r {
                    let n = &len.t;
                    let fact = match op {
                        // One byte each.
                        Bytes => format!("(= {n} {lx})"),
                        // One to four bytes each.
                        Chars => format!("(and (<= {n} {lx}) (=> (> {lx} 0) (> {n} 0)))"),
                        // Always at least one piece.
                        _ => format!("(>= {n} 1)"),
                    };
                    self.assume(st, fact);
                }
                Some(r)
            }
            StrConcatN => {
                let mut acc = args.first()?.value().clone();
                for x in &args[1..] {
                    let b = x.value().clone();
                    acc = self.concat(&acc, &b);
                }
                Some(acc)
            }
            ListNew => {
                let elem = self.elem_ty(t);
                let elems = self.fresh_at(elem, "e", 1);
                Some(Sv::List {
                    len: Leaf { t: "0".into(), base: Base::Int(IntTy::I64), depth: 0 },
                    elems: Box::new(elems),
                })
            }
            ListLen => match v0? {
                Sv::List { len, .. } => Some(Sv::int(len.t, IntTy::I64)),
                _ => Some(self.fresh(t, "len")),
            },
            ListIsEmpty => match v0? {
                Sv::List { len, .. } => Some(Sv::bool(self.define_bool(format!("(= {} 0)", len.t)))),
                _ => Some(self.fresh(t, "empty")),
            },
            Push => {
                let Some(Sv::List { len, elems }) = v0 else { return self.havoc_call(st, args.to_vec_args(), t) };
                let x = args.get(1)?.value().clone();
                let n = len.t.clone();
                let new_elems = self.store(&elems, &n, &x);
                let new_len = self.define_int(format!("(+ {n} 1)"));
                let new = match new_elems {
                    Some(e) => {
                        Sv::List { len: Leaf { t: new_len, base: Base::Int(IntTy::I64), depth: 0 }, elems: Box::new(e) }
                    }
                    None => return self.havoc_call(st, args.to_vec_args(), t),
                };
                self.write_arg(st, &args[0], new);
                Some(Sv::Unit)
            }
            Pop => {
                let Some(Sv::List { len, elems }) = v0 else { return self.havoc_call(st, args.to_vec_args(), t) };
                let TyKind::Adt(oadt, _) = self.kind(t) else { return self.havoc_call(st, args.to_vec_args(), t) };
                let n = len.t.clone();
                let last = self.define_int(format!("(- {n} 1)"));
                let elem = self.variant_tys(t).first().and_then(|v| v.first().copied()).unwrap_or(t);
                let nonempty = format!("(> {n} 0)");
                let mut s2 = st.clone();
                self.assume(&mut s2, nonempty.clone());
                let x = self.select(&mut s2, &elems, &last, elem);
                let some = self.construct(st, t, oadt, 0, vec![x]);
                let none = self.construct(st, t, oadt, 1, vec![]);
                let r = self.ite(&nonempty, &some, &none, t);
                let new_len = self.define_int(format!("(ite {nonempty} {last} 0)"));
                self.write_arg(
                    st,
                    &args[0],
                    Sv::List { len: Leaf { t: new_len, base: Base::Int(IntTy::I64), depth: 0 }, elems },
                );
                Some(r)
            }
            Get => {
                let Some(Sv::List { len, elems }) = v0 else { return Some(self.fresh(t, "get")) };
                let TyKind::Adt(oadt, _) = self.kind(t) else { return Some(self.fresh(t, "get")) };
                let i = term(1)?;
                let inb = format!("(and (<= 0 {i}) (< {i} {}))", len.t);
                let elem = self.variant_tys(t).first().and_then(|v| v.first().copied()).unwrap_or(t);
                let mut s2 = st.clone();
                self.assume(&mut s2, inb.clone());
                let x = self.select(&mut s2, &elems, &i, elem);
                let some = self.construct(st, t, oadt, 0, vec![x]);
                let none = self.construct(st, t, oadt, 1, vec![]);
                Some(self.ite(&inb, &some, &none, t))
            }
            Swap => {
                let Some(Sv::List { len, elems }) = v0 else { return self.havoc_call(st, args.to_vec_args(), t) };
                let (i, j) = (term(1)?, term(2)?);
                let lt = match &args[0] {
                    Arg::Place(p, _) => *p.tys.last().unwrap(),
                    _ => return self.havoc_call(st, args.to_vec_args(), t),
                };
                let elem = self.elem_ty(lt);
                let goal = format!("(and (<= 0 {i}) (< {i} {l}) (<= 0 {j}) (< {j} {l}))", l = len.t);
                self.oblige(
                    fr,
                    st,
                    ObKind::Bounds,
                    span,
                    format!("swap indices in bounds: {}", self.src(span)),
                    goal.clone(),
                );
                self.assume(st, goal);
                let (xi, xj) = (self.select(st, &elems, &i, elem), self.select(st, &elems, &j, elem));
                let e1 = self.store(&elems, &i, &xj)?;
                let e2 = self.store(&e1, &j, &xi)?;
                self.write_arg(st, &args[0], Sv::List { len, elems: Box::new(e2) });
                Some(Sv::Unit)
            }
            Remove | Insert => {
                let Some(Sv::List { len, .. }) = v0 else { return self.havoc_call(st, args.to_vec_args(), t) };
                let i = term(1)?;
                let (goal, new_len) = if op == Remove {
                    (format!("(and (<= 0 {i}) (< {i} {}))", len.t), format!("(- {} 1)", len.t))
                } else {
                    (format!("(and (<= 0 {i}) (<= {i} {}))", len.t), format!("(+ {} 1)", len.t))
                };
                self.oblige(fr, st, ObKind::Bounds, span, format!("index in bounds: {}", self.src(span)), goal.clone());
                self.assume(st, goal);
                let lt = match &args[0] {
                    Arg::Place(p, _) => *p.tys.last().unwrap(),
                    _ => return self.havoc_call(st, args.to_vec_args(), t),
                };
                let fresh = self.fresh(lt, "list");
                if let Sv::List { elems, .. } = fresh {
                    let nl = self.define_int(new_len);
                    self.write_arg(
                        st,
                        &args[0],
                        Sv::List { len: Leaf { t: nl, base: Base::Int(IntTy::I64), depth: 0 }, elems },
                    );
                }
                Some(self.unit_or_fresh(t, "removed"))
            }
            Clear => {
                let lt = match &args[0] {
                    Arg::Place(p, _) => *p.tys.last().unwrap(),
                    _ => return Some(Sv::Unit),
                };
                let fresh = self.fresh(lt, "list");
                if let Sv::List { elems, .. } = fresh {
                    self.write_arg(
                        st,
                        &args[0],
                        Sv::List { len: Leaf { t: "0".into(), base: Base::Int(IntTy::I64), depth: 0 }, elems },
                    );
                }
                Some(Sv::Unit)
            }
            Reverse | Sort | SortBy => {
                // Same length, unknown order.
                let Some(Sv::List { len, .. }) = v0 else { return self.havoc_call(st, args.to_vec_args(), t) };
                let lt = match &args[0] {
                    Arg::Place(p, _) => *p.tys.last().unwrap(),
                    _ => return Some(Sv::Unit),
                };
                let fresh = self.fresh(lt, "list");
                if let Sv::List { elems, .. } = fresh {
                    self.write_arg(st, &args[0], Sv::List { len, elems });
                }
                Some(Sv::Unit)
            }
            TaskAll | TaskMap => {
                // One result per job (item), in order; their values are unknown.
                let Some(Sv::List { len, .. }) = v0 else { return Some(self.fresh(t, "tasks")) };
                let Sv::List { elems, .. } = self.fresh(t, "tasks") else { return Some(self.fresh(t, "tasks")) };
                Some(Sv::List { len, elems })
            }
            ListRepeat => {
                let Some(Sv::List { len, .. }) = v0 else { return Some(self.fresh(t, "repeat")) };
                let n = term(1)?;
                self.oblige(
                    fr,
                    st,
                    ObKind::Panic,
                    span,
                    format!("repeat count is not negative: {}", self.src(span)),
                    format!("(>= {n} 0)"),
                );
                self.assume(st, format!("(>= {n} 0)"));
                // Same elements over and over; only the length is modeled.
                let Sv::List { elems, .. } = self.fresh(t, "repeat") else { return Some(self.fresh(t, "repeat")) };
                let new_len = self.define_int(format!("(* {} {n})", len.t));
                Some(Sv::List { len: Leaf { t: new_len, base: Base::Int(IntTy::I64), depth: 0 }, elems })
            }
            ListSlice | StrSlice => Some(self.fresh(t, "slice")),
            _ => self.havoc_call(st, args.to_vec_args(), t),
        }
    }
}

trait ArgsExt {
    fn to_vec_args(&self) -> Vec<Arg>;
}

impl ArgsExt for [Arg] {
    fn to_vec_args(&self) -> Vec<Arg> {
        self.iter()
            .map(|a| match a {
                Arg::Value(v) => Arg::Value(v.clone()),
                Arg::Place(p, v) => Arg::Place(p.clone(), v.clone()),
            })
            .collect()
    }
}
