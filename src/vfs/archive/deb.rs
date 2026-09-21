//! Debian packages (`.deb`) browsed like a directory.
//!
//! A `.deb` is a Unix `ar` container holding three members: `debian-binary` (a
//! version string), `control.tar.*` (the maintainer scripts and metadata) and
//! `data.tar.*` (the files the package installs). Both tarballs may be
//! compressed with gzip, xz or — on anything recent — zstd.
//!
//! **Layout.** The two tarballs are presented in *one* namespace rather than as
//! two members you step into: [`VfsPath`](crate::vfs::VfsPath) carries a single
//! container path, so an archive inside an archive cannot be expressed and
//! `data.tar.xz` would be a dead end. So the data tree sits at the root, the
//! control files go under `/DEBIAN` — exactly where `dpkg-deb -R` puts them, so
//! the layout is a convention rather than an invention — and `debian-binary`
//! stays at the top.
//!
//! Read-only: rebuilding a package means regenerating `md5sums`, keeping the
//! control fields consistent and honouring signing conventions, which is a
//! packaging tool's job rather than a file manager's.

use super::formats::{self, Comp, FullEntry, RawEntry, normalize};
use crate::util::{Error, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Where the control tarball's members are shown, matching `dpkg-deb -R`.
const CONTROL_DIR: &str = "/DEBIAN";

/// One `ar` member of interest: where its bytes are and how they are packed.
struct Member {
    offset: u64,
    size: u64,
    comp: Comp,
}

/// The three members a `.deb` is made of. `debian_binary` is read eagerly — it
/// is a handful of bytes and having it here keeps the listing code simple.
struct Deb {
    debian_binary: Option<Vec<u8>>,
    control: Option<Member>,
    data: Option<Member>,
}

/// Scan the `ar` container, without reading either tarball.
fn scan(container: &Path) -> Result<Deb> {
    let cache = object::read::ReadCache::new(File::open(container)?);
    let ar = object::read::archive::ArchiveFile::parse(&cache)
        .map_err(|e| Error::other(format!("not a Debian package: {e}")))?;
    let mut out = Deb { debian_binary: None, control: None, data: None };
    for m in ar.members() {
        let m = m.map_err(|e| Error::other(format!("damaged package: {e}")))?;
        let name = String::from_utf8_lossy(m.name()).into_owned();
        let (offset, size) = m.file_range();
        // A tarball whose compressor we cannot read is left out of the listing
        // rather than failing the whole package: the other half still opens.
        let comp = Comp::from_tar_name(&name);
        match name.as_str() {
            "debian-binary" => {
                out.debian_binary = Some(
                    m.data(&cache)
                        .map_err(|e| Error::other(format!("damaged package: {e}")))?
                        .to_vec(),
                );
            }
            n if n.starts_with("control.tar") => {
                out.control = comp.map(|comp| Member { offset, size, comp });
            }
            n if n.starts_with("data.tar") => {
                out.data = comp.map(|comp| Member { offset, size, comp });
            }
            _ => {}
        }
    }
    if out.data.is_none() && out.control.is_none() {
        return Err(Error::other("no readable data.tar or control.tar in the package"));
    }
    Ok(out)
}

/// A `tar::Archive` over one `ar` member, streamed rather than loaded: the
/// member is a slice of the file, so seek to it, cap the reader at its length,
/// and decompress through that.
fn tar_of(container: &Path, m: &Member) -> Result<tar::Archive<Box<dyn Read>>> {
    let mut f = File::open(container)?;
    f.seek(SeekFrom::Start(m.offset))?;
    Ok(tar::Archive::new(formats::decompress(m.comp, f.take(m.size))?))
}

/// Where a member of the control tarball is shown: `./control` becomes
/// `/DEBIAN/control`.
fn control_path(inner: &str) -> String {
    let n = normalize(inner);
    if n == "/" { CONTROL_DIR.to_string() } else { format!("{CONTROL_DIR}{n}") }
}

pub(super) fn list_deb(container: &Path) -> Result<Vec<RawEntry>> {
    let deb = scan(container)?;
    let mut out = Vec::new();
    if let Some(data) = &deb.debian_binary {
        out.push(RawEntry {
            path: "/debian-binary".to_string(),
            is_dir: false,
            size: data.len() as u64,
            mtime: None,
            mode: None,
        });
    }
    if let Some(m) = &deb.control {
        // The directory itself, so the panel has something to step into even
        // when the tarball lists only bare file names.
        out.push(RawEntry {
            path: CONTROL_DIR.to_string(),
            is_dir: true,
            size: 0,
            mtime: None,
            mode: None,
        });
        for e in tar_of(container, m)?.entries().map_err(super::formats::io)? {
            let e = e.map_err(super::formats::io)?;
            let path = e.path().map_err(super::formats::io)?.to_string_lossy().into_owned();
            let norm = control_path(&path);
            if norm == CONTROL_DIR {
                continue; // the `./` member: already added above
            }
            out.push(raw_from_tar(&e, norm));
        }
    }
    if let Some(m) = &deb.data {
        for e in tar_of(container, m)?.entries().map_err(super::formats::io)? {
            let e = e.map_err(super::formats::io)?;
            let path = e.path().map_err(super::formats::io)?.to_string_lossy().into_owned();
            let norm = normalize(&path);
            if norm == "/" {
                continue;
            }
            out.push(raw_from_tar(&e, norm));
        }
    }
    Ok(out)
}

fn raw_from_tar<R: Read>(e: &tar::Entry<'_, R>, path: String) -> RawEntry {
    let h = e.header();
    RawEntry {
        path,
        is_dir: h.entry_type().is_dir(),
        size: h.size().unwrap_or(0),
        mtime: h
            .mtime()
            .ok()
            .and_then(|s| std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(s))),
        mode: h.mode().ok(),
    }
}

pub(super) fn read_deb_entry(container: &Path, inner: &str) -> Result<Vec<u8>> {
    let target = normalize(inner);
    let deb = scan(container)?;
    if target == "/debian-binary" {
        return deb.debian_binary.ok_or(Error::NotFound(target));
    }
    // A `/DEBIAN/...` path comes from the control tarball, anything else from
    // the data one.
    let (member, want) = match target.strip_prefix(CONTROL_DIR) {
        Some(rest) => (deb.control.as_ref(), normalize(rest)),
        None => (deb.data.as_ref(), target.clone()),
    };
    let Some(m) = member else { return Err(Error::NotFound(target)) };
    for e in tar_of(container, m)?.entries().map_err(super::formats::io)? {
        let mut e = e.map_err(super::formats::io)?;
        let path = e.path().map_err(super::formats::io)?.to_string_lossy().into_owned();
        if normalize(&path) == want {
            let mut data = Vec::new();
            e.read_to_end(&mut data)?;
            return Ok(data);
        }
    }
    Err(Error::NotFound(target))
}

pub(super) fn read_deb_all(container: &Path) -> Result<Vec<FullEntry>> {
    let deb = scan(container)?;
    let mut out = Vec::new();
    if let Some(data) = deb.debian_binary {
        out.push(FullEntry::file("/debian-binary", data));
    }
    for (member, prefix) in [(&deb.control, CONTROL_DIR), (&deb.data, "")] {
        let Some(m) = member else { continue };
        if !prefix.is_empty() {
            out.push(FullEntry::dir(CONTROL_DIR));
        }
        for e in tar_of(container, m)?.entries().map_err(super::formats::io)? {
            let mut e = e.map_err(super::formats::io)?;
            let path = e.path().map_err(super::formats::io)?.to_string_lossy().into_owned();
            let norm = normalize(&path);
            if norm == "/" {
                continue;
            }
            let is_dir = e.header().entry_type().is_dir();
            let (mtime, mode) = {
                let h = e.header();
                (
                    h.mtime().ok().and_then(|s| {
                        std::time::UNIX_EPOCH.checked_add(std::time::Duration::from_secs(s))
                    }),
                    h.mode().ok(),
                )
            };
            let mut data = Vec::new();
            if !is_dir {
                e.read_to_end(&mut data)?;
            }
            out.push(FullEntry {
                path: format!("{prefix}{norm}"),
                is_dir,
                data,
                mtime,
                mode,
            });
        }
    }
    Ok(out)
}
