//! Archive backend tests.
//!
//! An archive is browsed and edited like a directory, so these exercise the
//! same repertoire an ordinary filesystem gets: list, stat, read, create a
//! subdirectory, delete one, copy a file in and out, move, rename, and
//! overwrite — across every format that can be written. Everything happens
//! inside a scratch directory of its own that is removed again afterwards.

use super::formats::{self, ArchiveFormat, FullEntry};
use super::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The formats that can be created *and* modified. Read-only RAR is covered
/// separately.
const WRITABLE: &[&str] = &["zip", "tar", "tar.gz", "tar.bz2", "tar.xz", "tar.zst", "7z"];

// ---------------------------------------------------------------------------
// Scratch space
// ---------------------------------------------------------------------------

/// A private directory under the system temp, removed when the test ends
/// (including on panic) so a run never leaves anything behind — and never
/// touches a path outside its own scratch tree.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let dir = crate::util::temp::rc_temp_path(&format!("test-{tag}"));
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        Scratch(dir)
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.0.join(rel)
    }

    /// Create a local file (making its parent directories), returning its path.
    fn file(&self, rel: &str, data: &[u8]) -> PathBuf {
        let p = self.path(rel);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(&p, data).unwrap();
        p
    }

    fn dir(&self, rel: &str) -> PathBuf {
        let p = self.path(rel);
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The stock source tree every archive in these tests is built from:
///
/// ```text
/// tree/notes.txt
/// tree/data/a.txt
/// tree/data/b.txt
/// tree/data/deep/c.txt
/// tree/empty/            (no members of its own)
/// ```
fn source_tree(s: &Scratch) -> Vec<PathBuf> {
    s.file("tree/notes.txt", b"notes");
    s.file("tree/data/a.txt", b"alpha");
    s.file("tree/data/b.txt", b"beta");
    s.file("tree/data/deep/c.txt", b"gamma");
    s.dir("tree/empty");
    vec![s.path("tree/notes.txt"), s.path("tree/data"), s.path("tree/empty")]
}

/// Build `<scratch>/archive.<ext>` from [`source_tree`].
fn make_archive(s: &Scratch, ext: &str) -> PathBuf {
    let container = s.path(&format!("archive.{ext}"));
    let format = ArchiveFormat::from_path(&container).expect("known format");
    create_archive(format, &container, &source_tree(s)).expect("create archive");
    container
}

fn at(container: &Path, inner: &str) -> VfsPath {
    VfsPath::archive(container, inner)
}

/// The sorted child names of an inner directory.
async fn names(fs: &ArchiveFs, container: &Path, inner: &str) -> Vec<String> {
    let mut v: Vec<String> = fs
        .read_dir(&at(container, inner))
        .await
        .unwrap_or_else(|e| panic!("read_dir {inner}: {e}"))
        .into_iter()
        .map(|e| e.name)
        .collect();
    v.sort();
    v
}

async fn read(fs: &ArchiveFs, container: &Path, inner: &str) -> Vec<u8> {
    let mut r = fs
        .open_read(&at(container, inner))
        .await
        .unwrap_or_else(|e| panic!("open_read {inner}: {e}"));
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).await.unwrap();
    buf
}

/// Write a member through the backend's own writer, the way the ops engine
/// copies a file into an archive.
async fn write_into(fs: &ArchiveFs, container: &Path, inner: &str, data: &[u8]) -> Result<()> {
    let mut w = fs.open_write(&at(container, inner), WriteMeta::default()).await?;
    w.write_all(data).await?;
    w.shutdown().await?;
    Ok(())
}

/// Every member name the archive actually stores, in order — the check that
/// catches a "successful" write that quietly left two members of one name.
fn members(container: &Path) -> Vec<String> {
    let format = ArchiveFormat::from_path(container).unwrap();
    formats::list_entries(format, container).unwrap().into_iter().map(|e| e.path).collect()
}

fn err_of<T>(r: Result<T>) -> String {
    match r {
        Ok(_) => panic!("expected an error"),
        Err(e) => e.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Browsing
// ---------------------------------------------------------------------------

/// The base case, for every writable format: an archive built from a directory
/// tree browses back as that same tree, contents included.
#[tokio::test]
async fn creates_browses_and_extracts_every_format() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("browse-{ext}"));
        let c = make_archive(&s, ext);
        let fs = ArchiveFs::new();

        assert_eq!(names(&fs, &c, "/").await, ["data", "empty", "notes.txt"], "root of .{ext}");
        assert_eq!(names(&fs, &c, "/data").await, ["a.txt", "b.txt", "deep"], ".{ext}");
        assert_eq!(names(&fs, &c, "/data/deep").await, ["c.txt"], ".{ext}");
        assert!(names(&fs, &c, "/empty").await.is_empty(), "empty dir survives in .{ext}");

        assert_eq!(read(&fs, &c, "/notes.txt").await, b"notes", ".{ext}");
        assert_eq!(read(&fs, &c, "/data/deep/c.txt").await, b"gamma", ".{ext}");

        let e = fs.stat(&at(&c, "/data/a.txt")).await.unwrap();
        assert_eq!((e.name.as_str(), e.kind, e.size), ("a.txt", VfsKind::File, 5), ".{ext}");
        assert!(fs.stat(&at(&c, "/data")).await.unwrap().kind.is_dir(), ".{ext}");
        assert!(fs.stat(&at(&c, "/")).await.unwrap().kind.is_dir(), "root of .{ext}");
    }
}

/// Names a filesystem allows — spaces, non-ASCII, dotfiles, deep nesting — come
/// back unchanged, and the entry is readable under exactly that name.
#[tokio::test]
async fn handles_awkward_member_names() {
    let s = Scratch::new("names");
    s.file("tree/a b/ünï/файл.txt", b"data");
    s.file("tree/.hidden", b"h");
    let c = s.path("odd.zip");
    create_archive(ArchiveFormat::Zip, &c, &[s.path("tree")]).unwrap();

    let fs = ArchiveFs::new();
    assert_eq!(names(&fs, &c, "/tree").await, [".hidden", "a b"]);
    assert_eq!(names(&fs, &c, "/tree/a b/ünï").await, ["файл.txt"]);
    assert_eq!(read(&fs, &c, "/tree/a b/ünï/файл.txt").await, b"data");
}

/// Missing paths and wrong-kind accesses report the same way a filesystem does,
/// rather than succeeding with something surprising.
#[tokio::test]
async fn reports_missing_and_wrong_kind_paths() {
    let s = Scratch::new("missing");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    assert!(fs.read_dir(&at(&c, "/nope")).await.is_err());
    assert!(fs.stat(&at(&c, "/nope")).await.is_err());
    assert!(fs.read_dir(&at(&c, "/notes.txt")).await.is_err(), "a file is not a directory");

    let broken = s.file("broken.zip", b"not really a zip");
    assert!(fs.read_dir(&at(&broken, "/")).await.is_err(), "a corrupt container errors");
}

/// A hostile member name (`../../etc/passwd`, the "zip slip") is clamped to the
/// archive root. Left alone it would show up as a `..` entry — colliding with
/// the panel's own parent link — and extraction would join it onto the
/// destination and write outside it.
#[tokio::test]
async fn clamps_member_names_that_climb_out_of_the_archive() {
    let s = Scratch::new("traversal");
    let c = s.path("evil.zip");
    formats::write_all(
        ArchiveFormat::Zip,
        &c,
        &[
            FullEntry::file("../escape.txt", b"pwned".to_vec()),
            FullEntry::file("../../../etc/passwd", b"nope".to_vec()),
            FullEntry::file("ok.txt", b"ok".to_vec()),
        ],
    )
    .unwrap();

    let fs = ArchiveFs::new();
    let root = names(&fs, &c, "/").await;
    assert!(
        !root.contains(&"..".to_string()),
        "no `..` entry to collide with the parent link: {root:?}"
    );
    assert_eq!(root, ["escape.txt", "etc", "ok.txt"], "everything landed under the root");
    assert_eq!(read(&fs, &c, "/escape.txt").await, b"pwned", "still readable, just contained");
}

/// Some writers store a folder as a zero-length *file* entry next to its
/// children. `stat` and `read_dir` have to agree about what such a name is —
/// otherwise the panel lists a file the ops engine then walks as a directory.
#[tokio::test]
async fn a_name_stored_as_both_file_and_directory_reads_as_one_kind() {
    let s = Scratch::new("collide");
    let c = s.path("both.zip");
    formats::write_all(
        ArchiveFormat::Zip,
        &c,
        &[FullEntry::file("x", Vec::new()), FullEntry::file("x/inner.txt", b"i".to_vec())],
    )
    .unwrap();

    let fs = ArchiveFs::new();
    let listed = fs.read_dir(&at(&c, "/")).await.unwrap();
    let x = listed.iter().find(|e| e.name == "x").expect("listed");
    let statted = fs.stat(&at(&c, "/x")).await.unwrap();
    assert_eq!(x.kind, statted.kind, "read_dir and stat agree");
    assert!(x.kind.is_dir(), "the name with children stays browsable");
    assert_eq!(names(&fs, &c, "/x").await, ["inner.txt"]);
}

// ---------------------------------------------------------------------------
// Creating and deleting directories
// ---------------------------------------------------------------------------

/// F7 inside an archive: the new directory appears, is empty, and can be
/// filled — for every writable format.
#[tokio::test]
async fn creates_a_subdirectory_in_every_format() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("mkdir-{ext}"));
        let c = make_archive(&s, ext);
        let fs = ArchiveFs::new();

        fs.mkdir(&at(&c, "/data/fresh")).await.unwrap_or_else(|e| panic!(".{ext}: {e}"));
        assert_eq!(names(&fs, &c, "/data").await, ["a.txt", "b.txt", "deep", "fresh"], ".{ext}");
        assert!(names(&fs, &c, "/data/fresh").await.is_empty(), ".{ext}");
        assert!(fs.stat(&at(&c, "/data/fresh")).await.unwrap().kind.is_dir(), ".{ext}");

        write_into(&fs, &c, "/data/fresh/x.txt", b"x").await.unwrap();
        assert_eq!(names(&fs, &c, "/data/fresh").await, ["x.txt"], ".{ext}");
    }
}

/// The same refusals a real `mkdir` gives: the name is taken, or the parent
/// isn't there.
#[tokio::test]
async fn mkdir_refuses_an_existing_name_or_a_missing_parent() {
    let s = Scratch::new("mkdir-refuse");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    assert!(err_of(fs.mkdir(&at(&c, "/data")).await).contains("already exists"));
    assert!(err_of(fs.mkdir(&at(&c, "/notes.txt")).await).contains("already exists"));
    assert!(err_of(fs.mkdir(&at(&c, "/nowhere/sub")).await).contains("not found"));
    assert!(err_of(fs.mkdir(&at(&c, "/notes.txt/sub")).await).contains("not a directory"));
    assert_eq!(names(&fs, &c, "/").await, ["data", "empty", "notes.txt"], "nothing was created");
}

/// Deleting behaves like `rm` / `rmdir`: a file goes, a non-empty directory is
/// refused, and once emptied the directory goes too.
#[tokio::test]
async fn deletes_files_and_empty_directories() {
    let s = Scratch::new("delete");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    fs.remove_file(&at(&c, "/data/b.txt")).await.unwrap();
    assert_eq!(names(&fs, &c, "/data").await, ["a.txt", "deep"]);

    assert!(err_of(fs.remove_dir(&at(&c, "/data")).await).contains("not empty"));
    assert!(err_of(fs.remove_file(&at(&c, "/data")).await).contains("is a directory"));
    assert!(err_of(fs.remove_file(&at(&c, "/data/gone.txt")).await).contains("not found"));

    fs.remove_dir(&at(&c, "/empty")).await.unwrap();
    assert_eq!(names(&fs, &c, "/").await, ["data", "notes.txt"]);

    // Emptying a directory removes it: nothing in the archive names it any more.
    fs.remove_file(&at(&c, "/data/deep/c.txt")).await.unwrap();
    fs.remove_dir(&at(&c, "/data/deep")).await.unwrap();
    assert_eq!(names(&fs, &c, "/data").await, ["a.txt"]);
}

/// Many archives never store a member for a directory — the folder exists only
/// because something is filed under it. Emptying such a directory makes it
/// vanish on its own, so the `rmdir` that follows a recursive delete must treat
/// "already gone" as done rather than as a missing path.
#[tokio::test]
async fn removes_a_directory_that_only_ever_existed_implicitly() {
    let s = Scratch::new("implicit-dir");
    let c = s.path("implicit.zip");
    // Only file members: nothing names `sub/` itself.
    formats::write_all(ArchiveFormat::Zip, &c, &[FullEntry::file("sub/only.txt", b"x".to_vec())])
        .unwrap();
    let fs = ArchiveFs::new();
    assert!(fs.stat(&at(&c, "/sub")).await.unwrap().kind.is_dir());

    fs.remove_file(&at(&c, "/sub/only.txt")).await.unwrap();
    fs.remove_dir(&at(&c, "/sub")).await.expect("the emptied directory is already gone");
    assert!(names(&fs, &c, "/").await.is_empty());
}

/// The bulk delete the panel uses for a marked selection takes whole subtrees in
/// one rebuild — and only on path boundaries, so `a.txt` never takes
/// `a.txt.bak` with it.
#[tokio::test]
async fn bulk_delete_takes_subtrees_but_not_name_prefixes() {
    let s = Scratch::new("bulk-delete");
    let c = make_archive(&s, "zip");
    s.file("extra/notes.txt.bak", b"backup");
    add_to_archive(&c, "/", &[s.path("extra/notes.txt.bak")]).unwrap();

    let fs = ArchiveFs::new();
    remove_from_archive(&c, &HashSet::from(["/data".to_string(), "/notes.txt".to_string()]))
        .unwrap();

    assert_eq!(names(&fs, &c, "/").await, ["empty", "notes.txt.bak"]);
    assert!(
        !members(&c).iter().any(|m| m.starts_with("/data")),
        "the subtree is gone: {:?}",
        members(&c)
    );
}

// ---------------------------------------------------------------------------
// Renaming
// ---------------------------------------------------------------------------

/// Renaming a file inside an archive, in every writable format.
#[tokio::test]
async fn renames_a_file_in_every_format() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("rename-{ext}"));
        let c = make_archive(&s, ext);
        let fs = ArchiveFs::new();

        fs.rename(&at(&c, "/data/a.txt"), &at(&c, "/data/renamed.txt"))
            .await
            .unwrap_or_else(|e| panic!(".{ext}: {e}"));
        assert_eq!(names(&fs, &c, "/data").await, ["b.txt", "deep", "renamed.txt"], ".{ext}");
        assert_eq!(read(&fs, &c, "/data/renamed.txt").await, b"alpha", ".{ext}");
    }
}

/// Renaming a directory carries its whole subtree, and can move it to another
/// directory in the same archive.
#[tokio::test]
async fn renaming_a_directory_moves_its_whole_subtree() {
    let s = Scratch::new("rename-dir");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    fs.rename(&at(&c, "/data/deep"), &at(&c, "/empty/moved")).await.unwrap();

    assert_eq!(names(&fs, &c, "/data").await, ["a.txt", "b.txt"]);
    assert_eq!(names(&fs, &c, "/empty").await, ["moved"]);
    assert_eq!(names(&fs, &c, "/empty/moved").await, ["c.txt"]);
    assert_eq!(read(&fs, &c, "/empty/moved/c.txt").await, b"gamma");
}

/// The refusals a real rename gives, plus the archive-specific one: a rename
/// between two different archives is not a rename at all, and reports
/// `Unsupported` so the ops engine falls back to copy + delete.
#[tokio::test]
async fn rename_refuses_impossible_targets() {
    let s = Scratch::new("rename-refuse");
    let c = make_archive(&s, "zip");
    let other = s.path("other.zip");
    create_archive(ArchiveFormat::Zip, &other, &[s.path("tree/notes.txt")]).unwrap();
    let fs = ArchiveFs::new();

    assert!(
        err_of(fs.rename(&at(&c, "/data/a.txt"), &at(&c, "/data/b.txt")).await)
            .contains("already exists")
    );
    assert!(err_of(fs.rename(&at(&c, "/gone.txt"), &at(&c, "/x.txt")).await).contains("not found"));
    assert!(
        err_of(fs.rename(&at(&c, "/data"), &at(&c, "/data/inner")).await).contains("into itself")
    );
    assert!(
        err_of(fs.rename(&at(&c, "/notes.txt"), &at(&c, "/nowhere/n.txt")).await)
            .contains("not found")
    );
    assert!(
        matches!(
            fs.rename(&at(&c, "/notes.txt"), &at(&other, "/notes.txt")).await,
            Err(Error::Unsupported)
        ),
        "a cross-archive rename is not a rename"
    );
    assert_eq!(names(&fs, &c, "/data").await, ["a.txt", "b.txt", "deep"], "nothing moved");
}

// ---------------------------------------------------------------------------
// Copying in and out
// ---------------------------------------------------------------------------

/// A member written through `open_write` — the path a copy *into* an archive
/// takes — lands with the right content, in every writable format.
#[tokio::test]
async fn writes_a_new_member_in_every_format() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("write-{ext}"));
        let c = make_archive(&s, ext);
        let fs = ArchiveFs::new();

        write_into(&fs, &c, "/data/deep/new.txt", b"fresh")
            .await
            .unwrap_or_else(|e| panic!(".{ext}: {e}"));
        assert_eq!(names(&fs, &c, "/data/deep").await, ["c.txt", "new.txt"], ".{ext}");
        assert_eq!(read(&fs, &c, "/data/deep/new.txt").await, b"fresh", ".{ext}");
        // The rest of the archive came through the rebuild intact.
        assert_eq!(read(&fs, &c, "/notes.txt").await, b"notes", ".{ext}");
    }
}

/// Overwriting an existing member replaces it. Before, every format got this
/// wrong in its own way: zip refused the write outright ("Duplicate filename"),
/// while tar and 7z stored a *second* member of the same name and kept handing
/// readers the stale one.
#[tokio::test]
async fn overwriting_a_member_replaces_it_rather_than_duplicating_it() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("overwrite-{ext}"));
        let c = make_archive(&s, ext);
        let fs = ArchiveFs::new();

        write_into(&fs, &c, "/data/a.txt", b"REPLACED")
            .await
            .unwrap_or_else(|e| panic!(".{ext}: {e}"));

        assert_eq!(read(&fs, &c, "/data/a.txt").await, b"REPLACED", ".{ext}");
        assert_eq!(names(&fs, &c, "/data").await, ["a.txt", "b.txt", "deep"], ".{ext}");
        let stored = members(&c);
        let dupes = stored.iter().filter(|m| *m == "/data/a.txt").count();
        assert_eq!(dupes, 1, ".{ext} stores one member, not two: {stored:?}");
        assert_eq!(
            fs.stat(&at(&c, "/data/a.txt")).await.unwrap().size,
            8,
            ".{ext} size is the new one"
        );
    }
}

/// The bulk add the panel uses for a whole selection has the same semantics as
/// the per-file write: same-named members are replaced, not duplicated, and the
/// rest of the directory is merged rather than clobbered.
#[tokio::test]
async fn bulk_add_replaces_matching_members_and_merges_the_rest() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("bulk-add-{ext}"));
        let c = make_archive(&s, ext);
        // A local `data/` holding one existing name and one new one.
        s.file("incoming/data/a.txt", b"NEWER");
        s.file("incoming/data/z.txt", b"zulu");

        add_to_archive(&c, "/", &[s.path("incoming/data")])
            .unwrap_or_else(|e| panic!(".{ext}: {e}"));

        let fs = ArchiveFs::new();
        assert_eq!(
            names(&fs, &c, "/data").await,
            ["a.txt", "b.txt", "deep", "z.txt"],
            ".{ext} merged"
        );
        assert_eq!(read(&fs, &c, "/data/a.txt").await, b"NEWER", ".{ext} replaced");
        assert_eq!(read(&fs, &c, "/data/b.txt").await, b"beta", ".{ext} untouched");
        let stored = members(&c);
        let dupes = stored.iter().filter(|m| *m == "/data/a.txt").count();
        assert_eq!(dupes, 1, ".{ext}: {stored:?}");
    }
}

/// The overwrite check that drives the confirmation dialog names exactly the
/// members a bulk add would replace — files only, and only ones already there.
#[tokio::test]
async fn add_conflicts_names_the_members_that_would_be_replaced() {
    let s = Scratch::new("conflicts");
    let c = make_archive(&s, "zip");
    s.file("incoming/data/a.txt", b"newer");
    s.file("incoming/data/z.txt", b"zulu");

    let mut found = add_conflicts(&c, "/", &[s.path("incoming/data")]).unwrap();
    found.sort();
    assert_eq!(found, ["/data/a.txt"]);

    assert!(
        add_conflicts(&c, "/", &[s.path("incoming/data/z.txt")]).unwrap().is_empty(),
        "a name that isn't there is not a conflict"
    );
    // The check must not have changed the archive.
    assert_eq!(read(&ArchiveFs::new(), &c, "/data/a.txt").await, b"alpha");
}

/// A filesystem refuses to overwrite a directory with a file (and the reverse);
/// so does the archive, rather than leaving one name meaning two things.
#[tokio::test]
async fn refuses_to_replace_a_directory_with_a_file() {
    let s = Scratch::new("kind-clash");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    assert!(err_of(write_into(&fs, &c, "/data", b"x").await).contains("is a directory"));

    s.file("clash/data", b"now a file");
    assert!(err_of(add_to_archive(&c, "/", &[s.path("clash/data")])).contains("is a directory"));

    s.dir("clash2/notes.txt");
    assert!(err_of(add_to_archive(&c, "/", &[s.path("clash2/notes.txt")])).contains("is a file"));

    assert!(fs.stat(&at(&c, "/data")).await.unwrap().kind.is_dir(), "unchanged");
    assert_eq!(read(&fs, &c, "/notes.txt").await, b"notes", "unchanged");
}

/// A bulk add needs its destination directory to exist and to be a directory,
/// rather than quietly inventing one.
#[tokio::test]
async fn bulk_add_needs_a_real_destination_directory() {
    let s = Scratch::new("add-dest");
    let c = make_archive(&s, "zip");
    let file = s.file("incoming/x.txt", b"x");

    let one = std::slice::from_ref(&file);
    assert!(err_of(add_to_archive(&c, "/nowhere", one)).contains("not found"));
    assert!(err_of(add_to_archive(&c, "/notes.txt", one)).contains("not a directory"));
    assert_eq!(names(&ArchiveFs::new(), &c, "/").await, ["data", "empty", "notes.txt"]);
}

/// Writing needs an existing parent directory, exactly as `open(2)` does.
#[tokio::test]
async fn writing_needs_an_existing_parent_directory() {
    let s = Scratch::new("write-parent");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    assert!(err_of(write_into(&fs, &c, "/nowhere/x.txt", b"x").await).contains("not found"));
    assert_eq!(names(&fs, &c, "/").await, ["data", "empty", "notes.txt"]);
}

/// The "Append" answer to an overwrite prompt extends the member instead of
/// replacing it.
#[tokio::test]
async fn appending_extends_an_existing_member() {
    let s = Scratch::new("append");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();

    let meta = WriteMeta { append: true, ..WriteMeta::default() };
    let mut w = fs.open_write(&at(&c, "/data/a.txt"), meta).await.unwrap();
    w.write_all(b"-more").await.unwrap();
    w.shutdown().await.unwrap();

    assert_eq!(read(&fs, &c, "/data/a.txt").await, b"alpha-more");
}

/// Nothing is committed until the writer is shut down, and the backend says so
/// via `atomic_write` — which is what stops the ops engine from "cleaning up" an
/// aborted copy by deleting the member that was already there.
#[tokio::test]
async fn an_abandoned_write_leaves_the_archive_untouched() {
    let s = Scratch::new("abandon");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();
    assert!(fs.capabilities().atomic_write, "the engine must not discard a partial member");

    let before = std::fs::read(&c).unwrap();
    let mut w = fs.open_write(&at(&c, "/data/a.txt"), WriteMeta::default()).await.unwrap();
    w.write_all(b"never committed").await.unwrap();
    drop(w);

    assert_eq!(std::fs::read(&c).unwrap(), before, "the container is byte-identical");
    assert_eq!(read(&fs, &c, "/data/a.txt").await, b"alpha");
}

// ---------------------------------------------------------------------------
// Metadata
// ---------------------------------------------------------------------------

/// Each member is listed with its own timestamp, not the container's — the
/// panel would otherwise stamp every file in an archive with the moment the
/// archive was last written.
#[tokio::test]
async fn lists_each_members_own_timestamp() {
    for ext in WRITABLE {
        let s = Scratch::new(&format!("mtime-{ext}"));
        let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000_000);
        s.file("tree/old.txt", b"old");
        std::fs::File::options()
            .write(true)
            .open(s.path("tree/old.txt"))
            .unwrap()
            .set_modified(stamp)
            .unwrap();
        let c = s.path(&format!("stamped.{ext}"));
        let format = ArchiveFormat::from_path(&c).unwrap();
        create_archive(format, &c, &[s.path("tree/old.txt")]).unwrap();

        let listed = ArchiveFs::new().stat(&at(&c, "/old.txt")).await.unwrap();
        let secs = |t: SystemTime| t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        // Zip's MS-DOS timestamps have two-second resolution.
        let got = secs(listed.mtime.unwrap_or_else(|| panic!(".{ext} recorded no mtime")));
        assert!(got.abs_diff(secs(stamp)) <= 2, ".{ext}: {got} vs {}", secs(stamp));
    }
}

/// A rebuild rewrites every member, so the ones it isn't touching have to come
/// back with their own timestamps and permission bits. Otherwise adding one
/// file to an archive silently restamps everything in it and drops the
/// executable bits.
#[cfg(unix)]
#[tokio::test]
async fn a_rebuild_preserves_the_other_members_metadata() {
    for ext in ["zip", "tar", "tar.gz"] {
        use std::os::unix::fs::PermissionsExt;
        let s = Scratch::new(&format!("preserve-{ext}"));
        let script = s.file("tree/run.sh", b"#!/bin/sh\n");
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_100_000_000);
        std::fs::File::options().write(true).open(&script).unwrap().set_modified(stamp).unwrap();

        let c = s.path(&format!("keep.{ext}"));
        let format = ArchiveFormat::from_path(&c).unwrap();
        create_archive(format, &c, &[script]).unwrap();

        // Touch something else entirely, forcing a full rebuild.
        s.file("extra/other.txt", b"other");
        add_to_archive(&c, "/", &[s.path("extra/other.txt")]).unwrap();

        let e = ArchiveFs::new().stat(&at(&c, "/run.sh")).await.unwrap();
        assert_eq!(e.mode.map(|m| m & 0o777), Some(0o755), ".{ext} kept the executable bit");
        let secs = |t: SystemTime| t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
        assert!(secs(e.mtime.unwrap()).abs_diff(secs(stamp)) <= 2, ".{ext} kept the timestamp");
    }
}

/// Re-applying the mode a member already has is not a change, so it must not
/// trigger another whole-archive rebuild — the ops engine re-applies the
/// permissions of every file it copies.
#[tokio::test]
async fn setting_unchanged_metadata_does_not_rewrite_the_archive() {
    let s = Scratch::new("no-op-meta");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();
    let mode = fs.stat(&at(&c, "/notes.txt")).await.unwrap().mode.expect("zip records a mode");

    let before = std::fs::read(&c).unwrap();
    fs.set_permissions(&at(&c, "/notes.txt"), mode).await.unwrap();
    assert_eq!(std::fs::read(&c).unwrap(), before, "no rebuild for a no-op");

    fs.set_permissions(&at(&c, "/notes.txt"), 0o700).await.unwrap();
    assert_eq!(fs.stat(&at(&c, "/notes.txt")).await.unwrap().mode.map(|m| m & 0o777), Some(0o700));
}

/// Directory sync stamps a copy with its source's time; an archive can oblige,
/// so a mirror into one converges instead of recopying forever.
#[tokio::test]
async fn set_mtime_stamps_a_member() {
    let s = Scratch::new("set-mtime");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();
    let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_200_000_000);

    fs.set_mtime(&at(&c, "/notes.txt"), stamp).await.unwrap();

    let got = fs.stat(&at(&c, "/notes.txt")).await.unwrap().mtime.unwrap();
    let secs = |t: SystemTime| t.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs();
    assert!(secs(got).abs_diff(secs(stamp)) <= 2);
}

/// A 7z archive records no unix mode, so chmod on one says so rather than
/// pretending to have worked.
#[tokio::test]
async fn chmod_reports_formats_that_cannot_record_a_mode() {
    let s = Scratch::new("no-mode");
    let c = make_archive(&s, "7z");
    let r = ArchiveFs::new().set_permissions(&at(&c, "/notes.txt"), 0o700).await;
    assert!(matches!(r, Err(Error::Unsupported)));
}

// ---------------------------------------------------------------------------
// Caching, container handling, read-only formats
// ---------------------------------------------------------------------------

/// The parsed listing is cached per container; a mutation through the backend
/// has to invalidate it, or the panel would keep showing the archive as it was.
/// The cache is keyed on the container's length as well as its timestamp, so a
/// filesystem with coarse timestamps can't hide a change either.
#[tokio::test]
async fn the_cached_listing_follows_the_archive() {
    let s = Scratch::new("cache");
    let c = make_archive(&s, "zip");
    let fs = ArchiveFs::new();
    assert_eq!(names(&fs, &c, "/").await, ["data", "empty", "notes.txt"]);

    // Through the backend...
    fs.mkdir(&at(&c, "/added")).await.unwrap();
    assert_eq!(names(&fs, &c, "/").await, ["added", "data", "empty", "notes.txt"]);

    // ...and behind its back, the way the bulk helpers and an external tool do.
    s.file("extra/late.txt", b"late");
    add_to_archive(&c, "/", &[s.path("extra/late.txt")]).unwrap();
    assert_eq!(names(&fs, &c, "/").await, ["added", "data", "empty", "late.txt", "notes.txt"]);
}

/// A failed rebuild must not leave a stray `.rc-tmp` beside the archive. The
/// rename is forced to fail (the target path is a directory) after the temp has
/// been written.
#[test]
fn a_failed_swap_cleans_up_its_temp_file() {
    let s = Scratch::new("swap-fail");
    let container = s.dir("archive.zip"); // a directory: renaming onto it fails
    let tmp = container.with_extension("rc-tmp");

    let entries = vec![FullEntry::file("f.txt", b"x".to_vec())];
    assert!(write_swap(ArchiveFormat::Zip, &container, &entries).is_err());
    assert!(!tmp.exists(), "no .rc-tmp left behind: {tmp:?}");
}

/// The swap replaces the container file, so the archive's own permissions have
/// to survive it — a private archive must not come back world-readable.
#[cfg(unix)]
#[tokio::test]
async fn a_rebuild_keeps_the_archives_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let s = Scratch::new("swap-mode");
    let c = make_archive(&s, "zip");
    std::fs::set_permissions(&c, std::fs::Permissions::from_mode(0o600)).unwrap();

    ArchiveFs::new().mkdir(&at(&c, "/added")).await.unwrap();

    let mode = std::fs::metadata(&c).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the rebuilt archive kept its permissions");
}

/// RAR can be read but not written, and every mutation says so plainly instead
/// of failing somewhere deeper.
#[cfg(feature = "rar")]
#[tokio::test]
async fn a_read_only_format_refuses_every_mutation() {
    let s = Scratch::new("readonly");
    // No RAR writer exists, so this stands in for one: the format is decided by
    // the name, and every mutation is refused before the file is even opened.
    let c = s.file("archive.rar", b"Rar!\x1a\x07\x00");
    let fs = ArchiveFs::new();

    for e in [
        err_of(fs.mkdir(&at(&c, "/d")).await),
        err_of(fs.remove_file(&at(&c, "/x")).await),
        err_of(fs.rename(&at(&c, "/x"), &at(&c, "/y")).await),
        err_of(write_into(&fs, &c, "/x", b"x").await),
        err_of(add_to_archive(&c, "/", &[s.file("some.txt", b"s")])),
    ] {
        assert!(e.contains("read-only"), "{e}");
    }
}

/// A path with no container is not an archive path at all.
#[tokio::test]
async fn rejects_paths_that_are_not_inside_an_archive() {
    let fs = ArchiveFs::new();
    assert!(matches!(fs.read_dir(&VfsPath::local("/tmp")).await, Err(Error::InvalidPath(_))));
}

// ---------------------------------------------------------------------------
// Debian packages
// ---------------------------------------------------------------------------

/// Build a Unix `ar` container from `(name, bytes)` members, the way `dpkg-deb`
/// lays a package out. Synthesised here rather than shipping a real `.deb`:
/// the point is that nothing external is needed to read one.
fn write_ar(dest: &Path, members: &[(&str, Vec<u8>)]) {
    let mut out: Vec<u8> = b"!<arch>\n".to_vec();
    for (name, data) in members {
        // 60-byte header: name(16) mtime(12) uid(6) gid(6) mode(8) size(10) magic(2).
        out.extend_from_slice(format!("{name:<16}").as_bytes());
        out.extend_from_slice(format!("{:<12}", 0).as_bytes());
        out.extend_from_slice(format!("{:<6}", 0).as_bytes());
        out.extend_from_slice(format!("{:<6}", 0).as_bytes());
        out.extend_from_slice(format!("{:<8}", "100644").as_bytes());
        out.extend_from_slice(format!("{:<10}", data.len()).as_bytes());
        out.extend_from_slice(b"`\n");
        out.extend_from_slice(data);
        // Members are aligned to an even offset.
        if data.len() % 2 == 1 {
            out.push(b'\n');
        }
    }
    std::fs::write(dest, out).expect("write ar");
}

/// A `.deb` whose two tarballs use *different* compressors, which is exactly
/// what a modern package looks like (zstd data, gzip control).
fn make_deb(s: &Scratch) -> PathBuf {
    let ctrl = s.path("control.tar.gz");
    formats::write_all(
        ArchiveFormat::TarGz,
        &ctrl,
        &[
            FullEntry::file("./control", b"Package: demo\nVersion: 1.0\n".to_vec()),
            FullEntry::file("./md5sums", b"d41d8cd98f00b204e9800998ecf8427e  usr/bin/demo\n".to_vec()),
        ],
    )
    .expect("control tarball");

    let data = s.path("data.tar.zst");
    formats::write_all(
        ArchiveFormat::TarZst,
        &data,
        &[
            FullEntry::dir("./usr/bin"),
            FullEntry::file("./usr/bin/demo", b"#!/bin/sh\necho hi\n".to_vec()),
            FullEntry::file("./usr/share/doc/demo/README", b"read me\n".to_vec()),
        ],
    )
    .expect("data tarball");

    let container = s.path("demo_1.0_amd64.deb");
    write_ar(
        &container,
        &[
            ("debian-binary", b"2.0\n".to_vec()),
            ("control.tar.gz", std::fs::read(&ctrl).unwrap()),
            ("data.tar.zst", std::fs::read(&data).unwrap()),
        ],
    );
    container
}

/// A `.deb` browses as one tree: the installed files at the root and the
/// control files under `/DEBIAN`, the layout `dpkg-deb -R` produces. They
/// cannot be two nested archives, because a `VfsPath` carries only one
/// container.
#[tokio::test]
async fn a_deb_lists_its_data_tree_and_control_files_together() {
    let s = Scratch::new("deb-list");
    let container = make_deb(&s);
    let fs = ArchiveFs::new();

    assert_eq!(
        names(&fs, &container, "/").await,
        vec!["DEBIAN".to_string(), "debian-binary".to_string(), "usr".to_string()]
    );
    assert_eq!(names(&fs, &container, "/DEBIAN").await, vec!["control", "md5sums"]);
    assert_eq!(names(&fs, &container, "/usr/bin").await, vec!["demo"]);
    assert_eq!(names(&fs, &container, "/usr/share/doc/demo").await, vec!["README"]);

    // Reading crosses back into whichever tarball the path came from — and the
    // two are compressed differently, so this also proves the compressor is
    // chosen per member rather than once for the package.
    assert_eq!(read(&fs, &container, "/debian-binary").await, b"2.0\n");
    assert!(
        read(&fs, &container, "/DEBIAN/control").await.starts_with(b"Package: demo"),
        "control came out of the gzip tarball"
    );
    assert_eq!(
        read(&fs, &container, "/usr/share/doc/demo/README").await,
        b"read me\n",
        "README came out of the zstd tarball"
    );
}

/// Rebuilding a package would mean regenerating `md5sums`, keeping the control
/// fields consistent and honouring signing conventions. Refuse rather than
/// produce something that looks like a package and is not one.
#[tokio::test]
async fn a_deb_refuses_to_be_written_to() {
    let s = Scratch::new("deb-ro");
    let container = make_deb(&s);
    let fs = ArchiveFs::new();

    assert!(!ArchiveFormat::Deb.writable());
    let e = err_of(write_into(&fs, &container, "/usr/bin/extra", b"nope").await);
    assert!(!e.is_empty(), "the write is refused: {e}");
    let e = err_of(fs.mkdir(&at(&container, "/opt")).await);
    assert!(!e.is_empty(), "so is a new directory: {e}");

    // And nothing was damaged in the attempt.
    assert!(names(&fs, &container, "/usr/bin").await.contains(&"demo".to_string()));
}

// ---------------------------------------------------------------------------
// RPM packages
// ---------------------------------------------------------------------------

/// One new-ASCII cpio member, header and padding included.
fn cpio_member(name: &str, mode: u32, data: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(b"070701");
    let namesize = name.len() + 1;
    for f in [1, mode, 0, 0, 1, 0x5F00_0000, data.len() as u32, 0, 0, 0, 0, namesize as u32, 0] {
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

/// One tag's value, in the shapes the reader understands.
enum Tag<'a> {
    Str(&'a str),
    /// A STRING_ARRAY: NUL-terminated strings laid end to end.
    Strs(&'a [&'a str]),
    /// An INT16 array (file modes).
    I16(&'a [u32]),
    /// An INT32 array (sizes, times, directory indices).
    I32(&'a [u32]),
}

/// One indexed (`07070X`) cpio member: sixteen bytes of header, then the data.
fn cpio_indexed(index: u32, data: &[u8]) -> Vec<u8> {
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

/// An RPM header section: magic, counts, index entries, then the store.
fn rpm_header(tags: &[(u32, Tag)]) -> Vec<u8> {
    let (mut index, mut store) = (Vec::new(), Vec::new());
    for (tag, value) in tags {
        let (ty, count) = match value {
            Tag::Str(_) => (6u32, 1u32),
            Tag::Strs(v) => (8, v.len() as u32),
            Tag::I16(v) => (3, v.len() as u32),
            Tag::I32(v) => (4, v.len() as u32),
        };
        index.extend_from_slice(&tag.to_be_bytes());
        index.extend_from_slice(&ty.to_be_bytes());
        index.extend_from_slice(&(store.len() as u32).to_be_bytes());
        index.extend_from_slice(&count.to_be_bytes());
        match value {
            Tag::Str(v) => {
                store.extend_from_slice(v.as_bytes());
                store.push(0);
            }
            Tag::Strs(vs) => {
                for v in *vs {
                    store.extend_from_slice(v.as_bytes());
                    store.push(0);
                }
            }
            Tag::I16(vs) => {
                for v in *vs {
                    store.extend_from_slice(&(*v as u16).to_be_bytes());
                }
            }
            Tag::I32(vs) => {
                for v in *vs {
                    store.extend_from_slice(&v.to_be_bytes());
                }
            }
        }
    }
    let mut v = vec![0x8E, 0xAD, 0xE8, 0x01, 0, 0, 0, 0];
    v.extend_from_slice(&((tags.len() as u32).to_be_bytes()));
    v.extend_from_slice(&((store.len() as u32).to_be_bytes()));
    v.extend_from_slice(&index);
    v.extend_from_slice(&store);
    v
}

/// A complete `.rpm`: lead, signature header (8-byte aligned), main header
/// naming the compressor, then the zstd-compressed cpio payload.
fn make_rpm(s: &Scratch) -> PathBuf {
    let payload: Vec<u8> = [
        cpio_member("./usr/bin", 0o040755, b""),
        cpio_member("./usr/bin/demo", 0o100755, b"#!/bin/sh\necho hi\n"),
        cpio_member("./usr/share/doc/demo/README", 0o100644, b"read me\n"),
        cpio_member("TRAILER!!!", 0, b""),
    ]
    .concat();

    let mut v = vec![0u8; 96];
    v[..4].copy_from_slice(&[0xED, 0xAB, 0xEE, 0xDB]);
    // The signature header is the one that is padded out to 8 bytes.
    v.extend_from_slice(&rpm_header(&[(999, Tag::Str("signature"))]));
    while !v.len().is_multiple_of(8) {
        v.push(0);
    }
    v.extend_from_slice(&rpm_header(&[
        (1124, Tag::Str("cpio")),
        (1125, Tag::Str("zstd")),
    ]));
    v.extend_from_slice(&zstd::encode_all(&payload[..], 3).unwrap());

    let container = s.path("demo-1.0-1.x86_64.rpm");
    std::fs::write(&container, v).expect("write rpm");
    container
}

/// An RPM's compressed cpio payload lists and reads like any other archive —
/// the headers only say where it starts and what packed it.
#[tokio::test]
async fn an_rpm_lists_and_reads_its_zstd_cpio_payload() {
    let s = Scratch::new("rpm-read");
    let container = make_rpm(&s);
    let fs = ArchiveFs::new();

    assert_eq!(names(&fs, &container, "/").await, vec!["usr"]);
    assert_eq!(names(&fs, &container, "/usr/bin").await, vec!["demo"]);
    assert_eq!(read(&fs, &container, "/usr/bin/demo").await, b"#!/bin/sh\necho hi\n");
    assert_eq!(read(&fs, &container, "/usr/share/doc/demo/README").await, b"read me\n");

    // The cpio mode field gives each member its real permissions, which is why
    // an extracted script comes out executable.
    let entries = formats::list_entries(ArchiveFormat::Rpm, &container).unwrap();
    let demo = entries.iter().find(|e| e.path == "/usr/bin/demo").expect("the script");
    assert_eq!(demo.mode, Some(0o755), "permissions survive, type bits masked off");
}

/// What rpm 4.14 and later actually write: a payload whose members carry only
/// an index, with every name, mode and size living in the package header. The
/// first real package tried against this reader was of exactly this shape, and
/// a payload-only parser cannot read it at all.
#[tokio::test]
async fn an_rpm_with_an_indexed_payload_takes_its_names_from_the_header() {
    let s = Scratch::new("rpm-indexed");

    let demo = b"#!/bin/sh\necho hi\n";
    let readme = b"read me\n";
    let payload: Vec<u8> =
        [cpio_indexed(0, demo), cpio_indexed(1, readme), cpio_member("TRAILER!!!", 0, b"")]
            .concat();

    let mut v = vec![0u8; 96];
    v[..4].copy_from_slice(&[0xED, 0xAB, 0xEE, 0xDB]);
    v.extend_from_slice(&rpm_header(&[(999, Tag::Str("signature"))]));
    while !v.len().is_multiple_of(8) {
        v.push(0);
    }
    v.extend_from_slice(&rpm_header(&[
        (1124, Tag::Str("cpio")),
        (1125, Tag::Str("zstd")),
        // basenames / dirnames / dirindexes: rpm splits paths so a package with
        // many files in few directories stores each directory once.
        (1117, Tag::Strs(&["demo", "README"])),
        (1118, Tag::Strs(&["/usr/bin/", "/usr/share/doc/demo/"])),
        (1116, Tag::I32(&[0, 1])),
        (1028, Tag::I32(&[demo.len() as u32, readme.len() as u32])),
        (1030, Tag::I16(&[0o100755, 0o100644])),
        (1034, Tag::I32(&[0x5F00_0000, 0x5F00_0000])),
    ]));
    v.extend_from_slice(&zstd::encode_all(&payload[..], 3).unwrap());

    let container = s.path("indexed-1.0-1.noarch.rpm");
    std::fs::write(&container, v).expect("write rpm");
    let fs = ArchiveFs::new();

    assert_eq!(names(&fs, &container, "/usr/bin").await, vec!["demo"]);
    assert_eq!(read(&fs, &container, "/usr/bin/demo").await, demo);
    assert_eq!(read(&fs, &container, "/usr/share/doc/demo/README").await, readme);

    let entries = formats::list_entries(ArchiveFormat::Rpm, &container).unwrap();
    let e = entries.iter().find(|e| e.path == "/usr/bin/demo").expect("the script");
    assert_eq!(e.mode, Some(0o755), "the mode came from the header, not the payload");
    assert_eq!(e.size, demo.len() as u64, "and so did the size");
}

/// The header signs the payload, so any rewrite would invalidate it.
#[tokio::test]
async fn an_rpm_refuses_to_be_written_to() {
    let s = Scratch::new("rpm-ro");
    let container = make_rpm(&s);
    let fs = ArchiveFs::new();

    assert!(!ArchiveFormat::Rpm.writable());
    assert!(!err_of(write_into(&fs, &container, "/usr/bin/extra", b"nope").await).is_empty());
    assert!(!err_of(fs.mkdir(&at(&container, "/opt")).await).is_empty());
}

/// A file that is not a package at all must be refused clearly rather than
/// producing a confusing empty listing.
#[tokio::test]
async fn a_file_that_is_not_a_package_is_refused() {
    let s = Scratch::new("pkg-bogus");
    let fake_rpm = s.file("bogus.rpm", b"I am not an RPM");
    let fake_deb = s.file("bogus.deb", b"I am not a Debian package");

    assert!(formats::list_entries(ArchiveFormat::Rpm, &fake_rpm).is_err());
    assert!(formats::list_entries(ArchiveFormat::Deb, &fake_deb).is_err());
}
