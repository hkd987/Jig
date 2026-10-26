//! Command-line entry point.

use crate::diag::Diagnostic;
use crate::sema::load::Sources;
use std::io::Write;
use std::path::PathBuf;

const USAGE: &str = "usage: jig <command> [path] [options]

commands:
  check [--json]          parse, types, effects, lints and dead code for the project
  test [filter] [--json]  run tests in the deterministic runner
  prove [filter] [--json] [--verbose]  prove contracts and safety with an SMT solver (cached)
  run [--native] [-- args]  run main (interpreter, or a native debug build)
  build [--release] [-o out]  compile main to a native executable (C; LLVM via clang for release)
  trace [fn <name> | scenario <name> | uncovered] [--json]  which scenarios exercise which code
  fmt [--check]           format .jig files in place
  explain <code>          explain a diagnostic code
  search <signature>      find functions by type, e.g. 'List<T> -> Option<T>'
  reference               print the language reference
  mcp                     serve the agent tools over MCP (stdio)
  hook <name>             Claude Code hook entry points (post-edit, pre-commit)";

struct Opts {
    json: bool,
    path: PathBuf,
    rest: Vec<String>,
}

fn parse_opts(args: &[String]) -> Opts {
    let mut json = false;
    let mut path = None;
    let mut rest = Vec::new();
    for a in args {
        match a.as_str() {
            "--json" => json = true,
            s if s.starts_with("--") => rest.push(s.to_string()),
            s => {
                if path.is_none() && std::path::Path::new(s).exists() {
                    path = Some(PathBuf::from(s));
                } else {
                    rest.push(s.to_string());
                }
            }
        }
    }
    let mut path = path.unwrap_or_else(|| PathBuf::from("."));
    if path.is_file() {
        path = path.parent().map(|p| p.to_path_buf()).unwrap_or_else(|| PathBuf::from("."));
        if path.as_os_str().is_empty() {
            path = PathBuf::from(".");
        }
    }
    Opts { json, path, rest }
}

pub fn print_diags(prog: &crate::sema::defs::Program, diags: &[Diagnostic], json: bool) {
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let color = !json && std::env::var_os("NO_COLOR").is_none() && is_tty();
    for d in diags {
        if json {
            let _ = writeln!(out, "{}", d.to_json(&prog.sm).to_string_compact());
        } else {
            let _ = write!(out, "{}", d.render(&prog.sm, color));
            let _ = writeln!(out);
        }
    }
}

fn is_tty() -> bool {
    std::env::var_os("TERM").is_some_and(|t| t != "dumb") && std::env::var_os("JIG_COLOR").is_some()
}

fn cmd_check(args: &[String]) -> i32 {
    let o = parse_opts(args);
    let report = crate::tools::project::check_project(&o.path);
    let errors = report.errors();
    for d in &report.diags {
        if o.json {
            println!("{}", d.json.to_string_compact());
        } else {
            println!("{}", d.text);
        }
    }
    if !o.json {
        if errors == 0 {
            eprintln!("ok: no errors");
        } else {
            eprintln!("{errors} error{}", if errors == 1 { "" } else { "s" });
        }
    }
    let _ = &o.rest;
    if errors == 0 {
        0
    } else {
        1
    }
}

fn split_dashdash(args: &[String]) -> (Vec<String>, Vec<String>) {
    match args.iter().position(|a| a == "--") {
        Some(i) => (args[..i].to_vec(), args[i + 1..].to_vec()),
        None => (args.to_vec(), Vec::new()),
    }
}

fn analyze_or_report(o: &Opts) -> Option<crate::sema::defs::Program> {
    let prog = crate::sema::analyze(&Sources::Disk, &o.path);
    if prog.diags.has_errors() {
        print_diags(&prog, &prog.diags.list, o.json);
        let n = prog.diags.error_count();
        if !o.json {
            eprintln!("{n} error{}", if n == 1 { "" } else { "s" });
        }
        return None;
    }
    Some(prog)
}

fn cmd_run(args: &[String]) -> i32 {
    let (ours, prog_args) = split_dashdash(args);
    let o = parse_opts(&ours);
    let Some(mut prog) = analyze_or_report(&o) else { return 1 };
    if o.rest.iter().any(|a| a == "--native") {
        let exe = match native_build(&o, &mut prog, false, None) {
            Ok(exe) => exe,
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        };
        return match std::process::Command::new(&exe).args(&prog_args).status() {
            Ok(s) => s.code().unwrap_or(101),
            Err(e) => {
                eprintln!("error: cannot run {}: {e}", exe.display());
                1
            }
        };
    }
    let mut argv = vec![o.path.display().to_string()];
    argv.extend(prog_args);
    crate::runner::with_big_stack(move || match crate::runner::run_main(&mut prog, argv) {
        Ok(code) => code,
        Err(msg) => {
            eprintln!("error: {msg}");
            1
        }
    })
}

/// Remove `-o <path>` from the arguments.
fn take_out(args: &[String]) -> (Vec<String>, Option<PathBuf>) {
    let mut rest = Vec::new();
    let mut out = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "-o" {
            out = it.next().map(PathBuf::from);
        } else {
            rest.push(a.clone());
        }
    }
    (rest, out)
}

fn native_build(
    o: &Opts,
    prog: &mut crate::sema::defs::Program,
    release: bool,
    out: Option<PathBuf>,
) -> Result<PathBuf, String> {
    let root = crate::sema::load::find_project_root(&Sources::Disk, &o.path).unwrap_or_else(|| o.path.clone());
    let mode = if release { "release" } else { "debug" };
    let name = std::fs::canonicalize(&o.path)
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()))
        .unwrap_or_else(|| "main".into());
    let build_dir = root.join(".jig").join("build").join(mode);
    let out = out.unwrap_or_else(|| build_dir.join(&name));
    // Proofs from `jig prove` that still match the code remove their runtime checks.
    let proven = crate::verify::cached_proven(prog, &root.join(".jig").join("proofs"));
    if !proven.is_empty() && std::env::var_os("JIG_QUIET").is_none() {
        eprintln!(
            "using proofs: {} contract checks and {} arithmetic checks removed",
            proven.contract.len(),
            proven.safety.len()
        );
    }
    crate::codegen::build::build(prog, &crate::codegen::build::BuildOpts { release, build_dir, out, proven })
}

fn cmd_build(args: &[String]) -> i32 {
    let (args, out) = take_out(args);
    let o = parse_opts(&args);
    let release = o.rest.iter().any(|a| a == "--release");
    let Some(mut prog) = analyze_or_report(&o) else { return 1 };
    match native_build(&o, &mut prog, release, out) {
        Ok(exe) => {
            println!("{}", exe.display());
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

fn proof_cache_dir(o: &Opts) -> PathBuf {
    let root = crate::sema::load::find_project_root(&Sources::Disk, &o.path).unwrap_or_else(|| o.path.clone());
    root.join(".jig").join("proofs")
}

fn cmd_prove(args: &[String]) -> i32 {
    let o = parse_opts(args);
    let verbose = o.rest.iter().any(|a| a == "--verbose");
    let no_cache = o.rest.iter().any(|a| a == "--no-cache");
    let filter = o.rest.iter().find(|a| !a.starts_with("--")).cloned();
    let Some(mut prog) = analyze_or_report(&o) else { return 1 };
    let opts = crate::verify::ProveOpts {
        cache_dir: if no_cache { None } else { Some(proof_cache_dir(&o)) },
        timeout_ms: 2000,
        filter,
    };
    let report = match crate::verify::prove(&mut prog, &opts) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    if o.json {
        for f in &report.fns {
            for ob in &f.obligations {
                println!("{}", crate::verify::obligation_json(&prog, f, ob).to_string_compact());
            }
        }
        let (cp, ct, sp, stot) = report.counts();
        let summary = crate::util::json::Json::obj()
            .with("kind", "summary")
            .with("contracts_proved", cp as i64)
            .with("contracts", ct as i64)
            .with("safety_proved", sp as i64)
            .with("safety", stot as i64)
            .with("functions", report.fns.len() as i64)
            .with("cached", report.fns.iter().filter(|f| f.cached).count() as i64)
            .with("millis", report.millis as i64);
        println!("{}", summary.to_string_compact());
    } else {
        print!("{}", crate::verify::render(&prog, &report, verbose));
    }
    if report.failed_contracts() > 0 {
        1
    } else {
        0
    }
}

fn cmd_trace(args: &[String]) -> i32 {
    let o = parse_opts(args);
    let words: Vec<&str> = o.rest.iter().filter(|a| !a.starts_with("--")).map(|s| s.as_str()).collect();
    let q = match words.as_slice() {
        [] => crate::tools::trace::Query::Summary,
        ["uncovered"] => crate::tools::trace::Query::Uncovered,
        ["fn", name @ ..] if !name.is_empty() => crate::tools::trace::Query::Function(name.join(" ")),
        ["scenario", name @ ..] if !name.is_empty() => crate::tools::trace::Query::Scenario(name.join(" ")),
        _ => {
            eprintln!("usage: jig trace [fn <name> | scenario <name> | uncovered] [--json]");
            return 2;
        }
    };
    let Some(prog) = analyze_or_report(&o) else { return 1 };
    let (text, json) = crate::tools::trace::run(&prog, &q);
    if o.json {
        println!("{}", json.to_string_compact());
    } else {
        print!("{text}");
    }
    0
}

fn cmd_test(args: &[String]) -> i32 {
    let o = parse_opts(args);
    let filter = o.rest.iter().find(|a| !a.starts_with("--")).cloned();
    let Some(mut prog) = analyze_or_report(&o) else { return 1 };
    let json = o.json;
    let start = std::time::Instant::now();
    let results = crate::runner::with_big_stack(move || crate::runner::run_tests(&mut prog, filter.as_deref()));
    let failed = results.iter().filter(|r| !r.passed).count();
    for r in &results {
        if json {
            println!("{}", crate::runner::test_result_json(r).to_string_compact());
        } else if r.passed {
            println!("ok      {} ({})", r.name, r.pkg);
        } else {
            println!("FAILED  {} ({})", r.name, r.pkg);
            println!("        {}", r.message.replace('\n', "\n        "));
            if !r.location.is_empty() {
                println!("        at {}", r.location);
            }
            if !r.stdout.is_empty() {
                println!("        stdout: {:?}", r.stdout);
            }
        }
    }
    if !json {
        eprintln!("{} passed, {failed} failed ({} ms)", results.len() - failed, start.elapsed().as_millis());
    } else {
        println!(
            "{}",
            crate::util::json::Json::obj()
                .with("kind", "summary")
                .with("passed", results.len() - failed)
                .with("failed", failed)
                .to_string_compact()
        );
    }
    if failed == 0 {
        0
    } else {
        1
    }
}

fn cmd_fmt(args: &[String]) -> i32 {
    let check = args.iter().any(|a| a == "--check");
    let mut files: Vec<PathBuf> = Vec::new();
    let targets: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let roots: Vec<PathBuf> =
        if targets.is_empty() { vec![PathBuf::from(".")] } else { targets.iter().map(PathBuf::from).collect() };
    for r in roots {
        collect_jig_files(&r, &mut files);
    }
    let mut changed = 0;
    let mut failed = 0;
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            eprintln!("error: cannot read {}", f.display());
            failed += 1;
            continue;
        };
        match crate::fmt::format_source(&src) {
            Ok(out) => {
                if out != src {
                    changed += 1;
                    if check {
                        println!("{}", f.display());
                    } else if std::fs::write(f, out).is_err() {
                        eprintln!("error: cannot write {}", f.display());
                        failed += 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("{}: cannot format a file that does not parse:\n{e}", f.display());
                failed += 1;
            }
        }
    }
    if check && changed > 0 {
        eprintln!("{changed} file{} not formatted", if changed == 1 { "" } else { "s" });
        return 1;
    }
    if !check && changed > 0 {
        eprintln!("formatted {changed} file{}", if changed == 1 { "" } else { "s" });
    }
    if failed > 0 {
        1
    } else {
        0
    }
}

/// `.jig` files under a path (a file, or a directory searched recursively, skipping hidden
/// directories and build output).
pub fn collect_jig_files(p: &std::path::Path, out: &mut Vec<PathBuf>) {
    if p.is_file() {
        if p.extension().is_some_and(|e| e == "jig") {
            out.push(p.to_path_buf());
        }
        return;
    }
    let Ok(rd) = std::fs::read_dir(p) else { return };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok()).map(|e| e.path()).collect();
    entries.sort();
    for e in entries {
        let name = e.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.starts_with('.') || name == "target" {
            continue;
        }
        collect_jig_files(&e, out);
    }
}

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
    match args.first().map(|s| s.as_str()) {
        Some("check") => cmd_check(rest),
        Some("run") => cmd_run(rest),
        Some("build") => cmd_build(rest),
        Some("prove") => cmd_prove(rest),
        Some("trace") => cmd_trace(rest),
        Some("test") => cmd_test(rest),
        Some("fmt") => cmd_fmt(rest),
        Some("mcp") => crate::tools::mcp::serve(),
        Some("hook") => crate::tools::hooks::main(rest),
        Some("search") => {
            let o = parse_opts(rest);
            let q = o.rest.join(" ");
            let prog = crate::sema::analyze(&Sources::Disk, &o.path);
            match crate::tools::search::search(&prog, &q, 20) {
                Ok(hits) => {
                    for h in hits {
                        println!("{}{}  [{}]", h.name, h.signature, h.package);
                    }
                    0
                }
                Err(e) => {
                    eprintln!("{e}");
                    2
                }
            }
        }
        Some("reference") => {
            print!("{}", crate::tools::mcp::REFERENCE);
            0
        }
        Some("--version") | Some("version") => {
            println!("jig {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some("explain") => {
            let Some(code) = args.get(1).and_then(|c| crate::diag::Code::parse(c)) else {
                eprintln!("usage: jig explain <code>");
                return 2;
            };
            let info = code.info().unwrap();
            println!("{} ({}): {}\n\n{}", code.name(), info.slug, info.summary, info.explain);
            0
        }
        _ => {
            eprintln!("{USAGE}");
            2
        }
    }
}
