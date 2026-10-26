//! Type-directed search over function signatures: `List<T> -> Map<K, List<T>>`.
//!
//! A query is a function signature without names: parameter types separated by commas, then
//! `->` and a result type. Single capital letters are type variables. A method's receiver
//! counts as its first parameter. Candidates are ranked by how well they match.

use crate::sema::defs::*;
use crate::sema::ty::*;
use crate::syntax::ast::{Ast, TypeExprId, TypeExprKind};
use crate::util::{FxHashMap, Symbol};

pub struct Hit {
    pub name: String,
    pub signature: String,
    pub package: String,
    pub score: u32,
}

/// A structural type pattern built from the query text.
#[derive(Clone, Debug, PartialEq)]
enum Pat {
    Var(Symbol),
    Con(String, Vec<Pat>),
    Tuple(Vec<Pat>),
    Fn(Vec<Pat>, Box<Pat>),
}

fn parse_query(q: &str) -> Option<(Vec<Pat>, Option<Pat>)> {
    // Reuse the Jig type parser: wrap the query as a function type.
    let src = if q.contains("->") {
        let (params, ret) = q.split_once("->")?;
        format!("type Q = fn({}) -> {}\n", params.trim(), ret.trim())
    } else {
        format!("type Q = fn({})\n", q.trim())
    };
    let mut sm = crate::diag::SourceMap::new();
    let f = sm.add("<query>".into(), src.clone());
    let base = sm.file(f).base;
    let mut ast = Ast::default();
    let mut li = crate::util::intern::LocalInterner::default();
    let out = crate::syntax::parser::parse_file(&mut ast, f, &src, base, &mut li);
    if !out.diags.is_empty() {
        return None;
    }
    let item = ast.top_items().next()?;
    let crate::syntax::ast::ItemKind::TypeAlias { ty, .. } = ast.items[item].kind else { return None };
    let TypeExprKind::Fn { params, ret, .. } = ast.ty(ty).kind else { return None };
    let ps: Vec<Pat> = ast.type_lists.get(params).iter().map(|&t| to_pat(&ast, t)).collect();
    let r = ret.map(|t| to_pat(&ast, t));
    Some((ps, if q.contains("->") { r } else { None }))
}

fn to_pat(ast: &Ast, t: TypeExprId) -> Pat {
    match ast.ty(t).kind {
        TypeExprKind::Named { name, args, .. } => {
            let s = name.sym.as_str();
            let args: Vec<Pat> = ast.type_lists.get(args).iter().map(|&a| to_pat(ast, a)).collect();
            if s.len() == 1 && s.chars().all(|c| c.is_ascii_uppercase()) && args.is_empty() {
                Pat::Var(name.sym)
            } else {
                Pat::Con(s.to_string(), args)
            }
        }
        TypeExprKind::Tuple(list) => Pat::Tuple(ast.type_lists.get(list).iter().map(|&a| to_pat(ast, a)).collect()),
        TypeExprKind::Fn { params, ret, .. } => {
            let ps = ast.type_lists.get(params).iter().map(|&a| to_pat(ast, a)).collect();
            let r = ret.map(|r| to_pat(ast, r)).unwrap_or(Pat::Tuple(vec![]));
            Pat::Fn(ps, Box::new(r))
        }
        TypeExprKind::Refined { base, .. } => to_pat(ast, base),
        TypeExprKind::Error => Pat::Tuple(vec![]),
    }
}

/// Convert a semantic type to a pattern; generic parameters become variables.
fn ty_pat(tys: &TyCtx, defs: &Defs, t: Ty) -> Pat {
    match tys.kind(t) {
        TyKind::Param(_, n) => Pat::Var(Symbol::intern(&format!("'{n}"))),
        TyKind::Int(k, _) => Pat::Con(k.name().into(), vec![]),
        TyKind::Float(_) => Pat::Con("Float".into(), vec![]),
        TyKind::Bool => Pat::Con("Bool".into(), vec![]),
        TyKind::Char => Pat::Con("Char".into(), vec![]),
        TyKind::Str => Pat::Con("Str".into(), vec![]),
        TyKind::Never => Pat::Con("Never".into(), vec![]),
        TyKind::List(x) => Pat::Con("List".into(), vec![ty_pat(tys, defs, *x)]),
        TyKind::Set(x) => Pat::Con("Set".into(), vec![ty_pat(tys, defs, *x)]),
        TyKind::Map(k, v) => Pat::Con("Map".into(), vec![ty_pat(tys, defs, *k), ty_pat(tys, defs, *v)]),
        TyKind::Adt(a, args) => {
            Pat::Con(defs.adts[*a].name.to_string(), args.iter().map(|&x| ty_pat(tys, defs, x)).collect())
        }
        TyKind::Tuple(ts) => Pat::Tuple(ts.iter().map(|&x| ty_pat(tys, defs, x)).collect()),
        TyKind::Fn(ps, r, _) => {
            Pat::Fn(ps.iter().map(|&x| ty_pat(tys, defs, x)).collect(), Box::new(ty_pat(tys, defs, *r)))
        }
        TyKind::Infer(_) | TyKind::Error => Pat::Tuple(vec![]),
    }
}

/// Unify a query pattern with a candidate pattern. Query variables and candidate variables
/// each bind consistently.
fn unify(q: &Pat, c: &Pat, env: &mut FxHashMap<Symbol, Pat>) -> bool {
    match (q, c) {
        (Pat::Var(v), _) | (_, Pat::Var(v)) => {
            let other = if matches!(q, Pat::Var(x) if x == v) { c } else { q };
            match env.get(v).cloned() {
                Some(b) => b == *other || matches!(other, Pat::Var(_)) || matches!(b, Pat::Var(_)),
                None => {
                    env.insert(*v, other.clone());
                    true
                }
            }
        }
        (Pat::Con(a, xs), Pat::Con(b, ys)) => {
            a == b && xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| unify(x, y, env))
        }
        (Pat::Tuple(xs), Pat::Tuple(ys)) => xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| unify(x, y, env)),
        (Pat::Fn(xs, r1), Pat::Fn(ys, r2)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| unify(x, y, env)) && unify(r1, r2, env)
        }
        _ => false,
    }
}

fn show_sig(prog: &Program, f: &FnDef) -> String {
    let tys = &prog.tys;
    let mut parts = Vec::new();
    if let (Some(m), Some(st)) = (f.self_mode, f.self_ty) {
        parts.push(format!("{}self: {}", if m == Mode::MutRef { "&mut " } else { "" }, tys.display(st)));
    }
    for p in &f.params {
        parts.push(format!("{}: {}{}", p.name, if p.mode == Mode::MutRef { "&mut " } else { "" }, tys.display(p.ty)));
    }
    let mut s = format!("({})", parts.join(", "));
    if !tys.is_unit(f.ret) {
        s.push_str(&format!(" -> {}", tys.display(f.ret)));
    }
    if !f.effects.is_pure() {
        s.push_str(&format!(" effects {}", f.effects.render()));
    }
    s
}

pub fn search(prog: &Program, query: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let (qparams, qret) = parse_query(query)
        .ok_or_else(|| format!("cannot parse the query `{query}`; write e.g. `List<T>, Int -> Option<T>`"))?;
    let mut hits = Vec::new();
    for (_, f) in prog.defs.fns.iter_enumerated() {
        if f.iface.is_some() || (!f.is_pub && prog.pkgs[f.pkg].is_std) {
            continue;
        }
        let mut cparams: Vec<Pat> = Vec::new();
        if let (Some(_), Some(st)) = (f.self_mode, f.self_ty) {
            cparams.push(ty_pat(&prog.tys, &prog.defs, st));
        }
        cparams.extend(f.params.iter().map(|p| ty_pat(&prog.tys, &prog.defs, p.ty)));
        let cret = ty_pat(&prog.tys, &prog.defs, f.ret);
        let mut score = 0u32;
        let mut env = FxHashMap::default();
        let ret_ok = match &qret {
            Some(r) => unify(r, &cret, &mut env),
            None => true,
        };
        if !ret_ok {
            continue;
        }
        if qret.is_some() {
            score += 40;
        }
        if cparams.len() == qparams.len() && qparams.iter().zip(&cparams).all(|(q, c)| unify(q, c, &mut env.clone())) {
            // In order.
            let mut env2 = env.clone();
            if qparams.iter().zip(&cparams).all(|(q, c)| unify(q, c, &mut env2)) {
                score += 60;
            } else {
                score += 45;
            }
        } else if cparams.len() == qparams.len() {
            // Any order.
            let mut used = vec![false; cparams.len()];
            let mut all = true;
            for q in &qparams {
                match (0..cparams.len()).find(|&i| !used[i] && unify(q, &cparams[i], &mut env.clone())) {
                    Some(i) => used[i] = true,
                    None => {
                        all = false;
                        break;
                    }
                }
            }
            if !all {
                continue;
            }
            score += 30;
        } else {
            let matched = qparams.iter().filter(|q| cparams.iter().any(|c| unify(q, c, &mut env.clone()))).count();
            if matched < qparams.len() || qparams.is_empty() && qret.is_none() {
                continue;
            }
            score += 10;
        }
        let pkg = &prog.pkgs[f.pkg];
        hits.push(Hit {
            name: f.display_name(&prog.defs, &prog.tys),
            signature: show_sig(prog, f),
            package: if pkg.path.as_str() == "<prelude>" { "prelude".into() } else { pkg.path.to_string() },
            score,
        });
    }
    hits.sort_by(|a, b| b.score.cmp(&a.score).then(a.name.cmp(&b.name)));
    hits.truncate(limit);
    Ok(hits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sema::load::Sources;

    fn prog() -> Program {
        let mut m = crate::util::FxHashMap::default();
        m.insert(std::path::PathBuf::from("/s/main.jig"), "pub fn group<T>(xs: List<T>) -> Map<Int, List<T>> {\n  var m: Map<Int, List<T>> = Map.new()\n  m.insert(0, xs)\n  return m\n}\n".to_string());
        crate::sema::analyze(&Sources::Memory(m), std::path::Path::new("/s"))
    }

    #[test]
    fn finds_by_signature() {
        let p = prog();
        assert!(p.diags.list.is_empty(), "{:?}", p.diags.list.iter().map(|d| &d.message).collect::<Vec<_>>());
        let hits = search(&p, "List<T> -> Map<K, List<T>>", 5).unwrap();
        assert_eq!(hits[0].name, "group", "{:?}", hits.iter().map(|h| &h.name).collect::<Vec<_>>());
        let hits = search(&p, "Str -> Option<Int>", 5).unwrap();
        assert!(hits.iter().any(|h| h.name == "Str.to_int"), "{:?}", hits.iter().map(|h| &h.name).collect::<Vec<_>>());
        let hits = search(&p, "List<T>, Int -> Option<T>", 5).unwrap();
        assert_eq!(hits[0].name, "List.get");
    }
}
