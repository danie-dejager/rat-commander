//! A big clock drifting around the screen, turning to a new colour each time it
//! bounces off an edge.

use crate::util::rng::Rng;
use ratatui::style::Color;

/// The digit and colon shapes, three pixels wide and five tall.
const FONT: [[&str; 5]; 11] = [
    ["███", "█ █", "█ █", "█ █", "███"],
    [" █ ", "██ ", " █ ", " █ ", "███"],
    ["███", "  █", "███", "█  ", "███"],
    ["███", "  █", "███", "  █", "███"],
    ["█ █", "█ █", "███", "  █", "  █"],
    ["███", "█  ", "███", "  █", "███"],
    ["███", "█  ", "███", "█ █", "███"],
    ["███", "  █", "  █", "  █", "  █"],
    ["███", "█ █", "███", "█ █", "███"],
    ["███", "█ █", "███", "  █", "███"],
    ["   ", " █ ", "   ", " █ ", "   "],
];
/// Columns per font pixel: two, since a cell is about twice as tall as wide.
const PIXEL: u16 = 2;
/// Width of `HH:MM:SS` in cells: eight glyphs of three pixels, a pixel between.
const WIDTH: u16 = (8 * 3 + 7) * PIXEL;
const HEIGHT: u16 = 5;

#[derive(Default)]
pub struct Clock {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    hue: f32,
}

impl Clock {
    pub fn reset(&mut self, w: u16, h: u16, rng: &mut Rng) {
        self.x = rng.unit() * w.saturating_sub(WIDTH) as f32;
        self.y = rng.unit() * h.saturating_sub(HEIGHT) as f32;
        self.vx = if rng.chance(0.5) { 0.9 } else { -0.9 };
        self.vy = if rng.chance(0.5) { 0.45 } else { -0.45 };
        self.hue = rng.unit() * 360.0;
    }

    pub fn step(&mut self, w: u16, h: u16, rng: &mut Rng) {
        let (max_x, max_y) = (w.saturating_sub(WIDTH) as f32, h.saturating_sub(HEIGHT) as f32);
        self.x += self.vx;
        self.y += self.vy;
        let mut bounced = false;
        if self.x <= 0.0 || self.x >= max_x {
            self.vx = -self.vx;
            self.x = self.x.clamp(0.0, max_x);
            bounced = true;
        }
        if self.y <= 0.0 || self.y >= max_y {
            self.vy = -self.vy;
            self.y = self.y.clamp(0.0, max_y);
            bounced = true;
        }
        if bounced {
            self.hue = (self.hue + 60.0 + rng.unit() * 120.0) % 360.0;
        }
    }

    pub fn draw(&self, _w: u16, _h: u16, put: &mut impl FnMut(u16, u16, char, Color)) {
        let (hh, mm, ss) = crate::util::localtime::local_hms();
        let text = format!("{hh:02}:{mm:02}:{ss:02}");
        let (r, g, b) = crate::ui::graphics::raster::hsv(self.hue as f64, 0.6, 1.0);
        let color = Color::Rgb(r, g, b);
        let (ox, oy) = (self.x as u16, self.y as u16);
        for (i, ch) in text.chars().enumerate() {
            let shape = match ch {
                ':' => &FONT[10],
                d => &FONT[d.to_digit(10).unwrap_or(0) as usize],
            };
            let gx = ox + i as u16 * 4 * PIXEL;
            for (row, line) in shape.iter().enumerate() {
                for (col, px) in line.chars().enumerate() {
                    if px != ' ' {
                        for dx in 0..PIXEL {
                            put(gx + col as u16 * PIXEL + dx, oy + row as u16, '█', color);
                        }
                    }
                }
            }
        }
    }
}
