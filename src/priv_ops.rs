//! Performing a single file operation as root, for the "escalate" answer to a
//! permission-denied prompt.
//!
//! The mechanism is the one the image flasher already uses (see [`crate::flash`]):
//! re-invoke *this* binary through `sudo -n` as a tiny helper that does one
//! thing and exits. `sudo -n` never prompts, because the app has already primed
//! sudo's credential cache with [`crate::mount::sudo_validate`] — so no password
//! is passed to, or held by, the long-running operation.
//!
//! **The privileged surface is deliberately tiny.** The helper accepts four
//! verbs, each taking exactly one or two already-resolved paths, and it never
//! recurses: the unprivileged parent walks the tree and asks for one primitive
//! at a time. Anyone who can run `sudo rc` can already run `sudo cp`, so this
//! grants no new authority — but keeping it primitive means the code running as
//! root is a page long and can be read in one sitting.

use crate::util::{Error, Result};
use std::path::Path;

/// Hidden argv flag that turns this process into the privileged helper.
pub const PRIV_OP_FLAG: &str = "--priv-op";

/// Copy one file's contents (parent must already exist).
pub async fn copy_file(src: &Path, dst: &Path) -> Result<()> {
    run(&["copy", &src.to_string_lossy(), &dst.to_string_lossy()]).await
}

/// Delete one file.
pub async fn remove_file(path: &Path) -> Result<()> {
    run(&["rm", &path.to_string_lossy()]).await
}

/// Delete one (already empty) directory.
pub async fn remove_dir(path: &Path) -> Result<()> {
    run(&["rmdir", &path.to_string_lossy()]).await
}

/// Create one directory.
pub async fn mkdir(path: &Path) -> Result<()> {
    run(&["mkdir", &path.to_string_lossy()]).await
}

/// Whether escalation is even possible here: either we are already root, or
/// `sudo` will run without prompting (its cache has been primed).
pub async fn available() -> bool {
    crate::mount::is_root() || crate::mount::sudo_can_noninteractive().await
}

/// Run one helper verb, as root.
async fn run(args: &[&str]) -> Result<()> {
    let exe = std::env::current_exe()
        .map_err(|e| Error::other(format!("cannot locate our own binary: {e}")))?;

    let mut cmd = if crate::mount::is_root() {
        // Already root: no point going through sudo at all.
        tokio::process::Command::new(&exe)
    } else {
        let mut c = tokio::process::Command::new("sudo");
        c.arg("-n").arg(&exe);
        c
    };
    cmd.arg(PRIV_OP_FLAG).args(args);
    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let out = cmd
        .output()
        .await
        .map_err(|e| Error::other(format!("cannot start the privileged helper: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let msg = String::from_utf8_lossy(&out.stderr);
    let msg = msg.trim();
    Err(Error::other(if msg.is_empty() {
        "the privileged helper failed".to_string()
    } else {
        msg.to_string()
    }))
}

/// The privileged half: entered from `main` when argv[1] is [`PRIV_OP_FLAG`],
/// before the tokio runtime and before any TUI work. Returns a process exit code.
pub fn helper_main(args: &[std::ffi::OsString]) -> i32 {
    // args are everything after the flag itself.
    let verb = args.first().map(|s| s.to_string_lossy().into_owned());
    let result = match (verb.as_deref(), args.get(1), args.get(2)) {
        (Some("copy"), Some(src), Some(dst)) => helper_copy(Path::new(src), Path::new(dst)),
        (Some("rm"), Some(path), None) => std::fs::remove_file(Path::new(path)),
        (Some("rmdir"), Some(path), None) => std::fs::remove_dir(Path::new(path)),
        (Some("mkdir"), Some(path), None) => {
            std::fs::create_dir(Path::new(path)).and_then(|()| give_to_caller(Path::new(path)))
        }
        _ => {
            eprintln!("{PRIV_OP_FLAG} takes: copy <src> <dst> | rm <path> | rmdir <path> | mkdir <path>");
            return 2;
        }
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e}");
            1
        }
    }
}

fn helper_copy(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::copy(src, dst)?;
    give_to_caller(dst)
}

/// Hand a file the helper created back to the user who asked for it, so an
/// escalated copy doesn't leave root-owned files in their home directory.
/// `sudo` exports the original ids; without them (already root) there is nobody
/// to hand it to and the file stays as it is.
#[cfg(unix)]
fn give_to_caller(path: &Path) -> std::io::Result<()> {
    let uid = std::env::var("SUDO_UID").ok().and_then(|v| v.parse::<u32>().ok());
    let gid = std::env::var("SUDO_GID").ok().and_then(|v| v.parse::<u32>().ok());
    let (Some(uid), Some(gid)) = (uid, gid) else {
        return Ok(());
    };
    nix::unistd::chown(path, Some(nix::unistd::Uid::from_raw(uid)), Some(nix::unistd::Gid::from_raw(gid)))
        .map_err(std::io::Error::from)
}

#[cfg(not(unix))]
fn give_to_caller(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
