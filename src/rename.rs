//! Multi-file rename engine.
//!
//! Expands a filename *mask* with placeholders into a new name for each
//! selected file, then optionally applies a search-and-replace and a case
//! transform. This is pure logic (no I/O, no clock) so it is fully unit-tested;
//! the date/time strings are captured once by the caller via [`date_time_now`].
//!
//! Supported mask placeholders (case-insensitive keyword, brackets literal when
//! unrecognised):
//! - `[N]` — file name without its extension; `[N3-5]`, `[N3-]`, `[N3]` slice it
//! - `[E]` — file extension without the dot; `[E1-2]` etc. slice it
//! - `[C]` — the running counter
//! - `[YMD]` — the captured date, `YYYYMMDD`
//! - `[hms]` — the captured time, `HHMMSS`
//! - `[EXIF:…]` / `[TAG:…]` — a value read from the file itself: a photo's EXIF
//!   or an audio file's tags, looked up in the [`FileMeta`] the caller resolved
//!   beforehand. A file that has no such value contributes nothing.

/// Per-file values a mask can reach that are not in the name: a photo's EXIF,
/// an audio file's tags.
///
/// Resolved once by the caller, before any expansion, and then only looked up:
/// the rename preview re-expands every visible row on every frame, so reading a
/// file here would mean reading it dozens of times a second. Keeping the values
/// in hand also leaves this module free of I/O, and so fully testable.
///
/// Keys are `"<prefix>:<token>"`, both lower-cased — `exif:model`, `tag:artist`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileMeta {
    vals: std::collections::BTreeMap<String, String>,
}

impl FileMeta {
    /// An empty set of values — usable in a constant, so a caller with nothing
    /// read yet can borrow one rather than build it per row.
    pub const fn new() -> Self {
        FileMeta { vals: std::collections::BTreeMap::new() }
    }

    /// Record `pairs` — `(token, value)` as the reading modules produce them —
    /// under `prefix`. Values are made safe to put in a file name; ones that
    /// sanitise away to nothing are dropped, so a mask referring to them
    /// expands to nothing rather than to whitespace.
    pub fn extend(&mut self, prefix: &str, pairs: impl IntoIterator<Item = (String, String)>) {
        for (token, value) in pairs {
            let value = sanitize_component(&value);
            if !value.is_empty() {
                self.vals.insert(format!("{}:{}", prefix, token.to_ascii_lowercase()), value);
            }
        }
    }

    /// The value for a full lower-cased key, e.g. `exif:model`.
    fn get(&self, key: &str) -> Option<&str> {
        self.vals.get(key).map(String::as_str)
    }
}

/// Make `s` safe to use as one component of a file name: drop the separators
/// and control characters no platform accepts, collapse runs of whitespace, and
/// trim. Long values are cut so one wordy tag cannot push a name past what the
/// filesystem will take.
///
/// Applied to metadata only — never to the mask the user typed, which is theirs
/// to write as they like.
pub fn sanitize_component(s: &str) -> String {
    /// Longest a single expanded value may be, in characters.
    const MAX: usize = 120;
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        // Whitespace is tested first: a tab is both whitespace and a control
        // character, and it should collapse to a space rather than vanish and
        // run the words either side of it together.
        if c.is_whitespace() {
            space = true;
            continue;
        }
        // `/` and `\` separate paths, the rest are refused by Windows; control
        // characters have no business in a name on any platform.
        if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.push(c);
        if out.chars().count() >= MAX {
            break;
        }
    }
    // A trailing dot or space is dropped by Windows, so never end with one.
    out.trim_end_matches(['.', ' ']).to_string()
}

/// How the generated name's letter case is transformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaseMode {
    /// Leave the case as produced by the mask.
    Unchanged,
    /// Lower-case the whole result.
    Lower,
    /// Upper-case the whole result.
    Upper,
}

impl CaseMode {
    /// All variants, in cycle order (matches the dialog's ◂ ▸ chooser).
    pub const ALL: [CaseMode; 3] = [CaseMode::Unchanged, CaseMode::Lower, CaseMode::Upper];

    /// Human label shown in the dialog.
    pub fn label(self) -> &'static str {
        match self {
            CaseMode::Unchanged => "unchanged",
            CaseMode::Lower => "lowercase",
            CaseMode::Upper => "UPPERCASE",
        }
    }
}

/// A complete multi-rename specification, applied per file by [`RenameRule::apply`].
#[derive(Debug, Clone)]
pub struct RenameRule {
    /// The rename mask with `[...]` placeholders.
    pub mask: String,
    pub case: CaseMode,
    /// Counter value for the first file.
    pub counter_start: i64,
    /// Added to the counter for each successive file.
    pub counter_step: i64,
    /// Minimum counter width (zero-padded).
    pub counter_digits: usize,
    /// Substring searched for in the generated name (empty = no replacement).
    pub search: String,
    /// What `search` is replaced with.
    pub replace: String,
    /// Whether `search` matches case-sensitively.
    pub search_case_sensitive: bool,
    /// Substituted for `[YMD]` (e.g. `"20260630"`).
    pub date: String,
    /// Substituted for `[hms]` (e.g. `"143007"`).
    pub time: String,
}

impl RenameRule {
    /// The new name for `original` at zero-based position `index`, with `meta`
    /// supplying any `[EXIF:…]` / `[TAG:…]` the mask asks for.
    pub fn apply(&self, original: &str, index: usize, meta: &FileMeta) -> String {
        let (stem, ext) = split_name(original);
        let counter = self.counter_start + (index as i64) * self.counter_step;
        let counter_str = format!("{counter:0width$}", width = self.counter_digits);

        let mut name =
            expand_mask(&self.mask, stem, ext, &counter_str, &self.date, &self.time, meta);
        // The default mask "[N].[E]" leaves a trailing dot for extension-less
        // files; drop the dot an empty [E] produced so it round-trips cleanly.
        if ext.is_empty() && name.ends_with('.') {
            name.pop();
        }
        let name = replace_all(&name, &self.search, &self.replace, self.search_case_sensitive);
        match self.case {
            CaseMode::Unchanged => name,
            CaseMode::Lower => name.to_lowercase(),
            CaseMode::Upper => name.to_uppercase(),
        }
    }
}

/// The mask prefix for a value read from a photo's EXIF.
pub const EXIF_PREFIX: &str = "exif:";
/// The mask prefix for a value read from an audio file's tags.
pub const TAG_PREFIX: &str = "tag:";

/// Split a file name into (stem, extension-without-dot). A leading dot is part of
/// the name (a dotfile like `.bashrc` has no extension); the split is on the last
/// interior dot (so `a.tar.gz` → `("a.tar", "gz")`).
fn split_name(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    }
}

/// Expand every `[...]` placeholder in `mask`. Unrecognised tokens are emitted
/// verbatim (brackets included) so literal `[`/`]` survive.
fn expand_mask(
    mask: &str,
    stem: &str,
    ext: &str,
    counter: &str,
    date: &str,
    time: &str,
    meta: &FileMeta,
) -> String {
    let chars: Vec<char> = mask.chars().collect();
    let mut out = String::with_capacity(mask.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '['
            && let Some(close) = (i + 1..chars.len()).find(|&j| chars[j] == ']')
        {
            let token: String = chars[i + 1..close].iter().collect();
            if let Some(sub) = substitute(&token, stem, ext, counter, date, time, meta) {
                out.push_str(&sub);
                i = close + 1;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Resolve a single placeholder token (the text between the brackets), or `None`
/// if it is not a recognised placeholder.
fn substitute(
    token: &str,
    stem: &str,
    ext: &str,
    counter: &str,
    date: &str,
    time: &str,
    meta: &FileMeta,
) -> Option<String> {
    let lower = token.to_ascii_lowercase();
    match lower.as_str() {
        "c" => return Some(counter.to_string()),
        "ymd" => return Some(date.to_string()),
        "hms" => return Some(time.to_string()),
        _ => {}
    }
    // Only these two prefixes are ours. Matching on "has a colon" would swallow
    // anything bracketed the user meant literally — `[10:30]` in a mask stays
    // `[10:30]`, the same as any other unrecognised token.
    if lower.starts_with(EXIF_PREFIX) || lower.starts_with(TAG_PREFIX) {
        // A file that lacks the value contributes nothing, rather than leaving
        // `[EXIF:Model]` sitting in the middle of the new name.
        return Some(meta.get(&lower).unwrap_or_default().to_string());
    }
    let first = token.chars().next()?;
    let source = match first.to_ascii_uppercase() {
        'N' => stem,
        'E' => ext,
        _ => return None,
    };
    let rest = &token[first.len_utf8()..];
    if rest.is_empty() {
        return Some(source.to_string());
    }
    let (start, end) = parse_range(rest, source.chars().count())?;
    Some(substr(source, start, end))
}

/// Parse a 1-based inclusive slice spec like `"3-5"`, `"3-"`, `"-5"` or `"3"`.
fn parse_range(spec: &str, len: usize) -> Option<(usize, usize)> {
    if let Some(dash) = spec.find('-') {
        let (l, r) = (&spec[..dash], &spec[dash + 1..]);
        let start = if l.is_empty() { 1 } else { l.parse().ok()? };
        let end = if r.is_empty() { len } else { r.parse().ok()? };
        Some((start, end))
    } else {
        let n = spec.parse().ok()?;
        Some((n, n))
    }
}

/// Characters `start..=end` (1-based, inclusive) of `s`, clamped to its length.
fn substr(s: &str, start: usize, end: usize) -> String {
    if start == 0 || end < start {
        return String::new();
    }
    s.chars().skip(start - 1).take(end - start + 1).collect()
}

/// Replace every occurrence of `needle` in `haystack` with `repl`. When
/// `case_sensitive` is false, matching is ASCII-case-insensitive (non-ASCII
/// bytes still match exactly, which keeps UTF-8 boundaries intact).
fn replace_all(haystack: &str, needle: &str, repl: &str, case_sensitive: bool) -> String {
    if needle.is_empty() {
        return haystack.to_string();
    }
    if case_sensitive {
        return haystack.replace(needle, repl);
    }
    let (hb, nb) = (haystack.as_bytes(), needle.as_bytes());
    let mut out = String::with_capacity(haystack.len());
    let mut i = 0;
    while i < hb.len() {
        if i + nb.len() <= hb.len() && hb[i..i + nb.len()].eq_ignore_ascii_case(nb) {
            out.push_str(repl);
            i += nb.len();
        } else {
            let c = haystack[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// Capture the current date (`YYYYMMDD`) and time (`HHMMSS`) for `[YMD]`/`[hms]`.
/// Uses the same calendar-agnostic (UTC) basis as the listing's `format_time`.
pub fn date_time_now() -> (String, String) {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, m, d, h, mi, s) = civil_from_unix(secs);
    (format!("{y:04}{m:02}{d:02}"), format!("{h:02}{mi:02}{s:02}"))
}

/// Convert a Unix timestamp (UTC) into civil (year, month, day, hour, min, sec).
/// Howard Hinnant's `civil_from_days` algorithm, with seconds.
fn civil_from_unix(secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (hour, min, sec) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d, hour, min, sec)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Expand with no metadata — what every case that is not about EXIF or
    /// tags wants, and what keeps those cases reading as they did.
    fn plain(r: &RenameRule, original: &str, index: usize) -> String {
        r.apply(original, index, &FileMeta::default())
    }

    fn rule(mask: &str) -> RenameRule {
        RenameRule {
            mask: mask.to_string(),
            case: CaseMode::Unchanged,
            counter_start: 1,
            counter_step: 1,
            counter_digits: 0,
            search: String::new(),
            replace: String::new(),
            search_case_sensitive: false,
            date: "20260630".to_string(),
            time: "143007".to_string(),
        }
    }

    #[test]
    fn default_mask_round_trips_names() {
        let r = rule("[N].[E]");
        assert_eq!(plain(&r, "photo.jpg", 0), "photo.jpg");
        assert_eq!(plain(&r, "archive.tar.gz", 0), "archive.tar.gz");
        // Extension-less files don't gain a trailing dot.
        assert_eq!(plain(&r, "README", 0), "README");
        // Dotfiles have no extension.
        assert_eq!(plain(&r, ".bashrc", 0), ".bashrc");
    }

    #[test]
    fn counter_increments_with_padding() {
        let mut r = rule("img[C].[E]");
        r.counter_digits = 3;
        assert_eq!(plain(&r, "a.png", 0), "img001.png");
        assert_eq!(plain(&r, "b.png", 1), "img002.png");
        assert_eq!(plain(&r, "c.png", 2), "img003.png");
    }

    #[test]
    fn counter_honours_start_and_step() {
        let mut r = rule("[C]");
        r.counter_start = 10;
        r.counter_step = 5;
        assert_eq!(plain(&r, "x", 0), "10");
        assert_eq!(plain(&r, "x", 1), "15");
        assert_eq!(plain(&r, "x", 2), "20");
    }

    #[test]
    fn substring_slices() {
        assert_eq!(plain(&rule("[N1-3]"), "hello.txt", 0), "hel");
        assert_eq!(plain(&rule("[N3-]"), "hello.txt", 0), "llo");
        assert_eq!(plain(&rule("[N2]"), "hello.txt", 0), "e");
        assert_eq!(plain(&rule("[E1-2]"), "a.jpeg", 0), "jp");
        // Out-of-range slices clamp to empty / available chars.
        assert_eq!(plain(&rule("[N9-12]"), "hi.txt", 0), "");
    }

    #[test]
    fn date_and_time_tokens() {
        assert_eq!(plain(&rule("[YMD]_[hms].[E]"), "a.log", 0), "20260630_143007.log");
    }

    #[test]
    fn unknown_tokens_stay_literal() {
        assert_eq!(plain(&rule("[X][N].[E]"), "a.txt", 0), "[X]a.txt");
        assert_eq!(plain(&rule("[N]([C]).[E]"), "a.txt", 0), "a(1).txt");
    }

    #[test]
    fn case_transforms() {
        let mut r = rule("[N].[E]");
        r.case = CaseMode::Upper;
        assert_eq!(plain(&r, "Photo.Jpg", 0), "PHOTO.JPG");
        r.case = CaseMode::Lower;
        assert_eq!(plain(&r, "Photo.Jpg", 0), "photo.jpg");
    }

    #[test]
    fn search_replace_respects_case_flag() {
        let mut r = rule("[N].[E]");
        r.search = "img".to_string();
        r.replace = "pic".to_string();
        // Case-insensitive by default: matches IMG / Img / img.
        assert_eq!(plain(&r, "IMG_01.jpg", 0), "pic_01.jpg");
        r.search_case_sensitive = true;
        assert_eq!(plain(&r, "IMG_01.jpg", 0), "IMG_01.jpg");
        assert_eq!(plain(&r, "img_01.jpg", 0), "pic_01.jpg");
    }

    fn meta(pairs: &[(&str, &str)]) -> FileMeta {
        let mut m = FileMeta::default();
        m.extend("exif", pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())));
        m
    }

    #[test]
    fn metadata_tokens_expand_and_are_case_insensitive() {
        let m = meta(&[("ymd", "20240714"), ("model", "Canon EOS 5D")]);
        let r = rule("[EXIF:YMD]_[EXIF:Model].[E]");
        assert_eq!(r.apply("a.jpg", 0, &m), "20240714_Canon EOS 5D.jpg");
        // The token is matched case-insensitively, like every other one.
        assert_eq!(rule("[exif:ymd]").apply("a.jpg", 0, &m), "20240714");
    }

    #[test]
    fn a_missing_metadata_value_expands_to_nothing() {
        let m = meta(&[("ymd", "20240714")]);
        // Not "[EXIF:Model]" left sitting in the name, and not the literal token.
        assert_eq!(rule("[EXIF:YMD][EXIF:Model].[E]").apply("a.jpg", 0, &m), "20240714.jpg");
        // A file with no metadata at all still renames by the rest of the mask.
        let empty = FileMeta::default();
        assert_eq!(rule("[EXIF:Model][N].[E]").apply("a.jpg", 0, &empty), "a.jpg");
    }

    #[test]
    fn tag_and_exif_prefixes_are_separate_namespaces() {
        let mut m = FileMeta::default();
        m.extend("exif", [("model".to_string(), "5D".to_string())]);
        m.extend("tag", [("artist".to_string(), "Portishead".to_string())]);
        assert_eq!(rule("[EXIF:Model]-[TAG:Artist]").apply("a.mp3", 0, &m), "5D-Portishead");
    }

    #[test]
    fn only_our_prefixes_are_consumed() {
        // A bracketed token that merely contains a colon is not metadata and
        // survives verbatim, exactly as any other unrecognised token does.
        let m = meta(&[("ymd", "20240714")]);
        assert_eq!(rule("[10:30][N].[E]").apply("a.txt", 0, &m), "[10:30]a.txt");
        assert_eq!(rule("[foo:bar]").apply("a.txt", 0, &m), "[foo:bar]");
    }

    #[test]
    fn metadata_values_are_made_safe_for_a_file_name() {
        // Separators and control characters go; whitespace runs collapse.
        let m = meta(&[("lens", "EF 24/70mm\tf:2.8")]);
        assert_eq!(rule("[EXIF:Lens]").apply("a.jpg", 0, &m), "EF 2470mm f2.8");
        assert_eq!(sanitize_component("  a\u{7}b  "), "ab");
        assert_eq!(sanitize_component("trailing. "), "trailing");
        // A value that is nothing but separators is dropped rather than stored.
        let m = meta(&[("model", "///")]);
        assert_eq!(rule("[EXIF:Model][N]").apply("a.jpg", 0, &m), "a");
    }

    #[test]
    fn a_long_metadata_value_is_cut() {
        let long = "x".repeat(500);
        let m = meta(&[("comment", &long)]);
        let out = rule("[EXIF:Comment]").apply("a.jpg", 0, &m);
        assert!(out.chars().count() <= 120, "cut to a sane length, got {}", out.chars().count());
    }

    #[test]
    fn case_labels_cover_all_variants() {
        for c in CaseMode::ALL {
            assert!(!c.label().is_empty());
        }
    }
}
