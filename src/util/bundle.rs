//! Reading the file bundles `build.rs` packs into the program: an 8-byte
//! magic, a u32 entry count, then zlib of the entries, each a u16 name
//! length, the name, a u32 content length and the content.

use std::io::Read;

/// A bundled file: its name and content.
pub type Entry = (Box<str>, Box<[u8]>);

/// The entries of `packed`, a bundle with `magic` holding `count` files (a
/// capacity hint); none when it isn't one.
pub fn unpack(packed: &[u8], magic: &[u8; 8], count: usize) -> Vec<Entry> {
    let mut out = Vec::with_capacity(count);
    if packed.len() < 12 || &packed[..8] != magic {
        return out;
    }
    let mut payload = Vec::new();
    let _ = flate2::read::ZlibDecoder::new(&packed[12..]).read_to_end(&mut payload);
    let mut p = 0usize;
    let take = |p: &mut usize, n: usize| -> Option<&[u8]> {
        let s = payload.get(*p..*p + n)?;
        *p += n;
        Some(s)
    };
    while p < payload.len() {
        let Some(n) = take(&mut p, 2).map(|b| u16::from_le_bytes([b[0], b[1]]) as usize) else {
            break;
        };
        let Some(name) = take(&mut p, n).map(|b| String::from_utf8_lossy(b).into_owned()) else {
            break;
        };
        let Some(len) =
            take(&mut p, 4).map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        else {
            break;
        };
        let Some(body) = take(&mut p, len) else { break };
        out.push((name.into_boxed_str(), body.to_vec().into_boxed_slice()));
    }
    out
}
