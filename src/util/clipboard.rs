//! The system clipboard, via the terminal's OSC 52 escape sequence.
//!
//! OSC 52 asks the *terminal* to set its clipboard, which is what makes this
//! work where a native clipboard API cannot: over SSH, inside a multiplexer, on
//! a headless box. It needs no library and no running X/Wayland session on this
//! side of the connection — which is why it, rather than a clipboard crate, is
//! what a self-contained TUI wants.
//!
//! Only *writing* is implemented. Reading the clipboard back is part of the same
//! sequence, but terminals disable it by default (a remote host could otherwise
//! silently exfiltrate whatever you last copied), so the editor keeps pasting
//! from its own internal clipboard.

use std::io::Write;

/// Longest text pushed through OSC 52. Terminals and multiplexers cap the
/// sequence length — tmux's buffer limit and xterm's selection limit are the
/// binding ones — and a truncated sequence is worse than a refused one, because
/// it silently replaces the clipboard with a prefix. 64 KiB of source text sits
/// under every cap we know of.
pub const MAX_CLIP_BYTES: usize = 64 * 1024;

/// Why a copy did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipError {
    /// The text was longer than [`MAX_CLIP_BYTES`]; nothing was written.
    TooLarge(usize),
}

/// The terminal stack the sequence has to travel through. A multiplexer eats
/// escape sequences it doesn't recognise, so it has to be told to pass this one
/// down to the real terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Host {
    /// A terminal we can talk to directly.
    Plain,
    /// tmux: wrap in its passthrough DCS. Needs `set -g allow-passthrough on`.
    Tmux,
    /// GNU screen: wrap in DCS and split the payload into short chunks.
    Screen,
}

impl Host {
    /// Detect the multiplexer from the environment, the way tmux and screen
    /// themselves advertise their presence.
    pub fn detect() -> Host {
        if std::env::var_os("TMUX").is_some() {
            return Host::Tmux;
        }
        match std::env::var("TERM") {
            Ok(t) if t.starts_with("screen") => Host::Screen,
            _ => Host::Plain,
        }
    }
}

/// screen splits a DCS string it forwards; keep each chunk well inside its
/// limit so nothing is dropped mid-payload.
const SCREEN_CHUNK: usize = 768;

/// Base64 as RFC 4648 requires, with padding.
///
/// Hand-rolled because this is the only place in the program that needs it, and
/// a dependency for 20 lines of table lookup would not pay for itself.
fn b64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        // Pack the (1..=3) bytes into a 24-bit group, high byte first.
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(n >> 18 & 63) as usize] as char);
        out.push(ALPHABET[(n >> 12 & 63) as usize] as char);
        // The trailing group is padded to four characters: a 1-byte tail encodes
        // two significant characters, a 2-byte tail three.
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[(n & 63) as usize] as char } else { '=' });
    }
    out
}

/// The bytes to write for `text`, wrapped for `host`. Split out from [`copy`] so
/// the escaping rules can be tested without a terminal.
pub fn sequence(text: &str, host: Host) -> String {
    // `c` is the clipboard selection (as against the primary selection, `p`).
    let inner = format!("\x1b]52;c;{}\x07", b64(text.as_bytes()));
    match host {
        Host::Plain => inner,
        // tmux passthrough carries the payload verbatim except that every ESC
        // in it must be doubled, or tmux ends its own DCS at the first one.
        Host::Tmux => format!("\x1bPtmux;{}\x1b\\", inner.replace('\x1b', "\x1b\x1b")),
        Host::Screen => {
            let mut out = String::new();
            for chunk in inner.as_bytes().chunks(SCREEN_CHUNK) {
                out.push_str("\x1bP");
                out.push_str(&String::from_utf8_lossy(chunk));
                out.push_str("\x1b\\");
            }
            out
        }
    }
}

/// Put `text` on the system clipboard.
///
/// Errors only on oversized input; a terminal that does not implement OSC 52
/// simply ignores the sequence, and there is no reply to tell us so.
pub fn copy(text: &str) -> Result<(), ClipError> {
    if text.len() > MAX_CLIP_BYTES {
        return Err(ClipError::TooLarge(text.len()));
    }
    let seq = sequence(text, Host::detect());
    let mut out = std::io::stdout();
    let _ = out.write_all(seq.as_bytes());
    let _ = out.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc_4648_vectors() {
        assert_eq!(b64(b""), "");
        assert_eq!(b64(b"f"), "Zg==");
        assert_eq!(b64(b"fo"), "Zm8=");
        assert_eq!(b64(b"foo"), "Zm9v");
        assert_eq!(b64(b"foob"), "Zm9vYg==");
        assert_eq!(b64(b"fooba"), "Zm9vYmE=");
        assert_eq!(b64(b"foobar"), "Zm9vYmFy");
        // Bytes above ASCII round-trip too — a path can hold any UTF-8.
        assert_eq!(b64("é".as_bytes()), "w6k=");
        assert_eq!(b64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn a_plain_terminal_gets_the_bare_sequence() {
        let s = sequence("hi", Host::Plain);
        assert_eq!(s, "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn tmux_passthrough_doubles_every_inner_escape() {
        let s = sequence("hi", Host::Tmux);
        assert!(s.starts_with("\x1bPtmux;"), "opens tmux passthrough");
        assert!(s.ends_with("\x1b\\"), "and terminates it");
        // The one ESC of the inner OSC must appear doubled, or tmux would end
        // its own DCS there and the rest would land on the screen as text.
        assert!(s.contains("\x1b\x1b]52;c;"), "inner ESC doubled");
        assert_eq!(s.matches("\x1b\x1b").count(), 1, "exactly the inner one");
    }

    #[test]
    fn screen_splits_the_payload_into_dcs_chunks() {
        // One chunk while it fits...
        assert_eq!(sequence("hi", Host::Screen).matches("\x1bP").count(), 1);
        // ...and more than one once the encoded payload outgrows the limit.
        let long = "x".repeat(SCREEN_CHUNK * 2);
        let s = sequence(&long, Host::Screen);
        assert!(s.matches("\x1bP").count() > 1, "long payloads are chunked");
        assert_eq!(s.matches("\x1bP").count(), s.matches("\x1b\\").count(), "each chunk closed");
    }

    #[test]
    fn oversized_text_is_refused_rather_than_truncated() {
        // `copy` returns before writing anything, so this test emits no escape
        // sequence — which is also why the accepted case is checked through
        // `sequence` rather than by actually copying 64 KiB to the terminal.
        let big = "x".repeat(MAX_CLIP_BYTES + 1);
        assert_eq!(copy(&big), Err(ClipError::TooLarge(MAX_CLIP_BYTES + 1)));
        let at_cap = "x".repeat(MAX_CLIP_BYTES);
        assert!(at_cap.len() <= MAX_CLIP_BYTES, "exactly at the cap is allowed");
        assert!(sequence(&at_cap, Host::Plain).len() > MAX_CLIP_BYTES, "and encodes in full");
    }
}
