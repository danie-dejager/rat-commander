//! One audio file on screen — in the viewer or the Details view — and the
//! controls that act on it.

use super::AudioInfo;
use super::analysis::Handle;
use super::output::{AudioOut, PlayState, TrackId, next_track};
use super::raster::{self, Palette};
use crate::config::AudioDisplay;
use image::RgbaImage;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use std::cell::RefCell;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

/// Seek steps: the arrow keys, the ◀◀/▶▶ buttons and the wheel; Page Up/Down.
pub const SEEK_STEP: Duration = Duration::from_secs(5);
pub const SEEK_PAGE: Duration = Duration::from_secs(30);
/// Volume step for `+`/`-`, the arrows and the wheel over the volume control.
pub const VOLUME_STEP: f32 = 0.05;
/// How often a picture still being analysed is redrawn. Each redraw sends the
/// whole image to the terminal again.
const PROGRESS_REDRAW: Duration = Duration::from_millis(500);
/// Within this of the end, Play starts again from the beginning.
const END_SLACK: Duration = Duration::from_millis(250);

/// A transport control.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Start,
    Back,
    PlayPause,
    Stop,
    Forward,
    VolumeDown,
    VolumeUp,
}

/// Where the controls were drawn last frame, for the mouse.
#[derive(Debug, Clone, Default)]
pub struct AudioHits {
    /// The whole widget: a click anywhere in it belongs to it.
    pub area: Rect,
    pub image: Rect,
    pub progress: Rect,
    pub buttons: Vec<(Rect, Transport)>,
    pub volume_bar: Rect,
}

fn contains(r: Rect, col: u16, row: u16) -> bool {
    r.width > 0 && r.height > 0 && r.contains((col, row).into())
}

/// Where along `r` column `col` falls, `0.0..=1.0` (clamped, so a drag off
/// either end pins to that end).
fn frac_in(r: Rect, col: u16) -> f32 {
    if r.width == 0 {
        return 0.0;
    }
    ((col.saturating_sub(r.x) as f32 + 0.5) / r.width as f32).clamp(0.0, 1.0)
}

/// What the analysis has shown so far, so a picture still filling in is
/// redrawn at a measured pace rather than on every column.
#[derive(Debug, Default)]
struct Shown {
    version: u64,
    at: Option<Instant>,
    /// The finished analysis has been shown.
    complete: bool,
}

pub struct AudioView {
    pub info: AudioInfo,
    track: TrackId,
    out: AudioOut,
    analysis: Handle,
    display: AudioDisplay,
    /// The position while this track is not the one loaded in the output.
    pos: Duration,
    /// Where a drag along the picture is, `0.0..=1.0`. The seek itself waits
    /// for the release: each one runs inside the audio callback.
    scrub: Option<f32>,
    shown: Shown,
    /// The play state the last [`poll`](AudioView::poll) saw, so a change the
    /// output made on its own (reaching the end) still gets drawn.
    seen: PlayState,
    hits: RefCell<AudioHits>,
}

impl AudioView {
    /// Show the file `info` describes, starting its analysis.
    pub fn new(info: AudioInfo, display: AudioDisplay, out: AudioOut) -> AudioView {
        let analysis = Handle::start(&info);
        AudioView {
            info,
            track: next_track(),
            out,
            analysis,
            display,
            pos: Duration::ZERO,
            scrub: None,
            shown: Shown::default(),
            seen: PlayState::Idle,
            hits: RefCell::default(),
        }
    }

    pub fn display(&self) -> AudioDisplay {
        self.display
    }

    pub fn toggle_display(&mut self) {
        self.display = self.display.toggled();
    }

    pub fn state(&self) -> PlayState {
        self.out.state_of(self.track)
    }

    /// Playing, or about to.
    pub fn playing(&self) -> bool {
        matches!(self.state(), PlayState::Opening | PlayState::Playing)
    }

    /// The analysis has not been shown complete yet.
    pub fn analyzing(&self) -> bool {
        !self.shown.complete
    }

    /// Whether the app has to keep ticking for this view: the picture is still
    /// filling in, it is playing, or the output changed state since the last
    /// poll and that has not been drawn.
    pub fn busy(&self) -> bool {
        self.analyzing() || self.playing() || self.state() != self.seen
    }

    /// Analysis progress for display, while it runs.
    pub fn progress(&self) -> Option<f32> {
        if self.shown.complete {
            return None;
        }
        Some(self.analysis.lock().progress().unwrap_or(0.0))
    }

    /// There is no output to play through.
    pub fn unavailable(&self) -> bool {
        self.state() == PlayState::Unavailable
    }

    /// Why, when there is none: the audio system's own words.
    pub fn output_error(&self) -> Option<String> {
        self.out.error()
    }

    /// The decode failed outright: there is nothing to draw or play.
    pub fn failed(&self) -> bool {
        let a = self.analysis.lock();
        a.done && a.failed
    }

    /// Catch up with the analysis and the output, on the app's tick.
    pub fn poll(&mut self, now: Instant) {
        let version = self.analysis.version();
        if version != self.shown.version {
            let done = self.analysis.lock().done;
            if done || self.shown.at.is_none_or(|t| now.duration_since(t) >= PROGRESS_REDRAW) {
                self.shown = Shown { version, at: Some(now), complete: done };
            }
        }
        let state = self.state();
        match state {
            PlayState::Opening | PlayState::Playing | PlayState::Paused => {
                if let Some(p) = self.out.position_of(self.track) {
                    self.pos = p;
                }
            }
            PlayState::Ended if self.seen != PlayState::Ended => self.pos = self.duration(),
            _ => {}
        }
        self.seen = state;
    }

    /// The file's length: the decoded length once known, else what the
    /// container claims.
    pub fn duration(&self) -> Duration {
        let a = self.analysis.lock();
        a.duration().or(self.info.duration).unwrap_or_else(|| {
            Duration::from_secs_f64(a.frames_done as f64 / a.sample_rate.max(1) as f64)
        })
    }

    /// The play position (where a drag is, while one is under way).
    pub fn position(&self) -> Duration {
        let dur = self.duration();
        if let Some(f) = self.scrub {
            return dur.mul_f32(f);
        }
        let p = match self.state() {
            PlayState::Opening | PlayState::Playing | PlayState::Paused => {
                self.out.position_of(self.track).unwrap_or(self.pos)
            }
            _ => self.pos,
        };
        if dur.is_zero() { p } else { p.min(dur) }
    }

    /// The play position as a fraction of the length.
    pub fn frac(&self) -> f32 {
        let dur = self.duration();
        if dur.is_zero() {
            0.0
        } else {
            (self.position().as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0)
        }
    }

    pub fn volume(&self) -> f32 {
        self.out.volume()
    }

    pub fn set_volume(&self, v: f32) {
        self.out.set_volume(v);
    }

    pub fn toggle_play(&mut self) {
        if self.playing() {
            self.out.pause(self.track);
            return;
        }
        let dur = self.duration();
        let mut from = self.position();
        if !dur.is_zero() && from + END_SLACK >= dur {
            from = Duration::ZERO;
        }
        self.pos = from;
        self.out.play(self.track, &self.info, from);
    }

    /// Pause and go back to the start.
    pub fn stop(&mut self) {
        self.out.pause(self.track);
        self.seek_to(Duration::ZERO);
    }

    pub fn seek_to(&mut self, to: Duration) {
        let dur = self.duration();
        let to = if dur.is_zero() { to } else { to.min(dur) };
        self.pos = to;
        self.out.seek(self.track, to);
    }

    /// Seek by `step`, back when `back`.
    pub fn seek_by(&mut self, step: Duration, back: bool) {
        let p = self.position();
        self.seek_to(if back { p.saturating_sub(step) } else { p + step });
    }

    /// The transport keys. Returns whether the key was one of them.
    ///
    /// Space plays and pauses; ←/→ seek five seconds and Page Up/Down thirty;
    /// Home/End go to either end; `+`/`-` and ↑/↓ change the volume.
    pub fn key(&mut self, key: KeyEvent) -> bool {
        if !(key.modifiers - KeyModifiers::SHIFT).is_empty() {
            return false;
        }
        match key.code {
            KeyCode::Char(' ') => self.toggle_play(),
            KeyCode::Left => self.seek_by(SEEK_STEP, true),
            KeyCode::Right => self.seek_by(SEEK_STEP, false),
            KeyCode::PageUp => self.seek_by(SEEK_PAGE, true),
            KeyCode::PageDown => self.seek_by(SEEK_PAGE, false),
            KeyCode::Home => self.seek_to(Duration::ZERO),
            KeyCode::End => self.seek_to(self.duration()),
            KeyCode::Char('+') | KeyCode::Char('=') | KeyCode::Up => {
                self.set_volume(self.volume() + VOLUME_STEP);
            }
            KeyCode::Char('-') | KeyCode::Char('_') | KeyCode::Down => {
                self.set_volume(self.volume() - VOLUME_STEP);
            }
            _ => return false,
        }
        true
    }

    fn act(&mut self, t: Transport) {
        match t {
            Transport::Start => self.seek_to(Duration::ZERO),
            Transport::Back => self.seek_by(SEEK_STEP, true),
            Transport::PlayPause => self.toggle_play(),
            Transport::Stop => self.stop(),
            Transport::Forward => self.seek_by(SEEK_STEP, false),
            Transport::VolumeDown => self.set_volume(self.volume() - VOLUME_STEP),
            Transport::VolumeUp => self.set_volume(self.volume() + VOLUME_STEP),
        }
    }

    /// A mouse event over the widget. Returns whether it landed on it.
    ///
    /// Pressing on the picture or the progress row starts a drag that moves the
    /// position marker; letting go seeks there, once. The wheel seeks over the
    /// picture and changes the volume over the volume control.
    pub fn mouse(&mut self, ev: MouseEvent) -> bool {
        let h = self.hits.borrow().clone();
        let (col, row) = (ev.column, ev.row);
        let on_timeline = contains(h.image, col, row) || contains(h.progress, col, row);
        let on_volume = contains(h.volume_bar, col, row)
            || h.buttons.iter().any(|(r, t)| {
                matches!(t, Transport::VolumeDown | Transport::VolumeUp) && contains(*r, col, row)
            });
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if on_timeline {
                    self.scrub = Some(frac_in(h.progress, col));
                } else if let Some(&(_, t)) = h.buttons.iter().find(|(r, _)| contains(*r, col, row))
                {
                    self.act(t);
                } else if contains(h.volume_bar, col, row) {
                    self.set_volume(frac_in(h.volume_bar, col));
                } else {
                    return contains(h.area, col, row);
                }
                true
            }
            MouseEventKind::Drag(MouseButton::Left) if self.scrub.is_some() => {
                self.scrub = Some(frac_in(h.progress, col));
                true
            }
            MouseEventKind::Up(MouseButton::Left) if self.scrub.is_some() => {
                if let Some(f) = self.scrub.take() {
                    let to = self.duration().mul_f32(f);
                    self.seek_to(to);
                }
                true
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = ev.kind == MouseEventKind::ScrollUp;
                if on_volume {
                    let step = if up { VOLUME_STEP } else { -VOLUME_STEP };
                    self.set_volume(self.volume() + step);
                } else if on_timeline {
                    // Down moves on through the file, the way scrolling a
                    // document moves on through it.
                    self.seek_by(SEEK_STEP, up);
                } else {
                    return contains(h.area, col, row);
                }
                true
            }
            _ => contains(h.area, col, row),
        }
    }

    /// A drag along the picture is under way.
    pub fn scrubbing(&self) -> bool {
        self.scrub.is_some()
    }

    pub(crate) fn set_hits(&self, hits: AudioHits) {
        *self.hits.borrow_mut() = hits;
    }

    /// Forget where the controls were: the widget is not on screen.
    pub fn clear_hits(&self) {
        *self.hits.borrow_mut() = AudioHits::default();
    }

    #[cfg(test)]
    pub(crate) fn hits(&self) -> AudioHits {
        self.hits.borrow().clone()
    }

    /// Wait for the analysis and show it (tests).
    #[cfg(test)]
    pub(crate) fn settle(&mut self) {
        self.analysis.wait();
        self.poll(Instant::now());
    }

    /// Everything a pixel picture of `w`×`h` depends on. Deliberately not the
    /// play position: see [`raster`].
    pub fn image_sig(&self, w: u32, h: u32, pal: &Palette) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (self.track, self.shown.version, w, h, self.display, pal).hash(&mut hasher);
        hasher.finish()
    }

    /// The picture at `w`×`h`, with a play-position line when `playhead`.
    pub fn build_image(&self, w: u32, h: u32, pal: &Palette, playhead: bool) -> RgbaImage {
        let mut img = {
            let a = self.analysis.lock();
            match self.display {
                AudioDisplay::Spectrogram => raster::spectrogram(&a, w, h, pal),
                AudioDisplay::Waveform => raster::waveform(&a, w, h, pal),
            }
        };
        if playhead {
            let c = match self.display {
                AudioDisplay::Spectrogram => (235, 235, 235),
                AudioDisplay::Waveform => pal.head,
            };
            raster::playhead(&mut img, self.frac(), c);
        }
        img
    }
}

impl Drop for AudioView {
    fn drop(&mut self) {
        // Closing the view silences it. (Dropping the analysis handle stops
        // the decode.)
        self.out.unload(self.track);
    }
}
