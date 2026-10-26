//! `jig mcp`: a Model Context Protocol server over stdio (newline-delimited JSON-RPC).
//!
//! Tools: check, test, prove, trace, run, fmt, explain, search, reference. Paths are relative to the
//! server's working directory (the project).

use crate::util::json::Json;
use std::io::{BufRead, Write};
use std::path::PathBuf;

const PROTOCOL: &str = "2025-11-25";

fn tool(name: &str, description: &str, props: Json, required: &[&str]) -> Json {
    Json::obj().with("name", name).with("description", description).with(
        "inputSchema",
        Json::obj()
            .with("type", "object")
            .with("properties", props)
            .with("required", Json::Arr(required.iter().map(|r| Json::from(*r)).collect())),
    )
}

fn prop(ty: &str, desc: &str) -> Json {
    Json::obj().with("type", ty).with("description", desc)
}

fn tools() -> Json {
    Json::Arr(vec![
        tool(
            "check",
            "Type-check a Jig project: parse, types, effects, lints and dead code (all errors). Returns every diagnostic with its stable code, location, message and candidate fixes. Run after every edit.",
            Json::obj().with("path", prop("string", "A file or package directory; the whole project containing it is checked. Defaults to the working directory.")),
            &[],
        ),
        tool(
            "test",
            "Run the `test` blocks of a Jig package in the deterministic runner (console, clock, randomness and files are faked). Returns each test's result, failure message and location.",
            Json::obj()
                .with("path", prop("string", "Package directory. Defaults to the working directory."))
                .with("filter", prop("string", "Only run tests whose name contains this text.")),
            &[],
        ),
        tool(
            "run",
            "Run a Jig program's `main` on the interpreter with faked effects and return its output. Use it to see what a program prints for a given input.",
            Json::obj()
                .with("path", prop("string", "Package directory with `fn main`."))
                .with("stdin", prop("string", "Text for standard input."))
                .with("args", Json::obj().with("type", "array").with("items", Json::obj().with("type", "string")).with("description", "Command-line arguments.")),
            &[],
        ),
        tool(
            "prove",
            "Prove a Jig project's contracts (requires, ensures, invariants, refinements, asserts) and the absence of runtime traps (overflow, division by zero, bounds, unwrap) with an SMT solver, one function at a time against its callees' contracts. Results are cached per function. Returns each failed or unknown obligation with a counterexample in source terms and a hint; proven ones are only counted.",
            Json::obj()
                .with("path", prop("string", "A file or package directory; the whole project is proved. Defaults to the working directory."))
                .with("filter", prop("string", "Only functions whose name contains this text."))
                .with("all", Json::obj().with("type", "boolean").with("description", "Also list unproven safety obligations (traps that keep their runtime check).")),
            &[],
        ),
        tool(
            "trace",
            "Traceability between scenarios (.feature files) and code: with no arguments, coverage per scenario and how much reachable code no scenario exercises; `function` lists the scenarios exercising matching functions; `scenario` lists what a scenario exercises and what becomes dead if it is deleted; `uncovered` lists reachable functions no scenario exercises.",
            Json::obj()
                .with("path", prop("string", "Package directory. Defaults to the working directory."))
                .with("function", prop("string", "Function name (or part of one) to look up."))
                .with("scenario", prop("string", "Scenario name (or part of one) to look up."))
                .with("uncovered", Json::obj().with("type", "boolean").with("description", "List the reachable functions no scenario exercises.")),
            &[],
        ),
        tool(
            "fmt",
            "Format Jig files in place with the canonical formatter (no options). Returns the files that changed.",
            Json::obj().with("path", prop("string", "File or directory. Defaults to the working directory.")),
            &[],
        ),
        tool(
            "explain",
            "Explain a Jig diagnostic code (for example E0301): what it means and how to fix it.",
            Json::obj().with("code", prop("string", "The code, e.g. E0401.")),
            &["code"],
        ),
        tool(
            "search",
            "Find functions and methods by type signature, e.g. `List<T> -> Map<K, List<T>>` or `Str -> Option<Int>`. Single capital letters are type variables; a method's receiver is its first parameter. Searches the standard library and the project.",
            Json::obj()
                .with("query", prop("string", "Parameter types separated by commas, then `->` and the result type."))
                .with("path", prop("string", "Package directory whose code is also searched.")),
            &["query"],
        ),
        tool(
            "reference",
            "Return the complete Jig language reference (syntax, types, effects, contracts, tests, standard library).",
            Json::obj(),
            &[],
        ),
    ])
}

fn text_result(text: String, structured: Option<Json>, is_error: bool) -> Json {
    let mut r = Json::obj()
        .with("content", Json::Arr(vec![Json::obj().with("type", "text").with("text", text)]))
        .with("isError", is_error);
    if let Some(s) = structured {
        r.set("structuredContent", s);
    }
    r
}

fn arg_path(args: &Json) -> PathBuf {
    args.get("path")
        .and_then(|p| p.as_str())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn pkg_dir(p: PathBuf) -> PathBuf {
    if p.is_file() {
        p.parent().map(|x| x.to_path_buf()).unwrap_or_else(|| PathBuf::from("."))
    } else {
        p
    }
}

pub fn call_tool(name: &str, args: &Json) -> Json {
    match name {
        "check" => {
            let report = super::project::check_project(&arg_path(args));
            let n = report.errors();
            let mut text = if n == 0 {
                "ok: no errors\n".to_string()
            } else {
                format!("{n} error{}\n\n", if n == 1 { "" } else { "s" })
            };
            for d in report.diags.iter().take(40) {
                text.push_str(&d.text);
                text.push('\n');
            }
            let structured = Json::obj()
                .with("errors", n)
                .with("diagnostics", Json::Arr(report.diags.iter().map(|d| d.json.clone()).collect()));
            text_result(text, Some(structured), false)
        }
        "prove" => {
            let dir = pkg_dir(arg_path(args));
            let filter = args.get("filter").and_then(|f| f.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string());
            let all = args.get("all").and_then(|a| a.as_bool()).unwrap_or(false);
            let mut prog = crate::sema::analyze(&crate::sema::load::Sources::Disk, &dir);
            if prog.diags.has_errors() {
                return text_result(
                    "the project does not check; run `check` and fix the errors first".into(),
                    None,
                    true,
                );
            }
            let root =
                crate::sema::load::find_project_root(&crate::sema::load::Sources::Disk, &dir).unwrap_or(dir.clone());
            let opts = crate::verify::ProveOpts { cache_dir: Some(root.join(".jig/proofs")), timeout_ms: 2000, filter };
            match crate::verify::prove(&mut prog, &opts) {
                Ok(r) => {
                    let text = crate::verify::render(&prog, &r, all);
                    let obs: Vec<Json> = r
                        .fns
                        .iter()
                        .flat_map(|f| f.obligations.iter().map(move |o| (f, o)))
                        .filter(|(_, o)| o.status != crate::verify::Status::Proved && (all || o.kind.is_contract()))
                        .map(|(f, o)| crate::verify::obligation_json(&prog, f, o))
                        .collect();
                    let (cp, ct, sp, st) = r.counts();
                    let structured = Json::obj()
                        .with("contracts_proved", cp as i64)
                        .with("contracts", ct as i64)
                        .with("safety_proved", sp as i64)
                        .with("safety", st as i64)
                        .with("obligations", Json::Arr(obs));
                    text_result(text, Some(structured), false)
                }
                Err(e) => text_result(format!("error: {e}"), None, true),
            }
        }
        "trace" => {
            let dir = pkg_dir(arg_path(args));
            let prog = crate::sema::analyze(&crate::sema::load::Sources::Disk, &dir);
            if prog.diags.has_errors() {
                return text_result("the project does not check; run `check` first".into(), None, true);
            }
            let s = |k: &str| args.get(k).and_then(|v| v.as_str()).filter(|v| !v.is_empty()).map(|v| v.to_string());
            use crate::tools::trace::Query;
            let q = if let Some(f) = s("function") {
                Query::Function(f)
            } else if let Some(sc) = s("scenario") {
                Query::Scenario(sc)
            } else if args.get("uncovered").and_then(|u| u.as_bool()).unwrap_or(false) {
                Query::Uncovered
            } else {
                Query::Summary
            };
            let (text, json) = crate::tools::trace::run(&prog, &q);
            text_result(text, Some(Json::obj().with("result", json)), false)
        }
        "test" => {
            let dir = pkg_dir(arg_path(args));
            let filter = args.get("filter").and_then(|f| f.as_str()).map(|s| s.to_string());
            let mut prog = crate::sema::analyze(&crate::sema::load::Sources::Disk, &dir);
            if prog.diags.has_errors() {
                let text: String = prog
                    .diags
                    .list
                    .iter()
                    .filter(|d| d.is_error())
                    .take(20)
                    .map(|d| d.render(&prog.sm, false))
                    .collect();
                return text_result(format!("the package does not check; fix these first:\n\n{text}"), None, true);
            }
            let results = crate::runner::with_big_stack(move || crate::runner::run_tests(&mut prog, filter.as_deref()));
            let failed = results.iter().filter(|r| !r.passed).count();
            let mut text = format!("{} passed, {failed} failed\n", results.len() - failed);
            for r in &results {
                if r.passed {
                    text.push_str(&format!("ok      {}\n", r.name));
                } else {
                    text.push_str(&format!(
                        "FAILED  {}\n        {}\n        at {}\n",
                        r.name,
                        r.message.replace('\n', "\n        "),
                        r.location
                    ));
                }
            }
            let structured = Json::obj()
                .with("passed", results.len() - failed)
                .with("failed", failed)
                .with("results", Json::Arr(results.iter().map(crate::runner::test_result_json).collect()));
            text_result(text, Some(structured), false)
        }
        "run" => {
            let dir = pkg_dir(arg_path(args));
            let mut prog = crate::sema::analyze(&crate::sema::load::Sources::Disk, &dir);
            if prog.diags.has_errors() {
                let text: String = prog
                    .diags
                    .list
                    .iter()
                    .filter(|d| d.is_error())
                    .take(20)
                    .map(|d| d.render(&prog.sm, false))
                    .collect();
                return text_result(format!("the program does not check; fix these first:\n\n{text}"), None, true);
            }
            let stdin = args.get("stdin").and_then(|s| s.as_str()).unwrap_or("").to_string();
            let argv: Vec<String> = args
                .get("args")
                .and_then(|a| a.as_arr())
                .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                .unwrap_or_default();
            let (out, status) =
                crate::runner::with_big_stack(move || crate::runner::run_main_captured(&mut prog, argv, &stdin));
            text_result(
                format!("{out}\n[{status}]"),
                Some(Json::obj().with("stdout", out.as_str()).with("status", status.as_str())),
                false,
            )
        }
        "fmt" => {
            let mut files = Vec::new();
            crate::cli::collect_jig_files(&arg_path(args), &mut files);
            let mut changed = Vec::new();
            let mut errors = Vec::new();
            for f in files {
                let Ok(src) = std::fs::read_to_string(&f) else { continue };
                match crate::fmt::format_source(&src) {
                    Ok(out) if out != src => {
                        if std::fs::write(&f, out).is_ok() {
                            changed.push(crate::diag::display_path(&f));
                        }
                    }
                    Ok(_) => {}
                    Err(e) => errors.push(format!("{}: {e}", f.display())),
                }
            }
            let mut text = if changed.is_empty() {
                "already formatted\n".to_string()
            } else {
                format!("formatted:\n{}\n", changed.join("\n"))
            };
            for e in &errors {
                text.push_str(e);
            }
            text_result(
                text,
                Some(Json::obj().with("changed", Json::Arr(changed.iter().map(|c| Json::from(c.as_str())).collect()))),
                !errors.is_empty(),
            )
        }
        "explain" => {
            let code = args.get("code").and_then(|c| c.as_str()).unwrap_or("");
            match crate::diag::Code::parse(code).and_then(|c| c.info().map(|i| (c, i))) {
                Some((c, i)) => {
                    text_result(format!("{} ({}): {}\n\n{}", c.name(), i.slug, i.summary, i.explain), None, false)
                }
                None => text_result(format!("unknown diagnostic code `{code}`"), None, true),
            }
        }
        "search" => {
            let q = args.get("query").and_then(|c| c.as_str()).unwrap_or("");
            let dir = pkg_dir(arg_path(args));
            let prog = crate::sema::analyze(&crate::sema::load::Sources::Disk, &dir);
            match super::search::search(&prog, q, 15) {
                Ok(hits) => {
                    let mut text = String::new();
                    for h in &hits {
                        text.push_str(&format!("{}{} [{}]\n", h.name, h.signature, h.package));
                    }
                    if hits.is_empty() {
                        text.push_str("no matches\n");
                    }
                    let s = Json::Arr(
                        hits.iter()
                            .map(|h| {
                                Json::obj()
                                    .with("name", h.name.as_str())
                                    .with("signature", h.signature.as_str())
                                    .with("package", h.package.as_str())
                            })
                            .collect(),
                    );
                    text_result(text, Some(Json::obj().with("matches", s)), false)
                }
                Err(e) => text_result(e, None, true),
            }
        }
        "reference" => text_result(REFERENCE.to_string(), None, false),
        _ => text_result(format!("unknown tool `{name}`"), None, true),
    }
}

pub const REFERENCE: &str = include_str!("../../../docs/reference.md");

fn respond(out: &mut impl Write, id: &Json, result: Json) {
    let msg = Json::obj().with("jsonrpc", "2.0").with("id", id.clone()).with("result", result);
    let _ = writeln!(out, "{}", msg.to_string_compact());
    let _ = out.flush();
}

fn respond_err(out: &mut impl Write, id: &Json, code: i64, message: &str) {
    let msg = Json::obj()
        .with("jsonrpc", "2.0")
        .with("id", id.clone())
        .with("error", Json::obj().with("code", code).with("message", message));
    let _ = writeln!(out, "{}", msg.to_string_compact());
    let _ = out.flush();
}

pub fn serve() -> i32 {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let msg = match Json::parse(&line) {
            Ok(m) => m,
            Err(e) => {
                respond_err(&mut out, &Json::Null, -32700, &format!("parse error: {e}"));
                continue;
            }
        };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Json::obj());
        let Some(id) = id else {
            // Notifications (`notifications/initialized`, cancellations) need no reply.
            continue;
        };
        match method {
            "initialize" => {
                let requested = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(PROTOCOL);
                let version = if requested.is_empty() { PROTOCOL } else { requested };
                respond(
                    &mut out,
                    &id,
                    Json::obj()
                        .with("protocolVersion", version)
                        .with("capabilities", Json::obj().with("tools", Json::obj()))
                        .with("serverInfo", Json::obj().with("name", "jig").with("version", env!("CARGO_PKG_VERSION")))
                        .with("instructions", "Jig toolchain. Run `check` after every edit; all lints are errors. `reference` returns the language reference; `search` finds functions by type signature; `explain` explains a diagnostic code."),
                );
            }
            "ping" => respond(&mut out, &id, Json::obj()),
            "tools/list" => respond(&mut out, &id, Json::obj().with("tools", tools())),
            "tools/call" => {
                let name = params.get("name").and_then(|n| n.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(Json::obj());
                if !["check", "test", "prove", "trace", "run", "fmt", "explain", "search", "reference"].contains(&name)
                {
                    respond_err(&mut out, &id, -32602, &format!("unknown tool: {name}"));
                    continue;
                }
                let name = name.to_string();
                let result = std::panic::catch_unwind(move || call_tool(&name, &args))
                    .unwrap_or_else(|_| text_result("internal compiler error".into(), None, true));
                respond(&mut out, &id, result);
            }
            _ => respond_err(&mut out, &id, -32601, &format!("method not found: {method}")),
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_tools_and_explains() {
        let t = tools();
        assert!(t.as_arr().unwrap().len() >= 7);
        let r = call_tool("explain", &Json::obj().with("code", "E0301"));
        let text = r.get("content").unwrap().as_arr().unwrap()[0].get("text").unwrap().as_str().unwrap().to_string();
        assert!(text.contains("mismatched types"));
        let r = call_tool("reference", &Json::obj());
        assert_eq!(r.get("isError").unwrap().as_bool(), Some(false));
    }
}
