//! The template library on disk: deploying the bundled templates into
//! `~/.config/rat-commander/templates/`, finding every template there (the
//! user's own included), and picking the one that fits a file.
//!
//! Deployment keeps a manifest of what it wrote, so a later release can refresh
//! the templates the user never touched while leaving edited ones alone and not
//! bringing back ones the user deleted.

use super::bundle;
use super::header::{self, Origin, TemplateInfo};
use crate::util::checksum::ChecksumKind;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;

/// The manifest file, inside the template directory.
pub const MANIFEST: &str = ".rc-manifest.toml";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Manifest {
    /// The bundle hash last deployed; when it matches, there's nothing to do.
    #[serde(default)]
    bundle: String,
    /// Every deployed file and the hash of the content deployed.
    #[serde(default)]
    files: BTreeMap<String, String>,
}

fn sha256(data: &[u8]) -> String {
    let mut h = ChecksumKind::Sha256.hasher();
    h.update(data);
    h.finalize()
}

/// What a deployment did, for tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct DeployReport {
    pub written: Vec<String>,
    pub removed: Vec<String>,
    /// Whether the manifest said everything was already up to date.
    pub up_to_date: bool,
}

/// Deploy `entries` (the bundle) into `dir`:
///
/// | on disk | manifest | action |
/// |---|---|---|
/// | absent | absent | write |
/// | absent | present | leave absent (the user deleted it) |
/// | as deployed | present | refresh to the bundled version |
/// | edited | present | leave alone |
/// | present | absent | leave alone unless it already equals the bundled one |
///
/// Files the bundle no longer has are removed if unedited.
pub fn deploy_to(
    dir: &Path,
    entries: &[bundle::Entry],
    bundle_hash: u64,
) -> io::Result<DeployReport> {
    let mut report = DeployReport::default();
    let manifest_path = dir.join(MANIFEST);
    let hash_text = format!("{bundle_hash:016x}");
    let mut manifest: Manifest = std::fs::read_to_string(&manifest_path)
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default();
    if manifest.bundle == hash_text && dir.is_dir() {
        report.up_to_date = true;
        return Ok(report);
    }
    std::fs::create_dir_all(dir)?;

    let mut files = BTreeMap::new();
    for (name, content) in entries {
        let name = name.to_string();
        let path = dir.join(&name);
        let new_hash = sha256(content);
        let recorded = manifest.files.remove(&name);
        match (std::fs::read(&path), recorded) {
            (Err(_), None) => {
                write_file(&path, content)?;
                report.written.push(name.clone());
                files.insert(name, new_hash);
            }
            // Deployed once and deleted since: remember it, write nothing.
            (Err(_), Some(old)) => {
                files.insert(name, old);
            }
            (Ok(disk), Some(old)) => {
                let disk_hash = sha256(&disk);
                if disk_hash == old {
                    if disk_hash != new_hash {
                        write_file(&path, content)?;
                        report.written.push(name.clone());
                    }
                    files.insert(name, new_hash);
                } else if disk_hash == new_hash {
                    files.insert(name, new_hash);
                } else {
                    // Edited by the user: keep the edit, and keep treating the
                    // original as what was deployed.
                    files.insert(name, old);
                }
            }
            (Ok(disk), None) => {
                if sha256(&disk) == new_hash {
                    files.insert(name, new_hash);
                }
            }
        }
    }
    // Whatever is left in the old manifest has left the bundle.
    for (name, old) in std::mem::take(&mut manifest.files) {
        let path = dir.join(&name);
        if std::fs::read(&path).is_ok_and(|d| sha256(&d) == old)
            && std::fs::remove_file(&path).is_ok()
        {
            report.removed.push(name);
        }
    }

    manifest.bundle = hash_text;
    manifest.files = files;
    let text = format!(
        "# Written by rat-commander: the bundled binary templates deployed here and\n\
         # the hash of what was deployed, so upgrades refresh only unedited copies.\n{}",
        toml::to_string(&manifest).unwrap_or_default()
    );
    let tmp = dir.join(format!("{MANIFEST}.tmp"));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, &manifest_path)?;
    Ok(report)
}

fn write_file(path: &Path, content: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension("bt.tmp");
    std::fs::write(&tmp, content)?;
    std::fs::rename(&tmp, path)
}

/// The user's template directory, or `None` in tests (which must never touch
/// the real config) or when there is no config directory.
pub fn user_dir() -> Option<PathBuf> {
    if cfg!(test) { None } else { crate::config::paths::templates_dir() }
}

static DEPLOYED: OnceLock<()> = OnceLock::new();

/// Deploy the bundle into the user's template directory, once per run. Called
/// on a background thread at startup and again (a no-op by then, or waiting
/// for the startup one) before templates are first listed.
pub fn deploy_once() {
    DEPLOYED.get_or_init(|| {
        if let Some(dir) = user_dir() {
            let _ = deploy_to(&dir, bundle::entries(), bundle::BUNDLE_HASH);
        }
    });
}

/// A snapshot of every template available.
pub type Templates = Arc<Vec<TemplateInfo>>;

/// A cheap fingerprint of a directory tree's `.bt` files: names, sizes, mtimes.
type Fingerprint = Vec<(PathBuf, u64, Option<SystemTime>)>;

static CACHE: Mutex<Option<(Option<PathBuf>, Fingerprint, Templates)>> = Mutex::new(None);

fn bt_files(dir: &Path) -> Fingerprint {
    let mut out: Fingerprint = walkdir::WalkDir::new(dir)
        .max_depth(3)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            let n = e.file_name().to_string_lossy();
            !n.starts_with('.') && n.to_ascii_lowercase().ends_with(".bt")
        })
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((e.into_path(), m.len(), m.modified().ok()))
        })
        .collect();
    out.sort();
    out
}

/// Every template: the ones in the user's directory, or the bundle when there
/// is no such directory. Cached until a file there changes.
pub fn discover() -> Templates {
    deploy_once();
    discover_in(user_dir().as_deref())
}

/// [`discover`] for an explicit directory (`None`: the bundle only).
pub fn discover_in(dir: Option<&Path>) -> Templates {
    let listing = dir.filter(|d| d.is_dir()).map(bt_files);
    let key = dir.map(Path::to_path_buf);
    let fp = listing.clone().unwrap_or_default();
    if let Ok(cache) = CACHE.lock()
        && let Some((k, f, t)) = cache.as_ref()
        && *k == key
        && *f == fp
    {
        return t.clone();
    }
    let templates: Vec<TemplateInfo> = match &listing {
        Some(files) if !files.is_empty() => files
            .iter()
            .filter_map(|(path, len, _)| {
                let data = std::fs::read(path).ok()?;
                let name = path.file_name()?.to_string_lossy().into_owned();
                let mut info = header::parse_header(&name, &data);
                let top = path.parent() == dir;
                info.origin = match bundle::get(&name) {
                    Some(b) if top && b.len() as u64 == *len && *b == *data => Origin::BuiltIn,
                    Some(_) if top => Origin::Modified,
                    _ => Origin::User,
                };
                info.path = Some(path.clone());
                Some(info)
            })
            .collect(),
        _ => {
            bundle::entries().iter().map(|(name, data)| header::parse_header(name, data)).collect()
        }
    };
    let templates = Arc::new(templates);
    if let Ok(mut cache) = CACHE.lock() {
        *cache = Some((key, fp, templates.clone()));
    }
    templates
}

/// Forget the cached listing, so the next [`discover`] reads the headers again.
pub fn invalidate() {
    if let Ok(mut cache) = CACHE.lock() {
        *cache = None;
    }
}

/// The source of a template: its file, or the bundle's copy.
pub fn read_source(info: &TemplateInfo) -> Option<Vec<u8>> {
    match &info.path {
        Some(p) => std::fs::read(p).ok(),
        None => bundle::get(&info.file_name).map(<[u8]>::to_vec),
    }
}

/// Compile a template. Includes resolve next to the including file, then in
/// the template directory (and its subdirectories), then in the bundle. Returns
/// the program and every file read; an error comes as `file:line: message`.
pub fn compile(
    info: &TemplateInfo,
    text: Vec<u8>,
) -> Result<(super::ast::Program, Vec<PathBuf>), String> {
    use super::preproc::{Source, preprocess};
    let root_dir = user_dir();
    let root = Source { name: info.file_name.clone(), path: info.path.clone(), text };
    let mut loader = |name: &str, from: Option<&Path>| -> Option<Source> {
        let rel = name.replace('\\', "/");
        let base = rel.rsplit('/').next().unwrap_or(&rel).to_string();
        let mut tries: Vec<PathBuf> = Vec::new();
        if let Some(d) = from {
            tries.push(d.join(&rel));
        }
        if let Some(d) = &root_dir {
            tries.push(d.join(&rel));
            tries.push(d.join(&base));
        }
        for p in tries {
            if let Ok(t) = std::fs::read(&p) {
                return Some(Source { name: base.clone(), path: Some(p), text: t });
            }
        }
        if let Some(d) = &root_dir
            && let Some(found) = bt_files(d).into_iter().find(|(p, ..)| {
                p.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(&base))
            })
            && let Ok(t) = std::fs::read(&found.0)
        {
            return Some(Source { name: base, path: Some(found.0), text: t });
        }
        bundle::get(&base).map(|t| Source { name: base, path: None, text: t.to_vec() })
    };
    let pp = preprocess(root, &mut loader)
        .map_err(|d| format!("{}:{}: {}", info.file_name, d.pos.line, d.msg))?;
    let deps = pp.deps.clone();
    let files = pp.files.clone();
    super::parse::parse(pp).map(|p| (p, deps)).map_err(|d| {
        let file =
            files.get(d.pos.file as usize).cloned().unwrap_or_else(|| info.file_name.clone());
        format!("{file}:{}: {}", d.pos.line, d.msg)
    })
}

/// The templates that fit a file named `file_name` whose first bytes are
/// `head`, best first (as indices into `templates`).
///
/// A template fits when a file mask matches and so do its ID Bytes (if it has
/// any); or, for a template with no file mask, when its ID Bytes match and
/// pin down at least three bytes. A bare `*` mask with no ID Bytes fits
/// nothing. Ties go to the longer
/// ID match, a real mask over `*`, the template with more ID alternatives (a
/// general format over a special case of it), the user's own template, and
/// then the name.
pub fn rank(templates: &[TemplateInfo], file_name: &str, head: &[u8]) -> Vec<usize> {
    ranked(templates, file_name, head).into_iter().map(|(i, _)| i).collect()
}

/// [`rank`], with each template's tier (1: mask and ID bytes, 2: mask alone,
/// 3: ID bytes alone).
fn ranked(templates: &[TemplateInfo], file_name: &str, head: &[u8]) -> Vec<(usize, u8)> {
    let text = looks_like_text(head);
    // Sort key (lower is better): tier, ID bytes left unmatched, wildcard mask,
    // ID alternatives left out, origin, name.
    type Key = (u8, usize, bool, usize, u8, String);
    let mut scored: Vec<(Key, usize)> = Vec::new();
    for (i, t) in templates.iter().enumerate() {
        let hits: Vec<&String> =
            t.masks.iter().filter(|m| header::mask_matches(m, file_name)).collect();
        let specific = hits.iter().any(|m| !header::mask_is_wildcard(m));
        let matched: Vec<&header::IdPattern> = t.ids.iter().filter(|p| p.matches(head)).collect();
        let fixed = matched.iter().map(|p| p.fixed()).max().unwrap_or(0);
        // In a plain text file, ID Bytes that are themselves just text (a
        // magic like "ANDR") are a coincidence, not a format.
        let textual_id = text && !matched.is_empty() && matched.iter().all(|p| p.is_text());
        let tier = if !hits.is_empty() && !t.ids.is_empty() {
            if fixed == 0 || (textual_id && !specific) {
                continue;
            }
            1
        } else if specific {
            // A binary format known by its extension alone doesn't fit text.
            if text {
                continue;
            }
            2
        } else if t.masks.is_empty() && fixed >= 3 && !textual_id {
            3
        } else {
            continue;
        };
        let origin = match t.origin {
            Origin::User => 0,
            Origin::Modified => 1,
            Origin::BuiltIn => 2,
        };
        let key = (
            tier,
            usize::MAX - fixed,
            !specific,
            usize::MAX - t.ids.len(),
            origin,
            t.file_name.to_lowercase(),
        );
        scored.push((key, i));
    }
    scored.sort();
    scored.into_iter().map(|(key, i)| (i, key.0)).collect()
}

/// Whether the start of a file reads as plain text: no NULs or stray control
/// characters, and valid UTF-8 (a character cut off at the end aside).
pub fn looks_like_text(head: &[u8]) -> bool {
    if head.is_empty() {
        return false;
    }
    let valid = match std::str::from_utf8(head) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none() && head.len() - e.valid_up_to() < 4,
    };
    valid && head.iter().all(|&b| b >= 0x20 || matches!(b, b'\t' | b'\n' | b'\r' | 0x0c | 0x1b))
}

/// The template to run by itself on a file named `file_name` whose first
/// bytes are `head`: the best that [`rank`] finds — unless it fits by file
/// mask alone and so does another, since an extension several formats use
/// (`*.img`, `*.dat`) says too little to pick one.
pub fn auto_pick(templates: &[TemplateInfo], file_name: &str, head: &[u8]) -> Option<usize> {
    let ranked = ranked(templates, file_name, head);
    let &(best, tier) = ranked.first()?;
    if tier == 2 && ranked.iter().filter(|(_, t)| *t == 2).count() > 1 {
        return None;
    }
    Some(best)
}

/// A starter template for the file named `file_name` whose first bytes are
/// `head`, written as `dir/<name>.bt`. Returns its path.
pub fn create_user_template(
    dir: &Path,
    name: &str,
    file_name: &str,
    head: &[u8],
) -> io::Result<PathBuf> {
    let mut stem: String = name
        .trim()
        .trim_end_matches(".bt")
        .chars()
        .map(|c| if c.is_alphanumeric() || "-_. ".contains(c) { c } else { '_' })
        .collect();
    if stem.trim().is_empty() {
        stem = "MyTemplate".into();
    }
    std::fs::create_dir_all(dir)?;
    let path = dir.join(format!("{}.bt", stem.trim()));
    if path.exists() {
        return Err(io::Error::new(io::ErrorKind::AlreadyExists, "template exists"));
    }
    let mask = match Path::new(file_name).extension() {
        Some(ext) => format!("*.{}", ext.to_string_lossy()),
        None => file_name.to_string(),
    };
    let ids: Vec<String> = head.iter().take(4).map(|b| format!("{b:02X}")).collect();
    let text = format!(
        "//------------------------------------------------\n\
         //--- 010 Editor Binary Template\n\
         //\n\
         //      File: {stem}.bt\n\
         //   Authors: \n\
         //   Version: 1.0\n\
         //   Purpose: \n\
         //  Category: User\n\
         // File Mask: {mask}\n\
         //  ID Bytes: {ids}\n\
         //   History: \n\
         //------------------------------------------------\n\
         \n\
         LittleEndian();\n\
         \n\
         typedef struct {{\n    \
             uchar magic[4] <format=hex>;\n\
         }} HEADER <bgcolor=cLtBlue>;\n\
         \n\
         HEADER header;\n",
        stem = stem.trim(),
        ids = ids.join(" "),
    );
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rc-bt-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    fn bundle_of(items: &[(&str, &str)]) -> Vec<bundle::Entry> {
        items.iter().map(|(n, c)| ((*n).into(), c.as_bytes().to_vec().into_boxed_slice())).collect()
    }

    #[test]
    fn deploy_follows_the_manifest_policy() {
        let dir = tmpdir("deploy");
        let v1 = bundle_of(&[("A.bt", "a1"), ("B.bt", "b1"), ("C.bt", "c1"), ("D.bt", "d1")]);
        let r = deploy_to(&dir, &v1, 1).unwrap();
        assert_eq!(r.written.len(), 4);
        // The same bundle again does nothing at all.
        assert!(deploy_to(&dir, &v1, 1).unwrap().up_to_date);

        // The user edits B, deletes C, and adds their own file.
        std::fs::write(dir.join("B.bt"), "b-edited").unwrap();
        std::fs::remove_file(dir.join("C.bt")).unwrap();
        std::fs::write(dir.join("Mine.bt"), "mine").unwrap();

        // A new release changes A, B and C, drops D and adds E.
        let v2 = bundle_of(&[("A.bt", "a2"), ("B.bt", "b2"), ("C.bt", "c2"), ("E.bt", "e2")]);
        let r = deploy_to(&dir, &v2, 2).unwrap();
        let read = |n: &str| std::fs::read_to_string(dir.join(n)).ok();
        assert_eq!(read("A.bt").as_deref(), Some("a2"), "unedited copies are refreshed");
        assert_eq!(read("B.bt").as_deref(), Some("b-edited"), "edits are kept");
        assert_eq!(read("C.bt"), None, "deleted templates stay deleted");
        assert_eq!(read("D.bt"), None, "templates gone from the bundle are removed");
        assert_eq!(read("E.bt").as_deref(), Some("e2"));
        assert_eq!(read("Mine.bt").as_deref(), Some("mine"));
        assert_eq!(r.removed, vec!["D.bt".to_string()]);

        // Still deleted and still edited after yet another release.
        let v3 = bundle_of(&[("A.bt", "a3"), ("B.bt", "b3"), ("C.bt", "c3"), ("E.bt", "e3")]);
        deploy_to(&dir, &v3, 3).unwrap();
        assert_eq!(read("C.bt"), None);
        assert_eq!(read("B.bt").as_deref(), Some("b-edited"));
        assert_eq!(read("E.bt").as_deref(), Some("e3"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discovery_tags_built_in_edited_and_user_templates() {
        let dir = tmpdir("discover");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        let zip = bundle::get("ZIP.bt").unwrap();
        std::fs::write(dir.join("ZIP.bt"), zip).unwrap();
        let mut png = bundle::get("PNG.bt").unwrap().to_vec();
        png.extend_from_slice(b"\n// edited\n");
        std::fs::write(dir.join("PNG.bt"), png).unwrap();
        std::fs::write(dir.join("sub/Mine.bt"), "// File Mask: *.mine\n").unwrap();
        let t = discover_in(Some(&dir));
        let origin = |n: &str| t.iter().find(|i| i.file_name == n).map(|i| i.origin);
        assert_eq!(origin("ZIP.bt"), Some(Origin::BuiltIn));
        assert_eq!(origin("PNG.bt"), Some(Origin::Modified));
        assert_eq!(origin("Mine.bt"), Some(Origin::User));
        assert_eq!(t.len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ranking_prefers_id_matches_and_skips_bare_wildcards() {
        let t = discover_in(None);
        let best = |name: &str, head: &[u8]| {
            rank(&t, name, head).first().map(|&i| t[i].file_name.clone()).unwrap_or_default()
        };
        assert_eq!(best("a.zip", b"PK\x03\x04"), "ZIP.bt");
        assert_eq!(best("pic.png", b"\x89PNG\r\n\x1a\n"), "PNG.bt");
        assert_eq!(best("sound.wav", b"RIFF\0\0\0\0WAVEfmt "), "WAV.bt");
        assert_eq!(best("prog", b"\x7fELF\x02\x01\x01"), "ELF.bt");
        assert_eq!(best("a.bmp", b"BM\0\0"), "BMP.bt");
        assert_eq!(best("a.gz", b"\x1f\x8b\x08"), "GZip.bt");
        // A .zip that isn't one: ZIP's mask matches but its ID Bytes don't.
        assert_ne!(best("fake.zip", b"hello world"), "ZIP.bt");
        // Nothing for plain text with an unknown name.
        assert!(rank(&t, "notes", b"just some text").is_empty());
        // Text that happens to start like a magic, or to share an extension
        // with a binary format, isn't taken for it.
        assert!(rank(&t, "ANDROID_API.rst", b"ANDROID_API\n-----------\n").is_empty());
        assert!(rank(&t, "fib.spec", b"; SPDX-License-Identifier\nstruct x {\n").is_empty());
        // A real text format with a real ID still is.
        assert_eq!(best("doc.pdf", b"%PDF-1.7\n%text\n"), "PDF.bt");
        // An extension alone picks a template only when just one claims it.
        let pick =
            |name: &str, head: &[u8]| auto_pick(&t, name, head).map(|i| t[i].file_name.clone());
        assert_eq!(pick("disk.img", &[0xeb, 0x3c, 0x90, 0, 0, 0]), None, "several *.img formats");
        assert_eq!(pick("a.shp", &[0, 0, 0x27, 0x0a, 1, 2]).as_deref(), Some("SHP.bt"));
        assert_eq!(pick("a.zip", b"PK\x03\x04").as_deref(), Some("ZIP.bt"));
    }

    #[test]
    fn a_new_template_gets_a_header_for_the_file() {
        let dir = tmpdir("new");
        let p =
            create_user_template(&dir, "My Format", "data.xyz", b"\x01\x02\x03\x04\x05").unwrap();
        let src = std::fs::read(&p).unwrap();
        let info = header::parse_header("My Format.bt", &src);
        assert_eq!(info.masks, vec!["*.xyz"]);
        assert!(info.ids[0].matches(b"\x01\x02\x03\x04"));
        assert!(create_user_template(&dir, "My Format", "data.xyz", b"").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
