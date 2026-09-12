//! `iso://` VFS backend — browse an ISO 9660 disc image as a directory.
//!
//! Hand-written rather than pulled from a crate, for the same reason the trash,
//! the QR renderer and the 3D rasterizer are: the format is a short read of
//! fixed-offset fields, and the alternative is a dependency on something barely
//! maintained. It replaces a real external dependency — until now an `.iso`
//! opened through Midnight Commander's `iso9660` extfs script, which in turn
//! wants cdrkit's `isoinfo` installed.
//!
//! What is supported:
//!
//! * **ISO 9660** primary volume, directory tree, file extents.
//! * **Joliet** (a supplementary volume with a UCS-2 escape sequence), which is
//!   what carries long, mixed-case, non-ASCII names on most images. Preferred
//!   over the primary tree when present, because the primary one holds the
//!   mangled `README.TXT;1` form of the same names.
//! * **Rock Ridge** `NM` (the real POSIX name), `PX` (mode, so the executable
//!   bit and the directory bits survive), `SL` (symlink targets) and `CE`
//!   (continuation areas, without which a long Rock Ridge name is truncated).
//!
//! What is not: UDF (a `.iso` may be UDF-only, and then this declines and the
//! `rc.ext` rule gets its chance), multi-extent files above 4 GiB, and writing —
//! an image is read-only here.

use crate::util::{Error, Result};
use crate::vfs::membuf::MemReader;
use crate::vfs::tree::{self, Meta, TreeBuilder, TreeCache, VfsTree};
use crate::vfs::{BoxRead, BoxWrite, Capabilities, Vfs, VfsEntry, VfsKind, VfsPath, WriteMeta};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

/// Every offset in the format is in 2 KiB logical sectors.
const SECTOR: u64 = 2048;
/// Volume descriptors begin here; sectors 0–15 are the system area.
const FIRST_DESCRIPTOR: u64 = 16;
/// A malformed image must not be walked forever.
const MAX_DESCRIPTORS: u64 = 64;
const MAX_ENTRIES: usize = 200_000;
const MAX_DEPTH: usize = 32;

/// Where a file's bytes are: a starting sector and a length.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Extent {
    pub lba: u32,
    pub len: u32,
}

/// The two little-endian halves of a both-endian field.
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// A directory record's 7-byte timestamp: years since 1900, then month, day,
/// hour, minute, second, and a quarter-hour offset from GMT.
fn record_time(b: &[u8]) -> Option<SystemTime> {
    let (year, mon, day) = (1900 + b[0] as i64, b[1] as i64, b[2] as i64);
    let (h, m, s) = (b[3] as i64, b[4] as i64, b[5] as i64);
    if !(1..=12).contains(&mon) || !(1..=31).contains(&day) {
        return None;
    }
    // Days from the civil epoch — Howard Hinnant's algorithm, the same one
    // `util::bytes` uses in the other direction.
    let y = if mon <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mon + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    // The stored offset is local; `b[6]` is a signed count of quarter hours.
    let offset = (b[6] as i8) as i64 * 15 * 60;
    let secs = days * 86_400 + h * 3_600 + m * 60 + s - offset;
    u64::try_from(secs).ok().map(|s| SystemTime::UNIX_EPOCH + Duration::from_secs(s))
}

/// Strip the `;1` version suffix and any trailing `.` from an ISO 9660 name.
fn clean_name(raw: &str) -> String {
    let base = raw.split(';').next().unwrap_or(raw);
    base.strip_suffix('.').unwrap_or(base).to_string()
}

/// Decode a Joliet (UCS-2 big-endian) identifier.
fn ucs2_name(raw: &[u8]) -> String {
    let units: Vec<u16> = raw.as_chunks::<2>().0.iter().map(|c| u16::from_be_bytes(*c)).collect();
    clean_name(&String::from_utf16_lossy(&units))
}

/// One parsed directory record.
struct Record {
    len: usize,
    name: String,
    is_dir: bool,
    extent: Extent,
    mtime: Option<SystemTime>,
    /// Rock Ridge `PX` mode, when the image carries one.
    mode: Option<u32>,
    /// Rock Ridge `SL` target, for a symlink.
    symlink: Option<String>,
    /// `.` and `..` — walked over rather than listed.
    special: bool,
}

/// Parse one directory record at `b[0..]`. `None` ends the sector's records.
fn parse_record(b: &[u8], joliet: bool, img: &mut Image) -> Option<Record> {
    let len = *b.first()? as usize;
    if len < 33 || len > b.len() {
        return None;
    }
    let rec = &b[..len];
    let name_len = rec[32] as usize;
    if 33 + name_len > len {
        return None;
    }
    let raw = &rec[33..33 + name_len];
    let special = name_len == 1 && (raw[0] == 0 || raw[0] == 1);
    let mut name = if special {
        String::new()
    } else if joliet {
        ucs2_name(raw)
    } else {
        clean_name(&String::from_utf8_lossy(raw))
    };

    let flags = rec[25];
    let mut is_dir = flags & 0x02 != 0;
    let mut mode = None;
    let mut symlink = None;

    // The System Use area follows the identifier, padded to an even offset.
    let sys_at = 33 + name_len + usize::from(name_len.is_multiple_of(2));
    if sys_at < len {
        let mut su = rec[sys_at..].to_vec();
        // A `CE` entry points at more system-use data in another sector; without
        // following it a long Rock Ridge name comes back truncated.
        let mut nm_seen = false;
        let mut hops = 0;
        while let Some(ce) = rock_ridge(&su, &mut name, &mut nm_seen, &mut mode, &mut symlink) {
            hops += 1;
            if hops > 8 {
                break;
            }
            match img.read_at(ce.lba as u64 * SECTOR + ce.offset as u64, ce.len as usize) {
                Ok(more) => su = more,
                Err(_) => break,
            }
        }
    }
    if let Some(m) = mode {
        // Rock Ridge is authoritative about what a thing is; `S_IFDIR`.
        is_dir = m & 0o170000 == 0o040000 || is_dir;
        if m & 0o170000 == 0o120000 {
            is_dir = false;
        }
    }

    Some(Record {
        len,
        name,
        is_dir,
        extent: Extent { lba: le32(rec, 2), len: le32(rec, 10) },
        mtime: record_time(&rec[18..25]),
        mode,
        symlink,
        special,
    })
}

/// A pointer to a Rock Ridge continuation area.
struct Continue {
    lba: u32,
    offset: u32,
    len: u32,
}

/// Walk the Rock Ridge entries in a System Use area, filling in whatever they
/// say. Returns a continuation pointer when the entries run on into one.
fn rock_ridge(
    su: &[u8],
    name: &mut String,
    nm_seen: &mut bool,
    mode: &mut Option<u32>,
    symlink: &mut Option<String>,
) -> Option<Continue> {
    let mut i = 0;
    while i + 4 <= su.len() {
        let (sig, len) = (&su[i..i + 2], su[i + 2] as usize);
        if len < 4 || i + len > su.len() {
            break;
        }
        let data = &su[i + 4..i + len];
        match sig {
            // The real POSIX name. The first `NM` *replaces* the mangled
            // `README.TXT;1` form the directory record carries; any further ones
            // (flagged CONTINUE on the entry before) append to it, which is how
            // a name longer than one system-use area is carried.
            b"NM" if !data.is_empty() => {
                let text = String::from_utf8_lossy(&data[1..]);
                if !*nm_seen {
                    name.clear();
                    *nm_seen = true;
                }
                name.push_str(&text);
            }
            // POSIX attributes; the mode is the first both-endian u32.
            b"PX" if data.len() >= 8 => *mode = Some(le32(data, 0)),
            // A symlink's target, assembled from component records.
            b"SL" if data.len() > 1 => {
                let target = symlink.get_or_insert_with(String::new);
                let mut c = 1;
                while c + 2 <= data.len() {
                    let (cflags, clen) = (data[c], data[c + 1] as usize);
                    if c + 2 + clen > data.len() {
                        break;
                    }
                    if !target.is_empty() && !target.ends_with('/') {
                        target.push('/');
                    }
                    match cflags & 0x0e {
                        0x02 => target.push('.'),
                        0x04 => target.push_str(".."),
                        0x08 => target.push('/'),
                        _ => target.push_str(&String::from_utf8_lossy(&data[c + 2..c + 2 + clen])),
                    }
                    c += 2 + clen;
                }
            }
            b"CE" if data.len() >= 24 => {
                return Some(Continue {
                    lba: le32(data, 0),
                    offset: le32(data, 8),
                    len: le32(data, 16),
                });
            }
            // `ST` ends the system use area early.
            b"ST" => break,
            _ => {}
        }
        i += len;
    }
    None
}

/// The image file, read sector by sector.
struct Image {
    file: std::fs::File,
}

impl Image {
    fn read_at(&mut self, at: u64, len: usize) -> Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(at))?;
        let mut buf = vec![0u8; len];
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn sector(&mut self, lba: u64) -> Result<Vec<u8>> {
        self.read_at(lba * SECTOR, SECTOR as usize)
    }
}

/// The root directory record and whether its names are Joliet-encoded.
struct Volume {
    root: Extent,
    joliet: bool,
    mtime: Option<SystemTime>,
}

/// Read the volume descriptors and pick the tree to present.
fn read_volume(img: &mut Image) -> Result<Volume> {
    let mut primary: Option<Volume> = None;
    let mut joliet: Option<Volume> = None;
    for i in 0..MAX_DESCRIPTORS {
        let Ok(s) = img.sector(FIRST_DESCRIPTOR + i) else { break };
        if &s[1..6] != b"CD001" {
            // Not an ISO 9660 image at all — a UDF-only `.iso`, say. Declining
            // here is what lets an `rc.ext` rule still have its chance.
            return Err(Error::other("not an ISO 9660 image"));
        }
        let root = Extent { lba: le32(&s, 158), len: le32(&s, 166) };
        let v = Volume { root, joliet: false, mtime: None };
        match s[0] {
            1 => primary = Some(v),
            // A supplementary volume whose escape sequence selects UCS-2 is
            // Joliet: the same tree, with the names people actually gave.
            2 if matches!(&s[88..91], b"%/@" | b"%/C" | b"%/E") => {
                joliet = Some(Volume { joliet: true, ..v });
            }
            255 => break,
            _ => {}
        }
    }
    joliet.or(primary).ok_or_else(|| Error::other("no ISO 9660 volume descriptor"))
}

/// Walk one directory's extent, grafting what it holds onto the tree.
#[allow(clippy::too_many_arguments)]
fn walk(
    img: &mut Image,
    b: &mut TreeBuilder<Extent>,
    at: Extent,
    prefix: &str,
    joliet: bool,
    depth: usize,
    seen: &mut Vec<u32>,
) -> Result<()> {
    if depth > MAX_DEPTH || b.entry_count() > MAX_ENTRIES {
        return Ok(());
    }
    // A directory chain that loops back on itself must not be walked forever.
    if seen.contains(&at.lba) {
        return Ok(());
    }
    seen.push(at.lba);

    let sectors = at.len.div_ceil(SECTOR as u32).max(1);
    let mut children: Vec<(String, Extent)> = Vec::new();
    for s in 0..sectors as u64 {
        let Ok(data) = img.sector(at.lba as u64 + s) else { break };
        let mut i = 0usize;
        while i < data.len() {
            if data[i] == 0 {
                // The rest of the sector is padding; records never straddle one.
                break;
            }
            let Some(rec) = parse_record(&data[i..], joliet, img) else { break };
            i += rec.len;
            if rec.special || rec.name.is_empty() {
                continue;
            }
            let path = format!("{prefix}/{}", rec.name);
            let kind = if rec.symlink.is_some() {
                VfsKind::Symlink
            } else if rec.is_dir {
                VfsKind::Dir
            } else {
                VfsKind::File
            };
            let meta = Meta {
                mtime: rec.mtime,
                // Only the permission bits; the type bits are carried by `kind`.
                mode: rec.mode.map(|m| m & 0o7777),
                symlink_target: rec.symlink.clone(),
            };
            b.insert(&path, kind, u64::from(rec.extent.len), meta, rec.extent);
            if rec.is_dir && rec.symlink.is_none() {
                children.push((path, rec.extent));
            }
        }
    }
    for (path, extent) in children {
        walk(img, b, extent, &path, joliet, depth + 1, seen)?;
    }
    Ok(())
}

/// Parse a whole image into a browsable tree.
fn build(container: &Path) -> Result<VfsTree<Extent>> {
    let file = std::fs::File::open(container)?;
    let mtime = file.metadata().ok().and_then(|m| m.modified().ok());
    let mut img = Image { file };
    let vol = read_volume(&mut img)?;
    let mut b = TreeBuilder::<Extent>::new(vol.mtime.or(mtime));
    walk(&mut img, &mut b, vol.root, "", vol.joliet, 0, &mut Vec::new())?;
    Ok(b.finish())
}

/// Whether `path` looks like an ISO 9660 image — the probe that decides whether
/// this backend claims a file. A `.iso` that is UDF-only fails it, and falls
/// through to whatever `rc.ext` says instead.
pub fn looks_like_iso(path: &Path) -> bool {
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    if file.seek(SeekFrom::Start(FIRST_DESCRIPTOR * SECTOR)).is_err() {
        return false;
    }
    let mut magic = [0u8; 6];
    file.read_exact(&mut magic).is_ok() && &magic[1..6] == b"CD001"
}

/// A disc image, browsed as a directory.
pub struct IsoFs {
    /// Keyed on the image's `(mtime, len)`, like the archive backend's — an
    /// mtime alone is not enough on a filesystem with coarse timestamps.
    cache: TreeCache<(Option<SystemTime>, u64), Extent>,
}

impl IsoFs {
    pub fn new() -> Self {
        IsoFs { cache: TreeCache::new() }
    }

    async fn tree(&self, container: &Path) -> Result<Arc<VfsTree<Extent>>> {
        let stamp = tree::stamp_mtime_len(container).await;
        let path = container.to_path_buf();
        self.cache
            .get_or_build(container, stamp, || async move {
                tokio::task::spawn_blocking(move || build(&path))
                    .await
                    .map_err(|e| Error::other(e.to_string()))?
            })
            .await
    }
}

impl Default for IsoFs {
    fn default() -> Self {
        Self::new()
    }
}

fn container_of(path: &VfsPath) -> Result<&PathBuf> {
    path.container.as_ref().ok_or_else(|| Error::InvalidPath("not an iso path".to_string()))
}

#[async_trait::async_trait]
impl Vfs for IsoFs {
    fn scheme(&self) -> &str {
        "iso"
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            // Rock Ridge carries both, when the image has it.
            permissions: true,
            symlinks: true,
            // Extents are contiguous, so a read is a seek and a read.
            random_access: true,
            ..Capabilities::read_only()
        }
    }

    async fn read_dir(&self, dir: &VfsPath) -> Result<Vec<VfsEntry>> {
        self.tree(container_of(dir)?).await?.read_dir(&dir.posix_path())
    }

    async fn stat(&self, path: &VfsPath) -> Result<VfsEntry> {
        self.tree(container_of(path)?).await?.stat(&path.posix_path())
    }

    async fn open_read(&self, path: &VfsPath) -> Result<BoxRead> {
        let container = container_of(path)?.clone();
        let t = self.tree(&container).await?;
        let child = t.child(&path.posix_path())?;
        if child.kind.is_dir() {
            return Err(Error::other(format!("\"{}\" is a directory", child.name)));
        }
        let at = child.payload;
        let data = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
            let mut img = Image { file: std::fs::File::open(&container)? };
            img.read_at(at.lba as u64 * SECTOR, at.len as usize)
        })
        .await
        .map_err(|e| Error::other(e.to_string()))??;
        Ok(Box::new(MemReader::new(data)))
    }

    async fn read_link(&self, path: &VfsPath) -> Result<String> {
        let t = self.tree(container_of(path)?).await?;
        let child = t.child(&path.posix_path())?;
        child
            .symlink_target
            .clone()
            .ok_or_else(|| Error::other(format!("\"{}\" is not a symlink", child.name)))
    }

    async fn open_write(&self, _path: &VfsPath, _meta: WriteMeta) -> Result<BoxWrite> {
        Err(Error::Unsupported)
    }
    async fn mkdir(&self, _path: &VfsPath) -> Result<()> {
        Err(Error::Unsupported)
    }
    async fn remove_file(&self, _path: &VfsPath) -> Result<()> {
        Err(Error::Unsupported)
    }
    async fn remove_dir(&self, _path: &VfsPath) -> Result<()> {
        Err(Error::Unsupported)
    }
    async fn rename(&self, _from: &VfsPath, _to: &VfsPath) -> Result<()> {
        Err(Error::Unsupported)
    }
}

#[cfg(test)]
mod tests;
