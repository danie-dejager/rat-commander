//! Audio tags: the values the multi-rename `[TAG:…]` placeholders expand to,
//! and (from the F4 tag editor) the fields that are written back.
//!
//! [`crate::audio`] already reads tags through symphonia, but symphonia is a
//! decoder and only ever reads. Tags are read *and written* here through lofty
//! instead, so one crate owns both directions and a value shown in the editor
//! is the same value a rename sees.
//!
//! Only the well-known fields are named. Anything else a file carries is left
//! alone rather than enumerated, so a rename never invents a token and a write
//! never drops a frame it did not understand.

pub(crate) mod editor;
pub(crate) mod render;

use lofty::config::WriteOptions;
use lofty::file::TaggedFileExt;
use lofty::prelude::TagExt;
use lofty::tag::{ItemKey, ItemValue, Tag, TagType};
use std::path::Path;

/// Extensions whose tags lofty can read and write. Deliberately narrower than
/// [`crate::audio::EXTENSIONS`]: that list is what can be *decoded* for the
/// spectrogram, while this one promises the tags can be edited and saved.
const EXTENSIONS: &[&str] = &[
    "mp3", "mp2", "ogg", "oga", "opus", "flac", "m4a", "m4b", "mp4", "wav", "wave", "aif", "aiff",
    "aifc", "ape", "wv", "mpc",
];

/// Whether `name`'s extension is one whose tags can be read and written.
pub fn is_taggable_name(name: &str) -> bool {
    let ext = name.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    EXTENSIONS.contains(&ext.as_str())
}

/// A well-known tag field — the ones worth naming in a rename mask and worth
/// showing as a labelled row in the editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Title,
    Artist,
    Album,
    AlbumArtist,
    Track,
    Disc,
    Year,
    Genre,
    Comment,
    Composer,
}

impl Field {
    /// Every field, in the order the editor lists them.
    pub const ALL: [Field; 10] = [
        Field::Title,
        Field::Artist,
        Field::Album,
        Field::AlbumArtist,
        Field::Track,
        Field::Disc,
        Field::Year,
        Field::Genre,
        Field::Comment,
        Field::Composer,
    ];

    /// The `[TAG:…]` token this field answers to, lower-cased — the second half
    /// of the key [`crate::rename::FileMeta`] stores it under.
    pub fn token(self) -> &'static str {
        match self {
            Field::Title => "title",
            Field::Artist => "artist",
            Field::Album => "album",
            Field::AlbumArtist => "albumartist",
            Field::Track => "track",
            Field::Disc => "disc",
            Field::Year => "year",
            Field::Genre => "genre",
            Field::Comment => "comment",
            Field::Composer => "composer",
        }
    }

    /// The label the editor shows for this field.
    pub fn label(self) -> &'static str {
        match self {
            Field::Title => "Title",
            Field::Artist => "Artist",
            Field::Album => "Album",
            Field::AlbumArtist => "Album artist",
            Field::Track => "Track",
            Field::Disc => "Disc",
            Field::Year => "Year",
            Field::Genre => "Genre",
            Field::Comment => "Comment",
            Field::Composer => "Composer",
        }
    }

    /// The lofty key this field is stored under.
    fn key(self) -> ItemKey {
        match self {
            Field::Title => ItemKey::TrackTitle,
            Field::Artist => ItemKey::TrackArtist,
            Field::Album => ItemKey::AlbumTitle,
            Field::AlbumArtist => ItemKey::AlbumArtist,
            Field::Track => ItemKey::TrackNumber,
            Field::Disc => ItemKey::DiscNumber,
            Field::Year => ItemKey::Year,
            Field::Genre => ItemKey::Genre,
            Field::Comment => ItemKey::Comment,
            Field::Composer => ItemKey::Composer,
        }
    }

    /// This field's value in `tag`, trimmed, or `None` when it is unset.
    ///
    /// `Year` is the awkward one: a file may carry a bare year, or a full
    /// recording date (`2007-08-13`) and no year at all, so fall back to the
    /// date's leading year rather than reporting nothing.
    fn read(self, tag: &Tag) -> Option<String> {
        let direct = tag.get_string(self.key()).map(str::trim).filter(|s| !s.is_empty());
        if let Some(s) = direct {
            return Some(s.to_string());
        }
        if self == Field::Year {
            let date = tag.get_string(ItemKey::RecordingDate)?;
            let year: String = date.trim().chars().take_while(char::is_ascii_digit).collect();
            return (year.len() == 4).then_some(year);
        }
        None
    }
}

/// The tag a file's values should be read from: its primary tag, or whatever
/// single tag it does carry (an MP3 with only an ID3v1 block, say).
fn best_tag(f: &lofty::file::TaggedFile) -> Option<&Tag> {
    f.primary_tag().or_else(|| f.first_tag())
}

/// Every well-known field set in `path`, as `(token, value)` pairs with the
/// tokens already lower-cased — what [`crate::rename::FileMeta`] is built from.
///
/// Empty when the file has no tags, is not taggable, or cannot be parsed: a
/// mask referring to a tag a file lacks expands to nothing, which is what a
/// batch of mixed files needs.
pub fn rename_fields(path: &Path) -> Vec<(String, String)> {
    let Ok(f) = lofty::read_from_path(path) else {
        return Vec::new();
    };
    let Some(tag) = best_tag(&f) else {
        return Vec::new();
    };
    let mut out: Vec<(String, String)> = Field::ALL
        .iter()
        .filter_map(|&fl| fl.read(tag).map(|v| (fl.token().to_string(), v)))
        .collect();
    // The track and disc numbers are worth having zero-padded for a filename
    // that should sort: `02 - ...` rather than `2 - ...`.
    for (tok, val) in &mut out {
        if (tok == "track" || tok == "disc")
            && val.len() == 1
            && val.starts_with(|c: char| c.is_ascii_digit())
        {
            val.insert(0, '0');
        }
    }
    out
}

/// An item the file carries that is not one of the well-known [`Field`]s — a
/// mood, a label, a MusicBrainz id, whatever the file happens to hold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extra {
    /// The key it lives under, so an edit goes back to the same place.
    pub key: ItemKey,
    /// How it is named on the page.
    pub label: String,
    pub value: String,
    /// Whether it can be edited here.
    ///
    /// False when the file holds several items under this one key: writing
    /// would collapse them into the single value being edited, so those are
    /// shown and carried through untouched rather than quietly merged.
    pub editable: bool,
}

/// Everything the tag editor shows for one file, and everything a save needs to
/// put back.
///
/// `extra` is the point of the split: a file may carry frames this program has
/// no name for, and they are kept here so that saving an edited title does not
/// quietly discard them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tags {
    /// The well-known fields, in [`Field::ALL`] order. A field the file does
    /// not set is present with an empty value, so the editor shows every row.
    pub fields: Vec<(Field, String)>,
    /// Everything else the file carries.
    pub extra: Vec<Extra>,
    /// Embedded pictures (cover art). Counted and preserved, not edited.
    pub pictures: usize,
    /// Which kind of tag the values came from, so a save writes the same kind
    /// back rather than adding a second one alongside it.
    pub tag_type: TagType,
}

/// Read `path`'s tags for the editor.
///
/// A taggable file with no tag at all is not an error — it comes back with
/// every field empty and the tag kind its format would use, so the editor can
/// be used to give it one.
pub fn read(path: &Path) -> Option<Tags> {
    let f = lofty::read_from_path(path).ok()?;
    let tag_type = f.primary_tag_type();
    let Some(tag) = best_tag(&f) else {
        return Some(Tags {
            fields: Field::ALL.iter().map(|&fl| (fl, String::new())).collect(),
            extra: Vec::new(),
            pictures: 0,
            tag_type,
        });
    };
    let mut named: Vec<ItemKey> = Field::ALL.iter().map(|f| f.key()).collect();
    // `Year` reads the recording date when there is no bare year, so the date
    // is already shown as a field; listing it again below would be the same
    // value twice, and editing one of them would contradict the other.
    named.push(ItemKey::RecordingDate);
    let mut extra: Vec<Extra> = tag
        .items()
        .filter(|i| !named.contains(&i.key()))
        .filter_map(|i| match i.value() {
            // Binary items are not text and have nothing to show; they are
            // still carried through a save, since the tag is edited in place.
            ItemValue::Text(t) | ItemValue::Locator(t) => Some(Extra {
                key: i.key(),
                label: format!("{:?}", i.key()),
                value: t.trim().to_string(),
                editable: true,
            }),
            ItemValue::Binary(_) => None,
        })
        .filter(|e| !e.value.is_empty())
        .collect();
    mark_duplicates(&mut extra);
    Some(Tags {
        fields: Field::ALL.iter().map(|&fl| (fl, fl.read(tag).unwrap_or_default())).collect(),
        extra,
        pictures: tag.picture_count() as usize,
        tag_type: tag.tag_type(),
    })
}

/// The keys that can be added to a tag of `tag_type`, less the `present` ones,
/// as `(key, label)` sorted by label.
///
/// Filtered by what the format can actually hold: offering an MP4 file a key
/// only Vorbis comments have would write something the file cannot keep, and
/// the value would quietly vanish on the next read.
pub fn addable_keys(tag_type: TagType, present: &[ItemKey]) -> Vec<(ItemKey, String)> {
    let mut out: Vec<(ItemKey, String)> = ItemKey::supported_keys(tag_type)
        .iter()
        .filter(|k| !present.contains(k))
        .map(|k| (*k, format!("{k:?}")))
        .collect();
    out.sort_by_key(|(_, label)| label.to_lowercase());
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Mark every item whose key appears more than once as not editable.
///
/// These are written by key, so editing one of a repeated key would replace all
/// of them with the single value being edited. Leaving them alone keeps every
/// value the file has; it is better to show them and refuse than to merge them
/// behind the user's back.
fn mark_duplicates(extra: &mut [Extra]) {
    let repeated: Vec<ItemKey> = extra
        .iter()
        .filter(|e| extra.iter().filter(|o| o.key == e.key).count() > 1)
        .map(|e| e.key)
        .collect();
    for e in extra.iter_mut() {
        if repeated.contains(&e.key) {
            e.editable = false;
        }
    }
}

/// Write `fields` and `extra` back to `path`, leaving everything else alone.
///
/// The file's existing tag is edited rather than replaced, so pictures and
/// anything not listed here survive untouched. A value that has been emptied
/// removes its key instead of writing a blank one, which is what every other
/// tag editor does and what players expect.
pub fn write(
    path: &Path,
    fields: &[(Field, String)],
    extra: &[Extra],
    tag_type: TagType,
) -> std::io::Result<()> {
    let err = |e: String| std::io::Error::other(e);
    let mut f = lofty::read_from_path(path).map_err(|e| err(e.to_string()))?;
    // Edit the tag that is there; a file with none gets one of the kind its
    // format uses. (`primary_tag`, not the `_mut` form, so the check does not
    // hold a borrow across the insert.)
    if f.primary_tag().is_none() && f.first_tag().is_none() {
        f.insert_tag(Tag::new(tag_type));
    }
    let has_primary = f.primary_tag().is_some();
    let tag = if has_primary { f.primary_tag_mut() } else { f.first_tag_mut() }
        .ok_or_else(|| err("this file cannot hold tags".into()))?;
    for (field, value) in fields {
        let value = value.trim();
        if value.is_empty() {
            tag.remove_key(field.key());
            if *field == Field::Year {
                tag.remove_key(ItemKey::RecordingDate);
            }
            continue;
        }
        // A file that keeps its year inside a recording date (`1994-08-22`,
        // which is how ID3v2.4 and Vorbis do it) has its date rewritten rather
        // than gaining a second, separate year that would disagree with it.
        if *field == Field::Year
            && let Some(date) = tag.get_string(ItemKey::RecordingDate).map(str::to_string)
        {
            let rest: String = date.chars().skip_while(char::is_ascii_digit).collect();
            tag.insert_text(ItemKey::RecordingDate, format!("{value}{rest}"));
            continue;
        }
        tag.insert_text(field.key(), value.to_string());
    }
    // The items the program has no name for, which are edited by key. The ones
    // marked not editable are skipped rather than written, so a key the file
    // uses several times keeps every one of its values.
    for item in extra.iter().filter(|e| e.editable) {
        let value = item.value.trim();
        if value.is_empty() {
            tag.remove_key(item.key);
        } else {
            tag.insert_text(item.key, value.to_string());
        }
    }
    tag.save_to_path(path, WriteOptions::default()).map_err(|e| err(e.to_string()))
}

#[cfg(test)]
mod tests;
