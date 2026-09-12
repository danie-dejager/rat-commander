//! Log-level colouring for plain text: a line that names a severity near its
//! start is drawn in that severity's colour, so errors and warnings stand out
//! in a log scrolling past in follow mode.
//!
//! Deliberately a heuristic over the head of the line rather than a parser of
//! any one log format. Syslog, journald exports, logfmt (`level=warn`), bracketed
//! (`[ERROR]`) and most application loggers all put the level within the first
//! few dozen characters, and looking no further keeps a message that merely
//! *mentions* an error ("retrying after error") from being painted as one.

use crate::ui::theme::Theme;
use ratatui::style::Color;

/// How far into a line (in characters) a level word may start.
const HEAD: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

/// Whether `name` looks like a log file: `*.log`, or a rotated `*.log.1`.
pub fn is_log_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".log") || lower.contains(".log.")
}

/// The severity the line names, judged by the first level word in its head.
pub fn level_of(line: &str) -> Option<Level> {
    let head: String = line.chars().take(HEAD).collect();
    head.split(|c: char| !c.is_ascii_alphanumeric()).find_map(word_level)
}

/// The level a single word names, if it names one (case-insensitively).
fn word_level(word: &str) -> Option<Level> {
    match word.to_ascii_lowercase().as_str() {
        "fatal" | "panic" | "crit" | "critical" | "alert" | "emerg" | "err" | "error" => {
            Some(Level::Error)
        }
        "warn" | "warning" => Some(Level::Warn),
        "info" | "notice" => Some(Level::Info),
        "debug" | "trace" => Some(Level::Debug),
        _ => None,
    }
}

/// The colour a line of `level` is drawn in, from the active theme: errors in
/// its error colour, warnings in the accent that marks modified files, debug
/// output dimmed. Informational lines keep the ordinary text colour.
pub fn color(level: Level, theme: &Theme) -> Option<Color> {
    match level {
        Level::Error => Some(theme.error_fg),
        Level::Warn => Some(theme.hotkey_fg),
        Level::Info => None,
        Level::Debug => Some(theme.panel_border),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_formats_are_recognised() {
        assert_eq!(level_of("2026-09-12T10:00:00Z ERROR db: gone"), Some(Level::Error));
        assert_eq!(level_of("[warn] disk almost full"), Some(Level::Warn));
        assert_eq!(level_of("ts=1 level=debug msg=hi"), Some(Level::Debug));
        assert_eq!(level_of("Sep 12 10:00:00 host kernel: <err> bad"), Some(Level::Error));
        assert_eq!(level_of("INFO starting"), Some(Level::Info));
    }

    #[test]
    fn the_first_level_word_wins_and_only_whole_words_count() {
        assert_eq!(level_of("INFO retrying after error"), Some(Level::Info));
        assert_eq!(level_of("errors are counted, informally"), None);
        assert_eq!(level_of("plain text"), None);
    }

    #[test]
    fn a_level_mentioned_late_in_the_line_is_ignored() {
        let line = format!("{} error", "x".repeat(HEAD));
        assert_eq!(level_of(&line), None);
    }

    #[test]
    fn log_names() {
        assert!(is_log_name("app.log"));
        assert!(is_log_name("APP.LOG"));
        assert!(is_log_name("syslog.log.1"));
        assert!(!is_log_name("catalog.txt"));
        assert!(!is_log_name("blog"));
    }
}
