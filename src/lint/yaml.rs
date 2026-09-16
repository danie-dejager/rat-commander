//! YAML: the first syntax error `saphyr-parser` stops at (YAML 1.2), and
//! before it every key given twice in one mapping — which parsers disagree
//! about, and which is nearly always a mistake.

use super::{Diagnostic, at};
use saphyr_parser::{Event, Marker, Parser};
use std::collections::HashSet;

/// Where a mapping is in the pairs it is made of.
enum Frame {
    /// A mapping: the keys seen, and whether the next node is a key.
    Map {
        keys: HashSet<String>,
        want_key: bool,
    },
    Seq,
}

/// A marker's byte offset in `text`, by its line (from 1) and column (in chars,
/// from 0).
fn offset(text: &str, m: &Marker) -> usize {
    let line_start = text
        .match_indices('\n')
        .nth(m.line().saturating_sub(2))
        .map_or(0, |(i, _)| if m.line() <= 1 { 0 } else { i + 1 });
    let line = &text[line_start..];
    line_start + line.char_indices().nth(m.col()).map_or(line.len(), |(i, _)| i)
}

pub fn check(text: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut stack: Vec<Frame> = Vec::new();
    let mut parser = Parser::new_from_str(text);
    while let Some(next) = parser.next_event() {
        let (event, span) = match next {
            Ok(e) => e,
            Err(e) => {
                let start = offset(text, e.marker());
                diags.push(at(text, start..start + 1, e.info()));
                break;
            }
        };
        // A node in a mapping is its next key or its next value.
        let is_node = matches!(
            event,
            Event::Scalar(..)
                | Event::Alias(_)
                | Event::SequenceStart(..)
                | Event::MappingStart(..)
        );
        let mut key_slot = false;
        if is_node && let Some(Frame::Map { want_key, .. }) = stack.last_mut() {
            key_slot = *want_key;
            *want_key = !*want_key;
        }
        match event {
            Event::Scalar(value, ..) if key_slot => {
                if let Some(Frame::Map { keys, .. }) = stack.last_mut()
                    && !keys.insert(value.to_string())
                {
                    let (start, end) = (offset(text, &span.start), offset(text, &span.end));
                    diags.push(at(text, start..end, format!("duplicate key '{value}'")));
                }
            }
            Event::MappingStart(..) => {
                stack.push(Frame::Map { keys: HashSet::new(), want_key: true })
            }
            Event::SequenceStart(..) => stack.push(Frame::Seq),
            Event::MappingEnd | Event::SequenceEnd => {
                stack.pop();
            }
            Event::DocumentStart(_) => stack.clear(),
            _ => {}
        }
    }
    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(text: &str, d: &Diagnostic) -> usize {
        text[..d.span.start].matches('\n').count()
    }

    #[test]
    fn a_syntax_error_is_placed_after_wide_characters_too() {
        let text = "name: Zoë — ünïcödé\nitems:\n  - one\n - two\n";
        let diags = check(text);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(line_of(text, &diags[0]), 3, "{diags:?}");
        let text = "title: \"ünclosed\nnext: 1\n";
        let d = &check(text)[0];
        assert!(text.is_char_boundary(d.span.start) && text.is_char_boundary(d.span.end));
        assert!(
            check("a: 1\nb:\n  - x\n  - {c: d}\n---\na: again\n").is_empty(),
            "documents are separate"
        );
    }

    #[test]
    fn a_key_given_twice_in_a_mapping_is_flagged() {
        let text =
            "services:\n  web:\n    image: nginx\n    image: httpd\n  db:\n    image: postgres\n";
        let diags = check(text);
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert_eq!(diags[0].message, "duplicate key 'image'");
        assert_eq!(line_of(text, &diags[0]), 3);
        assert_eq!(&text[diags[0].span.clone()], "image");
        // The same key in sibling mappings, or as a value, is fine.
        assert!(check("a: {k: 1}\nb: {k: 2}\nc: [k, k]\nk: k\n").is_empty());
    }
}
