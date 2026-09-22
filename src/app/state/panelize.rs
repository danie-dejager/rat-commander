//! External panelize (Command → Panelize command output): run a command and put
//! the paths it prints into the panel.
//!
//! The point is to reach a *set* of files that no directory holds — everything
//! `rg -l` matched, everything `git ls-files -m` changed, everything a package
//! owns — and then work on it with the ordinary keys: F3 to look, `+`/Insert to
//! tag, F5 to copy, F8 to delete. The listing this produces is the same kind
//! find-file makes, so all of that comes for free from [`panelize_results`].
//!
//! [`panelize_results`]: super::AppState::panelize_results

use super::*;
use std::path::{Path, PathBuf};

/// Refuse output longer than this. A mistyped command (`cat /dev/urandom`, a
/// `find /`) should stop at something a panel can still draw rather than eat
/// memory until the machine swaps. Matches the cap the ISO backend uses.
const MAX_ENTRIES: usize = 100_000;

impl AppState {
    /// Ask for the command whose output to panelize.
    pub(in crate::app::state) fn open_panelize_dialog(&mut self) {
        // The command runs in the panel's directory through the local shell,
        // which cannot be pointed at an SFTP or in-archive cwd — the same reason
        // the command line refuses to `cd` there.
        if !self.panels[self.active].cwd.is_plain_local() {
            return self.show_error("Panelize needs a local directory");
        }
        self.dialog = Some(Dialog::Input(InputDialog::new(
            "Panelize",
            "Command whose output lists the files:",
            self.last_panelize.clone(),
            InputPurpose::Panelize,
        )));
    }

    /// Run `cmd` in the active panel's directory and collect its stdout. The
    /// result arrives as [`AppEvent::PanelizeDone`].
    pub(in crate::app::state) fn start_panelize(&mut self, cmd: String) {
        if !self.panels[self.active].cwd.is_plain_local() {
            return self.show_error("Panelize needs a local directory");
        }
        self.last_panelize = cmd.clone();
        let cwd = self.panels[self.active].cwd.path.clone();
        let tx = self.tx.clone();
        let handle = tokio::spawn(async move {
            // The user's shell, but *not* interactively: an interactive one
            // takes the terminal's foreground process group and stops the
            // program outright. See `shell::capture_argv`.
            let mut c = crate::shell::capture_command(&cmd);
            c.current_dir(&cwd);
            let result = match c.output().await {
                // A non-zero exit still often prints usable paths (`grep -l`
                // exits 1 when the last file had no match), so the status is not
                // consulted; empty output is what gets reported, with stderr as
                // the explanation since that is where the shell says why.
                Ok(o) if !o.stdout.is_empty() => Ok(o.stdout),
                Ok(o) => {
                    let err = String::from_utf8_lossy(&o.stderr).trim_end().to_string();
                    Err(if err.is_empty() { format!("{cmd}: no output") } else { err })
                }
                Err(e) => Err(format!("Cannot run {cmd}: {e}")),
            };
            let _ = tx.send(AppEvent::PanelizeDone { result }).await;
        });
        self.busy_task = Some(handle);
        self.dialog = Some(Dialog::Busy(
            BusyDialog::new("Panelize", "Running the command…".to_string()).cancellable(),
        ));
    }

    /// The command finished: turn its output into a panelized listing.
    pub(in crate::app::state) fn on_panelize_done(&mut self, result: Result<Vec<u8>, String>) {
        self.busy_task = None; // the command delivered its result
        self.dialog = None;
        let bytes = match result {
            Ok(b) => b,
            Err(e) => return self.show_error(e),
        };
        let cwd = self.panels[self.active].cwd.path.clone();
        let lines = split_output(&bytes);
        // Lines that name nothing reachable are dropped silently: a listing that
        // has gone stale between the command and now is ordinary, and find-file
        // drops vanished matches the same way. Only having *nothing* left is
        // worth interrupting for.
        let hits = resolve_lines(&lines, &cwd);
        if hits.is_empty() {
            // The commonest mistake is a command that prints a *listing* rather
            // than paths — `ls -la` being the obvious one — so say what the
            // output has to look like instead of just refusing.
            let first = match lines.first() {
                Some(l) => format!("\nThe first line was: {l}"),
                None => String::new(),
            };
            return self.show_error(format!(
                "No line of the output named a file that exists.\nEach line has to be a \
                 path on its own, the way `rg -l`, `find` or `git ls-files` print them.{first}"
            ));
        }
        self.panelize_results(hits);
    }
}

/// Split a command's stdout into candidate path strings.
///
/// A NUL anywhere switches to NUL-separated parsing, so `find -print0`,
/// `rg -l --null` and `git ls-files -z` work with nothing to configure — and
/// those are exactly the forms that survive names with a newline in them.
fn split_output(bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let sep = if bytes.contains(&0) { '\0' } else { '\n' };
    text.split(sep)
        .map(|l| l.strip_suffix('\r').unwrap_or(l).trim_end_matches('\0'))
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Resolve `lines` against `cwd`, dropping the ones that name nothing reachable.
///
/// Relative paths join `cwd`, because that is where the command ran. Nothing is
/// canonicalized: that would resolve symlinks and so defeat the obvious
/// `find . -type l`. Existence is tested with `symlink_metadata` for the same
/// reason — a broken symlink is still a file worth listing and deleting.
fn resolve_lines(lines: &[String], cwd: &Path) -> Vec<crate::app::event::FindHit> {
    let mut hits: Vec<crate::app::event::FindHit> = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
    for line in lines {
        if hits.len() >= MAX_ENTRIES {
            break;
        }
        let raw = Path::new(line.as_str());
        let path = if raw.is_absolute() { raw.to_path_buf() } else { cwd.join(raw) };
        // A command run over several patterns repeats paths; the panel should
        // list each file once.
        if !seen.insert(path.clone()) {
            continue;
        }
        if let Ok(m) = std::fs::symlink_metadata(&path) {
            hits.push(crate::app::event::FindHit {
                path: VfsPath::local(path),
                size: m.len(),
                line: None,
            });
        }
    }
    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_output_handles_newlines_crlf_and_blank_lines() {
        assert_eq!(split_output(b"a.txt\nb.txt\n"), ["a.txt", "b.txt"]);
        assert_eq!(split_output(b"a.txt\r\nb.txt\r\n"), ["a.txt", "b.txt"]);
        assert_eq!(split_output(b"\n\na.txt\n\n"), ["a.txt"]);
        assert!(split_output(b"").is_empty());
    }

    #[test]
    fn a_nul_in_the_output_switches_to_nul_separated_parsing() {
        // `find -print0` style: the embedded newline is part of the name, not a
        // separator, which is the whole reason for supporting this form.
        let out = b"we\nird.txt\0plain.txt\0";
        assert_eq!(split_output(out), ["we\nird.txt", "plain.txt"]);
    }

    #[test]
    fn resolve_lines_joins_relative_keeps_absolute_and_drops_missing() {
        let dir = std::env::temp_dir().join(format!("rc-panelize-{}", std::process::id()));
        let sub = dir.join("sub");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(dir.join("a.txt"), b"aa").unwrap();
        std::fs::write(sub.join("b.txt"), b"bbb").unwrap();

        let abs = sub.join("b.txt").to_string_lossy().into_owned();
        let lines = vec![
            "a.txt".to_string(),      // relative → joins cwd
            abs,                      // absolute → kept as-is
            "missing.txt".to_string(),// dropped
            "a.txt".to_string(),      // duplicate → collapsed
        ];
        let hits = resolve_lines(&lines, &dir);

        assert_eq!(hits.len(), 2, "the two real files, deduplicated");
        assert_eq!(hits[0].path, VfsPath::local(dir.join("a.txt")));
        assert_eq!(hits[0].size, 2, "size comes from the file, for the panel column");
        assert_eq!(hits[1].path, VfsPath::local(sub.join("b.txt")));
        assert!(hits.iter().all(|h| h.line.is_none()), "no content hit, so F3 opens at the top");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `ls -la` prints a listing, not paths — and it is the first thing people
    /// try. Every line has to be rejected rather than joined to the cwd and
    /// hopefully missing, so the user gets told what the output should be.
    #[test]
    fn a_listing_rather_than_paths_resolves_to_nothing() {
        let dir = std::env::temp_dir().join(format!("rc-panelize-ls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), b"aa").unwrap();

        let lines: Vec<String> = [
            "total 12",
            "drwxr-xr-x  2 toumal toumal 4096 Jan  1 12:00 .",
            "drwxrwxrwt 20 root   root   4096 Jan  1 12:00 ..",
            "-rw-r--r--  1 toumal toumal    2 Jan  1 12:00 a.txt",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();

        assert!(
            resolve_lines(&lines, &dir).is_empty(),
            "a listing names no files, not even the one it mentions"
        );
        // Plain `ls`, which does print bare names, works as expected.
        assert_eq!(resolve_lines(&["a.txt".to_string()], &dir).len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_broken_symlink_is_still_listed() {
        // It is a file the user may well want to find and delete, and
        // `metadata()` would have hidden it by following the dangling link.
        #[cfg(unix)]
        {
            let dir = std::env::temp_dir().join(format!("rc-panelize-sym-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            std::os::unix::fs::symlink(dir.join("nowhere"), dir.join("dangling")).unwrap();
            let hits = resolve_lines(&["dangling".to_string()], &dir);
            assert_eq!(hits.len(), 1, "the dangling link is listed");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
