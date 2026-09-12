//! Tests for the shared container tree.

use super::*;

fn file(b: &mut TreeBuilder<u32>, path: &str, size: u64, payload: u32) {
    b.insert(path, VfsKind::File, size, Meta::default(), payload);
}

fn names(t: &VfsTree<u32>, dir: &str) -> Vec<String> {
    let mut n: Vec<String> = t.read_dir(dir).unwrap().into_iter().map(|e| e.name).collect();
    n.sort();
    n
}

#[test]
fn normalize_folds_separators_and_strips_empties() {
    assert_eq!(normalize(""), "/");
    assert_eq!(normalize("/"), "/");
    assert_eq!(normalize("a"), "/a");
    assert_eq!(normalize("/a/"), "/a");
    assert_eq!(normalize("//a//b//"), "/a/b");
    assert_eq!(normalize("./a/./b"), "/a/b");
    // A Windows `PathBuf::join` inserts a backslash; it still addresses the same
    // member, so the separator is folded rather than treated as a name.
    assert_eq!(normalize("\\a\\b"), "/a/b");
}

#[test]
fn base_name_and_parent_walk_back_to_the_root() {
    assert_eq!(base_name("/a/b"), "b");
    assert_eq!(base_name("/a"), "a");
    assert_eq!(base_name("/"), "");
    assert_eq!(parent_inner("/a/b"), "/a");
    assert_eq!(parent_inner("/a"), "/");
    assert_eq!(parent_inner("/"), "/");
}

#[test]
fn intermediate_directories_are_synthesized() {
    let mut b = TreeBuilder::<u32>::new(None);
    file(&mut b, "a/b/c.txt", 7, 1);
    let t = b.finish();

    assert_eq!(names(&t, "/"), ["a"]);
    assert_eq!(names(&t, "/a"), ["b"]);
    assert_eq!(names(&t, "/a/b"), ["c.txt"]);
    assert!(t.is_dir("/a") && t.is_dir("/a/b"));
    assert_eq!(t.stat("/a/b/c.txt").unwrap().size, 7);
    assert_eq!(*t.payload("/a/b/c.txt").unwrap(), 1);
}

/// The rule the two hand-rolled copies disagreed about. `archive` promoted such
/// a name to a directory; `extfs` let the file supersede it, which hid the whole
/// subtree. A directory wins, whichever order the members arrive in.
#[test]
fn a_name_that_is_a_directory_anywhere_stays_one() {
    for reversed in [false, true] {
        let mut b = TreeBuilder::<u32>::new(None);
        // Some writers store a folder as a zero-length *file* entry alongside
        // its children.
        if reversed {
            file(&mut b, "a/inside.txt", 3, 2);
            file(&mut b, "a", 0, 1);
        } else {
            file(&mut b, "a", 0, 1);
            file(&mut b, "a/inside.txt", 3, 2);
        }
        let t = b.finish();

        assert_eq!(t.stat("/a").unwrap().kind, VfsKind::Dir, "reversed={reversed}");
        assert_eq!(names(&t, "/a"), ["inside.txt"], "the subtree is not hidden");
    }
}

/// `stat` reads the parent's child list, so it cannot report a kind that
/// `read_dir` does not also show.
#[test]
fn stat_and_read_dir_agree_on_every_name() {
    let mut b = TreeBuilder::<u32>::new(None);
    file(&mut b, "a", 0, 1);
    file(&mut b, "a/b/c", 5, 2);
    file(&mut b, "d.txt", 9, 3);
    let t = b.finish();

    for dir in ["/", "/a", "/a/b"] {
        for e in t.read_dir(dir).unwrap() {
            let full =
                if dir == "/" { format!("/{}", e.name) } else { format!("{dir}/{}", e.name) };
            let s = t.stat(&full).unwrap();
            assert_eq!(s.kind, e.kind, "{full}");
            assert_eq!(s.size, e.size, "{full}");
            // Every name `stat` calls a directory can also be listed.
            assert_eq!(s.kind.is_dir(), t.read_dir(&full).is_ok(), "{full}");
        }
    }
}

#[test]
fn the_root_stats_as_a_nameless_directory() {
    let t = TreeBuilder::<u32>::new(None).finish();
    let root = t.stat("/").unwrap();
    assert_eq!(root.kind, VfsKind::Dir);
    assert!(root.name.is_empty());
    assert!(t.read_dir("/").unwrap().is_empty());
}

#[test]
fn a_missing_path_is_not_found() {
    let mut b = TreeBuilder::<u32>::new(None);
    file(&mut b, "a.txt", 1, 1);
    let t = b.finish();
    assert!(matches!(t.stat("/nope").unwrap_err(), Error::NotFound(_)));
    assert!(matches!(t.read_dir("/nope").unwrap_err(), Error::NotFound(_)));
    assert!(matches!(t.payload("/nope").unwrap_err(), Error::NotFound(_)));
    // A file is not a directory, so it has no listing.
    assert!(t.read_dir("/a.txt").is_err());
}

#[test]
fn an_entry_without_its_own_mtime_falls_back_to_the_containers() {
    let stamp = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_000);
    let own = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2_000);
    let mut b = TreeBuilder::<u32>::new(Some(stamp));
    file(&mut b, "bare.txt", 1, 0);
    b.insert("stamped.txt", VfsKind::File, 1, Meta { mtime: Some(own), ..Default::default() }, 0);
    let t = b.finish();

    assert_eq!(t.stat("/bare.txt").unwrap().mtime, Some(stamp));
    assert_eq!(t.stat("/stamped.txt").unwrap().mtime, Some(own));
    // A synthesized directory has none of its own either.
    assert_eq!(t.stat("/").unwrap().mtime, Some(stamp));
}

#[test]
fn mode_and_symlink_target_survive_the_graft() {
    let mut b = TreeBuilder::<u32>::new(None);
    b.insert("bin/run", VfsKind::File, 4, Meta::mode(0o755), 0);
    let meta = Meta { symlink_target: Some("../bin/run".into()), ..Default::default() };
    b.insert("link", VfsKind::Symlink, 0, meta, 0);
    let t = b.finish();

    let run = t.stat("/bin/run").unwrap();
    assert_eq!(run.mode, Some(0o755));
    assert!(run.is_executable(), "the exec bit drives exec-first sort");
    let link = t.stat("/link").unwrap();
    assert_eq!(link.kind, VfsKind::Symlink);
    assert_eq!(link.symlink_target.as_deref(), Some("../bin/run"));
}

#[test]
fn an_explicit_directory_entry_keeps_its_payload_and_metadata() {
    let mut b = TreeBuilder::<u32>::new(None);
    b.insert("d", VfsKind::Dir, 0, Meta::mode(0o750), 42);
    let t = b.finish();
    assert_eq!(t.stat("/d").unwrap().kind, VfsKind::Dir);
    assert_eq!(t.stat("/d").unwrap().mode, Some(0o750));
    assert_eq!(*t.payload("/d").unwrap(), 42, "an ISO directory carries its own extent");
    assert!(t.read_dir("/d").unwrap().is_empty());
}

#[test]
fn entry_count_tracks_what_has_been_grafted() {
    let mut b = TreeBuilder::<u32>::new(None);
    assert_eq!(b.entry_count(), 0);
    file(&mut b, "a/b/c.txt", 1, 0);
    // `a`, `a/b`, `a/b/c.txt`.
    assert_eq!(b.entry_count(), 3);
    file(&mut b, "a/b/d.txt", 1, 0);
    assert_eq!(b.entry_count(), 4);
}

#[tokio::test]
async fn the_cache_rebuilds_only_when_the_stamp_changes() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let cache: TreeCache<u64, u32> = TreeCache::new();
    let builds = AtomicUsize::new(0);
    let key = Path::new("/tmp/container");

    let build = || async {
        builds.fetch_add(1, Ordering::SeqCst);
        let mut b = TreeBuilder::<u32>::new(None);
        file(&mut b, "a.txt", 1, 0);
        Ok(b.finish())
    };

    cache.get_or_build(key, 1, build).await.unwrap();
    cache.get_or_build(key, 1, build).await.unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 1, "an unchanged stamp reuses the tree");

    cache.get_or_build(key, 2, build).await.unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 2, "a changed stamp rebuilds");

    // An explicit drop beats a stamp too coarse to have noticed our own write.
    cache.invalidate(key);
    cache.get_or_build(key, 2, build).await.unwrap();
    assert_eq!(builds.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn a_stamp_for_a_container_that_cannot_be_read_is_inert() {
    let missing = Path::new("/nonexistent/rc-tree-test");
    assert_eq!(stamp_mtime(missing).await, None);
}
