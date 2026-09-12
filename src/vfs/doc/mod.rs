//! `json://` and `toml://` VFS backends — a document browsed as a directory.
//!
//! Objects and tables are directories, arrays are directories of numbered
//! entries, and a scalar is a file whose contents are its value. So a setting
//! buried six levels down is something you `cd` to and `F3`:
//!
//! ```text
//! /package/metadata/deb/depends/0      one dependency, as a file
//! ```
//!
//! **Read-only, on purpose.** Writing one value back means re-serialising the
//! whole document, which destroys every comment and all of its formatting — for
//! a config file, the very thing worth keeping. Doing it properly needs a
//! format-preserving parse (`toml_edit` and its equivalents) and is a change of
//! its own; until then the backend says `writable: false` rather than quietly
//! rewriting someone's file.

use crate::util::{Error, Result};
use crate::vfs::membuf::MemReader;
use crate::vfs::tree::{self, Meta, TreeBuilder, TreeCache, VfsTree};
use crate::vfs::{BoxRead, BoxWrite, Capabilities, Vfs, VfsEntry, VfsKind, VfsPath, WriteMeta};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

/// Documents larger than this are refused rather than held in memory twice.
const MAX_BYTES: u64 = 64 * 1024 * 1024;
const MAX_NODES: usize = 200_000;
const MAX_DEPTH: usize = 64;

/// Which document syntax a backend reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    Json,
    Toml,
}

impl Syntax {
    pub fn scheme(self) -> &'static str {
        match self {
            Syntax::Json => "json",
            Syntax::Toml => "toml",
        }
    }

    /// The extensions this syntax claims when Enter is pressed on a file.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Syntax::Json => &["json"],
            Syntax::Toml => &["toml"],
        }
    }
}

/// What a leaf of the document holds.
///
/// Only **scalars** carry text. Containers are directories, and a directory has
/// no contents of its own here any more than it does on disk — storing each
/// one's rendered subtree would cost a copy of the document per level of
/// nesting, for bytes nothing can reach.
#[derive(Debug, Clone, Default)]
pub struct Node {
    text: String,
}

/// Render a scalar the way its file shows it: the value itself, unquoted, so
/// `F3` on a string setting shows the string and not `"the string"`.
fn json_scalar(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Null => Some("null".to_string()),
        serde_json::Value::Bool(b) => Some(b.to_string()),
        serde_json::Value::Number(n) => Some(n.to_string()),
        serde_json::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

fn toml_scalar(v: &toml::Value) -> Option<String> {
    match v {
        toml::Value::String(s) => Some(s.clone()),
        toml::Value::Integer(i) => Some(i.to_string()),
        toml::Value::Float(f) => Some(f.to_string()),
        toml::Value::Boolean(b) => Some(b.to_string()),
        toml::Value::Datetime(d) => Some(d.to_string()),
        _ => None,
    }
}

/// Zero-pad an array index to the width the array needs, so that the panel's
/// ordinary sort by name is sort by index — otherwise `10` would sort before
/// `2` and a list would read out of order.
fn index_name(i: usize, len: usize) -> String {
    let width = len.saturating_sub(1).to_string().len().max(1);
    format!("{i:0width$}")
}

/// A key that is safe as one path component. A `/` in a JSON key would otherwise
/// invent a directory level that is not in the document.
fn safe_key(key: &str) -> String {
    let mut out: String = key
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '-' } else { c })
        .collect();
    if out.is_empty() {
        out.push('-');
    }
    out
}

/// Walk a JSON value, grafting it onto the tree.
fn walk_json(b: &mut TreeBuilder<Node>, v: &serde_json::Value, at: &str, depth: usize) {
    if depth > MAX_DEPTH || b.entry_count() > MAX_NODES {
        return;
    }
    match v {
        serde_json::Value::Object(map) => {
            for (k, child) in map {
                let path = format!("{at}/{}", safe_key(k));
                insert_json(b, child, &path);
                walk_json(b, child, &path, depth + 1);
            }
        }
        serde_json::Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                let path = format!("{at}/{}", index_name(i, items.len()));
                insert_json(b, child, &path);
                walk_json(b, child, &path, depth + 1);
            }
        }
        _ => {}
    }
}

fn insert_json(b: &mut TreeBuilder<Node>, v: &serde_json::Value, path: &str) {
    let (kind, text) = match json_scalar(v) {
        Some(s) => (VfsKind::File, s),
        None => (VfsKind::Dir, String::new()),
    };
    b.insert(path, kind, text.len() as u64, Meta::default(), Node { text });
}

fn walk_toml(b: &mut TreeBuilder<Node>, v: &toml::Value, at: &str, depth: usize) {
    if depth > MAX_DEPTH || b.entry_count() > MAX_NODES {
        return;
    }
    match v {
        toml::Value::Table(map) => {
            for (k, child) in map {
                let path = format!("{at}/{}", safe_key(k));
                insert_toml(b, child, &path);
                walk_toml(b, child, &path, depth + 1);
            }
        }
        toml::Value::Array(items) => {
            for (i, child) in items.iter().enumerate() {
                let path = format!("{at}/{}", index_name(i, items.len()));
                insert_toml(b, child, &path);
                walk_toml(b, child, &path, depth + 1);
            }
        }
        _ => {}
    }
}

fn insert_toml(b: &mut TreeBuilder<Node>, v: &toml::Value, path: &str) {
    let (kind, text) = match toml_scalar(v) {
        Some(s) => (VfsKind::File, s),
        None => (VfsKind::Dir, String::new()),
    };
    b.insert(path, kind, text.len() as u64, Meta::default(), Node { text });
}

/// Parse a document into a browsable tree.
fn build(syntax: Syntax, container: &Path) -> Result<VfsTree<Node>> {
    let meta = std::fs::metadata(container)?;
    if meta.len() > MAX_BYTES {
        return Err(Error::other("this document is too large to browse"));
    }
    let text = std::fs::read_to_string(container)?;
    let mtime = meta.modified().ok();
    let mut b = TreeBuilder::<Node>::new(mtime);
    match syntax {
        Syntax::Json => {
            let v: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| Error::other(format!("json: {e}")))?;
            walk_json(&mut b, &v, "", 0);
        }
        Syntax::Toml => {
            let v: toml::Value =
                toml::from_str(&text).map_err(|e| Error::other(format!("toml: {e}")))?;
            walk_toml(&mut b, &v, "", 0);
        }
    }
    Ok(b.finish())
}

/// Whether `path` parses as `syntax` — the probe that decides whether Enter
/// opens it as a tree. A file that does not parse is declined, so an `rc.ext`
/// rule (or just opening it in the editor) still gets its chance.
pub fn parses_as(syntax: Syntax, path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else { return false };
    if meta.len() > MAX_BYTES {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    match syntax {
        Syntax::Json => serde_json::from_str::<serde_json::Value>(&text).is_ok(),
        Syntax::Toml => toml::from_str::<toml::Value>(&text).is_ok(),
    }
}

/// A document, browsed as a directory.
pub struct DocFs {
    syntax: Syntax,
    cache: TreeCache<(Option<SystemTime>, u64), Node>,
}

impl DocFs {
    pub fn new(syntax: Syntax) -> Self {
        DocFs { syntax, cache: TreeCache::new() }
    }

    async fn tree(&self, container: &Path) -> Result<Arc<VfsTree<Node>>> {
        let stamp = tree::stamp_mtime_len(container).await;
        let (path, syntax) = (container.to_path_buf(), self.syntax);
        self.cache
            .get_or_build(container, stamp, || async move {
                tokio::task::spawn_blocking(move || build(syntax, &path))
                    .await
                    .map_err(|e| Error::other(e.to_string()))?
            })
            .await
    }
}

fn container_of(path: &VfsPath) -> Result<&PathBuf> {
    path.container.as_ref().ok_or_else(|| Error::InvalidPath("not a document path".to_string()))
}

#[async_trait::async_trait]
impl Vfs for DocFs {
    fn scheme(&self) -> &str {
        self.syntax.scheme()
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities::read_only()
    }

    async fn read_dir(&self, dir: &VfsPath) -> Result<Vec<VfsEntry>> {
        self.tree(container_of(dir)?).await?.read_dir(&dir.posix_path())
    }

    async fn stat(&self, path: &VfsPath) -> Result<VfsEntry> {
        self.tree(container_of(path)?).await?.stat(&path.posix_path())
    }

    async fn open_read(&self, path: &VfsPath) -> Result<BoxRead> {
        let t = self.tree(container_of(path)?).await?;
        let inner = path.posix_path();
        if tree::normalize(&inner) == "/" {
            return Err(Error::other("is a directory"));
        }
        let child = t.child(&inner)?;
        if child.kind.is_dir() {
            return Err(Error::other(format!("\"{}\" is a directory", child.name)));
        }
        Ok(Box::new(MemReader::new(child.payload.text.clone().into_bytes())))
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
