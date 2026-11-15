//! The tables page: the tab strip, the filter, the list and its checkboxes,
//! row buttons, context menu, the selection bar and the line to type above
//! a table.
//!
//! The page keeps no state of the lists. Which rows show, in which order and
//! which are ticked comes with every [`Table`]; a click on a box, a header or
//! a group is reported as an event, and the answer is the next table.

use std::fmt::Write as _;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
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
    AppendMenuW, CreatePopupMenu, DLGC_WANTALLKEYS, DestroyMenu, GetWindowRect, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_CHAR,
    WM_GETDLGCODE, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_NCDESTROY, WM_NOTIFY,
};
use windows::core::PCWSTR;

use super::{Shell, ids};
use crate::boundary::{Line, RowFlags, Table, UiEvent};
use crate::host::ctx;
use crate::widgets::lines::{self, LineKey};
use crate::widgets::selection_bar::BarButtons;
use crate::widgets::table::TableView;
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

/// What an item of a popup menu does when it is chosen.
enum Chosen {
    Nothing,
    Event(UiEvent),
    /// Puts this text on the clipboard.
    Copy(String),
}

struct MenuItem {
    label: String,
    enabled: bool,
    chosen: Chosen,
}

impl MenuItem {
    fn new(label: impl Into<String>, enabled: bool, chosen: Chosen) -> Self {
        Self {
            label: label.into(),
            enabled,
            chosen,
        }
    }

    fn separator() -> Self {
        Self::new("", false, Chosen::Nothing)
    }
}

impl Shell {
    /// Runs `read` on the current tab's table, if it has arrived.
    fn with_table<T>(&self, read: impl FnOnce(&Table) -> T) -> Option<T> {
        let model = ctx().model.borrow();
        model.tables.get(self.tab.get()).map(read)
    }

    /// What line `index` of the list shows, in terms that survive new data.
    fn key_at(&self, index: usize) -> Option<LineKey> {
        self.keys.borrow().get(index).cloned()
    }

    pub(super) fn selected_count(&self) -> usize {
        self.with_table(|table| table.selected).unwrap_or(0)
    }

    /// Which buttons the bar shows for the current table.
    pub(super) fn bar_buttons(&self) -> BarButtons {
        let buttons = self.with_table(|table| BarButtons {
            selection: table.selected > 0,
            commands: !table.commands.is_empty(),
        });
        buttons.unwrap_or(BarButtons {
            selection: false,
            commands: false,
        })
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

    /// Shows the current tab's table as it is in the model now, keeping the
    /// focus on the line it was on if that line is still there.
    pub(super) fn rebuild_table(&self) {
        let ctx = ctx();
        let tab = self.tab.get();
        let focus = self.table.focused().and_then(|index| self.key_at(index));
        let model = ctx.model.borrow();
        let empty = Table::default();
        let table = model.tables.get(tab).unwrap_or(&empty);
        let keys = lines::keys(&table.lines);
        let count = keys.len();
        let position = |key: &LineKey| keys.iter().position(|candidate| candidate == key);
        let focused = focus.as_ref().and_then(position);
        let announce = self.announce.take().as_ref().and_then(position);
        // Sending to the list comes back as notifications that read the
        // model and the keys, so neither is borrowed mutably from here on.
        self.keys.replace(keys);
        let dpi = self.dpi.get();
        self.table.build_columns(table, dpi, self.fonts().ui);
        self.table.set_count(count);
        // After the count: a vertical scroll bar may have come or gone.
        self.table.fit_columns(dpi);
        match focused {
            Some(index) => self.table.focus_item(index, false),
            None if count > 0 && self.table.focused().is_none_or(|focused| focused >= count) => {
                self.table.focus_item(0, false);
            }
            None => {}
        }
        self.table.update_header(table.select_all, table.sort);
        // Before the first table of a tab there is nothing to count.
        let summary = if table.columns.is_empty() {
            "Loading…"
        } else {
            table.summary.as_str()
        };
        self.bar.set(summary);
        drop(model);
        self.sync_bar();
        if let Some(index) = announce {
            self.table.announce_state(index);
        }
    }

    /// Shows the bar's buttons exactly while they apply.
    fn sync_bar(&self) {
        if !is_visible(self.bar.summary) {
            return;
        }
        let wanted = self.bar_buttons();
        let shown = BarButtons {
            selection: is_visible(self.bar.remove),
            commands: is_visible(self.bar.commands),
        };
        if wanted != shown {
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
        // The filter of the tab arrives with the answer to the event below;
        // until then the box is empty rather than the other tab's.
        self.set_filter(tab, "");
        self.show_entry(None);
        self.rebuild_table();
        crate::win::send(self.table.list, LVM_ENSUREVISIBLE, 0, 0);
        self.apply_visibility();
        self.layout();
        ctx.emit(UiEvent::TabChanged { tab });
    }

    /// Puts `text` into the filter box of `tab`, without reporting it as
    /// typed.
    pub(crate) fn set_filter(&self, tab: usize, text: &str) {
        if tab != self.tab.get() || self.filter.text() == text {
            return;
        }
        self.quiet_filter.set(true);
        self.filter.set(text);
        self.quiet_filter.set(false);
    }

    pub(super) fn filter_changed(&self) {
        if self.quiet_filter.get() {
            return;
        }
        let text = self.filter.text();
        ctx().emit(UiEvent::FilterChanged {
            tab: self.tab.get(),
            text,
        });
    }

    /// The line to type of `tab` changed. `text` replaces what is typed.
    pub(crate) fn entry_changed(&self, tab: usize, text: Option<&str>) {
        if tab == self.tab.get() {
            self.show_entry(text);
        }
    }

    /// Makes the entry row show the current tab's entry, or hides it.
    fn show_entry(&self, text: Option<&str>) {
        let entry = {
            let model = ctx().model.borrow();
            model.entries.get(self.tab.get()).cloned().flatten()
        };
        let had = self.has_entry.replace(entry.is_some());
        if let Some(mut entry) = entry {
            entry.text = text.map(str::to_string);
            self.entry.set(&entry);
        }
        if had != self.has_entry.get() {
            self.apply_visibility();
            self.layout();
        }
    }

    /// The entry's button, or Enter in its box.
    pub(super) fn submit_entry(&self) {
        if self.has_entry.get() {
            ctx().emit(UiEvent::Submitted {
                tab: self.tab.get(),
                text: self.entry.text(),
            });
        }
    }

    /// A click on a line's box, or Space on the line.
    fn toggle_at(&self, index: usize, range: bool) {
        let tab = self.tab.get();
        let event = self.with_table(|table| match table.lines.get(index)? {
            Line::Row(row) => row.check.map(|_| UiEvent::RowToggled {
                tab,
                id: row.id.clone(),
                range,
            }),
            Line::Group { name, check, .. } => check.map(|_| UiEvent::GroupToggled {
                tab,
                name: name.clone(),
            }),
        });
        if let Some(event) = event.flatten() {
            // Told to screen readers once the table that shows it is here.
            self.announce.set(self.key_at(index));
            ctx().emit(event);
        }
    }

    fn toggle_all(&self) {
        ctx().emit(UiEvent::AllToggled {
            tab: self.tab.get(),
        });
    }

    pub(super) fn clear_selection(&self) {
        if self.selected_count() > 0 {
            ctx().emit(UiEvent::SelectionCleared {
                tab: self.tab.get(),
            });
        }
    }

    pub(super) fn remove_selected(&self) {
        if self.selected_count() > 0 {
            ctx().emit(UiEvent::RemoveRequested {
                tab: self.tab.get(),
                id: None,
            });
        }
    }

    /// Delete: the selected rows, or else the focused row if it can be
    /// removed.
    fn remove_key(&self) {
        if self.selected_count() > 0 {
            self.remove_selected();
            return;
        }
        let focused = self.table.focused();
        let id = focused.and_then(|index| {
            self.with_table(|table| {
                let row = lines::row_at(table, index)?;
                row.check.map(|_| row.id.clone())
            })
            .flatten()
        });
        if let Some(id) = id {
            ctx().emit(UiEvent::RemoveRequested {
                tab: self.tab.get(),
                id: Some(id),
            });
        }
    }

    /// Action `action` of the table on line `index`, if the row allows it.
    fn run_action(&self, index: usize, action: usize) {
        let tab = self.tab.get();
        let event = self
            .with_table(|table| {
                let row = lines::row_at(table, index)?;
                let spec = table.actions.get(action)?;
                row.action_enabled(action).then(|| UiEvent::RowAction {
                    tab,
                    id: row.id.clone(),
                    action: spec.key.clone(),
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
        let tab = self.tab.get();
        let target = self.with_table(|table| match table.lines.get(index)? {
            Line::Group { name, .. } => Some(Err(name.clone())),
            Line::Row(_) => table.default_action.map(Ok),
        });
        match target.flatten() {
            Some(Ok(action)) => self.run_action(index, action),
            Some(Err(name)) => ctx().emit(UiEvent::GroupFolded { tab, name }),
            None => {}
        }
    }

    /// Left and Right on a group header: collapses and expands it.
    fn fold_key(&self, index: usize, collapse: bool) -> bool {
        let tab = self.tab.get();
        let group = self.with_table(|table| match table.lines.get(index)? {
            Line::Group {
                name, collapsed, ..
            } => Some((name.clone(), *collapsed)),
            Line::Row(_) => None,
        });
        let Some((name, collapsed)) = group.flatten() else {
            return false;
        };
        if collapsed != collapse {
            ctx().emit(UiEvent::GroupFolded { tab, name });
        }
        true
    }

    fn sort_column(&self, column: usize) {
        let sortable = self.with_table(|table| {
            let column = table.columns.get(column);
            column.is_some_and(|column| column.sortable)
        });
        if sortable == Some(true) {
            ctx().emit(UiEvent::SortRequested {
                tab: self.tab.get(),
                column,
            });
        }
    }

    /// The text of line `index` for the clipboard.
    fn row_text(&self, index: usize) -> Option<String> {
        self.with_table(|table| match table.lines.get(index)? {
            Line::Row(row) => Some(row.cells.join("\t")),
            Line::Group { name, .. } => Some(name.clone()),
        })
        .flatten()
    }

    fn copy(&self, text: &str) {
        let units: Vec<u16> = text.encode_utf16().collect();
        copy_to_clipboard(self.hwnd, &units);
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
        self.with_table(|table| {
            let Some(line) = table.lines.get(index) else {
                return;
            };
            if item.mask.contains(LVIF_TEXT) && item.cchTextMax > 0 && !item.pszText.is_null() {
                // SAFETY: the list provides a buffer of `cchTextMax` units.
                let buffer = unsafe {
                    std::slice::from_raw_parts_mut(item.pszText.0, item.cchTextMax as usize)
                };
                let mut sink = TextSink { buffer, length: 0 };
                match line {
                    Line::Row(row) if Some(column) == actions_column => {
                        // Read aloud as the row's last cell; the context
                        // menu is the way to reach them from the keyboard.
                        let mut first = true;
                        for action in lines::button_actions(table) {
                            if row.action_enabled(action) {
                                let label = &table.actions[action].label;
                                let _ = write!(sink, "{}{label}", if first { "" } else { ", " });
                                first = false;
                            }
                        }
                    }
                    Line::Row(row) => {
                        let text = row.cells.get(column).map_or("", String::as_str);
                        let _ = sink.write_str(text);
                    }
                    Line::Group {
                        name,
                        count,
                        collapsed,
                        ..
                    } if column == 0 => {
                        let _ = sink.write_str(&lines::group_title(name, *count, *collapsed));
                    }
                    Line::Group { .. } => {}
                }
            }
            if item.mask.contains(LVIF_STATE) {
                let check = TableView::check_state(lines::check_of(line));
                item.state =
                    LIST_VIEW_ITEM_STATE_FLAGS((item.state.0 & !LVIS_STATEIMAGEMASK.0) | check);
                item.stateMask =
                    LIST_VIEW_ITEM_STATE_FLAGS(item.stateMask.0 | LVIS_STATEIMAGEMASK.0);
            }
            if item.mask.contains(LVIF_INDENT) {
                // In units of the 1-pixel small image.
                item.iIndent = match line {
                    Line::Row(row) if row.grouped => indent,
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
        self.with_table(|table| {
            let row = match table.lines.get(index)? {
                Line::Group { .. } => {
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
                Line::Row(row) => row,
            };
            // SAFETY: as above.
            unsafe { SelectObject(hdc, fonts.ui.into()) };
            if Some(column) == actions_column {
                let labels: Vec<(&str, bool)> = lines::button_actions(table)
                    .into_iter()
                    .map(|action| {
                        let label = table.actions[action].label.as_str();
                        (label, row.action_enabled(action))
                    })
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

    /// A click in the actions column lands on a button or between them.
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
        let buttons = self.with_table(lines::button_actions).unwrap_or_default();
        let cell = self.table.cell_rect(hit.item, actions_column);
        let slot = self
            .table
            .buttons()
            .hit(cell, buttons.len(), point.x, point.y);
        if let Some(action) = slot.and_then(|slot| buttons.get(slot)) {
            self.run_action(hit.item, *action);
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
        self.with_table(|table| lines::find_prefix(&table.lines, &prefix, start))
            .flatten()
            .map_or(-1, |index| index as isize)
    }

    /// A click on a checkbox, or with Shift or Ctrl: ticks instead of
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
            if let Some(text) = focused.and_then(|index| self.row_text(index)) {
                self.copy(&text);
            }
            return true;
        }
        if key == VK_DELETE.0 {
            self.remove_key();
            return true;
        }
        if key == VK_LEFT.0 || key == VK_RIGHT.0 {
            return focused.is_some_and(|index| self.fold_key(index, key == VK_LEFT.0));
        }
        false
    }

    /// Shows `items` as a popup menu at `point` (screen coordinates) and
    /// does what the chosen one stands for.
    fn popup(&self, items: Vec<MenuItem>, point: POINT) {
        // SAFETY: the menu is created, shown and destroyed here; the labels
        // outlive the calls that copy them.
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            for (index, item) in items.iter().enumerate() {
                if item.label.is_empty() {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
                    continue;
                }
                let label = wide(&item.label);
                let flags = if item.enabled {
                    MF_STRING
                } else {
                    MF_STRING | MF_GRAYED
                };
                // 0 means "nothing chosen", so the commands start at 1.
                let _ = AppendMenuW(menu, flags, index + 1, PCWSTR(label.as_ptr()));
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
        let Some(item) = chosen
            .checked_sub(1)
            .and_then(|index| items.into_iter().nth(index))
        else {
            return;
        };
        // The menu's loop dispatches updates, so the row may have gone
        // while it was open. Each event names its row; whoever receives it
        // looks the row up again.
        match item.chosen {
            Chosen::Nothing => {}
            Chosen::Event(event) => ctx().emit(event),
            Chosen::Copy(text) => self.copy(&text),
        }
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
        let tab = self.tab.get();
        let text = self.row_text(index).unwrap_or_default();
        let items = self.with_table(|table| {
            let mut items = Vec::new();
            match table.lines.get(index)? {
                Line::Row(row) => {
                    let event = |action: &str| {
                        Chosen::Event(UiEvent::RowAction {
                            tab,
                            id: row.id.clone(),
                            action: action.to_string(),
                        })
                    };
                    for (action, spec) in table.actions.iter().enumerate() {
                        let enabled = row.action_enabled(action);
                        items.push(MenuItem::new(&spec.label, enabled, event(&spec.key)));
                    }
                    let busy = row.flags.contains(RowFlags::BUSY);
                    for extra in &row.extra {
                        items.push(MenuItem::new(&extra.label, !busy, event(&extra.key)));
                    }
                }
                Line::Group {
                    name,
                    collapsed,
                    check,
                    ..
                } => {
                    let label = if *collapsed { "Expand" } else { "Collapse" };
                    let name = name.clone();
                    let fold = UiEvent::GroupFolded {
                        tab,
                        name: name.clone(),
                    };
                    items.push(MenuItem::new(label, true, Chosen::Event(fold)));
                    if let Some(checked) = check {
                        let label = if *checked {
                            "Deselect group"
                        } else {
                            "Select group"
                        };
                        let toggle = UiEvent::GroupToggled { tab, name };
                        items.push(MenuItem::new(label, true, Chosen::Event(toggle)));
                    }
                }
            }
            items.push(MenuItem::separator());
            items.push(MenuItem::new("Copy\tCtrl+C", true, Chosen::Copy(text)));
            if table.selected > 0 {
                let remove = UiEvent::RemoveRequested { tab, id: None };
                items.push(MenuItem::new(
                    format!("Remove {} selected\tDel", table.selected),
                    true,
                    Chosen::Event(remove),
                ));
                let clear = UiEvent::SelectionCleared { tab };
                items.push(MenuItem::new(
                    "Clear selection\tEsc",
                    true,
                    Chosen::Event(clear),
                ));
            }
            Some(items)
        });
        if let Some(items) = items.flatten() {
            self.popup(items, point);
        }
    }

    /// The table's menu, under the bar's button: its prunes and the like.
    pub(super) fn commands_menu(&self) {
        let tab = self.tab.get();
        let items = self.with_table(|table| {
            let commands = table.commands.iter();
            commands
                .map(|command| {
                    let event = UiEvent::Command {
                        tab,
                        key: command.key.clone(),
                    };
                    MenuItem::new(&command.label, true, Chosen::Event(event))
                })
                .collect::<Vec<_>>()
        });
        let Some(items) = items.filter(|items| !items.is_empty()) else {
            return;
        };
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid out pointer; the button is our child.
        unsafe {
            let _ = GetWindowRect(self.bar.commands, &mut rect);
        }
        self.popup(
            items,
            POINT {
                x: rect.left,
                y: rect.bottom,
            },
        );
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
