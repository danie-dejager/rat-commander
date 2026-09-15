//! Values in a running template.
//!
//! Integers keep their C width and signedness, so wrap-around, `>>` and
//! comparisons behave as a template expects (`long` is 32-bit, as in 010
//! Editor). A file variable stays a [`Value::Node`] until something needs its
//! contents.

use super::ast::{Prim, Sym, TypeId};
use super::tree::{NONE, NodeRef};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntTy {
    pub bytes: u8,
    pub signed: bool,
    /// The enum type an integer came from, or `NONE`.
    pub enum_ty: TypeId,
}

impl IntTy {
    pub const fn new(bytes: u8, signed: bool) -> IntTy {
        IntTy { bytes, signed, enum_ty: NONE }
    }
    pub const I8: IntTy = IntTy::new(1, true);
    pub const I32: IntTy = IntTy::new(4, true);
    pub const U32: IntTy = IntTy::new(4, false);
    pub const I64: IntTy = IntTy::new(8, true);
    pub const U64: IntTy = IntTy::new(8, false);

    pub fn of(p: Prim) -> IntTy {
        IntTy::new(p.size().clamp(1, 8) as u8, p.is_signed())
    }

    /// `v` truncated to this width and sign- or zero-extended back to 64 bits.
    pub fn norm(self, v: u64) -> u64 {
        let bits = self.bytes as u32 * 8;
        if bits >= 64 {
            return v;
        }
        let mask = (1u64 << bits) - 1;
        let t = v & mask;
        if self.signed && (t >> (bits - 1)) & 1 == 1 { t | !mask } else { t }
    }

    /// The type of `a op b` under C's usual arithmetic conversions.
    pub fn common(a: IntTy, b: IntTy) -> IntTy {
        let pa = a.promoted();
        let pb = b.promoted();
        if pa.signed == pb.signed {
            IntTy::new(pa.bytes.max(pb.bytes), pa.signed)
        } else {
            let (s, u) = if pa.signed { (pa, pb) } else { (pb, pa) };
            if u.bytes >= s.bytes { IntTy::new(u.bytes, false) } else { IntTy::new(s.bytes, true) }
        }
    }

    /// Integer promotion: anything narrower than `int` becomes `int`.
    pub fn promoted(self) -> IntTy {
        if self.bytes < 4 { IntTy::I32 } else { IntTy::new(self.bytes, self.signed) }
    }
}

#[derive(Debug, Clone)]
pub struct LocalArray {
    pub elem: TypeId,
    pub items: Vec<Value>,
}

/// A local struct (`local TFindResults r;`).
#[derive(Debug, Clone, Default)]
pub struct Record {
    pub fields: Vec<(Sym, Value)>,
}

impl Record {
    pub fn field(&self, s: Sym) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| *k == s).map(|(_, v)| v)
    }
    pub fn field_mut(&mut self, s: Sym) -> Option<&mut Value> {
        self.fields.iter_mut().find(|(k, _)| *k == s).map(|(_, v)| v)
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    Void,
    Int(u64, IntTy),
    /// A floating value, and whether it is a 32-bit `float`.
    Float(f64, bool),
    Str(Vec<u8>),
    WStr(Vec<u16>),
    Node(NodeRef),
    Array(Box<LocalArray>),
    Record(Box<Record>),
}

impl Value {
    pub fn int(v: i64) -> Value {
        Value::Int(v as u64, IntTy::I32)
    }

    pub fn int64(v: i64) -> Value {
        Value::Int(v as u64, IntTy::I64)
    }

    pub fn uint64(v: u64) -> Value {
        Value::Int(v, IntTy::U64)
    }

    pub fn bool(b: bool) -> Value {
        Value::Int(b as u64, IntTy::I32)
    }

    /// The integer as a signed 64-bit number (an unsigned 64-bit value above
    /// `i64::MAX` wraps).
    pub fn as_i64_lossy(&self) -> Option<i64> {
        match self {
            Value::Int(v, _) => Some(*v as i64),
            Value::Float(f, _) => Some(*f as i64),
            _ => None,
        }
    }

    pub fn as_f64_lossy(&self) -> Option<f64> {
        match self {
            Value::Int(v, t) => Some(if t.signed { *v as i64 as f64 } else { *v as f64 }),
            Value::Float(f, _) => Some(*f),
            _ => None,
        }
    }
}

/// Decode UTF-16 to UTF-8 bytes (unpaired surrogates become U+FFFD).
pub fn wide_to_bytes(w: &[u16]) -> Vec<u8> {
    String::from_utf16_lossy(w).into_bytes()
}

/// Encode bytes (UTF-8, or Latin-1 where they aren't) as UTF-16.
pub fn bytes_to_wide(b: &[u8]) -> Vec<u16> {
    match std::str::from_utf8(b) {
        Ok(s) => s.encode_utf16().collect(),
        Err(_) => b.iter().map(|&c| c as u16).collect(),
    }
}

/// Bytes up to (not including) the first NUL.
pub fn until_nul(b: &[u8]) -> &[u8] {
    match b.iter().position(|&c| c == 0) {
        Some(i) => &b[..i],
        None => b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_wrap_and_promote_like_c() {
        assert_eq!(IntTy::new(1, false).norm(256) as i64, 0);
        assert_eq!(IntTy::new(1, true).norm(0xff) as i64, -1);
        assert_eq!(IntTy::new(2, false).norm(0x1_ffff), 0xffff);
        assert_eq!(IntTy::common(IntTy::new(1, false), IntTy::new(2, false)), IntTy::I32);
        assert_eq!(IntTy::common(IntTy::I32, IntTy::U32), IntTy::U32);
        assert_eq!(IntTy::common(IntTy::I64, IntTy::U32), IntTy::I64);
        assert_eq!(IntTy::common(IntTy::I64, IntTy::U64), IntTy::U64);
    }
}
