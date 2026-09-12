//! Tests for the `git://` backend.
//!
//! The parsers are exercised against hand-written bytes, so they need no
//! repository and no `git`. The end-to-end tests build a throwaway repo and skip
//! themselves cleanly where `git` is unavailable, the way `crate::git`'s own do.

use super::*;

// ---------------------------------------------------------------------------
// Parsing `git ls-tree -r -l -z`
// ---------------------------------------------------------------------------

/// One NUL-terminated record: `<mode> <type> <oid> <size>\t<path>`.
fn rec(mode: &str, kind: &str, oid: &str, size: &str, path: &str) -> Vec<u8> {
    let mut v = format!("{mode} {kind} {oid} {size:>7}\t{path}").into_bytes();
    v.push(0);
    v
}

fn parse(records: &[Vec<u8>]) -> VfsTree<BlobRef> {
    let data: Vec<u8> = records.concat();
    parse_ls_tree_z(&data, None).unwrap()
}

#[test]
fn a_plain_blob_keeps_its_size_and_object_id() {
    let t = parse(&[rec("100644", "blob", "aaaa111", "1618", "ci.yml")]);
    let e = t.stat("/ci.yml").unwrap();
    assert_eq!(e.kind, VfsKind::File);
    assert_eq!(e.size, 1618);
    assert_eq!(e.mode, Some(0o644));
    assert_eq!(t.payload("/ci.yml").unwrap().oid, "aaaa111");
}

#[test]
fn an_executable_blob_carries_the_exec_bit() {
    let t = parse(&[rec("100755", "blob", "bbbb222", "42", "run.sh")]);
    let e = t.stat("/run.sh").unwrap();
    assert_eq!(e.mode, Some(0o755));
    assert!(e.is_executable(), "the exec bit is what drives exec-first sort");
}

#[test]
fn mode_120000_is_a_symlink() {
    let t = parse(&[rec("120000", "blob", "cccc333", "12", "link")]);
    assert_eq!(t.stat("/link").unwrap().kind, VfsKind::Symlink);
    // The target is the blob's contents, read on demand — never during a
    // listing, which would be one process per link.
    assert_eq!(t.stat("/link").unwrap().symlink_target, None);
}

/// A gitlink's objects live in another repository, so it is shown as an empty
/// directory rather than descended into. Its size column is `-`, not a number.
#[test]
fn a_submodule_is_an_empty_directory() {
    let t = parse(&[rec("160000", "commit", "dddd444", "-", "vendor/lib")]);
    assert_eq!(t.stat("/vendor/lib").unwrap().kind, VfsKind::Dir);
    assert_eq!(t.stat("/vendor/lib").unwrap().size, 0);
    assert!(t.read_dir("/vendor/lib").unwrap().is_empty());
}

/// The metadata half of a record never contains a tab, but a filename may — so
/// the split is at the *first* tab, not on every one.
#[test]
fn a_filename_containing_a_tab_parses_whole() {
    let t = parse(&[rec("100644", "blob", "eeee555", "3", "we\tird.txt")]);
    assert_eq!(t.stat("/we\tird.txt").unwrap().size, 3);
    let names: Vec<String> = t.read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["we\tird.txt"]);
}

/// `-z` hands us raw bytes, so a non-UTF-8 name arrives lossily decoded.
/// `VfsPath::has_lossy_name` is what then refuses to act on it.
#[test]
fn a_non_utf8_name_is_decoded_lossily_rather_than_dropped() {
    let mut r = b"100644 blob ffff666      5\tbad".to_vec();
    r.push(0xff);
    r.push(b'\0');
    let t = parse_ls_tree_z(&r, None).unwrap();
    let names: Vec<String> = t.read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names.len(), 1);
    assert!(names[0].contains('\u{FFFD}'), "got {names:?}");
}

#[test]
fn nested_paths_grow_the_directories_they_imply() {
    let t = parse(&[
        rec("100644", "blob", "a1", "10", "src/vfs/mod.rs"),
        rec("100644", "blob", "a2", "20", "src/vfs/git/mod.rs"),
        rec("100644", "blob", "a3", "30", "README.md"),
    ]);
    let mut root: Vec<String> = t.read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
    root.sort();
    assert_eq!(root, ["README.md", "src"]);
    assert_eq!(t.stat("/src").unwrap().kind, VfsKind::Dir);
    assert_eq!(t.stat("/src/vfs/git/mod.rs").unwrap().size, 20);
}

#[test]
fn a_tree_beyond_the_cap_is_refused_rather_than_eaten() {
    let records: Vec<Vec<u8>> =
        (0..=MAX_TREE_ENTRIES).map(|i| rec("100644", "blob", "a", "1", &format!("f{i}"))).collect();
    let data: Vec<u8> = records.concat();
    let err = parse_ls_tree_z(&data, None).unwrap_err();
    assert!(err.to_string().contains("too large to browse"), "{err}");
}

#[test]
fn empty_and_malformed_records_are_skipped() {
    let mut data = rec("100644", "blob", "a1", "5", "good.txt");
    data.extend_from_slice(b"\0");
    data.extend_from_slice(b"no-tab-here\0");
    data.extend_from_slice(b"100644 blob\tonly-two-fields\0");
    let t = parse_ls_tree_z(&data, None).unwrap();
    let names: Vec<String> = t.read_dir("/").unwrap().into_iter().map(|e| e.name).collect();
    assert_eq!(names, ["good.txt"]);
}

#[test]
fn every_entry_falls_back_to_the_commits_timestamp() {
    let when = SystemTime::UNIX_EPOCH + Duration::from_secs(1_789_156_340);
    let t = parse_ls_tree_z(&rec("100644", "blob", "a1", "5", "a/b.txt"), Some(when)).unwrap();
    assert_eq!(t.stat("/a/b.txt").unwrap().mtime, Some(when));
    assert_eq!(t.stat("/a").unwrap().mtime, Some(when), "and so does a synthesized directory");
}

// ---------------------------------------------------------------------------
// Parsing `git log`
// ---------------------------------------------------------------------------

#[test]
fn the_revision_log_parses_into_commits() {
    let text = "aaa111\tAAA\t1789211162\tStreamline readme\n\
                bbb222\tBBB\t1789156362\tBump version\n";
    let revs = parse_rev_log(text);
    assert_eq!(revs.len(), 2);
    assert_eq!(revs[0].oid, "aaa111");
    assert_eq!(revs[0].short, "AAA");
    assert_eq!(revs[0].time, 1_789_211_162);
    assert_eq!(revs[0].subject, "Streamline readme");
    assert_eq!(revs[1].subject, "Bump version");
}

/// A subject may hold anything, tabs included, so only the three fields before
/// it are split off.
#[test]
fn a_subject_containing_tabs_and_slashes_survives() {
    let revs = parse_rev_log("aaa\tAAA\t100\tfix\tthe a/b thing\n");
    assert_eq!(revs.len(), 1);
    assert_eq!(revs[0].subject, "fix\tthe a/b thing");
}

#[test]
fn malformed_log_lines_are_skipped() {
    let text = "aaa\tAAA\tnot-a-number\tbad time\n\
                short-line\n\
                \n\
                bbb\tBBB\t100\tgood\n";
    let revs = parse_rev_log(text);
    assert_eq!(revs.len(), 1);
    assert_eq!(revs[0].subject, "good");
}

#[test]
fn an_empty_subject_is_allowed() {
    let revs = parse_rev_log("aaa\tAAA\t100\t\n");
    assert_eq!(revs.len(), 1);
    assert_eq!(revs[0].subject, "");
}

// ---------------------------------------------------------------------------
// Revision components
// ---------------------------------------------------------------------------

fn rev(short: &str, time: i64, subject: &str) -> Rev {
    Rev { oid: format!("{short}full"), short: short.into(), time, subject: subject.into() }
}

/// The panel's default sort is by name, so a listing of commits is only useful
/// if name order is time order.
#[test]
fn revision_components_sort_chronologically_by_name() {
    let older = rev("aaaaaaa", 1_700_000_000, "older").component();
    let newer = rev("bbbbbbb", 1_789_156_340, "newer").component();
    assert!(older < newer, "{older} should sort before {newer}");
    assert!(newer.starts_with("2026-"), "got {newer}");
}

/// Most of a repository's commits share a date, so a day-resolution name would
/// leave them ordered by their abbreviated object id — which is to say at
/// random. The time is carried to the second for exactly this case.
#[test]
fn commits_on_the_same_day_still_sort_by_time_not_by_object_id() {
    // Deliberately adversarial: the earlier commit gets the higher-sorting oid.
    let earlier = rev("fffffff", 1_789_156_340, "first").component();
    let later = rev("0000000", 1_789_156_341, "second").component();
    assert!(earlier < later, "{earlier} should sort before {later}");
    assert_eq!(earlier[..10], later[..10], "the two share a date");
}

#[test]
fn a_subject_is_sanitised_into_one_path_component() {
    let c = rev("a9ef3a7", 1_789_156_340, "Made 3d/topography  more static!").component();
    assert!(!c.contains('/'), "a slash would break path joining: {c}");
    assert!(!c.contains(' '), "{c}");
    assert!(!c.contains("--"), "runs of punctuation collapse: {c}");
    assert!(c.contains("a9ef3a7"));
    assert!(c.contains("Made-3d-topography-more-static"), "{c}");
}

#[test]
fn a_long_subject_is_truncated_and_never_ends_in_a_dash() {
    let c = rev("aaaaaaa", 100, &"word ".repeat(60)).component();
    let subject = c.splitn(4, '_').nth(3).unwrap();
    assert!(subject.chars().count() <= SUBJECT_MAX, "{subject}");
    assert!(!subject.ends_with('-'), "{subject}");
}

#[test]
fn an_empty_or_punctuation_only_subject_still_yields_a_component() {
    for subject in ["", "!!!", "   "] {
        let c = rev("aaaaaaa", 100, subject).component();
        assert!(c.contains("aaaaaaa"), "{c}");
        assert_eq!(oid_of_component(&c), Some("aaaaaaa"), "{c}");
    }
}

/// The abbreviated oid is the only part of a component ever handed back to git,
/// which is why sanitising the subject is lossless for our purposes.
#[test]
fn the_object_id_round_trips_out_of_a_component() {
    let r = rev("a9ef3a7", 1_789_156_340, "Anything at all");
    assert_eq!(oid_of_component(&r.component()), Some("a9ef3a7"));
    // Anything that is not a hex field is refused rather than sent to git.
    assert_eq!(oid_of_component("2026-05-11"), None);
    assert_eq!(oid_of_component("2026-05-11_13-45-02"), None);
    assert_eq!(oid_of_component("2026-05-11_13-45-02_nothex_subject"), None);
    assert_eq!(oid_of_component(""), None);
}

// ---------------------------------------------------------------------------
// Path shape
// ---------------------------------------------------------------------------

#[test]
fn an_inner_path_splits_into_a_revision_and_a_path_within_it() {
    assert_eq!(split_rev("/"), None, "the root is the revision list");
    assert_eq!(split_rev(""), None);
    assert_eq!(split_rev("/abc"), Some(("abc".into(), "/".into())));
    assert_eq!(split_rev("/abc/"), Some(("abc".into(), "/".into())));
    assert_eq!(split_rev("/abc/src/main.rs"), Some(("abc".into(), "/src/main.rs".into())));
}

/// `container` carries the `.git` sentinel precisely so that leaving the mount
/// lands in the work tree rather than above the repository.
#[test]
fn leaving_the_mount_lands_in_the_work_tree() {
    let p = VfsPath::git(container_for(Path::new("/home/u/repo")), "/");
    assert_eq!(p.parent().unwrap().path, PathBuf::from("/home/u/repo"));
    assert!(p.parent().unwrap().is_plain_local());
}

#[test]
fn a_git_path_is_local_and_is_not_a_native_archive() {
    let p = VfsPath::git(container_for(Path::new("/home/u/repo")), "/abc/src");
    // Container-backed, so its files can be extracted to a temp for the viewer
    // and the one-remote invariant does not apply.
    assert!(!p.is_remote());
    assert!(p.is_archive());
    assert!(!p.is_native_archive(), "no archive-rebuild path may touch it");
    assert!(!p.is_plain_local());
}

/// Built from `Rev::component` rather than a hand-written string, so the reader
/// in `display` and the writer here can never drift apart — which they did once,
/// silently, when the component gained its time field.
#[test]
fn the_location_bar_names_the_repository_and_the_revision() {
    let c = container_for(Path::new("/home/u/rat-commander"));
    assert_eq!(VfsPath::git(&c, "/").display(), "git:rat-commander");

    let component = rev("a9ef3a7", 1_789_156_340, "Made it static").component();
    let at = VfsPath::git(&c, format!("/{component}"));
    assert_eq!(at.display(), "git:rat-commander@a9ef3a7:/");
    let deep = VfsPath::git(&c, format!("/{component}/src/space3d"));
    assert_eq!(deep.display(), "git:rat-commander@a9ef3a7:/src/space3d");
}

#[test]
fn joining_inside_a_revision_stays_posix() {
    let p = VfsPath::git(container_for(Path::new("/r")), "/abc/src");
    assert_eq!(p.join("main.rs").path, PathBuf::from("/abc/src/main.rs"));
}

// ---------------------------------------------------------------------------
// End to end, against a real repository
// ---------------------------------------------------------------------------

/// Whether `git` can be run at all; the end-to-end tests skip without it.
fn git_ok() -> bool {
    std::process::Command::new("git")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// A scratch directory that removes itself, sharing the sweepable `rc-tmp-`
/// prefix with the rest of the app.
struct Scratch(PathBuf);

impl Scratch {
    fn new(tag: &str) -> Self {
        let p = crate::util::temp::rc_temp_path(&format!("gittest-{tag}"));
        std::fs::create_dir_all(&p).unwrap();
        Scratch(p)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn run(dir: &Path, args: &[&str]) {
    run_at(dir, args, "2026-05-11T10:00:00+00:00")
}

/// Run a git command with the commit clock pinned, so a fixture's commits get
/// distinct, known timestamps instead of whatever the wall clock says. Two
/// commits made back to back otherwise land in the same second, where revision
/// names legitimately tie.
fn run_at(dir: &Path, args: &[&str], date: &str) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        // Keep the developer's own git config out of the test.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@e")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@e")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(ok.success(), "git {args:?} failed");
}

const FIRST_DATE: &str = "2026-05-11T10:00:00+00:00";
const SECOND_DATE: &str = "2026-05-12T11:30:45+00:00";

/// A repository with two commits: the first has `only-old.txt`, the second
/// replaces it with `only-new.txt` and a nested, executable file.
fn two_commit_repo(tag: &str) -> Scratch {
    let s = Scratch::new(tag);
    let d = &s.0;
    run(d, &["init", "-q", "-b", "main"]);
    std::fs::write(d.join("only-old.txt"), b"old\n").unwrap();
    run(d, &["add", "-A"]);
    run_at(d, &["commit", "-q", "-m", "first commit"], FIRST_DATE);

    std::fs::remove_file(d.join("only-old.txt")).unwrap();
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(d.join("only-new.txt"), b"new\n").unwrap();
    std::fs::write(d.join("sub/deep.bin"), b"\x00\x01\x02binary\xff").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let script = d.join("sub/run.sh");
        std::fs::write(&script, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("only-new.txt", d.join("sub/link")).unwrap();
    }
    run(d, &["add", "-A"]);
    run_at(d, &["commit", "-q", "-m", "second commit"], SECOND_DATE);
    s
}

fn names_of(entries: Vec<VfsEntry>) -> Vec<String> {
    let mut n: Vec<String> = entries.into_iter().map(|e| e.name).collect();
    n.sort();
    n
}

async fn read_all(fs: &GitFs, p: &VfsPath) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut r = fs.open_read(p).await.unwrap();
    let mut buf = Vec::new();
    r.read_to_end(&mut buf).await.unwrap();
    buf
}

#[tokio::test]
async fn each_revision_lists_the_files_it_had() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("list");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);

    // The mount root is the revision list, newest first.
    let revs = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap();
    assert_eq!(revs.len(), 2);
    assert!(revs.iter().all(|e| e.kind == VfsKind::Dir));
    let (newest, oldest) = (revs[0].name.clone(), revs[1].name.clone());
    assert!(newest > oldest, "name order is time order: {newest} vs {oldest}");
    assert!(newest.starts_with("2026-05-12_11-30-45_"), "{newest}");
    assert!(oldest.starts_with("2026-05-11_10-00-00_"), "{oldest}");

    let old = names_of(fs.read_dir(&VfsPath::git(&c, format!("/{oldest}"))).await.unwrap());
    assert_eq!(old, ["only-old.txt"]);

    let new = names_of(fs.read_dir(&VfsPath::git(&c, format!("/{newest}"))).await.unwrap());
    assert!(new.contains(&"only-new.txt".to_string()), "{new:?}");
    assert!(new.contains(&"sub".to_string()), "{new:?}");
    assert!(!new.contains(&"only-old.txt".to_string()), "deleted in the newer commit");
}

#[tokio::test]
async fn a_nested_blob_reads_back_byte_exact() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("read");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);
    let newest = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap()[0].name.clone();

    let p = VfsPath::git(&c, format!("/{newest}/sub/deep.bin"));
    assert_eq!(read_all(&fs, &p).await, b"\x00\x01\x02binary\xff");
    assert_eq!(fs.stat(&p).await.unwrap().size, 10);
}

#[tokio::test]
#[cfg(unix)]
async fn the_exec_bit_and_symlinks_survive_the_round_trip() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("modes");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);
    let newest = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap()[0].name.clone();

    let script = VfsPath::git(&c, format!("/{newest}/sub/run.sh"));
    assert!(fs.stat(&script).await.unwrap().is_executable());

    let link = VfsPath::git(&c, format!("/{newest}/sub/link"));
    assert_eq!(fs.stat(&link).await.unwrap().kind, VfsKind::Symlink);
    assert_eq!(fs.read_link(&link).await.unwrap(), "only-new.txt");
    // A non-symlink has no target to read.
    assert!(fs.read_link(&script).await.is_err());
}

/// History is read-only, and it says so up front rather than failing once a
/// transfer is already under way.
#[tokio::test]
async fn every_mutation_is_refused() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("ro");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);
    let newest = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap()[0].name.clone();
    let p = VfsPath::git(&c, format!("/{newest}/only-new.txt"));

    assert!(!fs.capabilities().writable);
    assert!(matches!(
        fs.open_write(&p, WriteMeta::default()).await.err(),
        Some(Error::Unsupported)
    ));
    assert!(matches!(fs.mkdir(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.remove_file(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.remove_dir(&p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.rename(&p, &p).await.err(), Some(Error::Unsupported)));
    assert!(matches!(fs.set_permissions(&p, 0o644).await.err(), Some(Error::Unsupported)));
}

#[tokio::test]
async fn a_directory_is_not_readable_as_a_file() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("isdir");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);
    let newest = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap()[0].name.clone();
    let err = match fs.open_read(&VfsPath::git(&c, format!("/{newest}/sub"))).await {
        Ok(_) => panic!("a directory is not readable as a file"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("is a directory"), "{err}");
    // Nor is the revision list.
    assert!(fs.open_read(&VfsPath::git(&c, "/")).await.is_err());
}

#[tokio::test]
async fn an_unknown_revision_or_path_is_not_found() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("missing");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);
    let newest = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap()[0].name.clone();

    let bogus = VfsPath::git(&c, "/2026-01-01_deadbee_nope");
    assert!(fs.read_dir(&bogus).await.is_err());
    let not_hex = VfsPath::git(&c, "/not-a-revision");
    assert!(matches!(fs.read_dir(&not_hex).await.err(), Some(Error::NotFound(_))));
    let gone = VfsPath::git(&c, format!("/{newest}/no-such-file"));
    assert!(matches!(fs.stat(&gone).await.err(), Some(Error::NotFound(_))));
}

#[tokio::test]
async fn a_directory_that_is_not_a_repository_reports_it() {
    if !git_ok() {
        return;
    }
    let s = Scratch::new("norepo");
    assert_eq!(toplevel_of(&s.0).await, None);
    let fs = GitFs::new(50);
    assert!(fs.read_dir(&VfsPath::git(container_for(&s.0), "/")).await.is_err());
}

#[tokio::test]
async fn a_revision_can_be_resolved_from_a_rev_spec() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("revspec");
    let head = resolve_rev(&s.0, "HEAD").await.unwrap();
    assert_eq!(head.subject, "second commit");
    let prev = resolve_rev(&s.0, "HEAD~1").await.unwrap();
    assert_eq!(prev.subject, "first commit");
    assert_ne!(head.oid, prev.oid);
    // An unknown spec is refused rather than guessed at.
    assert!(resolve_rev(&s.0, "no-such-branch").await.is_err());
}

#[tokio::test]
async fn a_bare_repository_can_be_browsed() {
    if !git_ok() {
        return;
    }
    let src = two_commit_repo("bare-src");
    let bare = Scratch::new("bare");
    let path = bare.0.join("repo.git");
    let ok = std::process::Command::new("git")
        .args(["clone", "-q", "--bare"])
        .arg(&src.0)
        .arg(&path)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(ok.success());

    // A bare repo has no work tree, so `container_for` is applied to the
    // repository directory itself.
    let fs = GitFs::new(50);
    let revs = fs.read_dir(&VfsPath::git(container_for(&path), "/")).await.unwrap();
    assert_eq!(revs.len(), 2, "a bare repository still lists its history");
}

#[tokio::test]
async fn a_revision_tree_is_parsed_once() {
    if !git_ok() {
        return;
    }
    let s = two_commit_repo("cache");
    let c = container_for(&s.0);
    let fs = GitFs::new(50);
    let newest = fs.read_dir(&VfsPath::git(&c, "/")).await.unwrap()[0].name.clone();

    let rev = fs.rev_for(&c, &newest).await.unwrap();
    let first = fs.tree(&c, &rev).await.unwrap();
    let again = fs.tree(&c, &rev).await.unwrap();
    // A commit's tree is immutable, so the parsed listing is shared rather than
    // rebuilt — this is the one place git is simpler than archive and extfs.
    assert!(Arc::ptr_eq(&first, &again));
}

// ---------------------------------------------------------------------------
// Flattened sizes, for the 3D timeline
// ---------------------------------------------------------------------------

#[test]
fn flattened_sizes_skip_submodules_but_keep_every_blob() {
    let data: Vec<u8> = [
        rec("100644", "blob", "a1", "10", "a.txt"),
        rec("100755", "blob", "a2", "20", "sub/run.sh"),
        rec("120000", "blob", "a3", "5", "sub/link"),
        // A gitlink contributes no bytes of its own, and its size column is `-`.
        rec("160000", "commit", "a4", "-", "vendor/lib"),
    ]
    .concat();
    let mut got = parse_ls_tree_sizes(&data);
    got.sort();
    assert_eq!(
        got,
        vec![
            ("a.txt".to_string(), 10),
            ("sub/link".to_string(), 5),
            ("sub/run.sh".to_string(), 20),
        ]
    );
}

/// The flattened form and the browsing form read the same records, so a file the
/// panel lists must also be a file the 3D view weighs.
#[test]
fn the_flattened_sizes_agree_with_the_browsable_tree() {
    let data: Vec<u8> =
        [rec("100644", "blob", "a1", "42", "src/deep/x.rs"), rec("100644", "blob", "a2", "7", "y")]
            .concat();
    let tree = parse_ls_tree_z(&data, None).unwrap();
    for (path, size) in parse_ls_tree_sizes(&data) {
        assert_eq!(tree.stat(&format!("/{path}")).unwrap().size, size, "{path}");
    }
}
