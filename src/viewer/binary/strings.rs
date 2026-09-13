//! The readable text in a binary, as `strings` finds it — with less noise.
//!
//! A run of at least [`MIN_CHARS`] printable characters counts. Two encodings
//! are looked for in the same pass:
//!
//! - **UTF-8**, not just ASCII: messages in a localized program, month names in
//!   a dozen scripts, box-drawing and arrows in a TUI's help text.
//! - **UTF-16 little-endian**, which is how Windows keeps most of its text.
//!   Only code units with a zero high byte are taken (ASCII and Latin-1): a run
//!   of "anything valid in the BMP" would match most random data.
//!
//! Data tables and machine code are full of byte runs that merely happen to be
//! printable, and a plain `strings` buries the real text under them. A few rules
//! drop most of that and almost none of the text:
//!
//! - A run needs a letter or a digit, and more than one distinct character, so
//!   `----`, `AAAA` and `\t\t\t\t` are not strings.
//! - A UTF-8 run is cut where a letter of one script runs straight into a letter
//!   of another — Latin into Cyrillic, Greek, Hebrew and so on. Words never do
//!   that; two random bytes that happen to decode as `г` beside a `Y` do it
//!   constantly.
//! - A UTF-16 run needs an ASCII letter or digit, and at most a quarter of its
//!   characters beyond ASCII: localized Windows text passes, and tables of
//!   `0xFF 0x00` pairs, which read as `ÿÿÿÿ`, do not.
//! - Inside a code section only ASCII counts, and a run needs [`CODE_MIN_CHARS`]
//!   of it with a space somewhere. x86 function prologues spell out
//!   `UAWAVAUATSH` in printable bytes by the thousand; the banners and messages
//!   that assembly does embed there are sentences.
//!
//! The file is streamed in chunks and never held whole.

use super::{MAX_ROWS, Str};
use std::sync::atomic::{AtomicBool, Ordering};

/// Shortest run reported, in characters — the same default as `strings`.
pub const MIN_CHARS: usize = 4;

/// Shortest run reported inside a code section.
pub const CODE_MIN_CHARS: usize = 16;

/// Longest text kept for one string, in characters. A longer run is still one
/// entry, cut here, so a megabyte of embedded text cannot use up the memory the
/// row cap is there to bound.
pub const MAX_CHARS: usize = 2048;

const CHUNK: usize = 1 << 20;

/// Scan `size` bytes starting at `base`, reading through `read(offset, len)`.
/// `code` holds the file ranges `(start, end)` of code sections, sorted.
/// Returns the strings in file order and whether the list stopped at
/// [`MAX_ROWS`]; `None` if `cancel` was raised part-way.
pub fn scan(
    mut read: impl FnMut(u64, usize) -> Vec<u8>,
    base: u64,
    size: u64,
    code: &[(u64, u64)],
    cancel: &AtomicBool,
) -> Option<(Vec<Str>, bool)> {
    let mut s = Scanner::default();
    let end = base.saturating_add(size);
    let mut at = base;
    while at < end && !s.full() {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        // Read up to the next change between code and anything else, so a whole
        // chunk is scanned under one set of rules.
        let (in_code, stretch_end) = match code.iter().find(|r| r.1 > at) {
            Some(&(start, stop)) if start <= at => (true, stop),
            Some(&(start, _)) => (false, start),
            None => (false, end),
        };
        if in_code != s.kind.code() {
            s.finish();
            s.kind = if in_code { Kind::Code } else { Kind::Data };
        }
        let want = (stretch_end.min(end) - at).min(CHUNK as u64) as usize;
        let buf = read(at, want);
        if buf.is_empty() {
            break;
        }
        for (i, &b) in buf.iter().enumerate() {
            s.feed(at + i as u64, b);
        }
        at += buf.len() as u64;
    }
    s.finish();
    let capped = s.full();
    let mut out = s.out;
    out.truncate(MAX_ROWS);
    // The two encodings finish their runs at different moments; show them in
    // the order they sit in the file.
    out.sort_by_key(|s| s.offset);
    Some((out, capped))
}

/// Which rules a run is judged by.
#[derive(Default, Clone, Copy, PartialEq, Eq)]
enum Kind {
    #[default]
    Data,
    Code,
    Wide,
}

impl Kind {
    fn code(self) -> bool {
        self == Kind::Code
    }
}

#[derive(Default)]
struct Run {
    start: u64,
    text: String,
    chars: usize,
    first: Option<char>,
    last: Option<char>,
    /// The character before `last`.
    prev: Option<char>,
    /// Some character differs from the first.
    varied: bool,
    alnum: bool,
    ascii_alnum: bool,
    space: bool,
    non_ascii: usize,
}

impl Run {
    fn push(&mut self, at: u64, c: char) {
        if self.chars == 0 {
            self.start = at;
            self.first = Some(c);
        }
        if self.chars < MAX_CHARS {
            self.text.push(c);
        }
        self.chars += 1;
        self.prev = self.last;
        self.last = Some(c);
        self.varied |= self.first != Some(c);
        self.alnum |= c.is_alphanumeric();
        self.ascii_alnum |= c.is_ascii_alphanumeric();
        self.space |= c == ' ';
        self.non_ascii += usize::from(!c.is_ascii());
    }

    fn end(&mut self, out: &mut Vec<Str>, kind: Kind) {
        // Called for nearly every byte that is not text, so an empty run must
        // cost nothing.
        if self.chars == 0 {
            return;
        }
        // A lone letter of another script stuck on the end by punctuation is a
        // stray byte pair, not part of the text (see `Scanner::push_narrow`).
        if kind == Kind::Data
            && self.chars > 1
            && self.last.is_some_and(foreign)
            && self.prev.is_some_and(|p| !p.is_alphabetic() && p != ' ')
        {
            if self.chars <= MAX_CHARS {
                self.text.pop();
            }
            self.chars -= 1;
            self.non_ascii -= 1;
        }
        let keep = self.varied
            && out.len() < MAX_ROWS
            && match kind {
                Kind::Data => self.chars >= MIN_CHARS && self.alnum,
                Kind::Code => self.chars >= CODE_MIN_CHARS && self.alnum && self.space,
                Kind::Wide => {
                    self.chars >= MIN_CHARS && self.ascii_alnum && self.non_ascii * 4 <= self.chars
                }
            };
        if keep {
            out.push(Str {
                offset: self.start,
                text: std::mem::take(&mut self.text),
                wide: kind == Kind::Wide,
            });
        }
        // Start over, keeping the text buffer's allocation for the next run.
        let mut text = std::mem::take(&mut self.text);
        text.clear();
        *self = Run { text, ..Run::default() };
    }
}

/// Whether `a` then `b` is a letter of one script running straight into a letter
/// of another: an ASCII letter against a non-Latin one. Accented Latin letters
/// (`ö`, `ů`, `ñ`) sit beside plain ones in ordinary words, so they do not count.
fn script_clash(a: char, b: char) -> bool {
    (a.is_ascii_alphabetic() && foreign(b)) || (foreign(a) && b.is_ascii_alphabetic())
}

/// A letter that is neither ASCII nor accented Latin.
fn foreign(c: char) -> bool {
    c.is_alphabetic()
        && !c.is_ascii()
        && !matches!(c, '\u{c0}'..='\u{24f}' | '\u{1e00}'..='\u{1eff}')
}

#[derive(Default)]
struct Scanner {
    out: Vec<Str>,
    /// Data or code: the rules the 8-bit run is judged by.
    kind: Kind,
    narrow: Run,
    /// A multi-byte UTF-8 sequence part-way through: its bytes, how many it
    /// needs in all, and where it began.
    seq: [u8; 4],
    seq_len: usize,
    seq_need: usize,
    seq_start: u64,
    /// UTF-16 runs, one for code units starting at even offsets and one for odd,
    /// each with the low byte it is holding while it waits for the high one.
    wide: [Run; 2],
    low: [Option<u8>; 2],
}

impl Scanner {
    fn full(&self) -> bool {
        self.out.len() >= MAX_ROWS
    }

    fn feed(&mut self, at: u64, b: u8) {
        if self.kind.code() {
            match b {
                b'\t' | 0x20..=0x7e => self.narrow.push(at, b as char),
                _ => self.narrow.end(&mut self.out, Kind::Code),
            }
            return;
        }
        self.feed_narrow(at, b);
        self.feed_wide(at, b);
    }

    /// Add a decoded character to the 8-bit run, starting a new run first where
    /// it would make two scripts collide — or where the run so far is one letter
    /// of another script pressed straight against punctuation, which is how the
    /// last two bytes of a hash read when real text follows them. (A space after
    /// it keeps it: `В папке` starts with a word.)
    fn push_narrow(&mut self, at: u64, c: char) {
        let stray = self.narrow.chars == 1
            && self.narrow.last.is_some_and(foreign)
            && !c.is_alphabetic()
            && c != ' ';
        if stray || self.narrow.last.is_some_and(|last| script_clash(last, c)) {
            // `end` drops what is too short to be a string, so a stray letter
            // simply vanishes.
            self.narrow.end(&mut self.out, Kind::Data);
        }
        self.narrow.push(at, c);
    }

    fn feed_narrow(&mut self, at: u64, b: u8) {
        if self.seq_need > 0 {
            if (0x80..=0xbf).contains(&b) {
                self.seq[self.seq_len] = b;
                self.seq_len += 1;
                if self.seq_len == self.seq_need {
                    self.seq_need = 0;
                    let decoded = std::str::from_utf8(&self.seq[..self.seq_len])
                        .ok()
                        .and_then(|s| s.chars().next());
                    match decoded {
                        // Private-use characters stand for nothing without the
                        // font that defines them; in a binary they are noise.
                        Some(c) if !c.is_control() && !('\u{e000}'..='\u{f8ff}').contains(&c) => {
                            self.push_narrow(self.seq_start, c)
                        }
                        _ => self.narrow.end(&mut self.out, Kind::Data),
                    }
                }
                return;
            }
            // Cut short: what came before stands on its own, and this byte
            // starts afresh.
            self.seq_need = 0;
            self.narrow.end(&mut self.out, Kind::Data);
        }
        match b {
            b'\t' | 0x20..=0x7e => self.push_narrow(at, b as char),
            0xc2..=0xf4 => {
                self.seq[0] = b;
                self.seq_len = 1;
                self.seq_need = match b {
                    0xc2..=0xdf => 2,
                    0xe0..=0xef => 3,
                    _ => 4,
                };
                self.seq_start = at;
            }
            _ => self.narrow.end(&mut self.out, Kind::Data),
        }
    }

    fn feed_wide(&mut self, at: u64, b: u8) {
        let lane = (at & 1) as usize;
        // Byte `at` is the low half of a code unit in its own lane, and the high
        // half of the one the other lane began a byte ago.
        let other = 1 - lane;
        if let Some(lo) = self.low[other].take() {
            let printable = b == 0 && matches!(lo, b'\t' | 0x20..=0x7e | 0xa0..=0xff);
            if printable {
                self.wide[other].push(at - 1, lo as char);
            } else {
                self.wide[other].end(&mut self.out, Kind::Wide);
            }
        }
        self.low[lane] = Some(b);
    }

    /// End every run in progress, and forget half-read characters.
    fn finish(&mut self) {
        self.narrow.end(&mut self.out, self.kind);
        let [a, b] = &mut self.wide;
        a.end(&mut self.out, Kind::Wide);
        b.end(&mut self.out, Kind::Wide);
        self.seq_need = 0;
        self.low = [None; 2];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scan_all(data: &[u8], code: &[(u64, u64)]) -> Vec<Str> {
        let read = |start: u64, n: usize| {
            let s = (start as usize).min(data.len());
            data[s..(s + n).min(data.len())].to_vec()
        };
        scan(read, 0, data.len() as u64, code, &AtomicBool::new(false)).unwrap().0
    }

    fn run(data: &[u8]) -> Vec<Str> {
        scan_all(data, &[])
    }

    fn texts(found: &[Str]) -> Vec<(&str, bool)> {
        found.iter().map(|s| (s.text.as_str(), s.wide)).collect()
    }

    fn wide(s: &str) -> Vec<u8> {
        s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect()
    }

    #[test]
    fn ascii_runs_of_four_or_more_are_found_with_their_offsets() {
        let found = run(b"\x00\x01abc\x00hello\x02world!\xff");
        assert_eq!(texts(&found), vec![("hello", false), ("world!", false)]);
        assert_eq!(found[0].offset, 6);
        assert_eq!(found[1].offset, 12);
    }

    #[test]
    fn utf8_text_counts_by_characters_and_stays_whole() {
        let mut data = vec![0u8];
        data.extend_from_slice("Größe: 5 €".as_bytes());
        data.push(0);
        // Three characters, though six bytes: too short.
        data.extend_from_slice("äöü".as_bytes());
        data.push(0);
        data.extend_from_slice("Файл не найден".as_bytes());
        data.push(0);
        let found = run(&data);
        assert_eq!(texts(&found), vec![("Größe: 5 €", false), ("Файл не найден", false)]);
        assert_eq!(found[0].offset, 1);
    }

    #[test]
    fn a_broken_utf8_sequence_ends_the_run_but_keeps_what_came_before() {
        // 0xe2 0x82 would begin '€' but is cut off by a plain letter.
        let found = run(b"abcd\xe2\x82efgh");
        assert_eq!(texts(&found), vec![("abcd", false), ("efgh", false)]);
    }

    #[test]
    fn letters_of_two_scripts_running_together_split_the_run() {
        let mut data = "гY>Y".as_bytes().to_vec();
        data.push(0);
        data.extend_from_slice("Бbogomips per cpu".as_bytes());
        data.push(0);
        // A space between the two scripts is how real text mixes them.
        data.extend_from_slice("Error: файл".as_bytes());
        data.push(0);
        let found = run(&data);
        assert_eq!(texts(&found), vec![("bogomips per cpu", false), ("Error: файл", false)]);
    }

    #[test]
    fn a_stray_foreign_letter_at_either_end_is_trimmed_but_a_one_letter_word_is_not() {
        let mut data = vec![0x3b, 0x99];
        data.extend_from_slice("Օ/lib64/ld-linux-x86-64.so.2".as_bytes());
        data.push(0);
        data.extend_from_slice("path/to/file:Ж".as_bytes());
        data.push(0);
        data.extend_from_slice("В папке нет файлов".as_bytes());
        data.push(0);
        let found = run(&data);
        assert_eq!(
            texts(&found),
            vec![
                ("/lib64/ld-linux-x86-64.so.2", false),
                ("path/to/file:", false),
                ("В папке нет файлов", false)
            ]
        );
        assert_eq!(found[0].offset, 4, "the string starts after the stray letter");
    }

    #[test]
    fn utf16le_is_found_at_either_alignment() {
        let mut data = vec![0xffu8];
        data.extend(wide("Hello"));
        data.extend([0xff, 0xff, 0xff]);
        data.extend(wide("Dateigröße"));
        let found = run(&data);
        assert_eq!(texts(&found), vec![("Hello", true), ("Dateigröße", true)]);
        assert_eq!(found[0].offset, 1, "odd alignment");
        assert_eq!(found[1].offset, 14, "even alignment");
    }

    #[test]
    fn utf16_made_mostly_of_latin1_bytes_is_table_data() {
        let mut data = wide("ÿÿÿÿÿÿ");
        data.extend([0xff, 0xff]);
        data.extend(wide("JïKïKïK"));
        data.extend([0xff, 0xff]);
        assert!(run(&data).is_empty());
    }

    #[test]
    fn runs_of_one_character_or_without_a_letter_or_digit_are_not_strings() {
        let found = run(b"\x00....\x00\t\t\t\t\x00-==-\x00AAAA\x003333\x00a-b-\x00");
        assert_eq!(texts(&found), vec![("a-b-", false)]);
    }

    #[test]
    fn a_run_straddling_a_chunk_seam_is_one_string() {
        let mut data = vec![0u8; CHUNK - 3];
        data.extend_from_slice(b"seamless");
        data.push(0);
        let found = run(&data);
        assert_eq!(texts(&found), vec![("seamless", false)]);
        assert_eq!(found[0].offset, (CHUNK - 3) as u64);
    }

    #[test]
    fn a_huge_run_is_one_entry_cut_to_the_limit() {
        let data = b"xy".repeat(MAX_CHARS * 3);
        let found = run(&data);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].text.len(), MAX_CHARS);
    }

    #[test]
    fn only_the_given_range_is_scanned_and_offsets_stay_absolute() {
        let data = b"outside\x00inside!\x00outside";
        let read = |start: u64, n: usize| {
            let s = start as usize;
            data[s..(s + n).min(data.len())].to_vec()
        };
        let (found, capped) = scan(read, 8, 8, &[], &AtomicBool::new(false)).unwrap();
        assert!(!capped);
        assert_eq!(texts(&found), vec![("inside!", false)]);
        assert_eq!(found[0].offset, 8);
    }

    #[test]
    fn inside_code_only_long_ascii_sentences_count() {
        // Code from 8 to 96: a prologue's worth of printable bytes and a short
        // word are noise there, a sentence is not; outside, a short word is text.
        let mut data = b"abcd\x00\x00\x00\x00".to_vec();
        data.extend_from_slice(b"\x90UAWAVAUATSHAWAVAUATS\x90word\x90");
        data.extend_from_slice(b"\x90Montgomery Multiplication\x90");
        data.resize(96, 0x90);
        data.extend_from_slice(b"wxyz\x00");
        let found = scan_all(&data, &[(8, 96)]);
        assert_eq!(
            texts(&found),
            vec![("abcd", false), ("Montgomery Multiplication", false), ("wxyz", false)]
        );
    }

    #[test]
    fn a_raised_cancel_abandons_the_scan() {
        let read = |_: u64, n: usize| vec![b'a'; n];
        assert!(scan(read, 0, 64, &[], &AtomicBool::new(true)).is_none());
    }
}
