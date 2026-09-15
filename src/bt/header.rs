//! The comment header of an 010 Editor template: what it parses and which files
//! it is for.
//!
//! ```text
//! //------------------------------------------------
//! //--- 010 Editor v7.0 Binary Template
//! //      File: ZIP.bt
//! //  Category: Archive
//! //   Purpose: Parse ZIP archive files.
//! // File Mask: *.zip,*.apk
//! //  ID Bytes: 50 4B //PK
//! ```
//!
//! ID Bytes are hex pairs, `[+N]` / `[+0xN]` skips, and comma-separated
//! alternatives of which any may match; they are compared against the start of
//! the file only, as 010 Editor does.

use std::path::PathBuf;

/// How much of a file's start the ID Bytes are matched against.
pub const ID_WINDOW: usize = 2048;

/// Where a template on disk came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// Deployed from the bundle and unchanged.
    BuiltIn,
    /// Deployed from the bundle, then edited.
    Modified,
    /// Written by the user.
    User,
}

/// One template, as described by its header.
#[derive(Debug, Clone)]
pub struct TemplateInfo {
    /// Where it is read from; `None` for a template only in the bundle.
    pub path: Option<PathBuf>,
    pub file_name: String,
    pub category: String,
    pub purpose: String,
    pub version: String,
    pub masks: Vec<String>,
    pub ids: Vec<IdPattern>,
    /// The raw `ID Bytes:` text, for display.
    pub id_text: String,
    pub origin: Origin,
}

/// One token of an ID Bytes alternative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdTok {
    Byte(u8),
    Skip(usize),
}

/// One ID Bytes alternative.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdPattern(pub Vec<IdTok>);

impl IdPattern {
    /// How many bytes it pins down — the more, the more specific the match.
    pub fn fixed(&self) -> usize {
        self.0.iter().filter(|t| matches!(t, IdTok::Byte(_))).count()
    }

    /// Whether every byte it pins down is printable ASCII.
    pub fn is_text(&self) -> bool {
        self.0.iter().all(|t| match t {
            IdTok::Byte(b) => (0x20..0x7f).contains(b),
            IdTok::Skip(_) => true,
        })
    }

    /// Whether `head` (the start of a file) matches.
    pub fn matches(&self, head: &[u8]) -> bool {
        let mut at = 0usize;
        for t in &self.0 {
            match *t {
                IdTok::Skip(n) => at = at.saturating_add(n),
                IdTok::Byte(b) => {
                    if at >= ID_WINDOW || head.get(at) != Some(&b) {
                        return false;
                    }
                    at += 1;
                }
            }
        }
        true
    }
}

/// Parse an `ID Bytes:` value. Alternatives that don't parse (odd hex digits,
/// no bytes at all) are dropped rather than matching everything.
pub fn parse_ids(text: &str) -> Vec<IdPattern> {
    let text = text.split("//").next().unwrap_or("");
    let mut out = Vec::new();
    'alt: for alt in text.split(',') {
        let mut toks = Vec::new();
        let mut rest = alt.trim();
        while !rest.is_empty() {
            if let Some(r) = rest.strip_prefix('[') {
                let Some(end) = r.find(']') else { continue 'alt };
                let inner = r[..end].trim();
                let Some(num) = inner.strip_prefix('+') else { continue 'alt };
                let num = num.trim();
                let n = if let Some(h) = num.strip_prefix("0x").or_else(|| num.strip_prefix("0X")) {
                    usize::from_str_radix(h, 16)
                } else {
                    num.parse::<usize>()
                };
                let Ok(n) = n else { continue 'alt };
                toks.push(IdTok::Skip(n));
                rest = r[end + 1..].trim_start();
                continue;
            }
            let end = rest.find(|c: char| c.is_whitespace() || c == '[').unwrap_or(rest.len());
            let word = &rest[..end];
            if word.len() % 2 != 0 || !word.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue 'alt;
            }
            for pair in word.as_bytes().chunks(2) {
                let s = std::str::from_utf8(pair).unwrap_or("00");
                toks.push(IdTok::Byte(u8::from_str_radix(s, 16).unwrap_or(0)));
            }
            rest = rest[end..].trim_start();
        }
        let p = IdPattern(toks);
        if p.fixed() > 0 {
            out.push(p);
        }
    }
    out
}

/// Split a `File Mask:` value into its masks.
pub fn parse_masks(text: &str) -> Vec<String> {
    text.split([',', ';']).map(|m| m.trim()).filter(|m| !m.is_empty()).map(str::to_string).collect()
}

/// The value of the header field `key` in `text` (a `// Key: value` line in the
/// leading comments), trimmed.
fn field<'a>(head: &'a str, keys: &[&str]) -> Option<&'a str> {
    for line in head.lines() {
        let Some(rest) = line.trim_start().strip_prefix("//") else { continue };
        let Some((k, v)) = rest.split_once(':') else { continue };
        let k = k.trim();
        if keys.iter().any(|want| k.eq_ignore_ascii_case(want)) {
            return Some(v.trim());
        }
    }
    None
}

/// Read a template's header. `name` is its file name; only the first few
/// kilobytes of `source` are looked at.
pub fn parse_header(name: &str, source: &[u8]) -> TemplateInfo {
    let head = &source[..source.len().min(8192)];
    let head = String::from_utf8_lossy(head);
    // Header fields live in the leading comment block; stop at the first line
    // of code so a `// Category:` comment deep in the file isn't taken.
    let mut end = head.len();
    let mut in_block = false;
    let mut pos = 0;
    for line in head.split_inclusive('\n') {
        let t = line.trim();
        if in_block {
            if t.contains("*/") {
                in_block = false;
            }
        } else if t.starts_with("/*") {
            in_block = !t.contains("*/");
        } else if !(t.is_empty() || t.starts_with("//") || t.starts_with('#')) {
            end = pos;
            break;
        }
        pos += line.len();
    }
    let head = &head[..end];
    let id_text = field(head, &["ID Bytes"]).unwrap_or("").to_string();
    TemplateInfo {
        path: None,
        file_name: name.to_string(),
        category: field(head, &["Category"]).unwrap_or("").to_string(),
        purpose: field(head, &["Purpose"]).unwrap_or("").to_string(),
        version: field(head, &["Version"]).unwrap_or("").to_string(),
        masks: parse_masks(field(head, &["File Mask", "FileMask"]).unwrap_or("")),
        ids: parse_ids(&id_text),
        id_text: id_text.split("//").next().unwrap_or("").trim().to_string(),
        origin: Origin::BuiltIn,
    }
}

/// Whether `mask` (with `*` and `?` wildcards) matches `name`, ignoring case.
pub fn mask_matches(mask: &str, name: &str) -> bool {
    fn go(m: &[char], n: &[char]) -> bool {
        match m.split_first() {
            None => n.is_empty(),
            Some(('*', rest)) => (0..=n.len()).any(|i| go(rest, &n[i..])),
            Some(('?', rest)) => !n.is_empty() && go(rest, &n[1..]),
            Some((c, rest)) => {
                n.first().is_some_and(|x| x.to_lowercase().eq(c.to_lowercase()))
                    && go(rest, &n[1..])
            }
        }
    }
    let m: Vec<char> = mask.chars().collect();
    let n: Vec<char> = name.chars().collect();
    // Collapse runs of `*` so the backtracking stays linear-ish.
    let mut mm: Vec<char> = Vec::with_capacity(m.len());
    for c in m {
        if !(c == '*' && mm.last() == Some(&'*')) {
            mm.push(c);
        }
    }
    go(&mm, &n)
}

/// Whether a mask matches every file, so it says nothing about the format.
pub fn mask_is_wildcard(mask: &str) -> bool {
    matches!(mask, "*" | "*.*")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_bytes_parse_skips_alternatives_and_comments() {
        let png = parse_ids("89 50 4E 47 //%PNG");
        assert_eq!(png.len(), 1);
        assert!(png[0].matches(b"\x89PNG\r\n"));
        assert!(!png[0].matches(b"\x89PNX"));

        let wav = parse_ids("52 49 46 46 [+4] 57 41 56 45 //RIFF????WAVE");
        assert!(wav[0].matches(b"RIFF\0\0\0\0WAVEfmt "));
        assert!(!wav[0].matches(b"RIFF\0\0\0\0AVI "));

        let mp4 = parse_ids("[+4] 66 74 79 70, [+4] 6D 6F 6F 76, [+4] 6D 64 61 74");
        assert_eq!(mp4.len(), 3);
        assert!(mp4.iter().any(|p| p.matches(b"\0\0\0\x18moov")));

        let mobi = parse_ids("[+60]42 4F 4F 4B, [+60]54 45 58 74 //BOOK,TEXt");
        let mut f = vec![0u8; 64];
        f[60..].copy_from_slice(b"TEXt");
        assert!(mobi.iter().any(|p| p.matches(&f)));

        let dicom = parse_ids("[+128] 44 49 43 4D //DICM");
        let mut d = vec![0u8; 132];
        d[128..].copy_from_slice(b"DICM");
        assert!(dicom[0].matches(&d));
        assert!(parse_ids("[+0x1FE] 55 AA")[0].0[0] == IdTok::Skip(0x1FE));

        // Run-together pairs split; junk alternatives are dropped, not matched.
        assert_eq!(parse_ids("6D73 636878 756470 //mschxudp")[0].fixed(), 8);
        assert!(parse_ids("1").is_empty());
        assert!(parse_ids("[+0x200]").is_empty());
        assert!(parse_ids("").is_empty());
        assert_eq!(parse_ids("46 115 99 102").len(), 0);
    }

    #[test]
    fn a_pattern_past_the_id_window_never_matches() {
        let p = parse_ids("[+4096] 41");
        let mut f = vec![0u8; 4200];
        f[4096] = 0x41;
        assert!(!p[0].matches(&f));
    }

    #[test]
    fn masks_split_and_match_without_case() {
        assert_eq!(parse_masks("*.zip,*.apk; *.JAR"), vec!["*.zip", "*.apk", "*.JAR"]);
        assert!(mask_matches("*.zip", "Archive.ZIP"));
        assert!(mask_matches("Drive*", "Drive C"));
        assert!(mask_matches("*.v??", "a.vhd"));
        assert!(!mask_matches("*.zip", "zip"));
        assert!(mask_matches("*", "anything"));
        assert!(mask_is_wildcard("*") && !mask_is_wildcard("*.bin"));
    }

    #[test]
    fn the_header_fields_come_from_the_leading_comments() {
        let src = b"//------\n//--- 010 Editor v7.0 Binary Template\n//\n//      File: ZIP.bt\n\
            //   Authors: SweetScape Software\n//   Purpose: Parse ZIP archive files.\n\
            //  Category: Archive\n// File Mask: *.zip,*.apk\n//  ID Bytes: 50 4B //PK\n\
            //------\nRequiresVersion(14);\n// Category: Wrong\n";
        let info = parse_header("ZIP.bt", src);
        assert_eq!(info.category, "Archive");
        assert_eq!(info.purpose, "Parse ZIP archive files.");
        assert_eq!(info.masks, vec!["*.zip", "*.apk"]);
        assert_eq!(info.ids.len(), 1);
        assert_eq!(info.id_text, "50 4B");
    }
}
