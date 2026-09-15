//! The CSV format, as files actually write it.
//!
//! RFC 4180 is the baseline — fields split on a delimiter, a field in double
//! quotes may hold the delimiter, line breaks and `""` for a quote — read
//! leniently, because a viewer that refuses a file is worse than one that shows
//! it slightly wrong: a quote is only special at the very start of a field, and
//! text after a closing quote simply carries on as part of the field. The
//! delimiter is guessed from the content (Europe writes semicolons, since its
//! decimal separator is the comma), with the extension deciding for `.tsv`.

use std::borrow::Cow;

/// How a particular file writes its table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dialect {
    pub delim: u8,
    /// Whether a double quote at the start of a field opens a quoted field.
    /// Turned off for a file whose quotes do not pair up, where honouring them
    /// would fold everything after a stray one into a single field.
    pub quoting: bool,
}

impl Default for Dialect {
    fn default() -> Self {
        Dialect { delim: b',', quoting: true }
    }
}

/// Delimiters a file is tried against, in the order a tie is settled by.
const DELIMITERS: [u8; 4] = *b",\t;|";

/// Records a guess looks at.
const SNIFF_RECORDS: usize = 64;

/// A quoted field this long that is still open at the end of the sample is
/// taken for a stray quote rather than for one enormous field.
const RUNAWAY_QUOTE: usize = 16 * 1024;

/// Guess the dialect of the file `name`, from `sample` — its first bytes, or
/// all of it when `complete`.
///
/// The delimiter is the one that splits the records most consistently: the
/// field count most records share, over the most records, with more than one
/// field. That is what tells `1,5;2,5` apart as two semicolon fields holding
/// decimal commas: the comma count wanders from row to row and the semicolon
/// count does not.
pub fn sniff(sample: &[u8], complete: bool, name: &str) -> Dialect {
    let lower = name.to_ascii_lowercase();
    let delim = if lower.ends_with(".tsv") || lower.ends_with(".tab") {
        b'\t'
    } else {
        let mut best = (b',', 0usize, 0usize);
        for delim in DELIMITERS {
            let (count, agree) = consistency(sample, complete, Dialect { delim, quoting: true });
            if count > 1 && agree > best.2 {
                best = (delim, count, agree);
            }
        }
        best.0
    };
    let quoting = quotes_pair_up(sample, complete, delim);
    Dialect { delim, quoting }
}

/// The field count most of the first records share, and how many share it.
fn consistency(sample: &[u8], complete: bool, dialect: Dialect) -> (usize, usize) {
    let mut starts = vec![0usize];
    let mut scanner = Scanner::new(dialect);
    scanner.feed(sample, 0, &mut starts);
    // A record cut off by the end of the sample would count short.
    let usable = if complete || starts.len() == 1 { starts.len() } else { starts.len() - 1 };
    let mut tally: Vec<(usize, usize)> = Vec::new();
    for i in 0..usable.min(SNIFF_RECORDS) {
        let end = starts.get(i + 1).copied().unwrap_or(sample.len());
        if starts[i] >= end {
            continue;
        }
        let n = split(&sample[starts[i]..end], dialect).len();
        match tally.iter_mut().find(|(count, _)| *count == n) {
            Some(t) => t.1 += 1,
            None => tally.push((n, 1)),
        }
    }
    tally.into_iter().max_by_key(|&(count, agree)| (agree, count)).unwrap_or((0, 0))
}

/// False when the sample ends inside a quoted field that is either the rest of
/// a complete file or longer than any real field would be: a quote that never
/// closes.
fn quotes_pair_up(sample: &[u8], complete: bool, delim: u8) -> bool {
    let mut scanner = Scanner::new(Dialect { delim, quoting: true });
    let mut opened = 0;
    let mut prev = scanner.state;
    for (i, &b) in sample.iter().enumerate() {
        scanner.step(b);
        if scanner.state == State::Quoted && prev == State::FieldStart {
            opened = i;
        }
        prev = scanner.state;
    }
    !(scanner.in_quotes() && (complete || sample.len() - opened > RUNAWAY_QUOTE))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    FieldStart,
    Unquoted,
    Quoted,
    /// A quote inside a quoted field: the closing one, or the first of `""`.
    QuoteInQuoted,
}

/// Finds where records start, a chunk of the file at a time.
///
/// The state carries from one [`feed`](Scanner::feed) to the next, so a file
/// can be indexed in pieces and a quoted field may straddle the seam.
#[derive(Debug, Clone, Copy)]
pub struct Scanner {
    state: State,
    dialect: Dialect,
}

impl Scanner {
    pub fn new(dialect: Dialect) -> Self {
        Scanner { state: State::FieldStart, dialect }
    }

    /// Scan `bytes`, which sit at offset `base` of the file, pushing the offset
    /// just past every line break that ends a record.
    pub fn feed(&mut self, bytes: &[u8], base: usize, starts: &mut Vec<usize>) {
        for (i, &b) in bytes.iter().enumerate() {
            if self.step(b) {
                starts.push(base + i + 1);
            }
        }
    }

    /// Whether the bytes so far end inside a quoted field.
    pub fn in_quotes(&self) -> bool {
        self.state == State::Quoted
    }

    /// Take one byte; true when it was the line break ending a record.
    #[inline]
    fn step(&mut self, b: u8) -> bool {
        let Dialect { delim, quoting } = self.dialect;
        match self.state {
            State::Quoted => {
                if b == b'"' {
                    self.state = State::QuoteInQuoted;
                }
                false
            }
            State::FieldStart | State::Unquoted | State::QuoteInQuoted => {
                if b == b'\n' {
                    self.state = State::FieldStart;
                    true
                } else if b == delim {
                    self.state = State::FieldStart;
                    false
                } else if b == b'"' && self.state == State::QuoteInQuoted {
                    // `""` inside quotes: an escaped quote, still quoted.
                    self.state = State::Quoted;
                    false
                } else if b == b'"' && self.state == State::FieldStart && quoting {
                    self.state = State::Quoted;
                    false
                } else {
                    self.state = State::Unquoted;
                    false
                }
            }
        }
    }
}

/// One field of a record: its bytes within the record, quotes included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Field {
    pub start: usize,
    pub end: usize,
    pub quoted: bool,
}

/// Split one record into its fields. The line break ending it is not part of
/// the last field; an empty record is one empty field.
pub fn split(rec: &[u8], dialect: Dialect) -> Vec<Field> {
    let body = strip_line_end(rec);
    let mut fields = Vec::new();
    let mut scanner = Scanner::new(dialect);
    let mut start = 0;
    for (i, &b) in body.iter().enumerate() {
        let was = scanner.state;
        scanner.step(b);
        if was != State::Quoted && b == dialect.delim {
            fields.push(field(body, start, i, dialect));
            start = i + 1;
        }
    }
    fields.push(field(body, start, body.len(), dialect));
    fields
}

fn field(body: &[u8], start: usize, end: usize, dialect: Dialect) -> Field {
    Field { start, end, quoted: dialect.quoting && body.get(start) == Some(&b'"') && start < end }
}

/// `rec` without the line break that ends it (`\n` or `\r\n`).
pub fn strip_line_end(rec: &[u8]) -> &[u8] {
    let rec = rec.strip_suffix(b"\n").unwrap_or(rec);
    rec.strip_suffix(b"\r").unwrap_or(rec)
}

/// The text a field holds: its quotes taken off and each `""` made one quote.
/// Anything after the closing quote is kept, as the lenient reading has it.
pub fn value(raw: &[u8], quoted: bool) -> Cow<'_, str> {
    if !quoted || raw.first() != Some(&b'"') {
        return String::from_utf8_lossy(raw);
    }
    let mut out = Vec::with_capacity(raw.len());
    let mut i = 1;
    let mut open = true;
    while i < raw.len() {
        let b = raw[i];
        if open && b == b'"' {
            if raw.get(i + 1) == Some(&b'"') {
                out.push(b'"');
                i += 2;
                continue;
            }
            open = false;
        } else {
            out.push(b);
        }
        i += 1;
    }
    Cow::Owned(String::from_utf8_lossy(&out).into_owned())
}

/// Whether a cell reads as a number — and so is right-aligned, and is no
/// column title: an optional sign, digits with at most one decimal point or
/// comma, an optional exponent and an optional trailing `%`.
pub fn looks_numeric(s: &str) -> bool {
    let s = s.trim();
    let s = s.strip_suffix('%').unwrap_or(s);
    let s = s.strip_prefix(['-', '+']).unwrap_or(s);
    let (mantissa, exponent) = match s.find(['e', 'E']) {
        Some(i) => (&s[..i], Some(&s[i + 1..])),
        None => (s, None),
    };
    let mut digits = 0;
    let mut points = 0;
    for c in mantissa.chars() {
        match c {
            '0'..='9' => digits += 1,
            '.' | ',' => points += 1,
            _ => return false,
        }
    }
    let exponent_ok = exponent.is_none_or(|e| {
        let e = e.strip_prefix(['-', '+']).unwrap_or(e);
        !e.is_empty() && e.bytes().all(|b| b.is_ascii_digit())
    });
    digits > 0 && points <= 1 && exponent_ok
}

/// Whether a table's first record looks like column titles: every cell filled,
/// none a number, and no title twice.
pub fn guess_header(first: &[String]) -> bool {
    if first.is_empty() {
        return false;
    }
    let titles = first.iter().all(|c| !c.trim().is_empty() && !looks_numeric(c));
    let distinct = first.iter().enumerate().all(|(i, c)| !first[..i].contains(c));
    titles && distinct
}

/// The spreadsheet name of column `i`: A … Z, AA … ZZ, AAA …
pub fn column_name(mut i: usize) -> String {
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (i % 26) as u8);
        if i < 26 {
            break;
        }
        i = i / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(rec: &str, d: Dialect) -> Vec<String> {
        split(rec.as_bytes(), d)
            .iter()
            .map(|f| value(&rec.as_bytes()[f.start..f.end], f.quoted).into_owned())
            .collect()
    }

    fn starts(data: &[u8], d: Dialect) -> Vec<usize> {
        let mut out = vec![0];
        Scanner::new(d).feed(data, 0, &mut out);
        out
    }

    #[test]
    fn the_delimiter_is_the_one_that_splits_records_consistently() {
        let comma = b"name,age,city\nann,31,oslo\nbob,42,rome\n";
        assert_eq!(sniff(comma, true, "x.csv").delim, b',');
        let semi = b"name;price\nfoo;1,5\nbar;22,75\nbaz;3\n";
        assert_eq!(sniff(semi, true, "x.csv").delim, b';', "decimal commas are not delimiters");
        let tab = b"a\tb\tc\n1\t2\t3\n";
        assert_eq!(sniff(tab, true, "x.csv").delim, b'\t');
        let pipe = b"a|b\n1|2\n3|4\n";
        assert_eq!(sniff(pipe, true, "x.csv").delim, b'|');
        // The extension decides for tab-separated files, whatever the content.
        assert_eq!(sniff(comma, true, "x.tsv").delim, b'\t');
        // A single column has no delimiter to find; comma is the default.
        assert_eq!(sniff(b"one\ntwo\n", true, "x.csv"), Dialect::default());
    }

    #[test]
    fn a_quote_that_never_closes_turns_quoting_off() {
        assert!(!sniff(b"a,\"b\nc,d\n", true, "x.csv").quoting);
        assert!(sniff(b"a,\"b\nc\",d\n", true, "x.csv").quoting, "a closed quote is fine");
        // A sample cut in the middle of a long-enough quoted field is not proof.
        assert!(sniff(b"a,\"bbb", false, "x.csv").quoting);
    }

    #[test]
    fn quoted_fields_hold_delimiters_quotes_and_line_breaks() {
        let d = Dialect::default();
        assert_eq!(values("a,\"b,c\",d", d), ["a", "b,c", "d"]);
        assert_eq!(values("\"say \"\"hi\"\"\",x", d), ["say \"hi\"", "x"]);
        assert_eq!(values("\"two\r\nlines\",y\r\n", d), ["two\r\nlines", "y"]);
        assert_eq!(values("", d), [""]);
        assert_eq!(values("a,,", d), ["a", "", ""]);
        let data = b"h1,h2\n\"multi\nline\",2\nlast,3";
        assert_eq!(starts(data, d), [0, 6, 21]);
    }

    #[test]
    fn stray_quotes_are_read_leniently() {
        let d = Dialect::default();
        // A quote in the middle of a field is an ordinary character.
        assert_eq!(values("5\" disk,x", d), ["5\" disk", "x"]);
        // Text after a closing quote carries on as part of the field.
        assert_eq!(values("\"a\"b,c", d), ["ab", "c"]);
        // With quoting off, quotes are just characters.
        let off = Dialect { quoting: false, ..d };
        assert_eq!(values("\"a,b\"", off), ["\"a", "b\""]);
        assert_eq!(starts(b"\"a\nb\n", off), [0, 3, 5]);
    }

    #[test]
    fn a_file_indexed_in_chunks_matches_one_indexed_whole() {
        let data = b"id,text\n1,\"with\nbreak\"\n2,\"a,\"\"b\"\"\"\n3,plain\n".repeat(3);
        let d = Dialect::default();
        let whole = starts(&data, d);
        for cut in 0..data.len() {
            let mut out = vec![0];
            let mut s = Scanner::new(d);
            s.feed(&data[..cut], 0, &mut out);
            s.feed(&data[cut..], cut, &mut out);
            assert_eq!(out, whole, "split at {cut}");
        }
    }

    #[test]
    fn numbers_and_titles_are_told_apart() {
        for n in ["42", "-3.5", "+1,25", "1e10", "2.5E-3", "12%", " 7 "] {
            assert!(looks_numeric(n), "{n}");
        }
        for s in ["", "abc", "1.2.3", "12a", "e5", "-", "1e", "1,000,000"] {
            assert!(!looks_numeric(s), "{s}");
        }
        let row = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(guess_header(&row(&["name", "age"])));
        assert!(!guess_header(&row(&["ann", "31"])), "a number is data");
        assert!(!guess_header(&row(&["a", ""])), "a title is never empty");
        assert!(!guess_header(&row(&["x", "x"])), "titles are distinct");
    }

    #[test]
    fn columns_are_named_like_a_spreadsheet_names_them() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }
}
