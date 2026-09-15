//! The backend the app draws through: Crossterm's, with each frame reaching the
//! terminal in one piece and the cursor kept out of it.
//!
//! Ratatui writes a frame as a run of cell updates and only then places the
//! cursor. Through plain stdout that run leaves in 1 KiB pieces, and while it
//! streams, the terminal's cursor is wherever the last piece stopped. When a
//! frame changes a few cells nobody notices. When an animated gradient repaints
//! most of the screen ten times a second, a terminal that paints between pieces
//! shows the cursor — visible for the command line's caret — racing across the
//! window.
//!
//! [`FrameBackend`] closes that from both ends. Between
//! [`begin_frame`](FrameBackend::begin_frame) and
//! [`end_frame`](FrameBackend::end_frame) nothing reaches the terminal: the frame
//! is collected and sent in one write, as a synchronized update (DEC mode 2026,
//! which has the terminal hold its display until the frame is complete, and
//! which terminals that don't know it ignore) with the cursor hidden throughout.
//! The cursor Ratatui places is held back to the very end, after the
//! trailing-space erase ([`crate::ui::trim`]) has done its own moving about.

use ratatui::backend::{Backend, ClearType, CrosstermBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::crossterm::cursor::{Hide, MoveTo, Show};
use ratatui::crossterm::queue;
use ratatui::crossterm::terminal::{self, BeginSynchronizedUpdate, EndSynchronizedUpdate};
use ratatui::layout::{Position, Size};
use std::io::{self, Write};

/// Output that collects everything written and hands it on at a flush — unless
/// it is held, in which case flushes are ignored until the hold is released.
struct Held<W: Write> {
    out: W,
    buf: Vec<u8>,
    held: bool,
}

impl<W: Write> Write for Held<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.held {
            return Ok(());
        }
        if !self.buf.is_empty() {
            let sent = self.out.write_all(&self.buf);
            self.buf.clear();
            sent?;
        }
        self.out.flush()
    }
}

/// Crossterm's backend, drawing frame by frame (see the module docs).
///
/// Outside a frame it behaves exactly like the backend it wraps, so the
/// terminal setup, suspend and restore paths are unaffected.
pub struct FrameBackend<W: Write> {
    out: Held<W>,
    /// Whether frames can be held back at all. A Windows console without
    /// virtual-terminal sequences can't: Crossterm drives it through the console
    /// API as commands are queued, and relies on a flush to get everything
    /// written before them out first.
    can_hold: bool,
    /// Whether a frame is being collected.
    framing: bool,
    /// Whether the terminal's cursor is showing — or, inside a frame, whether
    /// it will be once the frame is sent.
    shown: bool,
    /// Where the open frame put the cursor.
    at: Option<Position>,
}

impl<W: Write> FrameBackend<W> {
    pub fn new(out: W) -> Self {
        #[cfg(windows)]
        let can_hold = ratatui::crossterm::Command::is_ansi_code_supported(&MoveTo(0, 0));
        #[cfg(not(windows))]
        let can_hold = true;
        FrameBackend {
            out: Held { out, buf: Vec::new(), held: false },
            can_hold,
            framing: false,
            shown: false,
            at: None,
        }
    }

    /// Crossterm's own backend over this one's output, for the work handed on
    /// to it. It keeps no state of its own between calls, so a fresh one each
    /// time is the same as one kept.
    fn crossterm(&mut self) -> CrosstermBackend<&mut Held<W>> {
        CrosstermBackend::new(&mut self.out)
    }

    /// Start collecting a frame: nothing drawn from here reaches the terminal
    /// until [`end_frame`](Self::end_frame).
    pub fn begin_frame(&mut self) -> io::Result<()> {
        if !self.can_hold || self.framing {
            return Ok(());
        }
        self.framing = true;
        self.at = None;
        self.out.held = true;
        queue!(self.out, BeginSynchronizedUpdate, Hide)
    }

    /// Send the frame collected since [`begin_frame`](Self::begin_frame), with
    /// the cursor put where the frame left it as the last step.
    pub fn end_frame(&mut self) -> io::Result<()> {
        if !self.framing {
            return Ok(());
        }
        self.framing = false;
        let out = &mut self.out;
        if let Some(Position { x, y }) = self.at {
            queue!(out, MoveTo(x, y))?;
        }
        // The frame opened with the cursor hidden.
        if self.shown {
            queue!(out, Show)?;
        }
        queue!(out, EndSynchronizedUpdate)?;
        out.held = false;
        out.flush()
    }
}

impl<W: Write> Write for FrameBackend<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.out.write(data)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

impl<W: Write> Backend for FrameBackend<W> {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        self.crossterm().draw(content)
    }

    fn hide_cursor(&mut self) -> io::Result<()> {
        self.shown = false;
        if self.framing { Ok(()) } else { self.crossterm().hide_cursor() }
    }

    fn show_cursor(&mut self) -> io::Result<()> {
        self.shown = true;
        if self.framing { Ok(()) } else { self.crossterm().show_cursor() }
    }

    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.crossterm().get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        if self.framing {
            self.at = Some(position.into());
            Ok(())
        } else {
            self.crossterm().set_cursor_position(position)
        }
    }

    fn clear(&mut self) -> io::Result<()> {
        self.crossterm().clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.crossterm().clear_region(clear_type)
    }

    fn size(&self) -> io::Result<Size> {
        // What Crossterm's backend does, which needs no output to do it.
        let (width, height) = terminal::size()?;
        Ok(Size { width, height })
    }

    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.crossterm().window_size()
    }

    fn flush(&mut self) -> io::Result<()> {
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::{Terminal, TerminalOptions, Viewport};

    const OPEN: &[u8] = b"\x1b[?2026h\x1b[?25l";
    const CLOSE: &[u8] = b"\x1b[?2026l";

    fn term() -> Terminal<FrameBackend<Vec<u8>>> {
        let viewport = Viewport::Fixed(Rect::new(0, 0, 10, 2));
        Terminal::with_options(FrameBackend::new(Vec::new()), TerminalOptions { viewport }).unwrap()
    }

    /// What has reached the terminal so far, taken so the next check starts afresh.
    fn sent(term: &mut Terminal<FrameBackend<Vec<u8>>>) -> Vec<u8> {
        std::mem::take(&mut term.backend_mut().out.out)
    }

    fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// Draw `text` with the cursor at `caret`, then write `after` (standing in
    /// for the trailing-space erase) before the frame is sent.
    fn frame(
        term: &mut Terminal<FrameBackend<Vec<u8>>>,
        text: &'static str,
        caret: Option<(u16, u16)>,
        after: &[u8],
    ) -> Vec<u8> {
        term.backend_mut().begin_frame().unwrap();
        term.draw(|f| {
            f.render_widget(text, f.area());
            if let Some(c) = caret {
                f.set_cursor_position(c);
            }
        })
        .unwrap();
        term.backend_mut().write_all(after).unwrap();
        assert!(sent(term).is_empty(), "nothing leaves before the frame ends");
        term.backend_mut().end_frame().unwrap();
        sent(term)
    }

    #[test]
    fn a_frame_is_sent_whole_with_the_cursor_placed_last() {
        let mut term = term();
        let out = frame(&mut term, "hello", Some((3, 1)), b"ERASE");
        assert!(out.starts_with(OPEN), "opens synchronized, cursor hidden: {out:?}");
        let text = find(&out, b"hello").expect("the cells are in the frame");
        let erase = find(&out, b"ERASE").expect("so is what was written after the draw");
        let caret = find(&out, b"\x1b[2;4H\x1b[?25h").expect("the cursor is placed and shown");
        assert!(text < erase && erase < caret, "the cursor comes after everything else");
        assert_eq!(find(&out, b"\x1b[?25h"), Some(caret + 6), "and is shown only there");
        assert!(out.ends_with(CLOSE));
    }

    #[test]
    fn a_frame_without_a_cursor_leaves_it_hidden() {
        let mut term = term();
        let out = frame(&mut term, "hello", None, b"");
        assert!(out.starts_with(OPEN) && out.ends_with(CLOSE));
        assert_eq!(find(&out, b"\x1b[?25h"), None);
    }

    #[test]
    fn the_cursor_is_put_back_after_every_frame() {
        // Nothing changed the second time, so the cells aren't sent again — but
        // the frame still hid the cursor as it opened.
        let mut term = term();
        frame(&mut term, "hello", Some((3, 1)), b"");
        let out = frame(&mut term, "hello", Some((3, 1)), b"");
        assert_eq!(find(&out, b"hello"), None);
        assert!(out.starts_with(OPEN) && out.ends_with(b"\x1b[2;4H\x1b[?25h\x1b[?2026l"));
    }

    #[test]
    fn outside_a_frame_cursor_changes_go_straight_out() {
        let mut term = term();
        term.show_cursor().unwrap();
        assert_eq!(sent(&mut term), b"\x1b[?25h");
        term.hide_cursor().unwrap();
        assert_eq!(sent(&mut term), b"\x1b[?25l");
    }
}
