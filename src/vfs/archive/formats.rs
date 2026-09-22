//! Per-format archive adapters: list metadata, read one entry, read/write the
//! whole archive (used for create / add / remove via rebuild).
//!
//! Everything here is synchronous and meant to run on a blocking thread.

use crate::util::{Error, Result};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// Message used when RAR support was compiled out (`--no-default-features`).
#[cfg(not(feature = "rar"))]
const RAR_DISABLED: &str = "RAR support is not compiled into this build";

/// Supported archive formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    Tar,
    TarGz,
    TarBz2,
    TarXz,
    /// Zstandard-compressed tar: `.tar.zst`, `.tzst`, and every Arch package.
    TarZst,
    /// A single Zstandard-compressed file (`foo.log.zst`), browsed as a
    /// one-member pseudo-archive so the decompressed file can be copied out.
    Zst,
    /// A Debian package: an `ar` container of two tarballs. Read-only.
    Deb,
    /// An RPM package: headers followed by a compressed cpio payload.
    /// Read-only.
    Rpm,
    SevenZ,
    Rar,
}

impl ArchiveFormat {
    /// Detect the format from a file name's extension.
    pub fn from_name(name: &str) -> Option<ArchiveFormat> {
        let n = name.to_ascii_lowercase();
        if n.ends_with(".tar.gz") || n.ends_with(".tgz") {
            Some(ArchiveFormat::TarGz)
        } else if n.ends_with(".tar.bz2") || n.ends_with(".tbz2") || n.ends_with(".tbz") {
            Some(ArchiveFormat::TarBz2)
        } else if n.ends_with(".tar.xz") || n.ends_with(".txz") {
            Some(ArchiveFormat::TarXz)
        } else if n.ends_with(".tar.zst") || n.ends_with(".tzst") {
            Some(ArchiveFormat::TarZst)
        } else if n.ends_with(".tar") {
            Some(ArchiveFormat::Tar)
        } else if n.ends_with(".zip") {
            Some(ArchiveFormat::Zip)
        } else if n.ends_with(".7z") {
            Some(ArchiveFormat::SevenZ)
        } else if n.ends_with(".rar") {
            Some(ArchiveFormat::Rar)
        } else if n.ends_with(".deb") || n.ends_with(".ddeb") || n.ends_with(".udeb") {
            Some(ArchiveFormat::Deb)
        } else if n.ends_with(".rpm") || n.ends_with(".srpm") {
            Some(ArchiveFormat::Rpm)
        } else if n.ends_with(".zst") {
            // Only reached when it is not a `.tar.zst`, which is tested above.
            Some(ArchiveFormat::Zst)
        } else {
            None
        }
    }

    pub fn from_path(path: &Path) -> Option<ArchiveFormat> {
        path.file_name().and_then(|n| n.to_str()).and_then(ArchiveFormat::from_name)
    }

    /// Whether files can be added to / removed from this format (via rebuild).
    ///
    /// A plain `.zst` holds exactly one unnamed stream, so "add a file to it"
    /// has no meaning; rar is read-only because the decoder is.
    pub fn writable(self) -> bool {
        !matches!(
            self,
            ArchiveFormat::Rar | ArchiveFormat::Zst | ArchiveFormat::Deb | ArchiveFormat::Rpm
        )
    }

    /// Whether the format records a unix permission mode per member. 7z stores
    /// Windows attributes instead, and rar is read-only here.
    pub fn stores_mode(self) -> bool {
        matches!(
            self,
            ArchiveFormat::Zip
                | ArchiveFormat::Tar
                | ArchiveFormat::TarGz
                | ArchiveFormat::TarBz2
                | ArchiveFormat::TarXz
                | ArchiveFormat::TarZst
                | ArchiveFormat::Deb
                | ArchiveFormat::Rpm
        )
    }
}

/// One archive member: normalized inner path, dir flag, uncompressed size, and
/// whatever per-member metadata the format carries (so the panel can show a
/// member's own timestamp rather than the container's).
pub struct RawEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    pub mtime: Option<SystemTime>,
    /// Unix permission bits, for the formats that record them (zip, tar).
    pub mode: Option<u32>,
}

/// A member with its bytes (used for rebuild). The metadata rides along so a
/// rebuild — which rewrites *every* member, not just the ones being changed —
/// gives the untouched members back their original timestamps and modes.
#[derive(Clone)]
pub struct FullEntry {
    pub path: String,
    pub is_dir: bool,
    pub data: Vec<u8>,
    pub mtime: Option<SystemTime>,
    pub mode: Option<u32>,
}

impl FullEntry {
    /// A file member with no recorded metadata.
    pub fn file(path: impl Into<String>, data: Vec<u8>) -> Self {
        FullEntry { path: path.into(), is_dir: false, data, mtime: None, mode: None }
    }

    /// A directory member with no recorded metadata.
    pub fn dir(path: impl Into<String>) -> Self {
        FullEntry { path: path.into(), is_dir: true, data: Vec::new(), mtime: None, mode: None }
    }

    /// Attach the timestamp/mode this member should be written with.
    pub fn with_meta(mut self, mtime: Option<SystemTime>, mode: Option<u32>) -> Self {
        self.mtime = mtime;
        self.mode = mode;
        self
    }
}

/// Normalize an archive member path to `/a/b` form: backslashes become slashes,
/// empty and `.` components are dropped, and `..` pops the previous component
/// without ever climbing past the archive root.
///
/// Clamping `..` is what keeps a hostile archive (a member named `../../etc/x`,
/// the "zip slip") from showing up as a `..` entry in the panel — where it would
/// collide with the parent link — and from writing outside the destination
/// directory when extracted.
pub fn normalize(name: &str) -> String {
    let replaced = name.replace('\\', "/");
    let mut comps: Vec<&str> = Vec::new();
    for comp in replaced.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                comps.pop();
            }
            c => comps.push(c),
        }
    }
    if comps.is_empty() { "/".to_string() } else { format!("/{}", comps.join("/")) }
}

pub(super) fn io<E: std::fmt::Display>(e: E) -> Error {
    Error::other(e.to_string())
}

// ---------------------------------------------------------------------------
// Timestamp conversions
// ---------------------------------------------------------------------------

/// Split a `SystemTime` into UTC civil fields (year, month, day, hour, minute,
/// second). `None` for times before the Unix epoch, which none of the archive
/// formats we write can represent anyway.
fn civil_from_time(t: SystemTime) -> Option<(i64, u32, u32, u32, u32, u32)> {
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).ok()?.as_secs() as i64;
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    Some((y, m, d, (rem / 3600) as u32, ((rem % 3600) / 60) as u32, (rem % 60) as u32))
}

/// Rebuild a `SystemTime` from UTC civil fields.
fn time_from_civil(y: i64, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> Option<SystemTime> {
    let days = days_from_civil(y, mo, d);
    let secs = days.checked_mul(86_400)?.checked_add((h * 3600 + mi * 60 + s) as i64)?;
    let secs = u64::try_from(secs).ok()?;
    SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(secs))
}

/// Days since 1970-01-01 for a proleptic-Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64; // March-based month
    let doy = (153 * mp + 2) / 5 + d as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Decode a packed MS-DOS date/time word pair, the form rar records a member's
/// timestamp in. It is nominally local time with no zone recorded; we read it as
/// UTC, which leaves someone else's archive off by their zone offset — a
/// limitation of the format, not of this code. (Zip stores the same encoding,
/// but the zip crate unpacks it for us.)
#[cfg(feature = "rar")]
fn time_from_msdos(datepart: u16, timepart: u16) -> Option<SystemTime> {
    time_from_civil(
        (datepart >> 9) as i64 + 1980,
        ((datepart >> 5) & 0xF) as u32,
        (datepart & 0x1F) as u32,
        (timepart >> 11) as u32,
        ((timepart >> 5) & 0x3F) as u32,
        ((timepart & 0x1F) * 2) as u32,
    )
}

/// A `SystemTime` as a zip `DateTime`, or `None` when it falls outside the
/// MS-DOS range (1980-2107) the zip format can store.
fn zip_datetime(t: SystemTime) -> Option<zip::DateTime> {
    let (y, mo, d, h, mi, s) = civil_from_time(t)?;
    let y = u16::try_from(y).ok()?;
    zip::DateTime::from_date_and_time(y, mo as u8, d as u8, h as u8, mi as u8, s as u8).ok()
}

/// The `SystemTime` a zip `DateTime` stands for.
fn zip_mtime(dt: zip::DateTime) -> Option<SystemTime> {
    time_from_civil(
        dt.year() as i64,
        dt.month() as u32,
        dt.day() as u32,
        dt.hour() as u32,
        dt.minute() as u32,
        dt.second() as u32,
    )
}

/// Seconds since the Unix epoch, for tar's numeric mtime field.
fn unix_secs(t: SystemTime) -> Option<u64> {
    t.duration_since(SystemTime::UNIX_EPOCH).ok().map(|d| d.as_secs())
}

// ---------------------------------------------------------------------------
// Listing (metadata only)
// ---------------------------------------------------------------------------

pub fn list_entries(format: ArchiveFormat, container: &Path) -> Result<Vec<RawEntry>> {
    match format {
        ArchiveFormat::Zip => list_zip(container),
        ArchiveFormat::Tar
        | ArchiveFormat::TarGz
        | ArchiveFormat::TarBz2
        | ArchiveFormat::TarXz
        | ArchiveFormat::TarZst => list_tar(format, container),
        ArchiveFormat::Zst => list_zst(container),
        ArchiveFormat::Deb => super::deb::list_deb(container),
        ArchiveFormat::Rpm => super::rpm::list_rpm(container),
        ArchiveFormat::SevenZ => list_7z(container),
        ArchiveFormat::Rar => list_rar(container),
    }
}

fn list_zip(container: &Path) -> Result<Vec<RawEntry>> {
    let mut za = zip::ZipArchive::new(File::open(container)?).map_err(io)?;
    let mut out = Vec::with_capacity(za.len());
    for i in 0..za.len() {
        let f = za.by_index(i).map_err(io)?;
        out.push(RawEntry {
            path: normalize(f.name()),
            is_dir: f.is_dir(),
            size: f.size(),
            mtime: f.last_modified().and_then(zip_mtime),
            mode: f.unix_mode(),
        });
    }
    Ok(out)
}

fn list_tar(format: ArchiveFormat, container: &Path) -> Result<Vec<RawEntry>> {
    let reader = tar_reader(format, File::open(container)?)?;
    let mut ar = tar::Archive::new(reader);
    let mut out = Vec::new();
    for e in ar.entries().map_err(io)? {
        let e = e.map_err(io)?;
        let path = e.path().map_err(io)?.to_string_lossy().into_owned();
        let is_dir = e.header().entry_type().is_dir();
        let size = e.header().size().unwrap_or(0);
        out.push(RawEntry {
            path: normalize(&path),
            is_dir,
            size,
            mtime: tar_mtime(e.header()),
            mode: e.header().mode().ok(),
        });
    }
    Ok(out)
}

fn list_7z(container: &Path) -> Result<Vec<RawEntry>> {
    let archive = sevenz_rust2::Archive::open(container).map_err(io)?;
    Ok(archive
        .files
        .iter()
        .map(|f| RawEntry {
            path: normalize(f.name()),
            is_dir: f.is_directory(),
            size: f.size(),
            mtime: sevenz_mtime(f),
            mode: None, // 7z records Windows attributes, not a unix mode
        })
        .collect())
}

#[cfg(not(feature = "rar"))]
fn list_rar(_container: &Path) -> Result<Vec<RawEntry>> {
    Err(Error::other(RAR_DISABLED))
}

#[cfg(feature = "rar")]
fn list_rar(container: &Path) -> Result<Vec<RawEntry>> {
    let archive = unrar::Archive::new(container).open_for_listing().map_err(io)?;
    let mut out = Vec::new();
    for entry in archive {
        let e = entry.map_err(io)?;
        out.push(RawEntry {
            path: normalize(&e.filename.to_string_lossy()),
            is_dir: e.is_directory(),
            size: e.unpacked_size,
            // `file_time` is a packed MS-DOS date/time pair (date in the high word).
            mtime: time_from_msdos((e.file_time >> 16) as u16, e.file_time as u16),
            mode: None,
        });
    }
    Ok(out)
}

/// A tar header's mtime as a `SystemTime` (the header stores whole seconds
/// since the Unix epoch).
fn tar_mtime(header: &tar::Header) -> Option<SystemTime> {
    header.mtime().ok().and_then(|s| SystemTime::UNIX_EPOCH.checked_add(Duration::from_secs(s)))
}

/// A 7z member's mtime, when it recorded one.
fn sevenz_mtime(f: &sevenz_rust2::ArchiveEntry) -> Option<SystemTime> {
    f.has_last_modified_date.then(|| f.last_modified_date.into())
}

// ---------------------------------------------------------------------------
// Reading a single entry
// ---------------------------------------------------------------------------

/// Read one member identified by its normalized inner path (`/a/b`).
pub fn read_entry(format: ArchiveFormat, container: &Path, inner: &str) -> Result<Vec<u8>> {
    let target = normalize(inner);
    match format {
        ArchiveFormat::Zip => {
            let mut za = zip::ZipArchive::new(File::open(container)?).map_err(io)?;
            for i in 0..za.len() {
                let mut f = za.by_index(i).map_err(io)?;
                if normalize(f.name()) == target {
                    let mut data = Vec::new();
                    f.read_to_end(&mut data)?;
                    return Ok(data);
                }
            }
            Err(Error::NotFound(target))
        }
        ArchiveFormat::Tar
        | ArchiveFormat::TarGz
        | ArchiveFormat::TarBz2
        | ArchiveFormat::TarXz
        | ArchiveFormat::TarZst => {
            let reader = tar_reader(format, File::open(container)?)?;
            let mut ar = tar::Archive::new(reader);
            for e in ar.entries().map_err(io)? {
                let mut e = e.map_err(io)?;
                let path = e.path().map_err(io)?.to_string_lossy().into_owned();
                if normalize(&path) == target {
                    let mut data = Vec::new();
                    e.read_to_end(&mut data)?;
                    return Ok(data);
                }
            }
            Err(Error::NotFound(target))
        }
        ArchiveFormat::Zst => {
            if target != zst_member_path(container) {
                return Err(Error::NotFound(target));
            }
            read_zst(container)
        }
        ArchiveFormat::Deb => super::deb::read_deb_entry(container, &target),
        ArchiveFormat::Rpm => super::rpm::read_rpm_entry(container, &target),
        ArchiveFormat::SevenZ => {
            let archive = sevenz_rust2::Archive::open(container).map_err(io)?;
            let name = archive
                .files
                .iter()
                .find(|f| normalize(f.name()) == target)
                .map(|f| f.name().to_string())
                .ok_or_else(|| Error::NotFound(target.clone()))?;
            let mut reader =
                sevenz_rust2::ArchiveReader::open(container, sevenz_rust2::Password::empty())
                    .map_err(io)?;
            reader.read_file(&name).map_err(io)
        }
        ArchiveFormat::Rar => read_rar_entry(container, &target),
    }
}

#[cfg(not(feature = "rar"))]
fn read_rar_entry(_container: &Path, _target: &str) -> Result<Vec<u8>> {
    Err(Error::other(RAR_DISABLED))
}

#[cfg(feature = "rar")]
fn read_rar_entry(container: &Path, target: &str) -> Result<Vec<u8>> {
    let mut ar = unrar::Archive::new(container).open_for_processing().map_err(io)?;
    while let Some(header) = ar.read_header().map_err(io)? {
        let name = normalize(&header.entry().filename.to_string_lossy());
        if name == target {
            let (data, _next) = header.read().map_err(io)?;
            return Ok(data);
        }
        ar = header.skip().map_err(io)?;
    }
    Err(Error::NotFound(target.to_string()))
}

// ---------------------------------------------------------------------------
// Whole-archive read (for rebuild)
// ---------------------------------------------------------------------------

pub fn read_all(format: ArchiveFormat, container: &Path) -> Result<Vec<FullEntry>> {
    match format {
        ArchiveFormat::Zip => {
            let mut za = zip::ZipArchive::new(File::open(container)?).map_err(io)?;
            let mut out = Vec::with_capacity(za.len());
            for i in 0..za.len() {
                let mut f = za.by_index(i).map_err(io)?;
                let is_dir = f.is_dir();
                let path = normalize(f.name());
                let mtime = f.last_modified().and_then(zip_mtime);
                let mode = f.unix_mode();
                let mut data = Vec::new();
                if !is_dir {
                    f.read_to_end(&mut data)?;
                }
                out.push(FullEntry { path, is_dir, data, mtime, mode });
            }
            Ok(out)
        }
        ArchiveFormat::Tar
        | ArchiveFormat::TarGz
        | ArchiveFormat::TarBz2
        | ArchiveFormat::TarXz
        | ArchiveFormat::TarZst => {
            let reader = tar_reader(format, File::open(container)?)?;
            let mut ar = tar::Archive::new(reader);
            let mut out = Vec::new();
            for e in ar.entries().map_err(io)? {
                let mut e = e.map_err(io)?;
                let is_dir = e.header().entry_type().is_dir();
                let path = normalize(&e.path().map_err(io)?.to_string_lossy());
                let mtime = tar_mtime(e.header());
                let mode = e.header().mode().ok();
                let mut data = Vec::new();
                if !is_dir {
                    e.read_to_end(&mut data)?;
                }
                out.push(FullEntry { path, is_dir, data, mtime, mode });
            }
            Ok(out)
        }
        ArchiveFormat::Zst => {
            Ok(vec![FullEntry::file(zst_member_path(container), read_zst(container)?)])
        }
        ArchiveFormat::Deb => super::deb::read_deb_all(container),
        ArchiveFormat::Rpm => super::rpm::read_rpm_all(container),
        ArchiveFormat::SevenZ => {
            let archive = sevenz_rust2::Archive::open(container).map_err(io)?;
            let mut reader =
                sevenz_rust2::ArchiveReader::open(container, sevenz_rust2::Password::empty())
                    .map_err(io)?;
            let mut out = Vec::new();
            for f in &archive.files {
                let is_dir = f.is_directory();
                let data =
                    if is_dir { Vec::new() } else { reader.read_file(f.name()).map_err(io)? };
                out.push(FullEntry {
                    path: normalize(f.name()),
                    is_dir,
                    data,
                    mtime: sevenz_mtime(f),
                    mode: None,
                });
            }
            Ok(out)
        }
        ArchiveFormat::Rar => read_rar_all(container),
    }
}

#[cfg(not(feature = "rar"))]
fn read_rar_all(_container: &Path) -> Result<Vec<FullEntry>> {
    Err(Error::other(RAR_DISABLED))
}

#[cfg(feature = "rar")]
fn read_rar_all(container: &Path) -> Result<Vec<FullEntry>> {
    let mut ar = unrar::Archive::new(container).open_for_processing().map_err(io)?;
    let mut out = Vec::new();
    while let Some(header) = ar.read_header().map_err(io)? {
        let e = header.entry();
        let path = normalize(&e.filename.to_string_lossy());
        let is_dir = e.is_directory();
        let mtime = time_from_msdos((e.file_time >> 16) as u16, e.file_time as u16);
        if is_dir {
            out.push(FullEntry { path, is_dir, data: Vec::new(), mtime, mode: None });
            ar = header.skip().map_err(io)?;
        } else {
            let (data, next) = header.read().map_err(io)?;
            out.push(FullEntry { path, is_dir, data, mtime, mode: None });
            ar = next;
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Whole-archive write (create / rebuild). `dest` is the file to (over)write.
// ---------------------------------------------------------------------------

/// The members to actually write, keyed by normalized name (leading `/`
/// stripped, so it is the name as the format stores it).
///
/// Two members can't share a name: a zip writer rejects the second outright,
/// while tar and 7z would happily store both and leave readers disagreeing
/// about which one wins. So a later member *replaces* an earlier one of the
/// same name, keeping the earlier one's position — which is what makes
/// "copy a file into an archive that already has it" an overwrite rather than
/// an error or a silent shadow.
fn members(entries: &[FullEntry]) -> Vec<(String, &FullEntry)> {
    let mut at: HashMap<String, usize> = HashMap::new();
    let mut out: Vec<(String, &FullEntry)> = Vec::new();
    for e in entries {
        let norm = normalize(&e.path);
        if norm == "/" {
            continue; // the archive root is not a member
        }
        let name = norm[1..].to_string();
        match at.get(&name) {
            Some(&i) => out[i].1 = e,
            None => {
                at.insert(name.clone(), out.len());
                out.push((name, e));
            }
        }
    }
    out
}

pub fn write_all(format: ArchiveFormat, dest: &Path, entries: &[FullEntry]) -> Result<()> {
    let members = members(entries);
    match format {
        ArchiveFormat::Zip => write_zip(dest, &members),
        ArchiveFormat::Tar => {
            let f = File::create(dest)?;
            build_tar(f, &members)?.flush()?;
            Ok(())
        }
        ArchiveFormat::TarGz => {
            let enc =
                flate2::write::GzEncoder::new(File::create(dest)?, flate2::Compression::default());
            build_tar(enc, &members)?.finish().map_err(io)?;
            Ok(())
        }
        ArchiveFormat::TarBz2 => {
            let enc =
                bzip2::write::BzEncoder::new(File::create(dest)?, bzip2::Compression::default());
            build_tar(enc, &members)?.finish().map_err(io)?;
            Ok(())
        }
        ArchiveFormat::TarXz => {
            let enc = xz2::write::XzEncoder::new(File::create(dest)?, 6);
            build_tar(enc, &members)?.finish().map_err(io)?;
            Ok(())
        }
        ArchiveFormat::TarZst => {
            let enc = zstd::stream::write::Encoder::new(File::create(dest)?, 3).map_err(io)?;
            build_tar(enc, &members)?.finish().map_err(io)?;
            Ok(())
        }
        ArchiveFormat::SevenZ => write_7z(dest, &members),
        ArchiveFormat::Rar => Err(Error::other("creating RAR archives is not supported")),
        ArchiveFormat::Zst => Err(Error::other("a plain .zst holds a single file, not an archive")),
        ArchiveFormat::Deb => Err(Error::other("creating Debian packages is not supported")),
        ArchiveFormat::Rpm => Err(Error::other("creating RPM packages is not supported")),
    }
}

fn write_zip(dest: &Path, members: &[(String, &FullEntry)]) -> Result<()> {
    let mut zw = zip::ZipWriter::new(File::create(dest)?);
    for (name, e) in members {
        let mut opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        if let Some(mode) = e.mode {
            opts = opts.unix_permissions(mode);
        }
        if let Some(dt) = e.mtime.and_then(zip_datetime) {
            opts = opts.last_modified_time(dt);
        }
        if e.is_dir {
            zw.add_directory(name.as_str(), opts).map_err(io)?;
        } else {
            zw.start_file(name.as_str(), opts).map_err(io)?;
            zw.write_all(&e.data)?;
        }
    }
    zw.finish().map_err(io)?;
    Ok(())
}

fn build_tar<W: Write>(w: W, members: &[(String, &FullEntry)]) -> Result<W> {
    let mut b = tar::Builder::new(w);
    for (name, e) in members {
        let mut header = tar::Header::new_gnu();
        if let Some(secs) = e.mtime.and_then(unix_secs) {
            header.set_mtime(secs);
        }
        if e.is_dir {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_mode(e.mode.unwrap_or(0o755) & 0o7777);
            let dir_name = format!("{name}/");
            header.set_cksum();
            b.append_data(&mut header, dir_name, std::io::empty()).map_err(io)?;
        } else {
            header.set_size(e.data.len() as u64);
            header.set_mode(e.mode.unwrap_or(0o644) & 0o7777);
            header.set_cksum();
            b.append_data(&mut header, name.as_str(), &e.data[..]).map_err(io)?;
        }
    }
    b.into_inner().map_err(io)
}

fn write_7z(dest: &Path, members: &[(String, &FullEntry)]) -> Result<()> {
    let mut w = sevenz_rust2::ArchiveWriter::create(dest).map_err(io)?;
    for (name, e) in members {
        let mut entry = if e.is_dir {
            sevenz_rust2::ArchiveEntry::new_directory(name)
        } else {
            sevenz_rust2::ArchiveEntry::new_file(name)
        };
        if let Some(t) = e.mtime
            && let Ok(nt) = sevenz_rust2::NtTime::try_from(t)
        {
            entry.last_modified_date = nt;
            entry.has_last_modified_date = true;
        }
        if e.is_dir {
            w.push_archive_entry::<&[u8]>(entry, None).map_err(io)?;
        } else {
            w.push_archive_entry(entry, Some(&e.data[..])).map_err(io)?;
        }
    }
    w.finish().map_err(io)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Plain .zst (a single compressed file, browsed as a one-member archive)
// ---------------------------------------------------------------------------

/// The inner path a plain `.zst` presents: the container's own name with the
/// suffix taken off, so `syslog.1.zst` holds `/syslog.1`.
fn zst_member_path(container: &Path) -> String {
    let name = container.file_name().and_then(|n| n.to_str()).unwrap_or("data");
    let stem = name.strip_suffix(".zst").or_else(|| name.strip_suffix(".ZST")).unwrap_or(name);
    normalize(stem)
}

fn list_zst(container: &Path) -> Result<Vec<RawEntry>> {
    let meta = std::fs::metadata(container)?;
    // The frame usually declares the decompressed size, and reading the header
    // is far cheaper than decompressing a multi-gigabyte log just to fill in a
    // column. When it doesn't, the size stays 0 rather than being guessed at.
    let mut head = [0u8; 18];
    let n = File::open(container)?.read(&mut head).unwrap_or(0);
    Ok(vec![RawEntry {
        path: zst_member_path(container),
        is_dir: false,
        size: zstd_frame_content_size(&head[..n]).unwrap_or(0),
        mtime: meta.modified().ok(),
        mode: None,
    }])
}

fn read_zst(container: &Path) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    zstd::stream::read::Decoder::new(File::open(container)?).map_err(io)?.read_to_end(&mut out)?;
    Ok(out)
}

/// The `Frame_Content_Size` a Zstandard frame header declares, if it declares
/// one (RFC 8878 section 3.1.1). `head` need only be the first bytes of the file.
///
/// Hand-parsed rather than taken from the library because `Decompressor::
/// upper_bound` sits behind zstd's `experimental` feature, and this is a dozen
/// well-specified bits — the same trade the ISO 9660 reader makes.
fn zstd_frame_content_size(head: &[u8]) -> Option<u64> {
    // Magic_Number 0xFD2FB528, stored little-endian.
    if head.len() < 5 || head[..4] != [0x28, 0xB5, 0x2F, 0xFD] {
        return None;
    }
    let desc = head[4];
    let single_segment = desc & 0x20 != 0;
    let did_size = match desc & 0x03 {
        0 => 0,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    let fcs_size = match desc >> 6 {
        // 0 means "one byte, and only when there is no Window_Descriptor".
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    if fcs_size == 0 {
        return None;
    }
    // Window_Descriptor is present only while Single_Segment_flag is clear.
    let at = 5 + usize::from(!single_segment) + did_size;
    let bytes = head.get(at..at + fcs_size)?;
    let v = bytes.iter().enumerate().fold(0u64, |a, (i, b)| a | u64::from(*b) << (8 * i));
    // The two-byte form is stored biased by 256.
    Some(if fcs_size == 2 { v + 256 } else { v })
}

// ---------------------------------------------------------------------------
// tar decompression reader
// ---------------------------------------------------------------------------

/// Which compressor wraps a tar stream.
///
/// Split out from [`ArchiveFormat`] because a `.deb` names the compression of
/// its inner tarballs in their own member names (`data.tar.zst`), so the same
/// four decoders have to be reachable without a container format to go with
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Comp {
    None,
    Gz,
    Bz2,
    Xz,
    Zst,
}

impl Comp {
    /// The compressor a `.deb`'s inner tarball name implies.
    pub(super) fn from_tar_name(name: &str) -> Option<Comp> {
        let n = name.to_ascii_lowercase();
        // `.tar` last: every other suffix also ends in `.tar.<something>`.
        for (suffix, comp) in [
            (".gz", Comp::Gz),
            (".bz2", Comp::Bz2),
            (".xz", Comp::Xz),
            (".zst", Comp::Zst),
            ("", Comp::None),
        ] {
            if n.ends_with(&format!(".tar{suffix}")) {
                return Some(comp);
            }
        }
        None
    }
}

/// Wrap `r` in the matching decompressor.
pub(super) fn decompress<R: Read + 'static>(comp: Comp, r: R) -> Result<Box<dyn Read>> {
    Ok(match comp {
        Comp::None => Box::new(r),
        Comp::Gz => Box::new(flate2::read::GzDecoder::new(r)),
        Comp::Bz2 => Box::new(bzip2::read::BzDecoder::new(r)),
        Comp::Xz => Box::new(xz2::read::XzDecoder::new(r)),
        Comp::Zst => Box::new(zstd::stream::read::Decoder::new(r).map_err(io)?),
    })
}

fn tar_reader(format: ArchiveFormat, file: File) -> Result<Box<dyn Read>> {
    let comp = match format {
        ArchiveFormat::Tar => Comp::None,
        ArchiveFormat::TarGz => Comp::Gz,
        ArchiveFormat::TarBz2 => Comp::Bz2,
        ArchiveFormat::TarXz => Comp::Xz,
        ArchiveFormat::TarZst => Comp::Zst,
        _ => return Err(Error::other("not a tar format")),
    };
    decompress(comp, file)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `.tar.zst` has to be recognised before a bare `.zst`, or every Arch
    /// package would be taken for a single compressed file and show one
    /// meaningless member instead of its tree.
    #[test]
    fn zstd_names_resolve_tar_before_plain() {
        use ArchiveFormat::*;
        assert_eq!(ArchiveFormat::from_name("src.tar.zst"), Some(TarZst));
        assert_eq!(ArchiveFormat::from_name("src.tzst"), Some(TarZst));
        assert_eq!(ArchiveFormat::from_name("FOO.TAR.ZST"), Some(TarZst), "case-insensitive");
        assert_eq!(
            ArchiveFormat::from_name("rc-1.9.4-1-x86_64.pkg.tar.zst"),
            Some(TarZst),
            "an Arch package needs no special case; it already ends in .tar.zst"
        );
        assert_eq!(ArchiveFormat::from_name("syslog.1.zst"), Some(Zst), "a plain one");
    }

    /// A `.tar.zst` is rebuilt like any other tar, but a plain `.zst` holds one
    /// unnamed stream with nowhere to put a second member.
    #[test]
    fn only_the_tar_flavour_of_zstd_is_writable() {
        assert!(ArchiveFormat::TarZst.writable());
        assert!(ArchiveFormat::TarZst.stores_mode(), "tar carries a unix mode");
        assert!(!ArchiveFormat::Zst.writable());
        assert!(write_all(ArchiveFormat::Zst, Path::new("/nonexistent"), &[]).is_err());
    }

    /// The frame header's `Frame_Content_Size`, across the four field widths
    /// the format defines — including the two-byte form, which is stored biased
    /// by 256 and is the one easiest to get wrong.
    #[test]
    fn a_zstd_frame_header_yields_the_declared_size() {
        // desc bits: FCS_flag in 7-6, Single_Segment in 5, Dictionary_ID in 1-0.
        // Single_Segment set means no Window_Descriptor byte follows.
        let magic = [0x28, 0xB5, 0x2F, 0xFD];
        let frame = |desc: u8, rest: &[u8]| {
            let mut v = magic.to_vec();
            v.push(desc);
            v.extend_from_slice(rest);
            v
        };

        // FCS_flag 0 + Single_Segment: one byte, no window descriptor.
        assert_eq!(zstd_frame_content_size(&frame(0x20, &[42])), Some(42));
        // FCS_flag 1: two bytes, biased by 256.
        assert_eq!(zstd_frame_content_size(&frame(0x60, &[0x00, 0x00])), Some(256));
        assert_eq!(zstd_frame_content_size(&frame(0x60, &[0x01, 0x01])), Some(257 + 256));
        // FCS_flag 2: four bytes, little-endian.
        assert_eq!(zstd_frame_content_size(&frame(0xA0, &[0x40, 0x0D, 0x03, 0x00])), Some(200_000));
        // FCS_flag 3: eight bytes.
        let mut eight = vec![0u8; 8];
        eight[0] = 0xFF;
        assert_eq!(zstd_frame_content_size(&frame(0xE0, &eight)), Some(255));
        // Without Single_Segment a Window_Descriptor byte sits before the size.
        assert_eq!(zstd_frame_content_size(&frame(0x40, &[0x58, 0x00, 0x00])), Some(256));
        // A Dictionary_ID, when present, sits between the two.
        assert_eq!(zstd_frame_content_size(&frame(0x61, &[0x07, 0x00, 0x00])), Some(256));
    }

    /// Not every frame declares a size: `zstd::encode_all` streams without
    /// pledging one, and then the listing must say 0 rather than invent a
    /// number. Only a pledged stream (what the `zstd` tool writes for a file)
    /// carries it.
    #[test]
    fn an_unpledged_frame_declares_nothing_and_a_pledged_one_does() {
        let plain = zstd::encode_all(&b"0123456789"[..], 3).unwrap();
        assert_eq!(zstd_frame_content_size(&plain), None, "encode_all pledges nothing");

        let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
        enc.set_pledged_src_size(Some(10)).unwrap();
        enc.write_all(b"0123456789").unwrap();
        let pledged = enc.finish().unwrap();
        assert_eq!(zstd_frame_content_size(&pledged), Some(10));
        // Only the head is needed — the point is not to decompress a huge log.
        assert_eq!(zstd_frame_content_size(&pledged[..9.min(pledged.len())]), Some(10));
    }

    #[test]
    fn a_non_zstd_or_truncated_header_declares_nothing() {
        assert_eq!(zstd_frame_content_size(b""), None);
        assert_eq!(zstd_frame_content_size(b"not zstd at all"), None, "wrong magic");
        // Right magic, but the descriptor promises fields that were cut off.
        assert_eq!(zstd_frame_content_size(&[0x28, 0xB5, 0x2F, 0xFD, 0xE0, 0x01]), None);
    }

    #[test]
    fn a_plain_zst_member_is_named_after_its_container() {
        assert_eq!(zst_member_path(Path::new("/var/log/syslog.1.zst")), "/syslog.1");
        assert_eq!(zst_member_path(Path::new("dump.sql.zst")), "/dump.sql");
    }

    /// `..` never climbs out of the archive: a hostile member name is clamped to
    /// the root instead of becoming a `..` entry (which would collide with the
    /// panel's parent link) or escaping the extraction directory.
    #[test]
    fn normalize_clamps_traversal_and_strips_noise() {
        assert_eq!(normalize("a/b"), "/a/b");
        assert_eq!(normalize("./a/./b"), "/a/b");
        assert_eq!(normalize("/a//b/"), "/a/b");
        assert_eq!(normalize("a\\b"), "/a/b", "windows separators");
        assert_eq!(normalize("a/../b"), "/b");
        assert_eq!(normalize("../escape.txt"), "/escape.txt");
        assert_eq!(normalize("../../../etc/passwd"), "/etc/passwd");
        assert_eq!(normalize("a/../../b"), "/b", "cannot pop past the root");
        assert_eq!(normalize(".."), "/");
        assert_eq!(normalize(""), "/");
        assert_eq!(normalize("/"), "/");
    }

    /// The civil-date helpers round-trip, including across a leap day.
    #[test]
    fn civil_date_round_trips() {
        for (y, mo, d, h, mi, s) in [
            (1970, 1, 1, 0, 0, 0),
            (1980, 1, 1, 0, 0, 0),
            (2000, 2, 29, 12, 34, 56),
            (2024, 12, 31, 23, 59, 58),
            (2107, 6, 15, 1, 2, 4),
        ] {
            let t = time_from_civil(y, mo, d, h, mi, s).expect("representable");
            assert_eq!(civil_from_time(t), Some((y, mo, d, h, mi, s)), "{y}-{mo}-{d}");
        }
    }

    /// A duplicate member name is a replacement, not a second copy: the later
    /// entry's content wins and the earlier one's position is kept.
    #[test]
    fn members_replaces_duplicates_in_place() {
        let entries = vec![
            FullEntry::file("/a.txt", b"first".to_vec()),
            FullEntry::file("/b.txt", b"bee".to_vec()),
            FullEntry::file("a.txt", b"second".to_vec()),
        ];
        let m = members(&entries);
        let names: Vec<&str> = m.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a.txt", "b.txt"], "no duplicate, original order kept");
        assert_eq!(m[0].1.data, b"second", "the later member wins");
    }

    /// The archive root is never written as a member.
    #[test]
    fn members_drops_the_root() {
        let entries = vec![FullEntry::dir("/"), FullEntry::file("/a", b"x".to_vec())];
        assert_eq!(members(&entries).len(), 1);
    }
}
