//! The data inspector: the bytes at the hex cursor read as each common type —
//! integers, floats, LEB128, a UTF-8 or UTF-16 character, dates, a GUID — in
//! either byte order; and a value typed for one of them turned back into the
//! bytes it overwrites, never more or fewer than it covers.

use super::ast::Prim;
use super::interp::edit::{guid_digits, parse_int, parse_scalar};
use super::interp::{encode_scalar, guid_text, scalar_from_raw, time};
use super::value::Value;

/// The most bytes a row reads (a GUID's).
pub const MAX_WIDTH: usize = 16;

/// The longest LEB128 value read: ten groups hold 64 bits.
const LEB_MAX: usize = 10;

/// The order a GUID's bytes are stored in, as indexes into the digits as
/// written: the first three groups little-endian.
const GUID_LE: [usize; 16] = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];

/// What the bytes at the cursor are read as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Binary,
    Int8,
    UInt8,
    Int16,
    UInt16,
    Int32,
    UInt32,
    Int64,
    UInt64,
    Float16,
    Float32,
    Float64,
    Uleb128,
    Sleb128,
    Utf8,
    Utf16,
    TimeT,
    Time64T,
    FileTime,
    OleTime,
    DosDate,
    DosTime,
    Guid,
}

impl Field {
    pub const ALL: [Field; 23] = [
        Field::Binary,
        Field::Int8,
        Field::UInt8,
        Field::Int16,
        Field::UInt16,
        Field::Int32,
        Field::UInt32,
        Field::Int64,
        Field::UInt64,
        Field::Float16,
        Field::Float32,
        Field::Float64,
        Field::Uleb128,
        Field::Sleb128,
        Field::Utf8,
        Field::Utf16,
        Field::TimeT,
        Field::Time64T,
        Field::FileTime,
        Field::OleTime,
        Field::DosDate,
        Field::DosTime,
        Field::Guid,
    ];

    /// The row's label: a type name, the same in every language.
    pub fn label(self) -> &'static str {
        match self {
            Field::Binary => "binary",
            Field::Int8 => "int8",
            Field::UInt8 => "uint8",
            Field::Int16 => "int16",
            Field::UInt16 => "uint16",
            Field::Int32 => "int32",
            Field::UInt32 => "uint32",
            Field::Int64 => "int64",
            Field::UInt64 => "uint64",
            Field::Float16 => "float16",
            Field::Float32 => "float32",
            Field::Float64 => "float64",
            Field::Uleb128 => "ULEB128",
            Field::Sleb128 => "SLEB128",
            Field::Utf8 => "UTF-8",
            Field::Utf16 => "UTF-16",
            Field::TimeT => "time_t",
            Field::Time64T => "time64_t",
            Field::FileTime => "FILETIME",
            Field::OleTime => "OLETIME",
            Field::DosDate => "DOSDATE",
            Field::DosTime => "DOSTIME",
            Field::Guid => "GUID",
        }
    }

    /// The template type that reads the same bytes, when there is one.
    fn prim(self) -> Option<Prim> {
        Some(match self {
            Field::Int8 => Prim::Char,
            Field::UInt8 => Prim::UChar,
            Field::Int16 => Prim::Short,
            Field::UInt16 => Prim::UShort,
            Field::Int32 => Prim::Int,
            Field::UInt32 => Prim::UInt,
            Field::Int64 => Prim::Int64,
            Field::UInt64 => Prim::UInt64,
            Field::Float16 => Prim::HFloat,
            Field::Float32 => Prim::Float,
            Field::Float64 => Prim::Double,
            Field::TimeT => Prim::TimeT,
            Field::Time64T => Prim::Time64T,
            Field::FileTime => Prim::FileTime,
            Field::OleTime => Prim::OleTime,
            Field::DosDate => Prim::DosDate,
            Field::DosTime => Prim::DosTime,
            Field::Binary
            | Field::Uleb128
            | Field::Sleb128
            | Field::Utf8
            | Field::Utf16
            | Field::Guid => return None,
        })
    }
}

/// What `field` shows for `bytes` (those from the cursor on, fewer near the
/// end of the file), and how many of them it covers. `None` when there aren't
/// enough bytes left to read one.
pub fn decode(field: Field, bytes: &[u8], big: bool) -> Option<(String, usize)> {
    if let Some(prim) = field.prim() {
        let size = prim.size() as usize;
        let raw = read_uint(bytes.get(..size)?, big);
        return Some((scalar_text(prim, raw), size));
    }
    match field {
        Field::Binary => bytes.first().map(|b| (format!("{b:08b}"), 1)),
        Field::Uleb128 | Field::Sleb128 => {
            let (v, n) = read_leb(bytes, field == Field::Sleb128)?;
            Some((v.to_string(), n))
        }
        Field::Utf8 => {
            bytes.first()?;
            Some(match utf8_char(bytes) {
                Some(c) => (char_text(c), c.len_utf8()),
                None => ("invalid".to_string(), 1),
            })
        }
        Field::Utf16 => {
            let (c, n) = utf16_char(bytes, big)?;
            Some((c.map_or_else(|| "invalid".to_string(), char_text), n))
        }
        Field::Guid => {
            let b = bytes.get(..16)?;
            Some((if big { guid_text_in_order(b) } else { guid_text(b) }, 16))
        }
        _ => None,
    }
}

/// The bytes `text`, typed as a `field` value, overwrites at the cursor, where
/// `bytes` are now. A value that would change how many bytes the one there
/// takes (a character, a LEB128 number) is refused.
pub fn encode(field: Field, text: &str, big: bool, bytes: &[u8]) -> Result<Vec<u8>, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("enter a value".into());
    }
    let room = |n: usize| {
        if bytes.len() >= n {
            Ok(())
        } else {
            Err("not enough bytes before the end of the file".to_string())
        }
    };
    if let Some(prim) = field.prim() {
        room(prim.size() as usize)?;
        let v = parse_scalar(prim, t)?;
        return Ok(encode_scalar(prim, &v, big));
    }
    match field {
        Field::Binary => {
            room(1)?;
            let digits: String = t.chars().filter(|c| !matches!(c, ' ' | '_')).collect();
            let n = if (1..=8).contains(&digits.len())
                && digits.bytes().all(|c| c == b'0' || c == b'1')
            {
                i128::from_str_radix(&digits, 2).ok()
            } else {
                parse_int(t)
            };
            match n {
                Some(n) if (-128..=255).contains(&n) => Ok(vec![n as u8]),
                _ => Err(format!("'{t}' is not a byte")),
            }
        }
        Field::Uleb128 | Field::Sleb128 => {
            let signed = field == Field::Sleb128;
            let n = parse_int(t).ok_or_else(|| format!("'{t}' is not a number"))?;
            let fits = if signed {
                (i64::MIN as i128..=i64::MAX as i128).contains(&n)
            } else {
                (0..=u64::MAX as i128).contains(&n)
            };
            if !fits {
                return Err(format!("{t} is out of range"));
            }
            let need = leb_len(n, signed);
            let len = read_leb(bytes, signed).map_or(need, |(_, l)| l);
            if need > len {
                return Err(format!("{t} needs {need} bytes; the value here has {len}"));
            }
            room(len)?;
            // Padded out to the length already there with continuation groups.
            Ok((0..len)
                .map(|i| {
                    let group = ((n >> (7 * i)) & 0x7f) as u8;
                    if i + 1 < len { group | 0x80 } else { group }
                })
                .collect())
        }
        Field::Utf8 => {
            let c = parse_char(t)?;
            let here = utf8_char(bytes).map_or(1, char::len_utf8);
            let mut buf = [0u8; 4];
            let enc = c.encode_utf8(&mut buf).as_bytes();
            if enc.len() != here {
                return Err(format!(
                    "{} takes {} bytes in UTF-8; the character here takes {here}",
                    char_text(c),
                    enc.len()
                ));
            }
            room(here)?;
            Ok(enc.to_vec())
        }
        Field::Utf16 => {
            let c = parse_char(t)?;
            let here = utf16_char(bytes, big).map_or(2, |(_, n)| n);
            let mut buf = [0u16; 2];
            let units = c.encode_utf16(&mut buf);
            if units.len() * 2 != here {
                return Err(format!(
                    "{} takes {} bytes in UTF-16; the character here takes {here}",
                    char_text(c),
                    units.len() * 2
                ));
            }
            room(here)?;
            Ok(units
                .iter()
                .flat_map(|u| if big { u.to_be_bytes() } else { u.to_le_bytes() })
                .collect())
        }
        Field::Guid => {
            room(16)?;
            let digits = guid_digits(t)?;
            Ok(if big { digits.to_vec() } else { GUID_LE.iter().map(|&k| digits[k]).collect() })
        }
        _ => Err("this value can't be edited".into()),
    }
}

/// `bytes` as one unsigned integer.
fn read_uint(bytes: &[u8], big: bool) -> u64 {
    let fold = |acc: u64, &b: &u8| (acc << 8) | b as u64;
    if big { bytes.iter().fold(0, fold) } else { bytes.iter().rev().fold(0, fold) }
}

/// A scalar of type `prim` read as the unsigned integer `raw`, as the
/// template panel shows it (dates in UTC).
fn scalar_text(prim: Prim, raw: u64) -> String {
    const DATE: &str = "MM/dd/yyyy hh:mm:ss";
    match (prim, scalar_from_raw(prim, raw)) {
        (Prim::OleTime, Value::Float(d, _)) => time::format(&time::from_oletime(d), DATE),
        (Prim::HFloat | Prim::Float, Value::Float(x, _)) => float_text(x as f32),
        (_, Value::Float(x, _)) => float_text(x),
        (Prim::DosDate, _) => time::format(&time::from_dosdate(raw as u16), "MM/dd/yyyy"),
        (Prim::DosTime, _) => time::format(&time::from_dostime(raw as u16), "hh:mm:ss"),
        (Prim::FileTime, _) => time::format(&time::from_filetime(raw), DATE),
        (Prim::TimeT | Prim::Time64T, Value::Int(v, _)) => {
            time::format(&time::from_unix(v as i64, 0), DATE)
        }
        (_, Value::Int(v, t)) => {
            if t.signed {
                (v as i64).to_string()
            } else {
                v.to_string()
            }
        }
        _ => String::new(),
    }
}

/// A float as short as it reads back exactly, in exponent form when that's
/// still long.
fn float_text<T: std::fmt::Display + std::fmt::LowerExp>(x: T) -> String {
    let s = x.to_string();
    if s.len() > 16 { format!("{x:e}") } else { s }
}

/// The LEB128 number at the start of `bytes`, and how many bytes it takes.
fn read_leb(bytes: &[u8], signed: bool) -> Option<(i128, usize)> {
    let mut v: i128 = 0;
    for (i, &b) in bytes.iter().take(LEB_MAX).enumerate() {
        v |= ((b & 0x7f) as i128) << (7 * i);
        if b & 0x80 == 0 {
            let bits = 7 * (i + 1);
            if signed && b & 0x40 != 0 {
                v -= 1i128 << bits;
            }
            return Some((v, i + 1));
        }
    }
    None
}

/// How many bytes the shortest LEB128 form of `n` takes.
fn leb_len(n: i128, signed: bool) -> usize {
    let mut v = n;
    let mut len = 0;
    loop {
        let group = v & 0x7f;
        v >>= 7;
        len += 1;
        let done = if signed {
            (v == 0 && group & 0x40 == 0) || (v == -1 && group & 0x40 != 0)
        } else {
            v == 0
        };
        if done {
            return len;
        }
    }
}

/// The UTF-8 character at the start of `bytes`, if they begin with one.
fn utf8_char(bytes: &[u8]) -> Option<char> {
    let n = match *bytes.first()? {
        0x00..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => return None,
    };
    std::str::from_utf8(bytes.get(..n)?).ok()?.chars().next()
}

/// The UTF-16 character at the start of `bytes` and how many bytes it takes;
/// the character is `None` for a surrogate without its partner.
fn utf16_char(bytes: &[u8], big: bool) -> Option<(Option<char>, usize)> {
    let unit = |i: usize| bytes.get(i..i + 2).map(|b| read_uint(b, big) as u16);
    let first = unit(0)?;
    if (0xd800..0xdc00).contains(&first) {
        return Some(match unit(2) {
            Some(second) if (0xdc00..0xe000).contains(&second) => {
                (char::decode_utf16([first, second]).next().and_then(Result::ok), 4)
            }
            _ => (None, 2),
        });
    }
    Some((char::from_u32(first as u32), 2))
}

/// A character as the inspector shows it: `'é' U+00E9`, or only its code
/// when it has no glyph.
fn char_text(c: char) -> String {
    if c.is_control() { format!("U+{:04X}", c as u32) } else { format!("'{c}' U+{:04X}", c as u32) }
}

/// A character typed as itself, in quotes as shown, or as `U+` and its code.
fn parse_char(t: &str) -> Result<char, String> {
    if let Some(rest) = t.strip_prefix('\'') {
        let mut it = rest.chars();
        if let (Some(c), Some('\'')) = (it.next(), it.next()) {
            return Ok(c);
        }
    }
    let code = t
        .strip_prefix("U+")
        .or_else(|| t.strip_prefix("u+"))
        .or_else(|| t.strip_prefix("0x"))
        .or_else(|| t.strip_prefix("0X"));
    if let Some(hex) = code {
        return u32::from_str_radix(hex.trim(), 16)
            .ok()
            .and_then(char::from_u32)
            .ok_or_else(|| format!("'{t}' is not a character"));
    }
    let mut chars = t.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(c),
        _ => Err("type one character, or U+ and its code".into()),
    }
}

/// A GUID's 16 bytes shown in the order they are stored.
fn guid_text_in_order(b: &[u8]) -> String {
    let hex =
        |r: std::ops::Range<usize>| -> String { b[r].iter().map(|x| format!("{x:02X}")).collect() };
    format!("{{{}-{}-{}-{}-{}}}", hex(0..4), hex(4..6), hex(6..8), hex(8..10), hex(10..16))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn show(field: Field, bytes: &[u8], big: bool) -> Option<(String, usize)> {
        decode(field, bytes, big)
    }

    #[test]
    fn integers_and_floats_read_in_either_byte_order() {
        let b = [0xfe, 0xff, 0xff, 0xff, 0x01, 0x02, 0x03, 0x04];
        assert_eq!(show(Field::Binary, &b, false), Some(("11111110".into(), 1)));
        assert_eq!(show(Field::Int8, &b, false), Some(("-2".into(), 1)));
        assert_eq!(show(Field::UInt8, &b, false), Some(("254".into(), 1)));
        assert_eq!(show(Field::Int16, &b, false), Some(("-2".into(), 2)));
        assert_eq!(show(Field::UInt16, &b, true), Some(("65279".into(), 2)));
        assert_eq!(show(Field::Int32, &b, false), Some(("-2".into(), 4)));
        assert_eq!(show(Field::UInt32, &b, true), Some(("4278190079".into(), 4)));
        assert_eq!(show(Field::UInt64, &b, false), Some(("289077008695033854".into(), 8)));
        assert_eq!(show(Field::Int64, &b[..7], false), None, "past the end");
        let f = 1.1f32.to_be_bytes();
        assert_eq!(show(Field::Float32, &f, true), Some(("1.1".into(), 4)));
        let d = (-0.25f64).to_le_bytes();
        assert_eq!(show(Field::Float64, &d, false), Some(("-0.25".into(), 8)));
        // Half precision: 0x3c00 is 1.0.
        assert_eq!(show(Field::Float16, &[0x00, 0x3c], false), Some(("1".into(), 2)));
    }

    #[test]
    fn dates_read_as_the_template_panel_shows_them() {
        let t = 1_700_000_000u32.to_le_bytes();
        assert_eq!(show(Field::TimeT, &t, false).unwrap().0, "11/14/2023 22:13:20");
        let ft = 125_911_584_000_000_000u64.to_le_bytes();
        assert_eq!(show(Field::FileTime, &ft, false).unwrap().0, "01/01/2000 00:00:00");
        let dos = (((44u16 << 9) | (7 << 5) | 21).to_le_bytes(), false);
        assert_eq!(show(Field::DosDate, &dos.0, dos.1).unwrap().0, "07/21/2024");
        // Typed back, a date writes the bytes it was read from.
        assert_eq!(encode(Field::TimeT, "11/14/2023 22:13:20", false, &[0; 8]).unwrap(), t);
        assert_eq!(encode(Field::DosDate, "07/21/2024", false, &[0; 2]).unwrap(), dos.0);
    }

    #[test]
    fn typed_numbers_become_bytes_in_the_chosen_order() {
        assert_eq!(encode(Field::UInt16, "0x1234", true, &[0; 4]).unwrap(), vec![0x12, 0x34]);
        assert_eq!(encode(Field::UInt16, "0x1234", false, &[0; 4]).unwrap(), vec![0x34, 0x12]);
        assert_eq!(encode(Field::Int8, "-1", false, &[0]).unwrap(), vec![0xff]);
        assert!(encode(Field::UInt8, "300", false, &[0]).is_err(), "doesn't fit");
        assert!(encode(Field::UInt32, "1", false, &[0; 3]).is_err(), "past the end");
        assert_eq!(encode(Field::Float32, "1.5", false, &[0; 4]).unwrap(), 1.5f32.to_le_bytes());
        assert_eq!(encode(Field::Binary, "0100 0001", false, &[0]).unwrap(), vec![0x41]);
        assert_eq!(encode(Field::Binary, "0x7f", false, &[0]).unwrap(), vec![0x7f]);
        assert!(encode(Field::Binary, "", false, &[0]).is_err());
    }

    #[test]
    fn leb128_reads_and_keeps_its_length_when_written() {
        // 624485 is E5 8E 26; -123456 is C0 BB 78.
        assert_eq!(
            show(Field::Uleb128, &[0xe5, 0x8e, 0x26, 0xff], false),
            Some(("624485".into(), 3))
        );
        assert_eq!(show(Field::Sleb128, &[0xc0, 0xbb, 0x78], false), Some(("-123456".into(), 3)));
        assert_eq!(show(Field::Uleb128, &[0x80, 0x80], false), None, "no end in sight");
        assert_eq!(show(Field::Sleb128, &[0x7f], false), Some(("-1".into(), 1)));
        // A smaller value is padded out to the three bytes already there…
        let here = [0xe5, 0x8e, 0x26];
        let out = encode(Field::Uleb128, "2", false, &here).unwrap();
        assert_eq!(out, vec![0x82, 0x80, 0x00]);
        assert_eq!(read_leb(&out, false), Some((2, 3)));
        let out = encode(Field::Sleb128, "-2", false, &here).unwrap();
        assert_eq!(out, vec![0xfe, 0xff, 0x7f]);
        assert_eq!(read_leb(&out, true), Some((-2, 3)));
        // …and one that needs more than there are is refused.
        assert!(encode(Field::Uleb128, "300", false, &[0x05]).is_err());
        assert!(encode(Field::Uleb128, "-1", false, &here).is_err());
        assert_eq!(leb_len(u64::MAX as i128, false), 10);
        assert_eq!(leb_len(-64, true), 1);
        assert_eq!(leb_len(64, true), 2);
    }

    #[test]
    fn characters_read_and_must_keep_their_width() {
        let e = "é".as_bytes();
        assert_eq!(show(Field::Utf8, e, false), Some(("'é' U+00E9".into(), 2)));
        assert_eq!(show(Field::Utf8, &[0xff], false), Some(("invalid".into(), 1)));
        assert_eq!(show(Field::Utf8, &[0x0a], false), Some(("U+000A".into(), 1)));
        assert_eq!(encode(Field::Utf8, "'ü' U+00FC", false, e).unwrap(), "ü".as_bytes());
        assert_eq!(encode(Field::Utf8, "U+00FC", false, e).unwrap(), "ü".as_bytes());
        assert!(encode(Field::Utf8, "a", false, e).is_err(), "one byte for two");
        // UTF-16: a surrogate pair is one character of four bytes.
        let mut pair = Vec::new();
        for u in "😀".encode_utf16() {
            pair.extend(u.to_be_bytes());
        }
        assert_eq!(show(Field::Utf16, &pair, true), Some(("'😀' U+1F600".into(), 4)));
        // A high surrogate followed by anything but a low one is no character.
        assert_eq!(
            show(Field::Utf16, &[0x3d, 0xd8, 0x41, 0x00], false),
            Some(("invalid".into(), 2))
        );
        assert_eq!(show(Field::Utf16, &[0x41, 0x00], false), Some(("'A' U+0041".into(), 2)));
        let out = encode(Field::Utf16, "😁", true, &pair).unwrap();
        assert_eq!(show(Field::Utf16, &out, true).unwrap().0, "'😁' U+1F601");
        assert!(encode(Field::Utf16, "B", true, &pair).is_err());
    }

    #[test]
    fn guids_round_trip_in_both_layouts() {
        let bytes: Vec<u8> = (0..16).collect();
        let le = show(Field::Guid, &bytes, false).unwrap().0;
        assert_eq!(le, "{03020100-0504-0706-0809-0A0B0C0D0E0F}");
        assert_eq!(encode(Field::Guid, &le, false, &bytes).unwrap(), bytes);
        let be = show(Field::Guid, &bytes, true).unwrap().0;
        assert_eq!(be, "{00010203-0405-0607-0809-0A0B0C0D0E0F}");
        assert_eq!(encode(Field::Guid, &be, true, &bytes).unwrap(), bytes);
        assert!(encode(Field::Guid, "{1234}", false, &bytes).is_err());
    }
}
