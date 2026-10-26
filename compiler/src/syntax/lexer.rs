//! Hand-written lexer with Go-style automatic statement termination.
//!
//! A line break ends a statement when the last token on the line can end one
//! (`TokenKind::ends_statement`), except inside `(...)` and `[...]`, and except when the next
//! line continues the expression with a leading `.`, `&&` or `||`.

use super::token::{keyword, Token, TokenKind};
use crate::diag::{codes, Diagnostic, Span};

#[derive(Clone, Copy, Debug)]
pub struct Comment {
    pub lo: u32,
    pub hi: u32,
    pub doc: bool,
    /// True if code precedes the comment on its line.
    pub trailing: bool,
}

pub struct Lexed {
    pub tokens: Vec<Token>,
    pub comments: Vec<Comment>,
    /// Offsets (global) of line starts that follow a blank line; the formatter keeps one blank
    /// line where the author left at least one.
    pub blank_lines: Vec<u32>,
    pub diags: Vec<Diagnostic>,
}

pub fn lex(src: &str, base: u32) -> Lexed {
    let mut lx = Lexer {
        s: src.as_bytes(),
        i: 0,
        base,
        tokens: Vec::with_capacity(src.len() / 4),
        comments: Vec::new(),
        blank_lines: Vec::new(),
        diags: Vec::new(),
        stack: Vec::new(),
        line_has_code: false,
    };
    lx.run();
    Lexed { tokens: lx.tokens, comments: lx.comments, blank_lines: lx.blank_lines, diags: lx.diags }
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    base: u32,
    tokens: Vec<Token>,
    comments: Vec<Comment>,
    blank_lines: Vec<u32>,
    diags: Vec<Diagnostic>,
    /// Open delimiters: b'(' b'[' b'{'.
    stack: Vec<u8>,
    line_has_code: bool,
}

impl Lexer<'_> {
    fn pos(&self, i: usize) -> u32 {
        self.base + i as u32
    }

    fn push(&mut self, kind: TokenKind, lo: usize, hi: usize) {
        self.tokens.push(Token { kind, lo: self.pos(lo), hi: self.pos(hi) });
        self.line_has_code = true;
    }

    fn err(&mut self, code: crate::diag::Code, lo: usize, hi: usize, msg: impl Into<String>) {
        self.diags.push(Diagnostic::error(code, Span::new(self.pos(lo), self.pos(hi)), msg));
    }

    fn newlines_significant(&self) -> bool {
        !matches!(self.stack.last(), Some(b'(') | Some(b'['))
    }

    fn last_ends_statement(&self) -> bool {
        self.tokens.last().is_some_and(|t| t.kind.ends_statement())
    }

    /// Does the text after this line break continue the previous expression?
    fn continues_on_next_line(&self, mut j: usize) -> bool {
        loop {
            while j < self.s.len() && matches!(self.s[j], b' ' | b'\t' | b'\r' | b'\n') {
                j += 1;
            }
            // skip full-line comments between the lines
            if j + 1 < self.s.len() && self.s[j] == b'/' && self.s[j + 1] == b'/' {
                while j < self.s.len() && self.s[j] != b'\n' {
                    j += 1;
                }
                continue;
            }
            break;
        }
        let rest = &self.s[j.min(self.s.len())..];
        (rest.first() == Some(&b'.') && rest.get(1) != Some(&b'.'))
            || rest.starts_with(b"&&")
            || rest.starts_with(b"||")
    }

    fn newline(&mut self, at: usize) {
        if self.newlines_significant() && self.last_ends_statement() && !self.continues_on_next_line(at + 1) {
            let p = self.pos(at);
            self.tokens.push(Token { kind: TokenKind::Semi, lo: p, hi: p });
        }
        // Blank-line tracking: a line with only whitespace.
        let mut j = at + 1;
        while j < self.s.len() && matches!(self.s[j], b' ' | b'\t' | b'\r') {
            j += 1;
        }
        if j < self.s.len() && self.s[j] == b'\n' && self.line_has_code_or_comment() {
            self.blank_lines.push(self.pos(j + 1));
        }
        self.line_has_code = false;
    }

    fn line_has_code_or_comment(&self) -> bool {
        // A blank line only matters after some content.
        !self.tokens.is_empty() || !self.comments.is_empty()
    }

    fn run(&mut self) {
        use TokenKind::*;
        while self.i < self.s.len() {
            let c = self.s[self.i];
            let start = self.i;
            match c {
                b'\n' => {
                    self.newline(self.i);
                    self.i += 1;
                }
                b' ' | b'\t' | b'\r' => self.i += 1,
                b'/' if self.peek(1) == b'/' => {
                    let doc = self.peek(2) == b'/' && self.peek(3) != b'/';
                    while self.i < self.s.len() && self.s[self.i] != b'\n' {
                        self.i += 1;
                    }
                    let mut hi = self.i;
                    while hi > start && matches!(self.s[hi - 1], b' ' | b'\t' | b'\r') {
                        hi -= 1;
                    }
                    self.comments.push(Comment {
                        lo: self.pos(start),
                        hi: self.pos(hi),
                        doc,
                        trailing: self.line_has_code,
                    });
                }
                b'a'..=b'z' | b'A'..=b'Z' | b'_' => {
                    while self.i < self.s.len()
                        && matches!(self.s[self.i], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_')
                    {
                        self.i += 1;
                    }
                    let text = std::str::from_utf8(&self.s[start..self.i]).unwrap();
                    let kind = keyword(text).unwrap_or(Ident);
                    self.push(kind, start, self.i);
                }
                b'0'..=b'9' => self.number(),
                b'"' => self.string(),
                b'\'' => self.char_lit(),
                _ => self.punct(),
            }
        }
        // Terminate the last statement.
        if self.last_ends_statement() {
            let p = self.pos(self.s.len());
            self.tokens.push(Token { kind: Semi, lo: p, hi: p });
        }
        let p = self.pos(self.s.len());
        self.tokens.push(Token { kind: Eof, lo: p, hi: p });
    }

    fn peek(&self, k: usize) -> u8 {
        *self.s.get(self.i + k).unwrap_or(&0)
    }

    fn number(&mut self) {
        let start = self.i;
        let after_dot = self.tokens.last().is_some_and(|t| t.kind == TokenKind::Dot);
        let mut is_float = false;
        if self.s[self.i] == b'0' && matches!(self.peek(1), b'x' | b'b' | b'o') {
            self.i += 2;
            while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                self.i += 1;
            }
        } else {
            self.digits();
            if !after_dot && self.peek(0) == b'.' && self.peek(1).is_ascii_digit() {
                is_float = true;
                self.i += 1;
                self.digits();
            }
            if !after_dot && matches!(self.peek(0), b'e' | b'E') {
                let save = self.i;
                self.i += 1;
                if matches!(self.peek(0), b'+' | b'-') {
                    self.i += 1;
                }
                if self.peek(0).is_ascii_digit() {
                    is_float = true;
                    self.digits();
                } else {
                    self.i = save;
                }
            }
            // A trailing identifier character makes the literal invalid (`12abc`).
            if self.i < self.s.len() && (self.s[self.i].is_ascii_alphabetic() || self.s[self.i] == b'_') {
                while self.i < self.s.len() && (self.s[self.i].is_ascii_alphanumeric() || self.s[self.i] == b'_') {
                    self.i += 1;
                }
                self.err(codes::E0104, start, self.i, "invalid number literal");
            }
        }
        let kind = if is_float { TokenKind::Float } else { TokenKind::Int };
        self.push(kind, start, self.i);
    }

    fn digits(&mut self) {
        while self.i < self.s.len() && (self.s[self.i].is_ascii_digit() || self.s[self.i] == b'_') {
            self.i += 1;
        }
    }

    fn escape(&mut self) {
        // at backslash
        let start = self.i;
        self.i += 1;
        match self.peek(0) {
            b'n' | b't' | b'r' | b'0' | b'\\' | b'"' | b'\'' => self.i += 1,
            b'u' if self.peek(1) == b'{' => {
                self.i += 2;
                let hs = self.i;
                while self.i < self.s.len() && self.s[self.i].is_ascii_hexdigit() {
                    self.i += 1;
                }
                let ok = self.peek(0) == b'}'
                    && (1..=6).contains(&(self.i - hs))
                    && u32::from_str_radix(std::str::from_utf8(&self.s[hs..self.i]).unwrap(), 16)
                        .ok()
                        .and_then(char::from_u32)
                        .is_some();
                if self.peek(0) == b'}' {
                    self.i += 1;
                }
                if !ok {
                    self.err(codes::E0103, start, self.i, "invalid unicode escape");
                }
            }
            0 | b'\n' => {}
            _ => {
                let len = utf8_len(self.peek(0));
                self.i += len;
                self.err(codes::E0103, start, self.i, "invalid escape sequence");
            }
        }
    }

    fn string(&mut self) {
        let start = self.i;
        self.i += 1;
        loop {
            match self.s.get(self.i) {
                None | Some(b'\n') => {
                    self.err(codes::E0102, start, self.i, "unterminated string literal");
                    break;
                }
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => self.escape(),
                Some(_) => self.i += 1,
            }
        }
        self.push(TokenKind::Str, start, self.i);
    }

    fn char_lit(&mut self) {
        let start = self.i;
        self.i += 1;
        let mut count = 0;
        loop {
            match self.s.get(self.i) {
                None | Some(b'\n') => {
                    self.err(codes::E0106, start, self.i, "unterminated character literal");
                    break;
                }
                Some(b'\'') => {
                    self.i += 1;
                    break;
                }
                Some(b'\\') => {
                    self.escape();
                    count += 1;
                }
                Some(&b) => {
                    self.i += utf8_len(b);
                    count += 1;
                }
            }
        }
        if count != 1 {
            self.err(codes::E0106, start, self.i, "a character literal holds exactly one character");
        }
        self.push(TokenKind::Char, start, self.i);
    }

    fn punct(&mut self) {
        use TokenKind::*;
        let start = self.i;
        let c = self.s[self.i];
        let n1 = self.peek(1);
        let n2 = self.peek(2);
        let (kind, len) = match (c, n1, n2) {
            (b'(', _, _) => (LParen, 1),
            (b')', _, _) => (RParen, 1),
            (b'{', _, _) => (LBrace, 1),
            (b'}', _, _) => (RBrace, 1),
            (b'[', _, _) => (LBracket, 1),
            (b']', _, _) => (RBracket, 1),
            (b',', _, _) => (Comma, 1),
            (b'.', b'.', _) => (DotDot, 2),
            (b'.', _, _) => (Dot, 1),
            (b':', _, _) => (Colon, 1),
            (b';', _, _) => (Semi, 1),
            (b'-', b'>', _) => (Arrow, 2),
            (b'=', b'>', _) => (FatArrow, 2),
            (b'?', _, _) => (Question, 1),
            (b'&', b'&', _) => (AmpAmp, 2),
            (b'&', b'=', _) => (AmpEq, 2),
            (b'&', _, _) => (Amp, 1),
            (b'|', b'|', _) => (PipePipe, 2),
            (b'|', b'=', _) => (PipeEq, 2),
            (b'|', _, _) => (Pipe, 1),
            (b'^', b'=', _) => (CaretEq, 2),
            (b'^', _, _) => (Caret, 1),
            (b'~', _, _) => (Tilde, 1),
            (b'!', b'=', _) => (Ne, 2),
            (b'!', _, _) => (Bang, 1),
            (b'=', b'=', _) => (EqEq, 2),
            (b'=', _, _) => (Eq, 1),
            (b'<', b'<', b'=') => (ShlEq, 3),
            (b'<', b'<', _) => (Shl, 2),
            (b'<', b'=', _) => (Le, 2),
            (b'<', _, _) => (Lt, 1),
            (b'>', b'>', b'=') => (ShrEq, 3),
            (b'>', b'>', _) => (Shr, 2),
            (b'>', b'=', _) => (Ge, 2),
            (b'>', _, _) => (Gt, 1),
            (b'+', b'=', _) => (PlusEq, 2),
            (b'+', _, _) => (Plus, 1),
            (b'-', b'=', _) => (MinusEq, 2),
            (b'-', _, _) => (Minus, 1),
            (b'*', b'=', _) => (StarEq, 2),
            (b'*', _, _) => (Star, 1),
            (b'/', b'=', _) => (SlashEq, 2),
            (b'/', _, _) => (Slash, 1),
            (b'%', b'=', _) => (PercentEq, 2),
            (b'%', _, _) => (Percent, 1),
            _ => {
                let len = utf8_len(c);
                self.i += len;
                let ch = std::str::from_utf8(&self.s[start..self.i]).unwrap_or("?").to_string();
                self.err(codes::E0101, start, self.i, format!("unexpected character `{ch}`"));
                return;
            }
        };
        self.i += len;
        match kind {
            LParen => self.stack.push(b'('),
            LBracket => self.stack.push(b'['),
            LBrace => self.stack.push(b'{'),
            RParen | RBracket | RBrace => {
                self.stack.pop();
            }
            Semi => {
                let span = Span::new(self.pos(start), self.pos(self.i));
                self.diags.push(Diagnostic::error(codes::E0107, span, "semicolons are not used in Jig").fix(
                    "remove the `;` and put the next statement on its own line",
                    span,
                    "",
                ));
            }
            _ => {}
        }
        self.push(kind, start, self.i);
    }
}

fn utf8_len(b: u8) -> usize {
    match b {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Decode the contents of a string or character literal (without the quotes). The lexer has
/// already reported invalid escapes, so this is lenient.
pub fn unescape(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut it = body.chars().peekable();
    while let Some(c) = it.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match it.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('0') => out.push('\0'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some('\'') => out.push('\''),
            Some('u') => {
                if it.peek() == Some(&'{') {
                    it.next();
                    let mut hex = String::new();
                    while let Some(&h) = it.peek() {
                        if h == '}' {
                            it.next();
                            break;
                        }
                        hex.push(h);
                        it.next();
                    }
                    if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                        out.push(ch);
                    }
                }
            }
            Some(other) => out.push(other),
            None => {}
        }
    }
    out
}

/// Re-escape a string for printing as a Jig literal.
pub fn escape(s: &str, quote: char) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\0' => out.push_str("\\0"),
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{{{:x}}}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        let l = lex(src, 1);
        assert!(l.diags.is_empty(), "{:?}", l.diags);
        l.tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn inserts_semicolons_at_line_ends() {
        assert_eq!(kinds("let x = 1\nlet y = x\n"), vec![Let, Ident, Eq, Int, Semi, Let, Ident, Eq, Ident, Semi, Eof]);
    }

    #[test]
    fn no_semicolons_inside_parens() {
        assert_eq!(kinds("f(\n  a,\n  b\n)\n"), vec![Ident, LParen, Ident, Comma, Ident, RParen, Semi, Eof]);
    }

    #[test]
    fn braces_inside_parens_restore_newlines() {
        assert_eq!(
            kinds("f(fn(x) {\n  x\n})\n"),
            vec![Ident, LParen, Fn, LParen, Ident, RParen, LBrace, Ident, Semi, RBrace, RParen, Semi, Eof]
        );
    }

    #[test]
    fn leading_dot_continues() {
        assert_eq!(kinds("xs\n  .len()\n"), vec![Ident, Dot, Ident, LParen, RParen, Semi, Eof]);
        assert_eq!(kinds("a\n  && b\n"), vec![Ident, AmpAmp, Ident, Semi, Eof]);
    }

    #[test]
    fn numbers_and_ranges() {
        assert_eq!(kinds("0..10"), vec![Int, DotDot, Int, Semi, Eof]);
        assert_eq!(kinds("1.5e3"), vec![Float, Semi, Eof]);
        assert_eq!(kinds("t.0.1"), vec![Ident, Dot, Int, Dot, Int, Semi, Eof]);
        assert_eq!(kinds("0xff_ff 1_000"), vec![Int, Int, Semi, Eof]);
    }

    #[test]
    fn strings_chars_and_comments() {
        let l = lex("let s = \"a\\n\\u{e9}\" // hi\n'x'\n", 1);
        assert!(l.diags.is_empty());
        assert_eq!(l.comments.len(), 1);
        assert!(l.comments[0].trailing);
        assert_eq!(unescape("a\\n\\u{e9}"), "a\né");
        let kinds: Vec<_> = l.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(kinds, vec![Let, Ident, Eq, Str, Semi, Char, Semi, Eof]);
    }

    #[test]
    fn reports_errors() {
        let l = lex("\"abc\nlet x = 1;\n$", 1);
        let codes: Vec<_> = l.diags.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![codes::E0102, codes::E0107, codes::E0101]);
    }

    #[test]
    fn generic_close_ends_statement() {
        assert_eq!(
            kinds("items: List<Int>\nn: Int\n"),
            vec![Ident, Colon, Ident, Lt, Ident, Gt, Semi, Ident, Colon, Ident, Semi, Eof]
        );
    }
}
