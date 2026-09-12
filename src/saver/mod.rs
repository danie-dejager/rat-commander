//! The screensaver: after a while with no key press or mouse movement, the
//! screen gives way to an animation, as Norton Commander's did — until any key
//! or mouse movement brings everything back.
//!
//! **Text only, on purpose.** Every animation draws characters into the frame
//! buffer and advances on the app's existing 100 ms tick, so a screen nobody is
//! looking at costs a few hundred cells of diff ten times a second — no pixel
//! graphics, no faster frame clock.

mod clock;
mod matrix;
mod pipes;
mod stars;

#[cfg(test)]
mod tests;

use crate::config::SaverKind;
use crate::util::rng::Rng;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Style};

/// The black the animations play on.
pub const BLACK: Color = Color::Rgb(0, 0, 0);

pub struct Saver {
    anim: Anim,
    rng: Rng,
    /// The area the animation was laid out for; a different one (a resize)
    /// starts it afresh.
    area: Rect,
}

enum Anim {
    Stars(stars::Stars),
    Matrix(matrix::Matrix),
    Clock(clock::Clock),
    Pipes(pipes::Pipes),
}

impl Saver {
    /// A saver playing `kind`, with `Random` resolved to one of the others.
    pub fn new(kind: SaverKind, mut rng: Rng) -> Self {
        let kind = match kind {
            SaverKind::Random => match rng.below(4) {
                0 => SaverKind::Starfield,
                1 => SaverKind::Matrix,
                2 => SaverKind::Clock,
                _ => SaverKind::Pipes,
            },
            k => k,
        };
        let anim = match kind {
            SaverKind::Matrix => Anim::Matrix(matrix::Matrix::default()),
            SaverKind::Clock => Anim::Clock(clock::Clock::default()),
            SaverKind::Pipes => Anim::Pipes(pipes::Pipes::default()),
            _ => Anim::Stars(stars::Stars::default()),
        };
        Saver { anim, rng, area: Rect::default() }
    }

    /// Which animation is playing.
    #[cfg(test)]
    pub fn kind(&self) -> SaverKind {
        match self.anim {
            Anim::Stars(_) => SaverKind::Starfield,
            Anim::Matrix(_) => SaverKind::Matrix,
            Anim::Clock(_) => SaverKind::Clock,
            Anim::Pipes(_) => SaverKind::Pipes,
        }
    }

    /// Advance one tick. Nothing moves before the first draw has said how big
    /// the screen is.
    pub fn step(&mut self) {
        if self.area.is_empty() {
            return;
        }
        let (w, h) = (self.area.width, self.area.height);
        match &mut self.anim {
            Anim::Stars(a) => a.step(w, h, &mut self.rng),
            Anim::Matrix(a) => a.step(w, h, &mut self.rng),
            Anim::Clock(a) => a.step(w, h, &mut self.rng),
            Anim::Pipes(a) => a.step(w, h, &mut self.rng),
        }
    }

    /// Draw over all of `area`.
    pub fn render(&mut self, buf: &mut Buffer, area: Rect) {
        if area != self.area {
            self.area = area;
            let (w, h) = (area.width, area.height);
            match &mut self.anim {
                Anim::Stars(a) => a.reset(w, h, &mut self.rng),
                Anim::Matrix(a) => a.reset(w, h, &mut self.rng),
                Anim::Clock(a) => a.reset(w, h, &mut self.rng),
                Anim::Pipes(a) => a.reset(w, h, &mut self.rng),
            }
        }
        buf.set_style(area, Style::default().bg(BLACK));
        for y in area.top()..area.bottom() {
            for x in area.left()..area.right() {
                buf[(x, y)].set_char(' ');
            }
        }
        let mut put = |x: u16, y: u16, ch: char, fg: Color| {
            if x < area.width && y < area.height {
                buf[(area.x + x, area.y + y)].set_char(ch).set_fg(fg).set_bg(BLACK);
            }
        };
        match &self.anim {
            Anim::Stars(a) => a.draw(area.width, area.height, &mut put),
            Anim::Matrix(a) => a.draw(area.width, area.height, &mut put),
            Anim::Clock(a) => a.draw(area.width, area.height, &mut put),
            Anim::Pipes(a) => a.draw(area.width, area.height, &mut put),
        }
    }
}

/// A grey of brightness `v` (0–255).
fn grey(v: f32) -> Color {
    let v = v.clamp(0.0, 255.0) as u8;
    Color::Rgb(v, v, v)
}
