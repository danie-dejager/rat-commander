//! XML well-formedness: tags that don't match (reported, and read past, so
//! the errors after one are still found), elements left open, attributes
//! given twice or malformed, content outside the root element — and a fatal
//! error from the reader, after which nothing more can be said.

use super::{Diagnostic, at};
use quick_xml::Reader;
use quick_xml::events::Event;

pub fn check(text: &str) -> Vec<Diagnostic> {
    let mut diags = Vec::new();
    let mut reader = Reader::from_str(text);
    reader.config_mut().check_end_names = false;
    reader.config_mut().allow_unmatched_ends = true;
    // Open elements: name and where their start tag is.
    let mut open: Vec<(Vec<u8>, std::ops::Range<usize>)> = Vec::new();
    let mut root_seen = false;
    loop {
        let start = reader.buffer_position() as usize;
        let event = match reader.read_event() {
            Ok(e) => e,
            Err(e) => {
                let at_pos = (reader.error_position() as usize).min(text.len());
                diags.push(at(text, at_pos..at_pos + 1, e.to_string()));
                break;
            }
        };
        let span = start..reader.buffer_position() as usize;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if open.is_empty() && root_seen {
                    diags.push(at(text, span.clone(), "a second root element"));
                }
                root_seen = true;
                for attr in e.attributes().with_checks(true) {
                    if let Err(err) = attr {
                        diags.push(at(text, span.clone(), err.to_string()));
                        break;
                    }
                }
                if matches!(event, Event::Start(_)) {
                    open.push((e.name().as_ref().to_vec(), span));
                }
            }
            Event::End(ref e) => {
                let name = e.name().as_ref().to_vec();
                let shown = String::from_utf8_lossy(&name).into_owned();
                match open.iter().rposition(|(n, _)| *n == name) {
                    Some(i) => {
                        // Everything opened after it was left open.
                        for (inner, inner_span) in open.drain(i + 1..).rev() {
                            let inner = String::from_utf8_lossy(&inner);
                            diags.push(at(
                                text,
                                inner_span,
                                format!("<{inner}> isn't closed before </{shown}>"),
                            ));
                        }
                        open.pop();
                    }
                    None => diags.push(at(
                        text,
                        span,
                        format!("</{shown}> closes nothing that is open"),
                    )),
                }
            }
            Event::Text(ref t) if open.is_empty() => {
                if let Some(lead) = t.iter().position(|b| !b.is_ascii_whitespace()) {
                    let what = if root_seen {
                        "after the root element"
                    } else {
                        "before the root element"
                    };
                    diags.push(at(text, span.start + lead..span.end, format!("text {what}")));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    for (name, span) in open {
        let name = String::from_utf8_lossy(&name);
        diags.push(at(text, span, format!("<{name}> is never closed")));
    }
    diags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str) -> Vec<(usize, String)> {
        check(text)
            .into_iter()
            .map(|d| (text[..d.span.start].matches('\n').count(), d.message))
            .collect()
    }

    #[test]
    fn mismatched_and_unclosed_tags_are_reported_and_read_past() {
        let text = "<project>\n  <item>\n    <name>x</nmae>\n  </item>\n  <open>\n</project>\n";
        let f = found(text);
        assert!(f.contains(&(2, "</nmae> closes nothing that is open".into())), "{f:?}");
        assert!(f.contains(&(2, "<name> isn't closed before </item>".into())), "{f:?}");
        assert!(f.contains(&(4, "<open> isn't closed before </project>".into())), "{f:?}");
        let f = found("<a>\n  <b>\n");
        assert_eq!(f, vec![(0, "<a> is never closed".into()), (1, "<b> is never closed".into())]);
        assert!(
            check("<?xml version=\"1.0\"?>\n<!-- hi -->\n<a x=\"1\"><b/><c>t</c></a>\n").is_empty()
        );
    }

    #[test]
    fn duplicate_attributes_and_stray_content_are_errors() {
        let f = found("<a x=\"1\" x=\"2\"/>\n");
        assert_eq!(f.len(), 1, "{f:?}");
        let f = found("<a/>\n<b/>\ntrailing\n");
        assert!(f.iter().any(|(l, m)| *l == 1 && m == "a second root element"), "{f:?}");
        assert!(f.iter().any(|(l, m)| *l == 2 && m == "text after the root element"), "{f:?}");
    }

    #[test]
    fn a_fatal_error_stops_the_check_where_it_is() {
        let text = "<a>\n  <b attr=\"unterminated>\n</a>\n";
        let d = check(text);
        assert!(!d.is_empty());
        assert!(d.iter().all(|x| text.is_char_boundary(x.span.start)));
    }
}
