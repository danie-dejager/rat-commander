//! Binary mode's lists as the viewer drives them: which tab is up, the
//! highlighted row of each, a filter that narrows them all, and whether symbol
//! names are shown demangled.

use super::{Binary, Symbol, Tab, demangle, hex};
use crate::viewer::search::Needle;
use ratatui::layout::Rect;
use std::borrow::Cow;

const TABS: usize = Tab::ALL.len();

pub struct BinaryView {
    pub bin: Box<Binary>,
    pub tab: Tab,
    /// Per tab, the highlighted row and the first row on screen — both counted
    /// in the rows the filter lets through, not in the whole list.
    sel: [usize; TABS],
    top: [usize; TABS],
    filter: Option<Filter>,
    /// Show symbol names as written in the source (F8 switches to the raw,
    /// mangled form).
    pub demangle: bool,
    /// Horizontal scroll of each list's last, open-ended column.
    pub h_offset: usize,
    /// Rows of list on screen, recorded by the renderer so a page is a page.
    pub(crate) page: usize,
    /// Where the renderer drew the list and the tab strip, for the mouse.
    pub(crate) list_area: Rect,
    pub(crate) strip_row: u16,
    pub(crate) tab_hits: Vec<(u16, u16, Tab)>,
}

/// "Find all": every list narrowed to the rows that match.
struct Filter {
    term: String,
    /// Per tab, the indices into the full list of the rows that matched.
    rows: [Vec<u32>; TABS],
}

impl BinaryView {
    pub fn new(bin: Box<Binary>) -> Self {
        BinaryView {
            bin,
            tab: Tab::Info,
            sel: [0; TABS],
            top: [0; TABS],
            filter: None,
            demangle: true,
            h_offset: 0,
            page: 1,
            list_area: Rect::default(),
            strip_row: 0,
            tab_hits: Vec::new(),
        }
    }

    /// Rows of `tab` that are showing: all of them, or those the filter kept.
    pub fn len(&self, tab: Tab) -> usize {
        match &self.filter {
            Some(f) => f.rows[tab.index()].len(),
            None => self.bin.len(tab),
        }
    }

    /// The full-list index of showing row `i` of `tab`.
    pub fn row(&self, tab: Tab, i: usize) -> Option<usize> {
        match &self.filter {
            Some(f) => f.rows[tab.index()].get(i).map(|&r| r as usize),
            None => (i < self.bin.len(tab)).then_some(i),
        }
    }

    pub fn selected(&self) -> usize {
        self.sel[self.tab.index()]
    }

    pub fn top(&self) -> usize {
        self.top[self.tab.index()]
    }

    pub fn set_tab(&mut self, tab: Tab) {
        if tab != self.tab {
            self.tab = tab;
            self.h_offset = 0;
        }
    }

    /// Tab / Shift-Tab: the next or previous tab, wrapping.
    pub fn cycle_tab(&mut self, delta: isize) {
        let i = (self.tab.index() as isize + delta).rem_euclid(TABS as isize) as usize;
        self.set_tab(Tab::ALL[i]);
    }

    pub fn move_by(&mut self, delta: isize) {
        let i = (self.selected() as isize).saturating_add(delta).max(0) as usize;
        self.select(i);
    }

    /// Highlight row `i`, clamped to the list.
    pub fn select(&mut self, i: usize) {
        let last = self.len(self.tab).saturating_sub(1);
        self.sel[self.tab.index()] = i.min(last);
    }

    /// Keep the highlighted row within `rows` rows of screen; returns the first
    /// row to draw.
    pub(crate) fn scroll_into_view(&mut self, rows: usize) -> usize {
        let t = self.tab.index();
        let len = self.len(self.tab);
        self.sel[t] = self.sel[t].min(len.saturating_sub(1));
        let top = crate::util::scroll::scroll_to_visible(self.top[t], self.sel[t], rows);
        self.top[t] = top.min(len.saturating_sub(rows.max(1)));
        self.top[t]
    }

    /// Scroll the list by `delta` rows without moving the highlight off screen —
    /// what the mouse wheel does.
    pub fn scroll_by(&mut self, delta: isize) {
        let t = self.tab.index();
        let rows = self.page.max(1);
        let max_top = self.len(self.tab).saturating_sub(rows);
        self.top[t] = ((self.top[t] as isize).saturating_add(delta).max(0) as usize).min(max_top);
        let (top, sel) = (self.top[t], self.sel[t]);
        self.sel[t] = sel.clamp(top, top + rows - 1).min(self.len(self.tab).saturating_sub(1));
    }

    /// The file offset of the highlighted row, when it has one — what Enter
    /// opens the hex view at.
    pub fn selected_offset(&self) -> Option<u64> {
        let i = self.row(self.tab, self.selected())?;
        let b = &self.bin;
        match self.tab {
            Tab::Info | Tab::Libraries | Tab::Imports => None,
            Tab::Sections => b.sections[i].offset,
            Tab::Exports => b.exports[i].offset,
            Tab::Functions => b.functions[i].offset,
            Tab::Strings => Some(b.strings[i].offset),
        }
    }

    /// A symbol's name as it is to be shown. A function known only from an
    /// unwind table has none, and is called `sub_` and its address, the way
    /// disassemblers name one.
    pub fn name<'a>(&self, s: &'a Symbol) -> Cow<'a, str> {
        if s.name.is_empty() {
            return Cow::Owned(format!("sub_{:x}", s.address));
        }
        if self.demangle
            && let Some(d) = demangle(&s.name)
        {
            return Cow::Owned(d);
        }
        Cow::Borrowed(&s.name)
    }

    pub fn filter_term(&self) -> Option<&str> {
        self.filter.as_ref().map(|f| f.term.as_str())
    }

    /// Narrow every list to the rows matching `needle`, starting each at its top.
    pub fn set_filter(&mut self, term: &str, needle: &Needle) {
        let rows = Tab::ALL.map(|tab| {
            (0..self.bin.len(tab))
                .filter(|&i| needle.find(self.haystack(tab, i).as_bytes(), 0).is_some())
                .map(|i| i as u32)
                .collect()
        });
        self.filter = Some(Filter { term: term.to_string(), rows });
        self.sel = [0; TABS];
        self.top = [0; TABS];
    }

    /// Drop the filter, keeping the highlight on the row it was on. Returns
    /// whether there was one.
    pub fn clear_filter(&mut self) -> bool {
        let Some(filter) = self.filter.take() else { return false };
        for (t, rows) in filter.rows.iter().enumerate() {
            self.sel[t] = rows.get(self.sel[t]).map_or(0, |&r| r as usize);
            self.top[t] = self.sel[t].saturating_sub(self.page / 2);
        }
        true
    }

    /// Move the highlight to the next showing row of this tab that matches —
    /// or the previous one — wrapping round. Returns whether one did.
    pub fn find(&mut self, needle: &Needle, backwards: bool) -> bool {
        let len = self.len(self.tab);
        let from = self.selected();
        for step in 1..=len {
            let i = if backwards { (from + len - step % len) % len } else { (from + step) % len };
            let Some(idx) = self.row(self.tab, i) else { continue };
            if needle.find(self.haystack(self.tab, idx).as_bytes(), 0).is_some() {
                self.select(i);
                return true;
            }
        }
        false
    }

    /// What a search looks through for full-list row `i` of `tab`: the text the
    /// row shows, and for a symbol both spellings of its name and its address.
    fn haystack(&self, tab: Tab, i: usize) -> String {
        let b = &self.bin;
        let symbol = |s: &Symbol| {
            let shown = self.name(s);
            let raw = if shown == s.name { "" } else { s.name.as_str() };
            format!("{} {shown} {raw} {}", hex(s.address, b.is_64), s.library)
        };
        match tab {
            Tab::Info => {
                let f = &b.facts[i];
                let value = if f.translate { crate::l10n::tr(&f.value) } else { f.value.clone() };
                format!("{}: {value}", crate::l10n::tr(f.label))
            }
            Tab::Sections => {
                let s = &b.sections[i];
                format!("{} {} {}", s.name, s.kind, hex(s.address, b.is_64))
            }
            Tab::Libraries => format!("{} {}", b.libraries[i].name, b.libraries[i].note),
            Tab::Imports => symbol(&b.imports[i]),
            Tab::Exports => symbol(&b.exports[i]),
            Tab::Functions => symbol(&b.functions[i]),
            Tab::Strings => b.strings[i].text.clone(),
        }
    }
}
