//! The app's one audio output.
//!
//! [`AudioOut`] is a cheap handle shared by every audio view. Each view is a
//! *track*; playing one replaces whatever track was loaded before, which is
//! what keeps the viewer and the Details view from talking over each other.
//!
//! The device and the rodio player live on a worker thread, started on the
//! first Play and taking commands over a channel. Nothing on the UI thread ever
//! waits on it: opening a device can take a moment, and a rodio seek blocks
//! until the audio callback has carried it out — which, on a stalled device,
//! is never. What the UI reads back (the loaded track, the play state, the
//! position) sits in atomics the worker keeps current.
//!
//! Without the `audio` feature there is no worker: Play only reports that
//! there is no output. Under test there is no device either; the commands are
//! applied to the shared state directly, so the controls can be exercised on a
//! machine with no sound card.

use super::AudioInfo;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
#[cfg(all(feature = "audio", not(test)))]
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Identifies one audio view to the output.
pub type TrackId = u64;

/// A fresh, never-reused track id.
pub fn next_track() -> TrackId {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Where a track is, as far as the output is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// Not loaded (or loaded and then replaced by another track).
    Idle,
    /// Play was pressed; the device or the file is still being opened.
    Opening,
    Playing,
    Paused,
    /// Played through to the end.
    Ended,
    /// There is no output to play through — no device, or a build without the
    /// `audio` feature. [`AudioOut::error`] says why.
    Unavailable,
}

impl PlayState {
    const ALL: [PlayState; 6] = [
        PlayState::Idle,
        PlayState::Opening,
        PlayState::Playing,
        PlayState::Paused,
        PlayState::Ended,
        PlayState::Unavailable,
    ];

    fn to_u8(self) -> u8 {
        Self::ALL.iter().position(|s| *s == self).unwrap_or(0) as u8
    }

    fn from_u8(v: u8) -> PlayState {
        Self::ALL.get(v as usize).copied().unwrap_or(PlayState::Idle)
    }
}

/// What the worker is told to do.
#[cfg_attr(not(all(feature = "audio", not(test))), allow(dead_code))]
enum Cmd {
    Play { track: TrackId, path: PathBuf, hint: String, from: Duration },
    Pause,
    Seek(Duration),
    Volume(f32),
    Unload,
}

/// The state the UI reads, kept current by whoever acts on the commands.
struct Shared {
    /// The loaded track; 0 when none is.
    track: AtomicU64,
    state: AtomicU8,
    position_ms: AtomicU64,
    /// 0.0–1.0, as `f32` bits.
    volume: AtomicU32,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn track(&self) -> TrackId {
        self.track.load(Ordering::Relaxed)
    }

    fn state(&self) -> PlayState {
        PlayState::from_u8(self.state.load(Ordering::Relaxed))
    }

    fn set_state(&self, s: PlayState) {
        self.state.store(s.to_u8(), Ordering::Relaxed);
    }

    /// Set the state only if `track` is still the loaded one — a slow open must
    /// not report on a track the user has since replaced.
    #[cfg_attr(not(all(feature = "audio", not(test))), allow(dead_code))]
    fn set_state_of(&self, track: TrackId, s: PlayState) {
        if self.track() == track {
            self.set_state(s);
        }
    }

    fn set_position(&self, d: Duration) {
        self.position_ms.store(d.as_millis() as u64, Ordering::Relaxed);
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    #[cfg_attr(not(all(feature = "audio", not(test))), allow(dead_code))]
    fn set_error(&self, msg: impl Into<String>) {
        *self.error.lock().unwrap_or_else(|e| e.into_inner()) = Some(msg.into());
    }
}

enum Backend {
    /// A worker thread with a real device, started on the first command.
    #[cfg(all(feature = "audio", not(test)))]
    Device(Mutex<Option<Sender<Cmd>>>),
    /// Tests: commands change the shared state on the spot.
    #[cfg(test)]
    Simulated,
    /// Built without the `audio` feature.
    #[cfg(all(not(feature = "audio"), not(test)))]
    Missing,
}

struct Inner {
    shared: Arc<Shared>,
    backend: Backend,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // Tell the worker to let go of the device, but do not wait for it: a
        // stalled device must not hold up quitting.
        #[cfg(all(feature = "audio", not(test)))]
        {
            let Backend::Device(tx) = &self.backend;
            tx.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
    }
}

/// A handle to the app's audio output; clones share it.
#[derive(Clone)]
pub struct AudioOut {
    inner: Arc<Inner>,
}

impl Default for AudioOut {
    fn default() -> Self {
        AudioOut::new()
    }
}

impl AudioOut {
    pub fn new() -> AudioOut {
        let shared = Arc::new(Shared {
            track: AtomicU64::new(0),
            state: AtomicU8::new(PlayState::Idle.to_u8()),
            position_ms: AtomicU64::new(0),
            volume: AtomicU32::new(1.0f32.to_bits()),
            error: Mutex::new(None),
        });
        #[cfg(all(feature = "audio", not(test)))]
        let backend = Backend::Device(Mutex::new(None));
        #[cfg(test)]
        let backend = Backend::Simulated;
        #[cfg(all(not(feature = "audio"), not(test)))]
        let backend = Backend::Missing;
        AudioOut { inner: Arc::new(Inner { shared, backend }) }
    }

    fn shared(&self) -> &Shared {
        &self.inner.shared
    }

    fn send(&self, cmd: Cmd) {
        let shared = self.shared();
        match &self.inner.backend {
            #[cfg(all(feature = "audio", not(test)))]
            Backend::Device(tx) => {
                let mut tx = tx.lock().unwrap_or_else(|e| e.into_inner());
                if tx.is_none() {
                    *tx = device::spawn(self.inner.shared.clone());
                }
                let sent = tx.as_ref().is_some_and(|t| t.send(cmd).is_ok());
                if !sent {
                    // The worker could not be started, or has died.
                    tx.take();
                    shared.set_error("audio output thread is not running");
                    shared.set_state(PlayState::Unavailable);
                }
            }
            #[cfg(test)]
            Backend::Simulated => match cmd {
                Cmd::Play { track, from, .. } => {
                    shared.track.store(track, Ordering::Relaxed);
                    shared.set_position(from);
                    shared.set_state(PlayState::Playing);
                }
                Cmd::Pause => {
                    if shared.state() == PlayState::Playing {
                        shared.set_state(PlayState::Paused);
                    }
                }
                Cmd::Seek(to) => shared.set_position(to),
                Cmd::Volume(_) => {}
                Cmd::Unload => {
                    shared.track.store(0, Ordering::Relaxed);
                    shared.set_state(PlayState::Idle);
                }
            },
            #[cfg(all(not(feature = "audio"), not(test)))]
            Backend::Missing => {
                if let Cmd::Play { .. } = cmd {
                    shared.set_error("this build has no audio output");
                    shared.set_state(PlayState::Unavailable);
                }
            }
        }
    }

    /// Where `track` is: [`PlayState::Idle`] unless it is the loaded track.
    pub fn state_of(&self, track: TrackId) -> PlayState {
        let s = self.shared();
        if s.track() == track { s.state() } else { PlayState::Idle }
    }

    /// The play position of `track`, when it is the loaded track.
    pub fn position_of(&self, track: TrackId) -> Option<Duration> {
        let s = self.shared();
        (s.track() == track).then(|| Duration::from_millis(s.position_ms.load(Ordering::Relaxed)))
    }

    /// Whether anything is playing, or about to.
    #[cfg(test)]
    pub fn active(&self) -> bool {
        matches!(self.shared().state(), PlayState::Opening | PlayState::Playing)
    }

    /// Load `track` (the file `info` describes) and play it from `from`, or go
    /// on playing it if it is already loaded.
    pub fn play(&self, track: TrackId, info: &AudioInfo, from: Duration) {
        let s = self.shared();
        if s.track() != track {
            s.track.store(track, Ordering::Relaxed);
            s.set_position(from);
        }
        s.set_state(PlayState::Opening);
        self.send(Cmd::Play { track, path: info.path.clone(), hint: info.hint.clone(), from });
    }

    /// Pause `track`, if it is the one playing.
    pub fn pause(&self, track: TrackId) {
        if matches!(self.state_of(track), PlayState::Playing | PlayState::Opening) {
            self.shared().set_state(PlayState::Paused);
            self.send(Cmd::Pause);
        }
    }

    /// Pause whatever is playing — the terminal is being handed to another
    /// program, which should not have to talk over it.
    pub fn pause_all(&self) {
        let track = self.shared().track();
        if track != 0 {
            self.pause(track);
        }
    }

    /// Move `track`'s play position, if it is loaded. (A track that is not only
    /// remembers the position itself, and plays from it.)
    pub fn seek(&self, track: TrackId, to: Duration) {
        let state = self.state_of(track);
        if matches!(state, PlayState::Idle | PlayState::Unavailable) {
            return;
        }
        self.shared().set_position(to);
        if state == PlayState::Ended {
            // Nothing is left in the player to seek: the track waits, paused,
            // and its next Play starts a fresh player from here.
            self.shared().set_state(PlayState::Paused);
        }
        self.send(Cmd::Seek(to));
    }

    /// Forget `track` if it is loaded (its view has closed).
    pub fn unload(&self, track: TrackId) {
        let s = self.shared();
        if s.track() == track {
            s.track.store(0, Ordering::Relaxed);
            s.set_state(PlayState::Idle);
            self.send(Cmd::Unload);
        }
    }

    /// The output volume, 0.0–1.0.
    pub fn volume(&self) -> f32 {
        self.shared().volume()
    }

    pub fn set_volume(&self, v: f32) {
        let v = v.clamp(0.0, 1.0);
        self.shared().volume.store(v.to_bits(), Ordering::Relaxed);
        self.send(Cmd::Volume(v));
    }

    /// Why the output is unavailable, when it is.
    pub fn error(&self) -> Option<String> {
        self.shared().error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// The amplitude a volume setting plays at. Squared, so the lower half of the
/// control is not all "barely quieter": 50% is about −12 dB.
#[cfg_attr(not(all(feature = "audio", not(test))), allow(dead_code))]
fn gain(volume: f32) -> f32 {
    volume * volume
}

#[cfg(all(feature = "audio", not(test)))]
mod device {
    use super::{Cmd, PlayState, Shared, TrackId, gain};
    use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player};
    use std::fs::File;
    use std::io::BufReader;
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
    use std::time::{Duration, Instant};

    /// How often the worker reports the position.
    const REPORT_EVERY: Duration = Duration::from_millis(50);
    /// How long the device stays open with nothing playing. Holding it costs
    /// an audio callback running on silence, and on a system without a sound
    /// server it keeps other programs off the card.
    const RELEASE_AFTER: Duration = Duration::from_secs(30);

    pub(super) fn spawn(shared: Arc<Shared>) -> Option<Sender<Cmd>> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("audio-output".into())
            .spawn(move || run(&rx, &shared))
            .ok()?;
        Some(tx)
    }

    fn run(rx: &Receiver<Cmd>, shared: &Arc<Shared>) {
        let mut sink: Option<MixerDeviceSink> = None;
        let mut player: Option<Player> = None;
        let mut loaded: TrackId = 0;
        let mut quiet_since = Instant::now();
        loop {
            let first = match rx.recv_timeout(REPORT_EVERY) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                // The app has let go of the output.
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut cmds: Vec<Cmd> = first.into_iter().chain(rx.try_iter()).collect();
            // A drag across the picture sends a run of seeks; only the last
            // one is worth the decoder's time.
            cmds.dedup_by(|later, earlier| match (earlier, later) {
                (Cmd::Seek(e), Cmd::Seek(l)) => {
                    *e = *l;
                    true
                }
                _ => false,
            });
            for cmd in cmds {
                match cmd {
                    Cmd::Play { track, path, hint, from } => {
                        if loaded == track
                            && let Some(p) = player.as_ref().filter(|p| !p.empty())
                        {
                            p.play();
                            shared.set_state_of(track, PlayState::Playing);
                            continue;
                        }
                        player = None;
                        loaded = 0;
                        if sink.is_none() {
                            match open_sink(shared) {
                                Ok(s) => sink = Some(s),
                                Err(e) => {
                                    shared.set_error(e);
                                    shared.set_state_of(track, PlayState::Unavailable);
                                    continue;
                                }
                            }
                        }
                        let Some(s) = sink.as_ref() else { continue };
                        match start(s, &path, &hint, from, shared.volume()) {
                            Ok(p) => {
                                player = Some(p);
                                loaded = track;
                                shared.set_state_of(track, PlayState::Playing);
                            }
                            Err(e) => {
                                shared.set_error(e);
                                shared.set_state_of(track, PlayState::Unavailable);
                            }
                        }
                    }
                    Cmd::Pause => {
                        if let Some(p) = player.as_ref() {
                            p.pause();
                            shared.set_state_of(loaded, PlayState::Paused);
                        }
                    }
                    Cmd::Seek(to) => {
                        if let Some(p) = player.as_ref().filter(|p| !p.empty()) {
                            let _ = p.try_seek(to);
                        }
                    }
                    Cmd::Volume(v) => {
                        if let Some(p) = player.as_ref() {
                            p.set_volume(gain(v));
                        }
                    }
                    Cmd::Unload => {
                        player = None;
                        loaded = 0;
                    }
                }
            }
            if let Some(p) = player.as_ref()
                && shared.track() == loaded
            {
                if p.empty() {
                    if shared.state() == PlayState::Playing {
                        shared.set_state(PlayState::Ended);
                    }
                } else {
                    shared.position_ms.store(p.get_pos().as_millis() as u64, Ordering::Relaxed);
                }
            }
            let sounding = player.as_ref().is_some_and(|p| !p.is_paused() && !p.empty());
            if sounding {
                quiet_since = Instant::now();
            } else if sink.is_some() && quiet_since.elapsed() >= RELEASE_AFTER {
                // A paused track stays loaded as far as the UI knows; its next
                // Play reopens the device and picks up from the position.
                player = None;
                loaded = 0;
                sink = None;
            }
        }
    }

    /// Open the default output device.
    fn open_sink(shared: &Arc<Shared>) -> Result<MixerDeviceSink, String> {
        let s = shared.clone();
        // rodio's own error callback prints to stderr, which would land on top
        // of the TUI; record the error instead.
        let on_error = move |e: rodio::cpal::StreamError| s.set_error(e.to_string());
        quiet_stderr(|| {
            let builder = DeviceSinkBuilder::from_default_device().map_err(|e| e.to_string())?;
            let mut sink = builder
                .with_error_callback(on_error)
                .open_sink_or_fallback()
                .map_err(|e| e.to_string())?;
            sink.log_on_drop(false);
            Ok(sink)
        })
    }

    /// A paused player with the file queued at `from`, ready to play.
    fn start(
        sink: &MixerDeviceSink,
        path: &Path,
        hint: &str,
        from: Duration,
        volume: f32,
    ) -> Result<Player, String> {
        let make = |coarse: bool| -> Result<Player, String> {
            let player = Player::connect_new(sink.mixer());
            // Paused while the seek is carried out, so the start of the file
            // does not blip out first.
            player.pause();
            player.set_volume(gain(volume));
            player.append(decoder(path, hint, coarse)?);
            if !from.is_zero() {
                player.try_seek(from).map_err(|e| e.to_string())?;
            }
            Ok(player)
        };
        // An accurate seek needs a time base some streams lack; a coarse one
        // lands near enough.
        let player = make(false).or_else(|_| make(true))?;
        player.play();
        Ok(player)
    }

    fn decoder(path: &Path, hint: &str, coarse: bool) -> Result<Decoder<BufReader<File>>, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let mut builder = Decoder::builder()
            .with_data(BufReader::new(file))
            .with_byte_len(len)
            .with_coarse_seek(coarse);
        if !hint.is_empty() {
            builder = builder.with_hint(hint);
        }
        builder.build().map_err(|e| e.to_string())
    }

    /// Run `f` with stderr pointed at `/dev/null`. The ALSA library reports
    /// what it could not open straight to stderr — a line of text drawn over
    /// the panels.
    #[cfg(unix)]
    fn quiet_stderr<T>(f: impl FnOnce() -> T) -> T {
        let saved = nix::unistd::dup(std::io::stderr()).ok();
        let null = std::fs::OpenOptions::new().write(true).open("/dev/null").ok();
        let silenced = match (&saved, &null) {
            (Some(_), Some(n)) => nix::unistd::dup2_stderr(n).is_ok(),
            _ => false,
        };
        let out = f();
        if silenced && let Some(s) = &saved {
            let _ = nix::unistd::dup2_stderr(s);
        }
        out
    }

    #[cfg(not(unix))]
    fn quiet_stderr<T>(f: impl FnOnce() -> T) -> T {
        f()
    }
}
