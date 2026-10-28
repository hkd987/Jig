//! MIR: a monomorphized control-flow graph per function instance, shared by the
//! interpreter (the deterministic test runner) and the native back ends.
//!
//! Conventions: local 0 is the return slot; parameters follow (captured values first for
//! closures, then `self`, then declared parameters). `&mut` parameters are written back to
//! the caller's place when the call returns (copy-in/copy-out, equivalent to by-reference
//! under the exclusivity rule).

pub mod ffi;
pub mod interp;
pub mod intrinsics;
pub mod lower;
pub mod tls;

use crate::diag::Span;
use crate::sema::defs::{Mode, TestId};
use crate::sema::ty::FnId;
use crate::sema::ty::{AdtId, IntTy, Ty};
use crate::syntax::ast::{BinOp, UnOp};
use crate::util::{FxHashMap, IndexVec, Symbol};
pub use intrinsics::Intrinsic;

crate::define_idx!(MFnId);
crate::define_idx!(Local);
crate::define_idx!(BlockId);

#[derive(Clone, Debug)]
pub struct LocalDecl {
    pub ty: Ty,
    pub name: Option<Symbol>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PlaceElem {
    /// Field `i` of a struct or tuple.
    Field(u32),
    /// Field `i` of enum variant `v` (the value must be that variant).
    VariantField(u32, u32),
    /// Element of a list, bounds-checked. The span is the indexing expression's (proofs
    /// that it is in bounds are keyed by it); `Span::DUMMY` means in bounds by construction.
    Index(Operand, Span),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Place {
    pub local: Local,
    pub proj: Vec<PlaceElem>,
}

impl Place {
    pub fn local(l: Local) -> Place {
        Place { local: l, proj: Vec::new() }
    }
    pub fn field(&self, i: u32) -> Place {
        let mut p = self.clone();
        p.proj.push(PlaceElem::Field(i));
        p
    }
    pub fn variant_field(&self, v: u32, i: u32) -> Place {
        let mut p = self.clone();
        p.proj.push(PlaceElem::VariantField(v, i));
        p
    }
    pub fn index(&self, op: Operand, span: Span) -> Place {
        let mut p = self.clone();
        p.proj.push(PlaceElem::Index(op, span));
        p
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Const {
    Int(i128, IntTy),
    Float(f64),
    Bool(bool),
    Char(u32),
    Str(Symbol),
    Unit,
    /// A function item as a value.
    Fn(MFnId),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Copy(Place),
    Const(Const),
}

impl Operand {
    pub fn local(l: Local) -> Operand {
        Operand::Copy(Place::local(l))
    }
    pub fn int(v: i128) -> Operand {
        Operand::Const(Const::Int(v, IntTy::I64))
    }
    pub fn bool(b: bool) -> Operand {
        Operand::Const(Const::Bool(b))
    }
    pub fn unit() -> Operand {
        Operand::Const(Const::Unit)
    }
}

#[derive(Clone, Debug)]
pub enum AggKind {
    Tuple,
    Struct(AdtId),
    Variant(AdtId, u32),
    List,
}

#[derive(Clone, Debug)]
pub enum Callee {
    Fn(MFnId),
    Intrinsic(Intrinsic),
    /// The C function of this name (the body of a function declared with `effects {ffi}`).
    Ffi(Symbol),
    /// A function value (closure or function item) held in an operand.
    Value(Operand),
}

#[derive(Clone, Debug)]
pub enum Arg {
    Value(Operand),
    MutRef(Place),
}

#[derive(Clone, Debug)]
pub enum Rvalue {
    Use(Operand),
    /// Checked arithmetic and comparisons; `ty` is the operand type.
    Binary(BinOp, Operand, Operand, Ty),
    Unary(UnOp, Operand, Ty),
    /// Checked numeric conversion.
    Cast(Operand, Ty, Ty),
    /// `x + 1` for a loop counter that is below its bound, so it cannot overflow.
    Inc(Operand, IntTy),
    Aggregate(AggKind, Ty, Vec<Operand>),
    /// The variant index of an enum value.
    Discriminant(Place),
    Closure(MFnId, Vec<Operand>),
    /// Calls never unwind: a panic aborts (the test runner reports it as a failure).
    Call(Callee, Vec<Arg>, Ty),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    Assert,
    Precondition,
    Postcondition,
    Invariant,
    Refinement,
}

#[derive(Clone, Debug)]
pub enum Stmt {
    Assign(Place, Rvalue),
    /// Abort with a message unless `cond` holds. `values` are shown on failure.
    Check {
        cond: Operand,
        kind: CheckKind,
        msg: Symbol,
        values: Vec<(Symbol, Operand, Ty)>,
        span: Span,
    },
    /// The local is dead from here on (releases its storage).
    Drop(Local),
}

#[derive(Clone, Debug)]
pub enum Terminator {
    Goto(BlockId),
    If(Operand, BlockId, BlockId),
    /// Jump on an integer value (an enum discriminant).
    Switch(Operand, Vec<(i128, BlockId)>, BlockId),
    Return,
    /// Control cannot reach here (after a call returning `Never`).
    Unreachable,
}

#[derive(Clone, Debug)]
pub struct Block {
    pub stmts: Vec<(Stmt, Span)>,
    pub term: Terminator,
    pub term_span: Span,
}

#[derive(Clone, Debug)]
pub struct MFn {
    pub name: String,
    pub source: Option<FnId>,
    /// Instance type arguments (for display and caching).
    pub type_args: Box<[Ty]>,
    pub params: Vec<(Local, Mode)>,
    /// For closures, the number of leading parameters that are captured values.
    pub n_captures: usize,
    pub ret: Ty,
    pub locals: IndexVec<Local, LocalDecl>,
    pub blocks: IndexVec<BlockId, Block>,
    pub intrinsic: Option<Intrinsic>,
    pub span: Span,
    pub lowered: bool,
}

/// Lowering options.
#[derive(Clone, Copy, Debug)]
pub struct LowerOpts {
    /// Check `requires`, `ensures`, invariants and refinements at run time.
    pub check_contracts: bool,
}

pub struct Mir {
    pub fns: IndexVec<MFnId, MFn>,
    pub instances: FxHashMap<(FnId, Box<[Ty]>), MFnId>,
    pub tests: FxHashMap<TestId, MFnId>,
    /// Instances requested but not yet lowered.
    pub pending: Vec<MFnId>,
    pub opts: LowerOpts,
    /// Checks that proofs discharged (empty unless a build supplies cached proofs).
    pub elide: crate::verify::Proven,
}

impl Mir {
    pub fn new(opts: LowerOpts) -> Mir {
        Mir {
            fns: IndexVec::new(),
            instances: FxHashMap::default(),
            tests: FxHashMap::default(),
            pending: Vec::new(),
            opts,
            elide: Default::default(),
        }
    }
}
