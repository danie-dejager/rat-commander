//! Tokens of the template language: C's, plus 010 Editor's number forms (`25h`,
//! `0b101`) and wide literals (`L"…"`).
//!
//! A preprocessor directive is recognised here, where line starts are known:
//! `#` first on a line yields [`Tok::Directive`], and the rest of the line
//! (joined across `\` continuations) follows as ordinary tokens with the same
//! logical line — except for `#include` / `#warning` / `#error`, whose argument
//! is kept as raw text.

use std::fmt;

/// A source position: file index (into the program's file list), line and
/// column, both 1-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pos {
    pub file: u16,
    pub line: u32,
    pub col: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Tok {
    Ident(Box<str>),
    /// An integer literal: value, and whether it was marked unsigned / 64-bit.
    Int(u64, bool, bool),
    /// A floating literal, and whether it was marked `f` (32-bit).
    Float(f64, bool),
    Str(Vec<u8>),
    WStr(Vec<u8>),
    Char(u64),
    WChar(u64),
    Punct(&'static str),
    /// `#name` at the start of a line.
    Directive(Box<str>),
    /// The raw remainder of an `#include` / `#warning` / `#error` line.
    Raw(Box<str>),
}

impl fmt::Display for Tok {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Tok::Ident(s) => write!(f, "'{s}'"),
            Tok::Int(v, ..) => write!(f, "'{v}'"),
            Tok::Float(v, _) => write!(f, "'{v}'"),
            Tok::Str(s) | Tok::WStr(s) => write!(f, "\"{}\"", String::from_utf8_lossy(s)),
            Tok::Char(_) | Tok::WChar(_) => write!(f, "character constant"),
            Tok::Punct(p) => write!(f, "'{p}'"),
            Tok::Directive(d) => write!(f, "'#{d}'"),
            Tok::Raw(r) => write!(f, "'{r}'"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Token {
    pub tok: Tok,
    pub pos: Pos,
    /// The logical line: physical lines joined by `\` share one.
    pub lline: u32,
}

/// A lexing or parsing error.
#[derive(Debug, Clone)]
pub struct Diag {
    pub pos: Pos,
    pub msg: String,
}

/// Punctuators, longest first so the scan takes the longest match.
const PUNCTS: &[&str] = &[
    "<<=", ">>=", "...", "->", "++", "--", "<<", ">>", "<=", ">=", "==", "!=", "&&", "||", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "::", "(", ")", "[", "]", "{", "}", ";", ",", ".",
    "?", ":", "~", "!", "+", "-", "*", "/", "%", "&", "|", "^", "<", ">", "=", "@",
];

/// Directives whose argument is taken as raw text.
const RAW_DIRECTIVES: &[&str] = &["include", "warning", "error", "pragma", "link", "title"];

pub fn lex(src: &[u8], file: u16) -> Result<Vec<Token>, Diag> {
    Lexer { s: src, i: 0, line: 1, col: 1, lline: 1, file, line_start: true }.run()
}

struct Lexer<'a> {
    s: &'a [u8],
    i: usize,
    line: u32,
    col: u32,
    lline: u32,
    file: u16,
    line_start: bool,
}

impl Lexer<'_> {
    fn peek(&self, k: usize) -> u8 {
        self.s.get(self.i + k).copied().unwrap_or(0)
    }

    fn bump(&mut self) -> u8 {
        let c = self.peek(0);
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        c
    }

    fn pos(&self) -> Pos {
        Pos { file: self.file, line: self.line, col: self.col }
    }

    fn err(&self, msg: impl Into<String>) -> Diag {
        Diag { pos: self.pos(), msg: msg.into() }
    }

    /// Skip whitespace and comments; returns whether a (non-continued) newline
    /// was crossed.
    fn skip_space(&mut self) -> Result<bool, Diag> {
        let mut newline = false;
        loop {
            let c = self.peek(0);
            match c {
                b'\n' => {
                    self.bump();
                    self.lline += 1;
                    newline = true;
                }
                b' ' | b'\t' | b'\r' | 0x0b | 0x0c => {
                    self.bump();
                }
                b'\\'
                    if self.peek(1) == b'\n'
                        || (self.peek(1) == b'\r' && self.peek(2) == b'\n') =>
                {
                    // A line continuation: the physical line ends, the logical
                    // one doesn't.
                    while self.bump() != b'\n' {}
                }
                b'/' if self.peek(1) == b'/' => {
                    while self.i < self.s.len() && self.peek(0) != b'\n' {
                        // A `\` ending a `//` comment continues it, as in C.
                        if self.peek(0) == b'\\' && self.peek(1) == b'\n' {
                            self.bump();
                        }
                        self.bump();
                    }
                }
                b'/' if self.peek(1) == b'*' => {
                    let start = self.pos();
                    self.bump();
                    self.bump();
                    loop {
                        if self.i >= self.s.len() {
                            return Err(Diag { pos: start, msg: "unterminated comment".into() });
                        }
                        if self.peek(0) == b'*' && self.peek(1) == b'/' {
                            self.bump();
                            self.bump();
                            break;
                        }
                        if self.peek(0) == b'\n' {
                            self.lline += 1;
                            newline = true;
                        }
                        self.bump();
                    }
                }
                // Stray bytes outside any literal (a BOM, a mis-encoded
                // character) are ignored, as 010 Editor does.
                c if c >= 0x80 => {
                    self.bump();
                }
                0 if self.i < self.s.len() => {
                    self.bump();
                }
                _ => return Ok(newline),
            }
        }
    }

    fn run(mut self) -> Result<Vec<Token>, Diag> {
        let mut out = Vec::new();
        loop {
            if self.skip_space()? {
                self.line_start = true;
            }
            if self.i >= self.s.len() {
                break;
            }
            let pos = self.pos();
            let lline = self.lline;
            let at_start = std::mem::replace(&mut self.line_start, false);
            let c = self.peek(0);
            let tok = if c == b'#' && at_start {
                self.bump();
                while matches!(self.peek(0), b' ' | b'\t') {
                    self.bump();
                }
                let name = self.ident_text();
                if RAW_DIRECTIVES.contains(&name.as_str()) {
                    let raw = self.raw_line();
                    out.push(Token { tok: Tok::Directive(name.into()), pos, lline });
                    Tok::Raw(raw.into())
                } else {
                    Tok::Directive(name.into())
                }
            } else if c == b'L' && (self.peek(1) == b'"' || self.peek(1) == b'\'') {
                self.bump();
                if self.peek(0) == b'"' {
                    Tok::WStr(self.string_lit()?)
                } else {
                    Tok::WChar(self.char_lit()?)
                }
            } else if c.is_ascii_alphabetic() || c == b'_' || c == b'$' {
                Tok::Ident(self.ident_text().into())
            } else if c.is_ascii_digit() || (c == b'.' && self.peek(1).is_ascii_digit()) {
                self.number()?
            } else if c == b'"' {
                Tok::Str(self.string_lit()?)
            } else if c == b'\'' {
                Tok::Char(self.char_lit()?)
            } else if c == b'#' {
                // `#` mid-line: not a directive; nothing in the language uses it.
                return Err(self.err("unexpected '#'"));
            } else {
                let rest = &self.s[self.i..];
                let Some(p) = PUNCTS.iter().find(|p| rest.starts_with(p.as_bytes())) else {
                    return Err(self.err(format!("unexpected character '{}'", c as char)));
                };
                for _ in 0..p.len() {
                    self.bump();
                }
                Tok::Punct(p)
            };
            out.push(Token { tok, pos, lline });
        }
        Ok(out)
    }

    fn ident_text(&mut self) -> String {
        let start = self.i;
        while self.peek(0).is_ascii_alphanumeric() || self.peek(0) == b'_' || self.peek(0) == b'$' {
            self.bump();
        }
        String::from_utf8_lossy(&self.s[start..self.i]).into_owned()
    }

    /// The rest of the line (through `\` continuations), minus a trailing `//`
    /// comment outside quotes.
    fn raw_line(&mut self) -> String {
        let mut text = Vec::new();
        let mut quote = false;
        while self.i < self.s.len() && self.peek(0) != b'\n' {
            let c = self.peek(0);
            if c == b'\\' && self.peek(1) == b'\n' {
                self.bump();
                self.bump();
                continue;
            }
            if c == b'"' {
                quote = !quote;
            }
            if !quote && c == b'/' && self.peek(1) == b'/' {
                while self.i < self.s.len() && self.peek(0) != b'\n' {
                    self.bump();
                }
                break;
            }
            text.push(self.bump());
        }
        String::from_utf8_lossy(&text).trim().to_string()
    }

    fn escape(&mut self) -> Result<u32, Diag> {
        let c = self.bump();
        Ok(match c {
            b'a' => 7,
            b'b' => 8,
            b'f' => 12,
            b'n' => 10,
            b'r' => 13,
            b't' => 9,
            b'v' => 11,
            b'x' | b'X' => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 && self.peek(0).is_ascii_hexdigit() {
                    v = v * 16 + (self.bump() as char).to_digit(16).unwrap_or(0);
                    n += 1;
                }
                v
            }
            b'0'..=b'7' => {
                let mut v = (c - b'0') as u32;
                let mut n = 1;
                while n < 3 && (b'0'..=b'7').contains(&self.peek(0)) {
                    v = v * 8 + (self.bump() - b'0') as u32;
                    n += 1;
                }
                v
            }
            0 => return Err(self.err("unterminated literal")),
            other => other as u32,
        })
    }

    fn string_lit(&mut self) -> Result<Vec<u8>, Diag> {
        let start = self.pos();
        self.bump();
        let mut out = Vec::new();
        loop {
            match self.peek(0) {
                b'"' => {
                    self.bump();
                    break;
                }
                b'\\' if self.peek(1) == b'\n' => {
                    self.bump();
                    self.bump();
                }
                b'\\' => {
                    self.bump();
                    let v = self.escape()?;
                    out.push(v as u8);
                }
                b'\n' | 0 if self.i >= self.s.len() || self.peek(0) == b'\n' => {
                    return Err(Diag { pos: start, msg: "unterminated string".into() });
                }
                _ => out.push(self.bump()),
            }
        }
        Ok(out)
    }

    /// A character constant; several characters pack big-endian (`'PK'`).
    fn char_lit(&mut self) -> Result<u64, Diag> {
        let start = self.pos();
        self.bump();
        let mut v: u64 = 0;
        let mut n = 0;
        loop {
            match self.peek(0) {
                b'\'' => {
                    self.bump();
                    break;
                }
                b'\n' | 0 if self.i >= self.s.len() || self.peek(0) == b'\n' => {
                    return Err(Diag { pos: start, msg: "unterminated character constant".into() });
                }
                b'\\' => {
                    self.bump();
                    v = (v << 8) | (self.escape()? as u64 & 0xff);
                }
                c if c >= 0x80 => {
                    // A UTF-8 character: its code point.
                    let len = match c {
                        0xc0..=0xdf => 2,
                        0xe0..=0xef => 3,
                        _ => 4,
                    };
                    let bytes: Vec<u8> = (0..len).map(|_| self.bump()).collect();
                    let cp = std::str::from_utf8(&bytes)
                        .ok()
                        .and_then(|s| s.chars().next())
                        .map(|ch| ch as u64)
                        .unwrap_or(bytes[0] as u64);
                    v = if n == 0 { cp } else { (v << 8) | (cp & 0xff) };
                }
                _ => v = (v << 8) | self.bump() as u64,
            }
            n += 1;
        }
        Ok(v)
    }

    fn number(&mut self) -> Result<Tok, Diag> {
        let start = self.i;
        let pos = self.pos();
        // The alphanumeric run, plus a fraction and signed exponent for decimals.
        let hex_prefix = self.peek(0) == b'0' && matches!(self.peek(1), b'x' | b'X');
        loop {
            let c = self.peek(0);
            let part_of_number = c.is_ascii_alphanumeric()
                || c == b'_'
                || (c == b'.' && !hex_prefix && self.peek(1) != b'.')
                || ((c == b'+' || c == b'-')
                    && !hex_prefix
                    && matches!(self.s[self.i - 1], b'e' | b'E')
                    && self.s[start..self.i - 1].iter().all(|b| b.is_ascii_digit() || *b == b'.'));
            if !part_of_number {
                break;
            }
            self.bump();
        }
        let text = String::from_utf8_lossy(&self.s[start..self.i]).into_owned();
        let bad = || Diag { pos, msg: format!("invalid number '{text}'") };
        let lower = text.to_ascii_lowercase();

        // Integer suffixes.
        let strip = |mut t: &str| {
            let (mut u, mut l) = (false, false);
            loop {
                if let Some(r) = t.strip_suffix("i64") {
                    t = r;
                    l = true;
                } else if let Some(r) = t.strip_suffix('u') {
                    t = r;
                    u = true;
                } else if let Some(r) = t.strip_suffix('l') {
                    t = r;
                    l = true;
                } else {
                    break;
                }
            }
            (t.to_string(), u, l)
        };

        if let Some(h) = lower.strip_prefix("0x") {
            let (digits, u, l) = strip(h);
            let digits = digits.strip_suffix('h').unwrap_or(&digits).to_string();
            let v = u64::from_str_radix(&digits, 16).map_err(|_| bad())?;
            return Ok(Tok::Int(v, u, l));
        }
        // `25h`, `0EFh`: hex digits then `h`.
        if let Some(h) = lower.strip_suffix('h')
            && !h.is_empty()
            && h.bytes().all(|b| b.is_ascii_hexdigit())
        {
            let v = u64::from_str_radix(h, 16).map_err(|_| bad())?;
            return Ok(Tok::Int(v, false, false));
        }
        if let Some(b) = lower.strip_prefix("0b")
            && !b.is_empty()
        {
            let (digits, u, l) = strip(b);
            if digits.bytes().all(|c| c == b'0' || c == b'1') && !digits.is_empty() {
                let v = u64::from_str_radix(&digits, 2).map_err(|_| bad())?;
                return Ok(Tok::Int(v, u, l));
            }
        }
        let is_float = lower.contains('.')
            || (lower.contains('e')
                && lower.bytes().next().is_some_and(|b| b.is_ascii_digit() || b == b'.')
                && lower
                    .trim_end_matches(['f', 'l'])
                    .bytes()
                    .all(|b| b.is_ascii_digit() || b"e.+-".contains(&b)));
        if is_float {
            let f32_mark = lower.ends_with('f');
            let body = lower.trim_end_matches(['f', 'l']);
            let v: f64 = body.parse().map_err(|_| bad())?;
            return Ok(Tok::Float(v, f32_mark));
        }
        if let Some(body) = lower.strip_suffix('f')
            && body.bytes().all(|b| b.is_ascii_digit())
            && !body.is_empty()
        {
            return Ok(Tok::Float(body.parse().map_err(|_| bad())?, true));
        }
        let (digits, u, l) = strip(&lower);
        let v = if digits.len() > 1 && digits.starts_with('0') {
            u64::from_str_radix(&digits[1..], 8)
                .or_else(|_| digits.parse::<u64>())
                .map_err(|_| bad())?
        } else {
            // Decimal literals too large for u64 wrap, as a C compiler would warn.
            digits
                .parse::<u64>()
                .or_else(|_| digits.parse::<u128>().map(|v| v as u64))
                .map_err(|_| bad())?
        };
        Ok(Tok::Int(v, u, l))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s.as_bytes(), 0).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn numbers_in_every_form() {
        assert_eq!(
            toks("0xff 25h 0EFh 013 0b011 12u -1L 7"),
            vec![
                Tok::Int(255, false, false),
                Tok::Int(0x25, false, false),
                Tok::Int(0xef, false, false),
                Tok::Int(11, false, false),
                Tok::Int(3, false, false),
                Tok::Int(12, true, false),
                Tok::Punct("-"),
                Tok::Int(1, false, true),
                Tok::Int(7, false, false),
            ]
        );
        assert_eq!(
            toks("1e10 2.0f .5 3.25 1e-3"),
            vec![
                Tok::Float(1e10, false),
                Tok::Float(2.0, true),
                Tok::Float(0.5, false),
                Tok::Float(3.25, false),
                Tok::Float(1e-3, false),
            ]
        );
        // A hex constant ending in `e` followed by `+` is an addition.
        assert_eq!(
            toks("0x1e+5"),
            vec![Tok::Int(0x1e, false, false), Tok::Punct("+"), Tok::Int(5, false, false)]
        );
        assert!(lex(b"0x10000000000000000", 0).is_err());
    }

    #[test]
    fn literals_and_escapes() {
        assert_eq!(
            toks(r#""a\tb\x41\101" L"w" 'P' 'PK' '\n' L'x'"#),
            vec![
                Tok::Str(b"a\tbAA".to_vec()),
                Tok::WStr(b"w".to_vec()),
                Tok::Char(b'P' as u64),
                Tok::Char(0x504b),
                Tok::Char(10),
                Tok::WChar(b'x' as u64),
            ]
        );
    }

    #[test]
    fn directives_know_their_lines() {
        let t = lex(b"#define A \\\n  5\nint x; // c\n  #include \"x.bt\" // note\n/* # */ y", 0)
            .unwrap();
        assert_eq!(t[0].tok, Tok::Directive("define".into()));
        assert_eq!(t[1].tok, Tok::Ident("A".into()));
        assert_eq!(t[2].tok, Tok::Int(5, false, false));
        assert_eq!(t[0].lline, t[2].lline);
        assert_ne!(t[2].lline, t[3].lline);
        let inc = t.iter().position(|t| t.tok == Tok::Directive("include".into())).unwrap();
        assert_eq!(t[inc + 1].tok, Tok::Raw("\"x.bt\"".into()));
        assert_eq!(t.last().unwrap().tok, Tok::Ident("y".into()));
    }

    #[test]
    fn punctuators_take_the_longest_match() {
        assert_eq!(
            toks("a<<=b>>c->d"),
            vec![
                Tok::Ident("a".into()),
                Tok::Punct("<<="),
                Tok::Ident("b".into()),
                Tok::Punct(">>"),
                Tok::Ident("c".into()),
                Tok::Punct("->"),
                Tok::Ident("d".into()),
            ]
        );
    }
}
