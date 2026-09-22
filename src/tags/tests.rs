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

/// One well-known field's value in `t`, or an empty string.
fn field_of(t: &Tags, field: Field) -> &str {
    t.fields.iter().find(|(f, _)| *f == field).map(|(_, v)| v.as_str()).unwrap_or_default()
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

// -- The editable model and the write path -------------------------------

#[test]
fn reading_gives_every_field_even_when_unset() {
    let dir = scratch("model");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Only a title".to_string());
    let path = tagged_wav(&dir, "m.wav", tag);

    let t = read(&path).expect("reads");
    assert_eq!(t.fields.len(), Field::ALL.len(), "every field gets a row");
    let title = t.fields.iter().find(|(f, _)| *f == Field::Title).unwrap();
    assert_eq!(title.1, "Only a title");
    // The ones the file does not set are present and empty, not missing.
    let genre = t.fields.iter().find(|(f, _)| *f == Field::Genre).unwrap();
    assert!(genre.1.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_untagged_file_can_still_be_given_tags() {
    let dir = scratch("fresh");
    let path = dir.join("bare.wav");
    crate::audio::tests::write_wav(&path, 8_000, 0.05, 440.0, 0.2);

    let t = read(&path).expect("a taggable file with no tag is not an error");
    assert!(t.fields.iter().all(|(_, v)| v.is_empty()));

    let fields: Vec<(Field, String)> = vec![(Field::Title, "Written from scratch".to_string())];
    write(&path, &fields, &t.extra, t.tag_type).expect("writes");
    assert_eq!(read_map(&path).get("title").map(String::as_str), Some("Written from scratch"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edited_values_round_trip_through_a_write() {
    let dir = scratch("write");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Before".to_string());
    tag.set_artist("Keep me".to_string());
    let path = tagged_wav(&dir, "w.wav", tag);

    let before = read(&path).unwrap();
    let mut fields = before.fields.clone();
    for (f, v) in &mut fields {
        if *f == Field::Title {
            "After".clone_into(v);
        }
        if *f == Field::Genre {
            "Trip hop".clone_into(v);
        }
    }
    write(&path, &fields, &before.extra, before.tag_type).expect("writes");

    let got = read_map(&path);
    assert_eq!(got.get("title").map(String::as_str), Some("After"), "the edit landed");
    assert_eq!(got.get("genre").map(String::as_str), Some("Trip hop"), "a new field landed");
    assert_eq!(got.get("artist").map(String::as_str), Some("Keep me"), "the rest is untouched");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emptying_a_field_removes_it_rather_than_writing_a_blank() {
    let dir = scratch("clear");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Gone soon".to_string());
    tag.set_artist("Stays".to_string());
    let path = tagged_wav(&dir, "c.wav", tag);

    let before = read(&path).unwrap();
    let fields: Vec<(Field, String)> = before
        .fields
        .iter()
        .map(|(f, v)| (*f, if *f == Field::Title { String::new() } else { v.clone() }))
        .collect();
    write(&path, &fields, &before.extra, before.tag_type).expect("writes");

    let got = read_map(&path);
    assert!(!got.contains_key("title"), "an emptied field is removed, not blanked");
    assert_eq!(got.get("artist").map(String::as_str), Some("Stays"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_write_keeps_items_the_program_has_no_name_for() {
    let dir = scratch("extra");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Has extras".to_string());
    // Something outside the well-known set.
    tag.insert_text(ItemKey::Mood, "Rainy".to_string());
    let path = tagged_wav(&dir, "e.wav", tag);

    let before = read(&path).unwrap();
    assert!(
        before.extra.iter().any(|e| e.value == "Rainy"),
        "an unnamed item is shown, so it is visibly kept: {:?}",
        before.extra
    );

    let mut fields = before.fields.clone();
    for (f, v) in &mut fields {
        if *f == Field::Title {
            "Changed".clone_into(v);
        }
    }
    write(&path, &fields, &before.extra, before.tag_type).expect("writes");

    let after = read(&path).unwrap();
    assert!(
        after.extra.iter().any(|e| e.value == "Rainy"),
        "editing a title must not discard frames this program has no word for"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

// -- The editor sub-mode -------------------------------------------------

#[test]
fn the_tag_editor_tracks_changes_and_clears_fields() {
    use crate::tags::editor::TagEditor;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    let dir = scratch("editor");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Start".to_string());
    let path = tagged_wav(&dir, "ed.wav", tag);

    let mut te = TagEditor::open(&path).expect("opens");
    assert!(!te.dirty(), "nothing has been typed yet");

    let press = |te: &mut TagEditor, c: KeyCode| {
        te.key(KeyEvent::new(c, KeyModifiers::NONE));
    };
    // Title is the first row. Typing replaces it, Enter commits.
    press(&mut te, KeyCode::Enter);
    assert!(te.editing(), "Enter starts editing the selected field");
    for _ in 0.."Start".len() {
        press(&mut te, KeyCode::Backspace);
    }
    for c in "Ended".chars() {
        press(&mut te, KeyCode::Char(c));
    }
    press(&mut te, KeyCode::Enter);
    assert!(!te.editing() && te.dirty(), "the edit is committed and noticed");
    assert_eq!(te.to_write()[0].1, "Ended");

    // Esc abandons an edit in progress.
    press(&mut te, KeyCode::Enter);
    press(&mut te, KeyCode::Char('z'));
    press(&mut te, KeyCode::Esc);
    assert_eq!(te.to_write()[0].1, "Ended", "Esc leaves the committed value alone");

    // Del clears the selected field outright.
    press(&mut te, KeyCode::Delete);
    assert!(te.to_write()[0].1.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_tag_editor_saves_and_settles() {
    use crate::tags::editor::TagEditor;

    let dir = scratch("save");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Old".to_string());
    let path = tagged_wav(&dir, "s.wav", tag);

    let mut te = TagEditor::open(&path).unwrap();
    let fields: Vec<(Field, String)> = te
        .to_write()
        .into_iter()
        .map(|(f, v)| (f, if f == Field::Title { "New".to_string() } else { v }))
        .collect();
    write(&path, &fields, &te.extra_to_write(), te.tag_type()).unwrap();
    te.reload(&path);
    te.mark_saved();

    assert!(!te.dirty(), "after a save the view matches the file again");
    assert_eq!(te.to_write()[0].1, "New");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_the_year_rewrites_the_recording_date_rather_than_fighting_it() {
    let dir = scratch("year");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Roads".to_string());
    // How ID3v2.4 and Vorbis actually keep a year.
    tag.insert_text(ItemKey::RecordingDate, "1994-08-22".to_string());
    let path = tagged_wav(&dir, "y.wav", tag);

    let before = read(&path).unwrap();
    assert_eq!(field_of(&before, Field::Year), "1994", "the year is read out of the date");
    assert!(
        !before.extra.iter().any(|e| e.label.contains("RecordingDate")),
        "and the date is not also listed as an unnamed item: {:?}",
        before.extra
    );

    let fields: Vec<(Field, String)> = before
        .fields
        .iter()
        .map(|(f, v)| (*f, if *f == Field::Year { "1995".to_string() } else { v.clone() }))
        .collect();
    write(&path, &fields, &before.extra, before.tag_type).expect("writes");

    let after = read(&path).unwrap();
    assert_eq!(field_of(&after, Field::Year), "1995", "the edit took");
    // One source of truth: the date was rewritten, keeping its month and day,
    // rather than a second year frame appearing beside it.
    let raw = lofty::read_from_path(&path).unwrap();
    let t = raw.primary_tag().or_else(|| raw.first_tag()).unwrap();
    assert_eq!(
        t.get_string(ItemKey::RecordingDate),
        Some("1995-08-22"),
        "the month and day survive"
    );
    assert!(t.get_string(ItemKey::Year).is_none(), "no rival year frame was added");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn clearing_the_year_clears_the_date_with_it() {
    let dir = scratch("noyear");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("x".to_string());
    tag.insert_text(ItemKey::RecordingDate, "1994-08-22".to_string());
    let path = tagged_wav(&dir, "n.wav", tag);

    let before = read(&path).unwrap();
    let fields: Vec<(Field, String)> = before
        .fields
        .iter()
        .map(|(f, v)| (*f, if *f == Field::Year { String::new() } else { v.clone() }))
        .collect();
    write(&path, &fields, &before.extra, before.tag_type).expect("writes");

    assert_eq!(field_of(&read(&path).unwrap(), Field::Year), "", "the year is gone");
    let raw = lofty::read_from_path(&path).unwrap();
    let t = raw.primary_tag().or_else(|| raw.first_tag()).unwrap();
    assert!(
        t.get_string(ItemKey::RecordingDate).is_none(),
        "and it did not survive in the date it was read from"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_unnamed_item_can_be_edited_and_cleared() {
    let dir = scratch("editextra");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Song".to_string());
    tag.insert_text(ItemKey::Mood, "Rainy".to_string());
    tag.insert_text(ItemKey::Publisher, "Go! Beat".to_string());
    let path = tagged_wav(&dir, "x.wav", tag);

    let before = read(&path).unwrap();
    assert!(before.extra.iter().all(|e| e.editable), "both are editable: {:?}", before.extra);

    // Change one and clear the other.
    let extra: Vec<Extra> = before
        .extra
        .iter()
        .map(|e| {
            let value = match e.key {
                ItemKey::Mood => "Sunny".to_string(),
                _ => String::new(),
            };
            Extra { value, ..e.clone() }
        })
        .collect();
    write(&path, &before.fields, &extra, before.tag_type).expect("writes");

    let after = read(&path).unwrap();
    assert_eq!(
        after.extra.iter().find(|e| e.key == ItemKey::Mood).map(|e| e.value.as_str()),
        Some("Sunny"),
        "the edit landed"
    );
    assert!(
        !after.extra.iter().any(|e| e.key == ItemKey::Publisher),
        "and the cleared one was removed: {:?}",
        after.extra
    );
    // The named fields are untouched by all this.
    assert_eq!(field_of(&after, Field::Title), "Song");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_key_the_file_uses_twice_is_shown_but_not_edited() {
    // Written by key, so editing one of a repeated key would replace them all.
    // Rather than merge values behind the user's back, those rows are shown and
    // refused — and a save leaves them exactly as the file had them.
    let ex = |key: ItemKey, value: &str| Extra {
        key,
        label: format!("{key:?}"),
        value: value.to_string(),
        editable: true,
    };
    let mut items = vec![
        ex(ItemKey::Mood, "Rainy"),
        ex(ItemKey::Performer, "Beth"),
        ex(ItemKey::Performer, "Geoff"),
        ex(ItemKey::Publisher, "Go! Beat"),
    ];
    mark_duplicates(&mut items);

    assert!(items[0].editable, "a key used once stays editable");
    assert!(!items[1].editable && !items[2].editable, "both of a repeated key are refused");
    assert!(items[3].editable, "and the others are unaffected");

    // Nothing to mark when every key is distinct.
    let mut unique = vec![ex(ItemKey::Mood, "a"), ex(ItemKey::Publisher, "b")];
    mark_duplicates(&mut unique);
    assert!(unique.iter().all(|e| e.editable));
}

#[test]
fn addable_keys_are_what_the_format_can_hold_and_does_not_yet() {
    let all = addable_keys(TagType::Id3v2, &[]);
    assert!(all.len() > 50, "an ID3v2 tag has plenty of keys: {}", all.len());
    assert!(all.iter().any(|(k, _)| *k == ItemKey::Mood));
    // Sorted by label, so the picker reads alphabetically.
    let labels: Vec<&str> = all.iter().map(|(_, l)| l.as_str()).collect();
    let mut sorted = labels.clone();
    sorted.sort_by_key(|l| l.to_lowercase());
    assert_eq!(labels, sorted);

    // A key already on the page is not offered again.
    let less = addable_keys(TagType::Id3v2, &[ItemKey::Mood]);
    assert!(!less.iter().any(|(k, _)| *k == ItemKey::Mood));
    assert_eq!(less.len(), all.len() - 1);

    // Nor are the well-known fields, which have rows of their own.
    let named: Vec<ItemKey> = Field::ALL.iter().map(|f| f.key()).collect();
    let rest = addable_keys(TagType::Id3v2, &named);
    assert!(
        !rest.iter().any(|(k, _)| named.contains(k)),
        "the named fields are not offered as extras"
    );

    // Each format offers its own keys, not one shared list — and that is the
    // point of the filter, not a nicety: ID3v2 has no BPM frame, so offering
    // one would write a value the file cannot keep and that would be gone by
    // the next read.
    let vorbis = addable_keys(TagType::VorbisComments, &[]);
    assert!(vorbis.iter().any(|(k, _)| *k == ItemKey::Bpm), "Vorbis comments have a BPM");
    assert!(!all.iter().any(|(k, _)| *k == ItemKey::Bpm), "ID3v2 does not, so it is not offered");
}

#[test]
fn an_added_key_is_not_a_change_until_it_has_a_value() {
    use crate::tags::editor::TagEditor;
    let dir = scratch("addkey");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Song".to_string());
    let path = tagged_wav(&dir, "a.wav", tag);

    let mut te = TagEditor::open(&path).expect("opens");
    assert!(!te.dirty());

    te.add_key(ItemKey::Mood, "Mood".to_string());
    // The row is there and waiting to be typed into, but an empty tag is not
    // yet an unsaved change — nothing would be written for it.
    assert!(te.editing(), "the new row is ready for its value");
    assert!(!te.dirty(), "an empty new row is not a change");
    assert!(te.extra_to_write().iter().any(|e| e.key == ItemKey::Mood));

    // Adding the same key twice does not make a second row.
    let before = te.extra_to_write().len();
    te.add_key(ItemKey::Mood, "Mood".to_string());
    assert_eq!(te.extra_to_write().len(), before, "a key is added at most once");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_added_tag_is_written_and_an_empty_one_is_not() {
    let dir = scratch("addwrite");
    let mut tag = Tag::new(TagType::Id3v2);
    tag.set_title("Song".to_string());
    let path = tagged_wav(&dir, "w.wav", tag);
    let before = read(&path).unwrap();

    let extra = vec![
        Extra { key: ItemKey::Mood, label: "Mood".into(), value: "Rainy".into(), editable: true },
        // Added but never filled in.
        Extra {
            key: ItemKey::Publisher,
            label: "Publisher".into(),
            value: String::new(),
            editable: true,
        },
    ];
    write(&path, &before.fields, &extra, before.tag_type).expect("writes");

    let after = read(&path).unwrap();
    assert_eq!(
        after.extra.iter().find(|e| e.key == ItemKey::Mood).map(|e| e.value.as_str()),
        Some("Rainy"),
        "the added tag is in the file"
    );
    assert!(
        !after.extra.iter().any(|e| e.key == ItemKey::Publisher),
        "and one left empty was not written: {:?}",
        after.extra
    );
    let _ = std::fs::remove_dir_all(&dir);
}
