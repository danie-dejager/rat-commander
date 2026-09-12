//! Wavefront OBJ — the `v` and `f` lines, and nothing else.
//!
//! Materials, texture coordinates, vertex normals, groups and smoothing are all
//! skipped: the viewer shades flat from the geometry, so they would be parsed
//! only to be thrown away. What *is* handled carefully is index addressing,
//! because OBJ allows three forms per corner (`v`, `v/vt`, `v//vn`, `v/vt/vn`)
//! and permits negative indices counting back from the most recent vertex.

use super::{Tri, push_tri};
use crate::space3d::vec3::{V3, v3};

pub fn parse(bytes: &[u8]) -> Option<Vec<Tri>> {
    // OBJ is nominally ASCII but comments and object names carry anything; a
    // lossy decode keeps a file with a Latin-1 comment readable instead of
    // refusing it over bytes no geometry depends on.
    let text = String::from_utf8_lossy(bytes);
    let mut verts: Vec<V3> = Vec::new();
    let mut out = Vec::new();
    let mut face: Vec<V3> = Vec::new();

    for line in text.lines() {
        let line = line.trim_start();
        // `v` must not also match `vt`/`vn`, so split first and compare the
        // whole keyword rather than testing a prefix.
        let mut it = line.split_ascii_whitespace();
        match it.next() {
            Some("v") => {
                let mut c = [0.0f32; 3];
                for slot in &mut c {
                    // A vertex line short of three coordinates is malformed;
                    // treat the file as unparseable rather than silently
                    // shifting every later index by one.
                    *slot = it.next()?.parse().ok()?;
                }
                verts.push(v3(c[0], c[1], c[2]));
            }
            Some("f") => {
                face.clear();
                for tok in it {
                    // Only the first field (the vertex index) matters.
                    let idx: i64 = tok.split('/').next()?.parse().ok()?;
                    face.push(*resolve(&verts, idx)?);
                }
                // Fan-triangulate. OBJ faces are planar and convex by
                // convention, which is exactly when a fan is correct.
                for i in 1..face.len().saturating_sub(1) {
                    push_tri(&mut out, face[0], face[i], face[i + 1]);
                }
            }
            _ => {}
        }
    }
    Some(out)
}

/// Resolve an OBJ face index: 1-based from the start, or negative counting back
/// from the most recently defined vertex (`-1` is the last one).
fn resolve(verts: &[V3], idx: i64) -> Option<&V3> {
    let n = verts.len() as i64;
    let at = if idx < 0 { n + idx } else { idx - 1 };
    usize::try_from(at).ok().and_then(|i| verts.get(i))
}
