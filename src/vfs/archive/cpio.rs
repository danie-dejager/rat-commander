//! The cpio stream that carries an RPM's files, in both the shapes RPM writes.
//!
//! **Classic (`070701`/`070702`).** The "new ASCII" format: a 110-byte header
//! of 8-digit hex fields, then the name, then the data, each padded out to a
//! 4-byte boundary. Everything a listing needs is in the header.
//!
//! **Indexed (`07070X`).** What rpm 4.14 and later write. The header shrinks to
//! sixteen bytes — magic, the file's index into the package header's file list,
//! and two bytes of padding — and the name, mode, size and timestamp all move
//! into that header instead. So the payload alone is *not* enough to read a
//! modern package: the caller has to supply the metadata per index. The stream
//! still ends with a classic `TRAILER!!!` member either way.
//!
//! This module only handles the stream mechanics; the file list that resolves
//! an index lives in [`super::rpm`], which drives the walk.

use super::formats::normalize;
use crate::util::{Error, Result};
use std::io::Read;
use std::time::{Duration, SystemTime};

/// Classic header length before the name: 6 magic + 13 fields of 8 hex digits.
const CLASSIC_LEN: usize = 110;
/// Indexed header length: 6 magic + 8 index digits + 2 padding.
const INDEXED_LEN: usize = 16;
/// Everything is aligned to this, counted from the start of the stream.
const ALIGN: usize = 4;
/// The name that ends the stream.
const TRAILER: &str = "TRAILER!!!";

/// Refuse a member claiming an implausible size, so a damaged or hostile
/// archive cannot allocate its way out of memory.
const MAX_MEMBER: u64 = 4 << 30;

/// A member's metadata, however the stream chose to express it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Meta {
    /// Normalized inner path (`/usr/bin/foo`).
    pub path: String,
    /// The full mode, type bits included.
    pub mode: u32,
    pub size: u64,
    pub mtime: Option<SystemTime>,
}

impl Meta {
    pub fn is_dir(&self) -> bool {
        self.mode & 0o170000 == 0o040000
    }

    /// The permission bits alone, which is what a panel shows.
    pub fn permissions(&self) -> u32 {
        self.mode & 0o7777
    }
}

/// What the next header in the stream turned out to be.
pub(super) enum Header {
    /// A classic member: its metadata came with it.
    Full(Meta),
    /// An indexed member: only its position in the package's file list.
    Indexed(u32),
    /// `TRAILER!!!`; the stream ends here.
    End,
}

/// Tracks how far into the stream we are, which is what the 4-byte padding is
/// measured against.
pub(super) struct Reader<R: Read> {
    inner: R,
    at: usize,
}

impl<R: Read> Reader<R> {
    pub fn new(inner: R) -> Self {
        Reader { inner, at: 0 }
    }

    /// Read the next member header, or `None` at a clean end of stream.
    pub fn next_header(&mut self) -> Result<Option<Header>> {
        let mut magic = [0u8; 6];
        if !self.fill_or_eof(&mut magic)? {
            return Ok(None);
        }
        match &magic {
            b"070701" | b"070702" => self.classic_header().map(Some),
            b"07070X" => {
                let mut rest = [0u8; INDEXED_LEN - 6];
                self.fill(&mut rest)?;
                let index = hex(&rest[..8])? as u32;
                Ok(Some(Header::Indexed(index)))
            }
            _ => Err(Error::other("not a cpio payload")),
        }
    }

    /// The rest of a classic header: the hex fields, then the name.
    fn classic_header(&mut self) -> Result<Header> {
        let mut fields = [0u8; CLASSIC_LEN - 6];
        self.fill(&mut fields)?;
        let field = |i: usize| hex(&fields[i * 8..(i + 1) * 8]);
        let (mode, mtime, size, namesize) = (field(1)?, field(5)?, field(6)?, field(11)?);
        if size > MAX_MEMBER || namesize > 4096 {
            return Err(Error::other("implausible cpio member"));
        }

        let mut name = vec![0u8; namesize as usize];
        self.fill(&mut name)?;
        self.pad()?;
        // The stored name includes its NUL terminator.
        let name = String::from_utf8_lossy(name.split(|b| *b == 0).next().unwrap_or(&[]));
        if name == TRAILER {
            return Ok(Header::End);
        }
        Ok(Header::Full(Meta {
            // RPM names members `./usr/bin/foo`; `normalize` eats the leading
            // `.` along with everything else awkward.
            path: normalize(&name),
            mode: mode as u32,
            size,
            mtime: SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(mtime)),
        }))
    }

    /// Read (or skip) `size` bytes of member data and the padding after it.
    pub fn data(&mut self, size: u64, want: bool) -> Result<Vec<u8>> {
        if size > MAX_MEMBER {
            return Err(Error::other("implausible cpio member"));
        }
        let out = if want {
            let mut buf = vec![0u8; size as usize];
            self.fill(&mut buf)?;
            buf
        } else {
            self.skip(size as usize)?;
            Vec::new()
        };
        self.pad()?;
        Ok(out)
    }

    /// Discard bytes up to the next 4-byte boundary.
    fn pad(&mut self) -> Result<()> {
        self.skip((ALIGN - self.at % ALIGN) % ALIGN)
    }

    fn fill(&mut self, buf: &mut [u8]) -> Result<()> {
        match self.fill_or_eof(buf)? {
            true => Ok(()),
            false => Err(Error::other("cpio payload ends mid-member")),
        }
    }

    /// Fill `buf`, or report a clean end of stream. Anything in between means
    /// the payload is damaged.
    ///
    /// An empty request succeeds without reading: a directory member has no
    /// bytes, and asking for none of them is not an end of stream.
    fn fill_or_eof(&mut self, buf: &mut [u8]) -> Result<bool> {
        if buf.is_empty() {
            return Ok(true);
        }
        let mut got = 0usize;
        while got < buf.len() {
            match self.inner.read(&mut buf[got..])? {
                0 => break,
                n => got += n,
            }
        }
        self.at += got;
        match got {
            0 => Ok(false),
            n if n == buf.len() => Ok(true),
            _ => Err(Error::other("cpio payload ends mid-member")),
        }
    }

    fn skip(&mut self, n: usize) -> Result<()> {
        if n == 0 {
            return Ok(());
        }
        let copied = std::io::copy(&mut self.inner.by_ref().take(n as u64), &mut std::io::sink())?;
        self.at += copied as usize;
        match copied as usize == n {
            true => Ok(()),
            false => Err(Error::other("cpio payload ends mid-member")),
        }
    }
}

fn hex(b: &[u8]) -> Result<u64> {
    let s = std::str::from_utf8(b).map_err(|_| Error::other("damaged cpio header"))?;
    u64::from_str_radix(s.trim(), 16).map_err(|_| Error::other("damaged cpio header"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One classic member, header and padding included.
    pub(super) fn classic(name: &str, mode: u32, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"070701");
        let namesize = name.len() + 1;
        // ino, mode, uid, gid, nlink, mtime, filesize, devmaj, devmin,
        // rdevmaj, rdevmin, namesize, check
        for f in [1, mode, 0, 0, 1, 0x5F00_0000, data.len() as u32, 0, 0, 0, 0, namesize as u32, 0]
        {
            v.extend_from_slice(format!("{f:08X}").as_bytes());
        }
        v.extend_from_slice(name.as_bytes());
        v.push(0);
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
        v.extend_from_slice(data);
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
        v
    }

    /// One indexed (`07070X`) member: sixteen bytes of header, then the data.
    pub(super) fn indexed(index: u32, data: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"07070X");
        v.extend_from_slice(format!("{index:08X}").as_bytes());
        v.extend_from_slice(&[0, 0]);
        v.extend_from_slice(data);
        while !v.len().is_multiple_of(4) {
            v.push(0);
        }
        v
    }

    pub(super) fn trailer() -> Vec<u8> {
        classic(TRAILER, 0, b"")
    }

    /// Walk a whole stream, for tests: every member's metadata and bytes.
    fn walk_classic(bytes: &[u8]) -> Vec<(Meta, Vec<u8>)> {
        let mut r = Reader::new(bytes);
        let mut out = Vec::new();
        while let Some(h) = r.next_header().unwrap() {
            match h {
                Header::End => break,
                Header::Full(m) => {
                    let data = r.data(m.size, true).unwrap();
                    out.push((m, data));
                }
                Header::Indexed(_) => panic!("not expected here"),
            }
        }
        out
    }

    #[test]
    fn walks_files_directories_and_symlinks() {
        let s = [
            classic("./usr/bin", 0o040755, b""),
            classic("./usr/bin/demo", 0o100755, b"#!/bin/sh\n"),
            classic("./usr/bin/alias", 0o120777, b"demo"),
            trailer(),
        ]
        .concat();
        let got = walk_classic(&s);

        assert_eq!(got.len(), 3, "the trailer ends the walk and is not a member");
        assert_eq!(got[0].0.path, "/usr/bin", "the leading ./ is normalized away");
        assert!(got[0].0.is_dir());
        assert_eq!(got[1].0.permissions(), 0o755, "mode carries permissions as well as type");
        assert_eq!(got[1].1, b"#!/bin/sh\n");
        assert_eq!(got[2].0.mode & 0o170000, 0o120000, "symlink type bits");
        assert_eq!(got[2].1, b"demo", "a symlink stores its target as its data");
    }

    /// Odd-length names and data pad to a 4-byte boundary counted from the
    /// start of the *stream*: getting that wrong desynchronises everything
    /// after the first awkward member.
    #[test]
    fn odd_lengths_stay_aligned() {
        let s = [
            classic("./a", 0o100644, b"x"),
            classic("./bb", 0o100644, b"yy"),
            classic("./ccc", 0o100644, b"zzz"),
            classic("./dddd", 0o100644, b"wwww"),
            trailer(),
        ]
        .concat();
        let names: Vec<String> = walk_classic(&s).into_iter().map(|(m, _)| m.path).collect();
        assert_eq!(names, ["/a", "/bb", "/ccc", "/dddd"]);
    }

    /// What rpm 4.14+ actually writes: sixteen-byte headers carrying only an
    /// index, then a classic trailer. Sizes have to come from the caller.
    #[test]
    fn an_indexed_payload_yields_indices_and_ends_on_a_classic_trailer() {
        let s = [indexed(0, b"#!/bin/sh\necho hi\n"), indexed(1, b"read me\n"), trailer()].concat();
        let sizes = [18u64, 8];
        let mut r = Reader::new(&s[..]);
        let mut got = Vec::new();
        while let Some(h) = r.next_header().unwrap() {
            match h {
                Header::End => break,
                Header::Indexed(i) => {
                    let data = r.data(sizes[i as usize], true).unwrap();
                    got.push((i, data));
                }
                Header::Full(_) => panic!("indexed payload"),
            }
        }
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], (0, b"#!/bin/sh\necho hi\n".to_vec()));
        assert_eq!(got[1], (1, b"read me\n".to_vec()));
    }

    /// A directory member has no data at all. Reading zero bytes must not look
    /// like the stream ending — that made every package containing a directory
    /// list fine but fail the moment a file was read out of it.
    #[test]
    fn a_zero_length_member_reads_as_empty_not_as_end_of_stream() {
        let s = [
            classic("./usr/bin", 0o040755, b""),
            classic("./usr/bin/demo", 0o100755, b"hi\n"),
            trailer(),
        ]
        .concat();
        let got = walk_classic(&s);
        assert_eq!(got.len(), 2, "the file after the empty directory is still reached");
        assert!(got[0].1.is_empty());
        assert_eq!(got[1].1, b"hi\n");
    }

    #[test]
    fn a_foreign_or_damaged_stream_is_refused() {
        let mut r = Reader::new(&b"not a cpio at all, really"[..]);
        assert!(r.next_header().is_err());
        // A header that stops halfway is damage, not a clean end.
        let s = classic("./a", 0o100644, b"x");
        let mut r = Reader::new(&s[..50]);
        assert!(r.next_header().is_err());
    }
}
