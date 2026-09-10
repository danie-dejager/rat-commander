//! Viewer/editor lifecycle, fetch-to-temp, and directory/file comparison.

use super::*;

impl AppState {
    /// If we remember where the cursor was last left in this (local) file, put it
    /// there and center it. Only local text files are keyed (a remote session's
    /// path isn't stable across runs); hex buffers restore nothing.
    pub(in crate::app::state) fn restore_editor_position(ed: &mut EditorState) {
        if ed.path.scheme != "file" || ed.is_hex() || ed.is_unnamed() || !ed.save_file_position() {
            return;
        }
        if let Some((line, col)) = crate::config::load_editor_position(&ed.path.display()) {
            ed.restore_position(line, col);
        }
    }

    /// Remember the current editor cursor position for the (local) file being
    /// edited, so re-opening it restores the cursor. Called as the editor closes.
    pub(in crate::app::state) fn record_editor_position(&self) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        if ed.path.scheme != "file" || ed.is_hex() || ed.is_unnamed() || !ed.save_file_position() {
            return;
        }
        let (line, col) = ed.cursor_line_col();
        crate::config::save_editor_position(&ed.path.display(), line, col);
    }

    /// Set a freshly built editor up from the saved options: wrap mode, tab and
    /// typing behaviour, and whether it gets a syntax highlighter. Every editor
    /// the app opens goes through this.
    pub(in crate::app::state) fn prepare_editor(&self, ed: &mut EditorState) {
        ed.set_options(self.config.editor_options.clone(), self.dark_ui());
    }

    /// Apply an [`EditorSignal`] (from a key or a mouse gesture): save, close,
    /// or raise the relevant modal dialog.
    pub(in crate::app::state) async fn apply_editor_signal(&mut self, signal: EditorSignal) {
        match signal {
            EditorSignal::Stay => {}
            EditorSignal::Close => {
                self.record_editor_position();
                self.editor = None;
                self.reload_all().await;
            }
            EditorSignal::Save { close_after } => {
                if self.editor.as_ref().is_some_and(|e| e.is_unnamed()) {
                    // No filename yet → go straight to "Save as" (nothing to
                    // confirm, since there's no named file to overwrite).
                    self.open_save_as(None);
                } else if close_after {
                    self.save_editor(true).await;
                } else if self.editor.as_ref().is_some_and(|e| e.confirm_before_saving()) {
                    let name = self.editor.as_ref().map(|e| e.name.clone()).unwrap_or_default();
                    self.dialog = Some(Dialog::Confirm(ConfirmDialog::save_editor(&name)));
                } else {
                    self.save_editor(false).await;
                }
            }
            EditorSignal::SaveAs => self.open_save_as(None),
            EditorSignal::ConfirmQuit => {
                let name = self.editor.as_ref().map(|e| e.name.clone()).unwrap_or_default();
                self.dialog = Some(Dialog::Confirm(ConfirmDialog::editor_quit(&name)));
            }
            EditorSignal::OpenSearch => {
                self.dialog = Some(Dialog::SearchReplace(self.search_dialog(false)));
            }
            EditorSignal::OpenReplace => {
                self.dialog = Some(Dialog::SearchReplace(self.search_dialog(true)));
            }
            EditorSignal::NewFile => {
                // Starting a new buffer replaces this one, so unsaved work has to
                // be dealt with first — the same rule File → Open follows.
                if self.editor.as_ref().is_some_and(|e| e.dirty) {
                    self.show_error("Save or discard this file before starting a new one");
                } else {
                    self.record_editor_position();
                    self.open_new_editor();
                }
            }
            EditorSignal::Browse(kind) => self.open_editor_browser(kind),
            EditorSignal::OpenGotoLine => {
                let line = self.editor.as_ref().map(|e| e.cursor_line_col().0 + 1).unwrap_or(1);
                self.dialog = Some(Dialog::Input(InputDialog::new(
                    "Go to line",
                    "Line number",
                    line.to_string(),
                    InputPurpose::EditorGotoLine,
                )));
            }
            EditorSignal::OpenSortBlock => {
                self.dialog = Some(Dialog::Form(FormDialog::editor_sort()));
            }
            EditorSignal::OpenPasteOutput => {
                self.dialog = Some(Dialog::Input(InputDialog::new(
                    "Paste output of",
                    "Command",
                    "",
                    InputPurpose::EditorPasteOutput,
                )));
            }
            EditorSignal::OpenOptions => {
                let opts = self
                    .editor
                    .as_ref()
                    .map(|e| e.options().clone())
                    .unwrap_or_else(|| self.config.editor_options.clone());
                self.dialog = Some(Dialog::Form(FormDialog::editor_options(&opts)));
            }
            EditorSignal::SaveSetup => self.save_editor_setup(),
            EditorSignal::About => {
                self.dialog = Some(Dialog::Message(MessageDialog::info(
                    "About",
                    format!(
                        "Rat Commander {}\n\nInternal editor\n{}",
                        env!("CARGO_PKG_VERSION"),
                        env!("CARGO_PKG_REPOSITORY"),
                    ),
                )));
            }
            // The screen is repainted from scratch on the next frame.
            EditorSignal::RefreshScreen => self.force_clear = true,
        }
    }

    /// Open the editor's file browser for one of the File menu's actions,
    /// starting in the edited file's own directory.
    fn open_editor_browser(&mut self, kind: crate::editor::BrowseKind) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        let cwd = || std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let dir = if ed.path.scheme == "file" {
            ed.path.path.parent().map(|p| p.to_path_buf()).unwrap_or_else(cwd)
        } else {
            cwd()
        };
        // Only "copy to file" writes, and it needs a name to start from.
        let name = match kind {
            crate::editor::BrowseKind::CopyTo => ed.name.clone(),
            _ => String::new(),
        };
        self.dialog = Some(Dialog::SaveAs(SaveAsDialog::browse(kind, dir, name)));
    }

    /// Carry out a path picked in the editor's file browser.
    pub(in crate::app::state) async fn editor_browsed(
        &mut self,
        kind: crate::editor::BrowseKind,
        path: std::path::PathBuf,
    ) {
        use crate::editor::BrowseKind;
        match kind {
            BrowseKind::Open => {
                // A modified buffer would be lost — make the user save or discard
                // it first (the same rule File → New follows).
                if self.editor.as_ref().is_some_and(|e| e.dirty) {
                    return self.show_error("Save or discard this file before opening another");
                }
                match tokio::fs::read(&path).await {
                    Ok(data) => {
                        let name = crate::vfs::VfsPath::local(&path).file_name();
                        let text = String::from_utf8_lossy(&data).into_owned();
                        if let Some(ed) = self.editor.as_mut() {
                            ed.load_text(name, crate::vfs::VfsPath::local(&path), &text);
                            Self::restore_editor_position(ed);
                        }
                    }
                    Err(e) => self.show_error(format!("Cannot open file: {e}")),
                }
            }
            BrowseKind::Insert => match tokio::fs::read(&path).await {
                Ok(data) => {
                    let text = String::from_utf8_lossy(&data).into_owned();
                    if let Some(ed) = self.editor.as_mut() {
                        ed.insert_at_cursor(&text);
                    }
                }
                Err(e) => self.show_error(format!("Cannot read file: {e}")),
            },
            BrowseKind::CopyTo => {
                let Some(text) = self.editor.as_ref().map(|e| e.block_or_all()) else {
                    return;
                };
                match tokio::fs::write(&path, text.as_bytes()).await {
                    Ok(()) => self.reload_all().await,
                    Err(e) => self.show_error(format!("Cannot write file: {e}")),
                }
            }
        }
    }

    /// Run `cmd` through the user's shell and insert its output at the editor's
    /// cursor (Format → Paste output of…).
    pub(in crate::app::state) async fn editor_paste_output(&mut self, cmd: String) {
        let argv = crate::shell::command_argv(&cmd);
        let out = crate::shell::command_from(argv).output().await;
        match out {
            Ok(o) => {
                // Failures still have something to say — paste stderr so the user
                // sees why nothing came back, rather than a silent no-op.
                let bytes = if o.stdout.is_empty() && !o.status.success() { &o.stderr } else { &o.stdout };
                let text = String::from_utf8_lossy(bytes).into_owned();
                if text.is_empty() {
                    return self.show_error(format!("{cmd}: no output"));
                }
                if let Some(ed) = self.editor.as_mut() {
                    ed.insert_at_cursor(&text);
                }
            }
            Err(e) => self.show_error(format!("Cannot run {cmd}: {e}")),
        }
    }

    /// Apply new editor options: to the open editor at once, and to the config
    /// so the next file opens with them too.
    pub(in crate::app::state) fn apply_editor_options(&mut self, opts: crate::config::EditorOptions) {
        self.config.editor_options = opts.clone();
        let dark = self.dark_ui();
        if let Some(ed) = self.editor.as_mut() {
            ed.set_options(opts, dark);
        }
        if let Err(e) = self.config.save() {
            self.show_error(format!("Could not save settings: {e}"));
        }
    }

    /// Options → Save setup: write the editor's current options to the config.
    fn save_editor_setup(&mut self) {
        if let Some(ed) = self.editor.as_ref() {
            self.config.editor_options = ed.options().clone();
        }
        match self.config.save() {
            Ok(()) => {
                if let Some(ed) = self.editor.as_mut() {
                    ed.set_status("Setup saved");
                }
            }
            Err(e) => self.show_error(format!("Could not save settings: {e}")),
        }
    }

    /// Apply a [`ViewerSignal`] (from a key or a mouse gesture).
    pub(in crate::app::state) fn apply_viewer_signal(&mut self, sig: ViewerSignal) {
        match sig {
            ViewerSignal::Stay => {}
            ViewerSignal::Close => self.viewer = None,
            ViewerSignal::OpenGoto => {
                self.dialog = Some(Dialog::Goto(GotoDialog::new()));
            }
            // The viewer uses the editor's search dialog, so both offer the same
            // modes and options (Replace is meaningless on a read-only view).
            ViewerSignal::OpenSearch => {
                self.dialog = Some(Dialog::SearchReplace(self.search_dialog(false)));
            }
            // F1 opens the manual, replacing the current viewer — the "Help"
            // label on the viewer's F-key bar now does what it says.
            ViewerSignal::OpenHelp => self.open_help(),
        }
    }

    /// F3: view the file under the cursor (internal viewer or external pager).
    pub(in crate::app::state) async fn open_view(&mut self) -> Flow {
        let p = &self.panels[self.active];
        let Some(e) = p.current_entry() else {
            return Flow::Continue;
        };
        if e.kind.is_dir() {
            return Flow::Continue;
        }
        let name = e.name.clone();
        let size = e.size;
        let path = p.cwd.join(&name);
        let backend = p.backend.clone();
        if self.refuse_lossy(std::slice::from_ref(&path)) {
            return Flow::Continue;
        }

        // An rc.ext `View` rule takes precedence (MC behaviour): pipe a command's
        // output into the viewer, or run it in the foreground.
        if let Some(flow) = self.try_ext_view(&name) {
            return flow;
        }

        if !self.config.wants_internal_viewer() {
            return Flow::RunExternal {
                program: self.config.external_viewer().unwrap_or_default(),
                path: path.path,
            };
        }

        if path.scheme == "file" {
            // Local: page straight from disk — never load the whole file. The
            // line-index scan runs off-thread so it doesn't block the reactor.
            let local = path.path.clone();
            let dark = self.dark_ui();
            let scanned = tokio::task::spawn_blocking(move || crate::viewer::scan_file(&local)).await;
            match scanned {
                Ok(Ok((file, len, line_starts, scanned))) => {
                    let mut v = ViewerState::from_scanned(name, file, len, line_starts, scanned, None);
                    v.enable_syntax(dark);
                    v.set_search_seed(self.search_memory.viewer_query.clone());
                    // A find-file content hit opens the viewer at its matching
                    // line, so F3 on a result lands where the search matched.
                    if let Some(line) = self.find_hit_lines.get(&path.display()) {
                        v.goto(&line.to_string(), crate::viewer::GotoMode::Line);
                    }
                    // A supported image opens showing the decoded image fullscreen
                    // (it falls back to the raw text/hex view if it can't decode).
                    if crate::util::img::is_image_name(&v.name)
                        && let Some(iv) = load_view_image(&path.path).await
                    {
                        v.set_image(iv);
                    }
                    self.viewer = Some(v);
                }
                Ok(Err(e)) => self.show_error(format!("Cannot open file: {e}")),
                Err(_) => self.show_error("Viewer failed to open file"),
            }
        } else {
            // Remote/archive: stream to a temp file with a cancellable progress
            // bar; the viewer then pages from that temp copy.
            self.start_fetch(FetchKind::View, name, path, backend, size);
        }
        Flow::Continue
    }

    /// F4: edit the file under the cursor with the internal editor (or a
    /// configured external editor).
    pub(in crate::app::state) async fn open_edit(&mut self) -> Flow {
        let p = &self.panels[self.active];
        let Some(e) = p.current_entry() else {
            return Flow::Continue;
        };
        if e.kind.is_dir() {
            return Flow::Continue;
        }
        let name = e.name.clone();
        let size = e.size;
        let path = p.cwd.join(&name);
        let backend = p.backend.clone();
        if self.refuse_lossy(std::slice::from_ref(&path)) {
            return Flow::Continue;
        }

        // An rc.ext `Edit` rule takes precedence (MC behaviour): run its command
        // in the foreground instead of the built-in editor.
        if let Some(flow) = self.try_ext_edit() {
            return flow;
        }

        if !self.config.wants_internal_editor() {
            return Flow::RunExternal {
                program: self.config.external_editor().unwrap_or_default(),
                path: path.path,
            };
        }

        let local = path.scheme == "file";
        // Local files too big to load as text open directly in (in-place) hex mode.
        if local && size > crate::editor::MAX_TEXT_EDIT {
            match EditorState::new_hex(name, path) {
                Ok(mut ed) => {
                    self.prepare_editor(&mut ed);
                    self.editor = Some(ed);
                }
                Err(e) => self.show_error(format!("Cannot open file: {e}")),
            }
            return Flow::Continue;
        }
        if local {
            match load_file(&backend, &path).await {
                Ok(data) => {
                    let text = String::from_utf8_lossy(&data).into_owned();
                    let mut ed = EditorState::new(name, path, &text);
                    self.prepare_editor(&mut ed);
                    Self::restore_editor_position(&mut ed);
                    self.editor = Some(ed);
                }
                Err(e) => self.show_error(format!("Cannot open file: {e}")),
            }
            return Flow::Continue;
        }
        // Remote/archive: in-place hex editing isn't possible (no random write),
        // so editing requires loading into memory — cap the size and stream the
        // download with a cancellable progress bar.
        if size > crate::editor::MAX_TEXT_EDIT {
            self.show_error("File too large to edit over this connection");
            return Flow::Continue;
        }
        self.start_fetch(FetchKind::Edit, name, path, backend, size);
        Flow::Continue
    }

    /// Open a local file directly in the internal editor — used by the `/edit`
    /// startup mode. A missing file opens an empty buffer (so it can be created);
    /// a file too large to load as text opens in in-place hex mode.
    pub(crate) async fn open_path_in_editor(&mut self, path: std::path::PathBuf) {
        // Resolve to an absolute path so saving is unaffected by any later change
        // of the active panel's directory.
        let abs = if path.is_absolute() {
            path
        } else {
            std::env::current_dir().map(|d| d.join(&path)).unwrap_or(path)
        };
        let name = abs
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| abs.to_string_lossy().into_owned());
        let meta = std::fs::metadata(&abs).ok();
        if meta.as_ref().is_some_and(|m| m.is_dir()) {
            self.show_error(format!("{name} is a directory"));
            return;
        }
        let vpath = VfsPath::local(&abs);
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        if size > crate::editor::MAX_TEXT_EDIT {
            match EditorState::new_hex(name, vpath) {
                Ok(mut ed) => {
                    self.prepare_editor(&mut ed);
                    self.editor = Some(ed);
                }
                Err(e) => self.show_error(format!("Cannot open file: {e}")),
            }
            return;
        }
        let text = std::fs::read(&abs)
            .map(|b| String::from_utf8_lossy(&b).into_owned())
            .unwrap_or_default();
        let mut ed = EditorState::new(name, vpath, &text);
        self.prepare_editor(&mut ed);
        Self::restore_editor_position(&mut ed);
        self.editor = Some(ed);
    }

    /// Open the editor on a fresh, unnamed buffer (`rc /edit` with no file). It
    /// has no path yet, so the first save is redirected to "Save as" (see
    /// [`Self::save_editor`]).
    pub(crate) fn open_new_editor(&mut self) {
        let mut ed = EditorState::new_unnamed();
        self.prepare_editor(&mut ed);
        self.editor = Some(ed);
    }

    /// Stream a (remote/archive) file to a local temp file for view/edit, showing
    /// a cancellable progress dialog. Delivers `FileFetched` on success.
    fn start_fetch(
        &mut self,
        kind: FetchKind,
        name: String,
        path: VfsPath,
        backend: std::sync::Arc<dyn Vfs>,
        total: u64,
    ) {
        let id = self.next_task_id;
        self.next_task_id += 1;
        let cancel = CancelToken::new();
        let (reply, _reply_rx) = tokio::sync::mpsc::channel(1);
        self.tasks.insert(
            id,
            TaskHandle {
                id,
                cancel: cancel.clone(),
                reply,
            },
        );
        self.dialog = Some(Dialog::Progress(ProgressDialog::new(id, "Reading")));

        let temp = crate::util::temp::rc_temp_path("fetch");
        let tx = self.tx.clone();
        let orig_path = path.clone();
        tokio::spawn(async move {
            let outcome = fetch_to_temp(&backend, &path, &temp, total, &cancel, id, &name, &tx).await;
            match outcome {
                Ok(true) => {
                    let _ = tx
                        .send(AppEvent::FileFetched { id, kind, name, orig_path, temp })
                        .await;
                }
                Ok(false) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    let _ = tx
                        .send(AppEvent::TaskDone { id, outcome: TaskOutcome::Cancelled })
                        .await;
                }
                Err(e) => {
                    let _ = tokio::fs::remove_file(&temp).await;
                    let _ = tx
                        .send(AppEvent::TaskDone { id, outcome: TaskOutcome::Failed(e) })
                        .await;
                }
            }
        });
    }

    /// Persist the editor's contents to its file, optionally closing after.
    pub(in crate::app::state) async fn save_editor(&mut self, close_after: bool) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        // A fresh, unnamed buffer has no path to write to — send the user to
        // "Save as" to choose a filename first (`close_after` can't be honored
        // here; the user saves, then quits again once the buffer has a name).
        if ed.is_unnamed() {
            self.open_save_as(None);
            return;
        }
        // Hex mode writes only the changed bytes in place — never rewrite the
        // whole (possibly huge) file from the text buffer.
        if ed.is_hex() {
            let res = self.editor.as_mut().unwrap().flush_hex();
            match res {
                Ok(()) => {
                    if close_after {
                        self.editor = None;
                        self.reload_all().await;
                    } else if let Some(ed) = self.editor.as_mut() {
                        ed.mark_saved();
                    }
                }
                Err(e) => self.show_error(format!("Save failed: {e}")),
            }
            return;
        }
        let contents = ed.contents();
        let path = ed.path.clone();
        let backend = match self.registry.resolve(&path) {
            Ok(b) => b,
            Err(e) => return self.show_error(format!("Cannot save file: {e}")),
        };
        match write_file(&backend, &path, contents.as_bytes()).await {
            Ok(()) => {
                if close_after {
                    self.record_editor_position();
                    self.editor = None;
                    self.reload_all().await;
                } else if let Some(ed) = self.editor.as_mut() {
                    ed.mark_saved();
                }
                // Editing a config file (themes.toml, the F2 menu, rc.ext)
                // applies immediately on save.
                self.reload_config_if_edited(&path);
            }
            // A failed save (e.g. read-only location, permission denied) drops the
            // user into "Save as" prefilled with the path, so they can redirect it.
            Err(e) => self.open_save_as(Some(format!("Save failed: {e}"))),
        }
    }

    /// Open the editor's "Save as" browser, prefilled with the current file's
    /// directory and name. `error` shows the reason a prior save failed.
    pub(in crate::app::state) fn open_save_as(&mut self, error: Option<String>) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        let cwd = || std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        // An unnamed buffer has no meaningful directory or name to prefill:
        // start in the working directory with an empty name field.
        let (dir, name) = if ed.is_unnamed() {
            (cwd(), String::new())
        } else if ed.path.scheme == "file" {
            let dir = ed.path.path.parent().map(|p| p.to_path_buf()).unwrap_or_else(cwd);
            (dir, ed.name.clone())
        } else {
            // The browser is local-only, so a remote-edited file saves to disk.
            (cwd(), ed.name.clone())
        };
        self.dialog = Some(Dialog::SaveAs(SaveAsDialog::new(dir, name, error)));
    }

    /// Write the editor buffer to `dest` (from the "Save as" dialog) and retarget
    /// the editor at the new path on success.
    pub(in crate::app::state) async fn do_save_as(&mut self, dest: std::path::PathBuf) {
        let Some(ed) = self.editor.as_ref() else {
            return;
        };
        let contents = ed.contents();
        match tokio::fs::write(&dest, contents.as_bytes()).await {
            Ok(()) => {
                let name = dest
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dest.to_string_lossy().into_owned());
                let dark = self.dark_ui();
                let new_path = VfsPath::local(&dest);
                if let Some(ed) = self.editor.as_mut() {
                    ed.path = new_path.clone();
                    ed.name = name;
                    ed.set_named(); // it now has a filename; future saves write in place
                    ed.mark_saved();
                    ed.enable_syntax(dark); // re-detect syntax for the new name
                }
                self.reload_config_if_edited(&new_path);
                self.reload_all().await;
            }
            // Still failing: re-prompt with the new error so the user can adjust.
            Err(e) => self.open_save_as(Some(format!("Save failed: {e}"))),
        }
    }

    /// Open the visual theme editor (Options → Edit themes), starting on the
    /// app's currently-active theme.
    pub(in crate::app::state) fn open_edit_themes(&mut self) {
        self.theme_editor =
            Some(crate::ui::theme_editor::ThemeEditor::new(&self.config.theme, self.truecolor));
    }

    /// Apply a signal from the visual theme editor: close it, or persist the
    /// edited spec to `themes.toml` and re-derive the live theme.
    pub(in crate::app::state) fn apply_theme_editor_signal(
        &mut self,
        sig: crate::ui::theme_editor::ThemeEditorSignal,
    ) {
        use crate::ui::theme_editor::ThemeEditorSignal;
        match sig {
            ThemeEditorSignal::Stay => {}
            ThemeEditorSignal::Close => self.theme_editor = None,
            ThemeEditorSignal::Save(spec) => match crate::ui::theme::save_spec(*spec) {
                Ok(()) => {
                    if let Some(te) = self.theme_editor.as_mut() {
                        te.mark_saved();
                    }
                    // Re-derive the active theme: if the saved theme is the one in
                    // use, its edits take effect at once; otherwise this is a no-op.
                    self.theme = Theme::by_name(&self.config.theme, self.truecolor);
                }
                Err(e) => self.show_error(format!("Could not save theme: {e}")),
            },
            ThemeEditorSignal::SaveAndClose(spec) => match crate::ui::theme::save_spec(*spec) {
                Ok(()) => {
                    self.theme = Theme::by_name(&self.config.theme, self.truecolor);
                    self.theme_editor = None;
                }
                // Keep the editor open on a write error so the edits aren't lost.
                Err(e) => self.show_error(format!("Could not save theme: {e}")),
            },
        }
    }

    /// Open `rc.ext` (file associations) in the internal editor (Options → Edit
    /// extensions), creating the default file first if it doesn't exist yet.
    pub(in crate::app::state) fn open_edit_extensions(&mut self) {
        let Some(path) = crate::ext::ensure_ext_file() else {
            return self.show_error("No config directory available");
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut ed = EditorState::new("rc.ext".to_string(), VfsPath::local(&path), &text);
                self.prepare_editor(&mut ed);
                self.editor = Some(ed);
            }
            Err(e) => self.show_error(format!("Cannot open rc.ext: {e}")),
        }
    }

    /// Open the F2 user `menu` file in the internal editor (Options → Edit menu
    /// file), creating the default file first if it doesn't exist yet.
    pub(in crate::app::state) fn open_edit_user_menu(&mut self) {
        let Some(path) = crate::usermenu::ensure_menu_file() else {
            return self.show_error("No config directory available");
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let mut ed = EditorState::new("menu".to_string(), VfsPath::local(&path), &text);
                self.prepare_editor(&mut ed);
                self.editor = Some(ed);
            }
            Err(e) => self.show_error(format!("Cannot open menu: {e}")),
        }
    }

    /// After saving in the internal editor, apply the change to live state at
    /// once when the saved file is one of the user-editable config files —
    /// `themes.toml`, the F2 user `menu`, or `rc.ext` (file associations) — so the
    /// user doesn't have to restart. Ordinary files are ignored.
    fn reload_config_if_edited(&mut self, path: &VfsPath) {
        if path.scheme != "file" {
            return;
        }
        self.reload_themes_if_edited(path);
        self.reload_usermenu_if_edited(path);
        self.reload_ext_if_edited(path);
    }

    /// If the file just saved is `themes.toml`, re-read the palettes and
    /// re-derive the current theme so the change takes effect at once.
    fn reload_themes_if_edited(&mut self, path: &VfsPath) {
        let is_themes = path.scheme == "file"
            && crate::config::paths::themes_file().is_some_and(|p| p == path.path);
        if !is_themes {
            return;
        }
        match crate::ui::theme::reload_user_themes() {
            Ok(_) => self.theme = Theme::by_name(&self.config.theme, self.truecolor),
            Err(e) => self.show_error(format!("themes.toml: {e}")),
        }
    }

    /// If the file just saved is the F2 user `menu`, re-read it so the next F2
    /// reflects the edit without a restart.
    fn reload_usermenu_if_edited(&mut self, path: &VfsPath) {
        let is_menu = crate::config::paths::menu_file().is_some_and(|p| p == path.path);
        if is_menu {
            self.user_menu = crate::usermenu::load_or_create();
        }
    }

    /// If the file just saved is `rc.ext`, re-read the file associations so the
    /// new rules apply to the next Open/View/Edit without a restart.
    fn reload_ext_if_edited(&mut self, path: &VfsPath) {
        let is_ext = crate::config::paths::ext_file().is_some_and(|p| p == path.path);
        if is_ext {
            self.ext_rules = crate::ext::ExtRules::load_or_create();
        }
    }
    /// Compare the two panels' files and mark the differing ones (selection).
    /// `Quick` marks files missing from the other panel; `Size` additionally
    /// marks the larger of two differently-sized files; `Content` marks both
    /// files whenever their bytes differ.
    pub(in crate::app::state) async fn compare_dirs(&mut self, mode: CompareMode) {
        if self.panels[0].is_panelized() || self.panels[1].is_panelized() {
            return self.show_error("Cannot compare search-result panels");
        }
        let files = |p: &Panel| -> Vec<(String, u64)> {
            p.entries
                .iter()
                .filter(|e| e.kind == VfsKind::File && e.name != "..")
                .map(|e| (e.name.clone(), e.size))
                .collect()
        };
        let a = files(&self.panels[0]);
        let b = files(&self.panels[1]);
        let amap: HashMap<&str, u64> = a.iter().map(|(n, s)| (n.as_str(), *s)).collect();
        let bmap: HashMap<&str, u64> = b.iter().map(|(n, s)| (n.as_str(), *s)).collect();

        let mut mark_a: Vec<String> = Vec::new();
        let mut mark_b: Vec<String> = Vec::new();

        // Files present in only one panel are always marked there.
        for (n, _) in &a {
            if !bmap.contains_key(n.as_str()) {
                mark_a.push(n.clone());
            }
        }
        for (n, _) in &b {
            if !amap.contains_key(n.as_str()) {
                mark_b.push(n.clone());
            }
        }

        match mode {
            CompareMode::Quick => {}
            CompareMode::Size => {
                for (n, sa) in &a {
                    if let Some(sb) = bmap.get(n.as_str()) {
                        // Mark only the larger of the two.
                        if sa > sb {
                            mark_a.push(n.clone());
                        } else if sb > sa {
                            mark_b.push(n.clone());
                        }
                    }
                }
            }
            CompareMode::Content => {
                let ba = self.panels[0].backend.clone();
                let ca = self.panels[0].cwd.clone();
                let bb = self.panels[1].backend.clone();
                let cb = self.panels[1].cwd.clone();
                for (n, sa) in &a {
                    if let Some(sb) = bmap.get(n.as_str()) {
                        // Different sizes ⇒ different content (no need to read).
                        let differ = sa != sb
                            || files_differ(&ba, &ca.join(n), &bb, &cb.join(n)).await;
                        if differ {
                            mark_a.push(n.clone());
                            mark_b.push(n.clone());
                        }
                    }
                }
            }
        }

        self.panels[0].selection.clear();
        self.panels[1].selection.clear();
        for n in &mark_a {
            self.panels[0].selection.mark(n);
        }
        for n in &mark_b {
            self.panels[1].selection.mark(n);
        }
    }

    /// Open the side-by-side file comparison view on the files under the cursor
    /// in the left (panel 0) and right (panel 1) panels.
    pub(in crate::app::state) async fn open_compare_files(&mut self) {
        let pick = |p: &Panel| -> Option<(String, VfsPath)> {
            p.current_entry()
                .filter(|e| e.kind == VfsKind::File && e.name != "..")
                .map(|e| (e.name.clone(), p.cwd.join(&e.name)))
        };
        let (Some((ln, lp)), Some((rn, rp))) = (pick(&self.panels[0]), pick(&self.panels[1])) else {
            return self.show_error("Put the cursor on a file in both panels to compare");
        };
        let lback = self.panels[0].backend.clone();
        let rback = self.panels[1].backend.clone();
        let ldata = match load_file(&lback, &lp).await {
            Ok(d) => d,
            Err(e) => return self.show_error(format!("Cannot read {ln}: {e}")),
        };
        let rdata = match load_file(&rback, &rp).await {
            Ok(d) => d,
            Err(e) => return self.show_error(format!("Cannot read {rn}: {e}")),
        };
        self.diffview = Some(DiffView::new(ln, lp, &ldata, rn, rp, &rdata));
    }

    /// Write the diff view's changed buffers back to disk.
    pub(in crate::app::state) async fn save_diff(&mut self) {
        let saves = match self.diffview.as_ref() {
            Some(dv) => dv.pending_saves(),
            None => return,
        };
        if saves.is_empty() {
            return;
        }
        let mut ok = true;
        for (path, contents) in saves {
            match self.registry.resolve(&path) {
                Ok(backend) => {
                    if let Err(e) = write_file(&backend, &path, contents.as_bytes()).await {
                        self.show_error(format!("Save failed: {e}"));
                        ok = false;
                    }
                }
                Err(e) => {
                    self.show_error(format!("Cannot save file: {e}"));
                    ok = false;
                }
            }
        }
        if ok {
            if let Some(dv) = self.diffview.as_mut() {
                dv.mark_saved();
            }
            self.reload_all().await;
        }
    }

}
