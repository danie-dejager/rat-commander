//! The background pass that turns an audio file into the two pictures of it: a
//! spectrogram (band energy per time column) and a waveform (the level per
//! time bin).
//!
//! The whole file is decoded once on its own thread, mixed down to mono. Both
//! results have a fixed ceiling on their size whatever the length of the file:
//! when either passes twice its column budget, adjacent columns are folded
//! together and the time each one covers doubles. That keeps an hour-long
//! recording to the same few megabytes as a three-minute song, and it does not
//! depend on the length the container claims — an MP3 without a Xing header
//! only estimates it.
//!
//! Results land column by column in a mutex shared with the UI, with a version
//! counter it polls, so the picture fills in while the decode is still running.

use super::AudioInfo;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use symphonia::core::audio::{SampleBuffer, SignalSpec};
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::dsp::complex::Complex;
use symphonia::core::dsp::fft::Fft;
use symphonia::core::errors::Error;

/// Spectrogram columns kept once the file is analysed (up to twice this while
/// it runs, before a fold).
pub const SPEC_COLS: usize = 2048;
/// Waveform bins kept, likewise.
pub const WAVE_COLS: usize = 4096;
/// Frequency bands per spectrogram column, log-spaced from [`LOW_HZ`] to the
/// Nyquist frequency.
pub const BANDS: usize = 256;
/// Lowest frequency drawn. Below this a 2048-sample transform has only a bin or
/// two to spread over a whole octave of bands.
const LOW_HZ: f32 = 30.0;
/// Samples per transform.
const FFT_SIZE: usize = 2048;
/// Transforms averaged into one spectrogram column, spread evenly across the
/// time it covers, so a column is not just whatever instant it happened to
/// start on.
const FFTS_PER_COL: u64 = 4;
/// Shortest time a column or a bin may cover, so a clip of a second or two does
/// not become thousands of near-identical columns.
const MIN_SPEC_HOP: u64 = 256;
const MIN_WAVE_HOP: u64 = 32;
/// Starting hops when the container does not say how long the file is.
const UNKNOWN_SPEC_HOP: u64 = 1024;
const UNKNOWN_WAVE_HOP: u64 = 512;
/// How often progress (frames decoded) is published while the decode runs.
const PUBLISH_EVERY: Duration = Duration::from_millis(100);

/// One waveform bin: the extremes and the RMS level of the samples it covers.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct WaveBin {
    pub min: f32,
    pub max: f32,
    pub rms: f32,
}

/// What the analysis has produced so far.
#[derive(Debug, Default)]
pub struct Analysis {
    pub sample_rate: u32,
    /// The length the container claims, when it claims one.
    pub total_frames: Option<u64>,
    /// Frames decoded so far; the file's real length once `done`.
    pub frames_done: u64,
    /// Spectrogram columns, [`BANDS`] values each, flattened. Each value is the
    /// band's mean power in dB (relative — only the spread matters).
    pub spec: Vec<f32>,
    /// Frames each spectrogram column covers.
    pub spec_hop: u64,
    pub wave: Vec<WaveBin>,
    /// Frames each waveform bin covers.
    pub wave_hop: u64,
    /// The loudest band value seen, which the colour scale hangs from.
    pub peak_db: f32,
    /// The decode has finished (or failed part-way; what was read stays).
    pub done: bool,
    /// Nothing at all could be decoded.
    pub failed: bool,
}

impl Analysis {
    pub fn spec_cols(&self) -> usize {
        self.spec.len() / BANDS
    }

    /// Spectrogram column `c`, lowest band first.
    pub fn column(&self, c: usize) -> &[f32] {
        &self.spec[c * BANDS..(c + 1) * BANDS]
    }

    /// The frames the full width of a picture stands for: the real length once
    /// the decode is done, and until then the claimed length (so the picture
    /// fills in from the left rather than stretching as it grows).
    pub fn extent(&self) -> u64 {
        if self.done {
            self.frames_done
        } else {
            self.total_frames.unwrap_or(self.frames_done).max(self.frames_done)
        }
    }

    /// How far through the decode is, when the length is known.
    pub fn progress(&self) -> Option<f32> {
        if self.done {
            return Some(1.0);
        }
        let total = self.total_frames.filter(|&t| t > 0)?;
        Some((self.frames_done as f32 / total as f32).min(1.0))
    }

    /// The decoded length, once the whole file has been read.
    pub fn duration(&self) -> Option<Duration> {
        (self.done && !self.failed && self.sample_rate > 0)
            .then(|| Duration::from_secs_f64(self.frames_done as f64 / self.sample_rate as f64))
    }

    /// Append a spectrogram column, folding pairs together when the budget is
    /// reached. Returns whether it folded (so the analyser doubles its hop).
    fn push_column(&mut self, col: &[f32]) -> bool {
        for &v in col {
            if v > self.peak_db {
                self.peak_db = v;
            }
        }
        self.spec.extend_from_slice(col);
        if self.spec_cols() < 2 * SPEC_COLS {
            return false;
        }
        // Average in power, not in dB: two columns, one loud and one silent,
        // should read as half as loud rather than as the midpoint of the scale.
        let pairs = self.spec_cols() / 2;
        for c in 0..pairs {
            for b in 0..BANDS {
                let (x, y) = (self.spec[2 * c * BANDS + b], self.spec[(2 * c + 1) * BANDS + b]);
                self.spec[c * BANDS + b] = db_mean(x, y);
            }
        }
        self.spec.truncate(pairs * BANDS);
        self.spec_hop *= 2;
        true
    }

    /// Append a waveform bin, folding likewise.
    fn push_bin(&mut self, bin: WaveBin) -> bool {
        self.wave.push(bin);
        if self.wave.len() < 2 * WAVE_COLS {
            return false;
        }
        let pairs = self.wave.len() / 2;
        for i in 0..pairs {
            let (a, b) = (self.wave[2 * i], self.wave[2 * i + 1]);
            self.wave[i] = WaveBin {
                min: a.min.min(b.min),
                max: a.max.max(b.max),
                rms: ((a.rms * a.rms + b.rms * b.rms) / 2.0).sqrt(),
            };
        }
        self.wave.truncate(pairs);
        self.wave_hop *= 2;
        true
    }
}

/// The mean of two dB values, taken in power.
fn db_mean(a: f32, b: f32) -> f32 {
    let p = (10f32.powf(a / 10.0) + 10f32.powf(b / 10.0)) / 2.0;
    10.0 * (p + 1e-20).log10()
}

struct Shared {
    data: Mutex<Analysis>,
    version: AtomicU64,
}

/// A running (or finished) analysis. Dropping it stops the decode.
pub struct Handle {
    shared: Arc<Shared>,
    cancel: Arc<AtomicBool>,
}

impl Handle {
    /// Start analysing the file `info` describes on a thread of its own.
    pub fn start(info: &AudioInfo) -> Handle {
        let shared = Arc::new(Shared { data: Mutex::new(Analysis::default()), version: 0.into() });
        let cancel = Arc::new(AtomicBool::new(false));
        let (s, c, info) = (shared.clone(), cancel.clone(), info.clone());
        let spawned = std::thread::Builder::new()
            .name("audio-analysis".into())
            .spawn(move || run(&info, &s, &c));
        if spawned.is_err() {
            let mut a = lock(&shared);
            a.done = true;
            a.failed = true;
        }
        Handle { shared, cancel }
    }

    /// Bumped whenever there is more to draw.
    pub fn version(&self) -> u64 {
        self.shared.version.load(Ordering::Relaxed)
    }

    pub fn lock(&self) -> MutexGuard<'_, Analysis> {
        lock(&self.shared)
    }

    /// Block until the analysis has finished (tests).
    #[cfg(test)]
    pub fn wait(&self) {
        let start = Instant::now();
        while !self.lock().done {
            assert!(start.elapsed() < Duration::from_secs(20), "analysis never finished");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// A poisoned lock only means the analysis thread panicked part-way; what it
/// wrote is still worth drawing.
fn lock(shared: &Shared) -> MutexGuard<'_, Analysis> {
    shared.data.lock().unwrap_or_else(|e| e.into_inner())
}

fn run(info: &AudioInfo, shared: &Shared, cancel: &AtomicBool) {
    let finish = |failed: bool| {
        let mut a = lock(shared);
        a.done = true;
        a.failed = failed;
        drop(a);
        shared.version.fetch_add(1, Ordering::Relaxed);
    };
    let Some((mut format, track, _)) = super::open(&info.path, &info.hint) else {
        return finish(true);
    };
    let Ok(mut decoder) =
        symphonia::default::get_codecs().make(&track.codec_params, &DecoderOptions::default())
    else {
        return finish(true);
    };
    let rate = track.codec_params.sample_rate.unwrap_or(info.sample_rate).max(1);
    let total = track.codec_params.n_frames.filter(|&n| n > 0);
    let spec_hop =
        total.map_or(UNKNOWN_SPEC_HOP, |n| n.div_ceil(SPEC_COLS as u64).max(MIN_SPEC_HOP));
    let wave_hop =
        total.map_or(UNKNOWN_WAVE_HOP, |n| n.div_ceil(WAVE_COLS as u64).max(MIN_WAVE_HOP));
    {
        let mut a = lock(shared);
        a.sample_rate = rate;
        a.total_frames = total;
        a.spec_hop = spec_hop;
        a.wave_hop = wave_hop;
        a.peak_db = f32::NEG_INFINITY;
    }

    let mut spectro = Spectro::new(rate, spec_hop);
    let mut waver = Waver::new(wave_hop);
    let mut samples: Option<(SampleBuffer<f32>, SignalSpec)> = None;
    let mut frames: u64 = 0;
    let mut published = Instant::now();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return;
        }
        let packet = match format.next_packet() {
            Ok(p) => p,
            // The end of the stream, a chained stream we do not follow, or a
            // read error: stop with what was read.
            Err(_) => break,
        };
        if packet.track_id() != track.id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            // A damaged packet is skipped; the rest of the file still counts.
            Err(Error::DecodeError(_)) | Err(Error::IoError(_)) => continue,
            Err(_) => break,
        };
        let spec = *decoded.spec();
        let channels = spec.channels.count().max(1);
        let needed = decoded.capacity() * channels;
        if samples.as_ref().is_none_or(|(b, s)| *s != spec || b.capacity() < needed) {
            samples = Some((SampleBuffer::new(decoded.capacity() as u64, spec), spec));
        }
        let Some((buf, _)) = samples.as_mut() else { continue };
        buf.copy_interleaved_ref(decoded);
        for frame in buf.samples().chunks_exact(channels) {
            let m = frame.iter().sum::<f32>() / channels as f32;
            if let Some(col) = spectro.push(m) {
                let folded = lock(shared).push_column(col);
                if folded {
                    spectro.double_hop();
                }
            }
            if let Some(bin) = waver.push(m) {
                let folded = lock(shared).push_bin(bin);
                if folded {
                    waver.hop *= 2;
                }
            }
            frames += 1;
        }
        if published.elapsed() >= PUBLISH_EVERY {
            lock(shared).frames_done = frames;
            shared.version.fetch_add(1, Ordering::Relaxed);
            published = Instant::now();
        }
    }
    // The last, partly filled column and bin still describe real audio.
    {
        let mut a = lock(shared);
        if let Some(col) = spectro.flush() {
            a.push_column(col);
        }
        if let Some(bin) = waver.flush() {
            a.push_bin(bin);
        }
        a.frames_done = frames;
        if a.peak_db == f32::NEG_INFINITY {
            a.peak_db = 0.0;
        }
    }
    finish(frames == 0);
}

/// How one spectrogram band is read off a transform's power bins.
#[derive(Debug, Clone, Copy)]
enum BandSource {
    /// The band is narrower than a bin: interpolate at its centre.
    Interp { k: usize, frac: f32 },
    /// The mean of the bins whose centres fall inside the band.
    Range { k0: usize, k1: usize },
}

/// The streaming short-time transform behind the spectrogram.
struct Spectro {
    fft: Fft,
    window: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    buf: Vec<Complex>,
    power: Vec<f32>,
    bands: Vec<BandSource>,
    acc: Vec<f64>,
    acc_n: u32,
    col: Vec<f32>,
    hop: u64,
    col_frames: u64,
    since_fft: u64,
}

impl Spectro {
    fn new(rate: u32, hop: u64) -> Spectro {
        let window = (0..FFT_SIZE)
            .map(|i| {
                let x = std::f32::consts::TAU * i as f32 / FFT_SIZE as f32;
                0.5 - 0.5 * x.cos()
            })
            .collect();
        Spectro {
            fft: Fft::new(FFT_SIZE),
            window,
            ring: vec![0.0; FFT_SIZE],
            ring_pos: 0,
            buf: vec![Complex::default(); FFT_SIZE],
            power: vec![0.0; FFT_SIZE / 2],
            bands: band_sources(rate),
            acc: vec![0.0; BANDS],
            acc_n: 0,
            col: vec![0.0; BANDS],
            hop,
            col_frames: 0,
            since_fft: 0,
        }
    }

    fn step(&self) -> u64 {
        (self.hop / FFTS_PER_COL).max(1)
    }

    fn double_hop(&mut self) {
        self.hop *= 2;
    }

    /// Feed one sample; returns a finished column.
    fn push(&mut self, s: f32) -> Option<&[f32]> {
        self.ring[self.ring_pos] = s;
        self.ring_pos = (self.ring_pos + 1) % FFT_SIZE;
        self.col_frames += 1;
        self.since_fft += 1;
        if self.since_fft >= self.step() {
            self.since_fft = 0;
            self.transform();
        }
        if self.col_frames >= self.hop { self.emit() } else { None }
    }

    /// The partial column left at the end of the file.
    fn flush(&mut self) -> Option<&[f32]> {
        if self.col_frames == 0 { None } else { self.emit() }
    }

    fn emit(&mut self) -> Option<&[f32]> {
        if self.acc_n == 0 {
            self.transform();
        }
        let n = self.acc_n.max(1) as f64;
        for (out, acc) in self.col.iter_mut().zip(self.acc.iter_mut()) {
            *out = (10.0 * (*acc / n + 1e-20).log10()) as f32;
            *acc = 0.0;
        }
        self.acc_n = 0;
        self.col_frames = 0;
        self.since_fft = 0;
        Some(&self.col)
    }

    /// Transform the last [`FFT_SIZE`] samples and add their band powers to the
    /// column being built.
    fn transform(&mut self) {
        for (i, c) in self.buf.iter_mut().enumerate() {
            let s = self.ring[(self.ring_pos + i) % FFT_SIZE];
            *c = Complex { re: s * self.window[i], im: 0.0 };
        }
        self.fft.fft_inplace(&mut self.buf);
        for (p, c) in self.power.iter_mut().zip(self.buf.iter()) {
            *p = c.re * c.re + c.im * c.im;
        }
        let last = self.power.len() - 1;
        for (acc, band) in self.acc.iter_mut().zip(self.bands.iter()) {
            let v = match *band {
                BandSource::Interp { k, frac } => {
                    let (a, b) = (self.power[k.min(last)], self.power[(k + 1).min(last)]);
                    a + (b - a) * frac
                }
                BandSource::Range { k0, k1 } => {
                    let bins = &self.power[k0.min(last)..=k1.min(last)];
                    bins.iter().sum::<f32>() / bins.len() as f32
                }
            };
            *acc += v as f64;
        }
        self.acc_n += 1;
    }
}

/// Where each of the [`BANDS`] log-spaced bands reads its power from, for a
/// transform of [`FFT_SIZE`] samples at `rate`.
fn band_sources(rate: u32) -> Vec<BandSource> {
    let nyquist = rate as f32 / 2.0;
    let bin_hz = rate as f32 / FFT_SIZE as f32;
    let low = LOW_HZ.min(nyquist / 4.0).max(bin_hz / 2.0);
    let ratio = (nyquist / low).max(1.0001);
    let edge = |b: usize| low * ratio.powf(b as f32 / BANDS as f32);
    (0..BANDS)
        .map(|b| {
            let (lo, hi) = (edge(b), edge(b + 1));
            // Bins whose centre frequency lies in [lo, hi).
            let k0 = (lo / bin_hz).ceil() as usize;
            let k1 = ((hi / bin_hz).ceil() as usize).saturating_sub(1);
            if k1 > k0 {
                BandSource::Range { k0, k1 }
            } else {
                let pos = (lo * hi).sqrt() / bin_hz;
                BandSource::Interp { k: pos.floor() as usize, frac: pos.fract() }
            }
        })
        .collect()
}

/// The streaming level meter behind the waveform.
struct Waver {
    hop: u64,
    min: f32,
    max: f32,
    sq: f64,
    n: u64,
}

impl Waver {
    fn new(hop: u64) -> Waver {
        Waver { hop, min: f32::INFINITY, max: f32::NEG_INFINITY, sq: 0.0, n: 0 }
    }

    fn push(&mut self, s: f32) -> Option<WaveBin> {
        self.min = self.min.min(s);
        self.max = self.max.max(s);
        self.sq += (s * s) as f64;
        self.n += 1;
        if self.n >= self.hop { self.flush() } else { None }
    }

    fn flush(&mut self) -> Option<WaveBin> {
        if self.n == 0 {
            return None;
        }
        let bin =
            WaveBin { min: self.min, max: self.max, rms: (self.sq / self.n as f64).sqrt() as f32 };
        *self = Waver::new(self.hop);
        Some(bin)
    }
}

/// Run an analysis that was cancelled before it began; whether it finished.
#[cfg(test)]
pub(crate) fn run_cancelled_for_test(info: &AudioInfo) -> bool {
    let shared = Shared { data: Mutex::new(Analysis::default()), version: 0.into() };
    run(info, &shared, &AtomicBool::new(true));
    lock(&shared).done
}

#[cfg(test)]
pub(crate) fn fold_for_test(a: &mut Analysis, col: &[f32]) -> bool {
    a.push_column(col)
}

#[cfg(test)]
pub(crate) fn peak_band_hz(rate: u32, band: usize) -> (f32, f32) {
    let nyquist = rate as f32 / 2.0;
    let bin_hz = rate as f32 / FFT_SIZE as f32;
    let low = LOW_HZ.min(nyquist / 4.0).max(bin_hz / 2.0);
    let ratio = (nyquist / low).max(1.0001);
    let edge = |b: usize| low * ratio.powf(b as f32 / BANDS as f32);
    (edge(band), edge(band + 1))
}
