//! TOML: every error the `toml` crate's recovering parser finds.

use super::{Diagnostic, at};

pub fn check(text: &str) -> Vec<Diagnostic> {
    let (_, errors) = ::toml::de::DeTable::parse_recoverable(text);
    errors
        .iter()
        .map(|e| {
            let span = e.span().unwrap_or(0..1);
            // The crate's messages can run to several lines of context: the
            // first says what is wrong.
            let message = e.message().lines().next().unwrap_or("syntax error").trim();
            at(text, span, message)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_error_is_found_and_placed() {
        let text =
            "[package]\nname = \"x\"\nversion = 1.2.3\n\n[deps\nserde = \"1\"\nname = \"again\"\n";
        let diags = check(text);
        assert!(diags.len() >= 2, "{diags:?}");
        let lines: Vec<usize> =
            diags.iter().map(|d| text[..d.span.start].matches('\n').count()).collect();
        assert!(lines.contains(&2), "the bad version: {diags:?}");
        assert!(lines.contains(&4), "the unclosed table header: {diags:?}");
        assert!(check("a = 1\n[b]\nc = \"d\"\n").is_empty());
    }

    #[test]
    fn a_key_given_twice_is_an_error() {
        let diags = check("a = 1\na = 2\n");
        assert_eq!(diags.len(), 1, "{diags:?}");
        assert!(diags[0].message.to_lowercase().contains("duplicate"), "{diags:?}");
    }
}
