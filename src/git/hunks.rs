//! Unified diffs, parsed into hunks so one of them can be staged on its own.
//!
//! **Why this exists rather than reusing the Alt-D view's data.** That view is
//! built from `git show HEAD:<file>` and the working file, run through the
//! in-house LCS differ, and its `Delta`s are line *ranges*, not a patch. Four
//! separate things stop them becoming one:
//!
//! 1. The differ splits with `from_utf8_lossy` and strips `\r`, so a CRLF or
//!    non-UTF-8 file would produce context lines that no longer match the blob
//!    byte for byte — and `git apply` rejects on context mismatch.
//! 2. There is no way to express `\ No newline at end of file`.
//! 3. The view compares **HEAD** with the worktree, while `git apply --cached`
//!    needs a patch whose preimage is the **index**. The two differ the moment
//!    anything is staged.
//! 4. The view is editable, so its right-hand side need not match what is on
//!    disk any more.
//!
//! So `git diff` is the source of truth for patches, and the view is only where
//! the cursor lives. Everything here keeps the diff's bytes **verbatim**.

use crate::util::{Error, Result};
use std::path::Path;

/// One `@@` block of a unified diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub old_count: u32,
    pub new_start: u32,
    pub new_count: u32,
    /// The `@@ … @@` line exactly as git wrote it, trailing section heading and
    /// all — reused verbatim so the counts can never drift from the body.
    pub header: String,
    /// The body lines, each still carrying its leading ' ', '+', '-' or '\'.
    pub lines: Vec<String>,
}

impl Hunk {
    /// The worktree lines this hunk covers, 1-based and inclusive-exclusive.
    /// A pure deletion has an empty range starting at the line it was cut from.
    pub fn new_range(&self) -> std::ops::Range<u32> {
        self.new_start..self.new_start + self.new_count
    }
}

/// One file's worth of a unified diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileDiff {
    /// The `diff --git`, `index`, `---` and `+++` lines, verbatim.
    pub head: Vec<String>,
    /// Git said "Binary files … differ": there are no hunks to stage.
    pub binary: bool,
    pub hunks: Vec<Hunk>,
}

/// Parse `git diff` output into per-file diffs.
///
/// Deliberately forgiving about what it does not recognise: anything before the
/// first `diff --git` is ignored, and a line that is not part of a hunk body is
/// treated as the end of that hunk.
pub fn parse_unified(text: &str) -> Vec<FileDiff> {
    let mut out: Vec<FileDiff> = Vec::new();
    // Deliberately *not* `str::lines()`: it strips a trailing `\r`, which would
    // quietly turn every CRLF context line into an LF one and make the patch
    // fail to apply — the precise failure this module exists to avoid.
    let mut raw: Vec<&str> = text.split('\n').collect();
    if raw.last().is_some_and(|l| l.is_empty()) {
        raw.pop(); // the empty tail after a final newline
    }
    let mut lines = raw.into_iter().peekable();

    while let Some(line) = lines.next() {
        if !line.starts_with("diff --git ") {
            continue;
        }
        let mut file = FileDiff { head: vec![line.to_string()], ..Default::default() };
        // Everything up to the first `@@` is the file header.
        while let Some(next) = lines.peek() {
            if next.starts_with("@@") || next.starts_with("diff --git ") {
                break;
            }
            let next = lines.next().unwrap();
            if next.starts_with("Binary files ") || next.starts_with("GIT binary patch") {
                file.binary = true;
            }
            file.head.push(next.to_string());
        }
        // Then the hunks.
        while let Some(next) = lines.peek() {
            if !next.starts_with("@@") {
                break;
            }
            let header = lines.next().unwrap().to_string();
            let Some((old, new)) = parse_at(&header) else { continue };
            let mut body = Vec::new();
            while let Some(next) = lines.peek() {
                // A body line starts with one of these; anything else ends the
                // hunk (the next file's `diff --git`, or trailing noise).
                if !matches!(next.as_bytes().first(), Some(b' ' | b'+' | b'-' | b'\\')) {
                    break;
                }
                body.push(lines.next().unwrap().to_string());
            }
            file.hunks.push(Hunk {
                old_start: old.0,
                old_count: old.1,
                new_start: new.0,
                new_count: new.1,
                header,
                lines: body,
            });
        }
        out.push(file);
    }
    out
}

/// `@@ -a,b +c,d @@ …` → `((a, b), (c, d))`. A missing count means 1.
fn parse_at(header: &str) -> Option<((u32, u32), (u32, u32))> {
    let inner = header.strip_prefix("@@ ")?;
    let inner = inner.split(" @@").next()?;
    let mut parts = inner.split_whitespace();
    let old = parse_range(parts.next()?.strip_prefix('-')?)?;
    let new = parse_range(parts.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

fn parse_range(s: &str) -> Option<(u32, u32)> {
    match s.split_once(',') {
        Some((a, b)) => Some((a.parse().ok()?, b.parse().ok()?)),
        None => Some((s.parse().ok()?, 1)),
    }
}

/// A patch holding just this one hunk, ready for `git apply`.
///
/// The file header and the `@@` line are reused verbatim, so the counts come
/// from git itself and `--recount` is never needed.
pub fn single_hunk_patch(file: &FileDiff, hunk: &Hunk) -> String {
    let mut out = String::new();
    for line in &file.head {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str(&hunk.header);
    out.push('\n');
    for line in &hunk.lines {
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// `git diff` for one file, as a patch that can be fed back to `git apply`.
///
/// With `cached`, the diff is index-against-HEAD; without it,
/// worktree-against-index — which is the one whose hunks can be staged.
pub async fn diff_file(root: &Path, rel: &Path, cached: bool) -> Result<FileDiff> {
    let mut args: Vec<String> = vec!["diff".into()];
    if cached {
        args.push("--cached".into());
    }
    // Each flag is load-bearing: a user's `diff.external` or a `.gitattributes`
    // textconv would otherwise produce something that is not an appliable
    // patch, and `color.ui = always` would salt it with escape sequences.
    args.push("--no-color".into());
    args.push("--no-ext-diff".into());
    args.push("--no-textconv".into());
    args.push("-U3".into());
    args.push("--".into());
    args.push(rel.to_string_lossy().into_owned());

    let out = super::ops::run_text(root, &args).await;
    if !out.ok {
        return Err(Error::other(format!("git diff failed: {}", out.text)));
    }
    Ok(parse_unified(&out.text).into_iter().next().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
diff --git a/src/main.rs b/src/main.rs
index 1234567..89abcde 100644
--- a/src/main.rs
+++ b/src/main.rs
@@ -1,4 +1,5 @@
 fn main() {
-    println!(\"old\");
+    println!(\"new\");
+    println!(\"extra\");
 }
 
@@ -20,3 +21,3 @@ fn other() {
     let a = 1;
-    let b = 2;
+    let b = 3;
 }
";

    #[test]
    fn parses_hunks_with_their_counts_and_body() {
        let files = parse_unified(SAMPLE);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert!(!f.binary);
        assert_eq!(f.head.len(), 4, "diff --git, index, ---, +++");
        assert_eq!(f.hunks.len(), 2);

        let h = &f.hunks[0];
        assert_eq!((h.old_start, h.old_count, h.new_start, h.new_count), (1, 4, 1, 5));
        assert_eq!(h.new_range(), 1..6);
        assert_eq!(h.lines.len(), 6, "context, -, +, +, context, blank context");
        assert_eq!(h.lines[1], "-    println!(\"old\");");

        let h = &f.hunks[1];
        assert_eq!((h.old_start, h.old_count, h.new_start, h.new_count), (20, 3, 21, 3));
        assert!(h.header.ends_with("fn other() {"), "the section heading is kept: {}", h.header);
    }

    /// A one-line range is written without a count. Reading it as 0 would make
    /// the hunk cover nothing and the cursor never find it.
    #[test]
    fn a_range_without_a_count_means_one_line() {
        assert_eq!(parse_at("@@ -5 +6 @@"), Some(((5, 1), (6, 1))));
        assert_eq!(parse_at("@@ -5,0 +6,2 @@"), Some(((5, 0), (6, 2))));
        assert_eq!(parse_at("not a hunk header"), None);
    }

    /// The patch is assembled from bytes git produced, so it can be handed
    /// straight back to `git apply` — no re-rendering, no recounting.
    #[test]
    fn a_single_hunk_patch_keeps_the_header_and_only_that_hunk() {
        let f = &parse_unified(SAMPLE)[0];
        let patch = single_hunk_patch(f, &f.hunks[1]);
        assert!(patch.starts_with("diff --git a/src/main.rs b/src/main.rs\n"));
        assert!(patch.contains("--- a/src/main.rs\n+++ b/src/main.rs\n"));
        assert!(patch.contains("@@ -20,3 +21,3 @@"), "its own header");
        assert!(!patch.contains("@@ -1,4 +1,5 @@"), "and not the other hunk's");
        assert!(patch.contains("-    let b = 2;\n+    let b = 3;\n"));
        assert!(patch.ends_with('\n'), "git apply wants a final newline");
    }

    /// The marker has to survive into the patch, or applying it would silently
    /// add a newline the file never had.
    #[test]
    fn the_no_newline_marker_survives_into_the_patch() {
        let text = "\
diff --git a/f b/f
--- a/f
+++ b/f
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let f = &parse_unified(text)[0];
        assert_eq!(f.hunks.len(), 1);
        assert_eq!(f.hunks[0].lines.len(), 4);
        let patch = single_hunk_patch(f, &f.hunks[0]);
        assert_eq!(patch.matches("\\ No newline at end of file").count(), 2);
    }

    /// CRLF is the case this whole module exists for: the bytes have to come
    /// through untouched or `git apply` rejects the context.
    #[test]
    fn crlf_context_lines_are_kept_verbatim() {
        let text =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,2 +1,2 @@\n ctx\r\n-old\r\n+new\r\n";
        let f = &parse_unified(text)[0];
        let h = &f.hunks[0];
        assert_eq!(h.lines[0], " ctx\r", "the CR is still there");
        assert!(single_hunk_patch(f, h).contains(" ctx\r\n"));
    }

    #[test]
    fn a_binary_diff_has_no_hunks_to_stage() {
        let text = "\
diff --git a/img.png b/img.png
index 1234567..89abcde 100644
Binary files a/img.png and b/img.png differ
";
        let f = &parse_unified(text)[0];
        assert!(f.binary);
        assert!(f.hunks.is_empty());
    }

    #[test]
    fn several_files_are_split_apart() {
        let text = format!(
            "{SAMPLE}diff --git a/b.txt b/b.txt\n--- a/b.txt\n+++ b/b.txt\n@@ -1 +1 @@\n-x\n+y\n"
        );
        let files = parse_unified(&text);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].hunks.len(), 2);
        assert_eq!(files[1].hunks.len(), 1);
        assert!(files[1].head[0].contains("b.txt"));
    }

    #[test]
    fn nothing_to_diff_yields_nothing() {
        assert!(parse_unified("").is_empty());
        assert!(parse_unified("some unrelated output\n").is_empty());
    }
}

#[cfg(test)]
mod repo_tests {
    use super::*;
    use std::path::PathBuf;

    fn git_ok(dir: &std::path::Path, args: &[&str]) -> bool {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn git_out(dir: &std::path::Path, args: &[&str]) -> String {
        std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default()
    }

    /// A repository holding a file with two edits far enough apart that `-U3`
    /// reports them as separate hunks — three lines of context on each side of
    /// line 2 and of line 19 leaves a gap, where a closer pair would merge into
    /// one hunk and defeat the point of the test.
    fn make_repo(tag: &str, body: &str, edited: &str) -> Option<PathBuf> {
        let nanos =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_nanos();
        let dir =
            std::env::temp_dir().join(format!("rc_hunks_{tag}_{}_{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir).ok()?;
        if !git_ok(&dir, &["init", "-q"]) {
            let _ = std::fs::remove_dir_all(&dir);
            return None;
        }
        git_ok(&dir, &["config", "user.email", "t@example.com"]);
        git_ok(&dir, &["config", "user.name", "Test"]);
        std::fs::write(dir.join("f.txt"), body).ok()?;
        git_ok(&dir, &["add", "f.txt"]);
        git_ok(&dir, &["commit", "-qm", "init"]);
        std::fs::write(dir.join("f.txt"), edited).ok()?;
        Some(dir)
    }

    fn two_edits() -> (String, String) {
        let body: String = (1..=20).map(|i| format!("line{i}\n")).collect();
        let edited = body.replace("line2\n", "CHANGED2\n").replace("line19\n", "CHANGED19\n");
        (body, edited)
    }

    /// The whole point: staging one hunk leaves the other one unstaged. If the
    /// patch were rebuilt rather than reused, or the counts recomputed, git
    /// would reject it or stage the wrong lines.
    #[tokio::test]
    async fn staging_one_hunk_leaves_the_other_alone() {
        let (body, edited) = two_edits();
        let Some(dir) = make_repo("stage", &body, &edited) else {
            eprintln!("git unavailable; skipping");
            return;
        };
        let rel = std::path::Path::new("f.txt");

        let diff = diff_file(&dir, rel, false).await.expect("a diff");
        assert_eq!(diff.hunks.len(), 2, "the two edits are far enough apart to split");

        let patch = single_hunk_patch(&diff, &diff.hunks[0]);
        let out = crate::git::ops::apply_patch(&dir, &patch, true, false).await;
        assert!(out.ok, "git apply --cached accepted the patch: {}", out.text);

        let staged = git_out(&dir, &["diff", "--cached"]);
        assert!(staged.contains("CHANGED2"), "the first edit is staged:\n{staged}");
        assert!(!staged.contains("CHANGED19"), "the second is not:\n{staged}");

        let unstaged = git_out(&dir, &["diff"]);
        assert!(unstaged.contains("CHANGED19"), "the second is still unstaged:\n{unstaged}");
        assert!(!unstaged.contains("CHANGED2"), "and the first is no longer:\n{unstaged}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Discarding reverses the hunk out of the working file, leaving the other
    /// edit and the rest of the file untouched.
    #[tokio::test]
    async fn discarding_a_hunk_reverses_it_out_of_the_working_file() {
        let (body, edited) = two_edits();
        let Some(dir) = make_repo("discard", &body, &edited) else {
            eprintln!("git unavailable; skipping");
            return;
        };
        let rel = std::path::Path::new("f.txt");

        let diff = diff_file(&dir, rel, false).await.expect("a diff");
        let patch = single_hunk_patch(&diff, &diff.hunks[0]);
        let out = crate::git::ops::apply_patch(&dir, &patch, false, true).await;
        assert!(out.ok, "git apply --reverse accepted the patch: {}", out.text);

        let now = std::fs::read_to_string(dir.join("f.txt")).unwrap();
        assert!(now.contains("line2\n"), "the first edit is gone from the file:\n{now}");
        assert!(now.contains("CHANGED19\n"), "the second edit is still there:\n{now}");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A CRLF file is the case the verbatim handling exists for: `str::lines`
    /// would have eaten the CR and git would reject the context.
    #[tokio::test]
    async fn a_crlf_file_stages_a_hunk_without_rejection() {
        let body: String = (1..=20).map(|i| format!("line{i}\r\n")).collect();
        let edited =
            body.replace("line2\r\n", "CHANGED2\r\n").replace("line19\r\n", "CHANGED19\r\n");
        let Some(dir) = make_repo("crlf", &body, &edited) else {
            eprintln!("git unavailable; skipping");
            return;
        };
        let rel = std::path::Path::new("f.txt");

        let diff = diff_file(&dir, rel, false).await.expect("a diff");
        assert_eq!(diff.hunks.len(), 2);
        assert!(
            diff.hunks[0].lines.iter().any(|l| l.ends_with('\r')),
            "the CR survived parsing: {:?}",
            diff.hunks[0].lines
        );

        let patch = single_hunk_patch(&diff, &diff.hunks[0]);
        let out = crate::git::ops::apply_patch(&dir, &patch, true, false).await;
        assert!(out.ok, "git apply accepted CRLF context: {}", out.text);

        let staged = git_out(&dir, &["diff", "--cached"]);
        assert!(staged.contains("CHANGED2"), "staged:\n{staged}");
        assert!(!staged.contains("CHANGED19"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
