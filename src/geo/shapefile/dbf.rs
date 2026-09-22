//! The `.dbf` beside a shapefile: dBASE III/5, which is where a shapefile keeps
//! its attributes — one record per shape, in the same order.
//!
//! Only what a shapefile actually uses is implemented: a fixed-width, typed
//! schema and plain records. Memo fields (`M`, in a separate `.dbt`) are read
//! as their raw text and written back unchanged, since nothing here edits them.
//!
//! The schema is the reason this is kept rather than rebuilt from the GeoJSON:
//! field names, types and widths are what the file *is*, and a rewrite that
//! guessed them would change the dataset even where the user changed nothing.

use std::collections::BTreeSet;

/// Longest a `C` field may be, from the format.
const MAX_CHAR_LEN: usize = 254;
/// Field names are 11 bytes, null-terminated.
const NAME_LEN: usize = 11;

/// One column: what it is called, what it holds, and how wide it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    /// `C` text, `N`/`F` numeric, `D` date (`YYYYMMDD`), `L` logical.
    pub kind: u8,
    pub len: u8,
    pub decimals: u8,
}

impl Field {
    /// A field wide enough to hold `values`, with its type inferred from them —
    /// for a property the file's schema has no column for.
    ///
    /// Numbers stay numbers so the result is still usable as data; anything
    /// else becomes text sized to the longest value present.
    pub fn inferred(name: &str, values: &[String]) -> Field {
        let non_empty: Vec<&String> = values.iter().filter(|v| !v.trim().is_empty()).collect();
        let all = |f: fn(&str) -> bool| !non_empty.is_empty() && non_empty.iter().all(|v| f(v));
        let width = values.iter().map(|v| v.chars().count()).max().unwrap_or(1).max(1);

        if all(|v| matches!(v.trim(), "T" | "F" | "true" | "false" | "Y" | "N")) {
            return Field { name: clean_name(name), kind: b'L', len: 1, decimals: 0 };
        }
        if all(|v| v.trim().parse::<i64>().is_ok()) {
            let len = width.clamp(1, 18) as u8;
            return Field { name: clean_name(name), kind: b'N', len, decimals: 0 };
        }
        if all(|v| v.trim().parse::<f64>().is_ok()) {
            // Keep the decimals actually present, so a rewrite does not round.
            let decimals = non_empty
                .iter()
                .map(|v| v.trim().rsplit_once('.').map(|(_, d)| d.len()).unwrap_or(0))
                .max()
                .unwrap_or(0)
                .min(15);
            let len = width.clamp(decimals + 2, 19) as u8;
            return Field { name: clean_name(name), kind: b'F', len, decimals: decimals as u8 };
        }
        Field {
            name: clean_name(name),
            kind: b'C',
            len: width.clamp(1, MAX_CHAR_LEN) as u8,
            decimals: 0,
        }
    }

    /// How this field's value is written into a record: left-aligned and space
    /// padded for text, right-aligned for a number, as dBASE has it.
    fn encode(&self, value: &str) -> Vec<u8> {
        let w = self.len as usize;
        let v = value.trim();
        let mut out: Vec<u8> = match self.kind {
            b'L' => {
                let c = match v {
                    "T" | "t" | "Y" | "y" | "true" | "True" => b'T',
                    "F" | "f" | "N" | "n" | "false" | "False" => b'F',
                    _ => b'?',
                };
                vec![c]
            }
            b'N' | b'F' => {
                // A value that does not fit is written as blanks rather than as
                // a truncated number, which would be a different number.
                let text = if v.len() <= w { format!("{v:>w$}") } else { " ".repeat(w) };
                text.into_bytes()
            }
            // Text (and anything unrecognised) is latin-1-ish: non-ASCII is
            // written as the bytes it decodes to, and cut to the field width.
            _ => {
                let mut b: Vec<u8> = v.bytes().take(w).collect();
                b.resize(w, b' ');
                b
            }
        };
        out.resize(w, b' ');
        out
    }
}

/// A field name as dBASE will take it: upper-case ASCII, at most 10 characters.
fn clean_name(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(NAME_LEN - 1)
        .collect::<String>()
        .to_ascii_uppercase();
    if s.is_empty() { "FIELD".to_string() } else { s }
}

/// Make every name in `fields` distinct, since truncation to 10 characters can
/// collide. The first keeps its name; later ones get a numeric suffix.
pub fn deduplicate(fields: &mut [Field]) {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    for f in fields.iter_mut() {
        if seen.insert(f.name.clone()) {
            continue;
        }
        for n in 2..1000 {
            let suffix = n.to_string();
            let keep = (NAME_LEN - 1).saturating_sub(suffix.len());
            let candidate: String =
                format!("{}{}", f.name.chars().take(keep).collect::<String>(), suffix);
            if seen.insert(candidate.clone()) {
                f.name = candidate;
                break;
            }
        }
    }
}

/// A whole attribute table: the schema, and one row of already-decoded strings
/// per shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Table {
    pub fields: Vec<Field>,
    pub rows: Vec<Vec<String>>,
}

/// Read a `.dbf`. `None` when it is not one, or is truncated.
pub fn read(bytes: &[u8]) -> Option<Table> {
    if bytes.len() < 32 {
        return None;
    }
    let u16le = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]) as usize;
    let u32le = |at: usize| {
        u32::from_le_bytes(bytes[at..at + 4].try_into().ok().unwrap_or([0; 4])) as usize
    };
    let count = u32le(4);
    let header_len = u16le(8);
    let record_len = u16le(10);
    if header_len < 33 || record_len == 0 || header_len > bytes.len() {
        return None;
    }

    // Field descriptors run from byte 32 up to the 0x0D terminator.
    let mut fields: Vec<Field> = Vec::new();
    let mut at = 32;
    while at + 32 <= header_len {
        if bytes[at] == 0x0D {
            break;
        }
        let raw = &bytes[at..at + NAME_LEN];
        let end = raw.iter().position(|b| *b == 0).unwrap_or(NAME_LEN);
        let name = String::from_utf8_lossy(&raw[..end]).trim().to_string();
        fields.push(Field {
            name,
            kind: bytes[at + 11],
            len: bytes[at + 16],
            decimals: bytes[at + 17],
        });
        at += 32;
    }
    if fields.is_empty() {
        return None;
    }
    // A record is the deletion flag plus every field; a header that disagrees
    // is not one this can read safely.
    let expect: usize = 1 + fields.iter().map(|f| f.len as usize).sum::<usize>();
    if expect != record_len {
        return None;
    }

    let mut rows = Vec::with_capacity(count);
    for r in 0..count {
        let start = header_len + r * record_len;
        let Some(rec) = bytes.get(start..start + record_len) else {
            break; // truncated: keep what was read
        };
        // 0x2A marks a record deleted in place; it is not part of the data.
        if rec[0] == b'*' {
            continue;
        }
        let mut at = 1;
        let mut row = Vec::with_capacity(fields.len());
        for f in &fields {
            let w = f.len as usize;
            let raw = rec.get(at..at + w).unwrap_or(&[]);
            row.push(String::from_utf8_lossy(raw).trim().to_string());
            at += w;
        }
        rows.push(row);
    }
    Some(Table { fields, rows })
}

/// Write a `.dbf` holding `table`.
pub fn write(table: &Table) -> Vec<u8> {
    let header_len = 32 + table.fields.len() * 32 + 1;
    let record_len: usize = 1 + table.fields.iter().map(|f| f.len as usize).sum::<usize>();
    let mut out = Vec::with_capacity(header_len + table.rows.len() * record_len + 1);

    out.push(0x03); // dBASE III, no memo
    // The date the file was written. Deliberately not "now": a rewrite that
    // changed nothing should produce the same bytes, so this stays fixed.
    out.extend_from_slice(&[0x00, 0x01, 0x01]);
    out.extend_from_slice(&(table.rows.len() as u32).to_le_bytes());
    out.extend_from_slice(&(header_len as u16).to_le_bytes());
    out.extend_from_slice(&(record_len as u16).to_le_bytes());
    out.extend_from_slice(&[0u8; 20]);

    for f in &table.fields {
        let mut name = [0u8; NAME_LEN];
        for (i, b) in f.name.bytes().take(NAME_LEN - 1).enumerate() {
            name[i] = b;
        }
        out.extend_from_slice(&name);
        out.push(f.kind);
        out.extend_from_slice(&[0u8; 4]);
        out.push(f.len);
        out.push(f.decimals);
        out.extend_from_slice(&[0u8; 14]);
    }
    out.push(0x0D);

    for row in &table.rows {
        out.push(b' '); // not deleted
        for (i, f) in table.fields.iter().enumerate() {
            let v = row.get(i).map(String::as_str).unwrap_or("");
            out.extend_from_slice(&f.encode(v));
        }
    }
    out.push(0x1A); // end of file
    out
}
