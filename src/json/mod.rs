//! JSON read for a person editing it: every syntax error in a document, not
//! just the first, each tied to the characters it is about.
//!
//! A conforming parser stops at the first mistake, which is the right thing
//! for a program reading data and the wrong thing for someone looking at a
//! file: fixing one error only to be shown the next, one at a time, is slow,
//! and a missing comma on line 3 says nothing about the stray bracket on line
//! 400. [`parse`] reads on past each error the way a person would — a value
//! with no comma before it is taken as the next value, a closing bracket of the
//! wrong kind closes what it can — so the errors it reports are the ones that
//! are really there, rather than a cascade set off by the first.
//!
//! The document itself arrives as [`Sink`] events rather than as a tree: the
//! checker needs none of it, and a reader that does want the data (the GeoJSON
//! map) keeps only what it is looking for, instead of a copy of the whole file.

mod parser;

pub use parser::parse;
use std::borrow::Cow;
use std::ops::Range;

/// Most errors reported for one document. Past this many the file is not
/// JSON with mistakes in it but something else, and more would only cost time.
pub const MAX_ERRORS: usize = 1000;

/// What a particular kind of file allows beyond strict JSON.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Options {
    /// `//` and `/* */` comments (JSONC).
    pub comments: bool,
    /// A comma after the last element of an array or object.
    pub trailing_commas: bool,
    /// Any number of documents, one after another (JSON Lines).
    pub multiple_roots: bool,
}

/// How a file named `name` is checked, or `None` when it is not checked as
/// JSON at all. Strict JSON for `.json` and the formats built on it; comments
/// and trailing commas for JSONC and the configuration files known to be
/// written that way; a document per line for JSON Lines. JSON5 is a different
/// language, and is left alone.
pub fn options_for_name(name: &str) -> Option<Options> {
    let lower = name.to_ascii_lowercase();
    let lenient = Options { comments: true, trailing_commas: true, multiple_roots: false };
    let jsonc_config = (lower.starts_with("tsconfig") || lower.starts_with("jsconfig"))
        && lower.ends_with(".json")
        || matches!(lower.as_str(), ".eslintrc.json" | "devcontainer.json" | ".devcontainer.json");
    if jsonc_config || lower.ends_with(".jsonc") || lower.ends_with(".code-workspace") {
        return Some(lenient);
    }
    if [".jsonl", ".ndjson", ".geojsonl"].iter().any(|e| lower.ends_with(e)) {
        return Some(Options { multiple_roots: true, ..Options::default() });
    }
    if [".json", ".geojson", ".topojson"].iter().any(|e| lower.ends_with(e)) {
        return Some(Options::default());
    }
    None
}

/// `span` of `text` cut to its first line and made at least one character
/// long (on a char boundary), so it can always be underlined.
pub fn clip_to_line(text: &str, span: Range<usize>) -> Range<usize> {
    let bytes = text.as_bytes();
    let len = text.len();
    let floor = |mut i: usize| {
        while i > 0 && !text.is_char_boundary(i) {
            i -= 1;
        }
        i
    };
    let mut start = floor(span.start.min(len));
    let mut end = floor(span.end.min(len)).max(start);
    if let Some(nl) = bytes[start..end].iter().position(|&b| b == b'\n') {
        end = start + nl;
    }
    while end > start && bytes[end - 1] == b'\r' {
        end -= 1;
    }
    if end <= start {
        if start >= len {
            // Nothing left to point at: the last character before it.
            start = text[..start].char_indices().next_back().map_or(0, |(i, _)| i);
        }
        end = start + text[start..].chars().next().map_or(0, char::len_utf8);
    }
    start..end
}

/// One error: what is wrong, and the bytes it is about — never empty, and
/// never crossing a line break, so it can be underlined where it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub span: Range<usize>,
    pub message: String,
}

/// `true`, `false` or `null`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lit {
    True,
    False,
    Null,
}

/// Receives the document as it is read. Every event carries the bytes it came
/// from; strings and keys arrive exactly as written, quotes and escapes
/// included. The begin and end events always pair up — also around an error —
/// unless the parse stopped at [`MAX_ERRORS`].
pub trait Sink {
    fn begin_object(&mut self, _at: usize) {}
    fn end_object(&mut self, _span: Range<usize>) {}
    fn begin_array(&mut self, _at: usize) {}
    fn end_array(&mut self, _span: Range<usize>) {}
    fn key(&mut self, _raw: &str, _span: Range<usize>) {}
    fn string(&mut self, _raw: &str, _span: Range<usize>) {}
    fn number(&mut self, _raw: &str, _span: Range<usize>) {}
    fn literal(&mut self, _lit: Lit, _span: Range<usize>) {}
}

/// A string's value from its text as a [`Sink`] receives it: the quotes taken
/// off and the escapes resolved (a surrogate pair into its one character). An
/// escape that is not valid is kept as written — the error has been reported
/// already, and the text around it is still worth having.
pub fn unescape(raw: &str) -> Cow<'_, str> {
    let quote = raw.chars().next().filter(|c| matches!(c, '"' | '\''));
    let inner = match quote {
        Some(q) => {
            let body = &raw[1..];
            body.strip_suffix(q).unwrap_or(body)
        }
        None => raw,
    };
    if !inner.contains('\\') {
        return Cow::Borrowed(inner);
    }
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('b') => out.push('\u{8}'),
            Some('f') => out.push('\u{c}'),
            Some('u') => {
                let hex: String = chars.clone().take(4).collect();
                match u16::from_str_radix(&hex, 16) {
                    Ok(unit) if hex.len() == 4 => {
                        chars.nth(3);
                        let mut units = vec![unit];
                        // A high surrogate wants the low one after it.
                        if (0xD800..0xDC00).contains(&unit) {
                            let rest: String = chars.clone().take(6).collect();
                            if let Some(low) = rest
                                .strip_prefix("\\u")
                                .and_then(|h| u16::from_str_radix(h, 16).ok())
                                .filter(|l| (0xDC00..0xE000).contains(l))
                            {
                                chars.nth(5);
                                units.push(low);
                            }
                        }
                        out.extend(char::decode_utf16(units).map(|r| r.unwrap_or('\u{fffd}')));
                    }
                    _ => out.push_str("\\u"),
                }
            }
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    Cow::Owned(out)
}

/// A number's value from its text, when it is a finite one.
pub fn number(raw: &str) -> Option<f64> {
    raw.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// A sink that keeps nothing: checking only.
pub struct NullSink;

impl Sink for NullSink {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strings_are_read_back_to_their_values() {
        assert_eq!(unescape(r#""plain""#), "plain");
        assert!(matches!(unescape(r#""plain""#), Cow::Borrowed(_)));
        assert_eq!(unescape(r#""a\"b\\c\/d\n""#), "a\"b\\c/d\n");
        assert_eq!(unescape(r#""caf\u00e9""#), "café");
        assert_eq!(unescape(r#""\ud83d\ude00""#), "😀", "a surrogate pair is one character");
        assert_eq!(unescape(r#""bad \q and \u12""#), "bad q and \\u12");
        assert_eq!(unescape("'single'"), "single");
        assert_eq!(unescape("\"unterminated"), "unterminated");
        assert_eq!(number("-1.5e3"), Some(-1500.0));
        assert_eq!(number("1e999"), None);
    }

    #[test]
    fn the_file_name_decides_how_strict_the_check_is() {
        let strict = Some(Options::default());
        assert_eq!(options_for_name("data.json"), strict);
        assert_eq!(options_for_name("MAP.GeoJSON"), strict);
        let lenient = options_for_name("tsconfig.base.json").unwrap();
        assert!(lenient.comments && lenient.trailing_commas);
        assert!(options_for_name("settings.jsonc").unwrap().comments);
        assert!(options_for_name("log.ndjson").unwrap().multiple_roots);
        assert_eq!(options_for_name("app.json5"), None);
        assert_eq!(options_for_name("notes.txt"), None);
        assert_eq!(options_for_name("json"), None);
    }
}
