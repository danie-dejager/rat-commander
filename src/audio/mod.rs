//! Audio files: what the viewer (F3) and the Details view draw for them, and
//! the one output they play through.
//!
//! A file is [probed](probe) for its format and tags, then [analysed](analysis)
//! in the background into a spectrogram and a waveform. That analysis is what
//! [`AudioView`] draws — as pixels, half-block cell art or an ASCII ramp — with
//! a progress row and transport controls beneath. Playback goes through the
//! app-wide [`AudioOut`], so only one file is ever heard at a time.
//!
//! Decoding is pure Rust (symphonia) in every build; only playback needs the
//! `audio` feature, which is what links the system audio library.

pub mod analysis;
pub mod output;
pub mod raster;
pub mod view;
pub mod widget;

#[cfg(test)]
pub(crate) mod tests;

pub use output::AudioOut;
pub use view::AudioView;

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::Duration;
use symphonia::core::codecs::CODEC_TYPE_NULL;
use symphonia::core::formats::{FormatOptions, FormatReader, Track};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::{MetadataOptions, StandardTagKey, Tag};
use symphonia::core::probe::Hint;

/// Extensions tried as audio. Only formats symphonia can both read and decode:
/// Opus (in `.opus` and most `.webm`) has no decoder there, so it is left out
/// and those files open the way they always did.
const EXTENSIONS: &[&str] = &[
    "wav", "wave", "flac", "mp3", "mp2", "mp1", "ogg", "oga", "aac", "m4a", "m4b", "aif", "aiff",
    "aifc", "caf", "mka",
];

/// Whether `name` looks like an audio file this module can open (by extension).
pub fn is_audio_name(name: &str) -> bool {
    EXTENSIONS.contains(&hint_of(name).as_str())
}

/// The lower-cased extension of `name`, handed to the decoder as a format hint.
pub fn hint_of(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// What a probe learned about an audio file: enough to describe it, and where
/// it is so the analysis and the output can open it again.
#[derive(Debug, Clone, Default)]
pub struct AudioInfo {
    pub path: PathBuf,
    /// The original name's extension. A fetched temp copy has none of its own.
    pub hint: String,
    /// Short codec name for display (`FLAC`, `MP3`, `PCM`…).
    pub codec: String,
    pub sample_rate: u32,
    /// 0 when the container does not say (MP4 leaves it to the decoder).
    pub channels: u16,
    pub bits: Option<u32>,
    /// From the container's frame count. An MP3 without a Xing header only
    /// estimates this; the analysis knows the real length once it is done.
    pub duration: Option<Duration>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
}

impl AudioInfo {
    /// `44.1 kHz`, `48 kHz`.
    pub fn rate_label(&self) -> String {
        let khz = self.sample_rate as f64 / 1000.0;
        if khz.fract() == 0.0 { format!("{khz:.0} kHz") } else { format!("{khz:.1} kHz") }
    }

    /// `Mono`, `Stereo`, `6 ch` — the first two translated; empty when unknown.
    pub fn channels_label(&self) -> String {
        match self.channels {
            0 => String::new(),
            1 => crate::l10n::trd("Mono"),
            2 => crate::l10n::trd("Stereo"),
            n => format!("{n} ch"),
        }
    }

    /// The one-line format summary: `FLAC 44.1 kHz Stereo 16-bit`.
    pub fn summary(&self) -> String {
        let bits = self.bits.map(|b| format!("{b}-bit")).unwrap_or_default();
        [self.codec.clone(), self.rate_label(), self.channels_label(), bits]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// Open `path` for decoding: the container reader, the track to decode, and
/// any metadata that came before the container (ID3 tags).
pub(crate) fn open(
    path: &Path,
    hint: &str,
) -> Option<(Box<dyn FormatReader>, Track, Vec<Tag>)> {
    let file = File::open(path).ok()?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut h = Hint::new();
    if !hint.is_empty() {
        h.with_extension(hint);
    }
    let mut probed = symphonia::default::get_probe()
        .format(&h, mss, &FormatOptions::default(), &MetadataOptions::default())
        .ok()?;
    let track = probed.format.tracks().iter().find(|t| t.codec_params.codec != CODEC_TYPE_NULL)?;
    let track = track.clone();
    let mut tags: Vec<Tag> = Vec::new();
    if let Some(rev) = probed.metadata.get().as_ref().and_then(|m| m.current()) {
        tags.extend(rev.tags().iter().cloned());
    }
    if let Some(rev) = probed.format.metadata().current() {
        tags.extend(rev.tags().iter().cloned());
    }
    Some((probed.format, track, tags))
}

/// Read an audio file's format, length and tags without decoding it. `None`
/// when it is not audio symphonia can decode — the caller then opens the file
/// the way it would any other.
pub fn probe(path: &Path, hint: &str) -> Option<AudioInfo> {
    let (_, track, tags) = open(path, hint)?;
    let p = &track.codec_params;
    // A decoder has to exist for it, or there is nothing to show.
    let desc = symphonia::default::get_codecs().get_codec(p.codec)?;
    let sample_rate = p.sample_rate.filter(|&r| r > 0)?;
    let codec = if desc.short_name.starts_with("pcm") {
        "PCM".to_string()
    } else {
        desc.short_name.to_ascii_uppercase()
    };
    let duration = p.n_frames.map(|n| Duration::from_secs_f64(n as f64 / sample_rate as f64));
    let tag = |key: StandardTagKey| {
        tags.iter()
            .find(|t| t.std_key == Some(key))
            .map(|t| t.value.to_string().trim().to_string())
            .filter(|s| !s.is_empty())
    };
    Some(AudioInfo {
        path: path.to_path_buf(),
        hint: hint.to_string(),
        codec,
        sample_rate,
        channels: p.channels.map_or(0, |c| c.count() as u16),
        bits: p.bits_per_sample.or(p.bits_per_coded_sample),
        duration,
        title: tag(StandardTagKey::TrackTitle),
        artist: tag(StandardTagKey::Artist),
        album: tag(StandardTagKey::Album),
    })
}

/// A play time as `m:ss`, or `h:mm:ss` from an hour up.
pub fn format_time(d: Duration) -> String {
    let s = d.as_secs();
    let (h, m, s) = (s / 3600, (s / 60) % 60, s % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}
