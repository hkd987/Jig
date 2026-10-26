//! Inputs for property tests (`test "name"(x: Int) requires ... { ... }`): deterministic
//! generation by type, and shrinking toward small values once a case fails.

use crate::mir::interp::Value;
use crate::sema::defs::Defs;
use crate::sema::ty::{IntTy, Ty, TyCtx, TyKind};
use std::rc::Rc;

/// xorshift64*: small, fast and the same everywhere.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 {
            0
        } else {
            self.next() % n
        }
    }
    fn chance(&mut self, one_in: u64) -> bool {
        self.below(one_in) == 0
    }
}

fn int_in(r: &mut Rng, k: IntTy, size: u64) -> i128 {
    let (lo, hi) = (k.min(), k.max());
    if r.chance(6) {
        // Edge values find off-by-one and overflow bugs.
        let edges = [0, 1, -1, lo, hi, lo + 1, hi - 1, 2, -2];
        return edges[r.below(edges.len() as u64) as usize].clamp(lo, hi);
    }
    let span = if r.chance(10) { 1u64 << 31 } else { size.max(1) };
    let mag = r.below(span + 1) as i128;
    let v = if k.signed() && r.chance(2) { -mag } else { mag };
    v.clamp(lo, hi)
}

fn int_value(k: IntTy, v: i128) -> Value {
    if k == IntTy::U64 {
        Value::UInt(v as u64)
    } else {
        Value::Int(v as i64)
    }
}

fn int_of(v: &Value) -> Option<i128> {
    match v {
        Value::Int(i) => Some(*i as i128),
        Value::UInt(u) => Some(*u as i128),
        _ => None,
    }
}

/// A value of type `t`, or `None` for types we cannot generate (maps, functions).
pub fn generate(tys: &mut TyCtx, defs: &Defs, t: Ty, r: &mut Rng, size: u64, depth: u32) -> Option<Value> {
    Some(match tys.kind(t).clone() {
        TyKind::Int(k, _) => int_value(k, int_in(r, k, size)),
        TyKind::Float(_) => {
            if r.chance(8) {
                Value::Float(0.0)
            } else {
                Value::Float((r.below(2 * size + 1) as f64 - size as f64) / 4.0)
            }
        }
        TyKind::Bool => Value::Bool(r.chance(2)),
        TyKind::Char => Value::Char(gen_char(r)),
        TyKind::Str => {
            let n = r.below(size.min(12) + 1);
            let s: String = (0..n).filter_map(|_| char::from_u32(gen_char(r))).collect();
            Value::Str(Rc::from(s.as_str()))
        }
        TyKind::Tuple(ts) if ts.is_empty() => Value::Unit,
        TyKind::Tuple(ts) => {
            let mut xs = Vec::new();
            for &x in ts.iter() {
                xs.push(generate(tys, defs, x, r, size, depth + 1)?);
            }
            Value::Tuple(Rc::new(xs))
        }
        TyKind::List(e) => {
            let n = if depth > 2 { r.below(3) } else { r.below(size.min(8) + 1) };
            let mut xs = Vec::new();
            for _ in 0..n {
                xs.push(generate(tys, defs, e, r, size, depth + 1)?);
            }
            Value::List(Rc::new(xs))
        }
        TyKind::Adt(a, args) => {
            let ad = &defs.adts[a];
            if ad.is_enum() {
                let variants: Vec<Vec<Ty>> =
                    ad.variants().iter().map(|v| v.fields.iter().map(|f| f.ty).collect()).collect();
                // Deep in recursive types, prefer variants without payloads so values stay finite.
                let leafy: Vec<usize> = (0..variants.len()).filter(|&i| variants[i].is_empty()).collect();
                let vi = if depth > 3 && !leafy.is_empty() {
                    leafy[r.below(leafy.len() as u64) as usize]
                } else {
                    r.below(variants.len() as u64) as usize
                };
                let mut fields = Vec::new();
                for &ft in &variants[vi] {
                    let ft = tys.subst(ft, &args);
                    fields.push(generate(tys, defs, ft, r, size, depth + 1)?);
                }
                Value::Variant(vi as u32, Rc::new(fields))
            } else {
                let fts: Vec<Ty> = ad.fields().iter().map(|f| f.ty).collect();
                let mut xs = Vec::new();
                for ft in fts {
                    let ft = tys.subst(ft, &args);
                    xs.push(generate(tys, defs, ft, r, size, depth + 1)?);
                }
                Value::Tuple(Rc::new(xs))
            }
        }
        _ => return None,
    })
}

fn gen_char(r: &mut Rng) -> u32 {
    match r.below(10) {
        0 => ' ' as u32,
        1 => '0' as u32 + r.below(10) as u32,
        2 => ['é', 'ß', '中', '→'][r.below(4) as usize] as u32,
        3 => 'A' as u32 + r.below(26) as u32,
        _ => 'a' as u32 + r.below(26) as u32,
    }
}

/// Simpler candidates for a failing value, simplest first.
pub fn shrink(tys: &mut TyCtx, defs: &Defs, t: Ty, v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    match (tys.kind(t).clone(), v) {
        (TyKind::Int(k, _), _) => {
            let Some(x) = int_of(v) else { return out };
            if x != 0 {
                for c in [0, x / 2, x - x.signum()] {
                    let c = c.clamp(k.min(), k.max());
                    if c != x && !out.iter().any(|o| int_of(o) == Some(c)) {
                        out.push(int_value(k, c));
                    }
                }
            }
        }
        (TyKind::Float(_), Value::Float(f)) => {
            if *f != 0.0 {
                out.push(Value::Float(0.0));
                if f.fract() != 0.0 {
                    out.push(Value::Float(f.trunc()));
                }
                out.push(Value::Float(f / 2.0));
            }
        }
        (TyKind::Bool, Value::Bool(true)) => out.push(Value::Bool(false)),
        (TyKind::Char, Value::Char(c)) if *c != 'a' as u32 => out.push(Value::Char('a' as u32)),
        (TyKind::Str, Value::Str(s)) if !s.is_empty() => {
            out.push(Value::Str(Rc::from("")));
            let cs: Vec<char> = s.chars().collect();
            out.push(Value::Str(Rc::from(cs[1..].iter().collect::<String>().as_str())));
            out.push(Value::Str(Rc::from(cs[..cs.len() - 1].iter().collect::<String>().as_str())));
        }
        (TyKind::List(e), Value::List(xs)) => {
            if !xs.is_empty() {
                out.push(Value::List(Rc::new(Vec::new())));
                for i in 0..xs.len().min(8) {
                    let mut ys = (**xs).clone();
                    ys.remove(i);
                    out.push(Value::List(Rc::new(ys)));
                }
                for i in 0..xs.len().min(4) {
                    for c in shrink(tys, defs, e, &xs[i]) {
                        let mut ys = (**xs).clone();
                        ys[i] = c;
                        out.push(Value::List(Rc::new(ys)));
                    }
                }
            }
        }
        (TyKind::Tuple(ts), Value::Tuple(xs)) => {
            for (i, &ft) in ts.iter().enumerate() {
                for c in shrink(tys, defs, ft, &xs[i]) {
                    let mut ys = (**xs).clone();
                    ys[i] = c;
                    out.push(Value::Tuple(Rc::new(ys)));
                }
            }
        }
        (TyKind::Adt(a, args), Value::Tuple(xs)) if !defs.adts[a].is_enum() => {
            let fts: Vec<Ty> = defs.adts[a].fields().iter().map(|f| f.ty).collect();
            for (i, ft) in fts.into_iter().enumerate() {
                let ft = tys.subst(ft, &args);
                for c in shrink(tys, defs, ft, &xs[i]) {
                    let mut ys = (**xs).clone();
                    ys[i] = c;
                    out.push(Value::Tuple(Rc::new(ys)));
                }
            }
        }
        (TyKind::Adt(a, args), Value::Variant(vi, fs)) => {
            let fts: Vec<Ty> = defs.adts[a].variants()[*vi as usize].fields.iter().map(|f| f.ty).collect();
            for (i, ft) in fts.into_iter().enumerate() {
                let ft = tys.subst(ft, &args);
                for c in shrink(tys, defs, ft, &fs[i]) {
                    let mut ys = (**fs).clone();
                    ys[i] = c;
                    out.push(Value::Variant(*vi, Rc::new(ys)));
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_stay_in_range_and_shrink_toward_zero() {
        let mut r = Rng::new(7);
        for _ in 0..1000 {
            let v = int_in(&mut r, IntTy::U8, 1000);
            assert!((0..=255).contains(&v));
        }
        let mut tys = TyCtx::new();
        let defs = Defs::default();
        let int = tys.int;
        let c = shrink(&mut tys, &defs, int, &Value::Int(100));
        assert_eq!(int_of(&c[0]), Some(0));
        assert!(shrink(&mut tys, &defs, int, &Value::Int(0)).is_empty());
    }
}
