//! The canonical formatter behind `jig fmt`: one layout for every program, no options.
//!
//! Formatting is two passes over one parsed file:
//!
//! 1. [`comments`] gives every comment a *slot* in a *context*. A context is a list that
//!    prints one element per line when broken: the file's items, a block's statements, a
//!    match's arms, struct fields, call arguments, parameters, a function's contract clauses.
//!    A comment between two elements leads the next one, or trails the previous one when it
//!    shares its line; a comment inside an element that no nested context claims is hoisted
//!    to lead that element. Every comment gets exactly one slot.
//! 2. The printer walks the AST. A node takes its flat (single-line) layout when that fits in
//!    [`WIDTH`] columns and it has no comments to place, and a broken layout otherwise. Every
//!    context that is printed emits the comments of its slots.
//!
//! Layout functions return strings whose first line starts at the caller's column and whose
//! later lines carry their own indentation. They take `ind`, the indentation of the line the
//! node starts on (continuation lines and nested blocks indent from it), `col`, the column the
//! node starts at, and `trail`, the width of the text that follows the node on its last line.

mod comments;
mod expr;
mod items;
#[cfg(test)]
mod tests;

use crate::diag::{SourceMap, Span};
use crate::syntax::ast::{Ast, ExprId, FileAst, ItemId};
use crate::syntax::lexer::{self, Comment};
use crate::syntax::parser;
use crate::util::intern::LocalInterner;
use crate::util::{FxHashMap, FxHashSet};

/// Target line width.
const WIDTH: usize = 100;
/// Flat layouts are abandoned once they grow past this many bytes: they cannot fit.
const FLAT_LIMIT: usize = 4 * WIDTH;

/// Format one parsed file. `file_index` indexes `ast.files`; `src` is that file's text and
/// `base` its global start offset (spans and comment offsets are global: subtract `base`).
pub fn format_file(ast: &Ast, file_index: usize, src: &str, base: u32) -> String {
    format_file_width(ast, file_index, src, base, WIDTH)
}

/// Convenience: parse `src` and format it; returns Err(rendered diagnostics) if it doesn't parse.
pub fn format_source(src: &str) -> Result<String, String> {
    format_source_width(src, WIDTH)
}

/// [`format_source`] for another target width; tests use narrow widths to exercise every
/// broken layout.
fn format_source_width(src: &str, width: usize) -> Result<String, String> {
    let mut sm = SourceMap::new();
    let fid = sm.add("<input>".into(), src.to_string());
    let base = sm.file(fid).base;
    let mut ast = Ast::default();
    let mut interner = LocalInterner::default();
    let out = parser::parse_file(&mut ast, fid, src, base, &mut interner);
    if !out.diags.is_empty() {
        return Err(out.diags.iter().map(|d| d.render(&sm, false)).collect());
    }
    Ok(format_file_width(&ast, ast.files.len() - 1, src, base, width))
}

fn format_file_width(ast: &Ast, file_index: usize, src: &str, base: u32, width: usize) -> String {
    let file = &ast.files[file_index];
    let mut p = Printer {
        ast,
        file,
        src,
        base,
        width,
        comments: &file.comments,
        claimed: vec![false; file.comments.len()],
        slots: FxHashMap::default(),
        has: FxHashSet::default(),
    };
    p.claim_file();
    let out = tidy(&p.print_file());
    // Never lose a comment: a mismatch is a formatter bug, and the source is left as it was.
    let mut before: Vec<&str> = (0..file.comments.len() as u32).map(|ci| p.comment(ci)).collect();
    before.sort_unstable();
    let kept = comment_texts(&out) == before;
    debug_assert!(kept, "the formatter lost or duplicated a comment:\n{out}");
    if kept {
        out
    } else {
        src.to_string()
    }
}

/// Strip trailing whitespace and end with exactly one newline (none for an empty file).
fn tidy(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 1);
    for line in s.lines() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    let end = out.trim_end_matches('\n').len();
    out.truncate(end);
    if !out.is_empty() {
        out.push('\n');
    }
    out
}

/// The sorted texts of the comments in `src`.
fn comment_texts(src: &str) -> Vec<&str> {
    let mut v: Vec<&str> =
        lexer::lex(src, 0).comments.iter().map(|c| src.get(c.lo as usize..c.hi as usize).unwrap_or("")).collect();
    v.sort_unstable();
    v
}

/// A list whose elements print one per line when broken, and so can hold comments.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Key {
    /// The file's items.
    File,
    /// Struct fields, enum variants, interface methods, or function/test parameters.
    Item(ItemId),
    /// A function's `requires`/`ensures`/`effects` clauses.
    Clauses(ItemId),
    /// Block statements, match arms, struct literal fields, call arguments, list and tuple
    /// elements, or closure parameters.
    Expr(ExprId),
}

/// Where in a context a comment prints. Elements are named by their start offset.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum Slot {
    /// At the end of the opening line (`{ // ...`).
    Open,
    /// On its own line before the element.
    Lead(u32),
    /// At the end of the element's last line.
    Trail(u32),
    /// On its own line before the closing delimiter.
    End,
}

/// One element of a context, already rendered at the list's indentation.
struct LineElem {
    lo: u32,
    hi: u32,
    text: String,
}

/// Which blank lines a list keeps.
#[derive(Clone, Copy)]
enum Blanks<'k> {
    /// At most one where the author left one or more.
    Keep,
    Never,
    /// File items: one between items, except between consecutive single-line items of the same
    /// kind (`Some(kind)`), which keep the author's choice.
    Items(&'k [Option<u8>]),
}

struct Printer<'a> {
    ast: &'a Ast,
    file: &'a FileAst,
    src: &'a str,
    base: u32,
    /// Target line width: [`WIDTH`] except in tests.
    width: usize,
    comments: &'a [Comment],
    claimed: Vec<bool>,
    /// Comment indices per slot, in source order.
    slots: FxHashMap<(Key, Slot), Vec<u32>>,
    /// Contexts holding at least one comment: these never print flat.
    has: FxHashSet<Key>,
}

impl<'a> Printer<'a> {
    fn text(&self, lo: u32, hi: u32) -> &'a str {
        let lo = lo.saturating_sub(self.base) as usize;
        let hi = hi.saturating_sub(self.base) as usize;
        self.src.get(lo..hi.max(lo)).unwrap_or("")
    }

    fn span_text(&self, s: Span) -> &'a str {
        self.text(s.lo, s.hi)
    }

    fn comment(&self, ci: u32) -> &'a str {
        let c = self.comments[ci as usize];
        self.text(c.lo, c.hi)
    }

    fn slot(&self, key: Key, slot: Slot) -> &[u32] {
        self.slots.get(&(key, slot)).map(|v| v.as_slice()).unwrap_or(&[])
    }

    fn has(&self, key: Key) -> bool {
        self.has.contains(&key)
    }

    /// Did the author leave a blank line between offsets `a` and `b`?
    fn blank_between(&self, a: u32, b: u32) -> bool {
        let bl = &self.file.blank_lines;
        let i = bl.partition_point(|&x| x <= a);
        i < bl.len() && bl[i] <= b
    }

    fn same_line(&self, a: u32, b: u32) -> bool {
        !self.text(a, b).contains('\n')
    }

    fn byte(&self, pos: u32) -> u8 {
        self.src.as_bytes().get(pos.saturating_sub(self.base) as usize).copied().unwrap_or(0)
    }

    /// The offset of the first byte at or after `pos` that is not whitespace, a comment, or
    /// (if `commas`) a comma.
    fn skip_trivia(&self, pos: u32, commas: bool) -> u32 {
        let s = self.src.as_bytes();
        let mut i = pos.saturating_sub(self.base) as usize;
        while i < s.len() {
            match s[i] {
                b' ' | b'\t' | b'\r' | b'\n' => i += 1,
                b',' if commas => i += 1,
                b'/' if s.get(i + 1) == Some(&b'/') => {
                    while i < s.len() && s[i] != b'\n' {
                        i += 1;
                    }
                }
                _ => break,
            }
        }
        self.base + i as u32
    }

    /// The offset of the first `ch` at or after `pos`, outside comments.
    fn find_byte(&self, pos: u32, ch: u8) -> Option<u32> {
        let s = self.src.as_bytes();
        let mut i = pos.saturating_sub(self.base) as usize;
        while i < s.len() {
            if s[i] == b'/' && s.get(i + 1) == Some(&b'/') {
                while i < s.len() && s[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if s[i] == ch {
                return Some(self.base + i as u32);
            }
            i += 1;
        }
        None
    }

    /// The range `(` .. `)` of a parenthesized list opening at `lp` whose last element ends at
    /// `last_hi`.
    fn paren_range(&self, lp: u32, last_hi: Option<u32>) -> (u32, u32) {
        let after = last_hi.unwrap_or(lp + 1);
        let rp = self.skip_trivia(after, true);
        if self.byte(rp) == b')' {
            (lp, rp + 1)
        } else {
            (lp, after)
        }
    }

    /// Print a context's elements one per line at indentation `ind`, with their comments and
    /// blank lines; no newline before the first line or after the last.
    fn line_list(&self, key: Key, elems: &[LineElem], ind: usize, blanks: Blanks) -> String {
        let mut out = String::new();
        let mut prev_end = None;
        let mut prev_elem = None;
        for (i, el) in elems.iter().enumerate() {
            let mut between = prev_elem.map(|p| (p, i));
            for &ci in self.slot(key, Slot::Lead(el.lo)) {
                let c = self.comments[ci as usize];
                // A hoisted comment (from inside the element) stands where the element starts.
                self.separate(&mut out, prev_end, c.lo.min(el.lo), between.take(), blanks);
                expr::indent(&mut out, ind);
                out.push_str(self.comment(ci));
                prev_end = Some(c.hi);
            }
            self.separate(&mut out, prev_end, el.lo, between.take(), blanks);
            expr::indent(&mut out, ind);
            out.push_str(&el.text);
            prev_end = Some(el.hi);
            for &ci in self.slot(key, Slot::Trail(el.lo)) {
                out.push(' ');
                out.push_str(self.comment(ci));
                prev_end = Some(self.comments[ci as usize].hi);
            }
            prev_elem = Some(i);
        }
        for &ci in self.slot(key, Slot::End) {
            let c = self.comments[ci as usize];
            self.separate(&mut out, prev_end, c.lo, None, blanks);
            expr::indent(&mut out, ind);
            out.push_str(self.comment(ci));
            prev_end = Some(c.hi);
        }
        out
    }

    /// The line break before a unit starting at `lo`, plus a blank line where wanted.
    /// `between` is `(previous element, next element)` for the first unit of an element.
    fn separate(
        &self,
        out: &mut String,
        prev_end: Option<u32>,
        lo: u32,
        between: Option<(usize, usize)>,
        blanks: Blanks,
    ) {
        let Some(pe) = prev_end else { return };
        out.push('\n');
        let author = self.blank_between(pe, lo);
        let blank = match blanks {
            Blanks::Keep => author,
            Blanks::Never => false,
            Blanks::Items(kinds) => match between {
                Some((p, n)) => match (kinds[p], kinds[n]) {
                    (Some(a), Some(b)) if a == b => author,
                    _ => true,
                },
                None => author,
            },
        };
        if blank {
            out.push('\n');
        }
    }

    /// `open`, the opening comment, the elements one per line at `ind + 2`, and `close` on its
    /// own line at `ind`; just `open close` joined when there is nothing inside.
    fn wrapped(&self, open: &str, close: &str, key: Key, elems: &[LineElem], ind: usize, blanks: Blanks) -> String {
        let mut s = String::from(open);
        let opening = self.slot(key, Slot::Open);
        for &ci in opening {
            s.push(' ');
            s.push_str(self.comment(ci));
        }
        let body = self.line_list(key, elems, ind + 2, blanks);
        if body.is_empty() && opening.is_empty() {
            s.push_str(close);
            return s;
        }
        if !body.is_empty() {
            s.push('\n');
            s.push_str(&body);
        }
        expr::newline(&mut s, ind);
        s.push_str(close);
        s
    }

    /// A brace-delimited context: `{`, one element per line, `}`; `{}` when empty.
    fn braced(&self, key: Key, elems: &[LineElem], ind: usize) -> String {
        self.wrapped("{", "}", key, elems, ind, Blanks::Keep)
    }
}
