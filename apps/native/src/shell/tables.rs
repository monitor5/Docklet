//! The tables page: the tab strip, the filter, the list and its checkboxes,
//! row buttons, context menu and the selection bar.

use std::fmt::Write as _;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{ClientToScreen, SelectObject, SetTextColor};
use windows::Win32::UI::Controls::{
    CDDS_ITEMPREPAINT, CDDS_PREPAINT, CDDS_SUBITEM, CDRF_DODEFAULT, CDRF_NEWFONT,
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW, CDRF_SKIPDEFAULT, HDN_ITEMSTATEICONCLICK,
    LIST_VIEW_ITEM_STATE_FLAGS, LVFI_PARTIAL, LVFI_STRING, LVIF_INDENT, LVIF_STATE, LVIF_TEXT,
    LVIS_STATEIMAGEMASK, LVITEMW, LVM_ENSUREVISIBLE, LVN_COLUMNCLICK, LVN_GETDISPINFOW,
    LVN_ITEMACTIVATE, LVN_ODFINDITEMW, NM_CLICK, NM_CUSTOMDRAW, NMCUSTOMDRAW,
    NMCUSTOMDRAW_DRAW_STAGE, NMHDR, NMITEMACTIVATE, NMLISTVIEW, NMLVCUSTOMDRAW, NMLVDISPINFOW,
    NMLVFINDITEMW, TCN_SELCHANGE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_DELETE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DLGC_WANTALLKEYS, DestroyMenu, MF_GRAYED, MF_SEPARATOR,
    MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_CHAR, WM_GETDLGCODE, WM_KEYDOWN,
    WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_NCDESTROY, WM_NOTIFY,
};
use windows::core::PCWSTR;

use super::{Shell, ids};
use crate::boundary::{ActionKind, RowFlags, SortBy, Table, UiEvent};
use crate::host::ctx;
use crate::widgets::selection_bar::summary;
use crate::widgets::table::TableView;
use crate::widgets::table_state::{Entry, EntryKey, TableState};
use crate::win::{
    control_down, copy_to_clipboard, is_visible, point_of, shift_down, to_client, wide,
};

/// Writes UTF-16 into the buffer the list hands over, truncating, without
/// allocating.
struct TextSink<'a> {
    buffer: &'a mut [u16],
    length: usize,
}

impl std::fmt::Write for TextSink<'_> {
    fn write_str(&mut self, text: &str) -> std::fmt::Result {
        let room = self.buffer.len().saturating_sub(1);
        for unit in text.encode_utf16() {
            if self.length >= room {
                break;
            }
            self.buffer[self.length] = unit;
            self.length += 1;
        }
        Ok(())
    }
}

impl Drop for TextSink<'_> {
    fn drop(&mut self) {
        if let Some(end) = self.buffer.get_mut(self.length) {
            *end = 0;
        }
    }
}

const MENU_COPY: usize = 200;
const MENU_REMOVE_CHECKED: usize = 201;
const MENU_CLEAR_CHECKED: usize = 202;
const MENU_TOGGLE_GROUP: usize = 203;
const MENU_CHECK_GROUP: usize = 204;

impl Shell {
    fn ensure_state(&self, tab: usize) {
        let mut states = self.states.borrow_mut();
        if states.len() <= tab {
            states.resize_with(tab + 1, TableState::default);
        }
    }

    /// What line `index` of the current tab shows, in terms that survive
    /// new data.
    fn key_at(&self, index: usize) -> Option<EntryKey> {
        let states = self.states.borrow();
        states.get(self.tab.get())?.key_at(index).cloned()
    }

    /// The line that shows `key` now.
    fn index_of_key(&self, key: &EntryKey) -> Option<usize> {
        self.states.borrow().get(self.tab.get())?.index_of(key)
    }

    pub(super) fn checked_count(&self) -> usize {
        self.states
            .borrow()
            .get(self.tab.get())
            .map_or(0, TableState::checked_count)
    }

    /// Runs `read` on the current table and its state, if both exist and
    /// the state was built from a table of this shape. A state a step behind
    /// its table is never used to index it.
    fn with_table<T>(&self, read: impl FnOnce(&Table, &TableState) -> T) -> Option<T> {
        let model = ctx().model.borrow();
        let states = self.states.borrow();
        let tab = self.tab.get();
        let (table, state) = (model.tables.get(tab)?, states.get(tab)?);
        state.fits(table).then(|| read(table, state))
    }

    /// Changes the current table's state, on the same terms as `with_table`.
    fn change_table<T>(&self, change: impl FnOnce(&Table, &mut TableState) -> T) -> Option<T> {
        let tab = self.tab.get();
        let model = ctx().model.borrow();
        let mut states = self.states.borrow_mut();
        let (table, state) = (model.tables.get(tab)?, states.get_mut(tab)?);
        state.fits(table).then(|| change(table, state))
    }

    pub(crate) fn tabs_changed(&self) {
        let titles = ctx().model.borrow().tabs.clone();
        self.tabs.set_titles(&titles);
        let tab = self.tab.get().min(titles.len().saturating_sub(1));
        self.tab.set(tab);
        self.tabs.select(tab);
        crate::win::invalidate(self.tabs.hwnd);
        self.rebuild_table();
        self.apply_visibility();
        self.layout();
    }

    pub(crate) fn table_changed(&self, tab: usize) {
        if tab == self.tab.get() {
            self.rebuild_table();
        }
    }

    /// Recomputes the current tab's lines from the model and shows them,
    /// keeping the focused line and the checks of rows that are still there.
    pub(super) fn rebuild_table(&self) {
        let ctx = ctx();
        let tab = self.tab.get();
        self.ensure_state(tab);
        let focus = self.table.focused().and_then(|index| self.key_at(index));
        let model = ctx.model.borrow();
        let empty = Table::default();
        let table = model.tables.get(tab).unwrap_or(&empty);
        let (changed, count, header, sort, checked, matching) = {
            let mut states = self.states.borrow_mut();
            let state = &mut states[tab];
            let changed = state.rebuild(table);
            (
                changed,
                state.entries().len(),
                state.header_state(table),
                state.sort(),
                state.checked_count(),
                state.matching(),
            )
        };
        // Sending to the list comes back as notifications that read the
        // model and the state, so neither is borrowed mutably from here on.
        let dpi = self.dpi.get();
        self.table.build_columns(table, dpi, self.fonts().ui);
        self.table.set_count(count);
        // After the count: a vertical scroll bar may have come or gone.
        self.table.fit_columns(dpi);
        match focus.and_then(|key| self.index_of_key(&key)) {
            Some(index) => self.table.focus_item(index, false),
            None if count > 0 && self.table.focused().is_none_or(|focused| focused >= count) => {
                self.table.focus_item(0, false);
            }
            None => {}
        }
        self.table.update_header(header, sort);
        let text = if table.columns.is_empty() {
            "Loading…".to_string()
        } else if table.rows.is_empty() && !table.empty_text.is_empty() {
            table.empty_text.clone()
        } else {
            summary(checked, matching, table.rows.len(), &table.noun)
        };
        self.bar.set(&text);
        let ids = changed.then(|| self.states.borrow()[tab].checked_ids(table));
        drop(model);
        self.sync_bar();
        if let Some(ids) = ids {
            ctx.emit(UiEvent::SelectionChanged { tab, ids });
        }
    }

    /// Shows the Remove and Clear buttons exactly while rows are checked.
    fn sync_bar(&self) {
        let checked = self.checked_count() > 0;
        if checked != is_visible(self.bar.remove) && is_visible(self.bar.summary) {
            self.apply_visibility();
            self.layout();
        }
    }

    pub(super) fn select_tab(&self, tab: usize) {
        let ctx = ctx();
        if self.log_open.get() {
            ctx.model.borrow_mut().log = None;
            self.close_log();
        }
        self.tab.set(tab);
        self.ensure_state(tab);
        let filter = self.states.borrow()[tab].filter().to_string();
        self.quiet_filter.set(true);
        self.filter.set(&filter);
        self.quiet_filter.set(false);
        self.rebuild_table();
        crate::win::send(self.table.list, LVM_ENSUREVISIBLE, 0, 0);
        self.apply_visibility();
        self.layout();
        ctx.emit(UiEvent::TabChanged { tab });
    }

    pub(super) fn filter_changed(&self) {
        if self.quiet_filter.get() {
            return;
        }
        let text = self.filter.text();
        let tab = self.tab.get();
        self.ensure_state(tab);
        self.states.borrow_mut()[tab].set_filter(&text);
        self.rebuild_table();
        ctx().emit(UiEvent::FilterChanged { tab, text });
    }

    /// After checks changed: repaint, header checkbox, bar, event.
    fn checks_changed(&self) {
        let tab = self.tab.get();
        let Some((ids, header, sort, checked, matching, total, noun)) =
            self.with_table(|table, state| {
                (
                    state.checked_ids(table),
                    state.header_state(table),
                    state.sort(),
                    state.checked_count(),
                    state.matching(),
                    table.rows.len(),
                    table.noun.clone(),
                )
            })
        else {
            return;
        };
        self.table.redraw_all();
        self.table.update_header(header, sort);
        self.bar.set(&summary(checked, matching, total, &noun));
        self.sync_bar();
        ctx().emit(UiEvent::SelectionChanged { tab, ids });
    }

    fn toggle_at(&self, index: usize, range: bool) {
        let changed = self.change_table(|table, state| {
            if range {
                state.check_range(table, index)
            } else {
                state.toggle(table, index)
            }
        });
        if changed == Some(true) {
            self.checks_changed();
            self.table.announce_state(index);
        }
    }

    fn toggle_all(&self) {
        if self.change_table(|table, state| state.toggle_all(table)) == Some(true) {
            self.checks_changed();
        }
    }

    pub(super) fn set_all_checked_off(&self) {
        if self.change_table(|_, state| state.clear()) == Some(true) {
            self.checks_changed();
        }
    }

    pub(crate) fn clear_selection(&self, tab: usize) {
        if tab == self.tab.get() {
            self.set_all_checked_off();
        } else if let Some(state) = self.states.borrow_mut().get_mut(tab) {
            state.clear();
        }
    }

    pub(super) fn remove_checked(&self) {
        let ids = self
            .with_table(|table, state| state.checked_ids(table))
            .unwrap_or_default();
        if !ids.is_empty() {
            ctx().emit(UiEvent::RemoveRequested {
                tab: self.tab.get(),
                ids,
            });
        }
    }

    /// Delete: the checked rows, or else the focused row if it can be removed.
    fn remove_key(&self) {
        if self.checked_count() > 0 {
            self.remove_checked();
            return;
        }
        let Some(index) = self.table.focused() else {
            return;
        };
        let action = self
            .with_table(|table, state| {
                let Some(Entry::Row(row)) = state.entry(index) else {
                    return None;
                };
                table
                    .actions
                    .iter()
                    .position(|action| action.kind == ActionKind::Remove)
                    .filter(|&action| table.rows[row].action_enabled(action))
            })
            .flatten();
        if let Some(action) = action {
            self.run_action(index, action);
        }
    }

    /// Action `action` of the table on line `index`, if the row allows it.
    fn run_action(&self, index: usize, action: usize) {
        let tab = self.tab.get();
        let event = self
            .with_table(|table, state| {
                let Some(Entry::Row(row)) = state.entry(index) else {
                    return None;
                };
                let row = &table.rows[row];
                let spec = table.actions.get(action)?;
                if !row.action_enabled(action) {
                    return None;
                }
                Some(match spec.kind {
                    ActionKind::Remove => UiEvent::RemoveRequested {
                        tab,
                        ids: vec![row.id.clone()],
                    },
                    ActionKind::Plain => UiEvent::RowAction {
                        tab,
                        id: row.id.clone(),
                        action: spec.key.clone(),
                    },
                })
            })
            .flatten();
        if let Some(event) = event {
            ctx().emit(event);
        }
    }

    /// Enter or a double click: a group opens or closes, a row runs its
    /// default action.
    fn activate(&self, index: usize) {
        let target = self.with_table(|table, state| match state.entry(index) {
            Some(Entry::Group(group)) => Some(Err((group, !state.is_collapsed(group)))),
            Some(Entry::Row(_)) => table.default_action.map(Ok),
            None => None,
        });
        match target.flatten() {
            Some(Ok(action)) => self.run_action(index, action),
            Some(Err((group, collapse))) => self.set_collapsed(group, collapse),
            None => {}
        }
    }

    fn set_collapsed(&self, group: u32, collapsed: bool) {
        let tab = self.tab.get();
        self.ensure_state(tab);
        self.states.borrow_mut()[tab].set_collapsed(group, collapsed);
        self.rebuild_table();
        let index = self.states.borrow()[tab].index_of_group(group);
        if let Some(index) = index {
            self.table.focus_item(index, true);
        }
    }

    fn sort_column(&self, column: usize) {
        let sortable = self
            .with_table(|table, _| {
                table
                    .columns
                    .get(column)
                    .is_some_and(|column| column.sort != SortBy::Unsorted)
            })
            .unwrap_or(false);
        if !sortable {
            return;
        }
        let tab = self.tab.get();
        let sort = self.states.borrow_mut()[tab].sort_by(column);
        self.rebuild_table();
        ctx().emit(UiEvent::SortChanged {
            tab,
            column,
            ascending: sort.ascending,
        });
    }

    fn copy_row(&self, index: usize) {
        let text = self
            .with_table(|table, state| match state.entry(index) {
                Some(Entry::Row(row)) => Some(table.rows[row].cells.join("\t")),
                Some(Entry::Group(group)) => table.groups.get(group as usize).cloned(),
                None => None,
            })
            .flatten();
        if let Some(text) = text {
            let units: Vec<u16> = text.encode_utf16().collect();
            copy_to_clipboard(self.hwnd, &units);
        }
    }

    pub(super) fn on_notify(&self, lparam: LPARAM) -> LRESULT {
        // SAFETY: for WM_NOTIFY `lparam` points to an NMHDR that begins the
        // notification's own structure, which the code names.
        let header = unsafe { &*(lparam.0 as *const NMHDR) };
        match (header.idFrom, header.code) {
            (ids::TABS, TCN_SELCHANGE) => self.select_tab(self.tabs.selected()),
            (ids::LIST, LVN_GETDISPINFOW) => {
                // SAFETY: as above.
                let info = unsafe { &mut *(lparam.0 as *mut NMLVDISPINFOW) };
                self.fill_item(&mut info.item);
            }
            (ids::LIST, NM_CUSTOMDRAW) => {
                // SAFETY: as above.
                let draw = unsafe { &mut *(lparam.0 as *mut NMLVCUSTOMDRAW) };
                return LRESULT(self.custom_draw(draw) as isize);
            }
            (ids::LIST, NM_CLICK) => {
                // SAFETY: as above.
                let click = unsafe { &*(lparam.0 as *const NMITEMACTIVATE) };
                self.list_click(click.ptAction);
            }
            (ids::LIST, LVN_ITEMACTIVATE) => {
                // SAFETY: as above.
                let item = unsafe { &*(lparam.0 as *const NMITEMACTIVATE) };
                if let Ok(index) = usize::try_from(item.iItem) {
                    self.activate(index);
                }
            }
            (ids::LIST, LVN_COLUMNCLICK) => {
                // SAFETY: as above.
                let list = unsafe { &*(lparam.0 as *const NMLISTVIEW) };
                if let Ok(column) = usize::try_from(list.iSubItem) {
                    self.sort_column(column);
                }
            }
            (ids::LIST, LVN_ODFINDITEMW) => {
                // SAFETY: as above.
                let find = unsafe { &*(lparam.0 as *const NMLVFINDITEMW) };
                return LRESULT(self.find_item(find));
            }
            _ => {}
        }
        LRESULT(0)
    }

    /// Answers the virtual list's question "what is in this cell".
    fn fill_item(&self, item: &mut LVITEMW) {
        let Ok(index) = usize::try_from(item.iItem) else {
            return;
        };
        let column = item.iSubItem.max(0) as usize;
        let indent = self.dpi.get().px(18);
        let actions_column = self.table.actions_column();
        self.with_table(|table, state| {
            let Some(entry) = state.entry(index) else {
                return;
            };
            if item.mask.contains(LVIF_TEXT) && item.cchTextMax > 0 && !item.pszText.is_null() {
                // SAFETY: the list provides a buffer of `cchTextMax` units.
                let buffer = unsafe {
                    std::slice::from_raw_parts_mut(item.pszText.0, item.cchTextMax as usize)
                };
                let mut sink = TextSink { buffer, length: 0 };
                match entry {
                    Entry::Row(row) if Some(column) == actions_column => {
                        // Read aloud as the row's last cell; the context
                        // menu is the way to reach them from the keyboard.
                        let row = &table.rows[row];
                        let mut first = true;
                        for (action, spec) in table.actions.iter().enumerate() {
                            if row.action_enabled(action) {
                                let _ =
                                    write!(sink, "{}{}", if first { "" } else { ", " }, spec.label);
                                first = false;
                            }
                        }
                    }
                    Entry::Row(row) => {
                        let text = table.rows[row].cells.get(column).map_or("", String::as_str);
                        let _ = sink.write_str(text);
                    }
                    Entry::Group(group) if column == 0 => {
                        let arrow = if state.is_collapsed(group) {
                            '▸'
                        } else {
                            '▾'
                        };
                        let title = table.groups.get(group as usize).map_or("", String::as_str);
                        let _ = write!(sink, "{arrow} {title} ({})", state.group_size(group));
                    }
                    Entry::Group(_) => {}
                }
            }
            if item.mask.contains(LVIF_STATE) {
                let check = TableView::check_state(state.is_checked(table, index));
                item.state =
                    LIST_VIEW_ITEM_STATE_FLAGS((item.state.0 & !LVIS_STATEIMAGEMASK.0) | check);
                item.stateMask =
                    LIST_VIEW_ITEM_STATE_FLAGS(item.stateMask.0 | LVIS_STATEIMAGEMASK.0);
            }
            if item.mask.contains(LVIF_INDENT) {
                // In units of the 1-pixel small image.
                item.iIndent = match entry {
                    Entry::Row(row) if table.rows[row].group.is_some() => indent,
                    _ => 0,
                };
            }
        });
    }

    /// Colours the cells and paints the row buttons.
    fn custom_draw(&self, draw: &mut NMLVCUSTOMDRAW) -> u32 {
        let stage = draw.nmcd.dwDrawStage;
        if stage == CDDS_PREPAINT {
            return CDRF_NOTIFYITEMDRAW;
        }
        if stage == CDDS_ITEMPREPAINT {
            return CDRF_NOTIFYSUBITEMDRAW;
        }
        if stage != NMCUSTOMDRAW_DRAW_STAGE(CDDS_ITEMPREPAINT.0 | CDDS_SUBITEM.0) {
            return CDRF_DODEFAULT;
        }
        let index = draw.nmcd.dwItemSpec;
        let column = draw.iSubItem.max(0) as usize;
        let hdc = draw.nmcd.hdc;
        let theme = self.theme();
        let fonts = self.fonts();
        let dpi = self.dpi.get();
        let actions_column = self.table.actions_column();
        self.with_table(|table, state| {
            let entry = state.entry(index)?;
            let row = match entry {
                Entry::Group(_) => {
                    // SAFETY: `hdc` is the list's DC for this notification; it
                    // keeps the font for the rest of the cell's drawing.
                    unsafe { SelectObject(hdc, fonts.bold.into()) };
                    draw.clrText = theme.text;
                    return Some(if Some(column) == actions_column {
                        CDRF_SKIPDEFAULT
                    } else {
                        CDRF_NEWFONT
                    });
                }
                Entry::Row(row) => &table.rows[row],
            };
            // SAFETY: as above.
            unsafe { SelectObject(hdc, fonts.ui.into()) };
            if Some(column) == actions_column {
                let labels: Vec<(&str, bool)> = table
                    .actions
                    .iter()
                    .enumerate()
                    .map(|(action, spec)| (spec.label.as_str(), row.action_enabled(action)))
                    .collect();
                let cell = self.table.cell_rect(index, column);
                self.table.paint_buttons(hdc, cell, &labels, &theme, dpi);
                return Some(CDRF_SKIPDEFAULT);
            }
            let status = table
                .columns
                .get(column)
                .is_some_and(|column| column.status);
            draw.clrText =
                if row.flags.contains(RowFlags::MUTED) || row.flags.contains(RowFlags::BUSY) {
                    theme.muted
                } else if status && row.flags.contains(RowFlags::ACTIVE) {
                    theme.good
                } else if status {
                    theme.muted
                } else {
                    theme.text
                };
            Some(CDRF_NEWFONT)
        })
        .flatten()
        .unwrap_or(CDRF_DODEFAULT)
    }

    fn list_click(&self, point: POINT) {
        let Some(hit) = self.table.hit_test(point) else {
            return;
        };
        let Some(actions_column) = self.table.actions_column() else {
            return;
        };
        if hit.column != actions_column {
            return;
        }
        let count = self.with_table(|table, _| table.actions.len()).unwrap_or(0);
        let cell = self.table.cell_rect(hit.item, actions_column);
        if let Some(action) = self.table.buttons().hit(cell, count, point.x, point.y) {
            self.run_action(hit.item, action);
        }
    }

    fn find_item(&self, find: &NMLVFINDITEMW) -> isize {
        if find.lvfi.flags.0 & (LVFI_STRING.0 | LVFI_PARTIAL.0) == 0 || find.lvfi.psz.is_null() {
            return -1;
        }
        // SAFETY: with LVFI_STRING, `psz` is a NUL-terminated string.
        let Ok(prefix) = (unsafe { find.lvfi.psz.to_string() }) else {
            return -1;
        };
        let start = find.iStart.max(0) as usize;
        self.with_table(|table, state| state.find_prefix(table, &prefix, start))
            .flatten()
            .map_or(-1, |index| index as isize)
    }

    /// A click on a checkbox, or with Shift or Ctrl: checks instead of
    /// focusing. Returns whether the click was used.
    fn list_mouse(&self, point: POINT) -> bool {
        let Some(hit) = self.table.hit_test(point) else {
            return false;
        };
        let shift = shift_down();
        if !(hit.on_checkbox || shift || control_down()) {
            return false;
        }
        // SAFETY: the list is a child of the main window.
        unsafe {
            let _ = SetFocus(Some(self.table.list));
        }
        self.table.focus_item(hit.item, false);
        self.toggle_at(hit.item, shift);
        true
    }

    /// Keys the list does not know: Space, Ctrl+A, Ctrl+C, Delete, and Left
    /// and Right on a group. Returns whether the key was used.
    fn list_key(&self, key: u16) -> bool {
        let focused = self.table.focused();
        let control = control_down();
        if key == VK_SPACE.0 {
            match focused {
                Some(index) => self.toggle_at(index, shift_down()),
                None => self.table.focus_item(0, true),
            }
            return true;
        }
        if control && key == u16::from(b'A') {
            self.toggle_all();
            return true;
        }
        if control && key == u16::from(b'C') {
            if let Some(index) = focused {
                self.copy_row(index);
            }
            return true;
        }
        if key == VK_DELETE.0 {
            self.remove_key();
            return true;
        }
        if key == VK_LEFT.0 || key == VK_RIGHT.0 {
            let group = focused.and_then(|index| {
                self.with_table(|_, state| match state.entry(index) {
                    Some(Entry::Group(group)) => Some(group),
                    _ => None,
                })
                .flatten()
            });
            if let Some(group) = group {
                self.set_collapsed(group, key == VK_LEFT.0);
                return true;
            }
        }
        false
    }

    /// The row's context menu: its actions (the keyboard's way to the row
    /// buttons), Copy, and the selection's Remove and Clear.
    pub(super) fn list_context_menu(&self, lparam: LPARAM) {
        // From the keyboard the point is (-1, -1).
        let keyboard = point_of(lparam) == POINT { x: -1, y: -1 };
        let (index, mut point) = if keyboard {
            let Some(index) = self.table.focused() else {
                return;
            };
            let rect = self.table.item_rect(index);
            (
                index,
                POINT {
                    x: rect.left + self.dpi.get().px(24),
                    y: rect.bottom,
                },
            )
        } else {
            let screen = point_of(lparam);
            let Some(hit) = self.table.hit_test(to_client(self.table.list, screen)) else {
                return;
            };
            self.table.focus_item(hit.item, false);
            (hit.item, to_client(self.table.list, screen))
        };
        // SAFETY: converting a point of the list to screen coordinates.
        unsafe {
            let _ = ClientToScreen(self.table.list, &mut point);
        }
        let key = self.key_at(index);
        let checked = self.checked_count();
        struct Item {
            id: usize,
            label: String,
            enabled: bool,
        }
        let Some(items) = self
            .with_table(|table, state| {
                let mut items = Vec::new();
                match state.entry(index) {
                    Some(Entry::Row(row)) => {
                        let row = &table.rows[row];
                        for (action, spec) in table.actions.iter().enumerate() {
                            items.push(Item {
                                id: 1 + action,
                                label: spec.label.clone(),
                                enabled: row.action_enabled(action),
                            });
                        }
                    }
                    Some(Entry::Group(group)) => {
                        let collapsed = state.is_collapsed(group);
                        items.push(Item {
                            id: MENU_TOGGLE_GROUP,
                            label: if collapsed { "Expand" } else { "Collapse" }.into(),
                            enabled: true,
                        });
                        items.push(Item {
                            id: MENU_CHECK_GROUP,
                            label: if state.is_checked(table, index) {
                                "Uncheck group"
                            } else {
                                "Check group"
                            }
                            .into(),
                            enabled: true,
                        });
                    }
                    None => return None,
                }
                items.push(Item {
                    id: 0,
                    label: String::new(),
                    enabled: false,
                });
                items.push(Item {
                    id: MENU_COPY,
                    label: "Copy\tCtrl+C".into(),
                    enabled: true,
                });
                if checked > 0 {
                    items.push(Item {
                        id: MENU_REMOVE_CHECKED,
                        label: format!("Remove {checked} selected\tDel"),
                        enabled: true,
                    });
                    items.push(Item {
                        id: MENU_CLEAR_CHECKED,
                        label: "Clear selection\tEsc".into(),
                        enabled: true,
                    });
                }
                Some(items)
            })
            .flatten()
        else {
            return;
        };
        // SAFETY: the menu is created, shown and destroyed here; the labels
        // outlive the calls that copy them.
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            for item in &items {
                if item.id == 0 {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                    continue;
                }
                let label = wide(&item.label);
                let flags = if item.enabled {
                    MF_STRING
                } else {
                    MF_STRING | MF_GRAYED
                };
                let _ = AppendMenuW(menu, flags, item.id, PCWSTR(label.as_ptr()));
            }
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            chosen.0 as usize
        };
        // The menu's loop dispatches updates, so the line may have moved or
        // gone while it was open; find it again by id.
        let Some(index) = key.and_then(|key| self.index_of_key(&key)) else {
            return;
        };
        match chosen {
            0 => {}
            MENU_COPY => self.copy_row(index),
            MENU_REMOVE_CHECKED => self.remove_checked(),
            MENU_CLEAR_CHECKED => self.set_all_checked_off(),
            MENU_TOGGLE_GROUP => self.activate(index),
            MENU_CHECK_GROUP => self.toggle_at(index, false),
            action => self.run_action(index, action - 1),
        }
    }

    /// Header text colour: the dark header theme keeps black text.
    fn header_draw(&self, draw: &NMCUSTOMDRAW) -> Option<LRESULT> {
        if !self.theme().dark {
            return None;
        }
        if draw.dwDrawStage == CDDS_PREPAINT {
            return Some(LRESULT(CDRF_NOTIFYITEMDRAW as isize));
        }
        if draw.dwDrawStage == CDDS_ITEMPREPAINT {
            // SAFETY: `hdc` is the header's paint DC.
            unsafe {
                SetTextColor(draw.hdc, self.theme().text);
            }
            return Some(LRESULT(CDRF_DODEFAULT as isize));
        }
        None
    }
}

/// The list's own messages: checkboxes by mouse and keyboard, the header's
/// select-all checkbox, and the keys of [`Shell::list_key`].
pub(super) unsafe extern "system" fn list_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the shell that owns this control and outlives it.
    let shell = unsafe { &*(data as *const Shell) };
    match message {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK if shell.list_mouse(point_of(lparam)) => {
            return LRESULT(0);
        }
        WM_KEYDOWN if shell.list_key(wparam.0 as u16) => return LRESULT(0),
        // The characters of keys handled above: Space, Ctrl+A and Ctrl+C
        // would otherwise start a type-to-find search or beep.
        WM_CHAR if matches!(wparam.0, 0x20 | 0x01 | 0x03) => return LRESULT(0),
        WM_GETDLGCODE => {
            // SAFETY: forwarding the message as it was received.
            let code = unsafe { DefSubclassProc(hwnd, message, wparam, lparam) };
            // Enter goes to the list (row activation), not to the dialog
            // manager's default button.
            if wparam.0 as u16 == VK_RETURN.0 {
                return LRESULT(code.0 | DLGC_WANTALLKEYS as isize);
            }
            return code;
        }
        WM_NOTIFY => {
            // SAFETY: for WM_NOTIFY `lparam` points to an NMHDR.
            let header = unsafe { &*(lparam.0 as *const NMHDR) };
            if header.hwndFrom == shell.table.header {
                if header.code == HDN_ITEMSTATEICONCLICK {
                    shell.toggle_all();
                    return LRESULT(0);
                }
                if header.code == NM_CUSTOMDRAW {
                    // SAFETY: the code says which structure this is.
                    let draw = unsafe { &*(lparam.0 as *const NMCUSTOMDRAW) };
                    if let Some(result) = shell.header_draw(draw) {
                        return result;
                    }
                }
            }
        }
        WM_NCDESTROY => {
            // SAFETY: removing this very subclass before forwarding.
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(list_proc), id);
            }
        }
        _ => {}
    }
    // SAFETY: forwarding the message as it was received.
    unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
}
