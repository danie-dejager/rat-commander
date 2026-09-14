use super::analysis::{self, Analysis, BANDS, Handle, SPEC_COLS, WaveBin};
use super::output::PlayState;
use super::view::{AudioHits, Transport};
use super::widget;
use super::*;
use crate::config::AudioDisplay;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use std::path::PathBuf;
use std::time::Duration;

/// A scratch directory named after the calling test.
pub(crate) fn scratch(tag: &str) -> PathBuf {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("rc_audio_{tag}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Write a 16-bit mono PCM WAV of a `freq` Hz sine at amplitude `amp`.
pub(crate) fn write_wav(path: &std::path::Path, rate: u32, secs: f32, freq: f32, amp: f32) {
    let frames = (rate as f32 * secs) as u32;
    let data_len = frames * 2;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    for i in 0..frames {
        let t = i as f32 / rate as f32;
        let s = (std::f32::consts::TAU * freq * t).sin() * amp;
        b.extend_from_slice(&((s * i16::MAX as f32) as i16).to_le_bytes());
    }
    std::fs::write(path, b).unwrap();
}

fn sine_info(tag: &str, secs: f32) -> AudioInfo {
    let path = scratch(tag).join("tone.wav");
    write_wav(&path, 44_100, secs, 1000.0, 0.5);
    probe(&path, "wav").expect("a PCM WAV probes")
}

fn left_down(col: u16, row: u16) -> MouseEvent {
    MouseEvent { kind: MouseEventKind::Down(MouseButton::Left), column: col, row, modifiers: KeyModifiers::NONE }
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent { kind, column: col, row, modifiers: KeyModifiers::NONE }
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

#[test]
fn audio_names_are_recognised_by_extension() {
    for name in ["a.wav", "B.FLAC", "c.mp3", "d.ogg", "e.m4a", "f.aiff", "g.aac"] {
        assert!(is_audio_name(name), "{name}");
    }
    // Opus has no decoder, and a video container is not audio.
    for name in ["a.opus", "b.mp4", "c.txt", "noext", "wav"] {
        assert!(!is_audio_name(name), "{name}");
    }
    assert_eq!(hint_of("Song.FLAC"), "flac");
}

#[test]
fn play_times_read_as_minutes_or_hours() {
    assert_eq!(format_time(Duration::ZERO), "0:00");
    assert_eq!(format_time(Duration::from_millis(65_900)), "1:05");
    assert_eq!(format_time(Duration::from_secs(3599)), "59:59");
    assert_eq!(format_time(Duration::from_secs(3723)), "1:02:03");
}

#[test]
fn a_wav_probes_for_its_format_and_length() {
    let info = sine_info("probe", 1.5);
    assert_eq!(info.codec, "PCM");
    assert_eq!(info.sample_rate, 44_100);
    assert_eq!(info.channels, 1);
    assert_eq!(info.bits, Some(16));
    let d = info.duration.unwrap().as_secs_f32();
    assert!((d - 1.5).abs() < 0.01, "{d}");
    assert!(info.summary().starts_with("PCM 44.1 kHz"), "{}", info.summary());

    let junk = scratch("probe_junk").join("not.wav");
    std::fs::write(&junk, b"this is not a wave file at all").unwrap();
    assert!(probe(&junk, "wav").is_none());
}

#[test]
fn a_sine_lights_up_the_band_holding_its_frequency() {
    let info = sine_info("spectrum", 2.0);
    let h = Handle::start(&info);
    h.wait();
    let a = h.lock();
    assert!(!a.failed);
    assert_eq!(a.frames_done, 88_200);
    assert!(a.spec_cols() > 16, "{} columns", a.spec_cols());
    // Past the first (partly silent) window, every column peaks at 1 kHz.
    let loudest: Vec<usize> = (2..a.spec_cols() - 1)
        .map(|c| {
            let col = a.column(c);
            (0..BANDS).max_by(|&x, &y| col[x].total_cmp(&col[y])).unwrap()
        })
        .collect();
    for band in loudest {
        let (lo, hi) = analysis::peak_band_hz(44_100, band);
        assert!(lo < 1080.0 && hi > 920.0, "band {band} is {lo:.0}–{hi:.0} Hz");
    }
}

#[test]
fn the_waveform_follows_the_amplitude() {
    let info = sine_info("wave", 1.0);
    let h = Handle::start(&info);
    h.wait();
    let a = h.lock();
    let max = a.wave.iter().map(|b| b.max).fold(f32::MIN, f32::max);
    let min = a.wave.iter().map(|b| b.min).fold(f32::MAX, f32::min);
    assert!((max - 0.5).abs() < 0.01 && (min + 0.5).abs() < 0.01, "{min}..{max}");
    // A sine's RMS is its amplitude over √2.
    let mid = &a.wave[4..a.wave.len() - 4];
    let rms = (mid.iter().map(|b| b.rms * b.rms).sum::<f32>() / mid.len() as f32).sqrt();
    assert!((rms - 0.5 / 2f32.sqrt()).abs() < 0.02, "{rms}");
    assert_eq!(a.duration(), Some(Duration::from_secs(1)));
}

#[test]
fn columns_fold_in_pairs_so_memory_stays_bounded() {
    let mut a = Analysis { spec_hop: 100, ..Default::default() };
    let loud = vec![0.0f32; BANDS];
    let quiet = vec![-100.0f32; BANDS];
    for i in 0..2 * SPEC_COLS - 1 {
        let folded = analysis::fold_for_test(&mut a, if i % 2 == 0 { &loud } else { &quiet });
        assert!(!folded);
    }
    assert!(analysis::fold_for_test(&mut a, &quiet));
    assert_eq!(a.spec_cols(), SPEC_COLS);
    assert_eq!(a.spec_hop, 200);
    // Averaged in power: one loud and one silent column make half the power,
    // about 3 dB down — not the midpoint of the two dB values.
    let v = a.column(0)[0];
    assert!((v + 3.01).abs() < 0.05, "{v}");
}

#[test]
fn a_cancelled_analysis_stops_without_finishing() {
    let info = sine_info("cancel", 1.0);
    assert!(!analysis::run_cancelled_for_test(&info));
}

#[test]
fn an_undecodable_file_fails_the_analysis() {
    let path = scratch("fail").join("bad.wav");
    std::fs::write(&path, b"RIFF").unwrap();
    let info = AudioInfo { path, hint: "wav".into(), sample_rate: 44_100, ..Default::default() };
    let h = Handle::start(&info);
    h.wait();
    assert!(h.lock().failed);
}

#[test]
fn the_pictures_are_the_size_asked_for_and_show_the_sound() {
    let info = sine_info("pictures", 1.0);
    let mut v = AudioView::new(info, AudioDisplay::Spectrogram, AudioOut::new());
    v.settle();
    assert!(!v.analyzing());
    let pal = raster::Palette {
        bg: (0, 0, 0),
        wave: (0, 200, 0),
        core: (0, 255, 0),
        axis: (40, 40, 40),
        head: (255, 255, 255),
    };
    let img = v.build_image(120, 60, &pal, false);
    assert_eq!((img.width(), img.height()), (120, 60));
    // The tone is a bright line somewhere in the middle of every column.
    let bright = (0..60).filter(|&y| img.get_pixel(60, y)[0] > 200).count();
    assert!(bright > 0);

    v.toggle_display();
    let img = v.build_image(120, 60, &pal, false);
    // The envelope spans about half the height either side of the middle.
    let column: Vec<_> = (0..60).filter(|&y| img.get_pixel(60, y)[1] >= 200).collect();
    assert!(column.len() > 20 && column.len() < 40, "{} rows", column.len());
}

#[test]
fn the_picture_signature_ignores_the_play_position() {
    let info = sine_info("sig", 1.0);
    let mut v = AudioView::new(info, AudioDisplay::Spectrogram, AudioOut::new());
    v.settle();
    let pal = raster::Palette::from_theme(&crate::ui::theme::Theme::default());
    let sig = v.image_sig(200, 80, &pal);
    v.seek_to(Duration::from_millis(600));
    assert_eq!(v.image_sig(200, 80, &pal), sig, "moving the position must not resend the picture");
    assert_ne!(v.image_sig(201, 80, &pal), sig);
    v.toggle_display();
    assert_ne!(v.image_sig(200, 80, &pal), sig);
}

#[test]
fn dragging_along_the_picture_seeks_once_on_release() {
    let info = sine_info("scrub", 2.0);
    let mut v = AudioView::new(info, AudioDisplay::Waveform, AudioOut::new());
    v.settle();
    let strip = Rect { x: 10, y: 5, width: 100, height: 10 };
    v.set_hits(AudioHits {
        area: Rect { height: 12, ..strip },
        image: strip,
        progress: Rect { y: 15, height: 1, ..strip },
        ..Default::default()
    });
    assert!(v.mouse(left_down(35, 8)));
    assert!(v.scrubbing());
    // While dragging, the position follows the pointer…
    let p = v.position().as_secs_f32();
    assert!((p - 0.51).abs() < 0.02, "{p}");
    assert!(v.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 200, 8)));
    assert_eq!(v.position(), v.duration(), "a drag past the end pins to the end");
    assert!(v.mouse(mouse(MouseEventKind::Drag(MouseButton::Left), 60, 8)));
    // …and letting go leaves it there.
    assert!(v.mouse(mouse(MouseEventKind::Up(MouseButton::Left), 60, 8)));
    assert!(!v.scrubbing());
    let p = v.position().as_secs_f32();
    assert!((p - 1.01).abs() < 0.03, "{p}");
    // A click outside the widget is not its business.
    assert!(!v.mouse(left_down(0, 0)));
}

#[test]
fn transport_keys_play_pause_seek_and_change_the_volume() {
    let info = sine_info("keys", 20.0);
    let out = AudioOut::new();
    let mut v = AudioView::new(info, AudioDisplay::Spectrogram, out.clone());
    v.settle();
    assert_eq!(v.state(), PlayState::Idle);
    assert!(v.key(key(KeyCode::Char(' '))));
    assert_eq!(v.state(), PlayState::Playing);
    assert!(out.active());
    assert!(v.key(key(KeyCode::Right)));
    assert_eq!(v.position(), Duration::from_secs(5));
    assert!(v.key(key(KeyCode::PageDown)));
    assert_eq!(v.position(), Duration::from_secs(20), "clamped to the length");
    assert!(v.key(key(KeyCode::Home)));
    assert_eq!(v.position(), Duration::ZERO);
    assert!(v.key(key(KeyCode::Char(' '))));
    assert_eq!(v.state(), PlayState::Paused);

    let vol = v.volume();
    assert!(v.key(key(KeyCode::Char('-'))));
    assert!((v.volume() - (vol - 0.05)).abs() < 1e-4);
    assert!(v.key(key(KeyCode::Up)));
    assert!((v.volume() - vol).abs() < 1e-4);

    // Keys with a modifier, and anything else, are left for someone else.
    assert!(!v.key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT)));
    assert!(!v.key(key(KeyCode::Char('x'))));
}

#[test]
fn playing_one_view_silences_another_and_closing_a_view_unloads_it() {
    let out = AudioOut::new();
    let mut a = AudioView::new(sine_info("excl_a", 1.0), AudioDisplay::Spectrogram, out.clone());
    let mut b = AudioView::new(sine_info("excl_b", 1.0), AudioDisplay::Spectrogram, out.clone());
    a.toggle_play();
    assert_eq!(a.state(), PlayState::Playing);
    a.seek_to(Duration::from_millis(400));
    a.poll(std::time::Instant::now());
    b.toggle_play();
    assert_eq!(b.state(), PlayState::Playing);
    assert_eq!(a.state(), PlayState::Idle);
    a.poll(std::time::Instant::now());
    // `a` remembers where it was, and plays on from there.
    assert_eq!(a.position(), Duration::from_millis(400));
    drop(b);
    assert!(!out.active());
}

#[test]
fn the_widget_lays_out_its_rows_from_the_bottom() {
    let area = Rect { x: 0, y: 0, width: 80, height: 20 };
    let lay = widget::layout(area, false, false);
    assert_eq!(lay.transport.y, 19);
    assert_eq!(lay.ticks.map(|t| t.y), Some(18));
    assert_eq!(lay.progress.y, 17);
    assert_eq!(lay.image, Rect { height: 17, ..area });

    let compact = widget::layout(area, true, true);
    assert!(compact.ticks.is_none());
    assert_eq!(compact.progress.y, 18);
    assert_eq!(compact.image.height, 17, "a spare row under the picture for Sixel");

    let tiny = widget::layout(Rect { height: 1, ..area }, false, false);
    assert_eq!(tiny.transport.height, 1);
    assert_eq!(tiny.image.height, 0);
}

#[test]
fn the_drawn_buttons_answer_clicks() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    let mut v = AudioView::new(sine_info("buttons", 10.0), AudioDisplay::Waveform, AudioOut::new());
    v.settle();
    let theme = crate::ui::theme::Theme::default();
    let mut term = Terminal::new(TestBackend::new(90, 12)).unwrap();
    let area = Rect { x: 0, y: 0, width: 90, height: 12 };
    term.draw(|f| widget::render(f, area, &v, &theme, None, crate::ui::graphics::Slot::ViewerAudio))
        .unwrap();
    let hits = v.hits();
    let kinds: Vec<Transport> = hits.buttons.iter().map(|(_, t)| *t).collect();
    assert_eq!(
        kinds,
        [
            Transport::Start,
            Transport::Back,
            Transport::PlayPause,
            Transport::Stop,
            Transport::Forward,
            Transport::VolumeDown,
            Transport::VolumeUp
        ]
    );
    assert!(hits.volume_bar.width > 0);
    let screen: String = term.backend().buffer().content.iter().map(|c| c.symbol()).collect();
    assert!(screen.contains("0:00 / 0:10"), "{screen}");

    let play = hits.buttons.iter().find(|(_, t)| *t == Transport::PlayPause).unwrap().0;
    assert!(v.mouse(left_down(play.x + 1, play.y)));
    assert_eq!(v.state(), PlayState::Playing);
    // The volume bar sets the volume from where it is clicked.
    let bar = hits.volume_bar;
    assert!(v.mouse(left_down(bar.x, bar.y)));
    assert!(v.volume() < 0.1, "{}", v.volume());

    // The compact form (the Details view) has no Start button and no bar.
    term.draw(|f| {
        let lay = widget::layout(Rect { width: 36, ..area }, true, false);
        widget::render_cells(f, &lay, &v, &theme, false, true);
    })
    .unwrap();
    let hits = v.hits();
    assert!(!hits.buttons.iter().any(|(_, t)| *t == Transport::Start));
    assert_eq!(hits.volume_bar.width, 0);
    v.clear_hits();
    assert!(!v.mouse(left_down(play.x + 1, play.y)), "nothing to hit once cleared");
}

#[test]
fn waveform_bins_merge_by_extremes() {
    let bins = [WaveBin { min: -0.2, max: 0.1, rms: 0.1 }, WaveBin { min: -0.1, max: 0.4, rms: 0.3 }];
    let a = Analysis { wave: bins.to_vec(), wave_hop: 10, frames_done: 20, done: true, sample_rate: 20, ..Default::default() };
    let pal = raster::Palette { bg: (0, 0, 0), wave: (9, 9, 9), core: (99, 99, 99), axis: (1, 1, 1), head: (2, 2, 2) };
    // One pixel column over both bins: the envelope spans both extremes.
    let img = raster::waveform(&a, 1, 101, &pal);
    let top = (0..101).find(|&y| img.get_pixel(0, y)[0] > 1).unwrap();
    let bottom = (0..101).rev().find(|&y| img.get_pixel(0, y)[0] > 1).unwrap();
    assert!((29..=31).contains(&top), "{top}");
    assert!((59..=61).contains(&bottom), "{bottom}");
}
