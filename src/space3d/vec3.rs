//! The small amount of 3D math the space view needs.
//!
//! Hand-rolled rather than pulled from a crate: no matrices are required at
//! all. A camera basis is three orthonormal vectors, world→view is three dot
//! products, and the projection is one divide. That is about a hundred lines,
//! against a dependency of tens of thousands — and the project's Cargo.toml is
//! emphatic about staying pure Rust for the ARM cross-builds.

/// A point or direction in world space. `+Y` is up.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct V3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

pub const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl V3 {
    pub fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
    pub fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
    pub fn scale(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }
    pub fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: V3) -> V3 {
        v3(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    pub fn len(self) -> f32 {
        self.dot(self).sqrt()
    }
    /// Unit vector, or `+X` for a degenerate input so callers never see NaN.
    pub fn norm(self) -> V3 {
        let l = self.len();
        if l <= 1e-6 { v3(1.0, 0.0, 0.0) } else { self.scale(1.0 / l) }
    }
    pub fn lerp(self, o: V3, t: f32) -> V3 {
        self.add(o.sub(self).scale(t))
    }
}

/// An orthonormal camera frame plus its position.
#[derive(Debug, Clone, Copy)]
pub struct Basis {
    pub right: V3,
    pub up: V3,
    /// Direction the camera looks along.
    pub fwd: V3,
    pub eye: V3,
}

pub fn look_at(eye: V3, target: V3, world_up: V3) -> Basis {
    let fwd = target.sub(eye).norm();
    // Guard the degenerate case of looking straight along `world_up`, where the
    // cross product collapses.
    let mut right = fwd.cross(world_up);
    if right.len() <= 1e-5 {
        right = fwd.cross(v3(0.0, 0.0, 1.0));
    }
    let right = right.norm();
    let up = right.cross(fwd).norm();
    Basis { right, up, fwd, eye }
}

/// World → camera space: three dot products, no 4×4 matrix. The result's `z` is
/// distance along the view direction (positive in front of the camera).
pub fn to_view(b: &Basis, p: V3) -> V3 {
    let d = p.sub(b.eye);
    v3(d.dot(b.right), d.dot(b.up), d.dot(b.fwd))
}

/// Camera space → pixel coordinates, returning `(x, y, inv_z)`.
///
/// `inv_z` — not `z` — is what the depth buffer and any across-the-span
/// interpolation must use: `1/z` is the quantity that varies linearly in screen
/// space under perspective, so interpolating `z` instead produces visibly wrong
/// occlusion on steeply foreshortened faces.
///
/// Returns `None` for points at or behind the eye plane.
pub fn project(v: V3, w: f32, h: f32, focal: f32) -> Option<(f32, f32, f32)> {
    if v.z <= 1e-4 {
        return None;
    }
    let inv_z = 1.0 / v.z;
    // The raster is built with square pixels in both presentation modes (in text
    // mode it is `width × 2·height`, one pixel per half-cell), so a single focal
    // length is correct for both axes and no aspect fudge is needed.
    Some((w * 0.5 + v.x * focal * inv_z, h * 0.5 - v.y * focal * inv_z, inv_z))
}

/// Focal length in pixels for a vertical field of view, given the raster height.
pub fn focal_for(h: f32, fov_y_rad: f32) -> f32 {
    (h * 0.5) / (fov_y_rad * 0.5).tan().max(1e-4)
}

/// Wrap an angle difference into `(-π, π]` so a lerp always takes the short way
/// round. Without this, orbiting past a full turn unwinds the long way.
pub fn wrap_angle(mut d: f32) -> f32 {
    use std::f32::consts::PI;
    while d > PI {
        d -= 2.0 * PI;
    }
    while d <= -PI {
        d += 2.0 * PI;
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::PI;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-4
    }

    #[test]
    fn look_at_builds_an_orthonormal_basis() {
        let b = look_at(v3(3.0, 4.0, 5.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        assert!(close(b.right.len(), 1.0) && close(b.up.len(), 1.0) && close(b.fwd.len(), 1.0));
        assert!(close(b.right.dot(b.up), 0.0), "right ⟂ up");
        assert!(close(b.right.dot(b.fwd), 0.0), "right ⟂ fwd");
        assert!(close(b.up.dot(b.fwd), 0.0), "up ⟂ fwd");
    }

    #[test]
    fn looking_straight_down_still_yields_a_valid_basis() {
        // The degenerate case: view direction parallel to world up.
        let b = look_at(v3(0.0, 5.0, 0.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        assert!(b.right.len().is_finite() && close(b.right.len(), 1.0));
        assert!(close(b.right.dot(b.fwd), 0.0));
    }

    #[test]
    fn the_target_projects_to_the_centre_of_the_raster() {
        let b = look_at(v3(0.0, 0.0, -10.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        let f = focal_for(200.0, PI / 3.0);
        let (x, y, inv_z) = project(to_view(&b, v3(0.0, 0.0, 0.0)), 400.0, 200.0, f).unwrap();
        assert!(close(x, 200.0) && close(y, 100.0), "centre of a 400×200 raster");
        assert!(close(inv_z, 0.1), "10 units away → inv_z 0.1");
    }

    #[test]
    fn a_point_behind_the_eye_does_not_project() {
        let b = look_at(v3(0.0, 0.0, -10.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        let f = focal_for(200.0, PI / 3.0);
        assert!(project(to_view(&b, v3(0.0, 0.0, -20.0)), 400.0, 200.0, f).is_none());
    }

    #[test]
    fn nearer_points_have_larger_inv_z() {
        let b = look_at(v3(0.0, 0.0, -10.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        let f = focal_for(200.0, PI / 3.0);
        let near = project(to_view(&b, v3(0.0, 0.0, -5.0)), 400.0, 200.0, f).unwrap().2;
        let far = project(to_view(&b, v3(0.0, 0.0, 5.0)), 400.0, 200.0, f).unwrap().2;
        assert!(near > far, "the depth test keeps the nearer surface");
    }

    #[test]
    fn a_point_above_the_target_projects_above_the_centre() {
        let b = look_at(v3(0.0, 0.0, -10.0), v3(0.0, 0.0, 0.0), v3(0.0, 1.0, 0.0));
        let f = focal_for(200.0, PI / 3.0);
        let (_, y, _) = project(to_view(&b, v3(0.0, 1.0, 0.0)), 400.0, 200.0, f).unwrap();
        assert!(y < 100.0, "+Y is up on screen, so a smaller row index");
    }

    #[test]
    fn wrap_angle_takes_the_short_way_round() {
        assert!(close(wrap_angle(0.2), 0.2));
        // 350° apart the short way is −10°, not +350°.
        assert!(close(wrap_angle(2.0 * PI - 0.1), -0.1));
        assert!(close(wrap_angle(-2.0 * PI + 0.1), 0.1));
        assert!(wrap_angle(PI * 3.0).abs() <= PI + 1e-4);
    }

    #[test]
    fn norm_of_a_zero_vector_is_finite() {
        let n = v3(0.0, 0.0, 0.0).norm();
        assert!(n.x.is_finite() && n.y.is_finite() && n.z.is_finite());
    }
}
