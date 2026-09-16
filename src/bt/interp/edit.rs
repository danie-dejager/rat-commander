//! Editing a value in the template panel: the text typed is turned into the
//! bytes it stands for — through the variable's `write=` callback when it has
//! one, else by its type — without touching the file; the editor lays the
//! bytes over its unsaved edits.

use super::{Interp, Place, Stop, time};
use crate::bt::ast::Prim;
use crate::bt::tree::{ArrayKind, F_ENUM, F_READONLY, NodeKind, NodeRef};
use crate::bt::value::{IntTy, Value};

/// What an edit targets: a variable, or one element of a scalar array.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditTarget {
    Node(NodeRef),
    Elem(NodeRef, u64),
}

impl Interp {
    /// Whether the value of `t` can be edited.
    pub fn editable(&self, t: EditTarget) -> bool {
        match t {
            EditTarget::Elem(r, _) => {
                matches!(self.tree.node(r.id).kind, NodeKind::Array { kind: ArrayKind::Scalar, .. })
            }
            EditTarget::Node(r) => {
                let n = self.tree.node(r.id);
                if self.tree.lazy_of(r.id).write.is_some() {
                    return true;
                }
                if n.flags & F_READONLY != 0 {
                    return false;
                }
                match &n.kind {
                    NodeKind::Scalar { .. } | NodeKind::Str { .. } => true,
                    NodeKind::Array {
                        elem_prim: Some(Prim::Char | Prim::UChar | Prim::WChar),
                        ..
                    } => true,
                    NodeKind::Array { elem_ty, .. } => {
                        super::builtins::is_guid(self, n.ty)
                            || super::builtins::is_guid(self, *elem_ty)
                    }
                    NodeKind::Struct { .. } => false,
                }
            }
        }
    }

    /// The bytes to write for `text` typed as the value of `t`, as
    /// `(offset, bytes)` runs.
    pub fn encode_edit(
        &mut self,
        t: EditTarget,
        text: &str,
    ) -> Result<Vec<(u64, Vec<u8>)>, String> {
        self.steps = 0;
        self.writes = Some(Vec::new());
        self.in_callback += 1;
        let r = self.encode_inner(t, text);
        self.in_callback -= 1;
        let writes = self.writes.take().unwrap_or_default();
        r.map(|_| writes)
    }

    fn encode_inner(&mut self, t: EditTarget, text: &str) -> Result<(), String> {
        let fail = |s: Stop| match s {
            Stop::Error(m, _) => m,
            _ => "stopped".to_string(),
        };
        match t {
            EditTarget::Elem(r, i) => {
                let n = self.tree.node(r.id).clone();
                let NodeKind::Array { elem_prim: Some(p), .. } = n.kind else {
                    return Err("not an array of values".into());
                };
                let v = self.parse_scalar(p, n.flags & F_ENUM != 0, n.enum_ty, text)?;
                self.store(&Place::NodeElem(r, i), v).map_err(fail)
            }
            EditTarget::Node(r) => {
                if let Some(at) = self.tree.lazy_of(r.id).write {
                    return self
                        .attr_value(at, r, Some(Value::Str(text.as_bytes().to_vec())))
                        .map(|_| ())
                        .map_err(fail);
                }
                let n = self.tree.node(r.id).clone();
                if n.flags & F_READONLY != 0 {
                    return Err(
                        "this value is read-only (it has a read function and no write function)"
                            .into(),
                    );
                }
                match &n.kind {
                    NodeKind::Scalar { prim, .. } => {
                        let v = self.parse_scalar(*prim, n.flags & F_ENUM != 0, n.enum_ty, text)?;
                        self.store(&Place::Node(r), v).map_err(fail)
                    }
                    NodeKind::Str { .. }
                    | NodeKind::Array {
                        elem_prim: Some(Prim::Char | Prim::UChar | Prim::WChar),
                        ..
                    } => {
                        let bytes = unquote(text);
                        self.store(&Place::Node(r), Value::Str(bytes)).map_err(fail)
                    }
                    NodeKind::Array { .. } => {
                        let raw = guid_digits(text)?;
                        // The first three groups are stored little-endian.
                        let order = [3, 2, 1, 0, 5, 4, 7, 6, 8, 9, 10, 11, 12, 13, 14, 15];
                        let mut bytes = [0u8; 16];
                        for (k, &o) in order.iter().enumerate() {
                            bytes[o] = raw[k];
                        }
                        if let Some(w) = &mut self.writes {
                            w.push((n.start + r.shift, bytes.to_vec()));
                        }
                        Ok(())
                    }
                    NodeKind::Struct { .. } => {
                        Err("a struct has no value of its own to edit".into())
                    }
                }
            }
        }
    }

    /// `text` as a value of scalar type `prim`: an enum constant's name, or
    /// anything [`parse_scalar`] reads.
    fn parse_scalar(
        &mut self,
        prim: Prim,
        is_enum: bool,
        enum_ty: u32,
        text: &str,
    ) -> Result<Value, String> {
        if is_enum {
            let t = text.trim();
            let name = t.split(" (").next().unwrap_or(t).trim();
            if let Some(v) =
                self.enum_constants(enum_ty).iter().find(|(n, _)| n == name).map(|(_, v)| *v)
            {
                let it = IntTy::of(prim);
                return Ok(Value::Int(it.norm(v as u64), it));
            }
        }
        parse_scalar(prim, text)
    }
}

/// `text` as a value of scalar type `prim`: numbers in any notation the panel
/// shows (decimal, `0x…`, `…h`, `0b…`, a character in quotes), or a date.
pub(crate) fn parse_scalar(prim: Prim, text: &str) -> Result<Value, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("enter a value".into());
    }
    if prim.is_float() && prim != Prim::OleTime {
        return t
            .parse::<f64>()
            .map(|f| Value::Float(f, prim != Prim::Double))
            .map_err(|_| format!("'{t}' is not a number"));
    }
    let date = |n: usize| -> Vec<i64> {
        t.split(|c: char| !c.is_ascii_digit())
            .filter(|x| !x.is_empty())
            .filter_map(|x| x.parse().ok())
            .take(n)
            .collect()
    };
    match prim {
        Prim::DosDate => {
            let d = date(3);
            if d.len() < 3 {
                return Err("a date is MM/dd/yyyy".into());
            }
            let v =
                ((d[2] - 1980).clamp(0, 127) << 9) | (d[0].clamp(1, 12) << 5) | d[1].clamp(1, 31);
            return Ok(Value::Int(v as u64, IntTy::of(prim)));
        }
        Prim::DosTime => {
            let d = date(3);
            if d.len() < 2 {
                return Err("a time is hh:mm:ss".into());
            }
            let s = d.get(2).copied().unwrap_or(0);
            let v = (d[0].clamp(0, 23) << 11) | (d[1].clamp(0, 59) << 5) | (s.clamp(0, 59) / 2);
            return Ok(Value::Int(v as u64, IntTy::of(prim)));
        }
        Prim::FileTime | Prim::TimeT | Prim::Time64T | Prim::OleTime => {
            let d = date(6);
            if d.len() < 3 {
                return Err("a date is MM/dd/yyyy hh:mm:ss".into());
            }
            let secs = time::days_from_civil(d[2], d[0] as u32, d[1] as u32) * 86_400
                + d.get(3).copied().unwrap_or(0) * 3600
                + d.get(4).copied().unwrap_or(0) * 60
                + d.get(5).copied().unwrap_or(0);
            return Ok(match prim {
                Prim::FileTime => Value::Int(
                    ((secs + 11_644_473_600) as u64).wrapping_mul(10_000_000),
                    IntTy::of(prim),
                ),
                Prim::OleTime => Value::Float(secs as f64 / 86_400.0 + 25_569.0, false),
                _ => Value::Int(secs as u64, IntTy::of(prim)),
            });
        }
        _ => {}
    }
    let it = IntTy::of(prim);
    let n = parse_int(t).ok_or_else(|| format!("'{t}' is not a number"))?;
    let bits = it.bytes as u32 * 8;
    let fits = if bits >= 64 {
        true
    } else if it.signed {
        n >= -(1i128 << (bits - 1)) && n < (1i128 << (bits - 1))
    } else {
        n >= -(1i128 << (bits - 1)) && n < (1i128 << bits)
    };
    if !fits {
        return Err(format!("{t} doesn't fit in {} bytes", it.bytes));
    }
    Ok(Value::Int(it.norm(n as u64), it))
}

/// The 16 bytes a GUID's 32 hex digits spell, in the order they are written
/// (braces, dashes and spaces ignored).
pub(crate) fn guid_digits(text: &str) -> Result<[u8; 16], String> {
    let hex: Vec<u8> = text.bytes().filter(u8::is_ascii_hexdigit).collect();
    if hex.len() != 32 {
        return Err("a GUID needs 32 hex digits".into());
    }
    let mut out = [0u8; 16];
    for (k, pair) in hex.chunks(2).enumerate() {
        out[k] = u8::from_str_radix(std::str::from_utf8(pair).unwrap_or("0"), 16).unwrap_or(0);
    }
    Ok(out)
}

/// An integer in any of the notations the panel shows.
pub fn parse_int(text: &str) -> Option<i128> {
    let t = text.trim();
    // A character shown as `65 'A'`, or typed as `'A'`.
    if let Some(inner) = t.strip_prefix('\'').and_then(|x| x.strip_suffix('\'')) {
        let mut ch = inner.chars();
        return match (ch.next(), ch.next()) {
            (Some(c), None) => Some(c as i128),
            _ => None,
        };
    }
    let t = t.split_whitespace().next()?;
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let lower = t.to_ascii_lowercase();
    let v = if let Some(h) = lower.strip_prefix("0x") {
        i128::from_str_radix(h, 16).ok()?
    } else if let Some(b) = lower.strip_prefix("0b") {
        i128::from_str_radix(b, 2).ok()?
    } else if let Some(b) = lower
        .strip_suffix('b')
        .filter(|b| !b.is_empty() && b.bytes().all(|c| c == b'0' || c == b'1'))
    {
        i128::from_str_radix(b, 2).ok()?
    } else if let Some(h) = lower.strip_suffix('h') {
        i128::from_str_radix(h, 16).ok()?
    } else {
        lower.parse::<i128>().ok()?
    };
    Some(if neg { -v } else { v })
}

/// A string as the panel shows it (`"a\tb"`), back to bytes.
pub fn unquote(text: &str) -> Vec<u8> {
    let t = text.trim();
    let t = t.strip_prefix('"').and_then(|x| x.strip_suffix('"')).unwrap_or(text);
    let mut out = Vec::new();
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match chars.next() {
            Some('n') => out.push(b'\n'),
            Some('r') => out.push(b'\r'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('x') => {
                let hex: String = [chars.next(), chars.next()].into_iter().flatten().collect();
                out.push(u8::from_str_radix(&hex, 16).unwrap_or(0));
            }
            Some(other) => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(other.encode_utf8(&mut buf).as_bytes());
            }
            None => out.push(b'\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::super::tests::{node, run};
    use super::*;

    #[test]
    fn integers_parse_in_every_shown_form() {
        assert_eq!(parse_int("0x1F"), Some(31));
        assert_eq!(parse_int("1Fh"), Some(31));
        assert_eq!(parse_int("101b"), Some(5));
        assert_eq!(parse_int("-12"), Some(-12));
        assert_eq!(parse_int("65 'A'"), Some(65));
        assert_eq!(parse_int("'A'"), Some(65));
        assert_eq!(parse_int("nope"), None);
        assert_eq!(unquote(r#""a\tb\x41""#), b"a\tbA");
    }

    #[test]
    fn edits_encode_by_type_and_byte_order() {
        let src = r#"
            enum <ushort> KIND { OFF, ON = 0x102 } kind;
            BigEndian(); uint big;
            LittleEndian(); ushort flags : 4; ushort rest : 12;
            char name[4]; uchar arr[3]; float f;
            typedef uchar SHOWN <read=Str("%d!", this)>; SHOWN shown;
            typedef uchar FIXED <read=Str("%d", this / 2), write=(this = Atoi(value) * 2)>; FIXED fixed;
        "#;
        let mut data = vec![0u8; 22];
        data[8] = 0xf0;
        let (mut it, err) = run(src, &data);
        assert!(err.is_none(), "{err:?}");
        let kind = node(&mut it, "kind");
        assert_eq!(
            it.encode_edit(EditTarget::Node(kind), "ON (258)").unwrap(),
            vec![(0, vec![0x02, 0x01])]
        );
        let big = node(&mut it, "big");
        assert_eq!(
            it.encode_edit(EditTarget::Node(big), "0x01020304").unwrap(),
            vec![(2, vec![1, 2, 3, 4])]
        );
        assert!(it.encode_edit(EditTarget::Node(kind), "70000").is_err());
        // A bitfield keeps its neighbours' bits.
        let flags = node(&mut it, "flags");
        assert_eq!(
            it.encode_edit(EditTarget::Node(flags), "5").unwrap(),
            vec![(6, vec![0x05, 0x00])]
        );
        let name = node(&mut it, "name");
        assert_eq!(
            it.encode_edit(EditTarget::Node(name), "\"ab\"").unwrap(),
            vec![(8, b"ab\0\0".to_vec())]
        );
        let arr = node(&mut it, "arr");
        assert_eq!(it.encode_edit(EditTarget::Elem(arr, 2), "255").unwrap(), vec![(14, vec![255])]);
        let f = node(&mut it, "f");
        assert_eq!(
            it.encode_edit(EditTarget::Node(f), "1.5").unwrap(),
            vec![(15, 1.5f32.to_le_bytes().to_vec())]
        );
        let shown = node(&mut it, "shown");
        assert!(!it.editable(EditTarget::Node(shown)));
        assert!(it.encode_edit(EditTarget::Node(shown), "1").is_err());
        let fixed = node(&mut it, "fixed");
        assert!(it.editable(EditTarget::Node(fixed)));
        assert_eq!(it.encode_edit(EditTarget::Node(fixed), "21").unwrap(), vec![(20, vec![42])]);
    }
}
