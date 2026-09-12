//! Triangle meshes read from model files, for the F3 model viewer.
//!
//! Everything here is hand-rolled and pure Rust, for the same reason
//! [`crate::space3d::vec3`] is: the formats are simple enough that a parser costs
//! a few hundred lines, against crates that would each pull in a dependency tree
//! and put the ARM cross-builds at risk.
//!
//! The output is deliberately a flat triangle soup with one geometric normal per
//! face — exactly what [`crate::space3d::raster3d`] already rasterizes. No
//! indices, no materials, no vertex normals: the viewer shades flat, and a mesh
//! that has been read is never edited, so nothing downstream needs the topology.

use crate::space3d::vec3::{V3, v3};

pub mod obj;
pub mod stl;
#[cfg(test)]
mod tests;

/// Most triangles a mesh may have before it is refused.
///
/// The rasterizer is single-threaded, so this is a responsiveness bound rather
/// than a memory one — the same reasoning that gives the 3D scene its
/// `MAX_NODES` and `MAX_FILE_SOLIDS` caps. A refused file simply opens in the
/// ordinary hex/text view.
pub const MAX_TRIS: usize = 400_000;

/// Largest model file read into memory. Parsing needs the whole file (an STL's
/// facets are fixed-size but an OBJ's indices can reference any earlier vertex),
/// so this bounds the read rather than merely the triangle count.
pub const MAX_MODEL_BYTES: u64 = 192 * 1024 * 1024;

/// One triangle, with the geometric normal of its own winding.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Tri {
    pub v: [V3; 3],
    pub n: V3,
}

/// A parsed model: triangles plus the axis-aligned bounds they span.
#[derive(Debug, Clone)]
pub struct Mesh {
    pub tris: Vec<Tri>,
    pub min: V3,
    pub max: V3,
    /// Which parser produced this, for the viewer header.
    pub format: &'static str,
}

impl Mesh {
    /// Centre of the bounding box — what the camera orbits.
    pub fn centre(&self) -> V3 {
        self.min.add(self.max).scale(0.5)
    }

    /// Radius of the bounding sphere about [`centre`](Mesh::centre), which is
    /// what the camera fit solves against. Never zero, so a single degenerate
    /// triangle cannot produce a divide-by-zero in the fit.
    pub fn radius(&self) -> f32 {
        self.max.sub(self.min).scale(0.5).len().max(1e-4)
    }

    /// A camera looking at the whole model from three-quarters above, and the
    /// distance that exactly frames it (the bounding sphere filling the field of
    /// view, with a margin so the silhouette doesn't touch the edge).
    pub fn framed_camera(&self) -> (crate::space3d::CamPose, f32) {
        let fitted = self.radius() / (crate::space3d::raster3d::FOV_Y * 0.5).sin() * 1.15;
        (
            crate::space3d::CamPose { target: self.centre(), dist: fitted, yaw: 0.6, pitch: 0.45 },
            fitted,
        )
    }
}

/// Whether the viewer can open `name` as a model.
///
/// Narrower than [`crate::util::filetype`]'s `MODEL` list on purpose: that one
/// colours a listing and picks a shape in the 3D landscape, which is worth doing
/// for any model file, while this one promises the viewer can actually parse it.
/// The two grow together as parsers land.
pub fn is_model_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    matches!(ext.as_str(), "stl" | "obj")
}

/// Parse `bytes` as the model format `name`'s extension implies.
///
/// `None` when the file does not parse, has no usable geometry, or exceeds
/// [`MAX_TRIS`] — in every case the viewer falls back to the raw text/hex view,
/// exactly as it does for an image it cannot decode.
pub fn load(bytes: &[u8], name: &str) -> Option<Mesh> {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    let (tris, format) = match ext.as_str() {
        "stl" => (stl::parse(bytes)?, "STL"),
        "obj" => (obj::parse(bytes)?, "OBJ"),
        _ => return None,
    };
    build(tris, format)
}

/// Wrap parsed triangles in a [`Mesh`], computing bounds.
///
/// `None` for an empty mesh or one over [`MAX_TRIS`]; the cap is applied here
/// rather than in each parser so every format gets it.
fn build(tris: Vec<Tri>, format: &'static str) -> Option<Mesh> {
    if tris.is_empty() || tris.len() > MAX_TRIS {
        return None;
    }
    let (mut min, mut max) = (v3(f32::MAX, f32::MAX, f32::MAX), v3(f32::MIN, f32::MIN, f32::MIN));
    for t in &tris {
        for p in t.v {
            min = v3(min.x.min(p.x), min.y.min(p.y), min.z.min(p.z));
            max = v3(max.x.max(p.x), max.y.max(p.y), max.z.max(p.z));
        }
    }
    Some(Mesh { tris, min, max, format })
}

/// Append triangle `a,b,c` to `out` with its geometric normal, dropping it if it
/// encloses no area.
///
/// The normal is always recomputed rather than read from the file. STL stores one
/// per facet and OBJ can reference vertex normals, but exporters write zeroed,
/// unnormalised and outright inverted values often enough that trusting them
/// produces randomly black faces; the winding is the reliable signal.
///
/// The edges are normalised *before* crossing. A raw cross product's magnitude
/// scales with the product of the two edge lengths, so a model authored in
/// microns underflows [`V3::norm`]'s own degeneracy guard and every face comes
/// back with its fallback vector — a mesh lit entirely from one arbitrary
/// direction. Crossing unit edges instead gives `sin θ`, which depends only on
/// the triangle's shape and not on the units it was drawn in, and doubles as the
/// zero-area test: coincident corners and collinear points both drive it to zero.
pub(crate) fn push_tri(out: &mut Vec<Tri>, a: V3, b: V3, c: V3) {
    let n = b.sub(a).norm().cross(c.sub(a).norm());
    // Stated positively, so a NaN coordinate — which compares false against
    // everything — is dropped rather than admitted.
    if n.len() > 1e-6 {
        out.push(Tri { v: [a, b, c], n: n.norm() });
    }
}
