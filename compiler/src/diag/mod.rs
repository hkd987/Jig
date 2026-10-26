//! Diagnostics: one structure, two renderings (JSON for agents, text for humans).

pub mod codes;

pub use codes::Code;

use crate::util::json::Json;
use std::fmt::Write;
use std::path::{Path, PathBuf};

/// A byte range in the global source map (offsets are unique across all files).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct Span {
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub const DUMMY: Span = Span { lo: 0, hi: 0 };
    pub fn new(lo: u32, hi: u32) -> Span {
        Span { lo, hi }
    }
    pub fn to(self, other: Span) -> Span {
        Span { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
    pub fn is_dummy(self) -> bool {
        self.lo == 0 && self.hi == 0
    }
    pub fn shrink_to_lo(self) -> Span {
        Span { lo: self.lo, hi: self.lo }
    }
    pub fn shrink_to_hi(self) -> Span {
        Span { lo: self.hi, hi: self.hi }
    }
}

crate::define_idx!(FileId);

pub struct SourceFile {
    /// Path as given on the command line or discovered; used for display.
    pub path: PathBuf,
    pub text: String,
    pub base: u32,
    pub line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn end(&self) -> u32 {
        self.base + self.text.len() as u32
    }
}

#[derive(Default)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
    next_base: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineCol {
    /// 1-based line.
    pub line: u32,
    /// 1-based column, in Unicode scalar values.
    pub col: u32,
}

impl SourceMap {
    pub fn new() -> Self {
        // Offset 0 is reserved for dummy spans.
        SourceMap { files: Vec::new(), next_base: 1 }
    }

    pub fn add(&mut self, path: PathBuf, text: String) -> FileId {
        let base = self.next_base;
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        self.next_base = base + text.len() as u32 + 1;
        self.files.push(SourceFile { path, text, base, line_starts });
        FileId(self.files.len() as u32 - 1)
    }

    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    pub fn file_of(&self, pos: u32) -> Option<FileId> {
        let i = self.files.partition_point(|f| f.base <= pos);
        if i == 0 {
            return None;
        }
        let f = &self.files[i - 1];
        if pos <= f.end() {
            Some(FileId(i as u32 - 1))
        } else {
            None
        }
    }

    pub fn line_col(&self, pos: u32) -> Option<(FileId, LineCol)> {
        let fid = self.file_of(pos)?;
        let f = self.file(fid);
        let off = pos - f.base;
        let line = f.line_starts.partition_point(|&s| s <= off) - 1;
        let line_start = f.line_starts[line] as usize;
        let col = f.text[line_start..off as usize].chars().count() as u32 + 1;
        Some((fid, LineCol { line: line as u32 + 1, col }))
    }

    pub fn snippet(&self, span: Span) -> &str {
        match self.file_of(span.lo) {
            Some(fid) => {
                let f = self.file(fid);
                let lo = (span.lo - f.base) as usize;
                let hi = ((span.hi.max(span.lo)) - f.base) as usize;
                &f.text[lo..hi.min(f.text.len())]
            }
            None => "",
        }
    }

    pub fn line_text(&self, fid: FileId, line: u32) -> &str {
        let f = self.file(fid);
        let i = (line - 1) as usize;
        let start = f.line_starts[i] as usize;
        let end = f.line_starts.get(i + 1).map(|&e| e as usize - 1).unwrap_or(f.text.len());
        f.text[start..end].trim_end_matches('\r')
    }

    pub fn display_path(&self, fid: FileId) -> String {
        display_path(&self.file(fid).path)
    }
}

pub fn display_path(p: &Path) -> String {
    if let Ok(cwd) = std::env::current_dir() {
        if let Ok(rel) = p.strip_prefix(&cwd) {
            return rel.display().to_string();
        }
    }
    p.display().to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Severity {
    Error,
    /// Informational findings, e.g. an obligation the prover could not settle that falls back
    /// to testing. Never fails a build on its own.
    Info,
}

#[derive(Clone, Debug)]
pub struct Label {
    pub span: Span,
    pub message: String,
}

#[derive(Clone, Debug)]
pub struct Edit {
    pub span: Span,
    pub replacement: String,
}

#[derive(Clone, Debug)]
pub struct Fix {
    pub message: String,
    pub edits: Vec<Edit>,
}

#[derive(Clone, Debug)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    pub message: String,
    pub span: Span,
    pub label: Option<String>,
    pub labels: Vec<Label>,
    pub notes: Vec<String>,
    pub fixes: Vec<Fix>,
    /// Concrete values (name, value) that make the failure happen.
    pub counterexample: Vec<(String, String)>,
}

impl Diagnostic {
    pub fn error(code: Code, span: Span, message: impl Into<String>) -> Diagnostic {
        Diagnostic {
            code,
            severity: Severity::Error,
            message: message.into(),
            span,
            label: None,
            labels: Vec::new(),
            notes: Vec::new(),
            fixes: Vec::new(),
            counterexample: Vec::new(),
        }
    }
    pub fn info(code: Code, span: Span, message: impl Into<String>) -> Diagnostic {
        let mut d = Diagnostic::error(code, span, message);
        d.severity = Severity::Info;
        d
    }
    /// Text shown under the primary span.
    pub fn primary(mut self, msg: impl Into<String>) -> Self {
        self.label = Some(msg.into());
        self
    }
    pub fn label(mut self, span: Span, msg: impl Into<String>) -> Self {
        self.labels.push(Label { span, message: msg.into() });
        self
    }
    pub fn note(mut self, msg: impl Into<String>) -> Self {
        self.notes.push(msg.into());
        self
    }
    pub fn fix(mut self, msg: impl Into<String>, span: Span, replacement: impl Into<String>) -> Self {
        self.fixes.push(Fix { message: msg.into(), edits: vec![Edit { span, replacement: replacement.into() }] });
        self
    }
    pub fn fix_edits(mut self, msg: impl Into<String>, edits: Vec<Edit>) -> Self {
        self.fixes.push(Fix { message: msg.into(), edits });
        self
    }
    pub fn counterexample(mut self, vals: Vec<(String, String)>) -> Self {
        self.counterexample = vals;
        self
    }
    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    pub fn to_json(&self, sm: &SourceMap) -> Json {
        let mut o = Json::obj()
            .with("code", self.code.name())
            .with(
                "severity",
                match self.severity {
                    Severity::Error => "error",
                    Severity::Info => "info",
                },
            )
            .with("message", self.message.as_str());
        span_json(sm, self.span, &mut o);
        if let Some(l) = &self.label {
            o.set("label", l.as_str());
        }
        if !self.labels.is_empty() {
            let mut arr = Json::arr();
            for l in &self.labels {
                let mut lj = Json::obj().with("message", l.message.as_str());
                span_json(sm, l.span, &mut lj);
                arr.push(lj);
            }
            o.set("labels", arr);
        }
        if !self.notes.is_empty() {
            o.set("notes", Json::Arr(self.notes.iter().map(|n| Json::from(n.as_str())).collect()));
        }
        if !self.fixes.is_empty() {
            let mut arr = Json::arr();
            for f in &self.fixes {
                let mut edits = Json::arr();
                for e in &f.edits {
                    let mut ej = Json::obj().with("replacement", e.replacement.as_str());
                    span_json(sm, e.span, &mut ej);
                    edits.push(ej);
                }
                arr.push(Json::obj().with("message", f.message.as_str()).with("edits", edits));
            }
            o.set("fixes", arr);
        }
        if !self.counterexample.is_empty() {
            let mut ce = Json::obj();
            for (k, v) in &self.counterexample {
                ce.set(k, v.as_str());
            }
            o.set("counterexample", ce);
        }
        o
    }

    pub fn render(&self, sm: &SourceMap, color: bool) -> String {
        let mut out = String::new();
        let (sev, sev_color) = match self.severity {
            Severity::Error => ("error", "\x1b[1;31m"),
            Severity::Info => ("info", "\x1b[1;36m"),
        };
        let (bold, reset, blue) = if color { ("\x1b[1m", "\x1b[0m", "\x1b[1;34m") } else { ("", "", "") };
        let sevc = if color { sev_color } else { "" };
        let _ = writeln!(out, "{sevc}{sev}[{}]{reset}{bold}: {}{reset}", self.code.name(), self.message);
        let mut spans: Vec<(Span, Option<&str>, bool)> = Vec::new();
        if !self.span.is_dummy() {
            spans.push((self.span, self.label.as_deref(), true));
        }
        for l in &self.labels {
            spans.push((l.span, Some(l.message.as_str()), false));
        }
        let mut gutter = 2;
        for (s, _, _) in &spans {
            if let Some((_, lc)) = sm.line_col(s.lo) {
                gutter = gutter.max(lc.line.to_string().len() + 1);
            }
        }
        let mut first = true;
        for (span, label, primary) in spans {
            let Some((fid, lc)) = sm.line_col(span.lo) else { continue };
            if first {
                let _ = writeln!(
                    out,
                    "{:w$}{blue}-->{reset} {}:{}:{}",
                    "",
                    sm.display_path(fid),
                    lc.line,
                    lc.col,
                    w = gutter - 1
                );
                let _ = writeln!(out, "{:w$}{blue}|{reset}", "", w = gutter);
                first = false;
            } else if sm.line_col(span.lo).map(|x| x.0) != sm.line_col(self.span.lo).map(|x| x.0) {
                let _ = writeln!(
                    out,
                    "{:w$}{blue}::{reset} {}:{}:{}",
                    "",
                    sm.display_path(fid),
                    lc.line,
                    lc.col,
                    w = gutter - 1
                );
            }
            let line = sm.line_text(fid, lc.line);
            let _ = writeln!(out, "{blue}{:>w$} |{reset} {}", lc.line, line, w = gutter - 1);
            let end = sm.line_col(span.hi).map(|(_, e)| e).unwrap_or(lc);
            let width = if end.line == lc.line {
                (end.col.saturating_sub(lc.col)).max(1)
            } else {
                (line.chars().count() as u32 + 1).saturating_sub(lc.col).max(1)
            };
            let marker = if primary { '^' } else { '-' };
            let mc = if color {
                if primary {
                    sev_color
                } else {
                    blue
                }
            } else {
                ""
            };
            let _ = writeln!(
                out,
                "{:w$}{blue}|{reset} {:pad$}{mc}{}{}{reset}",
                "",
                "",
                std::iter::repeat_n(marker, width as usize).collect::<String>(),
                label.map(|l| format!(" {l}")).unwrap_or_default(),
                w = gutter,
                pad = (lc.col - 1) as usize
            );
        }
        if !self.counterexample.is_empty() {
            let vals: Vec<String> = self.counterexample.iter().map(|(k, v)| format!("{k} = {v}")).collect();
            let _ = writeln!(out, "{:w$}{blue}={reset} counterexample: {}", "", vals.join(", "), w = gutter);
        }
        for n in &self.notes {
            let _ = writeln!(out, "{:w$}{blue}={reset} note: {}", "", n, w = gutter);
        }
        for f in &self.fixes {
            let detail = match f.edits.as_slice() {
                [e] if !e.replacement.is_empty() && !e.replacement.contains('\n') => {
                    format!(": `{}`", e.replacement)
                }
                _ => String::new(),
            };
            let _ = writeln!(out, "{:w$}{blue}={reset} fix: {}{}", "", f.message, detail, w = gutter);
        }
        out
    }
}

fn span_json(sm: &SourceMap, span: Span, o: &mut Json) {
    if span.is_dummy() {
        return;
    }
    if let (Some((fid, a)), Some((_, b))) = (sm.line_col(span.lo), sm.line_col(span.hi)) {
        let base = sm.file(fid).base;
        o.set("file", sm.display_path(fid));
        o.set(
            "span",
            Json::obj()
                .with("line", a.line)
                .with("col", a.col)
                .with("end_line", b.line)
                .with("end_col", b.col)
                .with("byte_start", span.lo - base)
                .with("byte_end", span.hi - base),
        );
    }
}

/// Collects diagnostics for a compilation.
#[derive(Default)]
pub struct Diags {
    pub list: Vec<Diagnostic>,
}

impl Diags {
    pub fn push(&mut self, d: Diagnostic) {
        self.list.push(d);
    }
    pub fn has_errors(&self) -> bool {
        self.list.iter().any(|d| d.is_error())
    }
    pub fn error_count(&self) -> usize {
        self.list.iter().filter(|d| d.is_error()).count()
    }
    pub fn extend(&mut self, other: Diags) {
        self.list.extend(other.list);
    }
    /// Sort by position, and drop exact duplicates.
    pub fn sort(&mut self) {
        self.list.sort_by_key(|d| (d.span.lo, d.code.0));
        self.list.dedup_by(|a, b| a.span == b.span && a.code == b.code && a.message == b.message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_snippet() {
        let mut sm = SourceMap::new();
        let f = sm.add("a.jig".into(), "fn main() {\n  let x: Int = \"hi\"\n}\n".into());
        let base = sm.file(f).base;
        let lo = base + 27;
        let d = Diagnostic::error(codes::E0301, Span::new(lo, lo + 4), "mismatched types")
            .primary("expected `Int`, found `Str`");
        let s = d.render(&sm, false);
        assert!(s.contains("error[E0301]: mismatched types"), "{s}");
        assert!(s.contains("a.jig:2:16"), "{s}");
        assert!(s.contains("^^^^ expected `Int`"), "{s}");
        let j = d.to_json(&sm).to_string_compact();
        assert!(j.contains("\"line\":2"), "{j}");
    }
}
