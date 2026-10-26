//! Running programs and tests on the interpreter.

use crate::diag::Diagnostic;
use crate::mir::interp::{Effects, Interp, Trap};
use crate::mir::lower::Lowerer;
use crate::mir::LowerOpts;
use crate::sema::defs::{Program, TestId};
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
        std::thread::Builder::new().stack_size(1 << 30).spawn_scoped(s, f).expect("spawn interpreter thread").join().expect("interpreter thread panicked")
    })
}

pub fn run_tests(prog: &mut Program, filter: Option<&str>) -> Vec<TestResult> {
    let ids: Vec<TestId> = prog
        .defs
        .tests
        .iter_enumerated()
        .filter(|(_, t)| !prog.pkgs[t.pkg].is_std)
        .filter(|(_, t)| filter.is_none_or(|f| t.label.as_str().contains(f)))
        .map(|(i, _)| i)
        .collect();
    let labels: Vec<(String, String)> = ids
        .iter()
        .map(|&t| (prog.defs.tests[t].label.to_string(), prog.pkgs[prog.defs.tests[t].pkg].path.to_string()))
        .collect();
    let mut lw = Lowerer::new(prog, LowerOpts { check_contracts: true });
    let mut out = Vec::new();
    for (i, &tid) in ids.iter().enumerate() {
        let m = lw.test_fn(tid);
        let mut it = Interp::new(&mut lw, Effects::fake(0x9e37_79b9 ^ (i as u64 + 1)));
        it.fuel = 200_000_000;
        let r = it.call(m, Vec::new());
        let stdout = std::mem::take(&mut it.fx.stdout);
        let (name, pkg) = labels[i].clone();
        match r {
            Ok(_) | Err(Trap::Exit(0)) => out.push(TestResult { name, pkg, passed: true, message: String::new(), location: String::new(), stdout, backtrace: Vec::new() }),
            Err(Trap::Exit(c)) => out.push(TestResult { name, pkg, passed: false, message: format!("exited with code {c}"), location: String::new(), stdout, backtrace: Vec::new() }),
            Err(Trap::Panic { msg, span, backtrace }) => {
                let location = location(lw.sm, span);
                out.push(TestResult { name, pkg, passed: false, message: msg, location, stdout, backtrace })
            }
        }
    }
    out
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

/// Run `main` on the interpreter with real effects. Returns the exit code.
pub fn run_main(prog: &mut Program, args: Vec<String>) -> Result<i32, Diagnostic> {
    let mut lw = Lowerer::new(prog, LowerOpts { check_contracts: true });
    let Some(m) = lw.main() else {
        return Err(Diagnostic::error(crate::diag::codes::E0318, crate::diag::Span::DUMMY, "this package has no `main` function"));
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
