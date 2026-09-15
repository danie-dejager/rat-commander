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

/// A sink that keeps nothing: checking only.
pub struct NullSink;

impl Sink for NullSink {}

#[cfg(test)]
mod tests {
    use super::*;

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
