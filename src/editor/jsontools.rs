//! The JSON tools in the editor (Format → JSON): pretty-print (Alt-F), minify
//! and sort keys. Each rewrites the whole buffer as one undo step, and keeps
//! the cursor on the same token.

use super::EditorState;
use crate::json::format::{self, Tool};
use crate::lint::Lang;

impl EditorState {
    /// Whether the file is checked as JSON — what the JSON tools act on.
    pub(super) fn is_json(&self) -> bool {
        matches!(self.check_lang(), Some(Lang::Json(_))) && self.hex.is_none()
    }

    pub(super) fn json_tool(&mut self, tool: Tool) {
        let Some(Lang::Json(opts)) = self.check_lang() else {
            self.status = "Not a JSON file".to_string();
            return;
        };
        let text = self.buf.text();
        let indent = if self.opts.fill_tabs_with_spaces {
            " ".repeat(self.opts.tab_spacing.max(1))
        } else {
            "\t".to_string()
        };
        let (out, note) = match format::apply(tool, &text, opts, &indent) {
            Ok(r) => r,
            Err(msg) => {
                self.status = msg;
                return;
            }
        };
        if out == text {
            self.status = "Nothing to change".to_string();
            return;
        }
        // The cursor goes to the same place among the characters that aren't
        // whitespace, which reformatting doesn't change.
        let solid = text.chars().take(self.cursor).filter(|c| !c.is_whitespace()).count();
        self.buf.break_undo_group();
        let len = self.buf.len_chars();
        self.buf.replace_range(0, len, &out);
        self.buf.break_undo_group();
        let mut seen = 0;
        self.cursor = out
            .chars()
            .position(|c| {
                let here = seen == solid && !c.is_whitespace();
                seen += usize::from(!c.is_whitespace());
                here
            })
            .unwrap_or_else(|| self.buf.len_chars());
        self.dirty = true;
        self.goal_col = None;
        self.clear_marks();
        if let Some(hl) = self.hl.as_mut() {
            hl.invalidate(0);
        }
        let done = match tool {
            Tool::Pretty => "Pretty-printed",
            Tool::Minify => "Minified",
            Tool::SortKeys => "Keys sorted",
        };
        self.status = match note {
            Some(n) => format!("{done} ({n})"),
            None => done.to_string(),
        };
    }
}

#[cfg(test)]
mod tests {
    use crate::editor::EditorState;
    use crate::vfs::VfsPath;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn alt_f_pretty_prints_as_one_undo_step_and_keeps_the_cursor_on_its_token() {
        let text = "{\"b\":[1,2],\"a\":\"x\"}";
        let mut e = EditorState::new("data.json".into(), VfsPath::local("/tmp/x"), text);
        e.check_now();
        e.cursor = text.find("\"a\"").unwrap();
        e.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));
        let pretty = e.buf.text();
        assert_eq!(pretty, "{\n    \"b\": [\n        1,\n        2\n    ],\n    \"a\": \"x\"\n}\n");
        assert_eq!(e.cursor, pretty.find("\"a\"").unwrap(), "still on \"a\"");
        assert_eq!(e.status, "Pretty-printed");
        e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.buf.text(), text, "one undo puts it back");
    }

    #[test]
    fn the_tools_refuse_what_they_cant_do() {
        let mut e = EditorState::new("notes.txt".into(), VfsPath::local("/tmp/x"), "{}");
        e.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::ALT));
        assert_eq!(e.status, "Not a JSON file");
        let mut e = EditorState::new("a.json".into(), VfsPath::local("/tmp/x"), "{\"a\": 1,}");
        e.check_now();
        e.json_tool(crate::json::format::Tool::SortKeys);
        assert_eq!(e.status, "Fix 1 syntax error first");
        assert_eq!(e.buf.text(), "{\"a\": 1,}");
    }
}
