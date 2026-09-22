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

use lofty::file::TaggedFileExt;
use lofty::tag::{ItemKey, Tag};
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

#[cfg(test)]
mod tests;
