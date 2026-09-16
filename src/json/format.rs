//! Reformatting a JSON document without changing what it says: pretty-printed,
//! minified, or with every object's keys sorted. It is built on the parser's
//! events rather than on a parsed value, so keys keep their order, numbers and
//! strings keep their spelling (`1.0e+2` stays `1.0e+2`, `"\u00e9"` stays
//! escaped), and in JSONC the comments stay with what they are about.

use super::{Lit, Options, Sink};
use std::ops::Range;

/// What to do to the document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Pretty,
    Minify,
    SortKeys,
}

#[derive(Debug, Clone, PartialEq)]
struct Comment(String);

#[derive(Debug, Clone, PartialEq)]
struct Entry {
    /// Comments on the lines before it.
    lead: Vec<Comment>,
    /// The key, as written (objects only).
    key: Option<String>,
    value: Node,
    /// A comment after it on its line.
    trail: Option<Comment>,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Scalar(String),
    Container {
        object: bool,
        entries: Vec<Entry>,
        /// Comments before the closing bracket.
        tail: Vec<Comment>,
    },
}

/// The parser's events, flattened, each with its bytes.
#[derive(Default)]
struct Events(Vec<(Ev, Range<usize>)>);

enum Ev {
    Open(bool),
    Close,
    Key(String),
    Scalar(String),
}

impl Sink for Events {
    fn begin_object(&mut self, at: usize) {
        self.0.push((Ev::Open(true), at..at + 1));
    }
    fn end_object(&mut self, span: Range<usize>) {
        let end = span.end;
        self.0.push((Ev::Close, end.saturating_sub(1)..end));
    }
    fn begin_array(&mut self, at: usize) {
        self.0.push((Ev::Open(false), at..at + 1));
    }
    fn end_array(&mut self, span: Range<usize>) {
        let end = span.end;
        self.0.push((Ev::Close, end.saturating_sub(1)..end));
    }
    fn key(&mut self, raw: &str, span: Range<usize>) {
        self.0.push((Ev::Key(raw.to_string()), span));
    }
    fn string(&mut self, raw: &str, span: Range<usize>) {
        self.0.push((Ev::Scalar(raw.to_string()), span));
    }
    fn number(&mut self, raw: &str, span: Range<usize>) {
        self.0.push((Ev::Scalar(raw.to_string()), span));
    }
    fn literal(&mut self, lit: Lit, span: Range<usize>) {
        let raw = match lit {
            Lit::True => "true",
            Lit::False => "false",
            Lit::Null => "null",
        };
        self.0.push((Ev::Scalar(raw.to_string()), span));
    }
}

/// The comments in `gap` (text between two tokens, which otherwise holds
/// only whitespace, commas and colons), each with where it starts.
fn comments(text: &str, gap: Range<usize>) -> Vec<(usize, Comment)> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = gap.start;
    while i + 1 < gap.end {
        if bytes[i] == b'/' && bytes[i + 1] == b'/' {
            let end = text[i..gap.end].find('\n').map_or(gap.end, |n| i + n);
            out.push((i, Comment(text[i..end].trim_end().to_string())));
            i = end;
        } else if bytes[i] == b'/' && bytes[i + 1] == b'*' {
            let end = text[i + 2..gap.end].find("*/").map_or(gap.end, |n| i + 2 + n + 2);
            out.push((i, Comment(text[i..end].to_string())));
            i = end;
        } else {
            i += 1;
        }
    }
    out
}

struct Builder<'a> {
    text: &'a str,
    events: Vec<(Ev, Range<usize>)>,
    at: usize,
    /// Where the last token ended.
    pos: usize,
    /// Comments read but not yet given to anything.
    pending: Vec<Comment>,
}

impl Builder<'_> {
    /// Split the comments up to `upto` between the entry that ended at
    /// `self.pos` (one on its line, when `trail_to` is given) and whatever
    /// comes next.
    fn gather(&mut self, upto: usize, mut trail_to: Option<&mut Entry>) {
        for (start, c) in comments(self.text, self.pos..upto) {
            let same_line = !self.text[self.pos..start].contains('\n');
            match trail_to.as_deref_mut() {
                Some(entry) if same_line && entry.trail.is_none() && c.0.starts_with("//") => {
                    entry.trail = Some(c);
                }
                Some(entry) if same_line && entry.trail.is_none() && !c.0.contains('\n') => {
                    entry.trail = Some(c);
                }
                _ => self.pending.push(c),
            }
        }
        self.pos = upto;
    }

    fn node(&mut self) -> Option<Node> {
        let (ev, span) = self.events.get(self.at)?;
        let span = span.clone();
        match ev {
            Ev::Scalar(raw) => {
                let raw = raw.clone();
                self.at += 1;
                self.pos = span.end;
                Some(Node::Scalar(raw))
            }
            Ev::Open(object) => {
                let object = *object;
                self.at += 1;
                self.pos = span.end;
                let mut entries: Vec<Entry> = Vec::new();
                loop {
                    let (next, next_span) = self.events.get(self.at)?;
                    let next_start = next_span.start;
                    let closing = matches!(next, Ev::Close);
                    self.gather(next_start, entries.last_mut());
                    if closing {
                        self.at += 1;
                        self.pos = next_start + 1;
                        let tail = std::mem::take(&mut self.pending);
                        return Some(Node::Container { object, entries, tail });
                    }
                    let lead = std::mem::take(&mut self.pending);
                    let key = match &self.events[self.at].0 {
                        Ev::Key(k) if object => {
                            let (k, end) = (k.clone(), self.events[self.at].1.end);
                            self.at += 1;
                            self.pos = end;
                            // A comment between a key and its value goes before the entry.
                            let value_start = self.events.get(self.at)?.1.start;
                            self.gather(value_start, None);
                            Some(k)
                        }
                        _ => None,
                    };
                    let mut lead = lead;
                    lead.append(&mut self.pending);
                    let value = self.node()?;
                    entries.push(Entry { lead, key, value, trail: None });
                }
            }
            _ => None,
        }
    }
}

/// The documents in `text` (one, or several for JSON Lines), with the
/// comments before, between and after them.
fn build(text: &str, opts: Options) -> Result<Vec<Entry>, String> {
    let mut events = Events::default();
    let errors = super::parse(text, opts, &mut events).len();
    if errors > 0 {
        let s = if errors == 1 { "" } else { "s" };
        return Err(format!("Fix {errors} syntax error{s} first"));
    }
    let mut b = Builder { text, events: events.0, at: 0, pos: 0, pending: Vec::new() };
    let mut roots: Vec<Entry> = Vec::new();
    while b.at < b.events.len() {
        let start = b.events[b.at].1.start;
        b.gather(start, roots.last_mut());
        let lead = std::mem::take(&mut b.pending);
        let value = b.node().ok_or("the document couldn't be read")?;
        roots.push(Entry { lead, key: None, value, trail: None });
    }
    b.gather(text.len(), roots.last_mut());
    // Comments after the end of the document stay at the end, on an entry of
    // their own with no value.
    let rest = std::mem::take(&mut b.pending);
    if !roots.is_empty() && !rest.is_empty() {
        roots.push(Entry {
            lead: rest,
            key: None,
            value: Node::Scalar(String::new()),
            trail: None,
        });
    }
    Ok(roots)
}

fn has_comments(entries: &[Entry]) -> bool {
    entries.iter().any(|e| {
        !e.lead.is_empty()
            || e.trail.is_some()
            || matches!(&e.value, Node::Container { entries, tail, .. } if !tail.is_empty() || has_comments(entries))
    })
}

fn sort(node: &mut Node) {
    if let Node::Container { object, entries, .. } = node {
        for e in entries.iter_mut() {
            sort(&mut e.value);
        }
        if *object {
            entries.sort_by(|a, b| {
                let key = |e: &Entry| super::unescape(e.key.as_deref().unwrap_or("")).into_owned();
                key(a).cmp(&key(b))
            });
        }
    }
}

fn write_compact(node: &Node, out: &mut String) {
    match node {
        Node::Scalar(raw) => out.push_str(raw),
        Node::Container { object, entries, .. } => {
            out.push(if *object { '{' } else { '[' });
            for (i, e) in entries.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                if let Some(k) = &e.key {
                    out.push_str(k);
                    out.push(':');
                }
                write_compact(&e.value, out);
            }
            out.push(if *object { '}' } else { ']' });
        }
    }
}

fn write_pretty(node: &Node, depth: usize, indent: &str, out: &mut String) {
    let pad = |d: usize| indent.repeat(d);
    match node {
        Node::Scalar(raw) => out.push_str(raw),
        Node::Container { object, entries, tail } => {
            let (open, close) = if *object { ('{', '}') } else { ('[', ']') };
            out.push(open);
            if entries.is_empty() && tail.is_empty() {
                out.push(close);
                return;
            }
            out.push('\n');
            for (i, e) in entries.iter().enumerate() {
                for c in &e.lead {
                    out.push_str(&format!("{}{}\n", pad(depth + 1), c.0));
                }
                out.push_str(&pad(depth + 1));
                if let Some(k) = &e.key {
                    out.push_str(k);
                    out.push_str(": ");
                }
                write_pretty(&e.value, depth + 1, indent, out);
                if i + 1 < entries.len() {
                    out.push(',');
                }
                if let Some(c) = &e.trail {
                    out.push(' ');
                    out.push_str(&c.0);
                }
                out.push('\n');
            }
            for c in tail {
                out.push_str(&format!("{}{}\n", pad(depth + 1), c.0));
            }
            out.push_str(&pad(depth));
            out.push(close);
        }
    }
}

/// `text` reformatted by `tool`, indenting by `indent` — with a note to show
/// when something was dropped (a minified JSONC file loses its comments).
/// Refused while the document has syntax errors: what they mean is a guess.
pub fn apply(
    tool: Tool,
    text: &str,
    opts: Options,
    indent: &str,
) -> Result<(String, Option<&'static str>), String> {
    let mut roots = build(text, opts)?;
    if roots.is_empty() {
        return Err("The document is empty".into());
    }
    if tool == Tool::SortKeys {
        roots.iter_mut().for_each(|r| sort(&mut r.value));
    }
    let lines = opts.multiple_roots;
    let mut out = String::new();
    let mut note = None;
    if tool == Tool::Minify || lines {
        // JSON Lines stays one document to a line whatever is asked.
        if has_comments(&roots) {
            note = Some("comments removed");
        }
        for r in roots.iter().filter(|r| !matches!(&r.value, Node::Scalar(s) if s.is_empty())) {
            write_compact(&r.value, &mut out);
            out.push('\n');
        }
        if !lines && out.ends_with('\n') {
            out.pop();
        }
    } else {
        for r in &roots {
            for c in &r.lead {
                out.push_str(&c.0);
                out.push('\n');
            }
            if matches!(&r.value, Node::Scalar(s) if s.is_empty()) {
                continue;
            }
            write_pretty(&r.value, 0, indent, &mut out);
            if let Some(c) = &r.trail {
                out.push(' ');
                out.push_str(&c.0);
            }
            out.push('\n');
        }
    }
    Ok((out, note))
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRICT: Options =
        Options { comments: false, trailing_commas: false, multiple_roots: false };
    const JSONC: Options = Options { comments: true, trailing_commas: true, multiple_roots: false };

    fn pretty(text: &str, opts: Options) -> String {
        apply(Tool::Pretty, text, opts, "  ").unwrap().0
    }

    #[test]
    fn pretty_printing_keeps_order_and_spelling() {
        let text = r#"{"z":1.0e+2,"a":[true,null,"\u00e9"],"empty":{},"list":[]}"#;
        assert_eq!(
            pretty(text, STRICT),
            "{\n  \"z\": 1.0e+2,\n  \"a\": [\n    true,\n    null,\n    \"\\u00e9\"\n  ],\n  \"empty\": {},\n  \"list\": []\n}\n"
        );
        let once = pretty(text, STRICT);
        assert_eq!(pretty(&once, STRICT), once, "pretty-printing twice changes nothing");
    }

    #[test]
    fn minify_and_sort_keys() {
        let text = "{\n  \"b\": {\"y\": 1, \"x\": 2},\n  \"a\": [3, 2]\n}\n";
        assert_eq!(
            apply(Tool::Minify, text, STRICT, "  ").unwrap().0,
            r#"{"b":{"y":1,"x":2},"a":[3,2]}"#
        );
        let sorted = apply(Tool::SortKeys, text, STRICT, "\t").unwrap().0;
        assert_eq!(
            sorted,
            "{\n\t\"a\": [\n\t\t3,\n\t\t2\n\t],\n\t\"b\": {\n\t\t\"x\": 2,\n\t\t\"y\": 1\n\t}\n}\n"
        );
        // Keys sort by what they say, not how they are escaped.
        let esc = apply(Tool::SortKeys, r#"{"b":1,"\u0061":2}"#, STRICT, " ").unwrap().0;
        assert!(esc.find("\\u0061").unwrap() < esc.find("\"b\"").unwrap(), "{esc}");
    }

    #[test]
    fn comments_stay_with_what_they_are_about() {
        let text = "// settings\n{\n  // the name\n  \"name\": \"x\", // inline\n  \"list\": [1, /* two */ 2],\n  // nothing after\n}\n";
        let out = pretty(text, JSONC);
        assert_eq!(
            out,
            "// settings\n{\n  // the name\n  \"name\": \"x\", // inline\n  \"list\": [\n    1, /* two */\n    2\n  ]\n  // nothing after\n}\n"
        );
        assert_eq!(pretty(&out, JSONC), out, "and stay put the second time");
        let (min, note) = apply(Tool::Minify, text, JSONC, "  ").unwrap();
        assert_eq!(min, r#"{"name":"x","list":[1,2]}"#);
        assert_eq!(note, Some("comments removed"));
        // Sorting moves a member's comments with it.
        let sorted =
            apply(Tool::SortKeys, "{\n  // b's\n  \"b\": 1,\n  \"a\": 2 // a's\n}", JSONC, "  ")
                .unwrap()
                .0;
        assert_eq!(sorted, "{\n  \"a\": 2, // a's\n  // b's\n  \"b\": 1\n}\n");
    }

    #[test]
    fn json_lines_stay_a_document_to_a_line() {
        let opts = Options { multiple_roots: true, ..STRICT };
        let text = "{\"a\": 1}\n{\"c\": [1, 2], \"b\": 0}\n";
        assert_eq!(
            apply(Tool::Pretty, text, opts, "  ").unwrap().0,
            "{\"a\":1}\n{\"c\":[1,2],\"b\":0}\n"
        );
        assert_eq!(
            apply(Tool::SortKeys, text, opts, "  ").unwrap().0,
            "{\"a\":1}\n{\"b\":0,\"c\":[1,2]}\n"
        );
    }

    #[test]
    fn a_document_with_errors_is_left_alone() {
        assert_eq!(
            apply(Tool::Pretty, "{\"a\": 1,}", STRICT, "  ").unwrap_err(),
            "Fix 1 syntax error first"
        );
        assert!(apply(Tool::Pretty, "  \n", STRICT, "  ").is_err());
    }
}
