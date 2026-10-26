//! Features: the Gherkin subset (`Feature`, `Scenario`, `Scenario Outline` with `Examples`,
//! `Given`/`When`/`Then`/`And`/`But`), and binding each step line to a `step` function at
//! compile time with typed placeholders.

pub mod bind;

use crate::diag::{codes, Diagnostic, Span};

#[derive(Clone, Debug)]
pub struct Feature {
    pub name: String,
    pub span: Span,
    pub scenarios: Vec<ScenarioAst>,
}

#[derive(Clone, Debug)]
pub struct ScenarioAst {
    pub name: String,
    pub span: Span,
    pub steps: Vec<StepLine>,
    /// `Scenario Outline`: header and rows of its `Examples` table.
    pub examples: Option<Examples>,
}

/// An `Examples` table: the header, and each row's cells with the row's span.
pub type Examples = (Vec<String>, Vec<(Vec<String>, Span)>);

#[derive(Clone, Debug)]
pub struct StepLine {
    /// `Given`, `When`, `Then`, `And` or `But`.
    pub keyword: String,
    pub text: String,
    /// The whole line, and the text after the keyword.
    pub span: Span,
    pub text_span: Span,
}

const STEP_KEYWORDS: [&str; 5] = ["Given", "When", "Then", "And", "But"];

fn table_row(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    inner.split('|').map(|c| c.trim().to_string()).collect()
}

/// Parse one `.feature` file. `base` is the file's offset in the source map.
pub fn parse(src: &str, base: u32) -> (Option<Feature>, Vec<Diagnostic>) {
    let mut diags = Vec::new();
    let mut feature: Option<Feature> = None;
    let mut cur: Option<ScenarioAst> = None;
    let mut in_examples = false;
    let mut offset = 0u32;
    let finish = |cur: &mut Option<ScenarioAst>, feature: &mut Option<Feature>, diags: &mut Vec<Diagnostic>| {
        if let Some(s) = cur.take() {
            if s.steps.is_empty() {
                diags.push(Diagnostic::error(codes::E0801, s.span, format!("scenario `{}` has no steps", s.name)));
            }
            if let Some((_, rows)) = &s.examples {
                if rows.is_empty() {
                    diags.push(Diagnostic::error(codes::E0801, s.span, "the `Examples` table has no rows"));
                }
            } else if s.name.is_empty() {
                diags.push(Diagnostic::error(codes::E0801, s.span, "a scenario needs a name"));
            }
            if let Some(f) = feature.as_mut() {
                f.scenarios.push(s);
            }
        }
    };
    for raw in src.split_inclusive('\n') {
        let line_lo = base + offset;
        offset += raw.len() as u32;
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        let indent = (line.len() - trimmed.len()) as u32;
        let trimmed = trimmed.trim_end();
        let span = Span::new(line_lo + indent, line_lo + indent + trimmed.len() as u32);
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if trimmed.starts_with('@') {
            diags.push(Diagnostic::error(codes::E0801, span, "tags are not supported"));
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix("Feature:") {
            if feature.is_some() {
                diags.push(Diagnostic::error(codes::E0801, span, "one `Feature:` per file"));
                continue;
            }
            feature = Some(Feature { name: rest.trim().to_string(), span, scenarios: Vec::new() });
            continue;
        }
        if trimmed.starts_with("Background:") || trimmed.starts_with("Rule:") {
            let what = trimmed.split(':').next().unwrap_or("");
            diags.push(
                Diagnostic::error(codes::E0801, span, format!("`{what}` is not supported"))
                    .note("put shared setup in the world's `new()` or in an explicit first step"),
            );
            continue;
        }
        let outline = trimmed.strip_prefix("Scenario Outline:");
        if let Some(rest) = outline.or_else(|| trimmed.strip_prefix("Scenario:")) {
            if feature.is_none() {
                diags.push(Diagnostic::error(codes::E0801, span, "a scenario must follow `Feature:`"));
                continue;
            }
            finish(&mut cur, &mut feature, &mut diags);
            in_examples = false;
            cur = Some(ScenarioAst {
                name: rest.trim().to_string(),
                span,
                steps: Vec::new(),
                examples: outline.map(|_| (Vec::new(), Vec::new())),
            });
            continue;
        }
        if trimmed.starts_with("Examples:") {
            match cur.as_mut() {
                Some(s) if s.examples.is_some() => in_examples = true,
                _ => diags.push(Diagnostic::error(codes::E0801, span, "`Examples:` belongs to a `Scenario Outline:`")),
            }
            continue;
        }
        if trimmed.starts_with('|') {
            match cur.as_mut() {
                Some(ScenarioAst { examples: Some((header, rows)), .. }) if in_examples => {
                    let cells = table_row(trimmed);
                    if header.is_empty() {
                        *header = cells;
                    } else if cells.len() != header.len() {
                        diags.push(Diagnostic::error(
                            codes::E0801,
                            span,
                            format!("this row has {} cells; the header has {}", cells.len(), header.len()),
                        ));
                    } else {
                        rows.push((cells, span));
                    }
                }
                _ => diags.push(
                    Diagnostic::error(codes::E0801, span, "data tables on steps are not supported")
                        .note("pass values in the step text, or use a `Scenario Outline` with `Examples`"),
                ),
            }
            continue;
        }
        if trimmed.starts_with("\"\"\"") {
            diags.push(Diagnostic::error(codes::E0801, span, "doc strings are not supported"));
            continue;
        }
        let kw = STEP_KEYWORDS
            .iter()
            .find(|k| trimmed.strip_prefix(**k).is_some_and(|r| r.starts_with(' ') || r.is_empty()));
        if let Some(kw) = kw {
            let Some(s) = cur.as_mut() else {
                diags.push(Diagnostic::error(codes::E0801, span, "a step must be inside a scenario"));
                continue;
            };
            if in_examples {
                diags.push(Diagnostic::error(codes::E0801, span, "steps go before the `Examples:` table"));
                continue;
            }
            let after = &trimmed[kw.len()..];
            let text = after.trim();
            let text_lo = span.lo + kw.len() as u32 + (after.len() - after.trim_start().len()) as u32;
            s.steps.push(StepLine {
                keyword: kw.to_string(),
                text: text.to_string(),
                span,
                text_span: Span::new(text_lo, text_lo + text.len() as u32),
            });
            continue;
        }
        // Free text: a description, allowed after `Feature:` before the first scenario.
        if feature.is_some() && cur.is_none() {
            continue;
        }
        diags.push(
            Diagnostic::error(codes::E0801, span, format!("expected a step, found `{}`", trimmed))
                .note("steps start with `Given`, `When`, `Then`, `And` or `But`"),
        );
    }
    finish(&mut cur, &mut feature, &mut diags);
    if feature.is_none() {
        diags.push(Diagnostic::error(codes::E0801, Span::new(base, base), "a feature file starts with `Feature:`"));
    }
    (feature, diags)
}

/// A compiled step pattern: literal text and `{name}` placeholders.
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Lit(String),
    Hole(String),
}

pub fn compile_pattern(p: &str) -> Result<Vec<Piece>, String> {
    let mut out = Vec::new();
    let mut lit = String::new();
    let mut chars = p.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '{' {
            let mut name = String::new();
            loop {
                match chars.next() {
                    Some('}') => break,
                    Some(c) if c.is_ascii_alphanumeric() || c == '_' => name.push(c),
                    _ => return Err(format!("unterminated or invalid placeholder in `{p}`")),
                }
            }
            if name.is_empty() {
                return Err("empty placeholder `{}`".into());
            }
            if matches!(out.last(), Some(Piece::Hole(_))) && lit.is_empty() {
                return Err("two placeholders need text between them".into());
            }
            if !lit.is_empty() {
                out.push(Piece::Lit(std::mem::take(&mut lit)));
            }
            out.push(Piece::Hole(name));
        } else {
            lit.push(c);
        }
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Ok(out)
}

/// Match step text against a pattern; returns the placeholder texts in order. A placeholder
/// takes a quoted string (`"A b"`, quotes included) or a run of non-space characters.
pub fn match_pattern(pieces: &[Piece], text: &str) -> Option<Vec<String>> {
    fn go(pieces: &[Piece], text: &str, out: &mut Vec<String>) -> bool {
        match pieces.first() {
            None => text.is_empty(),
            Some(Piece::Lit(l)) => text.strip_prefix(l.as_str()).is_some_and(|rest| go(&pieces[1..], rest, out)),
            Some(Piece::Hole(_)) => {
                let mut ends = Vec::new();
                if text.starts_with('"') {
                    let mut esc = false;
                    for (i, c) in text.char_indices().skip(1) {
                        if esc {
                            esc = false;
                        } else if c == '\\' {
                            esc = true;
                        } else if c == '"' {
                            ends.push(i + 1);
                            break;
                        }
                    }
                } else {
                    let word = text.find(char::is_whitespace).unwrap_or(text.len());
                    // Longest first, so `{n}` in `{n} items` takes all of `12`.
                    let mut i = word;
                    while i > 0 {
                        ends.push(i);
                        i -= 1;
                        while i > 0 && !text.is_char_boundary(i) {
                            i -= 1;
                        }
                    }
                }
                for e in ends {
                    out.push(text[..e].to_string());
                    if go(&pieces[1..], &text[e..], out) {
                        return true;
                    }
                    out.pop();
                }
                false
            }
        }
    }
    let mut out = Vec::new();
    go(pieces, text, &mut out).then_some(out)
}

/// A suggested pattern for unmatched step text: quoted strings and numbers become
/// placeholders.
pub fn suggest(text: &str) -> (String, Vec<(String, &'static str)>) {
    let mut pat = String::new();
    let mut params = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if rest.starts_with('"') {
            if let Some(end) = rest[1..].find('"') {
                let name = format!("s{}", params.len() + 1);
                pat.push_str(&format!("{{{name}}}"));
                params.push((name, "Str"));
                rest = &rest[end + 2..];
                continue;
            }
        }
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        if !word.is_empty() && (word.parse::<i64>().is_ok() || word.parse::<f64>().is_ok()) {
            let name = format!("n{}", params.len() + 1);
            let ty = if word.parse::<i64>().is_ok() { "Int" } else { "Float" };
            pat.push_str(&format!("{{{name}}}"));
            params.push((name, ty));
            rest = &rest[word_end..];
            continue;
        }
        let c = rest.chars().next().unwrap();
        pat.push(c);
        rest = &rest[c.len_utf8()..];
    }
    (pat, params)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scenarios_and_outlines() {
        let src = "Feature: Transfers\n  Money moves.\n\n  Scenario: move\n    Given an account \"A\" with balance 100\n    When \"A\" transfers 40 to \"B\"\n    Then \"A\" has balance 60\n\n  Scenario Outline: many\n    Given an account \"A\" with balance <start>\n    Then \"A\" has balance <start>\n    Examples:\n      | start |\n      | 1     |\n      | 2     |\n";
        let (f, d) = parse(src, 0);
        assert!(d.is_empty(), "{d:?}");
        let f = f.unwrap();
        assert_eq!(f.name, "Transfers");
        assert_eq!(f.scenarios.len(), 2);
        assert_eq!(f.scenarios[0].steps[1].keyword, "When");
        assert_eq!(f.scenarios[0].steps[1].text, "\"A\" transfers 40 to \"B\"");
        let (h, rows) = f.scenarios[1].examples.as_ref().unwrap();
        assert_eq!(h, &vec!["start".to_string()]);
        assert_eq!(rows.len(), 2);
        let s = &f.scenarios[0].steps[0];
        assert_eq!(&src[s.text_span.lo as usize..s.text_span.hi as usize], s.text);
    }

    #[test]
    fn rejects_unsupported_gherkin() {
        let (_, d) = parse("Feature: X\n  Background:\n    Given a\n  @slow\n  Scenario: s\n    Given a\n", 0);
        assert_eq!(d.len(), 2);
    }

    #[test]
    fn matches_typed_placeholders() {
        let p = compile_pattern("an account {name} with balance {amt}").unwrap();
        assert_eq!(
            match_pattern(&p, "an account \"A b\" with balance 100"),
            Some(vec!["\"A b\"".into(), "100".into()])
        );
        assert_eq!(match_pattern(&p, "an account \"A\" with balance"), None);
        let q = compile_pattern("{n}% off").unwrap();
        assert_eq!(match_pattern(&q, "10% off"), Some(vec!["10".into()]));
        assert_eq!(suggest("\"A\" pays 40 to \"B\"").0, "{s1} pays {n2} to {s3}");
    }
}
