//! Where a running template reads bytes from.
//!
//! A run reads through its own file handle plus a snapshot of the hex editor's
//! unsaved edits, so it can work on a background thread while the editor goes
//! on changing; a small page cache keeps the many tiny reads a template makes
//! cheap.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

pub trait ByteSource: Send {
    fn len(&self) -> u64;
    /// Read up to `buf.len()` bytes at `off`; returns how many were read
    /// (fewer only at the end of the data).
    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize;
}

/// Bytes in memory.
#[cfg(test)]
pub struct MemSource(pub Vec<u8>);

#[cfg(test)]
impl ByteSource for MemSource {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        let Ok(start) = usize::try_from(off) else { return 0 };
        let Some(avail) = self.0.get(start..) else { return 0 };
        let n = avail.len().min(buf.len());
        buf[..n].copy_from_slice(&avail[..n]);
        n
    }
}

const PAGE: u64 = 1 << 16;
const PAGES: usize = 64;

/// A file with a byte overlay (pending edits) on top.
pub struct FileSource {
    file: File,
    len: u64,
    overlay: BTreeMap<u64, u8>,
    /// Recently read pages, most recent last.
    cache: Vec<(u64, Vec<u8>)>,
}

impl FileSource {
    pub fn open(path: &Path, overlay: BTreeMap<u64, u8>) -> std::io::Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        Ok(FileSource { file, len, overlay, cache: Vec::new() })
    }

    fn page(&mut self, index: u64) -> &[u8] {
        if let Some(pos) = self.cache.iter().position(|(i, _)| *i == index) {
            let p = self.cache.remove(pos);
            self.cache.push(p);
        } else {
            let mut data = vec![0u8; PAGE as usize];
            let mut got = 0;
            if self.file.seek(SeekFrom::Start(index * PAGE)).is_ok() {
                while got < data.len() {
                    match self.file.read(&mut data[got..]) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => got += n,
                    }
                }
            }
            data.truncate(got);
            if self.cache.len() >= PAGES {
                self.cache.remove(0);
            }
            self.cache.push((index, data));
        }
        &self.cache.last().expect("just pushed").1
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&mut self, off: u64, buf: &mut [u8]) -> usize {
        if off >= self.len {
            return 0;
        }
        let want = (buf.len() as u64).min(self.len - off) as usize;
        let mut done = 0usize;
        while done < want {
            let at = off + done as u64;
            let index = at / PAGE;
            let within = (at % PAGE) as usize;
            let page = self.page(index);
            if within >= page.len() {
                break;
            }
            let n = (page.len() - within).min(want - done);
            buf[done..done + n].copy_from_slice(&page[within..within + n]);
            done += n;
        }
        for (&k, &v) in self.overlay.range(off..off + done as u64) {
            buf[(k - off) as usize] = v;
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_source_reads_across_pages_with_the_overlay() {
        let path = std::env::temp_dir().join(format!("rc-bt-src-{}", std::process::id()));
        let data: Vec<u8> = (0..(PAGE * 2 + 100)).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        let mut overlay = BTreeMap::new();
        overlay.insert(PAGE, 0xAA);
        let mut s = FileSource::open(&path, overlay).unwrap();
        let mut buf = vec![0u8; 20];
        assert_eq!(s.read_at(PAGE - 10, &mut buf), 20);
        assert_eq!(&buf[..10], &data[(PAGE - 10) as usize..PAGE as usize]);
        assert_eq!(buf[10], 0xAA);
        assert_eq!(buf[11], data[(PAGE + 1) as usize]);
        assert_eq!(s.read_at(s.len() - 5, &mut buf), 5);
        assert_eq!(s.read_at(s.len(), &mut buf), 0);
        let _ = std::fs::remove_file(path);
    }
}
