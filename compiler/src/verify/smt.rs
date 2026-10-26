//! A solver process speaking SMT-LIB 2 over pipes. Only standard commands are used, so any
//! SMT-LIB solver could stand in for Z3 (D1).

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sat {
    Sat,
    Unsat,
    Unknown,
}

pub struct Solver {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// The last `(error ...)` the solver printed, if any.
    pub last_error: Option<String>,
}

/// The solver command: `$JIG_Z3`, else `z3` on the path.
pub fn solver_path() -> String {
    std::env::var("JIG_Z3").unwrap_or_else(|_| "z3".to_string())
}

/// The solver's version line, part of every proof cache key.
pub fn solver_version() -> Option<String> {
    let out = Command::new(solver_path()).arg("--version").output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

impl Solver {
    pub fn start(timeout_ms: u32) -> Result<Solver, String> {
        let mut child = Command::new(solver_path())
            .args(["-in", "-smt2"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start the SMT solver `{}`: {e} (install z3 or set JIG_Z3)", solver_path()))?;
        let stdin = child.stdin.take().ok_or("no solver stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no solver stdout")?);
        let mut s = Solver { child, stdin, stdout, last_error: None };
        s.send("(set-option :produce-models true)");
        s.send(&format!("(set-option :timeout {timeout_ms})"));
        s.send("(set-logic ALL)");
        Ok(s)
    }

    pub fn send(&mut self, cmd: &str) {
        let _ = self.stdin.write_all(cmd.as_bytes());
        let _ = self.stdin.write_all(b"\n");
    }

    /// Read one complete s-expression or atom from the solver.
    fn read_response(&mut self) -> Option<String> {
        let mut out = String::new();
        let mut depth = 0i32;
        let mut in_str = false;
        loop {
            let mut line = String::new();
            if self.stdout.read_line(&mut line).ok()? == 0 {
                return if out.is_empty() { None } else { Some(out) };
            }
            for c in line.chars() {
                match c {
                    '"' => in_str = !in_str,
                    '(' if !in_str => depth += 1,
                    ')' if !in_str => depth -= 1,
                    _ => {}
                }
            }
            out.push_str(&line);
            if depth <= 0 && !out.trim().is_empty() {
                return Some(out.trim().to_string());
            }
        }
    }

    pub fn check(&mut self) -> Sat {
        self.send("(check-sat)");
        let _ = self.stdin.flush();
        loop {
            let Some(r) = self.read_response() else { return Sat::Unknown };
            match r.as_str() {
                "sat" => return Sat::Sat,
                "unsat" => return Sat::Unsat,
                "unknown" | "timeout" => return Sat::Unknown,
                _ if r.starts_with("(error") => self.last_error = Some(r),
                _ => {}
            }
        }
    }

    /// Model values for terms, after a `sat` answer.
    pub fn values(&mut self, terms: &[String]) -> Vec<Option<Sexp>> {
        if terms.is_empty() {
            return Vec::new();
        }
        self.send(&format!("(get-value ({}))", terms.join(" ")));
        let _ = self.stdin.flush();
        let Some(r) = self.read_response() else { return vec![None; terms.len()] };
        if r.starts_with("(error") {
            self.last_error = Some(r);
            return vec![None; terms.len()];
        }
        let Some(Sexp::List(pairs)) = parse(&r) else { return vec![None; terms.len()] };
        let mut out: Vec<Option<Sexp>> = pairs
            .into_iter()
            .map(|p| match p {
                Sexp::List(mut kv) if kv.len() == 2 => kv.pop(),
                _ => None,
            })
            .collect();
        out.resize(terms.len(), None);
        out
    }
}

impl Drop for Solver {
    fn drop(&mut self) {
        self.send("(exit)");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Sexp {
    Atom(String),
    List(Vec<Sexp>),
}

impl Sexp {
    /// An integer value (`5`, `(- 5)`).
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Sexp::Atom(a) => a.parse().ok(),
            Sexp::List(l) if l.len() == 2 && l[0] == Sexp::Atom("-".into()) => l[1].as_int().map(|v| -v),
            _ => None,
        }
    }
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Sexp::Atom(a) if a == "true" => Some(true),
            Sexp::Atom(a) if a == "false" => Some(false),
            _ => None,
        }
    }
}

pub fn parse(s: &str) -> Option<Sexp> {
    let toks = tokenize(s);
    let mut pos = 0;
    let e = parse_at(&toks, &mut pos)?;
    Some(e)
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '(' | ')' => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
                out.push(c.to_string());
            }
            '"' => {
                cur.push(c);
                for d in chars.by_ref() {
                    cur.push(d);
                    if d == '"' {
                        break;
                    }
                }
            }
            '|' => {
                cur.push(c);
                for d in chars.by_ref() {
                    cur.push(d);
                    if d == '|' {
                        break;
                    }
                }
            }
            c if c.is_whitespace() => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn parse_at(toks: &[String], pos: &mut usize) -> Option<Sexp> {
    let t = toks.get(*pos)?;
    *pos += 1;
    if t == "(" {
        let mut items = Vec::new();
        while toks.get(*pos).map(|s| s.as_str()) != Some(")") {
            items.push(parse_at(toks, pos)?);
        }
        *pos += 1;
        Some(Sexp::List(items))
    } else {
        Some(Sexp::Atom(t.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_model_values() {
        let s = parse("((x 5) (y (- 3)) (b true))").unwrap();
        let Sexp::List(items) = s else { panic!() };
        let vals: Vec<Sexp> = items
            .into_iter()
            .map(|p| match p {
                Sexp::List(mut kv) => kv.pop().unwrap(),
                _ => panic!(),
            })
            .collect();
        assert_eq!(vals[0].as_int(), Some(5));
        assert_eq!(vals[1].as_int(), Some(-3));
        assert_eq!(vals[2].as_bool(), Some(true));
    }
}
