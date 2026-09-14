//! Drawing an [`Analysis`] as pixels: the spectrogram and the waveform.
//!
//! Both map the picture's full width onto the file's length, so a column of
//! pixels is a stretch of time and a click at a column seeks to it. Neither
//! draws the play position — a moving line would mean re-sending the whole
//! picture to the terminal every time it moved. The half-block fallback, which
//! is cheap to redraw, adds one with [`playhead`].

use super::analysis::{Analysis, BANDS};
use crate::ui::graphics::raster::{Rgb, canvas, over, rgb};
use crate::ui::theme::Theme;
use image::{Rgba, RgbaImage};
use std::sync::LazyLock;

/// The loudness range the spectrogram's colour scale spans, down from the
/// loudest band in the file. Quieter than this is drawn as silence.
const RANGE_DB: f32 = 80.0;

/// The theme colours a picture uses (hashed into its cache signature, so a
/// theme change redraws it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Palette {
    pub bg: Rgb,
    /// The waveform's peak envelope.
    pub wave: Rgb,
    /// The waveform's RMS core.
    pub core: Rgb,
    /// The zero line.
    pub axis: Rgb,
    /// The play-position line over a waveform (the half-block fallback only).
    pub head: Rgb,
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Palette {
        let bg = rgb(theme.panel_bg);
        let fg = rgb(theme.media_fg);
        Palette {
            bg,
            wave: over(bg, fg, 0.55),
            core: fg,
            axis: over(bg, rgb(theme.panel_border), 0.5),
            head: rgb(theme.panel_fg),
        }
    }
}

/// An inferno-like ramp: black through purple and red to pale yellow, which
/// stays readable as brightness alone (so the ASCII fallback keeps the shape).
static RAMP: LazyLock<[Rgb; 256]> = LazyLock::new(|| {
    const STOPS: [(f32, Rgb); 9] = [
        (0.0, (0, 0, 4)),
        (0.13, (31, 12, 72)),
        (0.25, (85, 15, 109)),
        (0.38, (136, 34, 106)),
        (0.5, (186, 54, 85)),
        (0.63, (227, 89, 51)),
        (0.75, (249, 140, 10)),
        (0.88, (249, 201, 50)),
        (1.0, (252, 255, 164)),
    ];
    let mut out = [(0, 0, 0); 256];
    for (i, px) in out.iter_mut().enumerate() {
        let t = i as f32 / 255.0;
        let j = STOPS.iter().rposition(|(s, _)| *s <= t).unwrap_or(0).min(STOPS.len() - 2);
        let ((t0, a), (t1, b)) = (STOPS[j], STOPS[j + 1]);
        let u = ((t - t0) / (t1 - t0)).clamp(0.0, 1.0);
        let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * u).round() as u8;
        *px = (l(a.0, b.0), l(a.1, b.1), l(a.2, b.2));
    }
    out
});

/// The colour for a normalised loudness in `[0, 1]`.
pub fn ramp(t: f32) -> Rgb {
    RAMP[(t.clamp(0.0, 1.0) * 255.0).round() as usize]
}

/// The run of analysis columns (each `hop` frames long) that pixel column `x`
/// of `w` covers, when the width stands for `extent` frames. `None` past what
/// has been analysed so far.
fn columns_at(x: u32, w: u32, extent: u64, hop: u64, have: usize) -> Option<(usize, usize)> {
    let f0 = x as u64 * extent / w as u64;
    let f1 = (x as u64 + 1) * extent / w as u64;
    let c0 = (f0 / hop.max(1)) as usize;
    if c0 >= have {
        return None;
    }
    let c1 = (f1.div_ceil(hop.max(1)) as usize).clamp(c0 + 1, have);
    Some((c0, c1))
}

/// The spectrogram: time across, frequency up (log-scaled), loudness as colour.
pub fn spectrogram(a: &Analysis, w: u32, h: u32, pal: &Palette) -> RgbaImage {
    let mut img = canvas(w, h, pal.bg);
    let (w, h) = (img.width(), img.height());
    let extent = a.extent();
    let cols = a.spec_cols();
    if extent == 0 || cols == 0 {
        return img;
    }
    let floor = a.peak_db - RANGE_DB;
    let mut col = [0f32; BANDS];
    for x in 0..w {
        let Some((c0, c1)) = columns_at(x, w, extent, a.spec_hop, cols) else { break };
        // Several columns under one pixel: keep the loudest of each band, so a
        // short burst is not averaged away when the picture is narrow.
        col.copy_from_slice(a.column(c0));
        for c in c0 + 1..c1 {
            for (v, &n) in col.iter_mut().zip(a.column(c)) {
                *v = v.max(n);
            }
        }
        for y in 0..h {
            // Bands interpolated from the top (highest) down.
            let pos = ((h - 1 - y) as f32 + 0.5) / h as f32 * BANDS as f32 - 0.5;
            let lo = pos.floor().clamp(0.0, (BANDS - 1) as f32) as usize;
            let hi = (lo + 1).min(BANDS - 1);
            let t = (pos - lo as f32).clamp(0.0, 1.0);
            let db = col[lo] + (col[hi] - col[lo]) * t;
            let c = ramp((db - floor) / RANGE_DB);
            img.put_pixel(x, y, Rgba([c.0, c.1, c.2, 255]));
        }
    }
    img
}

/// The waveform: the peak envelope around the zero line, with the RMS level
/// drawn solid inside it.
pub fn waveform(a: &Analysis, w: u32, h: u32, pal: &Palette) -> RgbaImage {
    let mut img = canvas(w, h, pal.bg);
    let (w, h) = (img.width(), img.height());
    let mid = h as f32 / 2.0;
    for x in 0..w {
        img.put_pixel(x, (h / 2).min(h - 1), Rgba([pal.axis.0, pal.axis.1, pal.axis.2, 255]));
    }
    let extent = a.extent();
    if extent == 0 || a.wave.is_empty() {
        return img;
    }
    let half = (mid - 0.5).max(0.5);
    let row = |v: f32| (mid - v.clamp(-1.0, 1.0) * half).round().clamp(0.0, (h - 1) as f32) as u32;
    for x in 0..w {
        let Some((b0, b1)) = columns_at(x, w, extent, a.wave_hop, a.wave.len()) else { break };
        let bins = &a.wave[b0..b1];
        let min = bins.iter().map(|b| b.min).fold(f32::INFINITY, f32::min);
        let max = bins.iter().map(|b| b.max).fold(f32::NEG_INFINITY, f32::max);
        let rms = (bins.iter().map(|b| b.rms * b.rms).sum::<f32>() / bins.len() as f32).sqrt();
        vspan(&mut img, x, row(max), row(min), pal.wave);
        // The RMS band stays inside the envelope (an offset signal can sit
        // entirely on one side of zero).
        let (top, bottom) = (rms.min(max), (-rms).max(min));
        if top >= bottom {
            vspan(&mut img, x, row(top), row(bottom), pal.core);
        }
    }
    img
}

/// A vertical run of pixels from row `y0` to `y1` inclusive (either order).
fn vspan(img: &mut RgbaImage, x: u32, y0: u32, y1: u32, c: Rgb) {
    let (a, b) = if y0 <= y1 { (y0, y1) } else { (y1, y0) };
    for y in a..=b.min(img.height() - 1) {
        img.put_pixel(x, y, Rgba([c.0, c.1, c.2, 255]));
    }
}

/// A one-pixel vertical line at `frac` of the width.
pub fn playhead(img: &mut RgbaImage, frac: f32, c: Rgb) {
    let w = img.width();
    let x = ((frac.clamp(0.0, 1.0) * w as f32) as u32).min(w - 1);
    vspan(img, x, 0, img.height() - 1, c);
}
