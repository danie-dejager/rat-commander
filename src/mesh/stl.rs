//! STL, in both of its forms.
//!
//! The format has a famous ambiguity: an ASCII file begins with the word
//! `solid`, and so do plenty of binary ones, because exporters write whatever
//! they like into the 80-byte header. The only reliable discriminator is
//! arithmetic — a binary file's length is exactly `84 + 50·count` — so that is
//! tested first and the leading word is never trusted.

use super::{Tri, push_tri};
use crate::space3d::vec3::v3;

/// Bytes before the triangle count: the 80-byte header.
const HEADER: usize = 80;
/// Bytes per facet: normal + three vertices (12 floats) + a 2-byte attribute.
const FACET: usize = 50;

pub fn parse(bytes: &[u8]) -> Option<Vec<Tri>> {
    match binary_count(bytes) {
        Some(n) => Some(parse_binary(bytes, n)),
        None => parse_ascii(bytes),
    }
}

/// The facet count when `bytes` is a binary STL, else `None`.
///
/// An exact length match is conclusive. Files with trailing junk are accepted
/// too, but only when the count is large enough that the match is not a
/// coincidence — a short ASCII file can otherwise produce a plausible-looking
/// count from whatever four bytes land at offset 80.
fn binary_count(bytes: &[u8]) -> Option<u32> {
    if bytes.len() < HEADER + 4 {
        return None;
    }
    let count = u32::from_le_bytes(bytes[HEADER..HEADER + 4].try_into().ok()?);
    let need = HEADER.checked_add(4)?.checked_add(FACET.checked_mul(count as usize)?)?;
    if need == bytes.len() {
        return Some(count);
    }
    // Trailing bytes after a complete facet table: still binary, provided the
    // table actually fills the file rather than a handful of leading bytes.
    if need < bytes.len() && count > 0 && need >= bytes.len() / 2 {
        return Some(count);
    }
    None
}

fn parse_binary(bytes: &[u8], count: u32) -> Vec<Tri> {
    let mut out = Vec::with_capacity(count as usize);
    for i in 0..count as usize {
        // The facet's own normal (its first three floats) is deliberately
        // skipped; `push_tri` derives one from the winding instead.
        let base = HEADER + 4 + i * FACET + 12;
        let Some(chunk) = bytes.get(base..base + 36) else {
            break;
        };
        let f = |k: usize| f32::from_le_bytes([chunk[k], chunk[k + 1], chunk[k + 2], chunk[k + 3]]);
        push_tri(&mut out, v3(f(0), f(4), f(8)), v3(f(12), f(16), f(20)), v3(f(24), f(28), f(32)));
    }
    out
}

/// Parse the ASCII form by collecting every `vertex` triple in order.
///
/// Reading only the vertices — rather than tracking the
/// `facet`/`outer loop`/`endloop`/`endfacet` nesting — costs nothing and
/// tolerates the malformed-but-common files that omit `outer loop` or run a
/// facet across unusual whitespace. The normals on the `facet` lines would be
/// discarded anyway.
fn parse_ascii(bytes: &[u8]) -> Option<Vec<Tri>> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut pts = Vec::new();
    for line in text.lines() {
        let mut it = line.split_ascii_whitespace();
        if it.next() != Some("vertex") {
            continue;
        }
        let mut c = [0.0f32; 3];
        for slot in &mut c {
            *slot = it.next()?.parse().ok()?;
        }
        pts.push(v3(c[0], c[1], c[2]));
    }
    let mut out = Vec::with_capacity(pts.len() / 3);
    // A trailing partial triple is a truncated file; `as_chunks` drops it.
    for t in pts.as_chunks::<3>().0 {
        push_tri(&mut out, t[0], t[1], t[2]);
    }
    Some(out)
}
