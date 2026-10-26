//! Binding scenario steps to `step` functions, with typed placeholder arguments.
//!
//! A step function takes an optional first parameter for the world (any struct with
//! `fn World.new() -> World`: `w: &mut World` to change it, `w: World` to read it) and one
//! parameter per `{placeholder}`, by name. Each scenario
//! gets a fresh world; its steps run in order on it.

use super::{compile_pattern, match_pattern, suggest, Piece};
use crate::diag::{codes, Diagnostic, Span};
use crate::sema::defs::{Mode, Program, TyKey};
use crate::sema::ty::{FnId, IntTy, PkgId, Ty, TyKind};
use crate::util::{FxHashMap, Symbol};

#[derive(Clone, Debug)]
pub struct ScenarioDef {
    pub pkg: PkgId,
    pub feature: String,
    pub name: String,
    pub span: Span,
    /// The world's type and its `new` function, when any step takes one.
    pub world: Option<(Ty, FnId)>,
    pub steps: Vec<StepCall>,
}

#[derive(Clone, Debug)]
pub struct StepCall {
    pub f: FnId,
    pub args: Vec<StepArg>,
    pub span: Span,
    pub text: String,
}

#[derive(Clone, Debug)]
pub enum StepArg {
    Int(i128, IntTy),
    Float(f64),
    Bool(bool),
    Str(Symbol),
}

struct StepDef {
    f: FnId,
    pieces: Vec<Piece>,
    /// The world type, if the first parameter is `&mut World`.
    world: Option<Ty>,
    /// For each non-world parameter, in order: (name, type).
    params: Vec<(Symbol, Ty)>,
}

fn unquote(s: &str) -> Option<String> {
    let inner = s.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::new();
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next()? {
                'n' => out.push('\n'),
                't' => out.push('\t'),
                c => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

fn convert(text: &str, t: Ty, prog: &Program) -> Result<StepArg, String> {
    match prog.tys.kind(t) {
        TyKind::Str => unquote(text)
            .map(|s| StepArg::Str(Symbol::intern(&s)))
            .ok_or_else(|| format!("`{text}` is not a quoted string")),
        TyKind::Int(k, _) => {
            let clean: String = text.chars().filter(|&c| c != '_').collect();
            let v: i128 = clean.parse().map_err(|_| format!("`{text}` is not an integer"))?;
            if v < IntTy::min(*k) || v > IntTy::max(*k) {
                return Err(format!("{v} does not fit in `{}`", k.name()));
            }
            Ok(StepArg::Int(v, *k))
        }
        TyKind::Float(_) => text.parse::<f64>().map(StepArg::Float).map_err(|_| format!("`{text}` is not a number")),
        TyKind::Bool => match text {
            "true" => Ok(StepArg::Bool(true)),
            "false" => Ok(StepArg::Bool(false)),
            _ => Err(format!("`{text}` is not `true` or `false`")),
        },
        _ => Err("unsupported placeholder type".into()),
    }
}

/// Check every package's step functions and bind its feature files.
pub fn bind(prog: &mut Program) -> Vec<ScenarioDef> {
    let mut out = Vec::new();
    let mut diags = Vec::new();
    for pid in prog.pkgs.indices() {
        if prog.pkgs[pid].is_std {
            continue;
        }
        let steps = step_defs(prog, pid, &mut diags);
        let files = prog.pkgs[pid].feature_files.clone();
        for fid in files {
            let (text, base) = {
                let f = prog.sm.file(fid);
                (f.text.clone(), f.base)
            };
            let (feature, ds) = super::parse(&text, base);
            diags.extend(ds);
            let Some(feature) = feature else { continue };
            for sc in &feature.scenarios {
                for (name, lines) in expand(sc) {
                    if let Some(def) =
                        bind_scenario(prog, pid, &feature.name, name, sc.span, &lines, &steps, &mut diags)
                    {
                        out.push(def);
                    }
                }
            }
        }
    }
    for d in diags {
        prog.diags.push(d);
    }
    out
}

/// A scenario's runs: one, or one per `Examples` row with `<column>` substituted.
fn expand(sc: &super::ScenarioAst) -> Vec<(String, Vec<super::StepLine>)> {
    let Some((header, rows)) = &sc.examples else { return vec![(sc.name.clone(), sc.steps.clone())] };
    rows.iter()
        .map(|(cells, _)| {
            let lines = sc
                .steps
                .iter()
                .map(|s| {
                    let mut text = s.text.clone();
                    for (h, c) in header.iter().zip(cells) {
                        text = text.replace(&format!("<{h}>"), c);
                    }
                    super::StepLine { text, ..s.clone() }
                })
                .collect();
            (format!("{} [{}]", sc.name, cells.join(", ")), lines)
        })
        .collect()
}

fn step_defs(prog: &Program, pid: PkgId, diags: &mut Vec<Diagnostic>) -> Vec<StepDef> {
    let mut out = Vec::new();
    for (f, fd) in prog.defs.fns.iter_enumerated() {
        if fd.pkg != pid {
            continue;
        }
        let Some((pattern, pspan)) = fd.step else { continue };
        let bad = |msg: String| Diagnostic::error(codes::E0805, pspan, msg);
        let pieces = match compile_pattern(pattern.as_str()) {
            Ok(p) => p,
            Err(e) => {
                diags.push(bad(e));
                continue;
            }
        };
        if !fd.generics.is_empty() || fd.self_mode.is_some() || fd.recv.is_some() {
            diags.push(bad("a step must be a plain function (no generics, not a method)".into()));
            continue;
        }
        if !prog.tys.is_unit(fd.ret) {
            diags.push(bad("a step function returns `()`; check results with `assert`".into()));
            continue;
        }
        let mut world = None;
        let mut params = Vec::new();
        let mut ok = true;
        for (i, p) in fd.params.iter().enumerate() {
            // The world: a struct first parameter, `&mut` for steps that change it.
            if i == 0 && matches!(prog.tys.kind(p.ty), TyKind::Adt(..)) {
                world = Some(p.ty);
                continue;
            }
            if p.mode == Mode::MutRef {
                diags.push(bad(format!("only the first parameter (the world) may be `&mut`; `{}` is too", p.name)));
                ok = false;
                continue;
            }
            match prog.tys.kind(p.ty) {
                TyKind::Str | TyKind::Int(..) | TyKind::Float(_) | TyKind::Bool => params.push((p.name, p.ty)),
                _ => {
                    diags.push(bad(format!(
                        "placeholder `{}` must be `Str`, a number or `Bool`, not `{}`",
                        p.name,
                        prog.tys.display(p.ty)
                    )));
                    ok = false;
                }
            }
        }
        let holes: Vec<&str> = pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Hole(h) => Some(h.as_str()),
                _ => None,
            })
            .collect();
        let mut problems = Vec::new();
        for h in &holes {
            if !params.iter().any(|(n, _)| n.as_str() == *h) {
                problems.push(format!("placeholder `{{{h}}}` names no parameter"));
            }
        }
        for (n, _) in &params {
            let count = holes.iter().filter(|h| **h == n.as_str()).count();
            if count != 1 {
                problems.push(format!("parameter `{n}` must appear exactly once, as `{{{n}}}`"));
            }
        }
        if !problems.is_empty() {
            let mut d = bad("the step's placeholders and parameters do not match".into());
            for p in problems {
                d = d.note(p);
            }
            diags.push(d);
            ok = false;
        }
        if ok {
            out.push(StepDef { f, pieces, world, params });
        }
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn bind_scenario(
    prog: &Program,
    pid: PkgId,
    feature: &str,
    name: String,
    span: Span,
    lines: &[super::StepLine],
    steps: &[StepDef],
    diags: &mut Vec<Diagnostic>,
) -> Option<ScenarioDef> {
    let mut calls = Vec::new();
    let mut world: Option<Ty> = None;
    let mut ok = true;
    for line in lines {
        let matches: Vec<(&StepDef, Vec<String>)> =
            steps.iter().filter_map(|s| match_pattern(&s.pieces, &line.text).map(|caps| (s, caps))).collect();
        match matches.len() {
            0 => {
                let (pat, ps) = suggest(&line.text);
                let mut sig = vec!["w: &mut World".to_string()];
                sig.extend(ps.iter().map(|(n, t)| format!("{n}: {t}")));
                let fname: String = line
                    .text
                    .split(|c: char| !c.is_ascii_alphanumeric())
                    .filter(|w| !w.is_empty() && w.parse::<f64>().is_err())
                    .take(4)
                    .map(|w| w.to_ascii_lowercase())
                    .collect::<Vec<_>>()
                    .join("_");
                diags.push(
                    Diagnostic::error(codes::E0802, line.text_span, format!("no step matches `{}`", line.text)).note(
                        format!(
                            "add one:\nstep \"{pat}\"\nfn {}_{fname}({}) {{ ... }}",
                            line.keyword.to_ascii_lowercase(),
                            sig.join(", ")
                        ),
                    ),
                );
                ok = false;
            }
            1 => {
                let (s, caps) = &matches[0];
                let holes: Vec<&str> = s
                    .pieces
                    .iter()
                    .filter_map(|p| match p {
                        Piece::Hole(h) => Some(h.as_str()),
                        _ => None,
                    })
                    .collect();
                let by_name: FxHashMap<&str, &str> =
                    holes.iter().copied().zip(caps.iter().map(|c| c.as_str())).collect();
                let mut args = Vec::new();
                for (pn, pt) in &s.params {
                    let text = by_name.get(pn.as_str()).copied().unwrap_or("");
                    match convert(text, *pt, prog) {
                        Ok(a) => args.push(a),
                        Err(e) => {
                            diags.push(Diagnostic::error(
                                codes::E0804,
                                line.text_span,
                                format!("`{pn}`: {e} (the parameter is `{}`)", prog.tys.display(*pt)),
                            ));
                            ok = false;
                        }
                    }
                }
                if let Some(w) = s.world {
                    match world {
                        None => world = Some(w),
                        Some(prev) if prev != w => {
                            diags.push(Diagnostic::error(
                                codes::E0805,
                                line.text_span,
                                format!(
                                    "this step uses world `{}` but earlier steps use `{}`",
                                    prog.tys.display(w),
                                    prog.tys.display(prev)
                                ),
                            ));
                            ok = false;
                        }
                        _ => {}
                    }
                }
                calls.push(StepCall { f: s.f, args, span: line.span, text: format!("{} {}", line.keyword, line.text) });
            }
            _ => {
                let mut d = Diagnostic::error(
                    codes::E0803,
                    line.text_span,
                    format!("{} steps match `{}`", matches.len(), line.text),
                );
                for (s, _) in &matches {
                    if let Some((_, sp)) = prog.defs.fns[s.f].step {
                        d = d.label(sp, "matches");
                    }
                }
                diags.push(d);
                ok = false;
            }
        }
    }
    let world = match world {
        None => None,
        Some(w) => {
            let TyKind::Adt(a, _) = prog.tys.kind(w) else { return None };
            let new = prog
                .defs
                .method(TyKey::Adt(*a), Symbol::intern("new"))
                .filter(|&f| prog.defs.fns[f].self_mode.is_none() && prog.defs.fns[f].params.is_empty());
            match new {
                Some(f) => Some((w, f)),
                None => {
                    let n = prog.tys.display(w);
                    diags.push(Diagnostic::error(
                        codes::E0805,
                        span,
                        format!("the world `{n}` needs `fn {n}.new() -> {n}` to start each scenario"),
                    ));
                    return None;
                }
            }
        }
    };
    ok.then(|| ScenarioDef { pkg: pid, feature: feature.to_string(), name, span, world, steps: calls })
}
