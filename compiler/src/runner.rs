//! Running programs and tests on the interpreter.

use crate::mir::interp::{Effects, Interp, Trap, Value};
use crate::mir::lower::Lowerer;
use crate::mir::LowerOpts;
use crate::sema::defs::{Program, TestId};
use crate::sema::ty::Ty;
use crate::util::json::Json;

pub struct TestResult {
    pub name: String,
    pub pkg: String,
    pub passed: bool,
    pub message: String,
    pub location: String,
    pub stdout: String,
    pub backtrace: Vec<String>,
}

fn location(prog_sm: &crate::diag::SourceMap, span: crate::diag::Span) -> String {
    match prog_sm.line_col(span.lo) {
        Some((fid, lc)) => format!("{}:{}:{}", crate::diag::display_path(&prog_sm.file(fid).path), lc.line, lc.col),
        None => String::new(),
    }
}

/// Run on a big stack so deeply recursive programs don't overflow the host.
pub fn with_big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(1 << 30)
            .spawn_scoped(s, f)
            .expect("spawn interpreter thread")
            .join()
            .expect("interpreter thread panicked")
    })
}

pub fn run_tests(prog: &mut Program, filter: Option<&str>) -> Vec<TestResult> {
    let ids: Vec<TestId> = prog
        .defs
        .tests
        .iter_enumerated()
        .filter(|(_, t)| !prog.pkgs[t.pkg].is_std && !prog.pkgs[t.pkg].external)
        .filter(|(_, t)| filter.is_none_or(|f| t.label.as_str().contains(f)))
        .map(|(i, _)| i)
        .collect();
    let labels: Vec<(String, String)> = ids
        .iter()
        .map(|&t| (prog.defs.tests[t].label.to_string(), prog.pkgs[prog.defs.tests[t].pkg].path.to_string()))
        .collect();
    let scenarios: Vec<crate::feature::bind::ScenarioDef> =
        prog.scenarios.iter().filter(|s| filter.is_none_or(|f| s.name.contains(f))).cloned().collect();
    let mut lw = Lowerer::new(prog, LowerOpts { check_contracts: true });
    let mut out = Vec::new();
    for (i, sc) in scenarios.iter().enumerate() {
        let m = lw.scenario_fn(sc);
        let r = match run_case(&mut lw, m, Vec::new(), 0x5ce7 ^ i as u64) {
            Case::Pass(stdout) | Case::Discard(stdout) => TestResult {
                name: format!("Scenario: {}", sc.name),
                pkg: lw.pkgs[sc.pkg].path.to_string(),
                passed: true,
                message: String::new(),
                location: String::new(),
                stdout,
                backtrace: Vec::new(),
            },
            Case::Fail { message, span, backtrace, stdout } => TestResult {
                name: format!("Scenario: {}", sc.name),
                pkg: lw.pkgs[sc.pkg].path.to_string(),
                passed: false,
                // Name the step that failed: the scenario's frame points at its line.
                message: match sc.steps.iter().find(|st| {
                    let loc = location(lw.sm, st.span);
                    backtrace.iter().any(|b| b.ends_with(&format!(" at {loc}")))
                }) {
                    Some(st) => format!("{message}\n  in step: {} ({})", st.text, location(lw.sm, st.span)),
                    None => message,
                },
                location: span.map(|s| location(lw.sm, s)).unwrap_or_default(),
                stdout,
                backtrace,
            },
        };
        out.push(r);
    }
    for (i, &tid) in ids.iter().enumerate() {
        let m = lw.test_fn(tid);
        let seed = 0x9e37_79b9 ^ (i as u64 + 1);
        let (name, pkg) = labels[i].clone();
        let params: Vec<Ty> = lw.defs.tests[tid].params.iter().map(|p| p.ty).collect();
        let outcome = if params.is_empty() {
            run_case(&mut lw, m, Vec::new(), seed)
        } else {
            run_property(&mut lw, tid, m, &params, seed)
        };
        let r = match outcome {
            Case::Pass(stdout) | Case::Discard(stdout) => TestResult {
                name,
                pkg,
                passed: true,
                message: String::new(),
                location: String::new(),
                stdout,
                backtrace: Vec::new(),
            },
            Case::Fail { message, span, backtrace, stdout } => {
                let location = span.map(|s| location(lw.sm, s)).unwrap_or_default();
                TestResult { name, pkg, passed: false, message, location, stdout, backtrace }
            }
        };
        out.push(r);
    }
    out
}

enum Case {
    Pass(String),
    /// A property test's own `requires` rejected the inputs.
    Discard(String),
    Fail {
        message: String,
        span: Option<crate::diag::Span>,
        backtrace: Vec<String>,
        stdout: String,
    },
}

fn run_case(lw: &mut Lowerer, m: crate::mir::MFnId, args: Vec<Value>, seed: u64) -> Case {
    run_case_with(lw, m, args, seed, &[])
}

/// Run one case; a failed check whose span is one of `discard` (the test's own `requires`)
/// discards the case instead of failing it.
fn run_case_with(
    lw: &mut Lowerer,
    m: crate::mir::MFnId,
    args: Vec<Value>,
    seed: u64,
    discard: &[crate::diag::Span],
) -> Case {
    let mut it = Interp::new(lw, Effects::fake(seed));
    it.fuel = 200_000_000;
    let r = it.call(m, args);
    let stdout = std::mem::take(&mut it.fx.stdout);
    match r {
        Ok(_) | Err(Trap::Exit(0)) => Case::Pass(stdout),
        Err(Trap::Exit(c)) => {
            Case::Fail { message: format!("exited with code {c}"), span: None, backtrace: Vec::new(), stdout }
        }
        Err(Trap::Panic { span, .. }) if discard.contains(&span) => Case::Discard(stdout),
        Err(Trap::Panic { msg, span, backtrace }) => Case::Fail { message: msg, span: Some(span), backtrace, stdout },
    }
}

const PROPERTY_CASES: u32 = 100;

/// A property test: generated inputs, discarded when they fail the test's `requires`,
/// and shrunk to a small counterexample when a case fails.
fn run_property(lw: &mut Lowerer, tid: TestId, m: crate::mir::MFnId, params: &[Ty], seed: u64) -> Case {
    let td = &lw.defs.tests[tid];
    let names: Vec<String> = td.params.iter().map(|p| p.name.to_string()).collect();
    let ast = &lw.pkgs[td.pkg].ast;
    let crate::syntax::ast::ItemKind::Test { requires, .. } = ast.items[td.item].kind else {
        return Case::Pass(String::new());
    };
    let discard: Vec<crate::diag::Span> = ast.expr_lists.get(requires).iter().map(|&e| ast.exprs[e].span).collect();
    let mut rng = crate::proptest::Rng::new(seed);
    let (mut ran, mut discarded) = (0u32, 0u32);
    let mut last_stdout = String::new();
    while ran < PROPERTY_CASES && discarded < PROPERTY_CASES * 20 {
        let size = 2 + (ran as u64) * 10;
        let mut args = Vec::new();
        for &t in params {
            match crate::proptest::generate(lw.tys, lw.defs, t, &mut rng, size, 0) {
                Some(v) => args.push(v),
                None => {
                    let ty = lw.tys.display(t);
                    return Case::Fail {
                        message: format!("cannot generate inputs of type `{ty}` for a property test"),
                        span: None,
                        backtrace: Vec::new(),
                        stdout: String::new(),
                    };
                }
            }
        }
        match run_case_with(lw, m, args.clone(), seed ^ ran as u64, &discard) {
            Case::Pass(out) => {
                ran += 1;
                last_stdout = out;
            }
            Case::Discard(_) => discarded += 1,
            Case::Fail { .. } => return shrink_failure(lw, m, params, &names, args, seed, &discard),
        }
    }
    if ran == 0 {
        return Case::Fail {
            message: format!("no generated input satisfied the test's `requires` ({discarded} tried)"),
            span: None,
            backtrace: Vec::new(),
            stdout: String::new(),
        };
    }
    Case::Pass(last_stdout)
}

fn shrink_failure(
    lw: &mut Lowerer,
    m: crate::mir::MFnId,
    params: &[Ty],
    names: &[String],
    mut args: Vec<Value>,
    seed: u64,
    discard: &[crate::diag::Span],
) -> Case {
    let mut fail = run_case_with(lw, m, args.clone(), seed, discard);
    let mut budget = 500;
    'outer: while budget > 0 {
        for i in 0..args.len() {
            for c in crate::proptest::shrink(lw.tys, lw.defs, params[i], &args[i]) {
                budget -= 1;
                if budget == 0 {
                    break 'outer;
                }
                let mut trial = args.clone();
                trial[i] = c;
                let r = run_case_with(lw, m, trial.clone(), seed, discard);
                if let Case::Fail { .. } = r {
                    args = trial;
                    fail = r;
                    continue 'outer;
                }
            }
        }
        break;
    }
    let mut it = Interp::new(lw, Effects::fake(seed));
    let shown: Vec<String> = args
        .iter()
        .zip(params)
        .zip(names)
        .map(|((v, &t), n)| format!("{n} = {}", it.show(v, t, true).unwrap_or_else(|_| "?".into())))
        .collect();
    match fail {
        Case::Fail { message, span, backtrace, stdout } => {
            Case::Fail { message: format!("{message}\n  with inputs: {}", shown.join(", ")), span, backtrace, stdout }
        }
        other => other,
    }
}

pub fn test_result_json(r: &TestResult) -> Json {
    let mut j = Json::obj()
        .with("kind", "test")
        .with("name", r.name.as_str())
        .with("package", r.pkg.as_str())
        .with("passed", r.passed);
    if !r.passed {
        j.set("message", r.message.as_str());
        j.set("location", r.location.as_str());
        if !r.backtrace.is_empty() {
            j.set("backtrace", Json::Arr(r.backtrace.iter().map(|s| Json::from(s.as_str())).collect()));
        }
    }
    if !r.stdout.is_empty() {
        j.set("stdout", r.stdout.as_str());
    }
    j
}

/// Run `main` with faked effects and captured output. Returns (stdout, status).
pub fn run_main_captured(prog: &mut Program, args: Vec<String>, stdin: &str) -> (String, String) {
    let mut lw = Lowerer::new(prog, LowerOpts { check_contracts: true });
    let Some(m) = lw.main() else {
        return (String::new(), "error: no `main` function".into());
    };
    let mut fx = Effects::fake(7);
    fx.args = std::iter::once("main".to_string()).chain(args).collect();
    fx.stdin = stdin.lines().map(|l| l.to_string()).collect();
    let mut it = Interp::new(&mut lw, fx);
    it.fuel = 500_000_000;
    let r = it.call(m, Vec::new());
    let mut out = std::mem::take(&mut it.fx.stdout);
    let err = std::mem::take(&mut it.fx.stderr);
    if !err.is_empty() {
        out.push_str(&err);
    }
    let status = match r {
        Ok(_) => "exit 0".to_string(),
        Err(Trap::Exit(c)) => format!("exit {c}"),
        Err(Trap::Panic { msg, span, .. }) => format!("panic at {}: {msg}", location(lw.sm, span)),
    };
    (out, status)
}

/// Run `main` on the interpreter with real effects. Returns the exit code.
pub fn run_main(prog: &mut Program, args: Vec<String>) -> Result<i32, String> {
    let mut lw = Lowerer::new(prog, LowerOpts { check_contracts: true });
    let Some(m) = lw.main() else {
        return Err("this package has no `main` function".to_string());
    };
    let mut it = Interp::new(&mut lw, Effects::real(args));
    match it.call(m, Vec::new()) {
        Ok(_) => Ok(0),
        Err(Trap::Exit(c)) => Ok(c),
        Err(Trap::Panic { msg, span, backtrace }) => {
            let loc = location(lw.sm, span);
            eprintln!("panic at {loc}: {msg}");
            for b in backtrace.iter().skip(1) {
                eprintln!("  in {b}");
            }
            Ok(101)
        }
    }
}
