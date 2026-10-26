//! Command-line entry point.

use crate::diag::Diagnostic;
use crate::sema::load::Sources;
use std::io::Write;
use std::path::PathBuf;

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
    let prog = crate::sema::analyze(&Sources::Disk, &o.path);
    let errors = prog.diags.error_count();
    print_diags(&prog, &prog.diags.list, o.json);
    if !o.json {
        if errors == 0 {
            eprintln!("ok: no errors");
        } else {
            eprintln!("{errors} error{}", if errors == 1 { "" } else { "s" });
        }
    }
    let _ = &o.rest;
    if errors == 0 { 0 } else { 1 }
}

pub fn main() -> i32 {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = if args.len() > 1 { &args[1..] } else { &[][..] };
    match args.first().map(|s| s.as_str()) {
        Some("check") => cmd_check(rest),
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
            eprintln!("usage: jig <check|explain> [path] [--json]");
            2
        }
    }
}
