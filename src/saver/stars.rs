//! Norton Commander's screensaver: flying forward through a field of stars that
//! stream out from the middle of the screen, growing brighter as they pass.

use super::grey;
use crate::util::rng::Rng;
use ratatui::style::Color;

/// How far a star comes closer each tick; from the far plane to the eye takes
/// about five seconds.
const SPEED: f32 = 0.02;
/// Nearest a star gets before it is sent back to the far plane.
const NEAR: f32 = 0.03;

#[derive(Default)]
pub struct Stars {
    stars: Vec<Star>,
}

#[derive(Clone, Copy)]
struct Star {
    x: f32,
    y: f32,
    /// Depth: 1 far away, towards 0 as it approaches.
    z: f32,
}

impl Stars {
    pub fn reset(&mut self, w: u16, h: u16, rng: &mut Rng) {
        let count = (w as usize * h as usize / 22).clamp(30, 1200);
        self.stars = (0..count)
            .map(|_| {
                let mut s = spawn(rng);
                // Spread the first ones through the whole depth, so the field
                // starts full rather than as a burst from the middle.
                s.z = NEAR + rng.unit() * (1.0 - NEAR);
                s
            })
            .collect();
    }

    pub fn step(&mut self, w: u16, h: u16, rng: &mut Rng) {
        for s in &mut self.stars {
            s.z -= SPEED;
            if s.z < NEAR || project(s, w, h).is_none() {
                *s = spawn(rng);
            }
        }
    }

    pub fn draw(&self, w: u16, h: u16, put: &mut impl FnMut(u16, u16, char, Color)) {
        // Far stars first, so a near one passing in front wins the cell.
        let mut order: Vec<&Star> = self.stars.iter().collect();
        order.sort_by(|a, b| b.z.total_cmp(&a.z));
        for s in order {
            let Some((x, y)) = project(s, w, h) else { continue };
            let near = 1.0 - s.z;
            // A close star moves several cells a tick: a short trail back towards
            // where it came from gives it the streak of speed.
            if near > 0.6 {
                for (i, back) in [2.5f32, 5.0].into_iter().enumerate() {
                    let behind = Star { z: (s.z + SPEED * back).min(1.0), ..*s };
                    if let Some((tx, ty)) = project(&behind, w, h)
                        && (tx, ty) != (x, y)
                    {
                        put(tx, ty, '·', grey(150.0 * near - 45.0 * i as f32));
                    }
                }
            }
            let glyph = match near {
                n if n < 0.3 => '.',
                n if n < 0.55 => '∙',
                n if n < 0.75 => '+',
                _ => '*',
            };
            put(x, y, glyph, grey(100.0 + 155.0 * near))
        }
    }
}

fn spawn(rng: &mut Rng) -> Star {
    Star { x: rng.unit() * 2.0 - 1.0, y: rng.unit() * 2.0 - 1.0, z: 1.0 }
}

/// Where a star lands on a `w`×`h` screen, if on it. Dividing by depth is the
/// whole perspective; spreading over the half-width and half-height keeps the
/// field round-ish on cells about twice as tall as they are wide.
fn project(s: &Star, w: u16, h: u16) -> Option<(u16, u16)> {
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let x = cx + s.x / s.z * cx;
    let y = cy + s.y / s.z * cy;
    (x >= 0.0 && y >= 0.0 && x < w as f32 && y < h as f32).then_some((x as u16, y as u16))
}
