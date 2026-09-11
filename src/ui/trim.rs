//! Erasing — rather than padding — the blank tail of every rendered row, so the
//! terminal's own selection doesn't copy trailing whitespace.
//!
//! Ratatui paints every cell of every row, so a half-empty line reaches the
//! terminal as its text followed by real space characters. Dragging the mouse
//! over the internal editor or viewer therefore copies each line padded out to
//! the window width — spaces the text never had.
//!
//! Midnight Commander doesn't have the problem, and not because of anything its
//! editor does: ncurses ends a partly-written row with `EL` (erase to end of
//! line) instead of blanks. An erased cell is *unset* in the terminal's grid
//! rather than holding a space, and terminals leave unset cells out of a
//! selection — kitty trims them whatever its `strip_trailing_spaces` setting
//! says, and the xterm family behaves the same way.
//!
//! Nothing changes on screen: `EL` paints the erased run in the background
//! colour that is active when it runs (background-colour erase), which is why
//! [`tails`] only claims a run that is a single colour, and why a theme with a
//! *horizontal* gradient behind the text keeps its padding — there is no one
//! colour to erase to. The handful of terminals without background-colour erase
//! would clear such a run to the default background instead, which is what the
//! `strip_trailing_spaces` config option turns this pass off for.
//!
//! [`Trimmer::plan`] picks the runs out of a finished frame and [`erase`]
//! writes the sequences; [`crate::app`] runs the pass after every draw.

use ratatui::backend::IntoCrossterm;
use ratatui::buffer::{Buffer, Cell, CellDiffOption};
use ratatui::crossterm::cursor::{MoveTo, RestorePosition, SavePosition};
use ratatui::crossterm::queue;
use ratatui::crossterm::style::SetBackgroundColor;
use ratatui::crossterm::terminal::{Clear, ClearType};
use ratatui::layout::Rect;
use ratatui::style::Color;
use std::io::{self, Write};

/// One row's erasable tail: from column `x` to the right edge, every cell is a
/// plain space on background `bg`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tail {
    pub x: u16,
    pub y: u16,
    pub bg: Color,
}

/// Whether `cell` is blank in the way an erase would leave it: a bare space, no
/// attributes that would paint something (an underline, or `REVERSED`, which
/// swaps in the foreground colour), and not a cell some escape sequence owns —
/// `Skip` marks the placeholders a terminal-graphics image is drawn over, and
/// erasing those would cut into the picture.
fn erasable(cell: &Cell) -> bool {
    cell.symbol() == " " && cell.modifier.is_empty() && cell.diff_option == CellDiffOption::None
}

/// Every row's erasable tail in `buf`, in row order. Rows that end in text (or
/// in blanks of more than one colour) contribute nothing.
pub fn tails(buf: &Buffer) -> Vec<Tail> {
    let area = buf.area;
    // `EL` erases to the end of the *terminal* line, so it can only stand in for
    // a buffer that reaches the right edge of the screen — which the app's
    // fullscreen viewport does, and an inline or fixed one would not.
    if area.width == 0 || area.x != 0 {
        return Vec::new();
    }
    let last = area.right() - 1;
    let mut out = Vec::with_capacity(area.height as usize);
    for y in area.top()..area.bottom() {
        let Some(cell) = buf.cell((last, y)) else { continue };
        if !erasable(cell) {
            continue;
        }
        let bg = cell.bg;
        // Walk left while the run stays blank *and* stays one colour.
        let mut x = last;
        while x > area.left() {
            match buf.cell((x - 1, y)) {
                Some(c) if erasable(c) && c.bg == bg => x -= 1,
                _ => break,
            }
        }
        out.push(Tail { x, y, bg });
    }
    out
}

/// Runs the erase pass, remembering what it erased so an unchanged row isn't
/// erased again on every frame.
///
/// Skipping is safe because Ratatui only writes the cells that differ from the
/// previous frame: a row whose tail is unchanged was not repainted, so the erase
/// from last time is still standing in the terminal's grid.
#[derive(Debug, Default)]
pub struct Trimmer {
    /// The tails erased for the last frame, in row order.
    last: Vec<Tail>,
    /// The area they were erased in — a resize repaints everything, so what was
    /// erased under the old size says nothing about the new one.
    area: Rect,
}

impl Trimmer {
    /// The tails of `buf` that still need erasing, remembering all of them as
    /// erased. Hand the result to [`erase`].
    pub fn plan(&mut self, buf: &Buffer) -> Vec<Tail> {
        if buf.area != self.area {
            self.area = buf.area;
            self.last.clear();
        }
        let tails = tails(buf);
        let todo = tails.iter().copied().filter(|t| !self.erased(t)).collect();
        self.last = tails;
        todo
    }

    /// Whether `t` is exactly what the previous frame already erased.
    fn erased(&self, t: &Tail) -> bool {
        self.last.binary_search_by_key(&t.y, |p| p.y).is_ok_and(|i| self.last[i] == *t)
    }

    /// Forget the previous frame, because the screen was cleared behind this
    /// pass's back (a forced repaint, or coming back from a subshell).
    pub fn invalidate(&mut self) {
        self.last.clear();
    }
}

/// Write the erase sequences for `tails`, leaving the cursor where the frame put
/// it.
///
/// Colours need no such care: the Crossterm backend resets them after every draw
/// and assumes a reset state at the start of the next one. The cursor does —
/// Ratatui places it as the last step of a draw, and these erases move it.
pub fn erase<W: Write>(out: &mut W, tails: &[Tail]) -> io::Result<()> {
    if tails.is_empty() {
        return Ok(());
    }
    queue!(out, SavePosition)?;
    let mut bg: Option<Color> = None;
    for t in tails {
        if bg != Some(t.bg) {
            queue!(out, SetBackgroundColor(t.bg.into_crossterm()))?;
            bg = Some(t.bg);
        }
        queue!(out, MoveTo(t.x, t.y), Clear(ClearType::UntilNewLine))?;
    }
    queue!(out, SetBackgroundColor(Color::Reset.into_crossterm()), RestorePosition)?;
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Modifier, Style};

    /// A buffer whose rows are `text`, padded to `width` with spaces on `bg`.
    fn buf_of(width: u16, bg: Color, rows: &[&str]) -> Buffer {
        let area = Rect::new(0, 0, width, rows.len() as u16);
        let mut buf = Buffer::empty(area);
        for (y, row) in rows.iter().enumerate() {
            for x in 0..width {
                let ch = row.chars().nth(x as usize).unwrap_or(' ');
                buf[(x, y as u16)].set_char(ch).set_style(Style::default().bg(bg));
            }
        }
        buf
    }

    #[test]
    fn a_rows_blank_tail_starts_where_its_text_ends() {
        let buf = buf_of(10, Color::Blue, &["hello", "hi"]);
        assert_eq!(
            tails(&buf),
            vec![Tail { x: 5, y: 0, bg: Color::Blue }, Tail { x: 2, y: 1, bg: Color::Blue },]
        );
    }

    #[test]
    fn a_row_ending_in_text_has_no_tail() {
        // The panels' right-hand border reaches the edge, so those rows are left
        // exactly as Ratatui painted them.
        let buf = buf_of(5, Color::Reset, &["ab  │", "ab   "]);
        assert_eq!(tails(&buf), vec![Tail { x: 2, y: 1, bg: Color::Reset }]);
    }

    #[test]
    fn an_all_blank_row_is_erased_from_column_zero() {
        let buf = buf_of(6, Color::Blue, &["      "]);
        assert_eq!(tails(&buf), vec![Tail { x: 0, y: 0, bg: Color::Blue }]);
    }

    #[test]
    fn the_tail_stops_where_the_background_colour_changes() {
        // A horizontal gradient behind the text: no single colour to erase to, so
        // only the last cell's own colour is claimed.
        let mut buf = buf_of(6, Color::Blue, &["ab"]);
        buf[(4, 0)].set_style(Style::default().bg(Color::Red));
        assert_eq!(tails(&buf), vec![Tail { x: 5, y: 0, bg: Color::Blue }]);
    }

    #[test]
    fn attributes_and_image_placeholders_are_left_alone() {
        // A reversed space paints the foreground colour; an erase would not.
        let mut buf = buf_of(4, Color::Blue, &["a"]);
        buf[(3, 0)].modifier = Modifier::REVERSED;
        assert!(tails(&buf).is_empty(), "reversed blank is not erasable");

        // Cells a terminal-graphics image is drawn over are marked `Skip`.
        let mut buf = buf_of(4, Color::Blue, &["a"]);
        buf[(2, 0)].set_diff_option(CellDiffOption::Skip);
        assert_eq!(tails(&buf), vec![Tail { x: 3, y: 0, bg: Color::Blue }]);
    }

    #[test]
    fn an_inline_viewport_is_left_alone() {
        // `EL` clears to the edge of the screen, not of the buffer.
        let mut buf = Buffer::empty(Rect::new(4, 0, 6, 1));
        buf[(4, 0)].set_char('a');
        assert!(tails(&buf).is_empty());
    }

    #[test]
    fn the_pass_emits_a_cursor_safe_erase_per_row() {
        let buf = buf_of(10, Color::Blue, &["hello", "hi"]);
        let mut out = Vec::new();
        erase(&mut out, &Trimmer::default().plan(&buf)).unwrap();
        let seq = String::from_utf8(out).unwrap();
        assert!(seq.starts_with("\x1b7"), "saves the frame's cursor first");
        assert!(seq.ends_with("\x1b8"), "and puts it back");
        assert_eq!(seq.matches("\x1b[K").count(), 2, "one erase per row");
        assert!(seq.contains("\x1b[1;6H"), "erases row 1 from column 6 (1-based)");
        assert!(seq.contains("\x1b[2;3H"), "and row 2 from column 3");
    }

    /// The end of the story: a real editor frame, written the way the real
    /// backend writes it, landing in a real terminal grid.
    ///
    /// `vt100` models the distinction this whole module rests on — a cell an
    /// erase left alone is *empty*, a cell holding a space is not — and
    /// `contents_between` extracts a range of the grid the way a terminal builds
    /// the text for a selection, keeping the spaces *inside* a line and dropping
    /// the empty cells after it.
    #[test]
    fn a_drawn_editor_frame_reaches_the_terminal_without_its_padding() {
        use crate::editor::EditorState;
        use crate::editor::render::render as ed_render;
        use crate::vfs::VfsPath;
        use ratatui::Terminal;
        use ratatui::backend::{Backend, CrosstermBackend, TestBackend};

        const W: u16 = 40;
        const H: u16 = 6;

        /// The bytes the Crossterm backend writes for a whole frame, optionally
        /// followed by the erase pass.
        fn painted(buf: &Buffer, trim: bool) -> Vec<u8> {
            let mut bytes: Vec<u8> = Vec::new();
            let mut backend = CrosstermBackend::new(&mut bytes);
            backend.draw(buf.area.positions().map(|p| (p.x, p.y, &buf[p]))).unwrap();
            if trim {
                erase(&mut backend, &Trimmer::default().plan(buf)).unwrap();
            }
            Backend::flush(&mut backend).unwrap();
            bytes
        }

        /// What a selection over the whole screen would copy.
        fn selected(bytes: &[u8]) -> Vec<String> {
            let mut vt = vt100::Parser::new(H, W, 0);
            vt.process(bytes);
            vt.screen().contents_between(0, 0, H - 1, W - 1).lines().map(str::to_string).collect()
        }

        let mut ed = EditorState::new("note.txt".into(), VfsPath::local("/tmp/n"), "hello\nworld");
        let theme = crate::ui::theme::Theme::mc();
        let mut term = Terminal::new(TestBackend::new(W, H)).unwrap();
        term.draw(|f| ed_render(f, f.area(), &mut ed, &theme)).unwrap();
        let buf = term.backend().buffer().clone();

        // The text rows sit between the status line and the shortcut bar.
        let padded = selected(&painted(&buf, false));
        let trimmed = selected(&painted(&buf, true));
        assert_eq!(padded[1], format!("hello{}", " ".repeat((W - 5) as usize)));
        assert_eq!(padded[2], format!("world{}", " ".repeat((W - 5) as usize)));
        assert_eq!(trimmed[1], "hello");
        assert_eq!(trimmed[2], "world");
        // The chrome is untouched: those rows are full-width bars, and the two
        // renderings must otherwise agree character for character.
        for (a, b) in padded.iter().zip(&trimmed) {
            assert_eq!(a.trim_end(), b.trim_end(), "only trailing blanks differ");
        }

        // And nothing changes on screen: an erase paints the run in the active
        // background colour, so every cell of the two grids looks the same.
        let (mut lhs, mut rhs) = (vt100::Parser::new(H, W, 0), vt100::Parser::new(H, W, 0));
        lhs.process(&painted(&buf, false));
        rhs.process(&painted(&buf, true));
        for y in 0..H {
            for x in 0..W {
                let (a, b) = (lhs.screen().cell(y, x).unwrap(), rhs.screen().cell(y, x).unwrap());
                assert_eq!(a.bgcolor(), b.bgcolor(), "background differs at {x},{y}");
                assert_eq!(
                    a.contents().trim_end(),
                    b.contents().trim_end(),
                    "glyph differs at {x},{y}"
                );
            }
        }
    }

    #[test]
    fn an_unchanged_row_is_not_erased_twice() {
        let mut trimmer = Trimmer::default();
        let buf = buf_of(10, Color::Blue, &["hello", "hi"]);
        assert_eq!(trimmer.plan(&buf).len(), 2, "both rows on the first frame");

        // Same frame again: the erases are still standing, so nothing is written.
        assert!(trimmer.plan(&buf).is_empty(), "an unchanged frame costs nothing");

        // One row's text changes; only that row is erased again.
        let grown = buf_of(10, Color::Blue, &["hello", "hi there"]);
        assert_eq!(trimmer.plan(&grown), vec![Tail { x: 8, y: 1, bg: Color::Blue }]);

        // A resize repaints everything, so nothing may be assumed to survive.
        let wider = buf_of(12, Color::Blue, &["hello", "hi there"]);
        assert_eq!(trimmer.plan(&wider).len(), 2);

        // As does a forced repaint, which the caller announces.
        trimmer.invalidate();
        assert_eq!(trimmer.plan(&wider).len(), 2);
    }
}
