//! `git blame` for the viewer: which commit last touched each line of a file.
//!
//! The file is blamed whole, with `--porcelain`, rather than a screenful at a
//! time with `-L`. What blame costs is the walk back through history, and a
//! window of lines needs almost the same walk as the file does; asking once
//! means scrolling never waits on git again.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// The object id git gives lines that are not committed yet.
const UNCOMMITTED: &str = "0000000000000000000000000000000000000000";

/// One commit that owns lines of the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlameCommit {
    pub oid: String,
    pub author: String,
    /// Author time, Unix seconds.
    pub time: i64,
    pub summary: String,
    /// The file's path, relative to the repository root, *in that commit* —
    /// which is not today's path when the file has been renamed since.
    pub path: String,
}

impl BlameCommit {
    /// Whether these are working-tree lines no commit has yet.
    pub fn uncommitted(&self) -> bool {
        self.oid == UNCOMMITTED
    }

    pub fn short(&self) -> &str {
        &self.oid[..self.oid.len().min(7)]
    }
}

/// A blamed file: its commits, and which of them each line belongs to.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Blame {
    pub commits: Vec<BlameCommit>,
    /// Index into `commits` for each line of the file, in order.
    pub lines: Vec<u32>,
    /// The repository's work-tree root, which the commits are relative to.
    pub toplevel: PathBuf,
}

impl Blame {
    /// The commit that owns line `line` (0-based), if the blame covers it.
    pub fn commit_of(&self, line: usize) -> Option<&BlameCommit> {
        self.lines.get(line).and_then(|&i| self.commits.get(i as usize))
    }

    /// Each commit's place in the file's history, for shading by age: `0.0` for
    /// the newest, `1.0` for the oldest, and the rest evenly between them. By
    /// rank rather than by date, so every change in the file is a step of its
    /// own however its dates cluster — two years of quiet and then a busy week
    /// would otherwise leave everything but that week looking equally old.
    /// Uncommitted lines count as the newest of all.
    pub fn age_ranks(&self) -> Vec<f64> {
        let mut times: Vec<i64> =
            self.commits.iter().filter(|c| !c.uncommitted()).map(|c| c.time).collect();
        times.sort_unstable_by(|a, b| b.cmp(a));
        times.dedup();
        let steps = times.len().saturating_sub(1).max(1) as f64;
        self.commits
            .iter()
            .map(|c| {
                if c.uncommitted() {
                    return 0.0;
                }
                times.binary_search_by(|t| c.time.cmp(t)).unwrap_or(0) as f64 / steps
            })
            .collect()
    }
}

/// Blame the local file at `path`. The error is a message fit for a dialog: not
/// in a repository, not tracked, git missing.
pub async fn blame(path: &Path) -> Result<Blame, String> {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("Not a file".to_string());
    };
    let Some(toplevel) = crate::vfs::git::toplevel_of(dir).await else {
        return Err(crate::l10n::tr("Not a git repository"));
    };
    // Run from the file's own directory with its bare name, so a path reached
    // through a symlink never has to be made relative to git's canonical root.
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["blame", "--porcelain", "--"])
        .arg(name)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => "git is not installed".to_string(),
            _ => format!("git blame: {e}"),
        })?;
    if !out.status.success() {
        let msg = String::from_utf8_lossy(&out.stderr);
        let msg = msg.lines().next().unwrap_or("").trim_start_matches("fatal: ");
        return Err(format!("git blame: {msg}"));
    }
    let mut blame = parse_porcelain(&out.stdout);
    blame.toplevel = toplevel;
    Ok(blame)
}

/// Parse `git blame --porcelain` output.
///
/// Each line of the file comes as a header — `<oid> <orig line> <final line>`,
/// with a fourth field on the first line of a group — then, the first time an
/// oid appears, `key value` lines describing its commit, then the line itself
/// behind a tab. Later lines from a commit already described carry only the
/// header, so commits are numbered as they are first seen.
pub fn parse_porcelain(out: &[u8]) -> Blame {
    let mut blame = Blame::default();
    let mut index: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    // The commit and final line the current header announced.
    let mut current: Option<(u32, usize)> = None;
    for raw in out.split(|&b| b == b'\n') {
        if raw.first() == Some(&b'\t') {
            // The line's content: the header before it said where it belongs.
            if let Some((commit, line)) = current.take() {
                if blame.lines.len() <= line {
                    blame.lines.resize(line + 1, commit);
                }
                blame.lines[line] = commit;
            }
            continue;
        }
        let text = String::from_utf8_lossy(raw);
        let mut fields = text.splitn(2, ' ');
        let key = fields.next().unwrap_or("");
        let value = fields.next().unwrap_or("");
        if key.len() == 40 && key.bytes().all(|b| b.is_ascii_hexdigit()) {
            let final_line = value.split(' ').nth(1).and_then(|n| n.parse::<usize>().ok());
            let Some(final_line) = final_line.filter(|&n| n > 0) else { continue };
            let commit = *index.entry(key.to_string()).or_insert_with(|| {
                blame.commits.push(BlameCommit {
                    oid: key.to_string(),
                    author: String::new(),
                    time: 0,
                    summary: String::new(),
                    path: String::new(),
                });
                (blame.commits.len() - 1) as u32
            });
            current = Some((commit, final_line - 1));
            continue;
        }
        let Some((commit, _)) = current else { continue };
        let c = &mut blame.commits[commit as usize];
        match key {
            "author" => c.author = value.to_string(),
            "author-time" => c.time = value.parse().unwrap_or(0),
            "summary" => c.summary = value.to_string(),
            "filename" => c.path = value.to_string(),
            _ => {}
        }
    }
    blame
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn sample() -> String {
        format!(
            "{A} 1 1 2\nauthor Ada\nauthor-mail <ada@example.com>\nauthor-time 1700000000\n\
             author-tz +0000\nsummary First\nfilename src/old.rs\n\tfn main() {{\n\
             {A} 2 2\n\t}}\n\
             {B} 3 3 1\nauthor Bob\nauthor-time 1750000000\nsummary Second\nprevious {A} src/old.rs\n\
             filename src/new.rs\n\t// added\n\
             {UNCOMMITTED} 4 4 1\nauthor Not Committed Yet\nauthor-time 1760000000\n\
             summary Version of new.rs from new.rs\nfilename src/new.rs\n\twip\n\
             {A} 3 5 1\n\t// back to the first\n"
        )
    }

    #[test]
    fn porcelain_maps_every_line_to_its_commit() {
        let b = parse_porcelain(sample().as_bytes());
        assert_eq!(b.commits.len(), 3, "a commit is described once however many lines it owns");
        assert_eq!(b.lines, vec![0, 0, 1, 2, 0]);
        let first = b.commit_of(0).unwrap();
        assert_eq!((first.author.as_str(), first.time), ("Ada", 1_700_000_000));
        assert_eq!(first.summary, "First");
        assert_eq!(first.path, "src/old.rs", "the path as it was in that commit");
        assert_eq!(first.short(), "aaaaaaa");
        assert_eq!(b.commit_of(4).unwrap().oid, A, "a later line of a seen commit");
        assert!(b.commit_of(3).unwrap().uncommitted());
        assert!(b.commit_of(5).is_none());
    }

    #[test]
    fn ages_rank_commits_from_newest_to_oldest() {
        let b = parse_porcelain(sample().as_bytes());
        // Ada's commit is the older of the two; uncommitted work is the newest.
        assert_eq!(b.age_ranks(), vec![1.0, 0.0, 0.0]);
        let mut three = b.clone();
        three.commits[2].oid = "c".repeat(40);
        three.commits[2].time = 1_720_000_000;
        assert_eq!(three.age_ranks(), vec![1.0, 0.0, 0.5], "evenly spaced, not by date");
    }

    fn git(dir: &Path, args: &[&str], date: &str) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env("GIT_AUTHOR_NAME", "Ada")
            .env("GIT_AUTHOR_EMAIL", "ada@example.com")
            .env("GIT_COMMITTER_NAME", "Ada")
            .env("GIT_COMMITTER_EMAIL", "ada@example.com")
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    #[tokio::test]
    async fn a_real_file_is_blamed_line_by_line() {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let dir = std::env::temp_dir().join(format!("rc_blame_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        if !git(&dir, &["init", "-q"], "2026-01-01T00:00:00Z") {
            eprintln!("git unavailable; skipping");
            return;
        }
        let file = dir.join("src/lib.rs");
        std::fs::write(&file, "one\ntwo\n").unwrap();
        git(&dir, &["add", "."], "2026-01-01T00:00:00Z");
        git(&dir, &["commit", "-qm", "Start"], "2026-01-01T00:00:00Z");
        std::fs::write(&file, "one\nTWO\nthree\n").unwrap();
        git(&dir, &["commit", "-qam", "Change"], "2026-02-01T00:00:00Z");
        std::fs::write(&file, "one\nTWO\nthree\nfour\n").unwrap();

        let b = blame(&file).await.expect("blame");
        let summaries: Vec<&str> =
            (0..4).map(|l| b.commit_of(l).unwrap().summary.as_str()).collect();
        assert_eq!(summaries[..3], ["Start", "Change", "Change"]);
        assert!(b.commit_of(3).unwrap().uncommitted(), "the unsaved fourth line");
        assert_eq!(b.commit_of(0).unwrap().path, "src/lib.rs");
        assert_eq!(
            std::fs::canonicalize(&b.toplevel).unwrap(),
            std::fs::canonicalize(&dir).unwrap()
        );

        let stray = dir.join("stray.txt");
        std::fs::write(&stray, "x\n").unwrap();
        assert!(blame(&stray).await.is_err(), "an untracked file has no blame");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
