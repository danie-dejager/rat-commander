//! Tests for the native ISO 9660 reader.
//!
//! Built against **real images** produced by `genisoimage` rather than
//! hand-assembled bytes: the point of writing this parser was to read what is
//! actually out there, and a fixture I wrote myself would only prove it agrees
//! with my own reading of the spec. Skipped cleanly where the tool is absent.

use super::*;
use crate::vfs::VfsKind;

fn tool() -> Option<&'static str> {
    for t in ["genisoimage", "mkisofs", "xorrisofs"] {
        let ok = std::process::Command::new(t)
            .arg("--version")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            return Some(t);
        }
    }
    None
}

/// A scratch directory that removes itself.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Lay out a small tree and burn it with the given `genisoimage` flags.
fn make_iso(tag: &str, flags: &[&str]) -> Option<(Scratch, PathBuf)> {
    let tool = tool()?;
    let root = crate::util::temp::rc_temp_path(&format!("isotest-{tag}"));
    let src = root.join("src");
    std::fs::create_dir_all(src.join("nested/deeper")).unwrap();
    std::fs::write(src.join("hello.txt"), b"hello iso\n").unwrap();
    std::fs::write(src.join("nested/inner.bin"), [0u8, 1, 2, 250, 255]).unwrap();
    std::fs::write(src.join("nested/deeper/leaf.txt"), b"leaf\n").unwrap();
    // A long, mixed-case name: mangled to `LONG_MIX.TXT;1` in the primary tree,
    // and only readable as written via Joliet or Rock Ridge.
    std::fs::write(src.join("A Long Mixed-Case Name.txt"), b"long\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = src.join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\necho hi\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("hello.txt", src.join("link")).unwrap();
    }

    let iso = root.join("test.iso");
    let ok = std::process::Command::new(tool)
        .args(flags)
        .arg("-quiet")
        .arg("-o")
        .arg(&iso)
        .arg(&src)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?;
    if !ok.success() {
        std::fs::remove_dir_all(&root).ok();
        return None;
    }
    Some((Scratch(root), iso))
}

fn path(iso: &Path, inner: &str) -> VfsPath {
    VfsPath { scheme: "iso".into(), path: PathBuf::from(inner), container: Some(iso.to_path_buf()) }
}

async fn names(fs: &IsoFs, iso: &Path, dir: &str) -> Vec<String> {
    let mut n: Vec<String> =
        fs.read_dir(&path(iso, dir)).await.unwrap().into_iter().map(|e| e.name).collect();
    n.sort();
    n
}

async fn read_all(fs: &IsoFs, p: &VfsPath) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut r = fs.open_read(p).await.unwrap();
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).await.unwrap();
    buf
}

/// The plain, unextended format: 8.3 upper-case names with a `;1` version.
#[tokio::test]
async fn a_plain_iso9660_image_lists_and_reads() {
    let Some((_s, iso)) = make_iso("plain", &[]) else { return };
    let fs = IsoFs::new();

    let root = names(&fs, &iso, "/").await;
    assert!(root.iter().any(|n| n.eq_ignore_ascii_case("hello.txt")), "{root:?}");
    assert!(root.iter().any(|n| n.eq_ignore_ascii_case("nested")), "{root:?}");
    // The version suffix is stripped rather than shown.
    assert!(!root.iter().any(|n| n.contains(';')), "{root:?}");

    let hello = root.iter().find(|n| n.eq_ignore_ascii_case("hello.txt")).unwrap();
    let p = path(&iso, &format!("/{hello}"));
    assert_eq!(read_all(&fs, &p).await, b"hello iso\n");
    assert_eq!(fs.stat(&p).await.unwrap().size, 10);
}

#[tokio::test]
async fn nested_directories_are_walked() {
    let Some((_s, iso)) = make_iso("nested", &["-r"]) else { return };
    let fs = IsoFs::new();
    assert!(names(&fs, &iso, "/nested").await.contains(&"inner.bin".to_string()));
    assert!(names(&fs, &iso, "/nested/deeper").await.contains(&"leaf.txt".to_string()));
    let p = path(&iso, "/nested/deeper/leaf.txt");
    assert_eq!(read_all(&fs, &p).await, b"leaf\n");
    assert_eq!(fs.stat(&path(&iso, "/nested")).await.unwrap().kind, VfsKind::Dir);
}

/// Binary content must come back byte-exact, not truncated at the extent or
/// padded out to the sector.
#[tokio::test]
async fn binary_content_round_trips_exactly() {
    let Some((_s, iso)) = make_iso("binary", &["-r"]) else { return };
    let fs = IsoFs::new();
    let p = path(&iso, "/nested/inner.bin");
    assert_eq!(read_all(&fs, &p).await, [0u8, 1, 2, 250, 255]);
}

/// Joliet is what carries the names people actually gave, so it is preferred
/// over the primary tree's mangled forms.
#[tokio::test]
async fn joliet_names_are_preferred_over_the_mangled_primary_ones() {
    let Some((_s, iso)) = make_iso("joliet", &["-J"]) else { return };
    let fs = IsoFs::new();
    let root = names(&fs, &iso, "/").await;
    assert!(
        root.contains(&"A Long Mixed-Case Name.txt".to_string()),
        "the long name should survive verbatim: {root:?}"
    );
    assert!(root.contains(&"hello.txt".to_string()), "lower case survives too: {root:?}");
    let p = path(&iso, "/A Long Mixed-Case Name.txt");
    assert_eq!(read_all(&fs, &p).await, b"long\n");
}

/// Rock Ridge carries the POSIX name, the mode and symlinks.
#[cfg(unix)]
#[tokio::test]
async fn rock_ridge_carries_names_modes_and_symlinks() {
    let Some((_s, iso)) = make_iso("rockridge", &["-r"]) else { return };
    let fs = IsoFs::new();

    let root = names(&fs, &iso, "/").await;
    assert!(root.contains(&"A Long Mixed-Case Name.txt".to_string()), "{root:?}");

    // `-r` forces 0444/0555, so the exec bit is what distinguishes the script.
    let script = fs.stat(&path(&iso, "/run.sh")).await.unwrap();
    assert!(script.mode.is_some(), "Rock Ridge should carry a mode");
    assert!(script.is_executable(), "got mode {:o}", script.mode.unwrap());
    let plain = fs.stat(&path(&iso, "/hello.txt")).await.unwrap();
    assert!(!plain.is_executable(), "got mode {:o}", plain.mode.unwrap_or(0));

    let link = fs.stat(&path(&iso, "/link")).await.unwrap();
    assert_eq!(link.kind, VfsKind::Symlink, "a symlink is not a file");
    assert_eq!(fs.read_link(&path(&iso, "/link")).await.unwrap(), "hello.txt");
    assert!(fs.read_link(&path(&iso, "/hello.txt")).await.is_err(), "and a file is not one");
}

#[tokio::test]
async fn an_image_is_read_only() {
    let Some((_s, iso)) = make_iso("readonly", &["-r"]) else { return };
    let fs = IsoFs::new();
    let p = path(&iso, "/hello.txt");
    assert!(!fs.capabilities().writable);
    assert!(matches!(
        fs.open_write(&p, WriteMeta::default()).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(fs.mkdir(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.remove_file(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.remove_dir(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.rename(&p, &p).await.err(), Some(Error::Unsupported)));
}

#[tokio::test]
async fn a_directory_is_not_readable_as_a_file() {
    let Some((_s, iso)) = make_iso("isdir", &["-r"]) else { return };
    let fs = IsoFs::new();
    let err = match fs.open_read(&path(&iso, "/nested")).await {
        Ok(_) => panic!("a directory is not a file"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("is a directory"), "{err}");
}

#[tokio::test]
async fn a_missing_entry_is_not_found() {
    let Some((_s, iso)) = make_iso("missing", &["-r"]) else { return };
    let fs = IsoFs::new();
    assert!(matches!(fs.stat(&path(&iso, "/nope")).await.err(), Some(Error::NotFound(_))));
    assert!(fs.read_dir(&path(&iso, "/nope")).await.is_err());
}

/// A file that is not an ISO at all is declined rather than half-parsed — which
/// is what lets an `rc.ext` rule still have its chance at a UDF-only image.
#[tokio::test]
async fn something_that_is_not_an_iso_is_declined() {
    let root = crate::util::temp::rc_temp_path("isotest-notiso");
    std::fs::create_dir_all(&root).unwrap();
    let _s = Scratch(root.clone());
    let fake = root.join("fake.iso");
    std::fs::write(&fake, vec![0u8; 40 * 2048]).unwrap();

    assert!(!looks_like_iso(&fake), "the probe declines it");
    let fs = IsoFs::new();
    assert!(fs.read_dir(&path(&fake, "/")).await.is_err());
    // And a file far too short to hold a descriptor at all.
    let tiny = root.join("tiny.iso");
    std::fs::write(&tiny, b"nope").unwrap();
    assert!(!looks_like_iso(&tiny));
}

#[tokio::test]
async fn a_real_image_passes_the_probe() {
    let Some((_s, iso)) = make_iso("probe", &["-r"]) else { return };
    assert!(looks_like_iso(&iso));
}

/// An image is immutable while it sits there, so its tree is parsed once.
#[tokio::test]
async fn the_tree_is_parsed_once_per_image() {
    let Some((_s, iso)) = make_iso("cache", &["-r"]) else { return };
    let fs = IsoFs::new();
    let a = fs.tree(&iso).await.unwrap();
    let b = fs.tree(&iso).await.unwrap();
    assert!(Arc::ptr_eq(&a, &b));
}

#[test]
fn a_name_loses_its_version_suffix_and_trailing_dot() {
    assert_eq!(clean_name("README.TXT;1"), "README.TXT");
    assert_eq!(clean_name("DIRNAME."), "DIRNAME");
    assert_eq!(clean_name("PLAIN"), "PLAIN");
    assert_eq!(clean_name("A.B;12"), "A.B");
}

#[test]
fn a_record_timestamp_decodes_to_a_real_instant() {
    // 2026-05-11 10:00:00 UTC.
    let bytes = [126u8, 5, 11, 10, 0, 0, 0];
    let t = record_time(&bytes).expect("a valid date");
    let secs = t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    assert_eq!(secs, 1_778_493_600, "got {secs}");
    // A zeroed or nonsensical date is rejected rather than fabricated.
    assert!(record_time(&[0u8; 7]).is_none());
    assert!(record_time(&[126, 13, 11, 0, 0, 0, 0]).is_none(), "month 13");
}
