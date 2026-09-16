//! Just enough DER to read keys by hand: elements walked one by one, object
//! identifiers named, and the few encodings needed to rebuild a public key's
//! SubjectPublicKeyInfo (what a key pin is the hash of).

pub const INTEGER: u8 = 0x02;
pub const BIT_STRING: u8 = 0x03;
pub const OCTET_STRING: u8 = 0x04;
pub const NULL: u8 = 0x05;
pub const OID: u8 = 0x06;
pub const SEQUENCE: u8 = 0x30;

pub const RSA: &str = "1.2.840.113549.1.1.1";
pub const EC: &str = "1.2.840.10045.2.1";
pub const ED25519: &str = "1.3.101.112";
pub const ED448: &str = "1.3.101.113";
pub const X25519: &str = "1.3.101.110";
pub const X448: &str = "1.3.101.111";
pub const PBES2: &str = "1.2.840.113549.1.5.13";
pub const PBKDF2: &str = "1.2.840.113549.1.5.12";

/// One DER element: its tag byte and its contents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tlv<'a> {
    pub tag: u8,
    pub body: &'a [u8],
}

/// The element at the start of `data`, and what follows it.
pub fn read(data: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let (&tag, rest) = data.split_first()?;
    if tag & 0x1f == 0x1f {
        return None;
    }
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        (rest[..n].iter().fold(0usize, |a, &b| (a << 8) | b as usize), &rest[n..])
    };
    (rest.len() >= len).then(|| (Tlv { tag, body: &rest[..len] }, &rest[len..]))
}

/// The elements inside a constructed element's contents.
pub fn children(body: &[u8]) -> Option<Vec<Tlv<'_>>> {
    let mut out = Vec::new();
    let mut rest = body;
    while !rest.is_empty() {
        let (t, r) = read(rest)?;
        out.push(t);
        rest = r;
    }
    Some(out)
}

/// The members of the SEQUENCE that is the whole of `data`.
pub fn sequence(data: &[u8]) -> Option<Vec<Tlv<'_>>> {
    let (t, rest) = read(data)?;
    if t.tag != SEQUENCE || !rest.is_empty() {
        return None;
    }
    children(t.body)
}

/// An INTEGER's magnitude, without the zero byte that keeps it positive.
pub fn unsigned(body: &[u8]) -> &[u8] {
    let zeros = body.iter().take_while(|&&b| b == 0).count();
    &body[zeros.min(body.len().saturating_sub(1))..]
}

/// How many bits an unsigned big-endian number takes.
pub fn bit_len(bytes: &[u8]) -> usize {
    let n = unsigned(bytes);
    match n.first() {
        Some(&b) if b != 0 => (n.len() - 1) * 8 + (8 - b.leading_zeros() as usize),
        _ => 0,
    }
}

/// An element encoded.
pub fn tlv(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = vec![tag];
    let len = body.len();
    if len < 0x80 {
        out.push(len as u8);
    } else {
        let bytes = len.to_be_bytes();
        let skip = bytes.iter().take_while(|&&b| b == 0).count();
        out.push(0x80 | (bytes.len() - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(body);
    out
}

/// An INTEGER holding the unsigned `magnitude`.
pub fn integer(magnitude: &[u8]) -> Vec<u8> {
    let m = unsigned(magnitude);
    if m.first().is_some_and(|&b| b & 0x80 != 0) {
        let mut body = vec![0];
        body.extend_from_slice(m);
        tlv(INTEGER, &body)
    } else {
        tlv(INTEGER, m)
    }
}

/// An object identifier's contents in dotted form.
pub fn oid_text(body: &[u8]) -> String {
    let mut arcs: Vec<u64> = Vec::new();
    let mut v: u64 = 0;
    for &b in body {
        v = (v << 7) | (b & 0x7f) as u64;
        if b & 0x80 == 0 {
            if arcs.is_empty() {
                let first = (v / 40).min(2);
                arcs.push(first);
                arcs.push(v - first * 40);
            } else {
                arcs.push(v);
            }
            v = 0;
        }
    }
    arcs.iter().map(u64::to_string).collect::<Vec<_>>().join(".")
}

/// An object identifier encoded from its dotted form.
pub fn oid(dotted: &str) -> Vec<u8> {
    let arcs: Vec<u64> = dotted.split('.').filter_map(|a| a.parse().ok()).collect();
    let mut body = Vec::new();
    let mut push = |mut v: u64| {
        let mut groups = vec![(v & 0x7f) as u8];
        v >>= 7;
        while v > 0 {
            groups.push((v & 0x7f) as u8 | 0x80);
            v >>= 7;
        }
        body.extend(groups.iter().rev());
    };
    if arcs.len() >= 2 {
        push(arcs[0] * 40 + arcs[1]);
        arcs[2..].iter().for_each(|&a| push(a));
    }
    tlv(OID, &body)
}

/// What the object identifiers keys and their encryption use are called.
pub fn oid_name(dotted: &str) -> Option<&'static str> {
    Some(match dotted {
        RSA => "RSA",
        "1.2.840.113549.1.1.10" => "RSA-PSS",
        EC => "EC",
        ED25519 => "Ed25519",
        ED448 => "Ed448",
        X25519 => "X25519",
        X448 => "X448",
        "1.2.840.10040.4.1" => "DSA",
        "1.2.840.10045.3.1.7" => "P-256",
        "1.3.132.0.34" => "P-384",
        "1.3.132.0.35" => "P-521",
        "1.3.132.0.10" => "secp256k1",
        "1.3.36.3.3.2.8.1.1.7" => "brainpoolP256r1",
        "1.3.36.3.3.2.8.1.1.11" => "brainpoolP384r1",
        "1.3.36.3.3.2.8.1.1.13" => "brainpoolP512r1",
        PBES2 => "PBES2",
        PBKDF2 => "PBKDF2",
        "1.3.6.1.4.1.11591.4.11" => "scrypt",
        "2.16.840.1.101.3.4.1.2" => "AES-128-CBC",
        "2.16.840.1.101.3.4.1.22" => "AES-192-CBC",
        "2.16.840.1.101.3.4.1.42" => "AES-256-CBC",
        "2.16.840.1.101.3.4.1.6" => "AES-128-GCM",
        "2.16.840.1.101.3.4.1.46" => "AES-256-GCM",
        "1.2.840.113549.3.7" => "DES-EDE3-CBC",
        "1.2.840.113549.2.7" => "HMAC-SHA1",
        "1.2.840.113549.2.9" => "HMAC-SHA256",
        "1.2.840.113549.2.10" => "HMAC-SHA384",
        "1.2.840.113549.2.11" => "HMAC-SHA512",
        "1.2.840.113549.1.12.1.3" => "PBE-SHA1-3DES",
        "1.2.840.113549.1.12.1.6" => "PBE-SHA1-RC2-40",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elements_read_and_write_back() {
        let seq = tlv(SEQUENCE, &[integer(&[0x00, 0x80]), oid(EC)].concat());
        let members = sequence(&seq).unwrap();
        assert_eq!(members.len(), 2);
        assert_eq!(members[0], Tlv { tag: INTEGER, body: &[0x00, 0x80] });
        assert_eq!(oid_text(members[1].body), EC);
        assert_eq!(oid_name(EC), Some("EC"));
        // A long form length.
        let big = tlv(OCTET_STRING, &[7u8; 300]);
        assert_eq!(&big[..4], &[OCTET_STRING, 0x82, 0x01, 0x2c]);
        assert_eq!(read(&big).unwrap().0.body.len(), 300);
        assert!(read(&big[..100]).is_none(), "cut short");
        assert_eq!(bit_len(&[0x00, 0x80, 0x00]), 16);
        assert_eq!(bit_len(&[0x01]), 1);
        assert_eq!(oid_text(&oid("1.3.132.0.34")[2..]), "1.3.132.0.34");
    }
}
