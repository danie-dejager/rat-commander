//! The recovering parser behind [`parse`].
//!
//! Hand-written rather than generated, because the part that matters here is
//! what happens *after* an error. Each rule below is the reading a person
//! would give the text: a value where a comma was expected is the next value
//! with its comma missing, a key without a colon is still the key, a closing
//! bracket of the wrong kind closes the container it was meant for. That is
//! what keeps one slip from being reported as a dozen.
//!
//! The containers open at any moment live on an explicit stack rather than on
//! the call stack, so a file of a hundred thousand `[` cannot overflow the
//! thread checking it.

use super::{Diagnostic, Lit, MAX_ERRORS, Options, Sink};
use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Colon,
    Comma,
    /// A string in double quotes (possibly unterminated — already reported).
    Str,
    /// A string in single quotes, which JSON does not have.
    SingleStr,
    /// A number, well-formed or not.
    Num,
    /// A bare word: `true`, `false`, `null`, or something JSON has no word for.
    Word,
    /// Any other character.
    Other,
    Eof,
}

#[derive(Debug, Clone, Copy)]
struct Token {
    kind: Kind,
    start: usize,
    end: usize,
}

impl Token {
    fn span(&self) -> Range<usize> {
        self.start..self.end
    }

    fn starts_value(&self) -> bool {
        matches!(
            self.kind,
            Kind::LBrace | Kind::LBracket | Kind::Str | Kind::SingleStr | Kind::Num | Kind::Word
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    Object,
    Array,
}

impl Container {
    fn name(self) -> &'static str {
        match self {
            Container::Object => "object",
            Container::Array => "array",
        }
    }

    fn closer(self) -> char {
        match self {
            Container::Object => '}',
            Container::Array => ']',
        }
    }
}

/// What an open container is waiting for next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// Just after `{`: a key, or `}`.
    KeyOrClose,
    /// After a `,` in an object: a key.
    KeyAfterComma,
    /// After a key: `:`.
    Colon,
    /// After `:`: the value.
    Value,
    /// Just after `[`: a value, or `]`.
    ValueOrClose,
    /// After a `,` in an array: a value.
    ValueAfterComma,
    /// After a member or an element: `,`, or the closer.
    CommaOrClose,
}

struct Frame {
    kind: Container,
    open: Range<usize>,
    expect: Expect,
    /// The last thing read in this container — the key, the `:`, the `,` or
    /// the value just finished — which is what an error about the next token
    /// is often really about ("missing ',' after *this* value").
    last: Range<usize>,
}

/// Read `text` as JSON of the kind `opts` describes, reporting its contents to
/// `sink` and returning every error found, in no particular order.
pub fn parse(text: &str, opts: Options, sink: &mut impl Sink) -> Vec<Diagnostic> {
    let mut p = Parser {
        text,
        bytes: text.as_bytes(),
        pos: 0,
        opts,
        sink,
        diags: Vec::new(),
        lines: None,
        stack: Vec::new(),
        root_done: false,
        trailing_reported: false,
        stopped: false,
    };
    p.run();
    p.diags
}

struct Parser<'a, S: Sink> {
    text: &'a str,
    bytes: &'a [u8],
    pos: usize,
    opts: Options,
    sink: &'a mut S,
    diags: Vec<Diagnostic>,
    /// Where each line starts, built the first time a message names a line.
    lines: Option<Vec<usize>>,
    stack: Vec<Frame>,
    /// A complete top-level value has been read.
    root_done: bool,
    /// Content after that value has been reported (once is enough).
    trailing_reported: bool,
    /// [`MAX_ERRORS`] reached: nothing more is read.
    stopped: bool,
}

impl<S: Sink> Parser<'_, S> {
    fn run(&mut self) {
        while !self.stopped {
            let tok = self.next_token();
            match (self.stack.last().map(|f| (f.kind, f.expect)), tok.kind) {
                (_, Kind::Eof) => return self.finish(),
                (None, _) => self.at_root(tok),
                (Some((Container::Object, expect)), _) => self.in_object(expect, tok),
                (Some((Container::Array, expect)), _) => self.in_array(expect, tok),
            }
        }
    }

    // -- Structure ------------------------------------------------------------

    fn at_root(&mut self, tok: Token) {
        if self.root_done && !self.opts.multiple_roots {
            if !self.trailing_reported {
                self.trailing_reported = true;
                self.error(tok.span(), "Unexpected content after the end of the document");
            }
            // A container there is still read, so the errors inside it show;
            // anything else after the end has been said enough about.
            if matches!(tok.kind, Kind::LBrace | Kind::LBracket) {
                self.value(tok);
            }
            return;
        }
        if tok.starts_value() {
            self.value(tok);
        } else {
            self.unexpected(tok);
        }
    }

    fn in_object(&mut self, expect: Expect, tok: Token) {
        use Expect::*;
        let last = self.stack.last().map_or(0..0, |f| f.last.clone());
        match (expect, tok.kind) {
            (KeyOrClose | KeyAfterComma, Kind::Str) => self.key(tok),
            (KeyOrClose | KeyAfterComma, Kind::SingleStr) => {
                self.error(tok.span(), "Keys must be in double quotes");
                self.key(tok);
            }
            (KeyOrClose | KeyAfterComma, Kind::Word | Kind::Num) => {
                self.error(tok.span(), "Keys must be strings in double quotes");
                self.key(tok);
            }
            (KeyOrClose, Kind::RBrace) => self.close(tok),
            (KeyAfterComma, Kind::RBrace) => {
                if !self.opts.trailing_commas {
                    self.error(last, "Trailing comma before '}'");
                }
                self.close(tok);
            }
            (KeyOrClose | KeyAfterComma, Kind::Comma) => {
                self.error(tok.span(), "Unexpected ',': a key belongs here");
                self.set_last(tok.span());
            }
            (KeyOrClose | KeyAfterComma, Kind::LBrace | Kind::LBracket) => {
                self.error(tok.span(), "Expected a key in double quotes");
                self.value(tok);
            }
            (Colon, Kind::Colon) => self.expect(Value, tok.span()),
            (Colon, _) if tok.starts_value() => {
                self.error(last, "Missing ':' after this key");
                self.value(tok);
            }
            (Colon, Kind::Comma) => {
                self.error(last, "Missing ':' and a value after this key");
                self.expect(KeyAfterComma, tok.span());
            }
            (Colon, Kind::RBrace) => {
                self.error(last, "Missing ':' and a value after this key");
                self.close(tok);
            }
            (Value, _) if tok.starts_value() => self.value(tok),
            (Value, Kind::Comma) => {
                self.error(last, "Missing value after ':'");
                self.expect(KeyAfterComma, tok.span());
            }
            (Value, Kind::RBrace) => {
                self.error(last, "Missing value after ':'");
                self.close(tok);
            }
            (CommaOrClose, Kind::Comma) => self.expect(KeyAfterComma, tok.span()),
            (CommaOrClose, Kind::RBrace) => self.close(tok),
            // The next member with its comma missing.
            (CommaOrClose, Kind::Str | Kind::SingleStr | Kind::Word | Kind::Num) => {
                self.error(last.clone(), "Missing ',' after this value");
                self.expect(KeyAfterComma, last);
                self.in_object(KeyAfterComma, tok);
            }
            (CommaOrClose, Kind::LBrace | Kind::LBracket) => {
                self.error(last, "Missing ',' after this value");
                self.value(tok);
            }
            (_, Kind::Colon) => self.error(tok.span(), "Unexpected ':'"),
            (_, Kind::RBracket) => self.mismatched(tok),
            _ => self.unexpected(tok),
        }
    }

    fn in_array(&mut self, expect: Expect, tok: Token) {
        use Expect::*;
        let last = self.stack.last().map_or(0..0, |f| f.last.clone());
        match (expect, tok.kind) {
            (ValueOrClose | ValueAfterComma, _) if tok.starts_value() => self.value(tok),
            (ValueOrClose, Kind::RBracket) => self.close(tok),
            (ValueAfterComma, Kind::RBracket) => {
                if !self.opts.trailing_commas {
                    self.error(last, "Trailing comma before ']'");
                }
                self.close(tok);
            }
            (ValueOrClose, Kind::Comma) => {
                self.error(tok.span(), "Missing value before ','");
                self.expect(ValueAfterComma, tok.span());
            }
            (ValueAfterComma, Kind::Comma) => {
                self.error(tok.span(), "Missing value between commas");
                self.set_last(tok.span());
            }
            (CommaOrClose, Kind::Comma) => self.expect(ValueAfterComma, tok.span()),
            (CommaOrClose, Kind::RBracket) => self.close(tok),
            (CommaOrClose, _) if tok.starts_value() => {
                self.error(last, "Missing ',' after this value");
                self.value(tok);
            }
            (_, Kind::Colon) => self.error(tok.span(), "Unexpected ':' in an array"),
            (_, Kind::RBrace) => self.mismatched(tok),
            _ => self.unexpected(tok),
        }
    }

    fn expect(&mut self, expect: Expect, last: Range<usize>) {
        if let Some(f) = self.stack.last_mut() {
            f.expect = expect;
            f.last = last;
        }
    }

    fn set_last(&mut self, last: Range<usize>) {
        if let Some(f) = self.stack.last_mut() {
            f.last = last;
        }
    }

    fn key(&mut self, tok: Token) {
        let text = self.text;
        self.sink.key(&text[tok.span()], tok.span());
        self.expect(Expect::Colon, tok.span());
    }

    /// Read a value starting at `tok`, which [`Token::starts_value`].
    fn value(&mut self, tok: Token) {
        let text = self.text;
        let raw = &text[tok.span()];
        match tok.kind {
            Kind::LBrace | Kind::LBracket => {
                let kind =
                    if tok.kind == Kind::LBrace { Container::Object } else { Container::Array };
                match kind {
                    Container::Object => self.sink.begin_object(tok.start),
                    Container::Array => self.sink.begin_array(tok.start),
                }
                let expect = if kind == Container::Object {
                    Expect::KeyOrClose
                } else {
                    Expect::ValueOrClose
                };
                self.stack.push(Frame { kind, open: tok.span(), expect, last: tok.span() });
                return;
            }
            Kind::Str => self.sink.string(raw, tok.span()),
            Kind::SingleStr => {
                self.error(tok.span(), "Strings must be in double quotes");
                self.sink.string(raw, tok.span());
            }
            Kind::Num => {
                if let Some(message) = number_error(raw) {
                    self.error(tok.span(), message);
                }
                self.sink.number(raw, tok.span());
            }
            Kind::Word => match raw {
                "true" => self.sink.literal(Lit::True, tok.span()),
                "false" => self.sink.literal(Lit::False, tok.span()),
                "null" => self.sink.literal(Lit::Null, tok.span()),
                _ => {
                    self.error(tok.span(), &word_error(raw));
                    self.sink.string(raw, tok.span());
                }
            },
            _ => return,
        }
        self.completed(tok.span());
    }

    /// A value ending at `span` is finished.
    fn completed(&mut self, span: Range<usize>) {
        match self.stack.last_mut() {
            None => self.root_done = true,
            Some(f) => {
                f.expect = Expect::CommaOrClose;
                f.last = span;
            }
        }
    }

    /// Close the innermost container at `closer`.
    fn close(&mut self, closer: Token) {
        let Some(f) = self.stack.pop() else { return };
        self.end(f.kind, f.open.start..closer.end);
        self.completed(closer.span());
    }

    fn end(&mut self, kind: Container, span: Range<usize>) {
        match kind {
            Container::Object => self.sink.end_object(span),
            Container::Array => self.sink.end_array(span),
        }
    }

    /// A closer of the wrong kind for the innermost container. When an outer
    /// container of its kind is open, it closes that one and everything inside
    /// it — the inner closer is what is missing. Otherwise it was meant for the
    /// innermost container and is simply the wrong character.
    fn mismatched(&mut self, closer: Token) {
        let want = if closer.kind == Kind::RBrace { Container::Object } else { Container::Array };
        let Some(top) = self.stack.last() else { return };
        let (kind, line) = (top.kind, self.line_of(top.open.start) + 1);
        let message =
            format!("Expected '{}' to close the {} from line {line}", kind.closer(), kind.name());
        self.error(closer.span(), &message);
        if let Some(depth) = self.stack.iter().rposition(|f| f.kind == want) {
            while self.stack.len() > depth + 1 {
                let f = self.stack.pop().expect("deeper than depth");
                self.end(f.kind, f.open.start..closer.start);
            }
        }
        self.close(closer);
    }

    fn unexpected(&mut self, tok: Token) {
        let what: String = self.text[tok.span()].chars().take(20).collect();
        let message = match tok.kind {
            Kind::Other => format!("Unexpected character '{what}'"),
            Kind::RBrace | Kind::RBracket if self.stack.is_empty() => {
                format!("Unexpected '{what}': nothing is open to close")
            }
            _ => format!("Unexpected '{what}'"),
        };
        self.error(tok.span(), &message);
    }

    /// The end of the text: whatever is still open is never closed.
    fn finish(&mut self) {
        while let Some(f) = self.stack.pop() {
            let message = format!("This {} is never closed", f.kind.name());
            self.error(f.open.clone(), &message);
            self.end(f.kind, f.open.start..self.text.len());
        }
    }

    // -- Errors -----------------------------------------------------------------

    fn error(&mut self, span: Range<usize>, message: &str) {
        if self.stopped {
            return;
        }
        let span = self.one_line(span);
        if self.diags.len() >= MAX_ERRORS {
            self.stopped = true;
            self.diags.push(Diagnostic {
                span,
                message: "Too many errors: the rest of the file was not checked".into(),
            });
            return;
        }
        self.diags.push(Diagnostic { span, message: message.into() });
    }

    fn one_line(&self, span: Range<usize>) -> Range<usize> {
        super::clip_to_line(self.text, span)
    }

    fn line_of(&mut self, pos: usize) -> usize {
        let lines = self.lines.get_or_insert_with(|| {
            std::iter::once(0)
                .chain(memchr::memchr_iter(b'\n', self.bytes).map(|i| i + 1))
                .collect()
        });
        lines.partition_point(|&s| s <= pos).saturating_sub(1)
    }

    // -- Tokens -----------------------------------------------------------------

    /// The next token, past whitespace and comments.
    fn next_token(&mut self) -> Token {
        loop {
            while let Some(b' ' | b'\t' | b'\n' | b'\r') = self.bytes.get(self.pos) {
                self.pos += 1;
            }
            if self.pos == 0 && self.text.starts_with('\u{feff}') {
                self.pos = '\u{feff}'.len_utf8();
                continue;
            }
            let start = self.pos;
            let Some(&b) = self.bytes.get(start) else {
                return Token { kind: Kind::Eof, start, end: start };
            };
            let single = |kind| (kind, start + 1);
            let (kind, end) = match b {
                b'{' => single(Kind::LBrace),
                b'}' => single(Kind::RBrace),
                b'[' => single(Kind::LBracket),
                b']' => single(Kind::RBracket),
                b':' => single(Kind::Colon),
                b',' => single(Kind::Comma),
                b'"' => (Kind::Str, self.string_end(start, b'"')),
                b'\'' => (Kind::SingleStr, self.string_end(start, b'\'')),
                b'/' if matches!(self.bytes.get(start + 1), Some(b'/' | b'*')) => {
                    self.pos = self.comment_end(start);
                    continue;
                }
                b'-' | b'+' | b'.' | b'0'..=b'9' => {
                    let len = self.bytes[start..]
                        .iter()
                        .position(|&c| {
                            !(c.is_ascii_alphanumeric() || matches!(c, b'.' | b'+' | b'-'))
                        })
                        .unwrap_or(self.bytes.len() - start);
                    (Kind::Num, start + len)
                }
                _ => {
                    let c = self.text[start..].chars().next().unwrap_or(' ');
                    if c.is_alphabetic() || c == '_' || c == '$' {
                        let len = self.text[start..]
                            .char_indices()
                            .find(|&(_, c)| !(c.is_alphanumeric() || c == '_' || c == '$'))
                            .map_or(self.text.len() - start, |(i, _)| i);
                        (Kind::Word, start + len)
                    } else {
                        (Kind::Other, start + c.len_utf8())
                    }
                }
            };
            self.pos = end;
            return Token { kind, start, end };
        }
    }

    /// Where a string starting with `quote` at `start` ends, reporting what is
    /// wrong inside it. One that never closes ends at the end of its line —
    /// JSON strings cannot hold a line break — so the next line reads on.
    fn string_end(&mut self, start: usize, quote: u8) -> usize {
        let mut i = start + 1;
        loop {
            match self.bytes.get(i) {
                None | Some(b'\n') => {
                    self.error(start..i, "Unterminated string: the closing quote is missing");
                    return i;
                }
                Some(&c) if c == quote => return i + 1,
                Some(b'\\') => {
                    let next = self.bytes.get(i + 1).copied();
                    match next {
                        Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => i += 2,
                        Some(b'\'') if quote == b'\'' => i += 2,
                        Some(b'u') => {
                            let hex = self.bytes.get(i + 2..i + 6);
                            if hex.is_some_and(|h| h.iter().all(u8::is_ascii_hexdigit)) {
                                i += 6;
                            } else {
                                let end = (i + 2..(i + 6).min(self.bytes.len()))
                                    .find(|&j| !self.bytes[j].is_ascii_hexdigit())
                                    .unwrap_or((i + 6).min(self.bytes.len()));
                                self.error(i..end, "\\u needs four hexadecimal digits");
                                i = end;
                            }
                        }
                        None | Some(b'\n') => i += 1,
                        Some(_) => {
                            let c = self.text[i + 1..].chars().next().map_or(1, char::len_utf8);
                            self.error(i..i + 1 + c, "Invalid escape in a string");
                            i += 1 + c;
                        }
                    }
                }
                Some(&c) if c < 0x20 && c != b'\r' => {
                    self.error(i..i + 1, "A control character (such as a tab) must be escaped");
                    i += 1;
                }
                Some(_) => i += 1,
            }
        }
    }

    /// Where a comment starting at `start` ends, reporting it if the file may
    /// not have comments.
    fn comment_end(&mut self, start: usize) -> usize {
        let end = if self.bytes[start + 1] == b'/' {
            memchr::memchr(b'\n', &self.bytes[start..]).map_or(self.bytes.len(), |i| start + i)
        } else {
            match memchr::memmem::find(&self.bytes[start + 2..], b"*/") {
                Some(i) => start + 2 + i + 2,
                None => {
                    self.error(start..self.bytes.len(), "Unterminated comment: '*/' is missing");
                    return self.bytes.len();
                }
            }
        };
        if !self.opts.comments {
            self.error(start..end, "Comments are not allowed in JSON");
        }
        end
    }
}

/// What is wrong with a number, if anything, by JSON's grammar:
/// `-? (0 | [1-9][0-9]*) (. [0-9]+)? ([eE] [+-]? [0-9]+)?`.
fn number_error(s: &str) -> Option<&'static str> {
    let b = s.as_bytes();
    if s.contains("Infinity") || s.contains("NaN") {
        return Some("NaN and Infinity are not allowed in JSON");
    }
    let digits = |i: &mut usize| {
        let from = *i;
        while b.get(*i).is_some_and(u8::is_ascii_digit) {
            *i += 1;
        }
        *i - from
    };
    let mut i = usize::from(b.first() == Some(&b'-'));
    match b.get(i) {
        Some(b'+') if i == 0 => return Some("A number cannot start with '+'"),
        Some(b'0') => {
            i += 1;
            match b.get(i) {
                Some(c) if c.is_ascii_digit() => return Some("A number cannot have leading zeros"),
                Some(b'x' | b'X') => return Some("Hexadecimal numbers are not allowed in JSON"),
                _ => {}
            }
        }
        Some(c) if c.is_ascii_digit() => {
            digits(&mut i);
        }
        Some(b'.') => return Some("A number needs a digit before the decimal point"),
        _ => return Some("Invalid number"),
    }
    if b.get(i) == Some(&b'.') {
        i += 1;
        if digits(&mut i) == 0 {
            return Some("A number needs a digit after the decimal point");
        }
    }
    if let Some(b'e' | b'E') = b.get(i) {
        i += 1;
        if let Some(b'+' | b'-') = b.get(i) {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return Some("An exponent needs at least one digit");
        }
    }
    (i != b.len()).then_some("Invalid number")
}

/// What is wrong with a bare word used as a value.
fn word_error(w: &str) -> String {
    match w {
        "NaN" | "Infinity" => "NaN and Infinity are not allowed in JSON".into(),
        _ if w.eq_ignore_ascii_case("true")
            || w.eq_ignore_ascii_case("false")
            || w.eq_ignore_ascii_case("null")
            || matches!(w, "None" | "nil" | "undefined") =>
        {
            format!("'{w}' is not a JSON value: the literals are true, false and null")
        }
        _ => format!("'{w}' is not a JSON value: text needs double quotes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::NullSink;

    /// Each error as the text it underlines and its message, in text order.
    fn errors_with(text: &str, opts: Options) -> Vec<(String, String)> {
        let mut d = parse(text, opts, &mut NullSink);
        d.sort_by_key(|d| d.span.start);
        for e in &d {
            assert!(e.span.start < e.span.end, "empty span for {:?} in {text:?}", e.message);
            assert!(!text[e.span.clone()].contains('\n'), "{:?} crosses a line", e.message);
        }
        d.into_iter().map(|d| (text[d.span].to_string(), d.message)).collect()
    }

    fn errors(text: &str) -> Vec<(String, String)> {
        errors_with(text, Options::default())
    }

    fn one(text: &str) -> (String, String) {
        let e = errors(text);
        assert_eq!(e.len(), 1, "exactly one error in {text:?}: {e:?}");
        e.into_iter().next().unwrap()
    }

    #[test]
    fn well_formed_documents_have_no_errors() {
        for ok in [
            "{}",
            "[]",
            "",
            "  \n ",
            "42",
            "\"s\"",
            "\u{feff}{}",
            r#"{"a":[1,-2.5e-3,0,1E+9,true,false,null,"x\u00e9\n\"q\""],"b":{"c":{}}}"#,
            "{\r\n  \"crlf\": [1, 2]\r\n}\r\n",
        ] {
            assert_eq!(errors(ok), [], "{ok:?}");
        }
    }

    #[test]
    fn a_missing_comma_is_one_error_on_the_value_before_it() {
        assert_eq!(one(r#"{"a": 1 "b": 2}"#), ("1".into(), "Missing ',' after this value".into()));
        assert_eq!(one("[1 2]"), ("1".into(), "Missing ',' after this value".into()));
        assert_eq!(one(r#"[{"a":1} {"b":2}]"#).0, "}");
    }

    #[test]
    fn trailing_commas_and_missing_pieces_are_named() {
        assert_eq!(one("[1,2,]"), (",".into(), "Trailing comma before ']'".into()));
        assert_eq!(one(r#"{"a":1,}"#), (",".into(), "Trailing comma before '}'".into()));
        assert_eq!(one(r#"{"a" 1}"#), ("\"a\"".into(), "Missing ':' after this key".into()));
        assert_eq!(one(r#"{"a":}"#), (":".into(), "Missing value after ':'".into()));
        assert_eq!(one("[,1]"), (",".into(), "Missing value before ','".into()));
    }

    #[test]
    fn javascript_habits_are_pointed_out() {
        assert_eq!(one("{a: 1}"), ("a".into(), "Keys must be strings in double quotes".into()));
        assert_eq!(one("{'a': 1}"), ("'a'".into(), "Keys must be in double quotes".into()));
        assert_eq!(one("['x']").1, "Strings must be in double quotes");
        assert_eq!(
            one("// note\n{}"),
            ("// note".into(), "Comments are not allowed in JSON".into())
        );
        assert_eq!(one("[1, /* two\nlines */ 2]").0, "/* two", "cut to its first line");
        assert!(one("[True]").1.contains("true, false and null"));
        assert!(one("[undefined]").1.contains("true, false and null"));
        assert!(one("[hello]").1.contains("double quotes"));
    }

    #[test]
    fn malformed_numbers_say_what_is_wrong() {
        let cases = [
            ("01", "A number cannot have leading zeros"),
            (".5", "A number needs a digit before the decimal point"),
            ("1.", "A number needs a digit after the decimal point"),
            ("+1", "A number cannot start with '+'"),
            ("0x1F", "Hexadecimal numbers are not allowed in JSON"),
            ("1e", "An exponent needs at least one digit"),
            ("NaN", "NaN and Infinity are not allowed in JSON"),
            ("-Infinity", "NaN and Infinity are not allowed in JSON"),
            ("1.2.3", "Invalid number"),
            ("-", "Invalid number"),
        ];
        for (n, message) in cases {
            assert_eq!(one(&format!("[{n}]")), (n.into(), message.into()), "{n}");
        }
    }

    #[test]
    fn strings_report_what_is_wrong_inside_them() {
        assert_eq!(one(r#"["a\qb"]"#), (r"\q".into(), "Invalid escape in a string".into()));
        assert_eq!(one(r#"["\u12"]"#).1, "\\u needs four hexadecimal digits");
        assert_eq!(one("[\"a\tb\"]").1, "A control character (such as a tab) must be escaped");
        // An unterminated string ends with its line, and the next line reads on.
        assert_eq!(
            one("[\"abc\n, 1]"),
            ("\"abc".into(), "Unterminated string: the closing quote is missing".into())
        );
        // A wrong escape in a multi-byte character is cut on a char boundary.
        assert_eq!(one("[\"\\é\"]").0, "\\é");
    }

    #[test]
    fn a_wrong_closer_closes_what_it_was_meant_for() {
        assert_eq!(
            one("{\"a\": [1, 2}"),
            ("}".into(), "Expected ']' to close the array from line 1".into())
        );
        assert_eq!(one("[1, 2}").1, "Expected ']' to close the array from line 1");
        assert_eq!(one("{\n\"a\": [\n1]]").1, "Expected '}' to close the object from line 1");
        assert_eq!(one("{}}").1, "Unexpected content after the end of the document");
        assert_eq!(one("}").1, "Unexpected '}': nothing is open to close");
    }

    #[test]
    fn what_is_left_open_at_the_end_is_reported_where_it_opened() {
        assert_eq!(
            errors("{\"a\": [1"),
            [
                ("{".into(), "This object is never closed".into()),
                ("[".into(), "This array is never closed".into()),
            ]
        );
        assert_eq!(
            one("{} {}"),
            ("{".into(), "Unexpected content after the end of the document".into())
        );
        assert_eq!(errors("{\"a\": 1,\n"), [("{".into(), "This object is never closed".into())]);
    }

    #[test]
    fn independent_errors_are_all_reported_and_nothing_else() {
        let text = "{\n  \"a\": 1\n  \"b\": [1 2],\n  \"c\": tru,\n  \"d\": {\"e\": 01}\n}";
        let e = errors(text);
        let messages: Vec<&str> = e.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(
            messages,
            [
                "Missing ',' after this value",
                "Missing ',' after this value",
                "'tru' is not a JSON value: text needs double quotes",
                "A number cannot have leading zeros",
            ]
        );
    }

    #[test]
    fn the_lenient_kinds_allow_what_they_allow() {
        let jsonc = Options { comments: true, trailing_commas: true, multiple_roots: false };
        assert_eq!(errors_with("// c\n{\"a\": [1,],}/* d */", jsonc), []);
        let lines = Options { multiple_roots: true, ..Options::default() };
        assert_eq!(errors_with("{\"a\":1}\n{\"a\":2}\n", lines), []);
        assert_eq!(errors_with("{\"a\":1}\n{\"a\" 2}\n", lines).len(), 1);
    }

    #[test]
    fn deep_nesting_neither_overflows_nor_runs_away() {
        let n = 100_000;
        let deep = format!("{}{}", "[".repeat(n), "]".repeat(n));
        assert_eq!(errors(&deep), []);
        let open = "[".repeat(n);
        let e = parse(&open, Options::default(), &mut NullSink);
        assert_eq!(e.len(), MAX_ERRORS + 1);
        assert!(e.last().unwrap().message.starts_with("Too many errors"));
    }

    #[test]
    fn the_sink_hears_the_document_with_balanced_events_despite_errors() {
        #[derive(Default)]
        struct Count {
            depth: i32,
            keys: Vec<String>,
            numbers: usize,
        }
        impl Sink for Count {
            fn begin_object(&mut self, _: usize) {
                self.depth += 1;
            }
            fn end_object(&mut self, _: Range<usize>) {
                self.depth -= 1;
            }
            fn begin_array(&mut self, _: usize) {
                self.depth += 1;
            }
            fn end_array(&mut self, _: Range<usize>) {
                self.depth -= 1;
            }
            fn key(&mut self, raw: &str, _: Range<usize>) {
                self.keys.push(raw.to_string());
            }
            fn number(&mut self, _: &str, _: Range<usize>) {
                self.numbers += 1;
            }
        }
        for text in ["{\"a\": [1, 2}", "{\"a\": [1 2", "[[[1]]]]", "{\"x\" 1, \"y\": {\"z\": [3}"] {
            let mut c = Count::default();
            parse(text, Options::default(), &mut c);
            assert_eq!(c.depth, 0, "{text}");
        }
        let mut c = Count::default();
        parse("{\"a\": 1 \"b\": [2, 3]}", Options::default(), &mut c);
        assert_eq!(c.keys, ["\"a\"", "\"b\""]);
        assert_eq!(c.numbers, 3);
    }
}
