//! What kind of thing a filename is, by extension.
//!
//! One table, two readers: the panel listing paints a name in its category's
//! accent colour, and the 3D view's fsn style *also* picks the solid it draws a
//! file as. Keeping the extension lists here means the two can never disagree
//! about what a `.zip` is.

use crate::ui::theme::Theme;
use ratatui::style::Color;

/// A broad file kind the UI gives its own colour (and, in the 3D view, its own
/// shape). Only the well-known ones — anything else is a plain file and takes
/// the theme's ordinary file colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileCategory {
    Archive,
    Document,
    Image,
    Media,
    /// 3D models and CAD exchange formats. Broader than what
    /// [`crate::mesh`] can actually parse and show: a `.step` is
    /// unmistakably a model file and should be coloured as one in the listing
    /// whether or not the viewer has a reader for it yet.
    Model,
}

const ARCHIVE: &[&str] = &[
    "zip", "rar", "7z", "tar", "gz", "tgz", "bz2", "tbz2", "tbz", "xz", "txz", "zst", "lz", "lzma",
    "z", "deb", "rpm", "jar", "war", "apk", "cab", "arj", "lha", "lzh", "iso", "dmg", "pkg", "msi",
    "xz2",
];
const DOCUMENT: &[&str] = &[
    "txt", "md", "rst", "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "ppt", "pptx",
    "odp", "csv", "tex", "epub", "djvu", "mobi", "log", "json", "xml", "yaml", "yml", "toml",
    "ini", "cfg", "conf", "html", "htm", "css",
];
const IMAGE: &[&str] = &[
    "jpg", "jpeg", "png", "gif", "bmp", "svg", "webp", "tiff", "tif", "ico", "ppm", "pgm", "xpm",
    "heic", "heif", "raw", "cr2", "nef", "psd", "xcf",
];
const MODEL: &[&str] = &[
    "stl", "obj", "ply", "3mf", "gltf", "glb", "fbx", "dae", "blend", "step", "stp", "iges", "igs",
    "3ds", "off", "x3d", "usdz", "scad",
];
const MEDIA: &[&str] = &[
    "wav", "mp3", "flac", "ogg", "oga", "opus", "aac", "m4a", "wma", "mid", "midi", "aiff", "mp4",
    "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v", "mpg", "mpeg", "3gp", "ts", "vob",
];

/// Extensions that mark a file as a program or a library.
///
/// Only the 3D view consults this. The panel listing has the real mode bit to
/// go on and uses that instead; the 3D view is drawn from the size crawler's
/// cache, which records a file's path and size but not its permissions — so an
/// extension is all there is, and an extension-less Unix executable is drawn as
/// an ordinary file.
const EXECUTABLE: &[&str] = &[
    "exe", "bat", "cmd", "com", "scr", "sh", "bash", "zsh", "fish", "ps1", "appimage", "bin",
    "run", "elf", "so", "dll", "dylib", "o", "a", "ko", "wasm",
];

/// The category `ext` belongs to, or `None` for an ordinary file.
pub fn categorize(ext: &str) -> Option<FileCategory> {
    let e = ext.to_ascii_lowercase();
    let e = e.as_str();
    if ARCHIVE.contains(&e) {
        Some(FileCategory::Archive)
    } else if DOCUMENT.contains(&e) {
        Some(FileCategory::Document)
    } else if IMAGE.contains(&e) {
        Some(FileCategory::Image)
    } else if MEDIA.contains(&e) {
        Some(FileCategory::Media)
    } else if MODEL.contains(&e) {
        Some(FileCategory::Model)
    } else {
        None
    }
}

/// The theme's accent colour for a category.
pub fn category_color(cat: FileCategory, theme: &Theme) -> Color {
    match cat {
        FileCategory::Archive => theme.archive_fg,
        FileCategory::Document => theme.doc_fg,
        FileCategory::Image => theme.image_fg,
        FileCategory::Media => theme.media_fg,
        FileCategory::Model => theme.model_fg,
    }
}

/// Whether `ext` names a program or a library. See [`EXECUTABLE`].
pub fn is_executable_ext(ext: &str) -> bool {
    EXECUTABLE.contains(&ext.to_ascii_lowercase().as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn well_known_extensions_land_in_their_category() {
        assert_eq!(categorize("zip"), Some(FileCategory::Archive));
        assert_eq!(categorize("deb"), Some(FileCategory::Archive));
        assert_eq!(categorize("pdf"), Some(FileCategory::Document));
        assert_eq!(categorize("png"), Some(FileCategory::Image));
        assert_eq!(categorize("flac"), Some(FileCategory::Media));
    }

    #[test]
    fn the_match_ignores_case_and_unknown_extensions_are_plain_files() {
        assert_eq!(categorize("PNG"), Some(FileCategory::Image));
        assert_eq!(categorize("TaR"), Some(FileCategory::Archive));
        assert_eq!(categorize("rs"), None, "source files have no category of their own");
        assert_eq!(categorize(""), None, "a name with no extension at all");
    }

    #[test]
    fn programs_are_recognised_by_extension_only() {
        assert!(is_executable_ext("exe"));
        assert!(is_executable_ext("AppImage"), "the match is case-insensitive");
        assert!(is_executable_ext("so"), "shared libraries count too");
        // The cache the 3D view reads has no mode bit, so this is all we get.
        assert!(!is_executable_ext(""), "an extension-less program cannot be told apart");
        assert!(!is_executable_ext("txt"));
    }
}
