//! The freedesktop.org trash, implemented directly rather than by shelling out
//! to `gio trash` — the same "no external tools" rule the archives and remote
//! clients follow.
//!
//! Layout (spec: <https://specifications.freedesktop.org/trash-spec/>):
//!
//! * `$XDG_DATA_HOME/Trash/{files,info}` is the **home trash**, used for
//!   anything on the same filesystem as the user's data directory.
//! * Anything on *another* mount (a USB stick, a separate `/home`) cannot be
//!   renamed into the home trash, so it goes to a trash directory on its own
//!   mount: `$topdir/.Trash/$uid` when an admin has pre-created a sticky
//!   `$topdir/.Trash`, otherwise `$topdir/.Trash-$uid`.
//!
//! Each trashed file gets a matching `info/<name>.trashinfo` recording where it
//! came from, so a desktop trash GUI can restore it. The info file is created
//! **first**, with `O_EXCL`, which is what reserves the name against a
//! concurrent trashing of the same basename (the spec's race rule).

use crate::util::{Error, Result};
use std::path::{Path, PathBuf};

/// Where a file is to be trashed, and how.
#[derive(Debug, Clone)]
pub struct TrashPlan {
    /// Final resting place: `<root>/files/<name>`.
    pub dest: PathBuf,
    /// The reserved `<root>/info/<name>.trashinfo`.
    pub info: PathBuf,
    /// Whether the source is on the same filesystem as `dest`, i.e. whether a
    /// plain rename will work. A cross-device trash has to copy and delete.
    pub same_device: bool,
}

/// A resolved trash directory.
#[derive(Debug, Clone)]
pub struct TrashRoot {
    /// The trash directory itself (holding `files/` and `info/`).
    dir: PathBuf,
    /// For a top-directory trash the spec records original paths *relative to
    /// the mount point*, so the trash stays valid if the volume is mounted
    /// elsewhere. `None` for the home trash, which records absolute paths.
    relative_to: Option<PathBuf>,
}

/// Whether trashing is supported at all here.
///
/// The freedesktop layout is a Linux/BSD desktop convention: macOS's `~/.Trash`
/// has no `.trashinfo` sidecars and Finder would not understand ours, and
/// Windows needs the shell's recycle-bin COM API. Both fall back to a permanent
/// delete rather than writing something their own file managers can't restore.
pub fn is_available() -> bool {
    cfg!(all(unix, not(target_os = "macos"))) && home_trash_dir().is_some()
}

/// `$XDG_DATA_HOME/Trash` (i.e. `~/.local/share/Trash` by default).
pub fn home_trash_dir() -> Option<PathBuf> {
    // Tests redirect this at a temp directory rather than mutating `$HOME`,
    // which is process-global (and `set_var` is unsafe in edition 2024).
    #[cfg(test)]
    if let Some(home) = test_home::current() {
        return Some(home.join(".local/share/Trash"));
    }
    directories::BaseDirs::new().map(|d| d.data_dir().join("Trash"))
}

/// Test-only redirection of the home trash. Anything touching the trash is
/// serialized through one lock, because the override is global.
#[cfg(test)]
pub(crate) mod test_home {
    use super::PathBuf;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    static HOME: Mutex<Option<PathBuf>> = Mutex::new(None);

    fn serializer() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    pub(crate) fn current() -> Option<PathBuf> {
        HOME.lock().ok().and_then(|h| h.clone())
    }

    /// Points the home trash at `home` until the returned guard is dropped, and
    /// holds the lock so trash-touching tests don't run concurrently.
    pub(crate) struct TempHome(#[allow(dead_code)] MutexGuard<'static, ()>);

    impl TempHome {
        pub(crate) fn set(home: &std::path::Path) -> TempHome {
            // A panicking test would otherwise poison the lock for every later
            // one; the override is replaced wholesale here anyway.
            let guard = serializer().lock().unwrap_or_else(|e| e.into_inner());
            *HOME.lock().unwrap_or_else(|e| e.into_inner()) = Some(home.to_path_buf());
            TempHome(guard)
        }
    }

    impl Drop for TempHome {
        fn drop(&mut self) {
            *HOME.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
    }
}

/// The `files/` subdirectory of the home trash — where the panel navigates for
/// "Go to Trash".
pub fn home_trash_files() -> Option<PathBuf> {
    home_trash_dir().map(|d| d.join("files"))
}

/// Reserve a slot in the appropriate trash for `src`, writing its `.trashinfo`.
/// The caller then moves `src` to [`TrashPlan::dest`], or calls [`rollback`].
pub fn reserve(src: &Path) -> Result<TrashPlan> {
    let root = root_for(src)?;
    reserve_in(&root, src)
}

/// Undo a [`reserve`] whose move then failed, so a stale `.trashinfo` doesn't
/// accumulate pointing at a file that was never trashed.
pub fn rollback(plan: &TrashPlan) {
    let _ = std::fs::remove_file(&plan.info);
}

/// Pick the trash directory `src` belongs in, creating it if needed.
fn root_for(src: &Path) -> Result<TrashRoot> {
    let home = home_trash_dir().ok_or_else(|| Error::other("no home directory for the trash"))?;
    let src_dev = device_of(parent_of(src));

    // Same filesystem as the home trash? Then it is just a rename away. The
    // trash directory may not exist yet on a fresh account, so the comparison
    // uses the nearest ancestor that does — walking up rather than jumping to
    // `/`, which would report the root filesystem's device and wrongly send
    // anything on another mount (a tmpfs `/tmp`, say) down the top-dir path.
    let home_dev = device_of_nearest_existing(&home);
    if src_dev.is_some() && src_dev == home_dev {
        ensure_trash_dirs(&home)?;
        return Ok(TrashRoot { dir: home, relative_to: None });
    }

    // Otherwise the file lives on another mount and has to be trashed there.
    let top = top_dir_for(src)
        .ok_or_else(|| Error::other("cannot find the mount point holding this file"))?;
    let dir = top_dir_trash(&top)?;
    ensure_trash_dirs(&dir)?;
    Ok(TrashRoot { dir, relative_to: Some(top) })
}

/// The trash directory to use on a foreign mount: an admin-provided sticky
/// `$topdir/.Trash/$uid` if there is one, else our own `$topdir/.Trash-$uid`.
fn top_dir_trash(top: &Path) -> Result<PathBuf> {
    let uid = current_uid();
    let shared = top.join(".Trash");
    if is_sticky_dir(&shared) {
        return Ok(shared.join(uid.to_string()));
    }
    Ok(top.join(format!(".Trash-{uid}")))
}

/// Write the `.trashinfo` (which reserves the name) and return the plan.
fn reserve_in(root: &TrashRoot, src: &Path) -> Result<TrashPlan> {
    let files = root.dir.join("files");
    let info_dir = root.dir.join("info");
    let original = absolute(src);
    // The path recorded in the info file: relative to the mount point for a
    // top-directory trash, absolute for the home trash.
    let recorded = match &root.relative_to {
        Some(top) => original.strip_prefix(top).unwrap_or(&original).to_path_buf(),
        None => original.clone(),
    };

    let base = src
        .file_name()
        .ok_or_else(|| Error::other("cannot trash a path with no file name"))?
        .to_string_lossy()
        .into_owned();

    // Claim a free name. Creating the info file with O_EXCL is the lock: if two
    // processes race for the same basename, exactly one wins each candidate.
    for attempt in 1..=1000u32 {
        let candidate = suffixed(&base, attempt);
        let info = info_dir.join(format!("{candidate}.trashinfo"));
        let dest = files.join(&candidate);
        // A leftover file with no info sidecar shouldn't be silently replaced.
        if dest.symlink_metadata().is_ok() {
            continue;
        }
        match std::fs::OpenOptions::new().write(true).create_new(true).open(&info) {
            Ok(mut handle) => {
                use std::io::Write;
                let body = trashinfo_body(&recorded, now_local_iso());
                handle.write_all(body.as_bytes())?;
                handle.flush()?;
                let same_device = device_of(parent_of(src)) == device_of(&files);
                return Ok(TrashPlan { dest, info, same_device });
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::other("too many files of that name are already in the trash"))
}

/// `name`, `name (2)`, `name (3)`, … keeping any extension last so the trash
/// still shows recognisable file types.
fn suffixed(base: &str, attempt: u32) -> String {
    if attempt == 1 {
        return base.to_string();
    }
    match base.rsplit_once('.') {
        // Leading dot = a dotfile, not an extension.
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({attempt}).{ext}"),
        _ => format!("{base} ({attempt})"),
    }
}

/// The `.trashinfo` file body.
fn trashinfo_body(original: &Path, deleted_at: String) -> String {
    format!("[Trash Info]\nPath={}\nDeletionDate={}\n", encode_path(original), deleted_at)
}

/// Percent-encode a path for the `Path=` field: everything outside RFC 2396's
/// unreserved set is escaped, but `/` is kept so the value stays readable as a
/// path (as the spec requires).
fn encode_path(path: &Path) -> String {
    const UNRESERVED: &str = "-_.!~*'()";
    let mut out = String::new();
    for byte in path.to_string_lossy().bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || UNRESERVED.contains(ch) || ch == '/' {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `YYYY-MM-DDThh:mm:ss`, the spec's format.
fn now_local_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (y, mo, d, h, mi, s) = crate::util::bytes::civil_parts(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}")
}

fn ensure_trash_dirs(root: &Path) -> Result<()> {
    let files = root.join("files");
    let info = root.join("info");
    std::fs::create_dir_all(&files)?;
    std::fs::create_dir_all(&info)?;
    // All three, not just the root: `create_dir_all` uses the umask, which
    // typically leaves the subdirectories world-readable.
    restrict_to_owner(root);
    restrict_to_owner(&files);
    restrict_to_owner(&info);
    Ok(())
}

/// The trash holds whatever the user deleted, so it must not be world-readable.
#[cfg(unix)]
fn restrict_to_owner(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700));
}

#[cfg(not(unix))]
fn restrict_to_owner(_root: &Path) {}

fn parent_of(path: &Path) -> &Path {
    path.parent().unwrap_or(Path::new("/"))
}

fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    match std::env::current_dir() {
        Ok(cwd) => cwd.join(path),
        Err(_) => path.to_path_buf(),
    }
}

/// The device of `path`, or of the closest ancestor that exists. Creating a
/// directory does not change which filesystem it lands on, so an ancestor's
/// device is the right answer for a path that is about to be created.
fn device_of_nearest_existing(path: &Path) -> Option<u64> {
    let mut cursor = Some(path);
    while let Some(p) = cursor {
        if let Some(dev) = device_of(p) {
            return Some(dev);
        }
        cursor = p.parent();
    }
    None
}

#[cfg(unix)]
fn device_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.dev())
}

#[cfg(not(unix))]
fn device_of(_path: &Path) -> Option<u64> {
    None
}

#[cfg(unix)]
fn current_uid() -> u32 {
    nix::unistd::Uid::effective().as_raw()
}

#[cfg(not(unix))]
fn current_uid() -> u32 {
    0
}

/// A `$topdir/.Trash` is only trustworthy if the admin made it sticky and it is
/// a real directory — a symlink there would let someone redirect other users'
/// deleted files, which is exactly why the spec demands both checks.
#[cfg(unix)]
fn is_sticky_dir(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    meta.is_dir() && !meta.file_type().is_symlink() && meta.permissions().mode() & 0o1000 != 0
}

#[cfg(not(unix))]
fn is_sticky_dir(_path: &Path) -> bool {
    false
}

/// The mount point holding `path`: walk up until the device number changes.
/// This needs no `/proc` parsing and is exact by construction.
fn top_dir_for(path: &Path) -> Option<PathBuf> {
    let start = absolute(path);
    let dev = device_of(parent_of(&start))?;
    let mut best = parent_of(&start).to_path_buf();
    let mut cursor = best.clone();
    while let Some(parent) = cursor.parent() {
        match device_of(parent) {
            Some(d) if d == dev => {
                best = parent.to_path_buf();
                cursor = parent.to_path_buf();
            }
            // Crossed a mount boundary (or can't stat): `best` is the top.
            _ => break,
        }
    }
    Some(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_dir(tag: &str) -> PathBuf {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rc_trash_{tag}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A trash root inside a temp dir, so no test ever touches the real
    /// `~/.local/share/Trash`.
    fn test_root(dir: &Path) -> TrashRoot {
        let root = dir.join("Trash");
        ensure_trash_dirs(&root).unwrap();
        TrashRoot { dir: root, relative_to: None }
    }

    #[test]
    fn encode_path_escapes_specials_but_keeps_slashes() {
        assert_eq!(encode_path(Path::new("/home/u/plain.txt")), "/home/u/plain.txt");
        // A space and a percent must be escaped; the slashes must not.
        assert_eq!(encode_path(Path::new("/a b/c%d")), "/a%20b/c%25d");
        // Unreserved punctuation survives as-is.
        assert_eq!(encode_path(Path::new("/x/-_.!~*'()")), "/x/-_.!~*'()");
        // Non-ASCII is percent-encoded per UTF-8 byte.
        assert_eq!(encode_path(Path::new("/ü")), "/%C3%BC");
    }

    #[test]
    fn trashinfo_body_has_the_spec_shape() {
        let body = trashinfo_body(Path::new("/home/u/a b.txt"), "2026-09-10T18:30:00".into());
        assert_eq!(
            body,
            "[Trash Info]\nPath=/home/u/a%20b.txt\nDeletionDate=2026-09-10T18:30:00\n"
        );
    }

    #[test]
    fn the_deletion_date_is_iso_8601_to_the_second() {
        let now = now_local_iso();
        assert_eq!(now.len(), 19, "{now}");
        let (date, time) = now.split_once('T').expect("a T separator");
        assert_eq!(date.split('-').count(), 3);
        assert_eq!(time.split(':').count(), 3);
    }

    #[test]
    fn collision_names_keep_the_extension_last() {
        assert_eq!(suffixed("notes.txt", 1), "notes.txt");
        assert_eq!(suffixed("notes.txt", 2), "notes (2).txt");
        assert_eq!(suffixed("notes.txt", 3), "notes (3).txt");
        // No extension, and a dotfile (whose leading dot is not an extension).
        assert_eq!(suffixed("README", 2), "README (2)");
        assert_eq!(suffixed(".bashrc", 2), ".bashrc (2)");
    }

    #[test]
    fn reserving_twice_claims_two_different_names() {
        let dir = tmp_dir("reserve");
        let root = test_root(&dir);
        let src = dir.join("a.txt");
        std::fs::write(&src, b"x").unwrap();

        let first = reserve_in(&root, &src).unwrap();
        let second = reserve_in(&root, &src).unwrap();

        assert_eq!(first.dest.file_name().unwrap(), "a.txt");
        assert_eq!(second.dest.file_name().unwrap(), "a (2).txt");
        assert_ne!(first.info, second.info);
        // Both info files exist: the first reservation still holds its name.
        assert!(first.info.is_file() && second.info.is_file());

        // And each records where the file came from.
        let body = std::fs::read_to_string(&first.info).unwrap();
        assert!(body.contains(&format!("Path={}", encode_path(&src))), "{body}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rollback_frees_the_reserved_name_again() {
        let dir = tmp_dir("rollback");
        let root = test_root(&dir);
        let src = dir.join("a.txt");
        std::fs::write(&src, b"x").unwrap();

        let plan = reserve_in(&root, &src).unwrap();
        assert!(plan.info.is_file());
        rollback(&plan);
        assert!(!plan.info.exists(), "the info file is gone");

        // The name is free again, so the next reservation reuses it rather than
        // drifting to "a (2).txt" forever.
        let again = reserve_in(&root, &src).unwrap();
        assert_eq!(again.dest.file_name().unwrap(), "a.txt");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_top_directory_trash_records_paths_relative_to_the_mount() {
        let dir = tmp_dir("topdir");
        let root_dir = dir.join(".Trash-1000");
        ensure_trash_dirs(&root_dir).unwrap();
        // Pretend `dir` is the mount point.
        let root = TrashRoot { dir: root_dir, relative_to: Some(dir.clone()) };

        let src = dir.join("sub/a.txt");
        std::fs::create_dir_all(src.parent().unwrap()).unwrap();
        std::fs::write(&src, b"x").unwrap();

        let plan = reserve_in(&root, &src).unwrap();
        let body = std::fs::read_to_string(&plan.info).unwrap();
        assert!(body.contains("Path=sub/a.txt"), "recorded relative to the mount: {body}");
        assert!(!body.contains(&dir.to_string_lossy().to_string()), "not absolute: {body}");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn the_trash_directory_is_not_world_readable() {
        let dir = tmp_dir("perms");
        let root = test_root(&dir);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&root.dir).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "others must not be able to read the trash");
        }
        assert!(root.dir.join("files").is_dir() && root.dir.join("info").is_dir());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn top_dir_for_finds_a_mount_point_that_contains_the_file() {
        let dir = tmp_dir("topfind");
        let src = dir.join("a.txt");
        std::fs::write(&src, b"x").unwrap();
        let top = top_dir_for(&src).expect("some mount point");
        // Whatever it is, it must be an ancestor of the file and a real dir.
        assert!(top.is_dir());
        assert!(absolute(&src).starts_with(&top), "{top:?} should contain {src:?}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_reserved_plan_reports_whether_a_rename_will_do() {
        let dir = tmp_dir("device");
        let root = test_root(&dir);
        let src = dir.join("a.txt");
        std::fs::write(&src, b"x").unwrap();
        let plan = reserve_in(&root, &src).unwrap();
        // The temp dir and its own subdirectory are necessarily one filesystem.
        assert!(plan.same_device, "same-filesystem trashing should be a rename");
        std::fs::remove_dir_all(&dir).ok();
    }
}
