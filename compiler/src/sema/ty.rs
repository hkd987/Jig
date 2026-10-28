//! Semantic types: hash-consed `TyKind`s addressed by `Ty`, units of measure, effect sets.

use crate::util::{FxHashMap, Symbol};
use std::fmt::Write;

crate::define_idx!(Ty);
crate::define_idx!(AdtId);
crate::define_idx!(FnId);
crate::define_idx!(IfaceId);
crate::define_idx!(ConstId);
crate::define_idx!(AliasId);
crate::define_idx!(PkgId);
crate::define_idx!(UnitId);

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub enum IntTy {
    I64,
    I32,
    U8,
    U32,
    U64,
}

impl IntTy {
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I64 => "Int",
            IntTy::I32 => "I32",
            IntTy::U8 => "U8",
            IntTy::U32 => "U32",
            IntTy::U64 => "U64",
        }
    }
    pub fn signed(self) -> bool {
        matches!(self, IntTy::I64 | IntTy::I32)
    }
    pub fn bits(self) -> u32 {
        match self {
            IntTy::I64 | IntTy::U64 => 64,
            IntTy::I32 | IntTy::U32 => 32,
            IntTy::U8 => 8,
        }
    }
    pub fn min(self) -> i128 {
        match self {
            IntTy::I64 => i64::MIN as i128,
            IntTy::I32 => i32::MIN as i128,
            _ => 0,
        }
    }
    pub fn max(self) -> i128 {
        match self {
            IntTy::I64 => i64::MAX as i128,
            IntTy::I32 => i32::MAX as i128,
            IntTy::U8 => u8::MAX as i128,
            IntTy::U32 => u32::MAX as i128,
            IntTy::U64 => u64::MAX as i128,
        }
    }
}

/// A set of effects as a bitmask: bits 0 to 7 are the eight effects, and bit `8 + k` is an
/// effect parameter, the effects of whatever function is passed for parameter `k` of the
/// function whose signature it is in (named in its `effects` clause). `OPEN`, only in the
/// type a call expects for such an argument, admits any effects.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct EffectSet(pub u32);

pub const EFFECT_NAMES: [&str; 8] = ["io", "fs", "net", "clock", "rand", "env", "spawn", "ffi"];

/// The names of the effects in `e`: the eight in order, then its effect parameters, named by
/// `vars` (`_` past its end).
pub fn effect_names_in(e: EffectSet, vars: &[Symbol]) -> Vec<String> {
    let mut out: Vec<String> = e.names().iter().map(|n| n.to_string()).collect();
    for k in e.params() {
        out.push(vars.get(k).map_or_else(|| "_".to_string(), |s| s.to_string()));
    }
    out
}

/// How many parameters of one function can be effect parameters.
pub const MAX_EFFECT_PARAMS: usize = 23;

impl EffectSet {
    pub const PURE: EffectSet = EffectSet(0);
    pub const ALL: EffectSet = EffectSet(0xff);
    pub const OPEN: EffectSet = EffectSet(1 << 31);
    pub fn from_name(s: &str) -> Option<EffectSet> {
        EFFECT_NAMES.iter().position(|&n| n == s).map(|i| EffectSet(1 << i))
    }
    /// The effect parameter for parameter `k`.
    pub fn param(k: usize) -> EffectSet {
        EffectSet(1 << (8 + k))
    }
    /// The parameters whose effects this set includes.
    pub fn params(self) -> Vec<usize> {
        if self.0 & !(Self::ALL.0 | Self::OPEN.0) == 0 {
            return Vec::new();
        }
        (0..MAX_EFFECT_PARAMS).filter(|k| self.0 & (1 << (8 + k)) != 0).collect()
    }
    pub fn is_open(self) -> bool {
        self.0 & Self::OPEN.0 != 0
    }
    pub fn union(self, o: EffectSet) -> EffectSet {
        EffectSet(self.0 | o.0)
    }
    pub fn minus(self, o: EffectSet) -> EffectSet {
        EffectSet(self.0 & !o.0)
    }
    pub fn contains(self, o: EffectSet) -> bool {
        self.0 & o.0 == o.0
    }
    pub fn is_pure(self) -> bool {
        self.0 == 0
    }
    /// The names of the eight effects in the set, in order.
    pub fn names(self) -> Vec<&'static str> {
        (0..8).filter(|i| self.0 & (1 << i) != 0).map(|i| EFFECT_NAMES[i]).collect()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum TyKind {
    Int(IntTy, UnitId),
    Float(UnitId),
    Bool,
    Char,
    Str,
    Never,
    Error,
    Tuple(Box<[Ty]>),
    Adt(AdtId, Box<[Ty]>),
    List(Ty),
    Map(Ty, Ty),
    Set(Ty),
    Fn(Box<[Ty]>, Ty, EffectSet),
    /// Generic parameter `index` of the item being checked (named for display).
    Param(u32, Symbol),
    /// Inference variable (only during body checking).
    Infer(u32),
}

/// A unit of measure: product of base units with integer exponents, sorted by symbol.
pub type UnitFactors = Box<[(Symbol, i8)]>;

/// One result of interning, recorded while a body is checked on a copy of the context (see
/// `check::check_bodies`).
#[derive(Clone, Copy)]
pub enum Interned {
    Ty(Ty),
    Unit(UnitId),
}

#[derive(Clone)]
pub struct TyCtx {
    kinds: Vec<TyKind>,
    map: FxHashMap<TyKind, Ty>,
    units: Vec<UnitFactors>,
    unit_map: FxHashMap<UnitFactors, UnitId>,
    /// Names of ADTs, for display.
    pub adt_names: Vec<(Symbol, Symbol)>,
    pub int: Ty,
    pub i32: Ty,
    pub u8: Ty,
    pub u32: Ty,
    pub u64: Ty,
    pub float: Ty,
    pub bool: Ty,
    pub char: Ty,
    pub str: Ty,
    pub unit: Ty,
    pub never: Ty,
    pub error: Ty,
    /// When set, every type and unit created is recorded, in order.
    pub log: Option<Vec<Interned>>,
    /// The parameter names of the function being checked, which name its effect parameters
    /// when effects are shown (`_` without them).
    pub effect_vars: Vec<Symbol>,
}

pub const NO_UNIT: UnitId = UnitId(0);

impl Default for TyCtx {
    fn default() -> Self {
        Self::new()
    }
}

impl TyCtx {
    pub fn new() -> TyCtx {
        let mut t = TyCtx {
            kinds: Vec::new(),
            map: FxHashMap::default(),
            units: Vec::new(),
            unit_map: FxHashMap::default(),
            adt_names: Vec::new(),
            int: Ty(0),
            i32: Ty(0),
            u8: Ty(0),
            u32: Ty(0),
            u64: Ty(0),
            float: Ty(0),
            bool: Ty(0),
            char: Ty(0),
            str: Ty(0),
            unit: Ty(0),
            never: Ty(0),
            error: Ty(0),
            log: None,
            effect_vars: Vec::new(),
        };
        t.unit_id(&[]);
        t.int = t.intern(TyKind::Int(IntTy::I64, NO_UNIT));
        t.i32 = t.intern(TyKind::Int(IntTy::I32, NO_UNIT));
        t.u8 = t.intern(TyKind::Int(IntTy::U8, NO_UNIT));
        t.u32 = t.intern(TyKind::Int(IntTy::U32, NO_UNIT));
        t.u64 = t.intern(TyKind::Int(IntTy::U64, NO_UNIT));
        t.float = t.intern(TyKind::Float(NO_UNIT));
        t.bool = t.intern(TyKind::Bool);
        t.char = t.intern(TyKind::Char);
        t.str = t.intern(TyKind::Str);
        t.unit = t.intern(TyKind::Tuple(Box::new([])));
        t.never = t.intern(TyKind::Never);
        t.error = t.intern(TyKind::Error);
        t
    }

    pub fn intern(&mut self, k: TyKind) -> Ty {
        if let Some(&t) = self.map.get(&k) {
            return t;
        }
        let t = Ty(self.kinds.len() as u32);
        self.kinds.push(k.clone());
        self.map.insert(k, t);
        if let Some(log) = &mut self.log {
            log.push(Interned::Ty(t));
        }
        t
    }

    /// How many types and units exist.
    pub fn sizes(&self) -> (usize, usize) {
        (self.kinds.len(), self.units.len())
    }

    /// Intern a unit's factors as they are (already normalized).
    pub fn intern_unit(&mut self, key: UnitFactors) -> UnitId {
        if let Some(&u) = self.unit_map.get(&key) {
            return u;
        }
        let u = UnitId(self.units.len() as u32);
        self.units.push(key.clone());
        self.unit_map.insert(key, u);
        u
    }

    #[inline]
    pub fn kind(&self, t: Ty) -> &TyKind {
        &self.kinds[t.0 as usize]
    }

    pub fn int_ty(&mut self, k: IntTy, u: UnitId) -> Ty {
        self.intern(TyKind::Int(k, u))
    }
    pub fn tuple(&mut self, ts: Vec<Ty>) -> Ty {
        self.intern(TyKind::Tuple(ts.into_boxed_slice()))
    }
    pub fn list(&mut self, t: Ty) -> Ty {
        self.intern(TyKind::List(t))
    }
    pub fn map_ty(&mut self, k: Ty, v: Ty) -> Ty {
        self.intern(TyKind::Map(k, v))
    }
    pub fn set(&mut self, t: Ty) -> Ty {
        self.intern(TyKind::Set(t))
    }
    pub fn adt(&mut self, a: AdtId, args: Vec<Ty>) -> Ty {
        self.intern(TyKind::Adt(a, args.into_boxed_slice()))
    }
    pub fn func(&mut self, params: Vec<Ty>, ret: Ty, eff: EffectSet) -> Ty {
        self.intern(TyKind::Fn(params.into_boxed_slice(), ret, eff))
    }
    pub fn param(&mut self, i: u32, name: Symbol) -> Ty {
        self.intern(TyKind::Param(i, name))
    }
    pub fn infer(&mut self, v: u32) -> Ty {
        self.intern(TyKind::Infer(v))
    }

    pub fn is_unit(&self, t: Ty) -> bool {
        t == self.unit
    }
    pub fn is_never(&self, t: Ty) -> bool {
        t == self.never
    }
    pub fn is_error(&self, t: Ty) -> bool {
        t == self.error
    }

    // ---- units ----

    pub fn unit_id(&mut self, factors: &[(Symbol, i8)]) -> UnitId {
        let mut v: Vec<(Symbol, i8)> = Vec::new();
        for &(s, e) in factors {
            if let Some(slot) = v.iter_mut().find(|(x, _)| *x == s) {
                slot.1 += e;
            } else {
                v.push((s, e));
            }
        }
        v.retain(|&(_, e)| e != 0);
        v.sort_by(|a, b| a.0.as_str().cmp(b.0.as_str()));
        let n = self.units.len();
        let u = self.intern_unit(v.into_boxed_slice());
        if let Some(log) = &mut self.log {
            if self.units.len() > n {
                log.push(Interned::Unit(u));
            }
        }
        u
    }
    pub fn unit_factors(&self, u: UnitId) -> &[(Symbol, i8)] {
        &self.units[u.0 as usize]
    }
    pub fn unit_mul(&mut self, a: UnitId, b: UnitId, sign: i8) -> UnitId {
        let mut f: Vec<(Symbol, i8)> = self.unit_factors(a).to_vec();
        for &(s, e) in self.units[b.0 as usize].iter() {
            f.push((s, e * sign));
        }
        self.unit_id(&f)
    }
    pub fn unit_str(&self, u: UnitId) -> String {
        let f = self.unit_factors(u);
        if f.is_empty() {
            return String::new();
        }
        let num: Vec<String> = f
            .iter()
            .filter(|(_, e)| *e > 0)
            .map(|(s, e)| if *e == 1 { s.to_string() } else { format!("{s}^{e}") })
            .collect();
        let den: Vec<String> = f
            .iter()
            .filter(|(_, e)| *e < 0)
            .map(|(s, e)| if *e == -1 { s.to_string() } else { format!("{s}^{}", -e) })
            .collect();
        let mut out = if num.is_empty() { "1".to_string() } else { num.join("*") };
        for d in den {
            out.push('/');
            out.push_str(&d);
        }
        out
    }
    /// The same numeric type with a different unit.
    pub fn with_unit(&mut self, t: Ty, u: UnitId) -> Ty {
        match *self.kind(t) {
            TyKind::Int(k, _) => self.intern(TyKind::Int(k, u)),
            TyKind::Float(_) => self.intern(TyKind::Float(u)),
            _ => t,
        }
    }
    pub fn unit_of(&self, t: Ty) -> Option<UnitId> {
        match *self.kind(t) {
            TyKind::Int(_, u) | TyKind::Float(u) => Some(u),
            _ => None,
        }
    }

    // ---- queries ----

    pub fn is_numeric(&self, t: Ty) -> bool {
        matches!(self.kind(t), TyKind::Int(..) | TyKind::Float(_))
    }
    pub fn is_int(&self, t: Ty) -> bool {
        matches!(self.kind(t), TyKind::Int(..))
    }

    /// Replace `Param(i)` with `args[i]`.
    pub fn subst(&mut self, t: Ty, args: &[Ty]) -> Ty {
        if args.is_empty() {
            return t;
        }
        self.map_ty_rec(t, &mut |tc, t| match *tc.kind(t) {
            TyKind::Param(i, _) => args.get(i as usize).copied(),
            _ => None,
        })
    }

    /// Rebuild a type bottom-up, letting `f` replace nodes (returning `Some`).
    pub fn map_ty_rec(&mut self, t: Ty, f: &mut dyn FnMut(&mut TyCtx, Ty) -> Option<Ty>) -> Ty {
        if let Some(r) = f(self, t) {
            return r;
        }
        match self.kind(t).clone() {
            TyKind::Tuple(ts) => {
                let ts: Vec<Ty> = ts.iter().map(|&x| self.map_ty_rec(x, f)).collect();
                self.tuple(ts)
            }
            TyKind::Adt(a, ts) => {
                let ts: Vec<Ty> = ts.iter().map(|&x| self.map_ty_rec(x, f)).collect();
                self.adt(a, ts)
            }
            TyKind::List(x) => {
                let x = self.map_ty_rec(x, f);
                self.list(x)
            }
            TyKind::Set(x) => {
                let x = self.map_ty_rec(x, f);
                self.set(x)
            }
            TyKind::Map(k, v) => {
                let k = self.map_ty_rec(k, f);
                let v = self.map_ty_rec(v, f);
                self.map_ty(k, v)
            }
            TyKind::Fn(ps, r, e) => {
                let ps: Vec<Ty> = ps.iter().map(|&x| self.map_ty_rec(x, f)).collect();
                let r = self.map_ty_rec(r, f);
                self.func(ps, r, e)
            }
            _ => t,
        }
    }

    /// Does the type mention any inference variable or generic parameter?
    pub fn has_vars(&self, t: Ty) -> bool {
        match self.kind(t) {
            TyKind::Infer(_) | TyKind::Param(..) => true,
            TyKind::Tuple(ts) | TyKind::Adt(_, ts) => ts.iter().any(|&x| self.has_vars(x)),
            TyKind::List(x) | TyKind::Set(x) => self.has_vars(*x),
            TyKind::Map(k, v) => self.has_vars(*k) || self.has_vars(*v),
            TyKind::Fn(ps, r, _) => ps.iter().any(|&x| self.has_vars(x)) || self.has_vars(*r),
            _ => false,
        }
    }

    pub fn has_infer(&self, t: Ty) -> bool {
        match self.kind(t) {
            TyKind::Infer(_) => true,
            TyKind::Tuple(ts) | TyKind::Adt(_, ts) => ts.iter().any(|&x| self.has_infer(x)),
            TyKind::List(x) | TyKind::Set(x) => self.has_infer(*x),
            TyKind::Map(k, v) => self.has_infer(*k) || self.has_infer(*v),
            TyKind::Fn(ps, r, _) => ps.iter().any(|&x| self.has_infer(x)) || self.has_infer(*r),
            _ => false,
        }
    }

    /// The names of the effects in `e`: the eight in order, then its effect parameters.
    pub fn effect_names(&self, e: EffectSet) -> Vec<String> {
        effect_names_in(e, &self.effect_vars)
    }

    /// `e` as written in an `effects` clause: `{io, net}`.
    pub fn effects_str(&self, e: EffectSet) -> String {
        format!("{{{}}}", self.effect_names(e).join(", "))
    }

    pub fn display(&self, t: Ty) -> String {
        let mut s = String::new();
        self.write_ty(t, &mut s, &self.effect_vars, None);
        s
    }

    /// `t` with its effect parameters named by `vars`, leaving out parameter `skip`'s (a
    /// parameter's type, as its function's signature writes it).
    pub fn display_in(&self, t: Ty, vars: &[Symbol], skip: Option<usize>) -> String {
        let mut s = String::new();
        self.write_ty(t, &mut s, vars, skip);
        s
    }

    fn write_ty(&self, t: Ty, out: &mut String, vars: &[Symbol], skip: Option<usize>) {
        match self.kind(t) {
            TyKind::Int(k, u) => {
                out.push_str(k.name());
                if *u != NO_UNIT {
                    let _ = write!(out, "<{}>", self.unit_str(*u));
                }
            }
            TyKind::Float(u) => {
                out.push_str("Float");
                if *u != NO_UNIT {
                    let _ = write!(out, "<{}>", self.unit_str(*u));
                }
            }
            TyKind::Bool => out.push_str("Bool"),
            TyKind::Char => out.push_str("Char"),
            TyKind::Str => out.push_str("Str"),
            TyKind::Never => out.push_str("Never"),
            TyKind::Error => out.push_str("{unknown}"),
            TyKind::Tuple(ts) => {
                out.push('(');
                for (i, &x) in ts.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write_ty(x, out, vars, skip);
                }
                if ts.len() == 1 {
                    out.push(',');
                }
                out.push(')');
            }
            TyKind::Adt(a, ts) => {
                let (pkg, name) = self.adt_names[a.0 as usize];
                if pkg != crate::util::sym::EMPTY {
                    let _ = write!(out, "{pkg}.");
                }
                out.push_str(name.as_str());
                self.write_args(ts, out, vars, skip);
            }
            TyKind::List(x) => {
                out.push_str("List");
                self.write_args(&[*x], out, vars, skip);
            }
            TyKind::Set(x) => {
                out.push_str("Set");
                self.write_args(&[*x], out, vars, skip);
            }
            TyKind::Map(k, v) => {
                out.push_str("Map");
                self.write_args(&[*k, *v], out, vars, skip);
            }
            TyKind::Fn(ps, r, e) => {
                out.push_str("fn(");
                for (i, &x) in ps.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    self.write_ty(x, out, vars, skip);
                }
                out.push(')');
                if !self.is_unit(*r) {
                    out.push_str(" -> ");
                    self.write_ty(*r, out, vars, skip);
                }
                let mut e = e.minus(EffectSet::OPEN);
                if let Some(k) = skip {
                    e = e.minus(EffectSet::param(k));
                }
                if !e.is_pure() {
                    let _ = write!(out, " effects {{{}}}", effect_names_in(e, vars).join(", "));
                }
            }
            TyKind::Param(_, name) => out.push_str(name.as_str()),
            TyKind::Infer(_) => out.push('_'),
        }
    }

    fn write_args(&self, ts: &[Ty], out: &mut String, vars: &[Symbol], skip: Option<usize>) {
        if ts.is_empty() {
            return;
        }
        out.push('<');
        for (i, &x) in ts.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            self.write_ty(x, out, vars, skip);
        }
        out.push('>');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_and_units() {
        let mut tc = TyCtx::new();
        let a = tc.list(tc.int);
        let b = tc.list(tc.int);
        assert_eq!(a, b);
        let m = Symbol::intern("m");
        let s = Symbol::intern("s");
        let mps = tc.unit_id(&[(m, 1), (s, -1)]);
        let mps2 = tc.unit_id(&[(s, -1), (m, 1)]);
        assert_eq!(mps, mps2);
        assert_eq!(tc.unit_str(mps), "m/s");
        let ms = tc.unit_id(&[(m, 1)]);
        let back = tc.unit_mul(mps, ms, -1);
        assert_eq!(tc.unit_str(back), "1/s");
        let f = tc.intern(TyKind::Float(mps));
        assert_eq!(tc.display(f), "Float<m/s>");
        let t = tc.tuple(vec![tc.int, tc.str]);
        assert_eq!(tc.display(t), "(Int, Str)");
    }
}
