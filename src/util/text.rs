//! Display-width-aware string helpers (handles wide/CJK characters).

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Truncate `s` so its display width is at most `max`, appending nothing.
/// Returns the truncated string and its actual display width.
pub fn truncate_width(s: &str, max: usize) -> (String, usize) {
    if s.width() <= max {
        let w = s.width();
        return (s.to_string(), w);
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = ch.width().unwrap_or(0);
        if w + cw > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    (out, w)
}

/// Truncate to `max`, putting an ellipsis at the end if it was shortened.
pub fn ellipsize(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let (mut out, mut w) = truncate_width(s, max.saturating_sub(1));
    out.push('~');
    w += 1;
    let _ = w;
    out
}

/// Left-align `s` in a field of display width `width`, padding with spaces.
pub fn pad_right(s: &str, width: usize) -> String {
    let (mut out, w) = truncate_width(s, width);
    for _ in w..width {
        out.push(' ');
    }
    out
}

/// Right-align `s` in a field of display width `width`.
pub fn pad_left(s: &str, width: usize) -> String {
    let (t, w) = truncate_width(s, width);
    let mut out = String::with_capacity(width);
    for _ in w..width {
        out.push(' ');
    }
    out.push_str(&t);
    out
}

/// Break `s` into lines no wider than `width` display cells. Lines break at
/// spaces, and also between wide (CJK) characters, since those scripts don't
/// separate words with spaces. A word longer than a whole line is split
/// wherever it has to be.
pub fn wrap(s: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    // Break opportunities: each token is a run of narrow characters or a single
    // wide one, flagged with whether a space came before it.
    let mut tokens: Vec<(String, bool)> = Vec::new();
    let mut spaced = false;
    for ch in s.chars() {
        if ch.is_whitespace() {
            spaced = true;
            continue;
        }
        let wide = ch.width().unwrap_or(0) > 1;
        match tokens.last_mut() {
            Some((run, _)) if !spaced && !wide && !run.chars().any(|c| c.width() > Some(1)) => {
                run.push(ch)
            }
            _ => tokens.push((ch.to_string(), spaced)),
        }
        spaced = false;
    }

    let mut lines = Vec::new();
    let (mut line, mut line_w) = (String::new(), 0);
    for (token, spaced) in tokens {
        let gap = usize::from(spaced && !line.is_empty());
        if !line.is_empty() && line_w + gap + token.width() > width {
            lines.push(std::mem::take(&mut line));
            line_w = 0;
        } else if gap == 1 {
            line.push(' ');
            line_w += 1;
        }
        for ch in token.chars() {
            let cw = ch.width().unwrap_or(0);
            if !line.is_empty() && line_w + cw > width {
                lines.push(std::mem::take(&mut line));
                line_w = 0;
            }
            line.push(ch);
            line_w += cw;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_breaks_at_spaces_within_the_width() {
        let lines = wrap("the quick brown fox jumps over the lazy dog", 16);
        assert_eq!(lines, ["the quick brown", "fox jumps over", "the lazy dog"]);
        assert!(wrap("  ", 10).is_empty(), "blank text has no lines");
        assert_eq!(wrap("a  b", 10), ["a b"], "runs of spaces collapse");
    }

    #[test]
    fn wrap_splits_a_word_longer_than_the_line() {
        assert_eq!(wrap("abcdefgh ij", 3), ["abc", "def", "gh", "ij"]);
    }

    #[test]
    fn wrap_breaks_between_wide_characters_and_counts_them_double() {
        // Six CJK characters are twelve cells: four to a five-cell line would be
        // too wide, so each line holds two.
        let lines = wrap("日本語の文章", 5);
        assert_eq!(lines, ["日本", "語の", "文章"]);
        // Latin words next to CJK stay whole, and a space before one is kept.
        assert_eq!(wrap("3D 表示: ファイル", 9), ["3D 表示:", "ファイル"]);
        assert!(wrap("Nerd Fontシンボル", 40).iter().all(|l| l.width() <= 40));
    }
}
