//! The MIR interpreter: the deterministic test runner, and `jig run --interp`.
//!
//! Values use reference counting with copy-on-write, which gives value semantics directly.
//! `&mut` parameters are copy-in/copy-out. Arithmetic, indexing and casts are checked.
//! In test mode every effect is faked: console I/O is captured, the clock and random numbers
//! are deterministic, and the file system is in memory.

use super::intrinsics::Op;
use super::lower::Lowerer;
use super::*;
use crate::diag::Span;
use crate::sema::defs::{AdtKind, TyKey};
use crate::sema::ty::{TyKind, NO_UNIT};
use crate::syntax::ast::{BinOp, UnOp};
use crate::util::FxHashMap;
use std::cmp::Ordering;
use std::hash::{Hash, Hasher};
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum Value {
    Unit,
    Bool(bool),
    /// Every integer type except `U64`.
    Int(i64),
    UInt(u64),
    Float(f64),
    Char(u32),
    Str(Rc<str>),
    /// Structs and tuples.
    Tuple(Rc<Vec<Value>>),
    Variant(u32, Rc<Vec<Value>>),
    List(Rc<Vec<Value>>),
    Map(Rc<OMap>),
    Set(Rc<OMap>),
    Fn(MFnId),
    Closure(Rc<(MFnId, Vec<Value>)>),
}

/// An insertion-ordered hash map.
#[derive(Clone, Debug, Default)]
pub struct OMap {
    pub keys: Vec<Value>,
    pub vals: Vec<Value>,
    index: FxHashMap<HKey, usize>,
}

#[derive(Clone, Debug)]
struct HKey(Value);

impl PartialEq for HKey {
    fn eq(&self, o: &HKey) -> bool {
        val_eq(&self.0, &o.0)
    }
}
impl Eq for HKey {}
impl Hash for HKey {
    fn hash<H: Hasher>(&self, h: &mut H) {
        hash_val(&self.0, h);
    }
}

fn hash_val<H: Hasher>(v: &Value, h: &mut H) {
    match v {
        Value::Unit => 0u8.hash(h),
        Value::Bool(b) => b.hash(h),
        Value::Int(i) => i.hash(h),
        Value::UInt(i) => i.hash(h),
        Value::Float(f) => f.to_bits().hash(h),
        Value::Char(c) => c.hash(h),
        Value::Str(s) => s.hash(h),
        Value::Tuple(xs) | Value::List(xs) => {
            xs.len().hash(h);
            for x in xs.iter() {
                hash_val(x, h);
            }
        }
        Value::Variant(t, xs) => {
            t.hash(h);
            for x in xs.iter() {
                hash_val(x, h);
            }
        }
        Value::Map(m) | Value::Set(m) => {
            m.keys.len().hash(h);
            for k in &m.keys {
                hash_val(k, h);
            }
        }
        Value::Fn(f) => f.0.hash(h),
        Value::Closure(c) => c.0 .0.hash(h),
    }
}

impl OMap {
    pub fn get(&self, k: &Value) -> Option<&Value> {
        self.index.get(&HKey(k.clone())).map(|&i| &self.vals[i])
    }
    pub fn contains(&self, k: &Value) -> bool {
        self.index.contains_key(&HKey(k.clone()))
    }
    pub fn insert(&mut self, k: Value, v: Value) {
        match self.index.get(&HKey(k.clone())) {
            Some(&i) => self.vals[i] = v,
            None => {
                self.index.insert(HKey(k.clone()), self.keys.len());
                self.keys.push(k);
                self.vals.push(v);
            }
        }
    }
    pub fn remove(&mut self, k: &Value) -> Option<Value> {
        let i = self.index.remove(&HKey(k.clone()))?;
        self.keys.remove(i);
        let v = self.vals.remove(i);
        for j in i..self.keys.len() {
            self.index.insert(HKey(self.keys[j].clone()), j);
        }
        Some(v)
    }
    pub fn len(&self) -> usize {
        self.keys.len()
    }
}

pub fn val_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::UInt(x), Value::UInt(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::Str(x), Value::Str(y)) => x == y,
        (Value::Tuple(x), Value::Tuple(y)) | (Value::List(x), Value::List(y)) => {
            Rc::ptr_eq(x, y) || (x.len() == y.len() && x.iter().zip(y.iter()).all(|(p, q)| val_eq(p, q)))
        }
        (Value::Variant(s, x), Value::Variant(t, y)) => s == t && x.iter().zip(y.iter()).all(|(p, q)| val_eq(p, q)),
        (Value::Map(x), Value::Map(y)) => {
            x.len() == y.len() && x.keys.iter().zip(x.vals.iter()).all(|(k, v)| y.get(k).is_some_and(|w| val_eq(v, w)))
        }
        (Value::Set(x), Value::Set(y)) => x.len() == y.len() && x.keys.iter().all(|k| y.contains(k)),
        _ => false,
    }
}

pub fn val_cmp(a: &Value, b: &Value) -> Ordering {
    match (a, b) {
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::UInt(x), Value::UInt(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::Str(x), Value::Str(y)) => x.cmp(y),
        (Value::Tuple(x), Value::Tuple(y)) | (Value::List(x), Value::List(y)) => {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = val_cmp(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            x.len().cmp(&y.len())
        }
        (Value::Variant(s, x), Value::Variant(t, y)) => s.cmp(t).then_with(|| {
            for (p, q) in x.iter().zip(y.iter()) {
                let c = val_cmp(p, q);
                if c != Ordering::Equal {
                    return c;
                }
            }
            Ordering::Equal
        }),
        _ => Ordering::Equal,
    }
}

#[derive(Debug)]
pub enum Trap {
    Panic { msg: String, span: Span, backtrace: Vec<String> },
    Exit(i32),
}

/// How effects behave.
pub struct Effects {
    pub fake: bool,
    pub stdout: String,
    pub stderr: String,
    pub stdin: Vec<String>,
    pub stdin_pos: usize,
    pub args: Vec<String>,
    pub env: FxHashMap<String, String>,
    pub files: FxHashMap<String, String>,
    pub clock_ms: i64,
    pub rng: u64,
}

impl Effects {
    pub fn fake(seed: u64) -> Effects {
        Effects {
            fake: true,
            stdout: String::new(),
            stderr: String::new(),
            stdin: Vec::new(),
            stdin_pos: 0,
            args: vec!["test".into()],
            env: FxHashMap::default(),
            files: FxHashMap::default(),
            clock_ms: 1_700_000_000_000,
            rng: seed | 1,
        }
    }
    pub fn real(args: Vec<String>) -> Effects {
        let seed = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1);
        let mut e = Effects::fake(seed);
        e.fake = false;
        e.args = args;
        e
    }
    fn next_rand(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
}

pub struct Interp<'l, 'a> {
    pub lw: &'l mut Lowerer<'a>,
    pub fx: Effects,
    code: Vec<Option<Rc<MFn>>>,
    stack: Vec<(MFnId, Span)>,
    /// Remaining steps before the run is stopped (guards tests against infinite loops).
    pub fuel: u64,
}

const MAX_DEPTH: usize = 20_000;

type R<T> = Result<T, Trap>;

impl<'l, 'a> Interp<'l, 'a> {
    pub fn new(lw: &'l mut Lowerer<'a>, fx: Effects) -> Self {
        Interp { lw, fx, code: Vec::new(), stack: Vec::new(), fuel: u64::MAX }
    }

    fn code(&mut self, f: MFnId) -> Rc<MFn> {
        let i = f.0 as usize;
        if i < self.code.len() {
            if let Some(c) = &self.code[i] {
                return c.clone();
            }
        }
        self.lw.ensure_lowered(f);
        let c = Rc::new(self.lw.mir.fns[f].clone());
        if self.code.len() <= i {
            self.code.resize(i + 1, None);
        }
        self.code[i] = Some(c.clone());
        c
    }

    fn panic(&self, msg: impl Into<String>, span: Span) -> Trap {
        let mut bt = Vec::new();
        for (f, s) in self.stack.iter().rev().take(32) {
            let name = self.lw.mir.fns[*f].name.clone();
            let loc = self.lw.sm.line_col(s.lo).map(|(fid, lc)| format!("{}:{}:{}", crate::diag::display_path(&self.lw.sm.file(fid).path), lc.line, lc.col)).unwrap_or_default();
            bt.push(format!("{name} at {loc}"));
        }
        Trap::Panic { msg: msg.into(), span, backtrace: bt }
    }

    /// Call a function with argument values; returns the result and the final values of
    /// `&mut` parameters.
    pub fn call(&mut self, f: MFnId, args: Vec<Value>) -> R<(Value, Vec<Value>)> {
        let code = self.code(f);
        if let Some(intr) = &code.intrinsic {
            let span = self.stack.last().map(|x| x.1).unwrap_or(code.span);
            return self.intrinsic(intr, args, span);
        }
        if self.stack.len() >= MAX_DEPTH {
            return Err(self.panic("stack overflow: recursion too deep", code.span));
        }
        self.stack.push((f, code.span));
        let r = self.run(&code, args);
        self.stack.pop();
        r
    }

    fn run(&mut self, code: &MFn, args: Vec<Value>) -> R<(Value, Vec<Value>)> {
        let mut locals: Vec<Value> = vec![Value::Unit; code.locals.len()];
        for ((l, _), v) in code.params.iter().zip(args) {
            locals[l.0 as usize] = v;
        }
        let mut bb = BlockId(0);
        loop {
            let block = &code.blocks[bb];
            for (stmt, span) in &block.stmts {
                if let Some(top) = self.stack.last_mut() {
                    top.1 = *span;
                }
                if self.fuel == 0 {
                    return Err(self.panic("step limit exceeded (infinite loop?)", *span));
                }
                self.fuel -= 1;
                match stmt {
                    Stmt::Assign(p, rv) => {
                        let v = self.rvalue(rv, &mut locals, *span)?;
                        self.write(&mut locals, p, v, *span)?;
                    }
                    Stmt::Check { cond, kind, msg, values, span } => {
                        let c = self.operand(cond, &locals, *span)?;
                        if !matches!(c, Value::Bool(true)) {
                            let what = match kind {
                                CheckKind::Assert => "assertion failed",
                                CheckKind::Precondition => "precondition failed",
                                CheckKind::Postcondition => "postcondition failed",
                                CheckKind::Invariant => "invariant failed",
                                CheckKind::Refinement => "refinement failed",
                            };
                            let mut m = format!("{what}: {msg}");
                            for (label, op, ty) in values {
                                let v = self.operand(op, &locals, *span)?;
                                let s = self.show(&v, *ty, true)?;
                                m.push_str(&format!("\n  {label} = {s}"));
                            }
                            return Err(self.panic(m, *span));
                        }
                    }
                    Stmt::Drop(l) => locals[l.0 as usize] = Value::Unit,
                }
            }
            match &block.term {
                Terminator::Goto(b) => bb = *b,
                Terminator::If(c, t, e) => {
                    bb = match self.operand(c, &locals, block.term_span)? {
                        Value::Bool(true) => *t,
                        _ => *e,
                    }
                }
                Terminator::Switch(op, cases, default) => {
                    let v = match self.operand(op, &locals, block.term_span)? {
                        Value::Int(i) => i as i128,
                        Value::UInt(u) => u as i128,
                        _ => -1,
                    };
                    bb = cases.iter().find(|(k, _)| *k == v).map(|(_, b)| *b).unwrap_or(*default);
                }
                Terminator::Return => break,
                Terminator::Unreachable => return Err(self.panic("entered unreachable code", block.term_span)),
            }
        }
        let outs: Vec<Value> = code
            .params
            .iter()
            .filter(|(_, m)| *m == Mode::MutRef)
            .map(|(l, _)| std::mem::replace(&mut locals[l.0 as usize], Value::Unit))
            .collect();
        Ok((std::mem::replace(&mut locals[0], Value::Unit), outs))
    }

    // ---------- places ----------

    fn index_of(&self, op: &Operand, locals: &[Value], span: Span) -> R<i64> {
        match self.operand(op, locals, span)? {
            Value::Int(i) => Ok(i),
            Value::UInt(u) => Ok(u as i64),
            _ => Err(self.panic("index is not an integer", span)),
        }
    }

    fn read(&self, locals: &[Value], p: &Place, span: Span) -> R<Value> {
        let mut v = &locals[p.local.0 as usize];
        for e in &p.proj {
            v = match (e, v) {
                (PlaceElem::Field(i), Value::Tuple(xs)) => &xs[*i as usize],
                (PlaceElem::VariantField(t, i), Value::Variant(tag, xs)) => {
                    if tag != t {
                        return Err(self.panic("internal error: wrong variant", span));
                    }
                    &xs[*i as usize]
                }
                (PlaceElem::Index(op), Value::List(xs)) => {
                    let i = self.index_of(op, locals, span)?;
                    if i < 0 || i as usize >= xs.len() {
                        return Err(self.panic(format!("index out of bounds: the index is {i} but the length is {}", xs.len()), span));
                    }
                    &xs[i as usize]
                }
                _ => return Err(self.panic(format!("internal error: bad place projection {e:?} on {v:?}"), span)),
            };
        }
        Ok(v.clone())
    }

    fn write(&self, locals: &mut [Value], p: &Place, val: Value, span: Span) -> R<()> {
        // Evaluate index operands before borrowing mutably.
        let mut idx: Vec<i64> = Vec::new();
        for e in &p.proj {
            if let PlaceElem::Index(op) = e {
                idx.push(self.index_of(op, locals, span)?);
            }
        }
        let mut k = 0;
        let mut slot = &mut locals[p.local.0 as usize];
        for e in &p.proj {
            slot = match (e, slot) {
                (PlaceElem::Field(i), Value::Tuple(xs)) => &mut Rc::make_mut(xs)[*i as usize],
                (PlaceElem::VariantField(_, i), Value::Variant(_, xs)) => &mut Rc::make_mut(xs)[*i as usize],
                (PlaceElem::Index(_), Value::List(xs)) => {
                    let i = idx[k];
                    k += 1;
                    let len = xs.len();
                    if i < 0 || i as usize >= len {
                        return Err(self.panic(format!("index out of bounds: the index is {i} but the length is {len}"), span));
                    }
                    &mut Rc::make_mut(xs)[i as usize]
                }
                (e, v) => return Err(self.panic(format!("internal error: cannot write through {e:?} into {v:?}"), span)),
            };
        }
        *slot = val;
        Ok(())
    }

    /// Move a value out of a place (for `&mut` arguments), leaving `()` behind.
    fn take(&self, locals: &mut [Value], p: &Place, span: Span) -> R<Value> {
        if p.proj.is_empty() {
            return Ok(std::mem::replace(&mut locals[p.local.0 as usize], Value::Unit));
        }
        let v = self.read(locals, p, span)?;
        // Replace with unit so the moved-out value is uniquely owned.
        self.write(locals, p, Value::Unit, span)?;
        Ok(v)
    }

    fn operand(&self, op: &Operand, locals: &[Value], span: Span) -> R<Value> {
        match op {
            Operand::Copy(p) => self.read(locals, p, span),
            Operand::Const(c) => Ok(match c {
                Const::Int(v, k) => {
                    if *k == IntTy::U64 {
                        Value::UInt(*v as u64)
                    } else {
                        Value::Int(*v as i64)
                    }
                }
                Const::Float(f) => Value::Float(*f),
                Const::Bool(b) => Value::Bool(*b),
                Const::Char(c) => Value::Char(*c),
                Const::Str(s) => Value::Str(Rc::from(s.as_str())),
                Const::Unit => Value::Unit,
                Const::Fn(f) => Value::Fn(*f),
            }),
        }
    }

    // ---------- rvalues ----------

    fn rvalue(&mut self, rv: &Rvalue, locals: &mut [Value], span: Span) -> R<Value> {
        match rv {
            Rvalue::Use(op) => self.operand(op, locals, span),
            Rvalue::Binary(op, a, b, ty) => {
                let x = self.operand(a, locals, span)?;
                let y = self.operand(b, locals, span)?;
                self.binary(*op, x, y, *ty, span)
            }
            Rvalue::Unary(op, a, ty) => {
                let x = self.operand(a, locals, span)?;
                self.unary(*op, x, *ty, span)
            }
            Rvalue::Cast(a, from, to) => {
                let x = self.operand(a, locals, span)?;
                self.cast(x, *from, *to, span)
            }
            Rvalue::Aggregate(kind, _, ops) => {
                let mut vals = Vec::with_capacity(ops.len());
                for o in ops {
                    vals.push(self.operand(o, locals, span)?);
                }
                Ok(match kind {
                    AggKind::Tuple | AggKind::Struct(_) => Value::Tuple(Rc::new(vals)),
                    AggKind::Variant(_, v) => Value::Variant(*v, Rc::new(vals)),
                    AggKind::List => Value::List(Rc::new(vals)),
                })
            }
            Rvalue::Discriminant(p) => match self.read(locals, p, span)? {
                Value::Variant(t, _) => Ok(Value::Int(t as i64)),
                v => Err(self.panic(format!("internal error: discriminant of {v:?}"), span)),
            },
            Rvalue::Closure(f, caps) => {
                let mut env = Vec::with_capacity(caps.len());
                for c in caps {
                    env.push(self.operand(c, locals, span)?);
                }
                Ok(Value::Closure(Rc::new((*f, env))))
            }
            Rvalue::Call(callee, args, _ret) => {
                let mut vals = Vec::with_capacity(args.len());
                let mut mut_places: Vec<&Place> = Vec::new();
                for a in args {
                    match a {
                        Arg::Value(o) => vals.push(self.operand(o, locals, span)?),
                        Arg::MutRef(p) => {
                            vals.push(self.take(locals, p, span)?);
                            mut_places.push(p);
                        }
                    }
                }
                let (ret, outs) = match callee {
                    Callee::Fn(f) => self.call(*f, vals)?,
                    Callee::Intrinsic(i) => self.intrinsic(i, vals, span)?,
                    Callee::Value(op) => match self.operand(op, locals, span)? {
                        Value::Fn(f) => self.call(f, vals)?,
                        Value::Closure(c) => {
                            let mut all = c.1.clone();
                            all.extend(vals);
                            self.call(c.0, all)?
                        }
                        v => return Err(self.panic(format!("internal error: calling {v:?}"), span)),
                    },
                };
                for (p, v) in mut_places.into_iter().zip(outs) {
                    self.write(locals, p, v, span)?;
                }
                Ok(ret)
            }
        }
    }

    fn int_kind(&self, ty: Ty) -> Option<IntTy> {
        match self.lw.tys.kind(ty) {
            TyKind::Int(k, _) => Some(*k),
            _ => None,
        }
    }

    fn fit(&self, v: i128, k: IntTy, span: Span, what: &str) -> R<Value> {
        if v < k.min() || v > k.max() {
            return Err(self.panic(format!("arithmetic overflow: {what} = {v} does not fit in `{}`", k.name()), span));
        }
        Ok(if k == IntTy::U64 { Value::UInt(v as u64) } else { Value::Int(v as i64) })
    }

    fn binary(&self, op: BinOp, x: Value, y: Value, ty: Ty, span: Span) -> R<Value> {
        use BinOp::*;
        if op.is_comparison() {
            let r = match op {
                Eq => val_eq(&x, &y),
                Ne => !val_eq(&x, &y),
                _ => {
                    if let (Value::Float(a), Value::Float(b)) = (&x, &y) {
                        match op {
                            Lt => a < b,
                            Le => a <= b,
                            Gt => a > b,
                            _ => a >= b,
                        }
                    } else {
                        let c = val_cmp(&x, &y);
                        match op {
                            Lt => c == Ordering::Less,
                            Le => c != Ordering::Greater,
                            Gt => c == Ordering::Greater,
                            _ => c != Ordering::Less,
                        }
                    }
                }
            };
            return Ok(Value::Bool(r));
        }
        match (x, y) {
            (Value::Float(a), Value::Float(b)) => Ok(Value::Float(match op {
                Add => a + b,
                Sub => a - b,
                Mul => a * b,
                Div => a / b,
                Rem => a % b,
                _ => return Err(self.panic("internal error: bad float op", span)),
            })),
            (x, y) => {
                let k = self.int_kind(ty).unwrap_or(IntTy::I64);
                let a = int_of(&x);
                let b = int_of(&y);
                let expr = format!("{a} {} {b}", op.as_str());
                let v: i128 = match op {
                    Add => a + b,
                    Sub => a - b,
                    Mul => a.checked_mul(b).unwrap_or(i128::MAX),
                    Div | Rem => {
                        if b == 0 {
                            return Err(self.panic(format!("division by zero: {expr}"), span));
                        }
                        if op == Div { a / b } else { a % b }
                    }
                    BitAnd => a & b,
                    BitOr => a | b,
                    BitXor => a ^ b,
                    Shl | Shr => {
                        if b < 0 || b >= k.bits() as i128 {
                            return Err(self.panic(format!("shift amount {b} is out of range for `{}`", k.name()), span));
                        }
                        if op == Shl {
                            // Shifts wrap within the type's bits (no overflow check).
                            let bits = k.bits();
                            let mask: u128 = if bits == 128 { u128::MAX } else { (1u128 << bits) - 1 };
                            let r = ((a as u128) << b) & mask;
                            return Ok(from_bits(r, k));
                        }
                        a >> b
                    }
                    _ => return Err(self.panic("internal error: bad int op", span)),
                };
                if matches!(op, BitAnd | BitOr | BitXor) {
                    return Ok(from_bits(v as u128 & mask_of(k), k));
                }
                self.fit(v, k, span, &expr)
            }
        }
    }

    fn unary(&self, op: UnOp, x: Value, ty: Ty, span: Span) -> R<Value> {
        match (op, x) {
            (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
            (UnOp::Neg, Value::Float(f)) => Ok(Value::Float(-f)),
            (UnOp::Neg, v) => {
                let k = self.int_kind(ty).unwrap_or(IntTy::I64);
                let a = int_of(&v);
                self.fit(-a, k, span, &format!("-({a})"))
            }
            (UnOp::BitNot, v) => {
                let k = self.int_kind(ty).unwrap_or(IntTy::I64);
                let a = int_of(&v);
                Ok(from_bits(!(a as u128) & mask_of(k), k))
            }
            (op, v) => Err(self.panic(format!("internal error: {op:?} on {v:?}"), span)),
        }
    }

    fn cast(&self, x: Value, _from: Ty, to: Ty, span: Span) -> R<Value> {
        match self.lw.tys.kind(to).clone() {
            TyKind::Float(_) => Ok(Value::Float(match x {
                Value::Float(f) => f,
                Value::UInt(u) => u as f64,
                v => int_of(&v) as f64,
            })),
            TyKind::Int(k, _) => {
                let v: i128 = match x {
                    Value::Float(f) => {
                        if f.is_nan() || f.trunc() < k.min() as f64 || f.trunc() > k.max() as f64 {
                            return Err(self.panic(format!("cannot convert {f:?} to `{}`: out of range", k.name()), span));
                        }
                        f.trunc() as i128
                    }
                    Value::Char(c) => c as i128,
                    v => int_of(&v),
                };
                if v < k.min() || v > k.max() {
                    return Err(self.panic(format!("cannot convert {v} to `{}`: out of range", k.name()), span));
                }
                Ok(if k == IntTy::U64 { Value::UInt(v as u64) } else { Value::Int(v as i64) })
            }
            TyKind::Char => {
                let v = int_of(&x);
                match u32::try_from(v).ok().and_then(char::from_u32) {
                    Some(c) => Ok(Value::Char(c as u32)),
                    None => Err(self.panic(format!("{v} is not a valid `Char`"), span)),
                }
            }
            _ => Ok(x),
        }
    }

    // ---------- display ----------

    /// Format a value the way `.str()` does. `nested` quotes strings and characters.
    pub fn show(&mut self, v: &Value, ty: Ty, nested: bool) -> R<String> {
        let kind = self.lw.tys.kind(ty).clone();
        Ok(match (v, kind) {
            (Value::Str(s), _) => {
                if nested { format!("{:?}", &**s) } else { s.to_string() }
            }
            (Value::Char(c), _) => {
                let ch = char::from_u32(*c).unwrap_or('?');
                if nested { format!("{ch:?}") } else { ch.to_string() }
            }
            (Value::Int(i), _) => i.to_string(),
            (Value::UInt(u), _) => u.to_string(),
            (Value::Float(f), _) => fmt_float(*f),
            (Value::Bool(b), _) => b.to_string(),
            (Value::Unit, _) => "()".into(),
            (Value::List(xs), TyKind::List(e)) => {
                let mut parts = Vec::with_capacity(xs.len());
                for x in xs.iter() {
                    parts.push(self.show(x, e, true)?);
                }
                format!("[{}]", parts.join(", "))
            }
            (Value::Set(m), TyKind::Set(e)) => {
                let mut parts = Vec::new();
                for x in m.keys.clone().iter() {
                    parts.push(self.show(x, e, true)?);
                }
                format!("{{{}}}", parts.join(", "))
            }
            (Value::Map(m), TyKind::Map(kt, vt)) => {
                let mut parts = Vec::new();
                let (ks, vs) = (m.keys.clone(), m.vals.clone());
                for (k, x) in ks.iter().zip(vs.iter()) {
                    parts.push(format!("{}: {}", self.show(k, kt, true)?, self.show(x, vt, true)?));
                }
                format!("{{{}}}", parts.join(", "))
            }
            (Value::Tuple(xs), TyKind::Tuple(ts)) => {
                let mut parts = Vec::new();
                for (x, t) in xs.iter().zip(ts.iter()) {
                    parts.push(self.show(x, *t, true)?);
                }
                if parts.len() == 1 {
                    format!("({},)", parts[0])
                } else {
                    format!("({})", parts.join(", "))
                }
            }
            (v @ (Value::Tuple(_) | Value::Variant(..)), TyKind::Adt(a, args)) => {
                // A user-defined `str` takes precedence.
                if let Some(f) = self.lw.defs.method(TyKey::Adt(a), crate::util::sym::STR_METHOD) {
                    let m = self.lw.instance(f, args.clone());
                    let (r, _) = self.call(m, vec![v.clone()])?;
                    if let Value::Str(s) = r {
                        return Ok(s.to_string());
                    }
                }
                let ad = &self.lw.defs.adts[a];
                let name = ad.name;
                match (&ad.kind, v) {
                    (AdtKind::Struct { fields }, Value::Tuple(xs)) => {
                        let fs: Vec<(crate::util::Symbol, Ty)> = fields.iter().map(|f| (f.name, f.ty)).collect();
                        let mut parts = Vec::new();
                        for ((fname, fty), x) in fs.into_iter().zip(xs.iter()) {
                            let ft = self.lw.tys.subst(fty, &args);
                            parts.push(format!("{fname}: {}", self.show(x, ft, true)?));
                        }
                        if parts.is_empty() { format!("{name} {{}}") } else { format!("{name} {{ {} }}", parts.join(", ")) }
                    }
                    (AdtKind::Enum { variants }, Value::Variant(t, xs)) => {
                        let var = &variants[*t as usize];
                        let vname = var.name;
                        let ftys: Vec<Ty> = var.fields.iter().map(|f| f.ty).collect();
                        if xs.is_empty() {
                            vname.to_string()
                        } else {
                            let mut parts = Vec::new();
                            for (fty, x) in ftys.into_iter().zip(xs.iter()) {
                                let ft = self.lw.tys.subst(fty, &args);
                                parts.push(self.show(x, ft, true)?);
                            }
                            format!("{vname}({})", parts.join(", "))
                        }
                    }
                    _ => "?".into(),
                }
            }
            (Value::Fn(_) | Value::Closure(_), _) => "<fn>".into(),
            (v, _) => format!("{v:?}"),
        })
    }

    // ---------- intrinsics ----------

    fn str_arg(&self, v: &Value, span: Span) -> R<Rc<str>> {
        match v {
            Value::Str(s) => Ok(s.clone()),
            _ => Err(self.panic("internal error: expected a string", span)),
        }
    }

    fn some(v: Value) -> Value {
        Value::Variant(0, Rc::new(vec![v]))
    }
    fn none() -> Value {
        Value::Variant(1, Rc::new(Vec::new()))
    }
    fn ok(v: Value) -> Value {
        Value::Variant(0, Rc::new(vec![v]))
    }
    fn err(msg: String) -> Value {
        Value::Variant(1, Rc::new(vec![Value::Str(Rc::from(msg.as_str()))]))
    }

    fn intrinsic(&mut self, intr: &Intrinsic, mut a: Vec<Value>, span: Span) -> R<(Value, Vec<Value>)> {
        use Op::*;
        let none = Vec::new();
        let ret = |v: Value| Ok((v, Vec::new()));
        macro_rules! list_mut {
            () => {{
                match &mut a[0] {
                    Value::List(xs) => Rc::make_mut(xs),
                    _ => return Err(self.panic("internal error: expected a list", span)),
                }
            }};
        }
        macro_rules! omap_mut {
            () => {{
                match &mut a[0] {
                    Value::Map(m) | Value::Set(m) => Rc::make_mut(m),
                    _ => return Err(self.panic("internal error: expected a map", span)),
                }
            }};
        }
        let list = |v: &Value| -> Option<Rc<Vec<Value>>> {
            match v {
                Value::List(xs) => Some(xs.clone()),
                _ => None,
            }
        };
        let omap = |v: &Value| -> Option<Rc<OMap>> {
            match v {
                Value::Map(m) | Value::Set(m) => Some(m.clone()),
                _ => None,
            }
        };
        let int = |v: &Value| int_of(v);
        let flt = |v: &Value| match v {
            Value::Float(f) => *f,
            _ => 0.0,
        };
        match intr.op {
            Panic => {
                let s = self.str_arg(&a[0], span)?;
                Err(self.panic(format!("panic: {s}"), span))
            }
            Assert => {
                if !matches!(a[0], Value::Bool(true)) {
                    return Err(self.panic("assertion failed", span));
                }
                ret(Value::Unit)
            }
            IntAbs | IntMin | IntMax | IntPow | WrappingAdd | WrappingSub | WrappingMul => {
                let k = intr.int.unwrap_or(IntTy::I64);
                let x = int(&a[0]);
                let y = a.get(1).map(int).unwrap_or(0);
                match intr.op {
                    IntAbs => Ok((self.fit(x.abs(), k, span, &format!("abs({x})"))?, none)),
                    IntMin => Ok((self.fit(x.min(y), k, span, "min")?, none)),
                    IntMax => Ok((self.fit(x.max(y), k, span, "max")?, none)),
                    IntPow => {
                        if y < 0 {
                            return Err(self.panic(format!("negative exponent {y}"), span));
                        }
                        let mut r: i128 = 1;
                        for _ in 0..y.min(200) {
                            r = r.saturating_mul(x);
                            if r > i64::MAX as i128 * 4 || r < i64::MIN as i128 * 4 {
                                break;
                            }
                        }
                        Ok((self.fit(r, k, span, &format!("{x}.pow({y})"))?, none))
                    }
                    _ => {
                        let r = match intr.op {
                            WrappingAdd => x.wrapping_add(y),
                            WrappingSub => x.wrapping_sub(y),
                            _ => x.wrapping_mul(y),
                        };
                        Ok((from_bits(r as u128 & mask_of(k), k), none))
                    }
                }
            }
            FloatAbs => ret(Value::Float(flt(&a[0]).abs())),
            Sqrt => ret(Value::Float(flt(&a[0]).sqrt())),
            Floor => ret(Value::Float(flt(&a[0]).floor())),
            Ceil => ret(Value::Float(flt(&a[0]).ceil())),
            Round => ret(Value::Float(flt(&a[0]).round())),
            FloatMin => ret(Value::Float(flt(&a[0]).min(flt(&a[1])))),
            FloatMax => ret(Value::Float(flt(&a[0]).max(flt(&a[1])))),
            IsNan => ret(Value::Bool(flt(&a[0]).is_nan())),
            IsDigit | IsAlpha | IsAlnum | IsSpace | IsUpper | IsLower | CharToUpper | CharToLower => {
                let c = match a[0] {
                    Value::Char(c) => char::from_u32(c).unwrap_or('\0'),
                    _ => '\0',
                };
                ret(match intr.op {
                    IsDigit => Value::Bool(c.is_ascii_digit()),
                    IsAlpha => Value::Bool(c.is_alphabetic()),
                    IsAlnum => Value::Bool(c.is_alphanumeric()),
                    IsSpace => Value::Bool(c.is_whitespace()),
                    IsUpper => Value::Bool(c.is_uppercase()),
                    IsLower => Value::Bool(c.is_lowercase()),
                    CharToUpper => Value::Char(c.to_uppercase().next().unwrap_or(c) as u32),
                    _ => Value::Char(c.to_lowercase().next().unwrap_or(c) as u32),
                })
            }
            StrLen => ret(Value::Int(self.str_arg(&a[0], span)?.len() as i64)),
            StrIsEmpty => ret(Value::Bool(self.str_arg(&a[0], span)?.is_empty())),
            StrContains | StartsWith | EndsWith => {
                let s = self.str_arg(&a[0], span)?;
                let t = self.str_arg(&a[1], span)?;
                ret(Value::Bool(match intr.op {
                    StrContains => s.contains(&*t),
                    StartsWith => s.starts_with(&*t),
                    _ => s.ends_with(&*t),
                }))
            }
            StrFind => {
                let s = self.str_arg(&a[0], span)?;
                let t = self.str_arg(&a[1], span)?;
                ret(match s.find(&*t) {
                    Some(i) => Self::some(Value::Int(i as i64)),
                    None => Self::none(),
                })
            }
            Split => {
                let s = self.str_arg(&a[0], span)?;
                let t = self.str_arg(&a[1], span)?;
                let parts: Vec<Value> = if t.is_empty() {
                    s.chars().map(|c| Value::Str(Rc::from(c.to_string().as_str()))).collect()
                } else {
                    s.split(&*t).map(|p| Value::Str(Rc::from(p))).collect()
                };
                ret(Value::List(Rc::new(parts)))
            }
            Lines => {
                let s = self.str_arg(&a[0], span)?;
                ret(Value::List(Rc::new(s.lines().map(|p| Value::Str(Rc::from(p))).collect())))
            }
            Trim => ret(Value::Str(Rc::from(self.str_arg(&a[0], span)?.trim()))),
            Replace => {
                let s = self.str_arg(&a[0], span)?;
                let f = self.str_arg(&a[1], span)?;
                let t = self.str_arg(&a[2], span)?;
                if f.is_empty() {
                    return ret(Value::Str(s));
                }
                ret(Value::Str(Rc::from(s.replace(&*f, &t).as_str())))
            }
            Repeat => {
                let s = self.str_arg(&a[0], span)?;
                let n = int(&a[1]);
                if n < 0 {
                    return Err(self.panic(format!("repeat count {n} is negative"), span));
                }
                ret(Value::Str(Rc::from(s.repeat(n as usize).as_str())))
            }
            StrToUpper => ret(Value::Str(Rc::from(self.str_arg(&a[0], span)?.to_uppercase().as_str()))),
            StrToLower => ret(Value::Str(Rc::from(self.str_arg(&a[0], span)?.to_lowercase().as_str()))),
            ToInt => {
                let s = self.str_arg(&a[0], span)?;
                ret(match s.trim().parse::<i64>() {
                    Ok(v) => Self::some(Value::Int(v)),
                    Err(_) => Self::none(),
                })
            }
            ToFloat => {
                let s = self.str_arg(&a[0], span)?;
                ret(match s.trim().parse::<f64>() {
                    Ok(v) => Self::some(Value::Float(v)),
                    Err(_) => Self::none(),
                })
            }
            Bytes => {
                let s = self.str_arg(&a[0], span)?;
                ret(Value::List(Rc::new(s.bytes().map(|b| Value::Int(b as i64)).collect())))
            }
            Chars => {
                let s = self.str_arg(&a[0], span)?;
                ret(Value::List(Rc::new(s.chars().map(|c| Value::Char(c as u32)).collect())))
            }
            FromChars => {
                let xs = list(&a[0]).unwrap_or_default();
                let s: String = xs.iter().filter_map(|v| match v {
                    Value::Char(c) => char::from_u32(*c),
                    _ => None,
                }).collect();
                ret(Value::Str(Rc::from(s.as_str())))
            }
            FromBytes => {
                let xs = list(&a[0]).unwrap_or_default();
                let bytes: Vec<u8> = xs.iter().map(|v| int(v) as u8).collect();
                ret(match String::from_utf8(bytes) {
                    Ok(s) => Self::some(Value::Str(Rc::from(s.as_str()))),
                    Err(_) => Self::none(),
                })
            }
            StrConcat => {
                let s = self.str_arg(&a[0], span)?;
                let t = self.str_arg(&a[1], span)?;
                let mut out = String::with_capacity(s.len() + t.len());
                out.push_str(&s);
                out.push_str(&t);
                ret(Value::Str(Rc::from(out.as_str())))
            }
            StrByte => {
                let s = self.str_arg(&a[0], span)?;
                let i = int(&a[1]);
                if i < 0 || i as usize >= s.len() {
                    return Err(self.panic(format!("index out of bounds: the index is {i} but the length is {}", s.len()), span));
                }
                ret(Value::Int(s.as_bytes()[i as usize] as i64))
            }
            StrSlice => {
                let s = self.str_arg(&a[0], span)?;
                let (lo, hi) = (int(&a[1]), int(&a[2]));
                if lo < 0 || hi < lo || hi as usize > s.len() {
                    return Err(self.panic(format!("slice {lo}..{hi} is out of bounds for a string of length {}", s.len()), span));
                }
                match s.get(lo as usize..hi as usize) {
                    Some(t) => ret(Value::Str(Rc::from(t))),
                    None => Err(self.panic(format!("slice {lo}..{hi} does not fall on character boundaries"), span)),
                }
            }
            ListSlice => {
                let xs = list(&a[0]).unwrap_or_default();
                let (lo, hi) = (int(&a[1]), int(&a[2]));
                if lo < 0 || hi < lo || hi as usize > xs.len() {
                    return Err(self.panic(format!("slice {lo}..{hi} is out of bounds for a list of length {}", xs.len()), span));
                }
                ret(Value::List(Rc::new(xs[lo as usize..hi as usize].to_vec())))
            }
            ListNew => ret(Value::List(Rc::new(Vec::new()))),
            ListLen => ret(Value::Int(list(&a[0]).map(|x| x.len()).unwrap_or(0) as i64)),
            ListIsEmpty => ret(Value::Bool(list(&a[0]).is_none_or(|x| x.is_empty()))),
            Get => {
                let xs = list(&a[0]).unwrap_or_default();
                let i = int(&a[1]);
                ret(if i >= 0 && (i as usize) < xs.len() { Self::some(xs[i as usize].clone()) } else { Self::none() })
            }
            Push => {
                let item = a.pop().unwrap();
                list_mut!().push(item);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Pop => {
                let v = list_mut!().pop();
                let r = match v {
                    Some(v) => Self::some(v),
                    None => Self::none(),
                };
                Ok((r, vec![a.swap_remove(0)]))
            }
            Insert => {
                let item = a.pop().unwrap();
                let i = int(&a[1]);
                let xs = list_mut!();
                if i < 0 || i as usize > xs.len() {
                    let n = xs.len();
                    return Err(self.panic(format!("insert index {i} is out of bounds for length {n}"), span));
                }
                xs.insert(i as usize, item);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Remove => {
                let i = int(&a[1]);
                let xs = list_mut!();
                if i < 0 || i as usize >= xs.len() {
                    let n = xs.len();
                    return Err(self.panic(format!("remove index {i} is out of bounds for length {n}"), span));
                }
                let v = xs.remove(i as usize);
                Ok((v, vec![a.swap_remove(0)]))
            }
            Clear => {
                list_mut!().clear();
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Extend => {
                let other = list(&a[1]).unwrap_or_default();
                list_mut!().extend(other.iter().cloned());
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Reverse => {
                list_mut!().reverse();
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Swap => {
                let (i, j) = (int(&a[1]), int(&a[2]));
                let xs = list_mut!();
                let n = xs.len() as i128;
                if i < 0 || j < 0 || i >= n || j >= n {
                    return Err(self.panic(format!("swap({i}, {j}) is out of bounds for length {n}"), span));
                }
                xs.swap(i as usize, j as usize);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Truncate => {
                let n = int(&a[1]);
                if n < 0 {
                    return Err(self.panic(format!("truncate length {n} is negative"), span));
                }
                list_mut!().truncate(n as usize);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            Sort => {
                list_mut!().sort_by(val_cmp);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            SortBy => {
                let f = a[1].clone();
                let mut xs: Vec<Value> = list(&a[0]).map(|x| x.to_vec()).unwrap_or_default();
                // Stable merge sort calling the comparator (which may trap).
                let mut err = None;
                let mut buf = xs.clone();
                self.merge_sort(&mut xs, &mut buf, &f, span, &mut err);
                if let Some(e) = err {
                    return Err(e);
                }
                Ok((Value::Unit, vec![Value::List(Rc::new(xs))]))
            }
            MapNew => ret(Value::Map(Rc::new(OMap::default()))),
            SetNew => ret(Value::Set(Rc::new(OMap::default()))),
            MapLen | SetLen => ret(Value::Int(omap(&a[0]).map(|m| m.len()).unwrap_or(0) as i64)),
            MapIsEmpty | SetIsEmpty => ret(Value::Bool(omap(&a[0]).is_none_or(|m| m.len() == 0))),
            MapGet => {
                let m = omap(&a[0]).unwrap_or_default();
                ret(match m.get(&a[1]) {
                    Some(v) => Self::some(v.clone()),
                    None => Self::none(),
                })
            }
            MapContains | SetContains => {
                let m = omap(&a[0]).unwrap_or_default();
                ret(Value::Bool(m.contains(&a[1])))
            }
            MapInsert => {
                let v = a.pop().unwrap();
                let k = a.pop().unwrap();
                omap_mut!().insert(k, v);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            SetInsert => {
                let k = a.pop().unwrap();
                omap_mut!().insert(k, Value::Unit);
                Ok((Value::Unit, vec![a.swap_remove(0)]))
            }
            MapRemove => {
                let k = a.pop().unwrap();
                let r = match omap_mut!().remove(&k) {
                    Some(v) => Self::some(v),
                    None => Self::none(),
                };
                Ok((r, vec![a.swap_remove(0)]))
            }
            SetRemove => {
                let k = a.pop().unwrap();
                let r = omap_mut!().remove(&k).is_some();
                Ok((Value::Bool(r), vec![a.swap_remove(0)]))
            }
            Keys | Items => ret(Value::List(Rc::new(omap(&a[0]).map(|m| m.keys.clone()).unwrap_or_default()))),
            Values => ret(Value::List(Rc::new(omap(&a[0]).map(|m| m.vals.clone()).unwrap_or_default()))),
            Entries => {
                let m = omap(&a[0]).unwrap_or_default();
                let es: Vec<Value> =
                    m.keys.iter().zip(m.vals.iter()).map(|(k, v)| Value::Tuple(Rc::new(vec![k.clone(), v.clone()]))).collect();
                ret(Value::List(Rc::new(es)))
            }
            Show => {
                let t = intr.tys[0];
                let s = self.show(&a[0], t, false)?;
                ret(Value::Str(Rc::from(s.as_str())))
            }
            Print | Println | Eprintln => {
                let s = self.str_arg(&a[0], span)?;
                let nl = if intr.op == Print { "" } else { "\n" };
                if self.fx.fake {
                    let buf = if intr.op == Eprintln { &mut self.fx.stderr } else { &mut self.fx.stdout };
                    buf.push_str(&s);
                    buf.push_str(nl);
                } else {
                    use std::io::Write;
                    if intr.op == Eprintln {
                        let _ = write!(std::io::stderr(), "{s}{nl}");
                    } else {
                        let mut out = std::io::stdout().lock();
                        let _ = write!(out, "{s}{nl}");
                        if intr.op == Print {
                            let _ = out.flush();
                        }
                    }
                }
                ret(Value::Unit)
            }
            ReadLine => {
                if self.fx.fake {
                    let r = match self.fx.stdin.get(self.fx.stdin_pos) {
                        Some(l) => {
                            self.fx.stdin_pos += 1;
                            Self::some(Value::Str(Rc::from(l.as_str())))
                        }
                        None => Self::none(),
                    };
                    return ret(r);
                }
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) | Err(_) => ret(Self::none()),
                    Ok(_) => {
                        let l = line.trim_end_matches(['\n', '\r']);
                        ret(Self::some(Value::Str(Rc::from(l))))
                    }
                }
            }
            ReadAll => {
                if self.fx.fake {
                    let rest = self.fx.stdin[self.fx.stdin_pos.min(self.fx.stdin.len())..].join("\n");
                    self.fx.stdin_pos = self.fx.stdin.len();
                    return ret(Value::Str(Rc::from(rest.as_str())));
                }
                let mut s = String::new();
                let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut s);
                ret(Value::Str(Rc::from(s.as_str())))
            }
            FsRead => {
                let p = self.str_arg(&a[0], span)?;
                if self.fx.fake {
                    return ret(match self.fx.files.get(&*p) {
                        Some(c) => Self::ok(Value::Str(Rc::from(c.as_str()))),
                        None => Self::err(format!("{p}: no such file")),
                    });
                }
                ret(match std::fs::read_to_string(&*p) {
                    Ok(c) => Self::ok(Value::Str(Rc::from(c.as_str()))),
                    Err(e) => Self::err(format!("{p}: {e}")),
                })
            }
            FsWrite => {
                let p = self.str_arg(&a[0], span)?;
                let c = self.str_arg(&a[1], span)?;
                if self.fx.fake {
                    self.fx.files.insert(p.to_string(), c.to_string());
                    return ret(Self::ok(Value::Unit));
                }
                ret(match std::fs::write(&*p, c.as_bytes()) {
                    Ok(()) => Self::ok(Value::Unit),
                    Err(e) => Self::err(format!("{p}: {e}")),
                })
            }
            FsExists => {
                let p = self.str_arg(&a[0], span)?;
                if self.fx.fake {
                    return ret(Value::Bool(self.fx.files.contains_key(&*p)));
                }
                ret(Value::Bool(std::path::Path::new(&*p).exists()))
            }
            FsList => {
                let p = self.str_arg(&a[0], span)?;
                if self.fx.fake {
                    let prefix = format!("{}/", p.trim_end_matches('/'));
                    let mut names: Vec<String> = self.fx.files.keys().filter_map(|k| k.strip_prefix(&prefix).map(|r| r.split('/').next().unwrap_or(r).to_string())).collect();
                    names.sort();
                    names.dedup();
                    return ret(Self::ok(Value::List(Rc::new(names.into_iter().map(|n| Value::Str(Rc::from(n.as_str()))).collect()))));
                }
                ret(match std::fs::read_dir(&*p) {
                    Ok(rd) => {
                        let mut names: Vec<String> = rd.filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().to_string()).collect();
                        names.sort();
                        Self::ok(Value::List(Rc::new(names.into_iter().map(|n| Value::Str(Rc::from(n.as_str()))).collect())))
                    }
                    Err(e) => Self::err(format!("{p}: {e}")),
                })
            }
            Args => ret(Value::List(Rc::new(self.fx.args.iter().map(|s| Value::Str(Rc::from(s.as_str()))).collect()))),
            EnvVar => {
                let k = self.str_arg(&a[0], span)?;
                let v = if self.fx.fake { self.fx.env.get(&*k).cloned() } else { std::env::var(&*k).ok() };
                ret(match v {
                    Some(v) => Self::some(Value::Str(Rc::from(v.as_str()))),
                    None => Self::none(),
                })
            }
            Exit => Err(Trap::Exit(int(&a[0]) as i32)),
            NowMs | NowNs => {
                if self.fx.fake {
                    self.fx.clock_ms += 1;
                    let ms = self.fx.clock_ms;
                    return ret(Value::Int(if intr.op == NowMs { ms } else { ms * 1_000_000 }));
                }
                let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
                ret(Value::Int(if intr.op == NowMs { d.as_millis() as i64 } else { d.as_nanos() as i64 }))
            }
            RandInt => {
                let (lo, hi) = (int(&a[0]), int(&a[1]));
                if lo >= hi {
                    return Err(self.panic(format!("precondition failed: lo < hi\n  lo = {lo}\n  hi = {hi}"), span));
                }
                let span_w = (hi - lo) as u128;
                let r = (self.fx.next_rand() as u128) % span_w;
                ret(Value::Int((lo + r as i128) as i64))
            }
            RandFloat => {
                let r = (self.fx.next_rand() >> 11) as f64 / (1u64 << 53) as f64;
                ret(Value::Float(r))
            }
            Sin => ret(Value::Float(flt(&a[0]).sin())),
            Cos => ret(Value::Float(flt(&a[0]).cos())),
            Tan => ret(Value::Float(flt(&a[0]).tan())),
            Atan2 => ret(Value::Float(flt(&a[0]).atan2(flt(&a[1])))),
            Exp => ret(Value::Float(flt(&a[0]).exp())),
            Ln => ret(Value::Float(flt(&a[0]).ln())),
            Log10 => ret(Value::Float(flt(&a[0]).log10())),
            Pow => ret(Value::Float(flt(&a[0]).powf(flt(&a[1])))),
        }
    }

    fn call_value(&mut self, f: &Value, args: Vec<Value>, span: Span) -> R<Value> {
        match f {
            Value::Fn(m) => Ok(self.call(*m, args)?.0),
            Value::Closure(c) => {
                let mut all = c.1.clone();
                all.extend(args);
                Ok(self.call(c.0, all)?.0)
            }
            v => Err(self.panic(format!("internal error: calling {v:?}"), span)),
        }
    }

    fn merge_sort(&mut self, xs: &mut [Value], buf: &mut [Value], less: &Value, span: Span, err: &mut Option<Trap>) {
        let n = xs.len();
        if n <= 1 || err.is_some() {
            return;
        }
        let mid = n / 2;
        {
            let (a, b) = xs.split_at_mut(mid);
            let (ba, bb) = buf.split_at_mut(mid);
            self.merge_sort(a, ba, less, span, err);
            self.merge_sort(b, bb, less, span, err);
        }
        if err.is_some() {
            return;
        }
        let (mut i, mut j, mut k) = (0, mid, 0);
        while i < mid && j < n {
            // Take from the right only if right < left (stability).
            let r = self.call_value(less, vec![xs[j].clone(), xs[i].clone()], span);
            let take_right = match r {
                Ok(Value::Bool(b)) => b,
                Ok(_) => false,
                Err(e) => {
                    *err = Some(e);
                    return;
                }
            };
            if take_right {
                buf[k] = xs[j].clone();
                j += 1;
            } else {
                buf[k] = xs[i].clone();
                i += 1;
            }
            k += 1;
        }
        while i < mid {
            buf[k] = xs[i].clone();
            i += 1;
            k += 1;
        }
        while j < n {
            buf[k] = xs[j].clone();
            j += 1;
            k += 1;
        }
        xs.clone_from_slice(&buf[..n]);
    }
}

fn int_of(v: &Value) -> i128 {
    match v {
        Value::Int(i) => *i as i128,
        Value::UInt(u) => *u as i128,
        Value::Char(c) => *c as i128,
        Value::Bool(b) => *b as i128,
        _ => 0,
    }
}

fn mask_of(k: IntTy) -> u128 {
    let bits = k.bits();
    (1u128 << bits) - 1
}

/// Interpret the low bits of `r` as a value of type `k`.
fn from_bits(r: u128, k: IntTy) -> Value {
    match k {
        IntTy::U64 => Value::UInt(r as u64),
        IntTy::I64 => Value::Int(r as u64 as i64),
        IntTy::I32 => Value::Int(r as u32 as i32 as i64),
        IntTy::U32 => Value::Int(r as u32 as i64),
        IntTy::U8 => Value::Int(r as u8 as i64),
    }
}

pub fn fmt_float(f: f64) -> String {
    if f.is_nan() {
        "NaN".into()
    } else if f.is_infinite() {
        if f > 0.0 { "inf".into() } else { "-inf".into() }
    } else {
        format!("{f:?}")
    }
}

/// Unused-unit helper kept for symmetry with native back ends.
pub fn unitless(t: crate::sema::ty::UnitId) -> bool {
    t == NO_UNIT
}
