//! Matrix rain: streams of glyphs falling down the screen at their own speeds,
//! a bright head leaving a green trail that fades behind it.

use crate::util::rng::Rng;
use ratatui::style::Color;

/// Chance per tick that an empty column starts a new stream.
const SPAWN: f32 = 0.04;
/// Share of the screen's glyphs swapped for others each tick.
const FLICKER: f32 = 0.02;

#[derive(Default)]
pub struct Matrix {
    /// One slot per column: the stream falling in it, if any.
    drops: Vec<Option<Drop>>,
    /// The glyph each cell shows when a stream passes over it.
    glyphs: Vec<char>,
}

struct Drop {
    /// Row of the head, fractional so slow streams move smoothly.
    head: f32,
    /// Rows per tick.
    speed: f32,
    /// Trail length, in rows.
    len: u16,
}

impl Matrix {
    pub fn reset(&mut self, w: u16, h: u16, rng: &mut Rng) {
        self.glyphs = (0..w as usize * h as usize).map(|_| glyph(rng)).collect();
        self.drops = (0..w)
            .map(|_| {
                // Start part-way through, so the screen isn't empty at first.
                rng.chance(0.4).then(|| {
                    let mut d = new_drop(h, rng);
                    d.head = rng.unit() * h as f32;
                    d
                })
            })
            .collect();
    }

    pub fn step(&mut self, _w: u16, h: u16, rng: &mut Rng) {
        for slot in &mut self.drops {
            match slot {
                Some(d) => {
                    d.head += d.speed;
                    if d.head - d.len as f32 > h as f32 {
                        *slot = None;
                    }
                }
                None if rng.chance(SPAWN) => *slot = Some(new_drop(h, rng)),
                None => {}
            }
        }
        let flicker = (self.glyphs.len() as f32 * FLICKER) as usize;
        for _ in 0..flicker.max(1) {
            if self.glyphs.is_empty() {
                break;
            }
            let i = rng.below(self.glyphs.len() as u64) as usize;
            self.glyphs[i] = glyph(rng);
        }
    }

    pub fn draw(&self, w: u16, h: u16, put: &mut impl FnMut(u16, u16, char, Color)) {
        for (x, drop) in self.drops.iter().enumerate().take(w as usize) {
            let Some(d) = drop else { continue };
            let head = d.head as i32;
            for y in (head - d.len as i32).max(0)..=head.min(h as i32 - 1) {
                let behind = (head - y) as f32;
                let color = if behind < 1.0 {
                    Color::Rgb(215, 255, 215)
                } else {
                    let fade = 1.0 - behind / d.len as f32;
                    Color::Rgb(0, (60.0 + 195.0 * fade) as u8, (25.0 + 60.0 * fade) as u8)
                };
                let ch = self.glyphs[y as usize * w as usize + x];
                put(x as u16, y as u16, ch, color);
            }
        }
    }
}

fn new_drop(h: u16, rng: &mut Rng) -> Drop {
    Drop {
        head: 0.0,
        speed: 0.35 + rng.unit() * 1.1,
        len: 4 + rng.below((h / 2).max(1) as u64) as u16,
    }
}

/// A glyph from the rain's alphabet: half-width katakana (one cell wide), digits
/// and a few symbols.
fn glyph(rng: &mut Rng) -> char {
    const EXTRA: &[char] = &['0', '1', '2', '3', '4', '5', '7', '8', '9', 'Z', ':', '=', '*', '+'];
    if rng.chance(0.75) {
        char::from_u32(0xFF66 + rng.below(0xFF9D - 0xFF66 + 1) as u32).unwrap_or('0')
    } else {
        EXTRA[rng.below(EXTRA.len() as u64) as usize]
    }
}
