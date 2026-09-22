//! Tag reading over files built in the test itself, so no audio fixture has to
//! live in the repository: a container is synthesized, lofty writes a tag onto
//! it, and the reader is pointed back at the result.

use super::*;
use lofty::config::WriteOptions;
use lofty::prelude::{Accessor, TagExt};
use lofty::tag::{Tag, TagType};
use std::path::{Path, PathBuf};

/// A scratch directory named after the calling test.
fn scratch(tag: &str) -> PathBuf {
    let nanos =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let dir = std::env::temp_dir().join(format!("rc_tags_{tag}_{}_{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A WAV carrying `tag`. WAV is the one container a test can synthesize exactly
/// (there is no encoder here to make an MP3 or an Ogg stream with), and the
/// field mapping under test is the same whatever the container turns out to be.
fn tagged_wav(dir: &Path, name: &str, tag: Tag) -> PathBuf {
    let path = dir.join(name);
    crate::audio::tests::write_wav(&path, 8_000, 0.05, 440.0, 0.2);
    tag.save_to_path(&path, WriteOptions::default()).unwrap();
    path
}

fn read_map(path: &Path) -> std::collections::HashMap<String, String> {
    rename_fields(path).into_iter().collect()
}

#[test]
fn taggable_names_by_extension() {
    assert!(
        is_taggable_name("song.MP3") && is_taggable_name("a.ogg") && is_taggable_name("x.flac")
    );
    assert!(!is_taggable_name("notes.txt") && !is_taggable_name("noext"));
}

#[test]
fn every_field_has_a_distinct_token() {
    let mut seen = std::collections::HashSet::new();
    for f in Field::ALL {
        assert!(!f.token().is_empty());
        assert!(seen.insert(f.token()), "duplicate token {}", f.token());
    }
}

#[test]
fn a_file_with_no_tags_yields_nothing() {
    let dir = scratch("untagged");
    let path = dir.join("plain.txt");
    std::fs::write(&path, b"not audio").unwrap();
    assert!(rename_fields(&path).is_empty());
    // A real audio file that simply carries no tag is just as empty.
    let wav = dir.join("bare.wav");
    crate::audio::tests::write_wav(&wav, 8_000, 0.05, 440.0, 0.2);
    assert!(rename_fields(&wav).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn well_known_fields_become_rename_tokens() {
    let dir = scratch("fields");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Roads".to_string());
    tag.set_artist("Portishead".to_string());
    tag.set_album("Dummy".to_string());
    tag.insert_text(ItemKey::TrackNumber, "3".to_string());
    // ID3v2.4 has no year frame of its own: the year is the front of the
    // recording date, which is the case the reader has to cope with.
    tag.insert_text(ItemKey::RecordingDate, "1994-08-22".to_string());
    let path = tagged_wav(&dir, "t.wav", tag);

    let got = read_map(&path);
    assert_eq!(got.get("title").map(String::as_str), Some("Roads"));
    assert_eq!(got.get("artist").map(String::as_str), Some("Portishead"));
    assert_eq!(got.get("album").map(String::as_str), Some("Dummy"));
    assert_eq!(got.get("year").map(String::as_str), Some("1994"));
    // A single-digit track is padded, so a renamed batch still sorts.
    assert_eq!(got.get("track").map(String::as_str), Some("03"));
    // Nothing is invented for fields the file does not carry.
    assert!(!got.contains_key("composer") && !got.contains_key("comment"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_recording_date_stands_in_for_a_missing_year() {
    let dir = scratch("date");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("x".to_string());
    tag.insert_text(ItemKey::RecordingDate, "2007-08-13".to_string());
    let path = tagged_wav(&dir, "d.wav", tag);
    assert_eq!(read_map(&path).get("year").map(String::as_str), Some("2007"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_ascii_values_survive() {
    let dir = scratch("utf8");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_artist("Björk".to_string());
    tag.set_title("Jóga".to_string());
    let path = tagged_wav(&dir, "u.wav", tag);
    let got = read_map(&path);
    assert_eq!(got.get("artist").map(String::as_str), Some("Björk"));
    assert_eq!(got.get("title").map(String::as_str), Some("Jóga"));
    let _ = std::fs::remove_dir_all(&dir);
}
