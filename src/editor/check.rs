//! The live syntax check of a JSON, TOML, YAML or XML file being edited.
//!
//! The check runs on a thread of its own, against a snapshot of the buffer
//! taken once typing has paused for a moment ([`QUIET`]): a rope clone is
//! cheap, so a large file costs the editor nothing while it is being read, and
//! a result that arrives after the text has changed again is simply not used.
//! Until the next one lands, the errors from the last check stay on screen.

use super::EditorState;
use crate::lint::{self, Lang};
use std::time::{Duration, Instant};
use tokio::sync::oneshot::{self, error::TryRecvError};

/// How long the buffer has to stay unchanged before it is checked again. Short
/// enough to feel live, long enough not to start a check per keystroke.
const QUIET: Duration = Duration::from_millis(250);

/// One error, in the buffer's own terms.
#[derive(Debug, Clone)]
pub(crate) struct CharDiag {
    /// The chars it underlines — never empty, and on one line.
    pub start: usize,
    pub end: usize,
    pub line: usize,
    pub message: String,
}

/// The check's state for one editor.
pub(crate) struct SyntaxCheck {
    lang: Lang,
    /// The errors of revision `diags_rev`, in text order.
    diags: Vec<CharDiag>,
    diags_rev: u64,
    /// The revision last seen, and when: the quiet period is timed from it.
    seen_rev: u64,
    changed_at: Instant,
    /// A check under way: the revision it reads, and where its result arrives.
    worker: Option<(u64, oneshot::Receiver<Vec<CharDiag>>)>,
}

impl SyntaxCheck {
    fn new(lang: Lang, rev: u64) -> Self {
        let now = Instant::now();
        SyntaxCheck {
            lang,
            diags: Vec::new(),
            diags_rev: 0,
            seen_rev: rev,
            // Due at once: a file just opened has not been typed into.
            changed_at: now.checked_sub(QUIET).unwrap_or(now),
            worker: None,
        }
    }
}

/// Check `rope` as `lang`, with the errors in char terms.
fn check(rope: &ropey::Rope, lang: Lang) -> Vec<CharDiag> {
    let text = rope.to_string();
    let mut diags: Vec<CharDiag> = lint::check(lang, &text)
        .into_iter()
        .map(|d| {
            let start = rope.byte_to_char(d.span.start);
            let end = rope.byte_to_char(d.span.end).max(start + 1);
            CharDiag { start, end, line: rope.char_to_line(start), message: d.message }
        })
        .collect();
    diags.sort_by_key(|d| d.start);
    diags
}

impl EditorState {
    /// Give the file a syntax check if its name says what language it is in,
    /// and take it away if not.
    pub(super) fn detect_check(&mut self) {
        let lang = lint::lang_for_name(&self.name);
        match (&self.check, lang) {
            (Some(j), Some(o)) if j.lang == o => {}
            (_, Some(o)) => self.check = Some(SyntaxCheck::new(o, self.buf.revision())),
            (_, None) => self.check = None,
        }
    }

    /// Whether this file is checked.
    pub(crate) fn checked(&self) -> bool {
        self.check.is_some() && self.hex.is_none()
    }

    /// Whether a check is due or running, so the app keeps its tick going.
    pub fn check_pending(&self) -> bool {
        self.check
            .as_ref()
            .is_some_and(|j| j.diags_rev != self.buf.revision() || j.worker.is_some())
    }

    /// The check's heartbeat, on the app's tick: collect a finished check, and
    /// start one once the buffer has been quiet long enough. Returns whether
    /// the errors on screen changed.
    pub fn poll_check(&mut self, now: Instant) -> bool {
        let rev = self.buf.revision();
        let Some(j) = self.check.as_mut() else { return false };
        let mut changed = false;
        if let Some((worker_rev, rx)) = j.worker.as_mut() {
            match rx.try_recv() {
                Ok(diags) => {
                    if *worker_rev == rev {
                        j.diags = diags;
                        j.diags_rev = rev;
                        changed = true;
                    }
                    j.worker = None;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Closed) => j.worker = None,
            }
        }
        if j.seen_rev != rev {
            j.seen_rev = rev;
            j.changed_at = now;
        }
        if j.worker.is_none() && j.diags_rev != rev && now.duration_since(j.changed_at) >= QUIET {
            let rope = self.buf.snapshot();
            let lang = j.lang;
            let (tx, rx) = oneshot::channel();
            let spawned = std::thread::Builder::new()
                .name("syntax-check".into())
                .spawn(move || drop(tx.send(check(&rope, lang))));
            if spawned.is_ok() {
                j.worker = Some((rev, rx));
            }
        }
        changed
    }

    /// Check the buffer now, on this thread — for tests, which have no tick.
    #[cfg(test)]
    pub(crate) fn check_now(&mut self) {
        let rev = self.buf.revision();
        if let Some(j) = self.check.as_mut() {
            j.diags = check(&self.buf.snapshot(), j.lang);
            j.diags_rev = rev;
            j.worker = None;
        }
    }

    /// Every error from the last check, in text order.
    pub(crate) fn check_errors(&self) -> &[CharDiag] {
        match &self.check {
            Some(j) if self.hex.is_none() => &j.diags,
            _ => &[],
        }
    }

    /// The errors starting on `line`.
    pub(crate) fn check_errors_on_line(&self, line: usize) -> &[CharDiag] {
        let all = self.check_errors();
        let from = all.partition_point(|d| d.line < line);
        let to = from + all[from..].partition_point(|d| d.line == line);
        &all[from..to]
    }

    /// The error the status row explains: the one under the cursor, else the
    /// first on the cursor's line.
    pub(crate) fn check_error_at_cursor(&self) -> Option<&CharDiag> {
        let on_line = self.check_errors_on_line(self.cur_line());
        on_line.iter().find(|d| (d.start..d.end).contains(&self.cursor)).or_else(|| on_line.first())
    }

    /// Alt-E / Alt-Shift-E: to the next (or previous) error, round the ends,
    /// with its message on the status line.
    pub(super) fn jump_error(&mut self, forward: bool) {
        let all = self.check_errors();
        if self.check.is_none() {
            self.status = "No syntax check for this file".to_string();
            return;
        }
        if all.is_empty() {
            self.status = "No syntax errors".to_string();
            return;
        }
        let at = self.cursor;
        let i = if forward {
            all.iter().position(|d| d.start > at).unwrap_or(0)
        } else {
            all.iter().rposition(|d| d.start < at).unwrap_or(all.len() - 1)
        };
        let (target, message, n) = (all[i].start, all[i].message.clone(), all.len());
        self.pre_move(false);
        self.cursor = target.min(self.buf.len_chars());
        self.goal_col = None;
        self.pending_center = true;
        self.status = format!("Error {}/{n}: {message}", i + 1);
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::EditorState;
    use crate::vfs::VfsPath;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn ed(name: &str, text: &str) -> EditorState {
        let mut e = EditorState::new(name.into(), VfsPath::local("/tmp/x"), text);
        e.check_now();
        e
    }

    #[test]
    fn files_are_checked_by_the_language_their_name_says() {
        assert!(ed("a.json", "{}").checked());
        assert!(ed("b.geojson", "{}").checked());
        assert!(!ed("c.txt", "{").checked());
        assert!(ed("c.txt", "{").check_errors().is_empty());
        // TOML, YAML and XML, each by its own rules, in char terms too.
        let toml = ed("Cargo.toml", "[package]\nname = \"é\"\nversion = \n");
        assert_eq!(toml.check_errors().len(), 1, "{:?}", toml.check_errors());
        assert_eq!(toml.check_errors()[0].line, 2);
        let yaml = ed("compose.yaml", "services:\n  web: {image: a, image: b}\n");
        assert_eq!(yaml.check_errors()[0].message, "duplicate key 'image'");
        assert_eq!(yaml.check_errors()[0].line, 1);
        let xml = ed("app.csproj", "<Project>\n  <ItemGroup>\n</Project>\n");
        assert_eq!(xml.check_errors()[0].message, "<ItemGroup> isn't closed before </Project>");
    }

    #[test]
    fn errors_are_found_in_char_terms_and_by_line() {
        // A multi-byte character before the error: spans are chars, not bytes.
        let e = ed("a.json", "{\"é\": 1\n \"b\": tru}");
        let errs = e.check_errors();
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert_eq!((errs[0].start, errs[0].end, errs[0].line), (6, 7, 0), "the 1 after é");
        assert_eq!(errs[1].line, 1);
        assert_eq!(e.check_errors_on_line(1).len(), 1);
        assert!(e.check_errors_on_line(2).is_empty());
    }

    #[test]
    fn alt_e_walks_the_errors_and_says_what_each_is() {
        let mut e = ed("a.json", "[1 2,\n 3 4]");
        e.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(e.cursor_line_col(), (0, 1));
        assert!(e.status.starts_with("Error 1/2: Missing ','"), "{}", e.status);
        e.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(e.cursor_line_col(), (1, 1));
        e.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(e.cursor_line_col(), (0, 1), "round from the last to the first");
        e.handle_key(KeyEvent::new(KeyCode::Char('E'), KeyModifiers::ALT | KeyModifiers::SHIFT));
        assert_eq!(e.cursor_line_col(), (1, 1), "Shift goes back");
        let mut clean = ed("a.json", "[]");
        clean.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(clean.status, "No syntax errors");
        let mut plain = ed("notes.txt", "[");
        plain.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::ALT));
        assert_eq!(plain.status, "No syntax check for this file");
    }

    #[test]
    fn a_result_for_an_older_revision_is_not_shown() {
        let mut e = EditorState::new("a.json".into(), VfsPath::local("/tmp/x"), "[1 2]");
        let start = std::time::Instant::now();
        assert!(e.check_pending());
        e.poll_check(start);
        // Edit before the check comes back: its answer is about text that is gone.
        e.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL));
        e.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
        let deadline = start + std::time::Duration::from_secs(5);
        while e.check.as_ref().unwrap().worker.is_some() && std::time::Instant::now() < deadline {
            e.poll_check(start);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(e.check_errors().is_empty(), "the stale result was dropped");
        assert!(e.check_pending(), "and the new text still wants checking");
        // Once quiet, the current text is checked and its error shows.
        let later = start + std::time::Duration::from_secs(1);
        while e.check_pending() && std::time::Instant::now() < deadline {
            e.poll_check(later);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(e.check_errors().len(), 1);
    }

    fn draw(e: &mut EditorState, w: u16, h: u16) -> ratatui::buffer::Buffer {
        use ratatui::{Terminal, backend::TestBackend};
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), e, &theme)).unwrap();
        t.backend().buffer().clone()
    }

    fn row(b: &ratatui::buffer::Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()
    }

    #[test]
    fn errors_are_marked_in_the_gutter_underlined_and_explained_on_the_status_row() {
        use ratatui::style::Modifier;
        let theme = crate::ui::theme::Theme::mc();
        let mut e = ed("a.json", "{\n  \"a\": 1\n  \"b\": 2\n}");
        let b = draw(&mut e, 60, 8);
        // Line 2 (screen row 2) holds the value missing its comma.
        assert!(row(&b, 2).starts_with("✗ "), "{:?}", row(&b, 2));
        assert!(
            row(&b, 1).starts_with("  {"),
            "the text starts after the gutter: {:?}",
            row(&b, 1)
        );
        assert!(!row(&b, 3).starts_with('✗'));
        let x = row(&b, 2).chars().position(|c| c == '1').unwrap() as u16;
        assert_eq!(b[(x, 2)].fg, theme.error_fg);
        assert!(b[(x, 2)].modifier.contains(Modifier::UNDERLINED));
        assert!(row(&b, 0).contains("✗ 1"), "the count: {:?}", row(&b, 0));
        // With the cursor on the error's line, the status row says what it is.
        e.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
        let b = draw(&mut e, 60, 8);
        assert!(row(&b, 0).contains("Missing ',' after this value"), "{:?}", row(&b, 0));
        // A click lands on the character under it, gutter and all.
        e.handle_mouse(ratatui::crossterm::event::MouseEvent {
            kind: ratatui::crossterm::event::MouseEventKind::Down(
                ratatui::crossterm::event::MouseButton::Left,
            ),
            column: x,
            row: 2,
            modifiers: KeyModifiers::NONE,
        });
        assert_eq!(e.cursor_line_col(), (1, 7));
    }

    #[test]
    fn reopening_another_file_leaves_no_errors_of_the_old_one_behind() {
        let mut e = ed("a.json", "[1 2]");
        assert_eq!(e.check_errors().len(), 1);
        e.load_text("b.json".into(), VfsPath::local("/tmp/b"), "[1, 2]");
        assert!(e.check_errors().is_empty() || e.check_pending());
        e.check_now();
        assert!(e.check_errors().is_empty());
    }
}
