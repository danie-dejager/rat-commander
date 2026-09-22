//! Binary templates in the hex editor: the template that fits the file runs on
//! a thread of its own as hex mode opens, and its variables show in a panel
//! beside (or below) the bytes — a tree of names, values, offsets, sizes, types
//! and comments. Moving through the tree moves the byte cursor to the variable,
//! the template's colours tint the bytes, and a value can be edited in place
//! (the bytes it stands for go into the hex editor's unsaved edits, and the
//! template runs again once typing pauses).

use super::{EditorSignal, EditorState, hex};
use crate::bt::header::TemplateInfo;
use crate::bt::interp::Interp;
use crate::bt::interp::display::LazyCache;
use crate::bt::interp::edit::EditTarget;
use crate::bt::library;
use crate::bt::source::{ByteSource, FileSource};
use crate::bt::tree::{ArrayKind, F_HIDDEN, F_OPEN, F_SUPPRESS, NodeKind, NodeRef, ROOT};
use crate::ui::theme::Theme;
use crate::util::text::{ellipsize, pad_right};
use ratatui::Frame;
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::oneshot::{self, error::TryRecvError};

/// How long the bytes must stay unchanged before an edited file is parsed again.
const QUIET: Duration = Duration::from_millis(500);
/// Only a template that ran at least this fast reruns by itself after an edit;
/// a slower one waits for Shift-F5.
const AUTO_RERUN_UNDER: Duration = Duration::from_secs(1);
/// Array elements listed at a time; a "more" row lists the next batch.
const CHUNK: u64 = 1000;
/// The stack callbacks evaluated for drawing run on.
const DISPLAY_STACK: usize = if cfg!(target_pointer_width = "64") { 64 << 20 } else { 16 << 20 };

/// Which template the user wants for this file.
#[derive(Debug, Clone)]
pub enum TemplateChoice {
    /// The one that fits, found by file mask and ID bytes.
    Auto,
    Chosen(Box<TemplateInfo>),
    /// None at all.
    Off,
}

/// A finished run.
struct Run {
    interp: Box<Interp>,
    info: TemplateInfo,
    /// Why it stopped early (`ZIP.bt:120: …`).
    error: Option<String>,
    took: Duration,
    /// The hex revision it parsed.
    rev: u64,
    deps: Vec<PathBuf>,
}

enum Outcome {
    Ran(Run),
    /// No template fits the file.
    NoTemplate,
    /// The template couldn't be read or compiled.
    Failed(TemplateInfo, String),
}

struct Job {
    rx: oneshot::Receiver<Outcome>,
    progress: Arc<AtomicU64>,
    cancel: Arc<AtomicBool>,
    len: u64,
    name: String,
}

impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

/// One row of the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RowKind {
    Node(NodeRef),
    /// Element `i` of a scalar array.
    Elem(NodeRef, u64),
    /// "… more elements" of an array.
    More(NodeRef),
}

#[derive(Debug, Clone, Copy)]
struct Row {
    kind: RowKind,
    depth: u16,
}

/// A row's text, computed when it is first drawn.
#[derive(Debug, Clone, Default)]
struct RowText {
    name: String,
    value: String,
    start: String,
    size: String,
    ty: String,
    comment: String,
    children: bool,
    range: Option<(u64, u64)>,
}

struct ValueEdit {
    target: EditTarget,
    text: String,
    caret: usize,
}

pub struct TemplateState {
    pub choice: TemplateChoice,
    job: Option<Job>,
    run: Option<Run>,
    /// A problem outside a run: nothing fits, or the template didn't compile.
    note: Option<String>,
    /// The template the note is about.
    note_name: String,
    seen_rev: u64,
    changed_at: Instant,
    /// The revision the display source was last given.
    source_rev: u64,
    pub tree_focus: bool,
    pub show_output: bool,
    expanded: HashSet<NodeRef>,
    collapsed: HashSet<NodeRef>,
    shown: HashMap<NodeRef, u64>,
    rows: Vec<Row>,
    rows_valid: bool,
    texts: HashMap<RowKind, RowText>,
    cursor: usize,
    scroll: usize,
    out_scroll: usize,
    edit: Option<ValueEdit>,
    cache: LazyCache,
    /// Where the list's first row is on screen (for the mouse).
    list_area: Rect,
}

impl TemplateState {
    fn new(choice: TemplateChoice) -> Self {
        TemplateState {
            choice,
            job: None,
            run: None,
            note: None,
            note_name: String::new(),
            seen_rev: 0,
            changed_at: Instant::now(),
            source_rev: 0,
            tree_focus: false,
            show_output: false,
            expanded: HashSet::new(),
            collapsed: HashSet::new(),
            shown: HashMap::new(),
            rows: Vec::new(),
            rows_valid: false,
            texts: HashMap::new(),
            cursor: 0,
            scroll: 0,
            out_scroll: 0,
            edit: None,
            cache: LazyCache::default(),
            list_area: Rect::default(),
        }
    }

    /// Whether there is anything for the panel to show.
    fn has_panel(&self) -> bool {
        !matches!(self.choice, TemplateChoice::Off)
            && (self.run.is_some()
                || self.job.is_some()
                || (self.note.is_some() && !self.note_name.is_empty()))
    }

    fn forget_texts(&mut self) {
        self.texts.clear();
        self.cache.clear();
    }

    fn is_open(&self, interp: &Interp, r: NodeRef) -> bool {
        self.expanded.contains(&r)
            || (interp.tree.node(r.id).flags & F_OPEN != 0 && !self.collapsed.contains(&r))
    }
}

/// Parse `file` with the template `choice` names, on this thread.
fn execute(
    choice: TemplateChoice,
    file: &Path,
    file_name: &str,
    overlay: BTreeMap<u64, u8>,
    rev: u64,
    cancel: Arc<AtomicBool>,
    progress: Arc<AtomicU64>,
) -> Outcome {
    let started = Instant::now();
    let info = match choice {
        TemplateChoice::Off => return Outcome::NoTemplate,
        TemplateChoice::Chosen(i) => *i,
        TemplateChoice::Auto => {
            let mut head = vec![0u8; crate::bt::header::ID_WINDOW];
            let n = FileSource::open(file, overlay.clone())
                .map(|mut s| s.read_at(0, &mut head))
                .unwrap_or(0);
            head.truncate(n);
            match library::pick_for(file_name, &head) {
                Some(info) => info,
                None => return Outcome::NoTemplate,
            }
        }
    };
    let src = match FileSource::open(file, overlay) {
        Ok(s) => s,
        Err(e) => return Outcome::Failed(info, e.to_string()),
    };
    let name = file.display().to_string();
    match library::run(&info, Box::new(src), &name, cancel, progress) {
        Ok((interp, error, deps)) => Outcome::Ran(Run {
            interp: Box::new(interp),
            info,
            error,
            took: started.elapsed(),
            rev,
            deps,
        }),
        Err(e) => Outcome::Failed(info, e),
    }
}

/// The rows under open node `r`.
fn child_rows(interp: &mut Interp, r: NodeRef, shown: &HashMap<NodeRef, u64>) -> Vec<RowKind> {
    let n = interp.tree.node(r.id).clone();
    let limit = shown.get(&r).copied().unwrap_or(CHUNK);
    let visible = |interp: &Interp, c: &NodeRef| interp.tree.node(c.id).flags & F_HIDDEN == 0;
    match &n.kind {
        NodeKind::Struct { .. } => {
            interp.open_node(r);
            interp
                .tree
                .child_refs(r)
                .into_iter()
                .filter(|c| visible(interp, c))
                .map(RowKind::Node)
                .collect()
        }
        NodeKind::Array { count, kind: ArrayKind::Scalar, .. } => {
            let upto = (*count).min(limit);
            let mut out: Vec<RowKind> = (0..upto).map(|i| RowKind::Elem(r, i)).collect();
            if upto < *count {
                out.push(RowKind::More(r));
            }
            out
        }
        NodeKind::Array { count, .. } => {
            let upto = (*count).min(limit);
            let mut out: Vec<RowKind> = (0..upto)
                .filter_map(|i| interp.tree.element(r, i))
                .filter(|c| visible(interp, c))
                .map(RowKind::Node)
                .collect();
            if upto < *count {
                out.push(RowKind::More(r));
            }
            out
        }
        _ => Vec::new(),
    }
}

impl TemplateState {
    fn rebuild_rows(&mut self) {
        self.rows_valid = true;
        self.rows.clear();
        let Some(run) = self.run.as_mut() else { return };
        let interp = &mut run.interp;
        let top = child_rows(interp, NodeRef::new(ROOT), &self.shown);
        let mut stack: Vec<Row> =
            top.into_iter().rev().map(|kind| Row { kind, depth: 0 }).collect();
        while let Some(row) = stack.pop() {
            self.rows.push(row);
            if self.rows.len() >= 2_000_000 {
                break;
            }
            if let RowKind::Node(r) = row.kind {
                let open = self.expanded.contains(&r)
                    || (interp.tree.node(r.id).flags & F_OPEN != 0 && !self.collapsed.contains(&r));
                if open && interp.has_children(r) {
                    for kind in child_rows(interp, r, &self.shown).into_iter().rev() {
                        stack.push(Row { kind, depth: row.depth + 1 });
                    }
                }
            }
        }
        self.cursor = self.cursor.min(self.rows.len().saturating_sub(1));
    }

    fn ensure_rows(&mut self) {
        if !self.rows_valid {
            self.rebuild_rows();
        }
    }

    /// The text of `rows`, computing (on a big stack, since callbacks recurse)
    /// the ones not cached yet.
    fn texts_for(&mut self, kinds: &[RowKind]) {
        let missing: Vec<RowKind> =
            kinds.iter().copied().filter(|k| !self.texts.contains_key(k)).collect();
        if missing.is_empty() {
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        let interp: &mut Interp = &mut run.interp;
        let cache = &mut self.cache;
        let computed = std::thread::scope(|s| {
            std::thread::Builder::new()
                .name("bt-show".into())
                .stack_size(DISPLAY_STACK)
                .spawn_scoped(s, || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        missing.iter().map(|&k| (k, row_text(interp, cache, k))).collect::<Vec<_>>()
                    }))
                    .ok()
                })
                .ok()
                .and_then(|h| h.join().ok())
                .flatten()
        });
        match computed {
            Some(list) => self.texts.extend(list),
            None => {
                for k in missing {
                    self.texts.insert(k, RowText { name: "(error)".into(), ..RowText::default() });
                }
            }
        }
    }
}

/// Compute one row's text.
fn row_text(interp: &mut Interp, cache: &mut LazyCache, kind: RowKind) -> RowText {
    match kind {
        RowKind::Node(r) => {
            let (start, size) = interp.range_of(r);
            let bits = match &interp.tree.node(r.id).kind {
                NodeKind::Scalar { bits: Some(b), .. } => Some(b.width),
                _ => None,
            };
            RowText {
                name: interp.row_name(cache, r, None),
                value: interp.row_value(cache, r),
                start: format!("0x{start:X}"),
                size: match bits {
                    Some(w) => format!("{w} bit{}", if w == 1 { "" } else { "s" }),
                    None => size.to_string(),
                },
                ty: interp.type_label(r),
                comment: interp.row_comment(cache, r),
                children: interp.has_children(r),
                range: Some((start, size)),
            }
        }
        RowKind::Elem(r, i) => {
            let n = interp.tree.node(r.id).clone();
            let esize = match n.kind {
                NodeKind::Array { elem_size, .. } => elem_size,
                _ => 1,
            };
            let start = n.start + r.shift + i * esize;
            let ty = interp.type_label(r);
            let elem_ty = ty.rsplit_once('[').map_or(ty.clone(), |(t, _)| t.to_string());
            RowText {
                name: format!("{}[{i}]", interp.prog.name(n.name)),
                value: interp.element_value(r, i),
                start: format!("0x{start:X}"),
                size: esize.to_string(),
                ty: elem_ty,
                comment: String::new(),
                children: false,
                range: Some((start, esize)),
            }
        }
        RowKind::More(r) => {
            let count = match interp.tree.node(r.id).kind {
                NodeKind::Array { count, .. } => count,
                _ => 0,
            };
            RowText { name: format!("… {count} elements"), ..RowText::default() }
        }
    }
}

/// What a run needs to know about the file: path, name, pending edits,
/// revision and length.
type FileState = (PathBuf, String, BTreeMap<u64, u8>, u64, u64);

impl EditorState {
    fn tpl_file(&self) -> Option<FileState> {
        let h = self.hex.as_ref()?;
        Some((self.path.path.clone(), self.name.clone(), h.overlay_snapshot(), h.rev, h.len))
    }

    /// Hex mode has opened: find and run the template for the file.
    pub(super) fn start_templates(&mut self) {
        let choice = self.tpl.take().map(|t| t.choice).unwrap_or(TemplateChoice::Auto);
        self.tpl = Some(TemplateState::new(choice));
        self.run_template();
    }

    /// Run the template again (Shift-F5, or after an edit).
    pub(super) fn run_template(&mut self) {
        let Some((file, name, overlay, rev, len)) = self.tpl_file() else { return };
        let Some(t) = self.tpl.as_mut() else { return };
        if matches!(t.choice, TemplateChoice::Off) {
            t.job = None;
            return;
        }
        let choice = t.choice.clone();
        let label = match &choice {
            TemplateChoice::Chosen(i) => i.file_name.clone(),
            _ => t.run.as_ref().map(|r| r.info.file_name.clone()).unwrap_or_default(),
        };
        let (tx, rx) = oneshot::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(AtomicU64::new(0));
        let (c2, p2) = (cancel.clone(), progress.clone());
        let spawned = std::thread::Builder::new()
            .name("bt-run".into())
            .stack_size(library::RUN_STACK)
            .spawn(move || {
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    execute(choice, &file, &name, overlay, rev, c2, p2)
                }));
                if let Ok(out) = out {
                    let _ = tx.send(out);
                }
            });
        t.seen_rev = rev;
        t.job = spawned.ok().map(|_| Job { rx, progress, cancel, len, name: label });
    }

    /// Leave templates behind (hex mode closed).
    pub(super) fn stop_templates(&mut self) {
        self.tpl = None;
    }

    /// Use `info` for this file from now on, or no template (`None`).
    pub fn set_template(&mut self, info: Option<TemplateInfo>) {
        let Some(t) = self.tpl.as_mut() else { return };
        match info {
            Some(i) => {
                t.choice = TemplateChoice::Chosen(Box::new(i));
                t.expanded.clear();
                t.collapsed.clear();
                t.shown.clear();
                t.cursor = 0;
                t.scroll = 0;
                self.run_template();
            }
            None => {
                t.choice = TemplateChoice::Off;
                t.job = None;
                t.run = None;
                t.note = None;
                t.tree_focus = false;
                t.rows_valid = false;
            }
        }
    }

    /// The template shown (or being run), if any.
    pub fn active_template(&self) -> Option<TemplateInfo> {
        let t = self.tpl.as_ref()?;
        match &t.choice {
            TemplateChoice::Chosen(i) => Some((**i).clone()),
            TemplateChoice::Off => None,
            TemplateChoice::Auto => t.run.as_ref().map(|r| r.info.clone()),
        }
    }

    /// The line of the active template the last run stopped at, for opening it
    /// there.
    pub fn template_error_line(&self) -> Option<usize> {
        let err = self.tpl.as_ref()?.run.as_ref()?.error.as_ref()?;
        let (loc, _) = err.split_once(": ")?;
        loc.rsplit(':').next()?.parse::<usize>().ok()
    }

    /// A template file was edited (and saved) while this hex editor waited:
    /// run again if it's the one in use or something it includes.
    pub fn resume_after_edit(&mut self, edited: &Path) {
        library::invalidate();
        let Some(t) = self.tpl.as_ref() else { return };
        let uses = t.run.as_ref().is_some_and(|r| {
            r.deps.iter().any(|d| d == edited) || r.info.path.as_deref() == Some(edited)
        }) || matches!(&t.choice, TemplateChoice::Chosen(i) if i.path.as_deref() == Some(edited))
            || t.note.is_some();
        if uses {
            // A chosen template's header may have changed with the edit.
            if let Some(t) = self.tpl.as_mut()
                && let TemplateChoice::Chosen(i) = &mut t.choice
                && let Some(p) = i.path.clone()
                && let Ok(data) = std::fs::read(&p)
            {
                let origin = i.origin;
                let mut fresh = crate::bt::header::parse_header(&i.file_name, &data);
                fresh.path = Some(p);
                fresh.origin = origin;
                **i = fresh;
            }
            self.run_template();
        }
    }

    /// Whether a template run is going on or due, so the app keeps its tick.
    pub fn template_pending(&self) -> bool {
        let Some(t) = self.tpl.as_ref() else { return false };
        let rev = self.hex.as_ref().map_or(0, |h| h.rev);
        t.job.is_some()
            || (t.run.as_ref().is_some_and(|r| r.rev != rev && r.took < AUTO_RERUN_UNDER))
    }

    /// The heartbeat, on the app's tick: collect a finished run, and rerun once
    /// edits have paused. Returns whether anything on screen changed.
    pub fn poll_template(&mut self, now: Instant) -> bool {
        let rev = self.hex.as_ref().map_or(0, |h| h.rev);
        let path = self.path.path.clone();
        let overlay = self.hex.as_ref().map(|h| h.overlay_snapshot());
        let Some(t) = self.tpl.as_mut() else { return false };
        let mut changed = false;
        if let Some(job) = t.job.as_mut() {
            match job.rx.try_recv() {
                Ok(outcome) => {
                    t.job = None;
                    changed = true;
                    match outcome {
                        Outcome::Ran(run) => {
                            t.note = None;
                            t.note_name.clear();
                            t.source_rev = run.rev;
                            t.run = Some(run);
                            t.rows_valid = false;
                            t.forget_texts();
                        }
                        Outcome::NoTemplate => {
                            t.run = None;
                            t.note = Some("no template fits this file".into());
                            t.note_name.clear();
                            t.tree_focus = false;
                            t.rows_valid = false;
                        }
                        Outcome::Failed(info, msg) => {
                            t.run = None;
                            t.note = Some(msg);
                            t.note_name = info.file_name.clone();
                            t.rows_valid = false;
                        }
                    }
                }
                Err(TryRecvError::Empty) => {
                    // The progress figure moves.
                    changed = true;
                }
                Err(TryRecvError::Closed) => {
                    t.job = None;
                    t.note = Some("the template stopped unexpectedly".into());
                    changed = true;
                }
            }
        }
        if t.seen_rev != rev {
            t.seen_rev = rev;
            t.changed_at = now;
        }
        // What's shown reads the bytes as they now are.
        if let Some(run) = t.run.as_mut()
            && t.source_rev != rev
            && let Ok(src) = FileSource::open(&path, overlay.unwrap_or_default())
        {
            run.interp.src = Box::new(src);
            t.source_rev = rev;
            t.texts.clear();
            t.cache.clear();
            changed = true;
        }
        let due = t.job.is_none()
            && t.run.as_ref().is_some_and(|r| r.rev != rev && r.took < AUTO_RERUN_UNDER)
            && now.duration_since(t.changed_at) >= QUIET;
        if due {
            self.run_template();
            changed = true;
        }
        changed
    }

    /// Run the template now, on this thread — for tests, which have no tick.
    #[cfg(test)]
    pub(crate) fn run_template_now(&mut self) {
        let Some((file, name, overlay, rev, _)) = self.tpl_file() else { return };
        let Some(t) = self.tpl.as_mut() else { return };
        let choice = t.choice.clone();
        let outcome = std::thread::Builder::new()
            .stack_size(library::RUN_STACK)
            .spawn(move || {
                execute(
                    choice,
                    &file,
                    &name,
                    overlay,
                    rev,
                    Arc::new(AtomicBool::new(false)),
                    Arc::new(AtomicU64::new(0)),
                )
            })
            .expect("spawn")
            .join()
            .ok();
        let (tx, rx) = oneshot::channel();
        if let Some(o) = outcome {
            let _ = tx.send(o);
        }
        t.job = Some(Job {
            rx,
            progress: Arc::new(AtomicU64::new(0)),
            cancel: Arc::new(AtomicBool::new(false)),
            len: 0,
            name: String::new(),
        });
        self.poll_template(Instant::now());
    }

    /// Whether the template panel is showing.
    pub fn template_panel(&self) -> bool {
        self.hex.is_some() && self.tpl.as_ref().is_some_and(TemplateState::has_panel)
    }

    /// Whether keys go to the template tree rather than the bytes.
    pub fn template_focus(&self) -> bool {
        self.template_panel() && self.tpl.as_ref().is_some_and(|t| t.tree_focus)
    }

    /// Whether a value in the tree is being edited (Esc must cancel at once).
    pub fn editing_template_value(&self) -> bool {
        self.template_focus() && self.tpl.as_ref().is_some_and(|t| t.edit.is_some())
    }

    /// The template part of the hex status line.
    pub fn template_status(&self) -> Option<String> {
        let t = self.tpl.as_ref()?;
        if matches!(t.choice, TemplateChoice::Off) {
            return None;
        }
        if let Some(job) = &t.job {
            let done = job.progress.load(Ordering::Relaxed);
            let pct = (done.min(job.len) * 100).checked_div(job.len).unwrap_or(0);
            let name = if job.name.is_empty() { "template".to_string() } else { job.name.clone() };
            return Some(format!("{name} {pct}%"));
        }
        if let Some(run) = &t.run {
            let rev = self.hex.as_ref().map_or(0, |h| h.rev);
            let mark = if run.error.is_some() {
                " ✗"
            } else if run.rev != rev && run.took >= AUTO_RERUN_UNDER {
                " (Shift-F5)"
            } else {
                ""
            };
            return Some(format!("{}{mark}", run.info.file_name));
        }
        match &t.note {
            Some(_) if !t.note_name.is_empty() => Some(format!("{} ✗", t.note_name)),
            Some(n) => Some(n.clone()),
            None => None,
        }
    }

    /// The F-key labels of hex mode.
    pub(super) fn hex_fkey_labels(&self) -> [&'static str; 10] {
        if !self.template_panel() {
            let mut labels = crate::ui::fkeys::HEX_LABELS;
            // These are the bytes behind a tag page: F3 goes back to it, which
            // is worth saying where the template's own F3 would have been.
            if self.tags.is_some() {
                labels[2] = "Tags";
            }
            return labels;
        }
        let mut labels = crate::ui::fkeys::HEX_TEMPLATE_LABELS;
        let t = self.tpl.as_ref().expect("panel implies state");
        if t.show_output {
            labels[2] = "Vars";
        }
        if t.tree_focus {
            labels[5] = "Hex";
        }
        if self.hint_mods.contains(KeyModifiers::SHIFT) {
            labels[4] = "Rerun";
        }
        labels
    }

    /// The styles of `n` bytes from `start` as the template coloured them, and
    /// the byte range of the selected variable.
    pub(super) fn template_byte_styles(
        &mut self,
        start: u64,
        n: usize,
        theme: &Theme,
    ) -> (Vec<Option<Style>>, Option<(u64, u64)>) {
        let mut out = vec![None; n];
        let Some(t) = self.tpl.as_mut() else { return (out, None) };
        if matches!(t.choice, TemplateChoice::Off) {
            return (out, None);
        }
        let Some(run) = t.run.as_ref() else { return (out, None) };
        for (slot, (fg, bg, style)) in out.iter_mut().zip(run.interp.tree.colors_in(start, n)) {
            *slot = crate::bt::colors::byte_style(theme, fg, bg, style);
        }
        let sel = if t.tree_focus {
            t.ensure_rows();
            let kind = t.rows.get(t.cursor).map(|r| r.kind);
            match kind {
                Some(k) => {
                    t.texts_for(&[k]);
                    t.texts.get(&k).and_then(|x| x.range)
                }
                None => None,
            }
        } else {
            None
        };
        (out, sel)
    }

    /// Keys for the templates while the bytes have the focus: `None` leaves
    /// the key to hex mode.
    pub(super) fn template_hex_key(&mut self, key: KeyEvent) -> Option<EditorSignal> {
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::F(5) if shift => {
                if let Some(t) = self.tpl.as_mut()
                    && matches!(t.choice, TemplateChoice::Off)
                {
                    t.choice = TemplateChoice::Auto;
                }
                self.run_template();
                Some(EditorSignal::Stay)
            }
            KeyCode::F(5) => Some(EditorSignal::OpenTemplatePicker),
            KeyCode::F(6) if self.template_panel() => {
                self.jump_to_template_variable();
                Some(EditorSignal::Stay)
            }
            KeyCode::F(3) if self.template_panel() => {
                if let Some(t) = self.tpl.as_mut() {
                    t.show_output = !t.show_output;
                }
                Some(EditorSignal::Stay)
            }
            _ => None,
        }
    }

    /// Select the variable under the byte cursor in the tree, opening what it
    /// is inside, and give the tree the focus.
    pub fn jump_to_template_variable(&mut self) {
        let cursor = self.hex.as_ref().map_or(0, |h| h.cursor);
        let Some(t) = self.tpl.as_mut() else { return };
        let Some(run) = t.run.as_mut() else {
            self.status = "No template results".to_string();
            return;
        };
        let path = run.interp.tree.path_at(cursor);
        t.show_output = false;
        t.tree_focus = true;
        self.insp.focus = false;
        let Some(&last) = path.last() else { return };
        let mut target = RowKind::Node(last);
        for (k, r) in path.iter().enumerate() {
            let is_last = k + 1 == path.len();
            let n = run.interp.tree.node(r.id).clone();
            // An element beyond the rows listed: list up to it.
            if let Some(next) = path.get(k + 1)
                && let NodeKind::Array { elem_size, kind: ArrayKind::Optimized, .. } = n.kind
                && elem_size > 0
            {
                let i = (next.shift - r.shift) / elem_size;
                let want = (i / CHUNK + 1) * CHUNK;
                let e = t.shown.entry(*r).or_insert(CHUNK);
                *e = (*e).max(want);
            }
            if let Some(next) = path.get(k + 1)
                && let NodeKind::Array { kind: ArrayKind::Full, .. } = n.kind
            {
                let i = run.interp.tree.node(next.id).index as u64;
                let want = (i / CHUNK + 1) * CHUNK;
                let e = t.shown.entry(*r).or_insert(CHUNK);
                *e = (*e).max(want);
            }
            if !is_last {
                t.collapsed.remove(r);
                t.expanded.insert(*r);
            } else if let NodeKind::Array { kind: ArrayKind::Scalar, elem_size, .. } = n.kind
                && elem_size > 0
            {
                let i = (cursor - (n.start + r.shift)) / elem_size;
                t.collapsed.remove(r);
                t.expanded.insert(*r);
                let want = (i / CHUNK + 1) * CHUNK;
                let e = t.shown.entry(*r).or_insert(CHUNK);
                *e = (*e).max(want);
                target = RowKind::Elem(*r, i);
            }
        }
        t.rebuild_rows();
        if let Some(i) = t.rows.iter().position(|row| row.kind == target) {
            t.cursor = i;
        }
    }

    /// Keys while the tree has the focus: `None` hands the key to hex mode
    /// (saving, searching, quitting).
    pub(super) fn template_tree_key(&mut self, key: KeyEvent) -> Option<EditorSignal> {
        if !self.template_focus() {
            return None;
        }
        if self.editing_template_value() {
            return self.template_edit_key(key);
        }
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let page =
            self.tpl.as_ref().map_or(1, |t| t.list_area.height.saturating_sub(1).max(1) as isize);
        // Back to the bytes, at the variable selected in the tree, in the
        // column they had (Tab steps on round, see `step_hex_focus`).
        if matches!(key.code, KeyCode::F(6) | KeyCode::Esc) {
            self.leave_template_tree();
            return Some(EditorSignal::Stay);
        }
        let t = self.tpl.as_mut().expect("focus implies state");
        if t.show_output {
            let lines = t.run.as_ref().map_or(0, |r| r.interp.output.len());
            match key.code {
                KeyCode::Up => t.out_scroll = t.out_scroll.saturating_sub(1),
                KeyCode::Down => t.out_scroll = (t.out_scroll + 1).min(lines.saturating_sub(1)),
                KeyCode::PageUp => t.out_scroll = t.out_scroll.saturating_sub(page as usize),
                KeyCode::PageDown => {
                    t.out_scroll = (t.out_scroll + page as usize).min(lines.saturating_sub(1))
                }
                KeyCode::Home => t.out_scroll = 0,
                KeyCode::End => t.out_scroll = lines.saturating_sub(1),
                KeyCode::F(3) => t.show_output = false,
                KeyCode::F(5) if shift => {
                    self.run_template();
                }
                KeyCode::F(5) => return Some(EditorSignal::OpenTemplatePicker),
                KeyCode::F(2) | KeyCode::F(4) | KeyCode::F(7) | KeyCode::F(10) => return None,
                _ => {}
            }
            return Some(EditorSignal::Stay);
        }
        t.ensure_rows();
        let len = t.rows.len();
        let mut moved = false;
        match key.code {
            KeyCode::Up => {
                t.cursor = t.cursor.saturating_sub(1);
                moved = true;
            }
            KeyCode::Down => {
                t.cursor = (t.cursor + 1).min(len.saturating_sub(1));
                moved = true;
            }
            KeyCode::PageUp => {
                t.cursor = t.cursor.saturating_sub(page as usize);
                moved = true;
            }
            KeyCode::PageDown => {
                t.cursor = (t.cursor + page as usize).min(len.saturating_sub(1));
                moved = true;
            }
            KeyCode::Home => {
                t.cursor = 0;
                moved = true;
            }
            KeyCode::End => {
                t.cursor = len.saturating_sub(1);
                moved = true;
            }
            KeyCode::Right | KeyCode::Char('+') => {
                if let Some(row) = t.rows.get(t.cursor).copied() {
                    match row.kind {
                        RowKind::Node(r) => {
                            let run = t.run.as_ref().expect("rows imply a run");
                            if run.interp.has_children(r) {
                                if t.is_open(&run.interp, r) {
                                    if key.code == KeyCode::Right {
                                        t.cursor = (t.cursor + 1).min(len.saturating_sub(1));
                                        moved = true;
                                    }
                                } else {
                                    t.collapsed.remove(&r);
                                    t.expanded.insert(r);
                                    t.rows_valid = false;
                                }
                            }
                        }
                        RowKind::More(r) => more(t, r),
                        RowKind::Elem(..) => {}
                    }
                }
            }
            KeyCode::Left | KeyCode::Char('-') => {
                if let Some(row) = t.rows.get(t.cursor).copied() {
                    let run = t.run.as_ref().expect("rows imply a run");
                    match row.kind {
                        RowKind::Node(r)
                            if run.interp.has_children(r) && t.is_open(&run.interp, r) =>
                        {
                            t.expanded.remove(&r);
                            t.collapsed.insert(r);
                            t.rows_valid = false;
                        }
                        _ if key.code == KeyCode::Left && row.depth > 0 => {
                            if let Some(p) =
                                t.rows[..t.cursor].iter().rposition(|x| x.depth < row.depth)
                            {
                                t.cursor = p;
                                moved = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            KeyCode::Char('*') => {
                if let Some(RowKind::Node(r)) = t.rows.get(t.cursor).map(|x| x.kind) {
                    expand_all(t, r);
                    t.rows_valid = false;
                }
            }
            KeyCode::Enter => {
                if let Some(row) = t.rows.get(t.cursor).copied() {
                    let run = t.run.as_ref().expect("rows imply a run");
                    match row.kind {
                        RowKind::Node(r) if run.interp.has_children(r) => {
                            if t.is_open(&run.interp, r) {
                                t.expanded.remove(&r);
                                t.collapsed.insert(r);
                            } else {
                                t.collapsed.remove(&r);
                                t.expanded.insert(r);
                            }
                            t.rows_valid = false;
                        }
                        RowKind::More(r) => more(t, r),
                        kind => self.begin_value_edit(kind),
                    }
                }
            }
            KeyCode::F(3) => t.show_output = true,
            KeyCode::F(5) if shift => {
                self.run_template();
            }
            KeyCode::F(5) => return Some(EditorSignal::OpenTemplatePicker),
            KeyCode::F(2) | KeyCode::F(4) | KeyCode::F(7) | KeyCode::F(10) => return None,
            _ => {}
        }
        if moved {
            self.follow_tree_cursor(false);
        }
        Some(EditorSignal::Stay)
    }

    /// Give the keys back from the tree to the bytes: the byte cursor goes to
    /// the variable selected there, unless it is already inside it.
    pub(super) fn leave_template_tree(&mut self) {
        if self.tpl.as_ref().is_some_and(|t| !t.show_output) {
            self.follow_tree_cursor(true);
        }
        if let Some(t) = self.tpl.as_mut() {
            t.tree_focus = false;
        }
    }

    /// Put the byte cursor on the selected variable's first byte — or, with
    /// `keep_inside`, leave it where it is if it is already in the variable.
    fn follow_tree_cursor(&mut self, keep_inside: bool) {
        let Some(t) = self.tpl.as_mut() else { return };
        t.ensure_rows();
        let Some(kind) = t.rows.get(t.cursor).map(|r| r.kind) else { return };
        t.texts_for(&[kind]);
        let range = t.texts.get(&kind).and_then(|x| x.range);
        if let (Some((start, size)), Some(h)) = (range, self.hex.as_mut())
            && start < h.len
            && !(keep_inside && (start..start.saturating_add(size)).contains(&h.cursor))
        {
            h.cursor = start;
            h.nibble_low = false;
        }
    }

    fn begin_value_edit(&mut self, kind: RowKind) {
        let target = match kind {
            RowKind::Node(r) => EditTarget::Node(r),
            RowKind::Elem(r, i) => EditTarget::Elem(r, i),
            RowKind::More(_) => return,
        };
        if self.hex.as_ref().is_some_and(|h| h.readonly) {
            self.status = "read-only file".to_string();
            return;
        }
        let Some(t) = self.tpl.as_mut() else { return };
        let Some(run) = t.run.as_ref() else { return };
        if !run.interp.editable(target) {
            self.status = "This value can't be edited".to_string();
            return;
        }
        t.texts_for(&[kind]);
        let text = t.texts.get(&kind).map(|x| x.value.clone()).unwrap_or_default();
        let caret = text.chars().count();
        t.edit = Some(ValueEdit { target, text, caret });
    }

    fn template_edit_key(&mut self, key: KeyEvent) -> Option<EditorSignal> {
        let t = self.tpl.as_mut()?;
        let e = t.edit.as_mut()?;
        match key.code {
            KeyCode::Esc => t.edit = None,
            KeyCode::Enter => self.commit_value_edit(),
            _ => {
                let _ = crate::ui::textedit::edit_key(&mut e.text, &mut e.caret, key);
            }
        }
        Some(EditorSignal::Stay)
    }

    /// Write the value being edited into the hex editor's pending edits.
    pub(super) fn commit_value_edit(&mut self) {
        let Some(t) = self.tpl.as_mut() else { return };
        let Some(e) = t.edit.take() else { return };
        let Some(run) = t.run.as_mut() else { return };
        let result = std::thread::scope(|s| {
            std::thread::Builder::new()
                .stack_size(DISPLAY_STACK)
                .spawn_scoped(s, || run.interp.encode_edit(e.target, &e.text))
                .ok()
                .and_then(|h| h.join().ok())
                .unwrap_or_else(|| Err("the value couldn't be written".into()))
        });
        match result {
            Ok(writes) => {
                let Some(h) = self.hex.as_mut() else { return };
                for (off, bytes) in writes {
                    if !h.set_bytes(off, &bytes) {
                        self.status = "read-only file".to_string();
                        return;
                    }
                }
                self.dirty = h.dirty;
                if let Some(t) = self.tpl.as_mut() {
                    t.texts.clear();
                    t.cache.clear();
                }
                self.poll_template(Instant::now());
            }
            Err(msg) => {
                self.status = msg;
                if let Some(t) = self.tpl.as_mut() {
                    t.edit = Some(e);
                }
            }
        }
    }

    /// Mouse events over the template panel. Returns whether the event was
    /// for it.
    pub(super) fn template_mouse(&mut self, ev: MouseEvent) -> bool {
        let area = self.tpl_area;
        let inside = ev.column >= area.x
            && ev.column < area.x + area.width
            && ev.row >= area.y
            && ev.row < area.y + area.height;
        if !self.template_panel() || !inside {
            if matches!(ev.kind, MouseEventKind::Down(MouseButton::Left))
                && let Some(t) = self.tpl.as_mut()
            {
                t.tree_focus = false;
                t.edit = None;
            }
            return false;
        }
        let Some(t) = self.tpl.as_mut() else { return false };
        match ev.kind {
            MouseEventKind::ScrollUp => {
                if t.show_output {
                    t.out_scroll = t.out_scroll.saturating_sub(3);
                } else {
                    t.cursor = t.cursor.saturating_sub(3);
                }
            }
            MouseEventKind::ScrollDown => {
                if t.show_output {
                    let lines = t.run.as_ref().map_or(0, |r| r.interp.output.len());
                    t.out_scroll = (t.out_scroll + 3).min(lines.saturating_sub(1));
                } else {
                    t.ensure_rows();
                    t.cursor = (t.cursor + 3).min(t.rows.len().saturating_sub(1));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                t.tree_focus = true;
                t.edit = None;
                let list = t.list_area;
                if !t.show_output && ev.row >= list.y && ev.row < list.y + list.height {
                    t.ensure_rows();
                    let i = t.scroll + (ev.row - list.y) as usize;
                    if let Some(row) = t.rows.get(i).copied() {
                        let was = t.cursor;
                        t.cursor = i;
                        let glyph_x = list.x + row.depth * 2;
                        if let RowKind::Node(r) = row.kind
                            && (ev.column <= glyph_x + 1 || was == i)
                            && t.run.as_ref().is_some_and(|run| run.interp.has_children(r))
                        {
                            let open = t.run.as_ref().is_some_and(|run| t.is_open(&run.interp, r));
                            if open {
                                t.expanded.remove(&r);
                                t.collapsed.insert(r);
                            } else {
                                t.collapsed.remove(&r);
                                t.expanded.insert(r);
                            }
                            t.rows_valid = false;
                        } else if let RowKind::More(r) = row.kind {
                            more(t, r);
                        }
                        self.follow_tree_cursor(false);
                    }
                }
            }
            _ => {}
        }
        true
    }
}

/// List the next batch of an array's elements.
fn more(t: &mut TemplateState, r: NodeRef) {
    let e = t.shown.entry(r).or_insert(CHUNK);
    *e += CHUNK;
    t.rows_valid = false;
}

/// Open `r` and everything under it (up to a limit), except what the template
/// asked not to be opened that way.
fn expand_all(t: &mut TemplateState, r: NodeRef) {
    let Some(run) = t.run.as_mut() else { return };
    let mut stack = vec![r];
    let mut budget = 20_000;
    while let Some(n) = stack.pop() {
        if budget == 0 {
            break;
        }
        budget -= 1;
        if n != r && run.interp.tree.node(n.id).flags & F_SUPPRESS != 0 {
            continue;
        }
        if !run.interp.has_children(n) {
            continue;
        }
        t.collapsed.remove(&n);
        t.expanded.insert(n);
        for c in child_rows(&mut run.interp, n, &t.shown) {
            if let RowKind::Node(c) = c {
                stack.push(c);
            }
        }
    }
}

/// Draw the template panel. Returns the caret position while a value is being
/// edited.
pub(super) fn render_panel(
    f: &mut Frame,
    area: Rect,
    ed: &mut EditorState,
    theme: &Theme,
) -> Option<Position> {
    let normal = Style::default().fg(theme.text_fg).bg(theme.panel_bg);
    let dim = Style::default().fg(theme.panel_border).bg(theme.panel_bg);
    let header =
        Style::default().fg(theme.header_fg).bg(theme.panel_bg).add_modifier(Modifier::BOLD);
    let error = Style::default().fg(theme.error_fg).bg(theme.panel_bg);
    let side = area.x > ed.text_area.x;
    let focus = ed.template_focus();
    let geom_start_w =
        hex::HexGeom::for_len(ed.hex.as_ref().map_or(0, |h| h.len)).off_w as usize + 2;
    let t = ed.tpl.as_mut()?;

    // A divider from the bytes: a column when beside them, a rule when below.
    let inner = if side {
        let rule: Vec<Line> =
            (0..area.height).map(|_| Line::from(Span::styled("│", dim))).collect();
        f.render_widget(Paragraph::new(rule), Rect { width: 1, ..area });
        Rect { x: area.x + 1, width: area.width.saturating_sub(1), ..area }
    } else {
        let label = t.run.as_ref().map(|r| format!(" {} ", r.info.file_name)).unwrap_or_default();
        let w = area.width as usize;
        let text = format!("──{label}{}", "─".repeat(w.saturating_sub(label.chars().count() + 2)));
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(ellipsize(&text, w), dim))),
            Rect { height: 1, ..area },
        );
        Rect { y: area.y + 1, height: area.height.saturating_sub(1), ..area }
    };
    let w = inner.width as usize;
    if inner.height == 0 || w < 8 {
        return None;
    }
    f.render_widget(Paragraph::new("").style(normal), inner);

    // Nothing ran: say what is happening or went wrong.
    let Some(run) = t.run.as_ref() else {
        let text = match (&t.job, &t.note) {
            (Some(_), _) => crate::l10n::tr("Running the template…"),
            (None, Some(n)) if !t.note_name.is_empty() => format!("{}: {n}", t.note_name),
            (None, Some(n)) => n.clone(),
            _ => String::new(),
        };
        let style = if t.note.is_some() && t.job.is_none() { error } else { normal };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(pad_right(&ellipsize(&text, w), w), style))),
            Rect { height: 1, ..inner },
        );
        return None;
    };
    let mut y = inner.y;
    let mut rows_h = inner.height;
    if let Some(err) = &run.error {
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(pad_right(&ellipsize(err, w), w), error))),
            Rect { y, height: 1, ..inner },
        );
        y += 1;
        rows_h = rows_h.saturating_sub(1);
    }
    if rows_h == 0 {
        return None;
    }

    if t.show_output {
        let title = format!("{} ({})", crate::l10n::tr("Output"), run.interp.output.len());
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(pad_right(&title, w), header))),
            Rect { y, height: 1, ..inner },
        );
        let body = Rect { y: y + 1, height: rows_h - 1, ..inner };
        t.list_area = body;
        let lines: Vec<Line> = run
            .interp
            .output
            .iter()
            .skip(t.out_scroll)
            .take(body.height as usize)
            .map(|l| Line::from(Span::styled(pad_right(&ellipsize(l, w), w), normal)))
            .collect();
        f.render_widget(Paragraph::new(lines).style(normal), body);
        return None;
    }

    // The rows in view, and their text.
    let list = Rect { y: y + 1, height: rows_h - 1, ..inner };
    t.list_area = list;
    let visible = list.height as usize;
    t.ensure_rows();
    t.scroll = crate::util::scroll::scroll_to_visible(t.scroll, t.cursor, visible.max(1));
    let kinds: Vec<(RowKind, u16)> =
        t.rows.iter().skip(t.scroll).take(visible).map(|r| (r.kind, r.depth)).collect();
    t.texts_for(&kinds.iter().map(|k| k.0).collect::<Vec<_>>());

    // Columns, dropped from the right as the panel narrows; the name column
    // fits the names in view.
    let show_start = w >= 44;
    let show_size = w >= 56;
    let show_type = w >= 76;
    let show_comment = w >= 100;
    let start_w = if show_start { geom_start_w.max(5) + 1 } else { 0 };
    let size_w = if show_size { 9 } else { 0 };
    let type_w = if show_type { 16 } else { 0 };
    let rest = w.saturating_sub(start_w + size_w + type_w);
    let widest = kinds
        .iter()
        .map(|(k, d)| *d as usize * 2 + 2 + t.texts.get(k).map_or(0, |x| x.name.chars().count()))
        .max()
        .unwrap_or(0);
    let name_w = (widest + 2).clamp(12, (rest * 55 / 100).max(12)).min(rest);
    let comment_w = if show_comment { (rest.saturating_sub(name_w)) * 40 / 100 } else { 0 };
    let value_w = rest.saturating_sub(name_w + comment_w);

    let cols =
        |name: &str, value: &str, start: &str, size: &str, ty: &str, comment: &str| -> String {
            let mut s = pad_right(&ellipsize(name, name_w.saturating_sub(1)), name_w);
            s.push_str(&pad_right(&ellipsize(value, value_w.saturating_sub(1)), value_w));
            if show_start {
                s.push_str(&pad_right(start, start_w));
            }
            if show_size {
                s.push_str(&pad_right(&ellipsize(size, size_w - 1), size_w));
            }
            if show_type {
                s.push_str(&pad_right(&ellipsize(ty, type_w - 1), type_w));
            }
            if show_comment {
                s.push_str(&ellipsize(comment, comment_w));
            }
            pad_right(&s, w)
        };
    let tr = crate::l10n::tr;
    let head =
        cols(&tr("Name"), &tr("Value"), &tr("Start"), &tr("Size"), &tr("Type"), &tr("Comment"));
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(head, header))),
        Rect { y, height: 1, ..inner },
    );

    let run = t.run.as_ref().expect("checked above");
    let selected = if focus { theme.cursor } else { theme.cursor_inactive };
    let mut lines = Vec::with_capacity(visible);
    let mut caret = None;
    for (k, (kind, depth)) in kinds.iter().enumerate() {
        let row_i = t.scroll + k;
        let text = t.texts.get(kind).cloned().unwrap_or_default();
        let glyph = match kind {
            RowKind::Node(r) if text.children => {
                if t.is_open(&run.interp, *r) {
                    "▾ "
                } else {
                    "▸ "
                }
            }
            _ => "  ",
        };
        let name = format!("{}{glyph}{}", "  ".repeat(*depth as usize), text.name);
        let editing = row_i == t.cursor && t.edit.is_some();
        let value = match (&t.edit, editing) {
            (Some(e), true) => e.text.clone(),
            _ => text.value.clone(),
        };
        let line = cols(&name, &value, &text.start, &text.size, &text.ty, &text.comment);
        let style = if row_i == t.cursor { selected } else { normal };
        if editing && let Some(e) = &t.edit {
            let caret_x = inner.x as usize + name_w + e.caret.min(value_w.saturating_sub(1));
            caret = Some(Position::new(caret_x as u16, list.y + k as u16));
            // The value being edited stands out from the rest of the row.
            let (before, after) =
                line.split_at(line.char_indices().nth(name_w).map_or(line.len(), |(i, _)| i));
            let vend = after.char_indices().nth(value_w).map_or(after.len(), |(i, _)| i);
            let (v, tail) = after.split_at(vend);
            lines.push(Line::from(vec![
                Span::styled(before.to_string(), style),
                Span::styled(
                    v.to_string(),
                    Style::default()
                        .fg(theme.text_fg)
                        .bg(theme.panel_bg)
                        .add_modifier(Modifier::UNDERLINED),
                ),
                Span::styled(tail.to_string(), style),
            ]));
        } else {
            lines.push(Line::from(Span::styled(line, style)));
        }
    }
    f.render_widget(Paragraph::new(lines).style(normal), list);
    caret
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::VfsPath;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn zip_file(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("rc_tpl_{tag}_{}.zip", std::process::id()));
        let f = std::fs::File::create(&p).unwrap();
        let mut z = zip::ZipWriter::new(f);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored);
        z.start_file("hello.txt", opts).unwrap();
        std::io::Write::write_all(&mut z, b"hello template").unwrap();
        z.finish().unwrap();
        p
    }

    fn hex_editor(p: &Path) -> EditorState {
        let mut e = EditorState::new_hex("sample.zip".into(), VfsPath::local(p)).unwrap();
        e.run_template_now();
        e
    }

    fn screen(e: &mut EditorState, w: u16, h: u16) -> (String, ratatui::buffer::Buffer) {
        let theme = crate::ui::theme::Theme::mc();
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| crate::editor::render::render(f, f.area(), e, &theme)).unwrap();
        let b = t.backend().buffer().clone();
        let mut s = String::new();
        for y in 0..b.area.height {
            for x in 0..b.area.width {
                s.push_str(b[(x, y)].symbol());
            }
            s.push('\n');
        }
        (s, b)
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn the_background_ramp_runs_on_past_the_tree_cursor_below_the_bytes() {
        // In a narrow window the tree sits below the bytes, and its cursor bar
        // runs the full width of the editor, cutting the page's background in
        // two. The part below the bar used to start the ramp over.
        use crate::ui::theme::{GradientDir, GradientSpec};
        use ratatui::style::Color;
        let p = zip_file("ramp");
        let mut e = hex_editor(&p);
        let mut spec = crate::ui::theme::active_specs().into_iter().next().unwrap();
        spec.panel_bg = Color::Rgb(0, 0, 0);
        spec.cursor_bg = Color::Rgb(200, 0, 0);
        spec.cursor_inactive_bg = Color::Rgb(200, 0, 0);
        spec.gradients = Default::default();
        spec.gradients.panel_bg = Some(GradientSpec {
            direction: GradientDir::Vertical,
            ..GradientSpec::new(Color::Rgb(255, 255, 255))
        });
        let theme = Theme::from_spec(&spec, true);
        let (w, h) = (80, 40);
        let mut t = Terminal::new(TestBackend::new(w, h)).unwrap();
        t.draw(|f| {
            crate::ui::gradient::reset();
            crate::editor::render::render(f, f.area(), &mut e, &theme);
            let area = f.area();
            crate::ui::gradient::apply(f, area, &theme);
        })
        .unwrap();
        assert!(e.tpl_area.y > e.text_area.y, "the tree sits below the bytes");
        let tpl = e.tpl.as_ref().unwrap();
        let bar = tpl.list_area.y + (tpl.cursor - tpl.scroll) as u16;
        let b = t.backend().buffer();
        assert_eq!(b[(w - 1, bar)].bg, Color::Rgb(200, 0, 0), "the cursor bar cuts across");
        let shade = |y| match b[(w - 1, y)].bg {
            Color::Rgb(r, _, _) => r,
            c => panic!("row {y} is not ramped: {c:?}"),
        };
        let (above, below) = (shade(bar - 1), shade(bar + 1));
        assert!(below > above, "the ramp runs on below the bar: {above} above, {below} below");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_zip_gets_the_zip_template_with_a_tree_and_coloured_bytes() {
        let p = zip_file("auto");
        let mut e = hex_editor(&p);
        assert_eq!(e.active_template().map(|i| i.file_name), Some("ZIP.bt".to_string()));
        assert!(e.template_panel());
        let (s, b) = screen(&mut e, 160, 30);
        assert!(s.contains("record"), "the tree lists the file record:\n{s}");
        assert!(s.contains("Name") && s.contains("Value"), "column header");
        assert!(s.contains("ZIP.bt"), "the status line names the template");
        assert!(s.contains("Templt") && s.contains("Tree"), "template keys on the bar");
        // The record's bytes carry the template's colour: the first byte cell
        // differs from an uncoloured gap cell past the file's end.
        let theme = crate::ui::theme::Theme::mc();
        let first = b[(10, 1)].bg;
        assert_ne!(first, theme.panel_bg, "template colours tint the bytes");
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn f6_jumps_to_the_variable_and_the_tree_moves_the_byte_cursor() {
        let p = zip_file("jump");
        let mut e = hex_editor(&p);
        e.hex.as_mut().unwrap().cursor = 8; // inside the first record's header
        let _ = screen(&mut e, 160, 30);
        assert!(matches!(e.handle_key(key(KeyCode::F(6))), EditorSignal::Stay));
        assert!(e.template_focus());
        let t = e.tpl.as_mut().unwrap();
        t.ensure_rows();
        let kind = t.rows[t.cursor].kind;
        t.texts_for(&[kind]);
        let text = t.texts[&kind].clone();
        let (start, size) = text.range.unwrap();
        assert!(
            start <= 8 && 8 < start + size,
            "the selected variable covers the cursor: {text:?}"
        );
        assert!(text.name.starts_with("fr"), "a field of the file record: {}", text.name);
        // Moving down the tree puts the byte cursor on the next variable.
        e.handle_key(key(KeyCode::Down));
        assert!(e.hex.as_ref().unwrap().cursor >= start + size);
        // Tab goes on to the hex column.
        e.handle_key(key(KeyCode::Tab));
        assert!(!e.template_focus());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn tab_steps_round_the_hex_and_ascii_columns_and_the_tree() {
        let p = zip_file("tab");
        let mut e = hex_editor(&p);
        e.hex.as_mut().unwrap().cursor = 9; // inside frCompression, at offset 8
        let _ = screen(&mut e, 160, 30);
        let at = |e: &EditorState| (e.hex.as_ref().unwrap().ascii_pane, e.template_focus());
        let back = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        let selected = |e: &mut EditorState| {
            let t = e.tpl.as_mut().unwrap();
            t.ensure_rows();
            let kind = t.rows[t.cursor].kind;
            t.texts_for(&[kind]);
            t.texts[&kind].clone()
        };
        assert_eq!(at(&e), (false, false));
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), (true, false), "hex → ASCII");
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), (true, true), "ASCII → tree");
        // Into the tree Tab goes as F6 does: to the variable at the cursor.
        let text = selected(&mut e);
        assert_eq!(text.name, "frCompression");
        assert_eq!(text.range.map(|r| r.0), Some(8));
        let (s, _) = screen(&mut e, 160, 30);
        assert!(s.contains("pane:TEMPLATE"), "the status line names the tree:\n{s}");
        // Back out, the cursor stays put when it is already in the variable…
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), (false, false), "tree → hex");
        assert_eq!(e.hex.as_ref().unwrap().cursor, 9);
        e.handle_key(back);
        assert_eq!(at(&e), (false, true), "hex ← tree");
        // …and otherwise goes to the variable selected in the tree.
        let t = e.tpl.as_mut().unwrap();
        t.cursor += 1;
        let (start, _) = selected(&mut e).range.unwrap();
        assert!(start > 9);
        e.handle_key(back);
        assert_eq!(at(&e), (true, false), "tree ← ASCII");
        assert_eq!(e.hex.as_ref().unwrap().cursor, start);
        e.handle_key(back);
        assert_eq!(at(&e), (false, false), "ASCII ← hex");
        // F6 back to the bytes does the same, keeping the column.
        e.handle_key(key(KeyCode::F(6)));
        let t = e.tpl.as_mut().unwrap();
        t.cursor += 1;
        let (next, _) = selected(&mut e).range.unwrap();
        e.handle_key(key(KeyCode::F(6)));
        assert_eq!(at(&e), (false, false));
        assert_eq!(e.hex.as_ref().unwrap().cursor, next);
        // Without a template Tab only swaps the columns.
        e.tpl.as_mut().unwrap().choice = TemplateChoice::Off;
        e.handle_key(key(KeyCode::Tab));
        e.handle_key(key(KeyCode::Tab));
        assert_eq!(at(&e), (false, false));
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn editing_a_value_writes_its_bytes_into_the_pending_edits() {
        let p = zip_file("edit");
        let mut e = hex_editor(&p);
        e.hex.as_mut().unwrap().cursor = 8; // frCompression, at offset 8
        let _ = screen(&mut e, 160, 30);
        e.handle_key(key(KeyCode::F(6)));
        e.handle_key(key(KeyCode::Enter));
        assert!(e.editing_template_value());
        // Clear the field and type the new value.
        for _ in 0..40 {
            e.handle_key(key(KeyCode::Backspace));
        }
        for c in "COMP_DEFLATE".chars() {
            e.handle_key(key(KeyCode::Char(c)));
        }
        e.handle_key(key(KeyCode::Enter));
        assert!(!e.editing_template_value(), "status: {}", e.status);
        let h = e.hex.as_mut().unwrap();
        assert_eq!(h.window(8, 2), vec![8, 0], "COMP_DEFLATE is 8, little-endian");
        assert!(e.dirty);
        // Esc cancels an edit without writing.
        e.handle_key(key(KeyCode::Enter));
        e.handle_key(key(KeyCode::Char('9')));
        e.handle_key(key(KeyCode::Esc));
        assert!(!e.editing_template_value());
        assert_eq!(e.hex.as_mut().unwrap().window(8, 2), vec![8, 0]);
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn f5_asks_for_the_picker_and_no_template_hides_the_panel() {
        let p = zip_file("pick");
        let mut e = hex_editor(&p);
        assert!(matches!(e.handle_key(key(KeyCode::F(5))), EditorSignal::OpenTemplatePicker));
        e.set_template(None);
        assert!(!e.template_panel());
        let (s, _) = screen(&mut e, 160, 30);
        assert!(!s.contains("record"));
        // Shift-F5 brings automatic selection back.
        e.handle_key(KeyEvent::new(KeyCode::F(5), KeyModifiers::SHIFT));
        e.run_template_now();
        assert!(e.template_panel());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn a_click_in_the_panel_selects_a_row_and_its_glyph_opens_it() {
        let p = zip_file("click");
        let mut e = hex_editor(&p);
        let _ = screen(&mut e, 160, 30);
        let list = e.tpl.as_ref().unwrap().list_area;
        let at = |row: u16, col: u16| MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: col,
            row,
            modifiers: KeyModifiers::NONE,
        };
        // The first row is the file record, closed; a click on its glyph opens it.
        let rows_before = e.tpl.as_ref().unwrap().rows.len();
        assert!(matches!(e.handle_mouse(at(list.y, list.x)), EditorSignal::Stay));
        assert!(e.template_focus());
        let _ = screen(&mut e, 160, 30);
        let t = e.tpl.as_mut().unwrap();
        t.ensure_rows();
        assert!(t.rows.len() > rows_before, "the record opened");
        // A click on its second child selects it and moves the byte cursor.
        e.handle_mouse(at(list.y + 2, list.x + 20));
        assert_eq!(e.tpl.as_ref().unwrap().cursor, 2);
        assert!(e.hex.as_ref().unwrap().cursor > 0);
        // A click on the bytes gives them the keys back.
        e.handle_mouse(at(3, 12));
        assert!(!e.template_focus());
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn help_shows_in_hex_mode_and_a_narrow_screen_stacks_the_panel() {
        let p = zip_file("help");
        let mut e = hex_editor(&p);
        e.handle_key(key(KeyCode::F(1)));
        let (s, _) = screen(&mut e, 90, 40);
        assert!(s.contains("Editor shortcuts"), "F1 help overlay in hex mode");
        e.handle_key(key(KeyCode::Esc));
        let (s, _) = screen(&mut e, 90, 40);
        assert!(e.tpl_area.y > e.text_area.y, "the panel sits below the bytes");
        assert!(s.contains("00000000") && s.contains("record"));
        std::fs::remove_file(&p).ok();
    }

    /// Print the hex editor on `$BT_VIEW` (a file) at `$BT_SIZE` (`WxH`), after
    /// `$BT_KEYS` (F6, Down, Enter, …). A look at the real thing:
    /// `BT_VIEW=file cargo test --bin rc editor::template::tests::preview -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn preview() {
        let file = PathBuf::from(std::env::var("BT_VIEW").expect("BT_VIEW"));
        let (w, h) = std::env::var("BT_SIZE")
            .ok()
            .and_then(|s| {
                s.split_once('x').and_then(|(a, b)| Some((a.parse().ok()?, b.parse().ok()?)))
            })
            .unwrap_or((160, 40));
        let name = file.file_name().unwrap().to_string_lossy().into_owned();
        let mut e = EditorState::new_hex(name, VfsPath::local(&file)).unwrap();
        e.run_template_now();
        let _ = screen(&mut e, w, h);
        for k in std::env::var("BT_KEYS").unwrap_or_default().split(',').filter(|k| !k.is_empty()) {
            let code = match k.trim() {
                "Down" => KeyCode::Down,
                "Up" => KeyCode::Up,
                "Right" => KeyCode::Right,
                "Left" => KeyCode::Left,
                "Enter" => KeyCode::Enter,
                "PgDn" => KeyCode::PageDown,
                "Tab" => KeyCode::Tab,
                other if other.starts_with('F') => KeyCode::F(other[1..].parse().unwrap()),
                other => KeyCode::Char(other.chars().next().unwrap()),
            };
            e.handle_key(key(code));
            let _ = screen(&mut e, w, h);
        }
        let (s, _) = screen(&mut e, w, h);
        println!("{s}");
    }
}
