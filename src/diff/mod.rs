//! Side-by-side file comparison/merge view.
//!
//! Two files are diffed line-by-line (LCS) and shown side by side with changed
//! blocks highlighted and connected across the gutter. The user can copy a
//! change from one side to the other (Ctrl-←/→) in memory and save with F2.

pub mod hex;
pub mod hexrender;
pub mod render;

use crate::vfs::VfsPath;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the app should do after the diff view handles a key.
pub enum DiffSignal {
    Stay,
    Close,
    /// Write the changed buffers back to their files.
    Save,
    /// Closing with unsaved changes — ask save / discard / cancel.
    ConfirmQuit,
    /// Stage the hunk under the cursor (`git apply --cached`).
    StageHunk,
    /// Throw the hunk under the cursor away (`git apply --reverse`).
    DiscardHunk,
    /// Unstage the whole file (`git restore --staged`).
    UnstageFile,
}

/// What the Alt-D view needs in order to stage out of itself.
///
/// The hunks here come from `git diff` (index → worktree), **not** from
/// `deltas`, which compare HEAD with the worktree. The two coincide only while
/// nothing is staged, so everything hunk-related is keyed on worktree line
/// numbers rather than on rows or deltas.
pub struct GitCtx {
    pub root: std::path::PathBuf,
    /// The file's path relative to the repository root.
    pub rel: std::path::PathBuf,
    /// The unstaged hunks, as `git diff` reports them.
    pub unstaged: crate::git::hunks::FileDiff,
    /// Something is already staged, so the left pane (HEAD) is not the
    /// preimage of these hunks. Worth saying on the status line.
    pub staged_dirty: bool,
}

/// A single aligned display row: a left line, a right line, or one of each.
#[derive(Clone, Copy)]
struct Row {
    left: Option<usize>,
    right: Option<usize>,
    /// Index into `deltas` when this row belongs to a change block.
    delta: Option<usize>,
}

/// A contiguous change block, spanning line ranges on each side.
struct Delta {
    rows: std::ops::Range<usize>,
    left: std::ops::Range<usize>,
    right: std::ops::Range<usize>,
}

pub struct DiffView {
    pub left_name: String,
    pub right_name: String,
    left_path: VfsPath,
    right_path: VfsPath,
    left: Vec<String>,
    right: Vec<String>,
    left_nl: bool,
    right_nl: bool,
    left_dirty: bool,
    right_dirty: bool,
    rows: Vec<Row>,
    deltas: Vec<Delta>,
    cursor: usize,
    top: usize,
    active: Option<usize>,
    view_rows: usize,
    status: String,
    /// Set only for the Alt-D git diff; `None` for a plain Compare-files, which
    /// is why the staging keys are inert there.
    pub git: Option<GitCtx>,
}

impl DiffView {
    pub fn new(
        left_name: String,
        left_path: VfsPath,
        left_data: &[u8],
        right_name: String,
        right_path: VfsPath,
        right_data: &[u8],
    ) -> Self {
        let (left, left_nl) = split_lines(left_data);
        let (right, right_nl) = split_lines(right_data);
        let mut v = DiffView {
            left_name,
            right_name,
            left_path,
            right_path,
            left,
            right,
            left_nl,
            right_nl,
            left_dirty: false,
            right_dirty: false,
            rows: Vec::new(),
            deltas: Vec::new(),
            cursor: 0,
            top: 0,
            active: None,
            view_rows: 1,
            status: String::new(),
            git: None,
        };
        v.recompute();
        // Start on the first difference, if any.
        if !v.deltas.is_empty() {
            v.move_to_delta(0);
        }
        v
    }

    pub fn dirty(&self) -> bool {
        self.left_dirty || self.right_dirty
    }

    /// Files (path, contents) that have unsaved edits.
    pub fn pending_saves(&self) -> Vec<(VfsPath, String)> {
        let mut out = Vec::new();
        if self.left_dirty {
            out.push((self.left_path.clone(), reconstruct(&self.left, self.left_nl)));
        }
        if self.right_dirty {
            out.push((self.right_path.clone(), reconstruct(&self.right, self.right_nl)));
        }
        out
    }

    pub fn mark_saved(&mut self) {
        self.left_dirty = false;
        self.right_dirty = false;
        self.status = crate::l10n::trd("Saved");
    }

    // -- Hunks (Alt-D only) ------------------------------------------------

    /// The index into `git.unstaged.hunks` of the hunk the cursor sits in.
    ///
    /// Keyed on the **worktree** line under the cursor, converted to git's
    /// 1-based numbering. A row that only exists on the left (a pure deletion)
    /// has no worktree line of its own, so it borrows the start of its delta's
    /// right-hand range — which is the line the deletion was cut from, and the
    /// line git's hunk covers.
    pub fn hunk_at_cursor(&self) -> Option<usize> {
        let git = self.git.as_ref()?;
        let row = self.rows.get(self.cursor)?;
        let line = match row.right {
            Some(r) => r,
            None => self.deltas.get(row.delta?)?.right.start,
        } as u32
            + 1;
        git.unstaged.hunks.iter().position(|h| {
            let r = h.new_range();
            // A pure deletion covers no worktree lines at all, so nothing would
            // ever fall inside it; treat the line it was cut from as its own.
            if r.is_empty() { line == r.start || line == r.start + 1 } else { r.contains(&line) }
        })
    }

    /// How many hunks this file has, and which one the cursor is in.
    pub fn hunk_position(&self) -> Option<(usize, usize)> {
        let git = self.git.as_ref()?;
        Some((self.hunk_at_cursor().map(|i| i + 1).unwrap_or(0), git.unstaged.hunks.len()))
    }

    pub fn set_status(&mut self, text: impl Into<String>) {
        self.status = text.into();
    }

    // -- Key handling ------------------------------------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> DiffSignal {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        self.status.clear();

        // Staging keys exist only for the Alt-D git diff. A plain Compare-files
        // has no repository behind it, so there they stay ordinary characters.
        if self.git.is_some() && !ctrl {
            match key.code {
                KeyCode::Char('s') | KeyCode::Char('S') => return DiffSignal::StageHunk,
                KeyCode::Char('x') | KeyCode::Char('X') => return DiffSignal::DiscardHunk,
                KeyCode::Char('u') | KeyCode::Char('U') => return DiffSignal::UnstageFile,
                _ => {}
            }
        }

        match key.code {
            KeyCode::Esc | KeyCode::F(10) | KeyCode::Char('q') | KeyCode::Char('Q') => {
                if self.dirty() { DiffSignal::ConfirmQuit } else { DiffSignal::Close }
            }
            KeyCode::F(2) => DiffSignal::Save,
            KeyCode::Left if ctrl => {
                self.apply(Side::Left);
                DiffSignal::Stay
            }
            KeyCode::Right if ctrl => {
                self.apply(Side::Right);
                DiffSignal::Stay
            }
            KeyCode::Up if ctrl => {
                self.step_delta(-1);
                DiffSignal::Stay
            }
            KeyCode::Down if ctrl => {
                self.step_delta(1);
                DiffSignal::Stay
            }
            KeyCode::Up => {
                self.move_cursor(-1);
                DiffSignal::Stay
            }
            KeyCode::Down => {
                self.move_cursor(1);
                DiffSignal::Stay
            }
            KeyCode::PageUp => {
                self.move_cursor(-(self.view_rows as isize - 1).max(1));
                DiffSignal::Stay
            }
            KeyCode::PageDown => {
                self.move_cursor((self.view_rows as isize - 1).max(1));
                DiffSignal::Stay
            }
            KeyCode::Home => {
                self.cursor = 0;
                self.active = self.delta_at(self.cursor);
                DiffSignal::Stay
            }
            KeyCode::End => {
                self.cursor = self.rows.len().saturating_sub(1);
                self.active = self.delta_at(self.cursor);
                DiffSignal::Stay
            }
            _ => DiffSignal::Stay,
        }
    }

    fn move_cursor(&mut self, delta: isize) {
        if self.rows.is_empty() {
            return;
        }
        let max = self.rows.len() as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, max) as usize;
        // Up/down also switches the active delta to whichever one we're on.
        self.active = self.delta_at(self.cursor);
    }

    fn delta_at(&self, row: usize) -> Option<usize> {
        self.rows.get(row).and_then(|r| r.delta)
    }

    fn move_to_delta(&mut self, i: usize) {
        if self.deltas.is_empty() {
            self.active = None;
            return;
        }
        let i = i.min(self.deltas.len() - 1);
        self.active = Some(i);
        self.cursor = self.deltas[i].rows.start;
    }

    /// Jump to the next (`+1`) or previous (`-1`) change block. If the cursor is
    /// between deltas, snaps to the nearest one in the requested direction.
    fn step_delta(&mut self, dir: isize) {
        if self.deltas.is_empty() {
            return;
        }
        let target = match self.active {
            Some(i) => (i as isize + dir).clamp(0, self.deltas.len() as isize - 1) as usize,
            // Not on a delta: find the first one on the requested side of cursor.
            None => {
                if dir > 0 {
                    self.deltas
                        .iter()
                        .position(|d| d.rows.start > self.cursor)
                        .unwrap_or(self.deltas.len() - 1)
                } else {
                    self.deltas.iter().rposition(|d| d.rows.start < self.cursor).unwrap_or(0)
                }
            }
        };
        self.move_to_delta(target);
    }

    // -- Applying changes --------------------------------------------------

    fn apply(&mut self, to: Side) {
        let Some(i) = self.active else {
            return;
        };
        let Some(d) = self.deltas.get(i) else {
            return;
        };
        let l = d.left.clone();
        let r = d.right.clone();
        match to {
            // Copy the right block over the left (empty right ⇒ delete left).
            Side::Left => {
                let repl: Vec<String> = self.right[r].to_vec();
                self.left.splice(l, repl);
                self.left_dirty = true;
            }
            // Copy the left block over the right (empty left ⇒ delete right).
            Side::Right => {
                let repl: Vec<String> = self.left[l].to_vec();
                self.right.splice(r, repl);
                self.right_dirty = true;
            }
        }
        self.recompute();
        // Land on the next remaining difference (the applied one is gone).
        self.move_to_delta(i);
    }

    // -- Diff computation --------------------------------------------------

    fn recompute(&mut self) {
        let ops = diff_lines(&self.left, &self.right);
        let mut rows: Vec<Row> = Vec::new();
        let mut deltas: Vec<Delta> = Vec::new();
        let (mut li, mut ri, mut i) = (0usize, 0usize, 0usize);
        while i < ops.len() {
            if ops[i] == Op::Eq {
                rows.push(Row { left: Some(li), right: Some(ri), delta: None });
                li += 1;
                ri += 1;
                i += 1;
                continue;
            }
            let (la, ra) = (li, ri);
            while i < ops.len() && ops[i] != Op::Eq {
                match ops[i] {
                    Op::Del => li += 1,
                    Op::Ins => ri += 1,
                    Op::Eq => unreachable!(),
                }
                i += 1;
            }
            let (lb, rb) = (li, ri);
            let didx = deltas.len();
            let row_start = rows.len();
            let n = (lb - la).max(rb - ra);
            for k in 0..n {
                rows.push(Row {
                    left: (la + k < lb).then_some(la + k),
                    right: (ra + k < rb).then_some(ra + k),
                    delta: Some(didx),
                });
            }
            deltas.push(Delta { rows: row_start..rows.len(), left: la..lb, right: ra..rb });
        }
        self.rows = rows;
        self.deltas = deltas;
        if self.cursor >= self.rows.len() {
            self.cursor = self.rows.len().saturating_sub(1);
        }
        self.active = self.delta_at(self.cursor);
    }
}

#[derive(Clone, Copy)]
enum Side {
    Left,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Del,
    Ins,
}

/// Split raw bytes into lines (stripping CR), remembering a trailing newline.
fn split_lines(data: &[u8]) -> (Vec<String>, bool) {
    if data.is_empty() {
        return (Vec::new(), false);
    }
    let s = String::from_utf8_lossy(data);
    let trailing = s.ends_with('\n');
    let mut lines: Vec<String> =
        s.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect();
    if trailing {
        lines.pop(); // drop the empty element produced by the final '\n'
    }
    (lines, trailing)
}

/// Rejoin lines into file content, restoring the trailing newline.
fn reconstruct(lines: &[String], trailing_nl: bool) -> String {
    let mut s = lines.join("\n");
    if trailing_nl && !lines.is_empty() {
        s.push('\n');
    }
    s
}

/// Line-level LCS diff. Falls back to a prefix/suffix split for very large
/// inputs to bound the O(n·m) DP memory.
fn diff_lines(a: &[String], b: &[String]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    if (n + 1).saturating_mul(m + 1) > 4_000_000 {
        return fallback_diff(a, b);
    }
    let w = m + 1;
    let mut dp = vec![0u32; (n + 1) * w];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * w + j] = if a[i] == b[j] {
                dp[(i + 1) * w + (j + 1)] + 1
            } else {
                dp[(i + 1) * w + j].max(dp[i * w + (j + 1)])
            };
        }
    }
    let mut ops = Vec::with_capacity(n + m);
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            ops.push(Op::Eq);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * w + j] >= dp[i * w + (j + 1)] {
            ops.push(Op::Del);
            i += 1;
        } else {
            ops.push(Op::Ins);
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Del);
        i += 1;
    }
    while j < m {
        ops.push(Op::Ins);
        j += 1;
    }
    ops
}

fn fallback_diff(a: &[String], b: &[String]) -> Vec<Op> {
    let (n, m) = (a.len(), b.len());
    let mut p = 0;
    while p < n && p < m && a[p] == b[p] {
        p += 1;
    }
    let mut s = 0;
    while s < n - p && s < m - p && a[n - 1 - s] == b[m - 1 - s] {
        s += 1;
    }
    let mut ops = Vec::with_capacity(n + m);
    ops.extend(std::iter::repeat_n(Op::Eq, p));
    ops.extend(std::iter::repeat_n(Op::Del, n - p - s));
    ops.extend(std::iter::repeat_n(Op::Ins, m - p - s));
    ops.extend(std::iter::repeat_n(Op::Eq, s));
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp() -> VfsPath {
        VfsPath::local("/tmp/x")
    }

    fn view(a: &str, b: &str) -> DiffView {
        DiffView::new("a".into(), vp(), a.as_bytes(), "b".into(), vp(), b.as_bytes())
    }

    /// Give a view the hunks `git diff` would have reported for it.
    fn with_hunks(mut v: DiffView, hunks: Vec<crate::git::hunks::Hunk>) -> DiffView {
        v.git = Some(GitCtx {
            root: "/tmp/repo".into(),
            rel: "f.txt".into(),
            unstaged: crate::git::hunks::FileDiff { head: Vec::new(), binary: false, hunks },
            staged_dirty: false,
        });
        v
    }

    fn hunk(new_start: u32, new_count: u32) -> crate::git::hunks::Hunk {
        crate::git::hunks::Hunk {
            old_start: new_start,
            old_count: new_count,
            new_start,
            new_count,
            header: format!("@@ -{new_start},{new_count} +{new_start},{new_count} @@"),
            lines: Vec::new(),
        }
    }

    /// The cursor is on a display row; a hunk covers worktree lines. Mapping
    /// between the two is what decides which hunk `s` stages, so it has to be
    /// right for an added line, a changed one, and a row far from any hunk.
    #[test]
    fn the_cursor_maps_to_the_hunk_covering_its_worktree_line() {
        // Ten identical lines with line 2 and line 9 changed.
        let a: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        let b = a.replace("line2\n", "CHANGED2\n").replace("line9\n", "CHANGED9\n");
        // git would report two hunks around worktree lines 2 and 9.
        let v = with_hunks(view(&a, &b), vec![hunk(1, 4), hunk(6, 5)]);

        // Walk every row and check it lands in the hunk covering its right line.
        for (i, row) in v.rows.iter().enumerate() {
            let Some(r) = row.right else { continue };
            let line = r as u32 + 1;
            let expect = if (1..5).contains(&line) {
                Some(0)
            } else if (6..11).contains(&line) {
                Some(1)
            } else {
                None
            };
            let mut probe = with_hunks(view(&a, &b), vec![hunk(1, 4), hunk(6, 5)]);
            probe.cursor = i;
            assert_eq!(probe.hunk_at_cursor(), expect, "row {i} covers worktree line {line}");
        }
    }

    /// A pure deletion has no worktree line of its own, so it borrows the line
    /// it was cut from — otherwise the cursor could sit on a deleted line and
    /// find no hunk to stage.
    #[test]
    fn a_deleted_row_still_finds_its_hunk() {
        let v = with_hunks(
            view("one\ntwo\nthree\n", "one\nthree\n"),
            // git writes a deletion as `@@ -2 +1,0 @@`: an empty new range.
            vec![crate::git::hunks::Hunk {
                old_start: 2,
                old_count: 1,
                new_start: 1,
                new_count: 0,
                header: "@@ -2 +1,0 @@".into(),
                lines: vec!["-two".into()],
            }],
        );
        let deleted = v
            .rows
            .iter()
            .position(|r| r.left == Some(1) && r.right.is_none())
            .expect("the deleted line has a row of its own");
        let mut probe = v;
        probe.cursor = deleted;
        assert_eq!(probe.hunk_at_cursor(), Some(0), "the deletion belongs to the hunk");
    }

    /// Compare-files has no repository behind it, so `s`, `x` and `u` must stay
    /// ordinary keys there rather than trying to stage anything.
    #[test]
    fn the_staging_keys_are_inert_without_a_repository() {
        let mut v = view("one\ntwo\n", "one\nTWO\n");
        assert!(v.git.is_none());
        for c in ['s', 'x', 'u'] {
            let sig = v.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
            assert!(matches!(sig, DiffSignal::Stay), "'{c}' does nothing in a plain compare");
        }
        assert!(v.hunk_at_cursor().is_none());
        assert!(v.hunk_position().is_none());
    }

    #[test]
    fn the_staging_keys_fire_in_the_git_diff() {
        let mut v = with_hunks(view("one\ntwo\n", "one\nTWO\n"), vec![hunk(1, 2)]);
        assert!(matches!(
            v.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE)),
            DiffSignal::StageHunk
        ));
        assert!(matches!(
            v.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
            DiffSignal::DiscardHunk
        ));
        assert!(matches!(
            v.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE)),
            DiffSignal::UnstageFile
        ));
        // Ctrl-keys keep their meaning: Ctrl-S is not "stage".
        assert!(matches!(
            v.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            DiffSignal::Stay
        ));
    }

    #[test]
    fn diff_finds_changes() {
        let v = view("one\ntwo\nthree\n", "one\nTWO\nthree\n");
        assert_eq!(v.deltas.len(), 1, "one changed block");
        // The change covers left line 1 and right line 1.
        assert_eq!(v.deltas[0].left, 1..2);
        assert_eq!(v.deltas[0].right, 1..2);
    }

    #[test]
    fn apply_right_to_left_replaces_block() {
        let mut v = view("one\ntwo\nthree\n", "one\nTWO\nthree\n");
        v.apply(Side::Left);
        assert_eq!(v.left, vec!["one", "TWO", "three"]);
        assert!(v.left_dirty);
        assert_eq!(v.deltas.len(), 0, "no differences remain");
        assert_eq!(reconstruct(&v.left, v.left_nl), "one\nTWO\nthree\n");
    }

    #[test]
    fn apply_left_to_right_replaces_block() {
        let mut v = view("one\ntwo\nthree\n", "one\nTWO\nthree\n");
        v.apply(Side::Right);
        assert_eq!(v.right, vec!["one", "two", "three"]);
        assert!(v.right_dirty);
    }

    #[test]
    fn left_only_block_is_deleted_when_applied_left() {
        // "two" exists only on the left; applying right→left deletes it.
        let mut v = view("one\ntwo\nthree\n", "one\nthree\n");
        assert_eq!(v.deltas.len(), 1);
        v.apply(Side::Left);
        assert_eq!(v.left, vec!["one", "three"]);
        assert_eq!(v.deltas.len(), 0);
    }

    #[test]
    fn right_only_block_added_to_left() {
        // "extra" exists only on the right; applying right→left inserts it.
        let mut v = view("one\ntwo\n", "one\nextra\ntwo\n");
        v.apply(Side::Left);
        assert_eq!(v.left, vec!["one", "extra", "two"]);
    }

    #[test]
    fn identical_files_have_no_deltas() {
        let v = view("a\nb\n", "a\nb\n");
        assert!(v.deltas.is_empty());
    }

    #[test]
    fn up_down_moves_cursor_and_switches_active_delta() {
        let mut v = view("a\nX\nc\nY\ne\n", "a\nXX\nc\nYY\ne\n");
        assert_eq!(v.deltas.len(), 2);
        assert_eq!(v.active, Some(0), "starts on the first difference");
        let target = v.deltas[1].rows.start;
        while v.cursor < target {
            v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        }
        assert_eq!(v.active, Some(1), "moving down switches the active delta");
        v.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
        // Moving back up onto the equal row between deltas → no active delta.
        if v.rows[v.cursor].delta.is_none() {
            assert_eq!(v.active, None);
        }
    }

    #[test]
    fn ctrl_up_down_jumps_between_deltas() {
        let mut v = view("a\nX\nc\nY\ne\n", "a\nXX\nc\nYY\ne\n");
        assert_eq!(v.deltas.len(), 2);
        assert_eq!(v.active, Some(0));
        v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(v.active, Some(1), "Ctrl-Down jumps to the next delta");
        assert_eq!(v.cursor, v.deltas[1].rows.start);
        v.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::CONTROL));
        assert_eq!(v.active, Some(1), "stays on the last delta past the end");
        v.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::CONTROL));
        assert_eq!(v.active, Some(0), "Ctrl-Up jumps to the previous delta");
        assert_eq!(v.cursor, v.deltas[0].rows.start);
    }

    #[test]
    fn close_with_unsaved_changes_asks_to_confirm() {
        let mut v = view("one\ntwo\n", "one\nTWO\n");
        // No edits yet → Esc closes immediately.
        assert!(matches!(
            v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            DiffSignal::Close
        ));
        // After an edit, Esc asks for confirmation instead of closing.
        v.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert!(v.dirty());
        assert!(matches!(
            v.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            DiffSignal::ConfirmQuit
        ));
    }

    #[test]
    fn ctrl_left_and_right_apply_via_keys() {
        let mut v = view("one\ntwo\n", "one\nTWO\n");
        v.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(v.left, vec!["one", "TWO"]);
        assert!(v.left_dirty);

        let mut v = view("one\ntwo\n", "one\nTWO\n");
        v.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        assert_eq!(v.right, vec!["one", "two"]);
        assert!(v.right_dirty);
    }

    #[test]
    fn renders_both_sides_with_chrome() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;
        let mut v = view("one\ntwo\n", "one\nTWO\n");
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(80, 10)).unwrap();
        t.draw(|f| super::render::render(f, f.area(), &mut v, &theme)).unwrap();
        let b = t.backend().buffer();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
        }
        assert!(s.contains("two") && s.contains("TWO"), "both sides shown");
        assert!(s.contains("diff(s)"), "status bar");
        assert!(s.contains("F2 save"), "footer hints");
    }
}
