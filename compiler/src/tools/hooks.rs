//! Claude Code hook entry points: `jig hook post-edit` and `jig hook pre-commit`. Both read
//! the hook's JSON from stdin, so the plugin needs no other tools (no `jq`).

use crate::util::json::Json;
use std::io::Read;
use std::path::PathBuf;

const MAX_SHOWN: usize = 12;

fn read_input() -> Json {
    let mut s = String::new();
    let _ = std::io::stdin().read_to_string(&mut s);
    Json::parse(&s).unwrap_or(Json::Null)
}

fn cwd_of(input: &Json) -> PathBuf {
    input.get("cwd").and_then(|c| c.as_str()).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// After an edit to a `.jig` file: check the project and feed errors back (exit code 2 shows
/// stderr to the agent).
pub fn post_edit() -> i32 {
    let input = read_input();
    let Some(file) = input.get("tool_input").and_then(|t| t.get("file_path")).and_then(|f| f.as_str()) else {
        return 0;
    };
    if !file.ends_with(".jig") && !file.ends_with(".feature") {
        return 0;
    }
    let path = {
        let p = PathBuf::from(file);
        if p.is_absolute() {
            p
        } else {
            cwd_of(&input).join(p)
        }
    };
    let report = super::project::check_project(&path);
    let errors: Vec<_> = report.diags.iter().filter(|d| d.is_error).collect();
    if errors.is_empty() {
        return 0;
    }
    let mut out = format!(
        "jig check: {} error{} (all Jig lints are errors; fix them before continuing)\n\n",
        errors.len(),
        if errors.len() == 1 { "" } else { "s" }
    );
    for d in errors.iter().take(MAX_SHOWN) {
        out.push_str(&d.text);
        out.push('\n');
    }
    if errors.len() > MAX_SHOWN {
        out.push_str(&format!("... and {} more (run `jig check --json`)\n", errors.len() - MAX_SHOWN));
    }
    eprint!("{out}");
    2
}

fn is_git_commit(cmd: &str) -> bool {
    // `git commit`, `git -C dir commit`, chained with && or ;
    for part in cmd.split(['&', ';', '|', '\n']) {
        let words: Vec<&str> = part.split_whitespace().collect();
        if let Some(i) = words.iter().position(|w| *w == "git") {
            let mut j = i + 1;
            while j < words.len() && words[j].starts_with('-') {
                // skip options and their arguments (`-C dir`, `-c k=v`)
                if matches!(words[j], "-C" | "-c") {
                    j += 1;
                }
                j += 1;
            }
            if words.get(j) == Some(&"commit") {
                return true;
            }
        }
    }
    false
}

fn deny(reason: &str) -> i32 {
    let out = Json::obj().with(
        "hookSpecificOutput",
        Json::obj()
            .with("hookEventName", "PreToolUse")
            .with("permissionDecision", "deny")
            .with("permissionDecisionReason", reason),
    );
    println!("{}", out.to_string_compact());
    0
}

/// Before `git commit`: the project must check, test and format cleanly.
pub fn pre_commit() -> i32 {
    let input = read_input();
    let Some(cmd) = input.get("tool_input").and_then(|t| t.get("command")).and_then(|c| c.as_str()) else {
        return 0;
    };
    if !is_git_commit(cmd) {
        return 0;
    }
    let cwd = cwd_of(&input);
    let dirs = super::project::package_dirs(&cwd);
    if dirs.is_empty() {
        return 0;
    }
    let report = super::project::check_project(&cwd);
    let errors: Vec<_> = report.diags.iter().filter(|d| d.is_error).collect();
    if !errors.is_empty() {
        let mut r = format!(
            "jig check failed with {} error(s); commits are blocked until the project checks cleanly:\n",
            errors.len()
        );
        for d in errors.iter().take(MAX_SHOWN) {
            r.push_str(&d.text);
        }
        return deny(&r);
    }
    // Tests.
    let mut failed = Vec::new();
    for p in
        &report.programs.iter().chain(report.packages.iter()).cloned().collect::<std::collections::BTreeSet<PathBuf>>()
    {
        let mut prog = crate::sema::analyze(&crate::sema::load::Sources::Disk, p);
        if prog.diags.has_errors() {
            continue;
        }
        let results = crate::runner::with_big_stack(move || crate::runner::run_tests(&mut prog, None));
        for r in results.into_iter().filter(|r| !r.passed) {
            failed.push(format!("FAILED {} ({}): {} at {}", r.name, r.pkg, r.message, r.location));
        }
    }
    failed.sort();
    failed.dedup();
    if !failed.is_empty() {
        let mut r = format!("jig test: {} test(s) failed; commits are blocked until tests pass:\n", failed.len());
        for f in failed.iter().take(MAX_SHOWN) {
            r.push_str(f);
            r.push('\n');
        }
        return deny(&r);
    }
    // Formatting.
    let mut unformatted = Vec::new();
    let mut files = Vec::new();
    crate::cli::collect_jig_files(&cwd, &mut files);
    for f in files {
        if let Ok(src) = std::fs::read_to_string(&f) {
            if let Ok(out) = crate::fmt::format_source(&src) {
                if out != src {
                    unformatted.push(crate::diag::display_path(&f));
                }
            }
        }
    }
    if !unformatted.is_empty() {
        return deny(&format!(
            "these files are not formatted; run `jig fmt` and commit again:\n{}",
            unformatted.join("\n")
        ));
    }
    0
}

pub fn main(args: &[String]) -> i32 {
    match args.first().map(|s| s.as_str()) {
        Some("post-edit") => post_edit(),
        Some("pre-commit") => pre_commit(),
        _ => {
            eprintln!("usage: jig hook <post-edit|pre-commit>  (reads Claude Code hook JSON on stdin)");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_git_commit;

    #[test]
    fn detects_commits() {
        assert!(is_git_commit("git commit -m x"));
        assert!(is_git_commit("cd a && git -C b commit -am 'x'"));
        assert!(is_git_commit("git add -A; git commit"));
        assert!(!is_git_commit("git status"));
        assert!(!is_git_commit("echo commit"));
        assert!(!is_git_commit("git log --grep commit"));
    }
}
