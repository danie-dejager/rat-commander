//! The binary templates built into the program: `assets/templates/*.bt`, packed
//! by `build.rs` into one zlib bundle and unpacked on first use.

use std::io::Read;
use std::sync::LazyLock;

include!(concat!(env!("OUT_DIR"), "/templates_meta.rs"));

static PACKED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/templates.bin"));

/// A bundled template: file name and content.
pub type Entry = (Box<str>, Box<[u8]>);

/// Every bundled template, sorted by name.
static ENTRIES: LazyLock<Vec<Entry>> = LazyLock::new(unpack);

fn unpack() -> Vec<Entry> {
    let mut out = Vec::with_capacity(BUNDLE_COUNT);
    if PACKED.len() < 12 || &PACKED[..8] != b"RCBT0001" {
        return out;
    }
    let mut payload = Vec::new();
    let _ = flate2::read::ZlibDecoder::new(&PACKED[12..]).read_to_end(&mut payload);
    let mut p = 0usize;
    let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
        let s = payload.get(*p..*p + n)?;
        *p += n;
        Some(s)
    };
    while p < payload.len() {
        let Some(n) = take(&mut p, 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize) else {
            break;
        };
        let Some(name) = take(&mut p, n).map(|b| String::from_utf8_lossy(b).into_owned()) else {
            break;
        };
        let Some(len) =
            take(&mut p, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            break;
        };
        let Some(body) = take(&mut p, len) else { break };
        out.push((name.into_boxed_str(), body.to_vec().into_boxed_slice()));
    }
    out
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
