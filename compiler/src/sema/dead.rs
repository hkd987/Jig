//! Dead code as an error: whole-program reachability from the entrypoints.
//!
//! Roots are `main`, or the `pub` API of the root package when it is a library. Tests do not
//! justify code. Without reflection, macros or dynamic loading the call graph is complete.

use super::defs::*;
use super::ty::*;
use crate::diag::{codes, Diagnostic};
use crate::util::{FxHashSet, Symbol};

pub fn check_dead_code(prog: &mut Program) {
    // Follow-on noise is worse than useless while other errors exist.
    if prog.diags.has_errors() {
        return;
    }
    let Program { defs, bodies, test_bodies, pkgs, tables, tys, diags, root, sm, scenarios, .. } = prog;
    let is_user = |pkg: PkgId| !pkgs[pkg].is_std;
    let library = defs.main.is_none();

    let mut roots: Vec<FnId> = Vec::new();
    // Scenarios justify the steps they use and the worlds they build.
    let mut used_steps: FxHashSet<FnId> = FxHashSet::default();
    for sc in scenarios.iter() {
        for st in &sc.steps {
            used_steps.insert(st.f);
            roots.push(st.f);
        }
        if let Some((_, new)) = sc.world {
            roots.push(new);
        }
    }
    match defs.main {
        Some(m) => roots.push(m),
        None => {
            for (fid, fd) in defs.fns.iter_enumerated() {
                if fd.pkg == *root && fd.is_pub && fd.iface.is_none() {
                    roots.push(fid);
                }
            }
        }
    }

    // Reachability over functions, including methods reached through generic bounds.
    let mut reachable: FxHashSet<FnId> = FxHashSet::default();
    let mut dyn_names: FxHashSet<Symbol> = FxHashSet::default();
    let mut stack = roots.clone();
    let mut cross_pkg: FxHashSet<FnId> = FxHashSet::default();
    let mut fields_read: FxHashSet<(AdtId, u32)> = FxHashSet::default();
    let mut variants_built: FxHashSet<(AdtId, u32)> = FxHashSet::default();
    let mut consts_used: FxHashSet<ConstId> = FxHashSet::default();
    let mut consts_cross: FxHashSet<ConstId> = FxHashSet::default();
    let mut derived: Vec<Ty> = Vec::new();
    loop {
        while let Some(f) = stack.pop() {
            if !reachable.insert(f) {
                continue;
            }
            let Some(body) = bodies[f].as_ref() else { continue };
            let fpkg = defs.fns[f].pkg;
            for &r in &body.refs {
                if defs.fns[r].pkg != fpkg {
                    cross_pkg.insert(r);
                }
                stack.push(r);
            }
            for &n in &body.dyn_methods {
                dyn_names.insert(n);
            }
            fields_read.extend(body.fields_read.iter().copied());
            variants_built.extend(body.variants_built.iter().copied());
            consts_used.extend(body.consts_used.iter().copied());
            consts_cross.extend(body.consts_used.iter().copied().filter(|&c| defs.consts[c].pkg != fpkg));
            for &e in &body.derived_exprs {
                derived.push(tables[fpkg].expr_ty[e.0 as usize]);
            }
        }
        // Methods named by bound-dispatched calls are reachable on every type.
        let mut more = false;
        for (&(_, name), &f) in defs.methods.iter() {
            if dyn_names.contains(&name) && !reachable.contains(&f) {
                stack.push(f);
                more = true;
            }
        }
        if !more {
            break;
        }
    }

    // Derived equality, ordering, hashing and display read every field, transitively.
    let mut seen_tys: FxHashSet<Ty> = FxHashSet::default();
    while let Some(t) = derived.pop() {
        if !seen_tys.insert(t) {
            continue;
        }
        match tys.kind(t).clone() {
            TyKind::Adt(a, args) => match &defs.adts[a].kind {
                AdtKind::Struct { fields } => {
                    for (i, f) in fields.iter().enumerate() {
                        fields_read.insert((a, i as u32));
                        let ft = tys.subst(f.ty, &args);
                        derived.push(ft);
                    }
                }
                AdtKind::Enum { variants } => {
                    for v in variants {
                        for f in &v.fields {
                            let ft = tys.subst(f.ty, &args);
                            derived.push(ft);
                        }
                    }
                }
            },
            TyKind::Tuple(ts) => derived.extend(ts.iter().copied()),
            TyKind::List(x) | TyKind::Set(x) => derived.push(x),
            TyKind::Map(k, v) => {
                derived.push(k);
                derived.push(v);
            }
            _ => {}
        }
    }

    // The test simulation is not part of the program: nothing `main` reaches may use it.
    {
        let entry: Vec<FnId> = match defs.main {
            Some(m) => vec![m],
            None => roots.iter().copied().filter(|f| !used_steps.contains(f)).collect(),
        };
        let mut seen: FxHashSet<FnId> = FxHashSet::default();
        let mut stack = entry;
        while let Some(f) = stack.pop() {
            if !seen.insert(f) {
                continue;
            }
            let Some(body) = bodies[f].as_ref() else { continue };
            for &r in &body.refs {
                if pkgs[defs.fns[r].pkg].path.as_str() == "std/sim" && is_user(defs.fns[f].pkg) {
                    let fd = &defs.fns[f];
                    diags.push(
                        Diagnostic::error(
                            codes::E0607,
                            fd.name_span,
                            format!("`{}` uses `sim.{}`, but `main` reaches it", fd.name, defs.fns[r].name),
                        )
                        .note("`std/sim` is for tests, steps and test helpers"),
                    );
                }
                stack.push(r);
            }
        }
    }

    let test_refs: FxHashSet<FnId> = test_bodies.iter().flatten().flat_map(|b| b.refs.iter().copied()).collect();
    // Test helpers: functions in `*_test.jig` files, reached from tests.
    let in_test_file = |f: FnId| {
        let fd = &defs.fns[f];
        let pkg = &pkgs[fd.pkg];
        let item = pkg.ast.items[fd.item];
        let file = pkg.ast.files[item.file as usize].file;
        sm.file(file).path.to_string_lossy().ends_with("_test.jig")
    };
    let mut test_reachable: FxHashSet<FnId> = FxHashSet::default();
    let mut work: Vec<FnId> = test_refs.iter().copied().collect();
    while let Some(f) = work.pop() {
        if !test_reachable.insert(f) {
            continue;
        }
        if let Some(b) = bodies[f].as_ref() {
            work.extend(b.refs.iter().copied());
        }
    }

    // Unreachable functions.
    for (fid, fd) in defs.fns.iter_enumerated() {
        if !is_user(fd.pkg) || fd.iface.is_some() || (!fd.has_body && !fd.ffi) || reachable.contains(&fid) {
            continue;
        }
        if let Some((pattern, pspan)) = fd.step {
            if !used_steps.contains(&fid) {
                diags.push(
                    Diagnostic::error(codes::E0605, pspan, format!("no scenario uses the step \"{pattern}\""))
                        .note("delete it, or write a scenario that needs it"),
                );
            }
            continue;
        }
        if test_reachable.contains(&fid) && in_test_file(fid) {
            continue; // a test helper
        }
        let name = fd.display_name(defs, tys);
        let mut d =
            Diagnostic::error(codes::E0601, fd.name_span, format!("`{name}` is never reached from an entrypoint"));
        if test_refs.contains(&fid) {
            d = d.note(
                "only tests use it; tests do not justify code. Move it to a `_test.jig` file to make it a test helper",
            );
        } else if library {
            d = d.note("in a library, only `pub` functions of the root package and what they call are entrypoints");
        } else {
            d = d.note("delete it, or call it from code reachable from `main`");
        }
        diags.push(d);
    }

    // Unused exports: `pub` functions of non-root packages used only inside their package.
    for (fid, fd) in defs.fns.iter_enumerated() {
        if !is_user(fd.pkg) || fd.pkg == *root || !fd.is_pub || fd.iface.is_some() || !reachable.contains(&fid) {
            continue;
        }
        if cross_pkg.contains(&fid) || dyn_names.contains(&fd.name) {
            continue;
        }
        let name = fd.display_name(defs, tys);
        diags.push(
            Diagnostic::error(codes::E0603, fd.name_span, format!("`{name}` is exported but no other package uses it"))
                .note("remove `pub`"),
        );
    }

    // Unused exports of types, interfaces and constants: `pub` items of non-root packages
    // that no other package names or handles.
    let (adts_cross, ifaces_cross) = cross_package_types(defs, pkgs, tables, tys);
    let exported = |pkg: PkgId, is_pub: bool| is_user(pkg) && pkg != *root && is_pub;
    for (aid, ad) in defs.adts.iter_enumerated() {
        if exported(ad.pkg, ad.is_pub) && !adts_cross.contains(&aid) {
            let span = pkgs[ad.pkg].ast.items[ad.item].name.span;
            let what = if ad.is_enum() { "enum" } else { "struct" };
            diags.push(
                Diagnostic::error(
                    codes::E0603,
                    span,
                    format!("{what} `{}` is exported but no other package uses it", ad.name),
                )
                .note("remove `pub`"),
            );
        }
    }
    for (iid, id) in defs.ifaces.iter_enumerated() {
        if exported(id.pkg, id.is_pub) && !ifaces_cross.contains(&iid) {
            let span = pkgs[id.pkg].ast.items[id.item].name.span;
            diags.push(
                Diagnostic::error(
                    codes::E0603,
                    span,
                    format!("interface `{}` is exported but no other package uses it", id.name),
                )
                .note("remove `pub`"),
            );
        }
    }
    for (cid, cd) in defs.consts.iter_enumerated() {
        if exported(cd.pkg, cd.is_pub) && consts_used.contains(&cid) && !consts_cross.contains(&cid) {
            let span = pkgs[cd.pkg].ast.items[cd.item].name.span;
            diags.push(
                Diagnostic::error(
                    codes::E0603,
                    span,
                    format!("constant `{}` is exported but no other package uses it", cd.name),
                )
                .note("remove `pub`"),
            );
        }
    }

    // Fields and variants.
    for (aid, ad) in defs.adts.iter_enumerated() {
        if !is_user(ad.pkg) {
            continue;
        }
        let api = library && ad.pkg == *root && ad.is_pub;
        match &ad.kind {
            AdtKind::Struct { fields } => {
                for (i, f) in fields.iter().enumerate() {
                    if fields_read.contains(&(aid, i as u32)) || (api && f.is_pub) {
                        continue;
                    }
                    if struct_invariant_mentions(pkgs, ad, f.name) {
                        continue;
                    }
                    diags.push(
                        Diagnostic::error(
                            codes::E0602,
                            f.span,
                            format!("field `{}` of `{}` is never read", f.name, ad.name),
                        )
                        .note("remove it"),
                    );
                }
            }
            AdtKind::Enum { variants } => {
                for (i, v) in variants.iter().enumerate() {
                    if variants_built.contains(&(aid, i as u32)) || api {
                        continue;
                    }
                    diags.push(
                        Diagnostic::error(
                            codes::E0604,
                            v.span,
                            format!("variant `{}` of `{}` is never constructed", v.name, ad.name),
                        )
                        .note("remove it; matches on it can never succeed"),
                    );
                }
            }
        }
    }

    // Constants.
    for (cid, cd) in defs.consts.iter_enumerated() {
        if !is_user(cd.pkg) || consts_used.contains(&cid) || (library && cd.pkg == *root && cd.is_pub) {
            continue;
        }
        let span = pkgs[cd.pkg].ast.items[cd.item].name.span;
        diags.push(
            Diagnostic::error(codes::E0606, span, format!("constant `{}` is never used", cd.name)).note("remove it"),
        );
    }
    diags.sort();
}

/// ADTs and interfaces that some package other than their own mentions: in any expression or
/// pattern type, a signature, a field, an alias or a generic bound.
fn cross_package_types(
    defs: &Defs,
    pkgs: &crate::util::IndexVec<PkgId, Package>,
    tables: &crate::util::IndexVec<PkgId, Tables>,
    tys: &TyCtx,
) -> (FxHashSet<AdtId>, FxHashSet<IfaceId>) {
    let mut adts = FxHashSet::default();
    let mut ifaces = FxHashSet::default();
    for (pid, pkg) in pkgs.iter_enumerated() {
        if pkg.is_std {
            continue;
        }
        let mut seen: FxHashSet<Ty> = FxHashSet::default();
        let mut work: Vec<Ty> = Vec::new();
        work.extend(tables[pid].expr_ty.iter().copied());
        work.extend(tables[pid].pat_ty.iter().copied());
        for fd in defs.fns.iter().filter(|f| f.pkg == pid) {
            work.extend(fd.params.iter().map(|p| p.ty));
            work.push(fd.ret);
            for g in &fd.generics {
                for b in &g.bounds {
                    if let Bound::Iface(i) = b {
                        if defs.ifaces[*i].pkg != pid {
                            ifaces.insert(*i);
                        }
                    }
                }
            }
        }
        for ad in defs.adts.iter().filter(|a| a.pkg == pid) {
            work.extend(ad.fields().iter().map(|f| f.ty));
            work.extend(ad.variants().iter().flat_map(|v| v.fields.iter().map(|f| f.ty)));
        }
        work.extend(defs.aliases.iter().filter(|a| a.pkg == pid).map(|a| a.ty));
        while let Some(t) = work.pop() {
            if !seen.insert(t) {
                continue;
            }
            match tys.kind(t) {
                TyKind::Adt(a, args) => {
                    if defs.adts[*a].pkg != pid {
                        adts.insert(*a);
                    }
                    work.extend(args.iter().copied());
                }
                TyKind::Tuple(ts) => work.extend(ts.iter().copied()),
                TyKind::List(x) | TyKind::Set(x) => work.push(*x),
                TyKind::Map(k, v) => {
                    work.push(*k);
                    work.push(*v);
                }
                TyKind::Fn(ps, r, _) => {
                    work.extend(ps.iter().copied());
                    work.push(*r);
                }
                _ => {}
            }
        }
    }
    (adts, ifaces)
}

fn struct_invariant_mentions(pkgs: &crate::util::IndexVec<PkgId, Package>, ad: &AdtDef, field: Symbol) -> bool {
    use crate::syntax::ast::{ExprKind, ItemKind};
    let ast = &pkgs[ad.pkg].ast;
    let ItemKind::Struct { invariants, fields, .. } = ast.items[ad.item].kind else { return false };
    let mut exprs: Vec<crate::syntax::ast::ExprId> = ast.expr_lists.get(invariants).to_vec();
    for f in ast.fields.get(fields) {
        if let Some(r) = f.refine {
            exprs.push(r);
        }
    }
    // Any name in an invariant or refinement that matches the field counts as a read.
    let mut stack = exprs;
    let mut steps = 0;
    while let Some(e) = stack.pop() {
        steps += 1;
        if steps > 10_000 {
            return true;
        }
        let k = ast.exprs[e].kind;
        match k {
            ExprKind::Name(n) if n == field => return true,
            ExprKind::Field { base, .. } | ExprKind::TupleField { base, .. } => stack.push(base),
            ExprKind::Paren(x)
            | ExprKind::Try(x)
            | ExprKind::Unary { expr: x, .. }
            | ExprKind::Cast { expr: x, .. } => stack.push(x),
            ExprKind::Binary { lhs, rhs, .. } => {
                stack.push(lhs);
                stack.push(rhs);
            }
            ExprKind::Call { callee, args } => {
                stack.push(callee);
                stack.extend(ast.expr_lists.get(args).iter().copied());
            }
            ExprKind::Index { base, index } => {
                stack.push(base);
                stack.push(index);
            }
            ExprKind::Quant { lo, hi, body, .. } => {
                stack.push(lo);
                stack.push(hi);
                stack.push(body);
            }
            ExprKind::Tuple(l) | ExprKind::List(l) => stack.extend(ast.expr_lists.get(l).iter().copied()),
            _ => {}
        }
    }
    false
}
