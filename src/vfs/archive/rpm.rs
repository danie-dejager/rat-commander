//! RPM packages browsed like a directory.
//!
//! An `.rpm` is a 96-byte lead, then two header sections (a signature header
//! and the main one), then a compressed cpio payload. Only three things are
//! needed to read the files out: where the payload starts, what compressed it,
//! and that it really is cpio — the payload itself is authoritative about what
//! the package contains, so none of the file-list tags need parsing.
//!
//! Read-only: the header carries a signature over the payload, so any rewrite
//! would invalidate it.

use super::cpio;
use super::formats::{Comp, FullEntry, RawEntry, normalize};
use crate::util::{Error, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// `RPMTAG_PAYLOADFORMAT` — must say `cpio`.
const TAG_PAYLOADFORMAT: u32 = 1124;
/// `RPMTAG_PAYLOADCOMPRESSOR` — `gzip`, `xz`, `zstd`, `bzip2`, `lzma` or `none`.
const TAG_PAYLOADCOMPRESSOR: u32 = 1125;
/// Index-entry types used by the tags read here.
const TYPE_INT16: u32 = 3;
const TYPE_INT32: u32 = 4;
const TYPE_INT64: u32 = 5;
const TYPE_STRING: u32 = 6;
const TYPE_STRING_ARRAY: u32 = 8;

/// The file list. A package built by rpm 4.14 or later keeps *all* per-file
/// metadata here rather than in the payload, so these are not optional extras:
/// without them an indexed payload cannot be given names at all.
const TAG_FILESIZES: u32 = 1028;
const TAG_FILEMODES: u32 = 1030;
const TAG_FILEMTIMES: u32 = 1034;
const TAG_DIRINDEXES: u32 = 1116;
const TAG_BASENAMES: u32 = 1117;
const TAG_DIRNAMES: u32 = 1118;
/// Used in place of `TAG_FILESIZES` once a package holds a file over 4 GiB.
const TAG_LONGFILESIZES: u32 = 5008;

/// A header section's magic: three bytes plus a version byte.
const HEADER_MAGIC: [u8; 4] = [0x8E, 0xAD, 0xE8, 0x01];
const LEAD_LEN: u64 = 96;

/// Where the payload begins, how it is packed, and what the header says each
/// of its members is.
struct Payload {
    offset: u64,
    comp: Comp,
    files: Vec<cpio::Meta>,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Read one header section starting at `at`, returning its index, its store,
/// and the offset just past it.
fn read_header(f: &mut File, at: u64) -> Result<(Vec<u8>, Vec<u8>, u64)> {
    f.seek(SeekFrom::Start(at))?;
    let mut intro = [0u8; 16];
    f.read_exact(&mut intro)?;
    if intro[..4] != HEADER_MAGIC {
        return Err(Error::other("not an RPM package: bad header magic"));
    }
    let (nindex, hsize) = (be32(&intro[8..12]) as u64, be32(&intro[12..16]) as u64);
    // A damaged package must not talk us into a huge allocation.
    if nindex > 100_000 || hsize > (64 << 20) {
        return Err(Error::other("implausible RPM header"));
    }
    let mut index = vec![0u8; (nindex * 16) as usize];
    f.read_exact(&mut index)?;
    let mut store = vec![0u8; hsize as usize];
    f.read_exact(&mut store)?;
    Ok((index, store, at + 16 + nindex * 16 + hsize))
}

/// The NUL-terminated string an index entry of type STRING points at.
fn string_at(store: &[u8], offset: u32) -> Option<String> {
    let rest = store.get(offset as usize..)?;
    let end = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    Some(String::from_utf8_lossy(&rest[..end]).into_owned())
}

/// Locate a tag's index entry: its type, offset into the store, and count.
fn entry(index: &[u8], tag: u32) -> Option<(u32, u32, u32)> {
    index
        .as_chunks::<16>()
        .0
        .iter()
        .find(|e| be32(&e[0..4]) == tag)
        .map(|e| (be32(&e[4..8]), be32(&e[8..12]), be32(&e[12..16])))
}

/// Find a STRING tag in a header's index.
fn string_tag(index: &[u8], store: &[u8], tag: u32) -> Option<String> {
    match entry(index, tag)? {
        (TYPE_STRING, offset, _) => string_at(store, offset),
        _ => None,
    }
}

/// A STRING_ARRAY tag: `count` NUL-terminated strings laid end to end.
fn string_array(index: &[u8], store: &[u8], tag: u32) -> Option<Vec<String>> {
    let (ty, offset, count) = entry(index, tag)?;
    if ty != TYPE_STRING_ARRAY {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    let mut rest = store.get(offset as usize..)?;
    for _ in 0..count {
        let end = rest.iter().position(|b| *b == 0)?;
        out.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        rest = rest.get(end + 1..)?;
    }
    Some(out)
}

/// An integer-array tag, whatever width it was stored at.
fn int_array(index: &[u8], store: &[u8], tag: u32) -> Option<Vec<u64>> {
    let (ty, offset, count) = entry(index, tag)?;
    let width = match ty {
        TYPE_INT16 => 2,
        TYPE_INT32 => 4,
        TYPE_INT64 => 8,
        _ => return None,
    };
    let bytes = store.get(offset as usize..offset as usize + width * count as usize)?;
    Some(
        bytes
            .chunks_exact(width)
            .map(|c| c.iter().fold(0u64, |a, b| (a << 8) | u64::from(*b)))
            .collect(),
    )
}

/// The package's file list, in payload order.
///
/// A path is `DIRNAMES[DIRINDEXES[i]] + BASENAMES[i]`; rpm splits it that way
/// so a package with many files in few directories stores each directory once.
fn file_list(index: &[u8], store: &[u8]) -> Vec<cpio::Meta> {
    let (Some(base), Some(dirs), Some(di)) = (
        string_array(index, store, TAG_BASENAMES),
        string_array(index, store, TAG_DIRNAMES),
        int_array(index, store, TAG_DIRINDEXES),
    ) else {
        return Vec::new();
    };
    let sizes = int_array(index, store, TAG_LONGFILESIZES)
        .or_else(|| int_array(index, store, TAG_FILESIZES))
        .unwrap_or_default();
    let modes = int_array(index, store, TAG_FILEMODES).unwrap_or_default();
    let times = int_array(index, store, TAG_FILEMTIMES).unwrap_or_default();

    (0..base.len())
        .map(|i| {
            let dir =
                di.get(i).and_then(|d| dirs.get(*d as usize)).map(String::as_str).unwrap_or("");
            cpio::Meta {
                path: normalize(&format!("{dir}{}", base[i])),
                // Default to a plain readable file when the tag is missing, so a
                // listing still shows something sensible.
                mode: modes.get(i).copied().unwrap_or(0o100644) as u32,
                size: sizes.get(i).copied().unwrap_or(0),
                mtime: times
                    .get(i)
                    .and_then(|t| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(*t))),
            }
        })
        .collect()
}

/// Locate the payload: skip the lead, skip the signature header (which is
/// padded out to an 8-byte boundary), then read the main header for the two
/// tags that say what follows.
fn locate(container: &Path) -> Result<Payload> {
    let mut f = File::open(container)?;
    let mut lead = [0u8; 4];
    f.read_exact(&mut lead)?;
    if lead != [0xED, 0xAB, 0xEE, 0xDB] {
        return Err(Error::other("not an RPM package"));
    }

    let (_, _, after_sig) = read_header(&mut f, LEAD_LEN)?;
    // The signature header — and only the signature header — is padded so the
    // main header starts on an 8-byte boundary.
    let main_at = after_sig.div_ceil(8) * 8;
    let (index, store, after_main) = read_header(&mut f, main_at)?;

    if let Some(fmt) = string_tag(&index, &store, TAG_PAYLOADFORMAT)
        && fmt != "cpio"
    {
        return Err(Error::other(format!("unsupported RPM payload format: {fmt}")));
    }
    let named = string_tag(&index, &store, TAG_PAYLOADCOMPRESSOR);
    let comp = match named.as_deref() {
        Some("gzip") => Comp::Gz,
        Some("xz") => Comp::Xz,
        Some("zstd") => Comp::Zst,
        Some("bzip2") => Comp::Bz2,
        Some("none") => Comp::None,
        // The tag is advisory — sniff the payload when it is missing, and say
        // which compressor we could not handle when it names one we lack.
        Some(other) => {
            return Err(Error::other(format!("unsupported RPM payload compressor: {other}")));
        }
        None => sniff(&mut f, after_main)?,
    };
    Ok(Payload { offset: after_main, comp, files: file_list(&index, &store) })
}

/// Identify the payload compressor from its first bytes, for a package whose
/// header does not name one.
fn sniff(f: &mut File, at: u64) -> Result<Comp> {
    f.seek(SeekFrom::Start(at))?;
    let mut magic = [0u8; 6];
    let n = f.read(&mut magic)?;
    let m = &magic[..n];
    Ok(if m.starts_with(&[0x1F, 0x8B]) {
        Comp::Gz
    } else if m.starts_with(&[0xFD, b'7', b'z', b'X', b'Z']) {
        Comp::Xz
    } else if m.starts_with(&[0x28, 0xB5, 0x2F, 0xFD]) {
        Comp::Zst
    } else if m.starts_with(b"BZh") {
        Comp::Bz2
    } else if m.starts_with(b"070701") || m.starts_with(b"070702") {
        Comp::None
    } else {
        return Err(Error::other("unrecognised RPM payload compressor"));
    })
}

/// Walk the payload, handing each member's metadata and bytes to `visit`.
/// Returning `false` stops the walk, which is how reading one file avoids
/// decompressing the rest of a large package.
///
/// Both payload shapes end up in the same place: a classic member brings its
/// own metadata, an indexed one is looked up in the header's file list.
fn walk(
    container: &Path,
    p: &Payload,
    want_data: bool,
    mut visit: impl FnMut(cpio::Meta, Vec<u8>) -> Result<bool>,
) -> Result<()> {
    let mut f = File::open(container)?;
    f.seek(SeekFrom::Start(p.offset))?;
    let mut r = cpio::Reader::new(super::formats::decompress(p.comp, f)?);
    while let Some(h) = r.next_header()? {
        let meta =
            match h {
                cpio::Header::End => break,
                cpio::Header::Full(m) => m,
                cpio::Header::Indexed(i) => p.files.get(i as usize).cloned().ok_or_else(|| {
                    Error::other("RPM payload names a file the header does not list")
                })?,
            };
        let data = r.data(meta.size, want_data)?;
        if !visit(meta, data)? {
            break;
        }
    }
    Ok(())
}

pub(super) fn list_rpm(container: &Path) -> Result<Vec<RawEntry>> {
    let p = locate(container)?;
    let mut out = Vec::new();
    walk(container, &p, false, |m, _| {
        let (is_dir, mode) = (m.is_dir(), m.permissions());
        if m.path != "/" {
            out.push(RawEntry {
                path: m.path,
                is_dir,
                size: m.size,
                mtime: m.mtime,
                mode: Some(mode),
            });
        }
        Ok(true)
    })?;
    Ok(out)
}

pub(super) fn read_rpm_entry(container: &Path, inner: &str) -> Result<Vec<u8>> {
    let target = normalize(inner);
    let p = locate(container)?;
    let mut found = None;
    walk(container, &p, true, |m, data| {
        if m.path == target {
            found = Some(data);
            return Ok(false); // the rest of the payload is irrelevant
        }
        Ok(true)
    })?;
    found.ok_or(Error::NotFound(target))
}

pub(super) fn read_rpm_all(container: &Path) -> Result<Vec<FullEntry>> {
    let p = locate(container)?;
    let mut out = Vec::new();
    walk(container, &p, true, |m, data| {
        let (is_dir, mode) = (m.is_dir(), m.permissions());
        if m.path != "/" {
            out.push(FullEntry { path: m.path, is_dir, data, mtime: m.mtime, mode: Some(mode) });
        }
        Ok(true)
    })?;
    Ok(out)
}
