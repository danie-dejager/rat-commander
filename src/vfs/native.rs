//! Which native backend, if any, claims the file under the cursor.
//!
//! Rat Commander can open several kinds of file *as a directory*, and until now
//! each route was wired separately: the built-in archive formats had their own
//! probe in `enter_dir`, and everything else went through an `rc.ext` rule that
//! shells out to a Midnight Commander `extfs` script. This is the one place the
//! native providers beyond archives are listed, so the precedence is written
//! down once rather than implied by the order of a chain of `if`s.
//!
//! The order Enter resolves in:
//!
//! ```text
//! a real directory or ".."
//!   → a built-in archive  (zip, tar, 7z, rar)
//!   → a NATIVE PROVIDER   (here)
//!   → an rc.ext Open rule (a command, or an extfs script)
//!   → the image flasher, the default application, or just running it
//! ```
//!
//! A native provider shadows an `rc.ext` rule for the same reason the built-in
//! archive handler already does: it needs no external tool and it is faster. The
//! escape hatch is not a setting but the probe itself — **a provider may
//! decline**, and a `.iso` that turns out to hold no ISO 9660 volume (a UDF-only
//! image, say) falls straight through to whatever `rc.ext` says instead.

use crate::vfs::{VfsKind, VfsPath};
use std::path::Path;

/// A file the cursor is on that one of the native backends can open as a tree.
pub struct NativeOpen {
    pub path: VfsPath,
}

/// Whether `name` has one of `exts` as its extension, case-insensitively.
fn has_ext(name: &str, exts: &[&str]) -> bool {
    match name.rsplit_once('.') {
        Some((_, ext)) => exts.iter().any(|e| ext.eq_ignore_ascii_case(e)),
        None => false,
    }
}

/// The native provider that claims `file`, if any.
///
/// `kind` is the entry's own kind, so a directory named `foo.iso` is not mistaken
/// for an image. The check is cheap first and definite second: an extension
/// narrows the field without touching the disk, and only then does the provider
/// look inside to confirm.
pub fn probe(file: &Path, kind: VfsKind) -> Option<NativeOpen> {
    if kind != VfsKind::File {
        return None;
    }
    let name = file.file_name()?.to_string_lossy().into_owned();

    // A disc image. `looks_like_iso` reads the one sector that settles it, so a
    // `.iso` holding something else is declined rather than half-parsed.
    if has_ext(&name, &["iso", "img", "udf"]) && crate::vfs::iso::looks_like_iso(file) {
        return Some(NativeOpen {
            path: VfsPath { scheme: "iso".into(), path: "/".into(), container: Some(file.into()) },
        });
    }
    // A structured document, browsable as the tree it already is. Confirmed by
    // parsing rather than trusting the extension, so a `.json` that is broken
    // opens in the editor — where it can be fixed — instead of half-listing.
    for syntax in [crate::vfs::doc::Syntax::Json, crate::vfs::doc::Syntax::Toml] {
        if has_ext(&name, syntax.extensions()) && crate::vfs::doc::parses_as(syntax, file) {
            return Some(NativeOpen {
                path: VfsPath {
                    scheme: syntax.scheme().into(),
                    path: "/".into(),
                    container: Some(file.into()),
                },
            });
        }
    }

    // A SQLite database. Matched by its header magic rather than its extension:
    // these are called `.db`, `.sqlite`, `.sqlite3`, and very often nothing at
    // all, while plenty of unrelated files are called `.db` too.
    #[cfg(feature = "sqlite")]
    if has_ext(&name, &["db", "sqlite", "sqlite3", "db3"])
        && crate::vfs::sqlite::looks_like_sqlite(file)
    {
        return Some(NativeOpen {
            path: VfsPath {
                scheme: "sqlite".into(),
                path: "/".into(),
                container: Some(file.into()),
            },
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn scratch(tag: &str) -> PathBuf {
        let p = crate::util::temp::rc_temp_path(&format!("native-{tag}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn extension_matching_ignores_case_and_needs_a_dot() {
        assert!(has_ext("disc.iso", &["iso"]));
        assert!(has_ext("DISC.ISO", &["iso"]));
        assert!(has_ext("a.b.Iso", &["iso"]));
        assert!(!has_ext("iso", &["iso"]), "a bare name is not an extension");
        assert!(!has_ext("disc.iso.gz", &["iso"]));
    }

    /// A directory called `foo.iso` is a directory, not an image.
    #[test]
    fn a_directory_is_never_claimed() {
        let root = scratch("dir");
        let d = root.join("looks.iso");
        std::fs::create_dir_all(&d).unwrap();
        assert!(probe(&d, VfsKind::Dir).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    /// The escape hatch: a file with the right name but the wrong contents is
    /// declined, so an `rc.ext` rule still gets its chance at it.
    #[test]
    fn a_file_that_only_looks_like_an_image_is_declined() {
        let root = scratch("decline");
        let f = root.join("udf-only.iso");
        std::fs::write(&f, vec![0u8; 40 * 2048]).unwrap();
        assert!(probe(&f, VfsKind::File).is_none());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nothing_claims_an_ordinary_file() {
        let root = scratch("plain");
        let f = root.join("notes.txt");
        std::fs::write(&f, b"hi").unwrap();
        assert!(probe(&f, VfsKind::File).is_none());
        std::fs::remove_dir_all(&root).ok();
    }
}
