//! The generic pulldown menu bar: a row of titles, each opening a dropdown of
//! items (optionally with one level of submenu).
//!
//! Both menus in the program are built on this: the file manager's F9 bar
//! ([`crate::ui::menu`]) and the editor's ([`crate::editor::menu`]). The action
//! type is the caller's own enum — it only has to name a separator variant, via
//! [`Action`].

use crate::ui::theme::Theme;
use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

/// What a menu's action enum must provide: a variant standing for a separator
/// row, which is drawn as a rule and can never be selected or activated.
pub trait Action: Copy {
    fn separator() -> Self;
    fn is_separator(self) -> bool;
}

pub struct MenuItem<A> {
    /// The item's label in the active language (translated when built).
    pub label: String,
    /// Optional keyboard-shortcut hint, drawn right-aligned in the dropdown
    /// (e.g. `"F3"`, `"Shift-F6"`). Empty for items without a shortcut.
    pub shortcut: &'static str,
    pub action: A,
    /// When false the item is greyed out and cannot be selected or activated
    /// (e.g. "Go local" while the panel is already on a local directory).
    pub enabled: bool,
    /// Nested items opened as a submenu beside the dropdown. Empty for a normal
    /// (leaf) item; a non-empty submenu makes the item open it rather than
    /// activate its own action.
    pub submenu: Vec<MenuItem<A>>,
}

impl<A: Action> MenuItem<A> {
    /// Grey this item out so it can't be navigated to or activated. Used for
    /// context-dependent items that don't apply in the current state.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.enabled = !disabled;
        self
    }

    /// Whether the item can be selected/activated: not a separator, and enabled.
    pub fn selectable(&self) -> bool {
        self.enabled && !self.action.is_separator()
    }

    /// Whether opening this item reveals a submenu rather than running an action.
    fn has_sub(&self) -> bool {
        !self.submenu.is_empty()
    }

    /// The lower-cased accelerator key for this item, if its label marks one
    /// with `&` (e.g. `"&Copy"` → `'c'`, `"Select &group"` → `'g'`).
    pub fn hotkey(&self) -> Option<char> {
        let (display, idx) = split_hotkey(&self.label);
        idx.and_then(|i| display.chars().nth(i)).map(|c| c.to_ascii_lowercase())
    }
}

pub struct Menu<A> {
    pub items: Vec<MenuItem<A>>,
}

/// Result of a key press routed to the menu.
pub enum MenuSignal<A> {
    Stay,
    Close,
    Activate(A),
}

/// One menu bar: its titles, their dropdowns, and where the highlight sits.
pub struct PulldownState<A> {
    /// The top-bar titles, already translated. The accelerator is each title's
    /// first letter.
    pub(crate) titles: Vec<String>,
    pub(crate) menus: Vec<Menu<A>>,
    pub(crate) active: usize,
    pub(crate) item: usize,
    /// Whether the highlighted item's submenu is open (only items with a
    /// non-empty `submenu` can open one).
    pub(crate) sub_open: bool,
    /// Highlighted row inside the open submenu.
    pub(crate) sub_item: usize,
    /// Screen rect of each top-bar title, recorded at render time.
    title_rects: Vec<Rect>,
    /// Screen rect of each dropdown item (with its item index).
    item_rects: Vec<(usize, Rect)>,
    /// Screen rect of each open-submenu row (with its index), for click routing.
    sub_rects: Vec<(usize, Rect)>,
}

impl<A: Action> PulldownState<A> {
    /// A bar over `menus`, labelled by `titles` (one per menu), opened on the
    /// `active`th menu. (Named `build` rather than `new` so each concrete menu
    /// can keep its own `new` constructor on the same type.)
    pub fn build(titles: Vec<String>, menus: Vec<Menu<A>>, active: usize) -> Self {
        let active = active.min(menus.len().saturating_sub(1));
        let mut s = PulldownState {
            titles,
            menus,
            active,
            item: 0,
            sub_open: false,
            sub_item: 0,
            title_rects: Vec::new(),
            item_rects: Vec::new(),
            sub_rects: Vec::new(),
        };
        s.item = s.first_selectable(0, 1);
        s
    }

    /// Open the highlighted item's submenu, landing on its first selectable row.
    /// No-op when the item has no submenu.
    pub(crate) fn open_sub(&mut self) {
        let Some(it) = self.menus[self.active].items.get(self.item) else {
            return;
        };
        if !it.has_sub() {
            return;
        }
        self.sub_item = first_sel(&it.submenu, 0, 1);
        self.sub_open = true;
    }

    /// The open submenu's items, or `None` when no submenu is showing.
    fn sub_items(&self) -> Option<&[MenuItem<A>]> {
        if !self.sub_open {
            return None;
        }
        let it = self.menus[self.active].items.get(self.item)?;
        it.has_sub().then_some(it.submenu.as_slice())
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> MenuSignal<A> {
        // An open submenu captures navigation first; Esc/← step back to its parent
        // rather than closing the whole bar.
        if self.sub_open {
            return self.handle_sub_key(key);
        }
        match key.code {
            KeyCode::Esc | KeyCode::F(9) | KeyCode::F(10) => MenuSignal::Close,
            KeyCode::Left => {
                self.active = (self.active + self.menus.len() - 1) % self.menus.len();
                self.item = self.first_selectable(0, 1);
                MenuSignal::Stay
            }
            // → opens the highlighted item's submenu when it has one, else moves
            // on to the next top menu.
            KeyCode::Right => {
                if self.menus[self.active].items[self.item].has_sub() {
                    self.open_sub();
                } else {
                    self.active = (self.active + 1) % self.menus.len();
                    self.item = self.first_selectable(0, 1);
                }
                MenuSignal::Stay
            }
            KeyCode::Up => {
                self.item = self.next_selectable(self.item, -1);
                MenuSignal::Stay
            }
            KeyCode::Down => {
                self.item = self.next_selectable(self.item, 1);
                MenuSignal::Stay
            }
            KeyCode::Enter => {
                let it = &self.menus[self.active].items[self.item];
                if !it.selectable() {
                    MenuSignal::Stay
                } else if it.has_sub() {
                    self.open_sub();
                    MenuSignal::Stay
                } else {
                    MenuSignal::Activate(it.action)
                }
            }
            KeyCode::Char(c) => self.activate_hotkey(c),
            _ => MenuSignal::Stay,
        }
    }

    /// Keys while a submenu is open. Esc/← close just the submenu; F9/F10 close
    /// the whole bar; letters match the submenu's own accelerators.
    fn handle_sub_key(&mut self, key: KeyEvent) -> MenuSignal<A> {
        if self.sub_items().is_none() {
            // Defensive: the submenu vanished (shouldn't happen) — drop the flag.
            self.sub_open = false;
            return MenuSignal::Stay;
        }
        // Resolve the move against a borrow, then apply it — `sub_items()` borrows
        // `self`, so nothing may be assigned while it is held.
        let (next, signal) = {
            let items = self.sub_items().expect("checked above");
            match key.code {
                KeyCode::F(9) | KeyCode::F(10) => return MenuSignal::Close,
                KeyCode::Esc | KeyCode::Left => {
                    self.sub_open = false;
                    return MenuSignal::Stay;
                }
                KeyCode::Up => (next_sel(items, self.sub_item, -1), MenuSignal::Stay),
                KeyCode::Down => (next_sel(items, self.sub_item, 1), MenuSignal::Stay),
                KeyCode::Enter => match items.get(self.sub_item) {
                    Some(it) if it.selectable() => (self.sub_item, MenuSignal::Activate(it.action)),
                    _ => return MenuSignal::Stay,
                },
                KeyCode::Char(c) => {
                    let lc = c.to_ascii_lowercase();
                    match items.iter().position(|it| it.selectable() && it.hotkey() == Some(lc)) {
                        Some(idx) => (idx, MenuSignal::Activate(items[idx].action)),
                        // An unclaimed letter does nothing while a submenu is open
                        // — it must not fall through and switch top menus behind it.
                        None => return MenuSignal::Stay,
                    }
                }
                _ => return MenuSignal::Stay,
            }
        };
        self.sub_item = next;
        signal
    }

    /// Handle a typed letter: an accelerator in the open dropdown activates that
    /// item; otherwise a top-bar letter switches to that menu.
    fn activate_hotkey(&mut self, c: char) -> MenuSignal<A> {
        let lc = c.to_ascii_lowercase();
        if let Some(idx) = self.menus[self.active]
            .items
            .iter()
            .position(|it| it.selectable() && it.hotkey() == Some(lc))
        {
            self.item = idx;
            // A parent's accelerator reveals its submenu instead of acting.
            if self.menus[self.active].items[idx].has_sub() {
                self.open_sub();
                return MenuSignal::Stay;
            }
            return MenuSignal::Activate(self.menus[self.active].items[idx].action);
        }
        // Top-bar titles are accelerated by their first letter. Two titles can
        // share one (the editor's File and Format do), so the search starts just
        // past the open menu and wraps: repeating the letter steps between them.
        let n = self.titles.len();
        let first_letter = |t: &str| t.chars().next().map(|x| x.to_ascii_lowercase());
        if let Some(off) =
            (1..=n).find(|k| first_letter(&self.titles[(self.active + k) % n]) == Some(lc))
        {
            self.active = (self.active + off) % n;
            self.item = self.first_selectable(0, 1);
            return MenuSignal::Stay;
        }
        MenuSignal::Stay
    }

    /// The bar's menus, for tests and for builders that need to find an item.
    pub fn menus(&self) -> &[Menu<A>] {
        &self.menus
    }

    /// First selectable item at or after `start`, scanning by `dir`.
    fn first_selectable(&self, start: usize, dir: isize) -> usize {
        first_sel(&self.menus[self.active].items, start, dir)
    }

    fn next_selectable(&self, from: usize, dir: isize) -> usize {
        next_sel(&self.menus[self.active].items, from, dir)
    }

    /// Route a left-click to the menu (titles switch/open; items activate;
    /// anything else closes).
    pub fn click(&mut self, area: Rect, col: u16, row: u16) -> MenuSignal<A> {
        let hit =
            |r: &Rect| col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height;
        // A click on a top-bar title switches to that menu (closing any submenu).
        if let Some(i) = title_index_at(&self.titles, area, col, row) {
            self.active = i;
            self.sub_open = false;
            self.item = self.first_selectable(0, 1);
            return MenuSignal::Stay;
        }
        // An open submenu's rows take precedence: they overlay the dropdown.
        if self.sub_open {
            let picked = self.sub_rects.iter().find(|(_, r)| hit(r)).map(|(i, _)| *i);
            if let Some(idx) = picked {
                let action = self
                    .sub_items()
                    .and_then(|items| items.get(idx))
                    .filter(|it| it.selectable())
                    .map(|it| it.action);
                self.sub_item = idx;
                return match action {
                    Some(a) => MenuSignal::Activate(a),
                    None => MenuSignal::Stay,
                };
            }
        }
        // A click on a dropdown item activates it (or opens its submenu).
        for (idx, rect) in &self.item_rects {
            if hit(rect) {
                let idx = *idx;
                let it = &self.menus[self.active].items[idx];
                if !it.selectable() {
                    return MenuSignal::Stay;
                }
                let (action, has_sub) = (it.action, it.has_sub());
                self.item = idx;
                if has_sub {
                    self.open_sub();
                    return MenuSignal::Stay;
                }
                return MenuSignal::Activate(action);
            }
        }
        MenuSignal::Close
    }

    pub fn render(&mut self, f: &mut Frame, area: Rect, theme: &Theme) {
        self.title_rects.clear();
        self.item_rects.clear();
        self.sub_rects.clear();
        // Where each title sits on the bar, for the dropdown and for clicks.
        let rtl = crate::l10n::active_is_rtl();
        let mut title_x = vec![];
        let mut x = area.x + 1;
        for title in &self.titles {
            let w = format!(" {} ", crate::l10n::display(title)).chars().count() as u16;
            title_x.push(x);
            self.title_rects.push(Rect { x, y: area.y, width: w, height: 1 });
            x += w;
        }
        // Only the highlighted title is painted: the full-width bar underneath
        // (drawn by `menubar::render_titles`, gradient and all) already carries
        // every title, and repainting the rest in the bar's flat color would
        // break its ramp — the gradient pass cannot restore it, because the bar
        // ramps itself and so is left alone.
        // The title's first letter (after the leading space) is its hotkey
        // (skipped in RTL, where the reshaped title reads right-to-left).
        let hk = if rtl { None } else { Some(1) };
        let style =
            Style::default().bg(theme.dialog_bg).fg(theme.dialog_fg).add_modifier(Modifier::BOLD);
        let text = format!(" {} ", crate::l10n::display(&self.titles[self.active]));
        f.render_widget(
            Paragraph::new(label_spans(&text, hk, style, theme)),
            self.title_rects[self.active],
        );

        // Dropdown under the active title.
        let items = &self.menus[self.active].items;
        let width = menu_width(items);
        let height = items.len() as u16 + 2;
        let dx = title_x[self.active].min(area.x + area.width.saturating_sub(width));
        let rect = Rect {
            x: dx,
            y: area.y + 1,
            width: width.min(area.width),
            height: height.min(area.height.saturating_sub(1)),
        };
        let inner = draw_menu_box(f, rect, theme);
        // While a submenu is open the parent row keeps its highlight, so the path
        // through the menu stays visible.
        self.item_rects = draw_items(f, inner, items, self.item, rtl, theme);

        // The submenu, anchored beside its parent row.
        if let Some(sub) = self.sub_items() {
            let sw = menu_width(sub);
            let sh = (sub.len() as u16 + 2).min(area.height.saturating_sub(1));
            // Prefer the right of the parent dropdown; flip to its left when that
            // would run off the screen edge.
            let right = rect.x + rect.width;
            let sx = if right + sw <= area.x + area.width {
                right
            } else {
                rect.x.saturating_sub(sw).max(area.x)
            };
            // Align the box so its first row meets the parent item, then pull it
            // back inside the screen if it would overhang the bottom.
            let parent_y = inner.y + self.item as u16;
            let max_y = (area.y + area.height).saturating_sub(sh);
            let sy = parent_y.saturating_sub(1).min(max_y).max(area.y + 1);
            let srect = Rect { x: sx, y: sy, width: sw.min(area.width), height: sh };
            let sinner = draw_menu_box(f, srect, theme);
            self.sub_rects = draw_items(f, sinner, sub, self.sub_item, rtl, theme);
        }
    }
}

/// The top-bar title index at screen column `col` on the menu-bar row, or
/// `None`. Mirrors the title layout used by [`PulldownState::render`] (and
/// `menubar::render`) so it works even before the bar has been drawn — i.e. to
/// open the menu on a click.
pub fn title_index_at(titles: &[String], area: Rect, col: u16, row: u16) -> Option<usize> {
    if row != area.y {
        return None;
    }
    let mut x = area.x + 1;
    for (i, title) in titles.iter().enumerate() {
        let w = title.chars().count() as u16 + 2; // " {title} "
        if col >= x && col < x + w {
            return Some(i);
        }
        x += w;
    }
    None
}

/// Interior width for a dropdown holding `items`: the longest label (plus its
/// right-aligned shortcut) with padding for the border and margins.
fn menu_width<A>(items: &[MenuItem<A>]) -> u16 {
    items
        .iter()
        .map(|it| {
            let disp = it.label.chars().filter(|&c| c != '&').count();
            if it.shortcut.is_empty() {
                disp
            } else {
                // label + a 2-space gap + the right-aligned shortcut
                disp + 2 + it.shortcut.chars().count()
            }
        })
        .max()
        .unwrap_or(8) as u16
        + 4
}

/// Clear `rect`, draw the menu border, and return the interior.
fn draw_menu_box(f: &mut Frame, rect: Rect, theme: &Theme) -> Rect {
    f.render_widget(Clear, rect);
    let block = Block::default()
        .borders(Borders::ALL)
        .style(Style::default().fg(theme.menu_fg).bg(theme.menu_bg));
    let inner = block.inner(rect);
    f.render_widget(block, rect);
    inner
}

/// Draw `items` into `inner` with row `sel` highlighted, returning the
/// `(index, rect)` of every drawn row for click hit-testing.
fn draw_items<A: Action>(
    f: &mut Frame,
    inner: Rect,
    items: &[MenuItem<A>],
    sel: usize,
    rtl: bool,
    theme: &Theme,
) -> Vec<(usize, Rect)> {
    let mut rects = Vec::new();
    let mut lines: Vec<Line> = Vec::with_capacity(items.len());
    for (i, it) in items.iter().enumerate() {
        let row_y = inner.y + i as u16;
        if it.action.is_separator() {
            lines.push(Line::from(Span::styled(
                "─".repeat(inner.width as usize),
                Style::default().fg(theme.panel_border).bg(theme.menu_bg),
            )));
            continue;
        }
        if row_y < inner.y + inner.height {
            rects.push((i, Rect { x: inner.x, y: row_y, width: inner.width, height: 1 }));
        }
        let style = if !it.enabled {
            // Greyed out: dimmed foreground, never the selection highlight
            // (navigation skips disabled items so `i` never lands here).
            Style::default().fg(theme.panel_border).bg(theme.menu_bg)
        } else if i == sel {
            theme.menu_selection
        } else {
            Style::default().fg(theme.menu_fg).bg(theme.menu_bg)
        };
        let (display, hk) = split_hotkey(&it.label);
        // Reshape RTL text for display; in that case the hotkey accent can't
        // line up with the reversed text, so it is dropped (the key still works).
        let display = crate::l10n::display(&display);
        // Disabled items don't accent their accelerator (it's inactive).
        let hk = if rtl || !it.enabled { None } else { hk.map(|i| i + 1) };
        let iw = inner.width as usize;
        let mut text = format!(" {display}");
        // Right-align the shortcut hint (one trailing space from the edge),
        // then pad the row out to the full interior width.
        if !it.shortcut.is_empty() {
            let sc_start = iw.saturating_sub(it.shortcut.chars().count() + 1);
            while text.chars().count() < sc_start {
                text.push(' ');
            }
            text.push_str(it.shortcut);
        }
        while text.chars().count() < iw {
            text.push(' ');
        }
        // The hotkey sits one column right of its index (the leading space).
        lines.push(label_spans(&text, hk, style, theme));
    }
    f.render_widget(Paragraph::new(lines).style(Style::default().bg(theme.menu_bg)), inner);
    rects
}

/// A menu item whose label is translated and may mark an `&` accelerator.
pub fn item<A: Action>(label: &str, action: A) -> MenuItem<A> {
    MenuItem {
        label: crate::l10n::tr(label),
        shortcut: "",
        action,
        enabled: true,
        submenu: Vec::new(),
    }
}

/// A menu item whose label is used verbatim (no translation, no `&` hotkey) —
/// for runtime text like a remote-connection label.
pub fn item_raw<A: Action>(label: String, action: A) -> MenuItem<A> {
    MenuItem { label, shortcut: "", action, enabled: true, submenu: Vec::new() }
}

/// A menu item with a right-aligned keyboard-shortcut hint.
pub fn item_key<A: Action>(label: &str, shortcut: &'static str, action: A) -> MenuItem<A> {
    MenuItem { label: crate::l10n::tr(label), shortcut, action, enabled: true, submenu: Vec::new() }
}

/// A parent item that opens `submenu` (with a right-aligned shortcut hint).
pub fn item_sub<A: Action>(
    label: &str,
    shortcut: &'static str,
    action: A,
    submenu: Vec<MenuItem<A>>,
) -> MenuItem<A> {
    MenuItem { label: crate::l10n::tr(label), shortcut, action, enabled: true, submenu }
}

/// A separator rule between groups of items.
pub fn sep<A: Action>() -> MenuItem<A> {
    MenuItem {
        label: String::new(),
        shortcut: "",
        action: A::separator(),
        enabled: true,
        submenu: Vec::new(),
    }
}

/// First selectable entry of `items` at or after `start`, scanning by `dir`.
/// Falls back to `start` when nothing is selectable.
pub(crate) fn first_sel<A: Action>(items: &[MenuItem<A>], start: usize, dir: isize) -> usize {
    if items.is_empty() {
        return 0;
    }
    let mut i = start.min(items.len() - 1);
    for _ in 0..items.len() {
        if items[i].selectable() {
            return i;
        }
        i = (i as isize + dir).rem_euclid(items.len() as isize) as usize;
    }
    start
}

/// Next selectable entry of `items` from `from`, wrapping, scanning by `dir`.
fn next_sel<A: Action>(items: &[MenuItem<A>], from: usize, dir: isize) -> usize {
    if items.is_empty() {
        return 0;
    }
    let n = items.len() as isize;
    let mut i = (from as isize + dir).rem_euclid(n);
    for _ in 0..items.len() {
        if items[i as usize].selectable() {
            return i as usize;
        }
        i = (i + dir).rem_euclid(n);
    }
    from
}

/// Strip the `&` accelerator marker from `label`, returning the display text and
/// the char index (within that text) of the highlighted hotkey, if any.
pub(crate) fn split_hotkey(label: &str) -> (String, Option<usize>) {
    match label.find('&') {
        Some(byte_pos) => {
            let idx = label[..byte_pos].chars().count();
            let display: String = label.chars().filter(|&c| c != '&').collect();
            (display, Some(idx))
        }
        None => (label.to_string(), None),
    }
}

/// Render `text` with the char at `pos` painted in the hotkey accent color.
fn label_spans(text: &str, pos: Option<usize>, base: Style, theme: &Theme) -> Line<'static> {
    let chars: Vec<char> = text.chars().collect();
    match pos {
        Some(p) if p < chars.len() => {
            let hot = base.fg(theme.hotkey_fg).add_modifier(Modifier::BOLD);
            Line::from(vec![
                Span::styled(chars[..p].iter().collect::<String>(), base),
                Span::styled(chars[p].to_string(), hot),
                Span::styled(chars[p + 1..].iter().collect::<String>(), base),
            ])
        }
        _ => Line::from(Span::styled(text.to_string(), base)),
    }
}
