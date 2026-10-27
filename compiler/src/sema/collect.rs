//! Item collection: declare every item, then resolve signatures, fields, aliases, methods and
//! constants. Bodies are checked later (`check.rs`).

use super::defs::*;
use super::ty::*;
use crate::diag::{codes, Diagnostic, Diags, Span};
use crate::syntax::ast::{self, ExprKind, ItemId, ItemKind, TypeExprId, TypeExprKind, UnOp};
use crate::syntax::parser::is_numeric_type_name;
use crate::util::{sym, FxHashMap, FxHashSet, IndexVec, Symbol};

pub fn builtin_type(s: Symbol) -> Option<BuiltinTy> {
    Some(match s.as_str() {
        "Int" => BuiltinTy::Int(IntTy::I64),
        "I32" => BuiltinTy::Int(IntTy::I32),
        "U8" => BuiltinTy::Int(IntTy::U8),
        "U32" => BuiltinTy::Int(IntTy::U32),
        "U64" => BuiltinTy::Int(IntTy::U64),
        "Float" => BuiltinTy::Float,
        "Bool" => BuiltinTy::Bool,
        "Char" => BuiltinTy::Char,
        "Str" => BuiltinTy::Str,
        "List" => BuiltinTy::List,
        "Map" => BuiltinTy::Map,
        "Set" => BuiltinTy::Set,
        "Never" => BuiltinTy::Never,
        _ => return None,
    })
}

pub fn builtin_bound(s: Symbol) -> Option<Bound> {
    Some(match s.as_str() {
        "Eq" => Bound::Eq,
        "Ord" => Bound::Ord,
        "Hash" => Bound::Hash,
        "Show" => Bound::Show,
        _ => return None,
    })
}

/// Context for resolving a type expression.
#[derive(Clone)]
pub struct TyScope {
    pub pkg: PkgId,
    pub file: usize,
    /// Generic parameter names in scope, by index.
    pub generics: Vec<Symbol>,
    /// Inside an interface: `Self` is `Param(0)`.
    pub has_self: bool,
}

pub struct Collector<'a> {
    pub pkgs: &'a IndexVec<PkgId, Package>,
    pub defs: &'a mut Defs,
    pub tys: &'a mut TyCtx,
    pub diags: &'a mut Diags,
    pub prelude: PkgId,
    pub units: FxHashSet<Symbol>,
    alias_state: Vec<u8>,
}

pub fn collect(prog: &mut Program) {
    let Program { pkgs, defs, tys, diags, prelude, root, .. } = prog;
    // Declarations mutate package scopes; do them first with mutable access.
    declare_all(pkgs, defs, tys, diags, *prelude);
    let mut c =
        Collector { pkgs, defs, tys, diags, prelude: *prelude, units: FxHashSet::default(), alias_state: Vec::new() };
    c.collect_units();
    c.resolve_aliases();
    c.resolve_adts();
    c.resolve_ifaces();
    c.resolve_fns();
    c.resolve_consts();
    c.resolve_tests();
    c.check_recursive_adts();
    c.find_main(*root);
    prog.bodies = IndexVec::from_raw((0..prog.defs.fns.len()).map(|_| None).collect());
    prog.test_bodies = IndexVec::from_raw((0..prog.defs.tests.len()).map(|_| None).collect());
}

fn generic_defs(ast: &ast::Ast, list: crate::util::List<ast::GenericParam>) -> Vec<GenericDef> {
    ast.generics
        .get(list)
        .iter()
        .map(|g| GenericDef { name: g.name.sym, bounds: Vec::new(), span: g.name.span })
        .collect()
}

fn declare_all(
    pkgs: &mut IndexVec<PkgId, Package>,
    defs: &mut Defs,
    tys: &mut TyCtx,
    diags: &mut Diags,
    prelude: PkgId,
) {
    let prelude_names: FxHashSet<Symbol> = FxHashSet::default();
    let mut prelude_names = prelude_names;
    for pid in pkgs.indices().collect::<Vec<_>>() {
        let is_prelude = pid == prelude;
        let pkg_path = pkgs[pid].path;
        let mut scope: FxHashMap<Symbol, Def> = FxHashMap::default();
        let mut spans: FxHashMap<Symbol, Span> = FxHashMap::default();
        let items: Vec<ItemId> = pkgs[pid].ast.top_items().collect();
        let mut declare = |name: ast::Name, def: Def, diags: &mut Diags, scope: &mut FxHashMap<Symbol, Def>| {
            if builtin_type(name.sym).is_some() {
                diags.push(Diagnostic::error(
                    codes::E0202,
                    name.span,
                    format!("`{}` is a built-in type name and cannot be redefined", name.sym),
                ));
                return;
            }
            if !is_prelude && prelude_names.contains(&name.sym) {
                diags.push(Diagnostic::error(
                    codes::E0202,
                    name.span,
                    format!("`{}` is defined by the prelude and cannot be redefined", name.sym),
                ));
                return;
            }
            if let Some(prev) = spans.get(&name.sym) {
                diags.push(
                    Diagnostic::error(codes::E0202, name.span, format!("`{}` is defined more than once", name.sym))
                        .label(*prev, "first defined here"),
                );
                return;
            }
            spans.insert(name.sym, name.span);
            scope.insert(name.sym, def);
        };
        for &item_id in &items {
            let item = pkgs[pid].ast.items[item_id];
            let ast = &pkgs[pid].ast;
            match item.kind {
                ItemKind::Import { .. } => {}
                ItemKind::Fn(f) => {
                    let fid = defs.fns.push(new_fn(pid, item_id, &item, &f, ast, None));
                    defs.fn_of_item.insert((pid, item_id), fid);
                    if f.recv.is_none() {
                        declare(item.name, Def::Fn(fid), diags, &mut scope);
                    }
                }
                ItemKind::Struct { generics, .. } | ItemKind::Enum { generics, .. } => {
                    let is_enum = matches!(item.kind, ItemKind::Enum { .. });
                    for g in ast.generics.get(generics) {
                        if !g.bounds.is_empty() {
                            diags.push(
                                Diagnostic::error(
                                    codes::E0312,
                                    g.name.span,
                                    "type parameters of structs and enums cannot have bounds",
                                )
                                .note("put the bound on the functions and methods that need it"),
                            );
                        }
                    }
                    let aid = defs.adts.push(AdtDef {
                        pkg: pid,
                        item: item_id,
                        name: item.name.sym,
                        generics: generic_defs(ast, generics),
                        kind: if is_enum {
                            AdtKind::Enum { variants: Vec::new() }
                        } else {
                            AdtKind::Struct { fields: Vec::new() }
                        },
                        is_pub: item.is_pub,
                        recursive: false,
                    });
                    tys.adt_names.push((sym::EMPTY, item.name.sym));
                    debug_assert_eq!(tys.adt_names.len(), defs.adts.len());
                    if is_prelude && item.name.sym == sym::OPTION {
                        defs.option = Some(aid);
                    }
                    if is_prelude && item.name.sym == sym::RESULT_TY {
                        defs.result = Some(aid);
                    }
                    declare(item.name, Def::Adt(aid), diags, &mut scope);
                }
                ItemKind::Interface { methods, generics } => {
                    if !generics.is_empty() {
                        diags.push(Diagnostic::error(
                            codes::E0904,
                            item.name.span,
                            "generic interfaces are not supported yet",
                        ));
                    }
                    let iid = defs.ifaces.push(IfaceDef {
                        pkg: pid,
                        item: item_id,
                        name: item.name.sym,
                        methods: Vec::new(),
                        is_pub: item.is_pub,
                    });
                    let mut ms = Vec::new();
                    for &m in ast.item_lists.get(methods) {
                        let mi = ast.items[m];
                        if let ItemKind::Fn(f) = mi.kind {
                            let fid = defs.fns.push(new_fn(pid, m, &mi, &f, ast, Some(iid)));
                            defs.fn_of_item.insert((pid, m), fid);
                            ms.push(fid);
                        }
                    }
                    defs.ifaces[iid].methods = ms;
                    declare(item.name, Def::Iface(iid), diags, &mut scope);
                }
                ItemKind::TypeAlias { generics, .. } => {
                    let alid = defs.aliases.push(AliasDef {
                        pkg: pid,
                        item: item_id,
                        name: item.name.sym,
                        generics: generic_defs(ast, generics),
                        ty: tys.error,
                        refine: None,
                        is_pub: item.is_pub,
                    });
                    declare(item.name, Def::Alias(alid), diags, &mut scope);
                }
                ItemKind::Const { .. } => {
                    let cid = defs.consts.push(ConstDef {
                        pkg: pid,
                        item: item_id,
                        name: item.name.sym,
                        ty: tys.error,
                        value: None,
                        is_pub: item.is_pub,
                    });
                    declare(item.name, Def::Const(cid), diags, &mut scope);
                }
                ItemKind::Unit => declare(item.name, Def::Unit, diags, &mut scope),
                ItemKind::Test { label, .. } => {
                    defs.tests.push(TestDef { pkg: pid, item: item_id, label, params: Vec::new() });
                }
            }
        }
        // Imports conflict with package-level names.
        for file_imports in &pkgs[pid].imports {
            for (name, &(_, imp_item)) in file_imports {
                if let Some(span) = spans.get(name) {
                    let imp_span = pkgs[pid].ast.items[imp_item].span;
                    diags.push(
                        Diagnostic::error(
                            codes::E0209,
                            imp_span,
                            format!("import `{name}` conflicts with a declaration of the same name"),
                        )
                        .label(*span, "declared here"),
                    );
                }
            }
        }
        if is_prelude {
            prelude_names.extend(scope.keys().copied());
            prelude_names.extend([sym::SOME, sym::NONE, sym::OK, sym::ERR]);
        }
        let _ = pkg_path;
        pkgs[pid].scope = scope;
    }
}

fn new_fn(
    pid: PkgId,
    item_id: ItemId,
    item: &ast::Item,
    f: &ast::FnDecl,
    ast: &ast::Ast,
    iface: Option<IfaceId>,
) -> FnDef {
    let mut generics = Vec::new();
    let mut n_recv = 0;
    if iface.is_some() {
        generics.push(GenericDef { name: Symbol::intern("Self"), bounds: Vec::new(), span: item.name.span });
        n_recv = 1;
    }
    if let Some(r) = f.recv {
        generics.extend(generic_defs(ast, r.generics));
        n_recv = generics.len();
    }
    generics.extend(generic_defs(ast, f.generics));
    FnDef {
        pkg: pid,
        item: item_id,
        name: item.name.sym,
        recv: None,
        self_mode: f.self_param.map(|s| if s.mutable { Mode::MutRef } else { Mode::Value }),
        self_ty: None,
        generics,
        n_recv_generics: n_recv,
        params: Vec::new(),
        ret: Ty(0),
        ret_refine: None,
        effects: EffectSet::PURE,
        is_pub: item.is_pub || iface.is_some(),
        has_body: f.body.is_some(),
        ffi: false,
        iface,
        step: f.step,
        span: item.span,
        name_span: item.name.span,
    }
}

impl Collector<'_> {
    fn ast(&self, pid: PkgId) -> &ast::Ast {
        &self.pkgs[pid].ast
    }

    fn err(&mut self, code: crate::diag::Code, span: Span, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(code, span, msg));
    }

    fn collect_units(&mut self) {
        for p in self.pkgs.iter() {
            for (name, def) in &p.scope {
                if *def == Def::Unit {
                    self.units.insert(*name);
                }
            }
        }
    }

    /// Look up a name in a package's scope, honoring visibility from `from`.
    pub fn lookup_in(&mut self, target: PkgId, name: ast::Name, from: PkgId) -> Option<Def> {
        let def = self.pkgs[target].scope.get(&name.sym).copied()?;
        if target != from && !self.def_is_pub(def) {
            self.err(
                codes::E0205,
                name.span,
                format!("`{}` is private to package `{}`", name.sym, self.pkgs[target].path),
            );
        }
        Some(def)
    }

    fn def_is_pub(&self, def: Def) -> bool {
        match def {
            Def::Fn(f) => self.defs.fns[f].is_pub,
            Def::Adt(a) => self.defs.adts[a].is_pub,
            Def::Iface(i) => self.defs.ifaces[i].is_pub,
            Def::Const(c) => self.defs.consts[c].is_pub,
            Def::Alias(a) => self.defs.aliases[a].is_pub,
            Def::Unit | Def::Builtin(_) => true,
        }
    }

    /// Resolve an unqualified name at package level: package scope, then prelude.
    pub fn lookup_name(&mut self, sc: &TyScope, name: ast::Name) -> Option<Def> {
        if let Some(d) = self.pkgs[sc.pkg].scope.get(&name.sym).copied() {
            return Some(d);
        }
        if sc.pkg != self.prelude {
            if let Some(d) = self.pkgs[self.prelude].scope.get(&name.sym).copied() {
                return Some(d);
            }
        }
        None
    }

    /// The refinement alias a type expression names directly (`amt: Cents`), if any.
    fn refinement_alias(&mut self, sc: &TyScope, te: TypeExprId) -> Option<AliasId> {
        let t = *self.ast(sc.pkg).ty(te);
        let TypeExprKind::Named { pkg, name, args, .. } = t.kind else { return None };
        if !self.ast(sc.pkg).type_lists.get(args).is_empty() {
            return None;
        }
        let def = match pkg {
            Some(p) => {
                let &(target, _) = self.pkgs[sc.pkg].imports.get(sc.file)?.get(&p.sym)?;
                self.pkgs[target].scope.get(&name.sym).copied()?
            }
            None => {
                if sc.generics.contains(&name.sym) || builtin_type(name.sym).is_some() {
                    return None;
                }
                self.lookup_name(sc, name)?
            }
        };
        let Def::Alias(al) = def else { return None };
        self.ensure_alias(al);
        self.defs.aliases[al].refine.is_some().then_some(al)
    }

    pub fn resolve_type(&mut self, sc: &TyScope, te: TypeExprId) -> Ty {
        let t = *self.ast(sc.pkg).ty(te);
        match t.kind {
            TypeExprKind::Error => self.tys.error,
            TypeExprKind::Tuple(list) => {
                let elems: Vec<TypeExprId> = self.ast(sc.pkg).type_lists.get(list).to_vec();
                let ts: Vec<Ty> = elems.iter().map(|&e| self.resolve_type(sc, e)).collect();
                self.tys.tuple(ts)
            }
            TypeExprKind::Fn { params, ret, effects } => {
                let elems: Vec<TypeExprId> = self.ast(sc.pkg).type_lists.get(params).to_vec();
                let ps: Vec<Ty> = elems.iter().map(|&e| self.resolve_type(sc, e)).collect();
                let r = match ret {
                    Some(r) => self.resolve_type(sc, r),
                    None => self.tys.unit,
                };
                let eff = match effects {
                    Some(list) => self.resolve_effects(sc.pkg, list),
                    None => EffectSet::PURE,
                };
                self.tys.func(ps, r, eff)
            }
            TypeExprKind::Refined { base, .. } => self.resolve_type(sc, base),
            TypeExprKind::Named { pkg, name, args, unit } => {
                let arg_ids: Vec<TypeExprId> = self.ast(sc.pkg).type_lists.get(args).to_vec();
                if let Some(p) = pkg {
                    let Some(&(target, _)) = self.pkgs[sc.pkg].imports.get(sc.file).and_then(|m| m.get(&p.sym)) else {
                        self.err(codes::E0201, p.span, format!("cannot find package `{}`", p.sym));
                        return self.tys.error;
                    };
                    self.defs.sig_imports_used.insert((sc.pkg, sc.file, p.sym));
                    let Some(def) = self.lookup_in(target, name, sc.pkg) else {
                        self.err(
                            codes::E0201,
                            name.span,
                            format!("package `{}` has no type `{}`", self.pkgs[target].path, name.sym),
                        );
                        return self.tys.error;
                    };
                    let args: Vec<Ty> = arg_ids.iter().map(|&a| self.resolve_type(sc, a)).collect();
                    return self.type_from_def(def, name, args, t.span);
                }
                if let Some(i) = sc.generics.iter().rposition(|&g| g == name.sym) {
                    if !arg_ids.is_empty() {
                        self.err(codes::E0313, t.span, "type parameters do not take type arguments");
                    }
                    return self.tys.param(i as u32, name.sym);
                }
                if sc.has_self && name.sym.as_str() == "Self" {
                    return self.tys.param(0, name.sym);
                }
                if let Some(b) = builtin_type(name.sym) {
                    let args: Vec<Ty> = arg_ids.iter().map(|&a| self.resolve_type(sc, a)).collect();
                    let unit_id = match unit {
                        Some(factors) => {
                            let fs: Vec<ast::UnitFactor> = self.ast(sc.pkg).unit_factors.get(factors).to_vec();
                            let mut out = Vec::new();
                            for f in fs {
                                if !self.units.contains(&f.name.sym) {
                                    self.err(
                                        codes::E0201,
                                        f.name.span,
                                        format!(
                                            "cannot find unit `{}`; declare it with `unit {}`",
                                            f.name.sym, f.name.sym
                                        ),
                                    );
                                }
                                out.push((f.name.sym, f.exp));
                            }
                            self.tys.unit_id(&out)
                        }
                        None => NO_UNIT,
                    };
                    return self.builtin_ty(b, args, unit_id, t.span, name.sym);
                }
                match self.lookup_name(sc, name) {
                    Some(def) => {
                        let args: Vec<Ty> = arg_ids.iter().map(|&a| self.resolve_type(sc, a)).collect();
                        self.type_from_def(def, name, args, t.span)
                    }
                    None => {
                        self.err(codes::E0201, name.span, format!("cannot find type `{}`", name.sym));
                        self.tys.error
                    }
                }
            }
        }
    }

    fn builtin_ty(&mut self, b: BuiltinTy, args: Vec<Ty>, unit: UnitId, span: Span, name: Symbol) -> Ty {
        let want = match b {
            BuiltinTy::List | BuiltinTy::Set => 1,
            BuiltinTy::Map => 2,
            _ => 0,
        };
        if args.len() != want {
            self.err(
                codes::E0313,
                span,
                format!(
                    "`{name}` takes {want} type argument{}, found {}",
                    if want == 1 { "" } else { "s" },
                    args.len()
                ),
            );
            return self.tys.error;
        }
        match b {
            BuiltinTy::Int(k) => self.tys.int_ty(k, unit),
            BuiltinTy::Float => self.tys.intern(TyKind::Float(unit)),
            BuiltinTy::Bool => self.tys.bool,
            BuiltinTy::Char => self.tys.char,
            BuiltinTy::Str => self.tys.str,
            BuiltinTy::Never => self.tys.never,
            BuiltinTy::List => self.tys.list(args[0]),
            BuiltinTy::Set => self.tys.set(args[0]),
            BuiltinTy::Map => self.tys.map_ty(args[0], args[1]),
        }
    }

    fn type_from_def(&mut self, def: Def, name: ast::Name, args: Vec<Ty>, span: Span) -> Ty {
        match def {
            Def::Adt(a) => {
                let n = self.defs.adts[a].generics.len();
                if args.len() != n {
                    self.err(
                        codes::E0313,
                        span,
                        format!(
                            "`{}` takes {n} type argument{}, found {}",
                            name.sym,
                            if n == 1 { "" } else { "s" },
                            args.len()
                        ),
                    );
                    return self.tys.error;
                }
                self.tys.adt(a, args)
            }
            Def::Alias(al) => {
                self.ensure_alias(al);
                let n = self.defs.aliases[al].generics.len();
                if args.len() != n {
                    self.err(
                        codes::E0313,
                        span,
                        format!("`{}` takes {n} type arguments, found {}", name.sym, args.len()),
                    );
                    return self.tys.error;
                }
                let t = self.defs.aliases[al].ty;
                self.tys.subst(t, &args)
            }
            Def::Builtin(b) => self.builtin_ty(b, args, NO_UNIT, span, name.sym),
            Def::Iface(_) => {
                self.diags.push(
                    Diagnostic::error(codes::E0325, name.span, format!("interface `{}` is not a type", name.sym))
                        .note("use it as a bound on a type parameter: `fn f<T: Shape>(x: T)`"),
                );
                self.tys.error
            }
            _ => {
                self.err(codes::E0325, name.span, format!("`{}` is not a type", name.sym));
                self.tys.error
            }
        }
    }

    pub fn resolve_effects(&mut self, pid: PkgId, list: crate::util::List<ast::Name>) -> EffectSet {
        let names: Vec<ast::Name> = self.ast(pid).names.get(list).to_vec();
        let mut set = EffectSet::PURE;
        for n in names {
            match EffectSet::from_name(n.sym.as_str()) {
                Some(e) => {
                    if set.contains(e) {
                        self.err(codes::E0402, n.span, format!("effect `{}` is listed twice", n.sym));
                    }
                    set = set.union(e)
                }
                None => self.diags.push(
                    Diagnostic::error(codes::E0402, n.span, format!("unknown effect `{}`", n.sym))
                        .note("effects are io, fs, net, clock, rand, env, spawn and ffi"),
                ),
            }
        }
        set
    }

    fn resolve_bounds(&mut self, sc: &TyScope, bounds: crate::util::List<TypeExprId>) -> Vec<Bound> {
        let ids: Vec<TypeExprId> = self.ast(sc.pkg).type_lists.get(bounds).to_vec();
        let mut out = Vec::new();
        for b in ids {
            let te = *self.ast(sc.pkg).ty(b);
            let TypeExprKind::Named { pkg, name, .. } = te.kind else {
                self.err(codes::E0312, te.span, "a bound must name an interface");
                continue;
            };
            if pkg.is_none() {
                if let Some(bb) = builtin_bound(name.sym) {
                    out.push(bb);
                    continue;
                }
            }
            let def = match pkg {
                Some(p) => match self.pkgs[sc.pkg].imports.get(sc.file).and_then(|m| m.get(&p.sym)).copied() {
                    Some((target, _)) => self.lookup_in(target, name, sc.pkg),
                    None => None,
                },
                None => self.lookup_name(sc, name),
            };
            match def {
                Some(Def::Iface(i)) => out.push(Bound::Iface(i)),
                _ => self.err(codes::E0312, name.span, format!("`{}` is not an interface", name.sym)),
            }
        }
        out
    }

    // ---- aliases ----

    fn resolve_aliases(&mut self) {
        self.alias_state = vec![0; self.defs.aliases.len()];
        for i in 0..self.defs.aliases.len() {
            self.ensure_alias(AliasId(i as u32));
        }
    }

    fn ensure_alias(&mut self, al: AliasId) {
        if self.alias_state.is_empty() {
            self.alias_state = vec![0; self.defs.aliases.len()];
        }
        match self.alias_state[al.0 as usize] {
            2 => return,
            1 => {
                let a = &self.defs.aliases[al];
                let span = self.pkgs[a.pkg].ast.items[a.item].name.span;
                self.err(codes::E0319, span, format!("type alias `{}` refers to itself", a.name));
                self.defs.aliases[al].ty = self.tys.error;
                self.alias_state[al.0 as usize] = 2;
                return;
            }
            _ => {}
        }
        self.alias_state[al.0 as usize] = 1;
        let (pid, item_id) = (self.defs.aliases[al].pkg, self.defs.aliases[al].item);
        let item = self.ast(pid).items[item_id];
        let ItemKind::TypeAlias { ty, .. } = item.kind else { unreachable!() };
        let generics: Vec<Symbol> = self.defs.aliases[al].generics.iter().map(|g| g.name).collect();
        let sc = TyScope { pkg: pid, file: item.file as usize, generics, has_self: false };
        let t = self.resolve_type(&sc, ty);
        let refine = match self.ast(pid).ty(ty).kind {
            TypeExprKind::Refined { pred, .. } => Some(pred),
            _ => None,
        };
        self.defs.aliases[al].ty = t;
        self.defs.aliases[al].refine = refine;
        self.alias_state[al.0 as usize] = 2;
    }

    // ---- structs and enums ----

    fn resolve_adts(&mut self) {
        for i in 0..self.defs.adts.len() {
            let aid = AdtId(i as u32);
            let (pid, item_id) = (self.defs.adts[aid].pkg, self.defs.adts[aid].item);
            let item = self.ast(pid).items[item_id];
            let generics: Vec<Symbol> = self.defs.adts[aid].generics.iter().map(|g| g.name).collect();
            let sc = TyScope { pkg: pid, file: item.file as usize, generics, has_self: false };
            match item.kind {
                ItemKind::Struct { fields, .. } => {
                    let fs: Vec<ast::FieldDecl> = self.ast(pid).fields.get(fields).to_vec();
                    let defs = self.resolve_fields(&sc, &fs, "field");
                    self.defs.adts[aid].kind = AdtKind::Struct { fields: defs };
                }
                ItemKind::Enum { variants, .. } => {
                    let vs: Vec<ast::VariantDecl> = self.ast(pid).variants.get(variants).to_vec();
                    let mut out: Vec<VariantDef> = Vec::new();
                    for v in vs {
                        if out.iter().any(|x| x.name == v.name.sym) {
                            self.err(codes::E0322, v.name.span, format!("variant `{}` is declared twice", v.name.sym));
                            continue;
                        }
                        let fs: Vec<ast::FieldDecl> = self.ast(pid).fields.get(v.fields).to_vec();
                        let fields = self.resolve_fields(&sc, &fs, "variant field");
                        let refined =
                            fields.iter().any(|f| f.refine.is_some()) || fs.iter().any(|f| f.refine.is_some());
                        out.push(VariantDef { name: v.name.sym, fields, span: v.span, refined });
                    }
                    self.defs.adts[aid].kind = AdtKind::Enum { variants: out };
                }
                _ => unreachable!(),
            }
        }
    }

    fn resolve_fields(&mut self, sc: &TyScope, fs: &[ast::FieldDecl], what: &str) -> Vec<FieldDef> {
        let mut out: Vec<FieldDef> = Vec::new();
        for f in fs {
            if out.iter().any(|x| x.name == f.name.sym) {
                self.err(codes::E0322, f.name.span, format!("{what} `{}` is declared twice", f.name.sym));
                continue;
            }
            let ty = self.resolve_type(sc, f.ty);
            let refine = self.refinement_alias(sc, f.ty);
            out.push(FieldDef { name: f.name.sym, ty, is_pub: f.is_pub, span: f.span, refine });
        }
        out
    }

    fn check_recursive_adts(&mut self) {
        // A struct containing itself by value (through structs and tuples) has infinite size.
        // Enums that reach themselves are boxed.
        let n = self.defs.adts.len();
        for i in 0..n {
            let aid = AdtId(i as u32);
            let mut seen = FxHashSet::default();
            let mut stack: Vec<Ty> = Vec::new();
            for t in self.adt_payload_tys(aid) {
                stack.push(t);
            }
            let mut reaches_self = false;
            let mut via_enum_only = true;
            while let Some(t) = stack.pop() {
                match self.tys.kind(t).clone() {
                    TyKind::Adt(b, _) => {
                        if b == aid {
                            reaches_self = true;
                            continue;
                        }
                        if !seen.insert(b) {
                            continue;
                        }
                        if self.defs.adts[b].is_enum() {
                            // Reached through an enum: if this enum is recursive it is boxed.
                            via_enum_only = via_enum_only && true;
                        }
                        for t2 in self.adt_payload_tys(b) {
                            stack.push(t2);
                        }
                    }
                    TyKind::Tuple(ts) => stack.extend(ts.iter().copied()),
                    _ => {}
                }
            }
            if reaches_self {
                if self.defs.adts[aid].is_enum() {
                    self.defs.adts[aid].recursive = true;
                } else if !self.struct_reaches_self_through_enum(aid) {
                    let a = &self.defs.adts[aid];
                    let span = self.pkgs[a.pkg].ast.items[a.item].name.span;
                    let name = a.name;
                    self.err(
                        codes::E0319,
                        span,
                        format!("struct `{name}` contains itself and would have infinite size"),
                    );
                }
            }
            let _ = via_enum_only;
        }
    }

    /// True if every path from the struct back to itself passes through an enum (which then
    /// gets boxed), so the struct has a finite size.
    fn struct_reaches_self_through_enum(&self, aid: AdtId) -> bool {
        let mut seen = FxHashSet::default();
        let mut stack: Vec<Ty> = self.adt_payload_tys(aid);
        while let Some(t) = stack.pop() {
            match self.tys.kind(t) {
                TyKind::Adt(b, _) => {
                    if *b == aid {
                        return false;
                    }
                    if self.defs.adts[*b].is_enum() || !seen.insert(*b) {
                        continue;
                    }
                    stack.extend(self.adt_payload_tys(*b));
                }
                TyKind::Tuple(ts) => stack.extend(ts.iter().copied()),
                _ => {}
            }
        }
        true
    }

    fn adt_payload_tys(&self, aid: AdtId) -> Vec<Ty> {
        match &self.defs.adts[aid].kind {
            AdtKind::Struct { fields } => fields.iter().map(|f| f.ty).collect(),
            AdtKind::Enum { variants } => variants.iter().flat_map(|v| v.fields.iter().map(|f| f.ty)).collect(),
        }
    }

    // ---- interfaces ----

    fn resolve_ifaces(&mut self) {
        // Interface method signatures are resolved with the functions.
    }

    // ---- functions ----

    fn resolve_fns(&mut self) {
        for i in 0..self.defs.fns.len() {
            self.resolve_fn(FnId(i as u32));
        }
    }

    fn resolve_fn(&mut self, fid: FnId) {
        let (pid, item_id) = (self.defs.fns[fid].pkg, self.defs.fns[fid].item);
        let item = self.ast(pid).items[item_id];
        let ItemKind::Fn(f) = item.kind else { unreachable!() };
        let is_iface = self.defs.fns[fid].iface.is_some();
        let generics: Vec<Symbol> = self.defs.fns[fid].generics.iter().map(|g| g.name).collect();
        let sc = TyScope { pkg: pid, file: item.file as usize, generics, has_self: is_iface };
        // bounds
        let mut gi = if is_iface { 1 } else { 0 };
        let recv_generics = f.recv.map(|r| r.generics).unwrap_or_default();
        for list in [recv_generics, f.generics] {
            let gps: Vec<ast::GenericParam> = self.ast(pid).generics.get(list).to_vec();
            for g in gps {
                let b = self.resolve_bounds(&sc, g.bounds);
                self.defs.fns[fid].generics[gi].bounds = b;
                gi += 1;
            }
        }
        // receiver
        if is_iface {
            if f.self_param.is_none() {
                self.err(codes::E0327, item.name.span, "interface methods take `self` or `&mut self`");
            }
            let self_ty = self.tys.param(0, Symbol::intern("Self"));
            self.defs.fns[fid].self_ty = Some(self_ty);
        } else if let Some(r) = f.recv {
            let n = self.defs.fns[fid].n_recv_generics;
            let params: Vec<Ty> = (0..n).map(|i| self.tys.param(i as u32, sc.generics[i])).collect();
            let (key, self_ty) = match self.pkgs[pid].scope.get(&r.ty.sym).copied() {
                Some(Def::Adt(a)) => {
                    let want = self.defs.adts[a].generics.len();
                    if want != n {
                        self.err(
                            codes::E0313,
                            r.ty.span,
                            format!(
                                "`{}` takes {want} type parameter{}; write `fn {}{}.{}`",
                                r.ty.sym,
                                if want == 1 { "" } else { "s" },
                                r.ty.sym,
                                generic_hint(want),
                                item.name.sym
                            ),
                        );
                        return;
                    }
                    (TyKey::Adt(a), self.tys.adt(a, params))
                }
                Some(_) => {
                    self.err(codes::E0328, r.ty.span, format!("`{}` is not a struct or enum", r.ty.sym));
                    return;
                }
                None => match builtin_type(r.ty.sym) {
                    Some(b) if pid == self.prelude => {
                        let t = self.builtin_ty(b, params, NO_UNIT, r.ty.span, r.ty.sym);
                        match TyKey::of(self.tys, t) {
                            Some(k) => (k, t),
                            None => return,
                        }
                    }
                    Some(_) => {
                        self.err(
                            codes::E0328,
                            r.ty.span,
                            format!("cannot declare methods on the built-in type `{}`", r.ty.sym),
                        );
                        return;
                    }
                    None => {
                        let in_other = self.pkgs.iter().any(|p| p.scope.contains_key(&r.ty.sym));
                        if in_other {
                            self.err(
                                codes::E0328,
                                r.ty.span,
                                format!("methods on `{}` must be declared in its own package", r.ty.sym),
                            );
                        } else {
                            self.err(codes::E0201, r.ty.span, format!("cannot find type `{}`", r.ty.sym));
                        }
                        return;
                    }
                },
            };
            self.defs.fns[fid].recv = Some(key);
            self.defs.fns[fid].self_ty = Some(self_ty);
            if let Some(&prev) = self.defs.methods.get(&(key, item.name.sym)) {
                let prev_span = self.defs.fns[prev].name_span;
                self.diags.push(
                    Diagnostic::error(
                        codes::E0202,
                        item.name.span,
                        format!("method `{}` is defined more than once", item.name.sym),
                    )
                    .label(prev_span, "first defined here"),
                );
            } else {
                self.defs.methods.insert((key, item.name.sym), fid);
            }
            // A method may share a field's name: `x.name` reads the field, `x.name()` calls the
            // method (so a struct can satisfy a getter-style interface).
        } else if f.self_param.is_some() {
            self.err(codes::E0327, item.name.span, "only methods (`fn Type.name`) can take `self`");
        }
        // parameters
        let ps: Vec<ast::Param> = self.ast(pid).params.get(f.params).to_vec();
        let mut params: Vec<ParamDef> = Vec::new();
        for p in ps {
            if params.iter().any(|q| q.name == p.name.sym) {
                self.err(codes::E0322, p.name.span, format!("parameter `{}` is declared twice", p.name.sym));
            }
            let ty = match p.ty {
                Some(t) => self.resolve_type(&sc, t),
                None => self.tys.error,
            };
            let refine = p.ty.and_then(|t| self.refinement_alias(&sc, t));
            params.push(ParamDef {
                name: p.name.sym,
                ty,
                mode: if p.mut_ref { Mode::MutRef } else { Mode::Value },
                span: p.span,
                refine,
            });
        }
        self.defs.fns[fid].params = params;
        self.defs.fns[fid].ret = match f.ret {
            Some(t) => self.resolve_type(&sc, t),
            None => self.tys.unit,
        };
        self.defs.fns[fid].ret_refine = f.ret.and_then(|t| self.refinement_alias(&sc, t));
        self.defs.fns[fid].effects = match f.effects {
            Some(list) => self.resolve_effects(pid, list),
            None => EffectSet::PURE,
        };
        let ffi_effect = EffectSet::from_name("ffi").unwrap_or(EffectSet::PURE);
        if !self.defs.fns[fid].has_body && !self.pkgs[pid].is_std && !is_iface {
            if self.defs.fns[fid].effects.contains(ffi_effect) {
                self.defs.fns[fid].ffi = true;
                self.check_ffi_signature(fid);
            } else {
                self.err(codes::E0109, item.name.span, format!("function `{}` has no body", item.name.sym));
            }
        }
    }

    /// A C function's signature: scalars and `Str` only, few enough arguments to pass in
    /// registers, no generics, methods or `&mut`.
    fn check_ffi_signature(&mut self, fid: FnId) {
        let fd = &self.defs.fns[fid];
        let span = fd.name_span;
        let name = fd.name;
        let mut problems: Vec<String> = Vec::new();
        if !fd.generics.is_empty() {
            problems.push("it is generic".into());
        }
        if fd.recv.is_some() || fd.self_mode.is_some() {
            problems.push("it is a method".into());
        }
        let (mut ints, mut floats) = (0, 0);
        let params: Vec<(Symbol, Ty, Mode)> = fd.params.iter().map(|p| (p.name, p.ty, p.mode)).collect();
        let ret = fd.ret;
        for (pname, t, mode) in params {
            if mode == Mode::MutRef {
                problems.push(format!("`{pname}` is `&mut`"));
            }
            match self.tys.kind(t) {
                TyKind::Float(_) => floats += 1,
                TyKind::Int(..) | TyKind::Bool | TyKind::Str => ints += 1,
                _ => problems.push(format!("`{pname}` has type `{}`", self.tys.display(t))),
            }
        }
        if !matches!(self.tys.kind(ret), TyKind::Int(..) | TyKind::Float(_) | TyKind::Bool | TyKind::Str)
            && !self.tys.is_unit(ret)
        {
            problems.push(format!("it returns `{}`", self.tys.display(ret)));
        }
        if ints > 6 || floats > 8 {
            problems.push("it takes more than 6 integer or 8 `Float` arguments".into());
        }
        if !problems.is_empty() {
            self.diags.push(
                Diagnostic::error(
                    codes::E0331,
                    span,
                    format!("`{name}` cannot be a C function: {}", problems.join(", ")),
                )
                .note("C functions take and return `Int`, `I32`, `U8`, `U32`, `U64`, `Float`, `Bool` and `Str`, or return nothing"),
            );
        }
    }

    // ---- constants ----

    fn resolve_consts(&mut self) {
        for i in 0..self.defs.consts.len() {
            let cid = ConstId(i as u32);
            let (pid, item_id) = (self.defs.consts[cid].pkg, self.defs.consts[cid].item);
            let item = self.ast(pid).items[item_id];
            let ItemKind::Const { ty, .. } = item.kind else { unreachable!() };
            let sc = TyScope { pkg: pid, file: item.file as usize, generics: Vec::new(), has_self: false };
            let t = self.resolve_type(&sc, ty);
            self.defs.consts[cid].ty = t;
        }
        for i in 0..self.defs.consts.len() {
            let cid = ConstId(i as u32);
            let mut visiting = Vec::new();
            self.eval_const(cid, &mut visiting);
        }
    }

    fn eval_const(&mut self, cid: ConstId, visiting: &mut Vec<ConstId>) -> Option<ConstVal> {
        if let Some(v) = &self.defs.consts[cid].value {
            return Some(v.clone());
        }
        let (pid, item_id) = (self.defs.consts[cid].pkg, self.defs.consts[cid].item);
        let item = self.ast(pid).items[item_id];
        if visiting.contains(&cid) {
            self.err(codes::E0329, item.name.span, format!("constant `{}` depends on itself", item.name.sym));
            return None;
        }
        visiting.push(cid);
        let ItemKind::Const { value, .. } = item.kind else { unreachable!() };
        let ty = self.defs.consts[cid].ty;
        let v = self.eval_const_expr(pid, item.file as usize, value, ty, visiting);
        visiting.pop();
        if let Some(v) = &v {
            self.defs.consts[cid].value = Some(v.clone());
        }
        v
    }

    fn eval_const_expr(
        &mut self,
        pid: PkgId,
        file: usize,
        e: ast::ExprId,
        ty: Ty,
        visiting: &mut Vec<ConstId>,
    ) -> Option<ConstVal> {
        let ex = *self.ast(pid).expr(e);
        let bad = |c: &mut Self, msg: &str| {
            c.err(codes::E0329, ex.span, msg.to_string());
            None
        };
        let kind = self.tys.kind(ty).clone();
        match ex.kind {
            ExprKind::Paren(inner) => self.eval_const_expr(pid, file, inner, ty, visiting),
            ExprKind::Int(v) => match kind {
                TyKind::Int(k, _) => {
                    if v as i128 > k.max() {
                        return bad(self, &format!("literal does not fit in `{}`", k.name()));
                    }
                    Some(ConstVal::Int(v as i128))
                }
                TyKind::Float(_) => Some(ConstVal::Float(v as f64)),
                _ => self.const_mismatch(ex.span, ty, "an integer"),
            },
            ExprKind::Float(v) => match kind {
                TyKind::Float(_) => Some(ConstVal::Float(v)),
                _ => self.const_mismatch(ex.span, ty, "a float"),
            },
            ExprKind::Bool(b) => match kind {
                TyKind::Bool => Some(ConstVal::Bool(b)),
                _ => self.const_mismatch(ex.span, ty, "a Bool"),
            },
            ExprKind::Str(s) => match kind {
                TyKind::Str => Some(ConstVal::Str(s)),
                _ => self.const_mismatch(ex.span, ty, "a Str"),
            },
            ExprKind::Char(c) => match kind {
                TyKind::Char => Some(ConstVal::Char(c)),
                TyKind::Int(IntTy::U8, _) if c < 128 => Some(ConstVal::Int(c as i128)),
                _ => self.const_mismatch(ex.span, ty, "a Char"),
            },
            ExprKind::Unary { op, expr } => {
                let v = self.eval_const_expr(pid, file, expr, ty, visiting)?;
                match (op, v) {
                    (UnOp::Neg, ConstVal::Int(i)) => self.check_int_range(ex.span, ty, -i),
                    (UnOp::Neg, ConstVal::Float(f)) => Some(ConstVal::Float(-f)),
                    (UnOp::Not, ConstVal::Bool(b)) => Some(ConstVal::Bool(!b)),
                    (UnOp::BitNot, ConstVal::Int(i)) => self.check_int_range(ex.span, ty, !i),
                    _ => bad(self, "invalid operand in constant expression"),
                }
            }
            ExprKind::Binary { op, lhs, rhs } => {
                use ast::BinOp::*;
                let operand_ty = if op.is_comparison() { None } else { Some(ty) };
                let (a, b) = match operand_ty {
                    Some(t) => (
                        self.eval_const_expr(pid, file, lhs, t, visiting)?,
                        self.eval_const_expr(pid, file, rhs, t, visiting)?,
                    ),
                    None => return bad(self, "comparisons are not supported in constant expressions"),
                };
                let r = match (a, b) {
                    (ConstVal::Int(x), ConstVal::Int(y)) => match op {
                        Add => x.checked_add(y),
                        Sub => x.checked_sub(y),
                        Mul => x.checked_mul(y),
                        Div if y != 0 => Some(x / y),
                        Rem if y != 0 => Some(x % y),
                        Shl if (0..64).contains(&y) => Some(x << y),
                        Shr if (0..64).contains(&y) => Some(x >> y),
                        BitAnd => Some(x & y),
                        BitOr => Some(x | y),
                        BitXor => Some(x ^ y),
                        _ => None,
                    }
                    .map(ConstVal::Int),
                    (ConstVal::Float(x), ConstVal::Float(y)) => match op {
                        Add => Some(x + y),
                        Sub => Some(x - y),
                        Mul => Some(x * y),
                        Div => Some(x / y),
                        _ => None,
                    }
                    .map(ConstVal::Float),
                    (ConstVal::Bool(x), ConstVal::Bool(y)) => match op {
                        And => Some(x && y),
                        Or => Some(x || y),
                        _ => None,
                    }
                    .map(ConstVal::Bool),
                    (ConstVal::Str(x), ConstVal::Str(y)) if op == Add => {
                        Some(ConstVal::Str(Symbol::intern(&format!("{x}{y}"))))
                    }
                    _ => None,
                };
                match r {
                    Some(ConstVal::Int(i)) => self.check_int_range(ex.span, ty, i),
                    Some(v) => Some(v),
                    None => {
                        bad(self, "invalid constant expression (overflow, division by zero, or unsupported operator)")
                    }
                }
            }
            ExprKind::Name(n) => {
                let sc = TyScope { pkg: pid, file, generics: Vec::new(), has_self: false };
                match self.lookup_name(&sc, ast::Name { sym: n, span: ex.span }) {
                    Some(Def::Const(c)) => {
                        let cty = self.defs.consts[c].ty;
                        if cty != ty {
                            return self.const_mismatch(ex.span, ty, &format!("`{}`", self.tys.display(cty)));
                        }
                        self.eval_const(c, visiting)
                    }
                    _ => bad(self, "constants may only refer to other constants"),
                }
            }
            ExprKind::Field { base, name } => {
                let ExprKind::Name(p) = self.ast(pid).expr(base).kind else {
                    return bad(self, "not a constant expression");
                };
                let Some(&(target, _)) = self.pkgs[pid].imports.get(file).and_then(|m| m.get(&p)) else {
                    return bad(self, "not a constant expression");
                };
                match self.lookup_in(target, name, pid) {
                    Some(Def::Const(c)) => self.eval_const(c, visiting),
                    _ => bad(self, "constants may only refer to other constants"),
                }
            }
            _ => bad(self, "constants must be literals or arithmetic on other constants"),
        }
    }

    fn const_mismatch(&mut self, span: Span, ty: Ty, found: &str) -> Option<ConstVal> {
        let t = self.tys.display(ty);
        self.err(codes::E0301, span, format!("mismatched types: expected `{t}`, found {found}"));
        None
    }

    fn check_int_range(&mut self, span: Span, ty: Ty, v: i128) -> Option<ConstVal> {
        if let TyKind::Int(k, _) = *self.tys.kind(ty) {
            if v < k.min() || v > k.max() {
                self.err(codes::E0329, span, format!("constant value {v} does not fit in `{}`", k.name()));
                return None;
            }
        }
        Some(ConstVal::Int(v))
    }

    // ---- tests ----

    fn resolve_tests(&mut self) {
        for i in 0..self.defs.tests.len() {
            let tid = TestId(i as u32);
            let (pid, item_id) = (self.defs.tests[tid].pkg, self.defs.tests[tid].item);
            let item = self.ast(pid).items[item_id];
            let ItemKind::Test { params, .. } = item.kind else { unreachable!() };
            let sc = TyScope { pkg: pid, file: item.file as usize, generics: Vec::new(), has_self: false };
            let ps: Vec<ast::Param> = self.ast(pid).params.get(params).to_vec();
            let mut out = Vec::new();
            for p in ps {
                let ty = match p.ty {
                    Some(t) => self.resolve_type(&sc, t),
                    None => {
                        self.err(codes::E0305, p.name.span, "test parameters need a type");
                        self.tys.error
                    }
                };
                if p.mut_ref {
                    self.err(codes::E0314, p.span, "test parameters cannot be `&mut`");
                }
                let refine = p.ty.and_then(|t| self.refinement_alias(&sc, t));
                out.push(ParamDef { name: p.name.sym, ty, mode: Mode::Value, span: p.span, refine });
            }
            self.defs.tests[tid].params = out;
        }
    }

    fn find_main(&mut self, root: PkgId) {
        if let Some(Def::Fn(f)) = self.pkgs[root].scope.get(&sym::MAIN).copied() {
            self.defs.main = Some(f);
            let fd = &self.defs.fns[f];
            let bad = !fd.params.is_empty() || !fd.generics.is_empty() || fd.ret != self.tys.unit;
            if bad {
                let span = fd.name_span;
                self.err(codes::E0318, span, "`main` takes no parameters and returns nothing");
            }
        }
    }
}

fn generic_hint(n: usize) -> String {
    if n == 0 {
        return String::new();
    }
    let names: Vec<String> = (0..n).map(|i| ((b'T' + i as u8) as char).to_string()).collect();
    format!("<{}>", names.join(", "))
}

pub fn is_numeric_name(s: Symbol) -> bool {
    is_numeric_type_name(s)
}
