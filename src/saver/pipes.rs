//! Pipes: a few pipes growing across the screen in heavy box-drawing lines,
//! turning at random, until the screen is mostly full and it starts again.

use crate::util::rng::Rng;
use ratatui::style::Color;

/// Pipes growing at once.
const PIPES: usize = 3;
/// Chance per cell that a pipe turns.
const TURN: f32 = 0.14;
/// Cells each pipe grows per tick.
const PER_TICK: usize = 2;
/// Share of the screen that has to be covered before it is wiped.
const FULL: f32 = 0.55;
const COLORS: [Color; 7] = [
    Color::Rgb(255, 85, 85),
    Color::Rgb(85, 255, 120),
    Color::Rgb(90, 160, 255),
    Color::Rgb(255, 220, 80),
    Color::Rgb(230, 110, 255),
    Color::Rgb(80, 230, 230),
    Color::Rgb(240, 240, 240),
];

#[derive(Default)]
pub struct Pipes {
    w: u16,
    h: u16,
    /// What each cell has been drawn with.
    cells: Vec<Option<(char, Color)>>,
    filled: usize,
    heads: Vec<Head>,
}

#[derive(Clone, Copy)]
struct Head {
    x: i32,
    y: i32,
    dir: Dir,
    color: Color,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Up,
    Right,
    Down,
    Left,
}

impl Dir {
    fn delta(self) -> (i32, i32) {
        match self {
            Dir::Up => (0, -1),
            Dir::Right => (1, 0),
            Dir::Down => (0, 1),
            Dir::Left => (-1, 0),
        }
    }

    fn turn(self, clockwise: bool) -> Dir {
        match (self, clockwise) {
            (Dir::Up, true) | (Dir::Down, false) => Dir::Right,
            (Dir::Right, true) | (Dir::Left, false) => Dir::Down,
            (Dir::Down, true) | (Dir::Up, false) => Dir::Left,
            (Dir::Left, true) | (Dir::Right, false) => Dir::Up,
        }
    }
}

/// The piece for a cell a pipe enters travelling `from` and leaves travelling
/// `to`.
fn piece(from: Dir, to: Dir) -> char {
    use Dir::*;
    match (from, to) {
        (Up, Up) | (Down, Down) => '┃',
        (Left, Left) | (Right, Right) => '━',
        (Right, Down) | (Up, Left) => '┓',
        (Right, Up) | (Down, Left) => '┛',
        (Left, Down) | (Up, Right) => '┏',
        (Left, Up) | (Down, Right) => '┗',
        // A U-turn never happens: a turn is always a quarter.
        _ => '╋',
    }
}

impl Pipes {
    pub fn reset(&mut self, w: u16, h: u16, rng: &mut Rng) {
        self.w = w;
        self.h = h;
        self.cells = vec![None; w as usize * h as usize];
        self.filled = 0;
        self.heads = (0..PIPES).map(|_| new_head(w, h, rng)).collect();
    }

    pub fn step(&mut self, w: u16, h: u16, rng: &mut Rng) {
        if self.cells.len() != w as usize * h as usize || self.cells.is_empty() {
            return;
        }
        if self.filled as f32 > self.cells.len() as f32 * FULL {
            self.reset(w, h, rng);
        }
        for _ in 0..PER_TICK {
            for i in 0..self.heads.len() {
                let mut head = self.heads[i];
                let to = if rng.chance(TURN) { head.dir.turn(rng.chance(0.5)) } else { head.dir };
                let at = head.y as usize * w as usize + head.x as usize;
                if self.cells[at].is_none() {
                    self.filled += 1;
                }
                self.cells[at] = Some((piece(head.dir, to), head.color));
                let (dx, dy) = to.delta();
                head.dir = to;
                head.x += dx;
                head.y += dy;
                // Off an edge: that pipe is done, and a new one starts elsewhere.
                if head.x < 0 || head.y < 0 || head.x >= w as i32 || head.y >= h as i32 {
                    head = new_head(w, h, rng);
                }
                self.heads[i] = head;
            }
        }
    }

    pub fn draw(&self, w: u16, h: u16, put: &mut impl FnMut(u16, u16, char, Color)) {
        if w != self.w || h != self.h {
            return;
        }
        for (i, cell) in self.cells.iter().enumerate() {
            if let Some((ch, color)) = cell {
                put((i % w as usize) as u16, (i / w as usize) as u16, *ch, *color);
            }
        }
    }
}

/// A pipe starting at a random cell, heading a random way, in a random colour.
fn new_head(w: u16, h: u16, rng: &mut Rng) -> Head {
    let dir = [Dir::Up, Dir::Right, Dir::Down, Dir::Left][rng.below(4) as usize];
    Head {
        x: rng.below(w.max(1) as u64) as i32,
        y: rng.below(h.max(1) as u64) as i32,
        dir,
        color: COLORS[rng.below(COLORS.len() as u64) as usize],
    }
}
