//! The binary templates built into the program: `assets/templates/*.bt`, packed
//! by `build.rs` into one zlib bundle and unpacked on first use.

use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/templates_meta.rs"));

static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/templates.bin"));

/// A bundled template: file name and content.
pub type Entry = crate::util::bundle::Entry;

/// Every bundled template, sorted by name.
static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(unpack);

fn unpack() -> Vec<Entry> {
    crate::util::bundle::unpack(PACKED, b"RCBT0001", BUNDLE_COUNT)
}

/// All bundled templates, sorted by file name.
pub fn entries() -> &'static [Entry] {
    &ENTRIES
}

/// The bundled template named `name` (compared case-insensitively).
pub fn get(name: &str) -> Option<&'static [u8]> {
    entries().iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, b)| &**b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundle_unpacks_with_unique_names_and_without_the_exclusions() {
        let e = entries();
        assert_eq!(e.len(), BUNDLE_COUNT);
        assert!(e.len() > 250, "only {} templates bundled", e.len());
        let mut names: Vec<String> = e.iter().map(|(n, _)| n.to_ascii_lowercase()).collect();
        names.dedup();
        assert_eq!(names.len(), e.len());
        for gone in ["mds.bt", "p5r_tbl.bt", "python.bt", "inspector.bt"] {
            assert!(!names.contains(&gone.to_string()), "{gone} should not be bundled");
        }
        assert!(get("zip.BT").is_some_and(|b| b.starts_with(b"//")));
        assert!(e.iter().all(|(_, b)| !b.contains(&b'\r')));
    }
}
