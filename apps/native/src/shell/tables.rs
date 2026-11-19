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
    CDRF_NOTIFYITEMDRAW, CDRF_NOTIFYSUBITEMDRAW, CDRF_SKIPDEFAULT, HDI_WIDTH, HDN_BEGINTRACKW,
    HDN_DIVIDERDBLCLICKW, HDN_ENDTRACKW, HDN_ITEMCHANGINGW, HDN_ITEMSTATEICONCLICK,
    LIST_VIEW_ITEM_STATE_FLAGS, LVFI_PARTIAL, LVFI_STRING, LVIF_INDENT, LVIF_STATE, LVIF_TEXT,
    LVIS_STATEIMAGEMASK, LVITEMW, LVM_ENSUREVISIBLE, LVN_COLUMNCLICK, LVN_GETDISPINFOW,
    LVN_ITEMACTIVATE, LVN_ODFINDITEMW, LVN_SETDISPINFOW, NM_CLICK, NM_CUSTOMDRAW,
    NM_RELEASEDCAPTURE, NMCUSTOMDRAW, NMCUSTOMDRAW_DRAW_STAGE, NMHDR, NMHEADERW, NMITEMACTIVATE,
    NMLISTVIEW, NMLVCUSTOMDRAW, NMLVDISPINFOW, NMLVFINDITEMW, TCN_SELCHANGE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SetFocus, VK_DELETE, VK_LEFT, VK_RETURN, VK_RIGHT, VK_SPACE,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DLGC_WANTALLKEYS, DestroyMenu, GetWindowRect, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, PostMessageW, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
    WM_CHAR, WM_GETDLGCODE, WM_KEYDOWN, WM_LBUTTONDBLCLK, WM_LBUTTONDOWN, WM_NCDESTROY, WM_NOTIFY,
};
use windows::core::PCWSTR;

use super::{Shell, ids};
use crate::boundary::{Line, RowFlags, Table, UiEvent};
use crate::host::ctx;
use crate::widgets::columns::{Pressed, fix_after_drag};

/// Posted to the main window when a divider of the list's header was let
/// go; `wparam` is the column at its left.
pub(super) const WM_DIVIDER_DRAGGED: u32 = WM_APP + 20;
/// The narrowest a divider can make a column, in device-independent
/// pixels: enough to find the divider again.
const LEAST_COLUMN: i32 = 40;
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
        self.name_list();
        self.rebuild_table();
        self.apply_visibility();
        self.layout();
    }

    /// Names the list after its tab, for screen readers: a list without a
    /// name is read as its control number.
    fn name_list(&self) {
        let title = {
            let model = ctx().model.borrow();
            model.tabs.get(self.tab.get()).cloned().unwrap_or_default()
        };
        crate::win::set_text(self.table.list, &title);
    }

    pub(crate) fn table_changed(&self, tab: usize) {
        if tab != self.tab.get() {
            return;
        }
        if self.log_open.get() {
            // The log covers the list. It is brought up to date once, when
            // the log is closed, not with every table that arrives.
            self.table_stale.set(true);
            return;
        }
        self.rebuild_table();
    }

    /// Brings the list up to date if tables arrived while a log covered it.
    pub(super) fn refresh_stale_table(&self) {
        if self.table_stale.get() {
            self.rebuild_table();
        }
    }

    /// Shows the current tab's table as it is in the model now, keeping the
    /// focus on the line it was on if that line is still there.
    pub(super) fn rebuild_table(&self) {
        let ctx = ctx();
        self.table_stale.set(false);
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
        self.fit_table();
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

    /// Lays the list's columns out in its width, around the widths the
    /// user set on this tab.
    pub(super) fn fit_table(&self) {
        // Not under a divider that is being dragged: the list has that
        // column where the pointer is, and a table that arrives meanwhile
        // must not pull it back. It is laid out when the divider is let go.
        if self.dragging.get() {
            return;
        }
        let fixed = {
            let fixed = self.fixed_widths.borrow();
            fixed.get(self.tab.get()).cloned().unwrap_or_default()
        };
        self.table.fit_columns(self.dpi.get(), &fixed);
    }

    /// Whether the divider at the right of column `column` can be dragged:
    /// that of a column that shows, the buttons' column not among them.
    fn divider_moves(&self, column: usize) -> bool {
        column < self.table.data_columns() && self.table.column_width(column) > 0
    }

    /// What the list's header says about a divider. Returns the answer if
    /// the list is not to see the notification.
    fn header_divider(&self, notice: &NMHEADERW) -> Option<LRESULT> {
        let column = usize::try_from(notice.iItem).ok()?;
        match notice.hdr.code {
            HDN_BEGINTRACKW => {
                if !self.divider_moves(column) {
                    // Refused: the drag does not begin.
                    return Some(LRESULT(1));
                }
                self.dragging.set(true);
            }
            HDN_ITEMCHANGINGW if self.dragging.get() => {
                // SAFETY: when not null, the notification's item is valid
                // for as long as the notification lasts.
                let item = unsafe { notice.pitem.as_ref() }?;
                let least = self.dpi.get().px(LEAST_COLUMN);
                if item.mask.0 & HDI_WIDTH.0 != 0 && item.cxy < least {
                    // Refused: the column stays as wide as it was.
                    return Some(LRESULT(1));
                }
            }
            HDN_ENDTRACKW => self.divider_let_go(column),
            HDN_DIVIDERDBLCLICKW => {
                if !self.divider_moves(column) {
                    return Some(LRESULT(0));
                }
                // The list makes the column as wide as what is in it; that
                // is a width the user set like any other.
                self.divider_let_go(column);
            }
            _ => {}
        }
        None
    }

    /// The drag of the divider right of `column` is over. The list takes
    /// in the last width after this notification; what follows from it is
    /// done from a message of its own, once the header is done with the
    /// mouse.
    fn divider_let_go(&self, column: usize) {
        self.dragging.set(false);
        // SAFETY: posting to this thread's own window.
        unsafe {
            let _ = PostMessageW(
                Some(self.hwnd),
                WM_DIVIDER_DRAGGED,
                WPARAM(column),
                LPARAM(0),
            );
        }
    }

    /// The header let go of the mouse. A drag that ended without saying so
    /// (the window lost the mouse to something else) is over all the same.
    fn header_released(&self) {
        if self.dragging.get() {
            self.divider_let_go(usize::MAX);
        }
    }

    /// Keeps the widths a drag of the divider right of `column` set, and
    /// lays the other columns out around them.
    pub(super) fn divider_dragged(&self, column: usize) {
        let count = self.table.data_columns();
        if column < count {
            let widths: Vec<i32> = (0..count)
                .map(|index| self.table.column_width(index))
                .collect();
            let least = self.dpi.get().px(LEAST_COLUMN);
            let most = crate::win::client_rect(self.table.list).right;
            let tab = self.tab.get();
            let mut fixed = self.fixed_widths.borrow_mut();
            if fixed.len() <= tab {
                fixed.resize_with(tab + 1, Vec::new);
            }
            fix_after_drag(&mut fixed[tab], &widths, column, least, most);
        }
        self.table.widths_changed_by_hand();
        self.fit_table();
        self.table.redraw_all();
    }

    /// The window moved to a screen of another DPI: the widths the user
    /// set are in pixels.
    pub(super) fn scale_fixed_widths(&self, from: crate::win::Dpi, to: crate::win::Dpi) {
        let scale =
            |width: i32| (i64::from(width) * i64::from(to.0) / i64::from(from.0.max(1))) as i32;
        for tab in self.fixed_widths.borrow_mut().iter_mut() {
            for width in tab.iter_mut().flatten() {
                *width = scale(*width);
            }
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
        self.tab.set(tab);
        // The filter of the tab arrives with the answer to the event below;
        // until then the box is empty rather than the other tab's.
        self.set_filter(tab, "");
        self.show_entry(None);
        self.name_list();
        self.rebuild_table();
        crate::win::send(self.table.list, LVM_ENSUREVISIBLE, 0, 0);
        // The tab is said before a log that covered the old one is closed:
        // whoever listens then loads the list of the new tab, and not that
        // of the old one first.
        ctx.emit(UiEvent::TabChanged { tab });
        if self.log_open.get() {
            ctx.model.borrow_mut().log = None;
            // Lays the window out again as well.
            self.close_log();
        } else {
            self.apply_visibility();
            self.layout();
        }
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
            (ids::LIST, LVN_SETDISPINFOW) => {
                // SAFETY: as above.
                let info = unsafe { &*(lparam.0 as *const NMLVDISPINFOW) };
                self.item_set(&info.item);
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
                // What stands in a place depends on the row: Stop on one
                // that runs, where Start is on one that does not.
                let label = |slot: usize| {
                    let action = self.table.slot_action(row, slot)?;
                    let label = table.actions.get(action)?.label.as_str();
                    Some((label, row.action_enabled(action)))
                };
                let cell = self.table.cell_rect(index, column);
                self.table.paint_buttons(hdc, cell, label, &theme, dpi);
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
        let cell = self.table.cell_rect(hit.item, actions_column);
        let buttons = self.table.buttons();
        let Some((pressed, button)) = buttons.hit(cell, point.x, point.y) else {
            return;
        };
        // A group header has no buttons, whatever is under the pointer.
        let slot_action = |slot: usize| {
            let action = self.with_table(|table| {
                let row = lines::row_at(table, hit.item)?;
                self.table.slot_action(row, slot)
            });
            action.flatten()
        };
        match pressed {
            Pressed::Slot(slot) => {
                if let Some(action) = slot_action(slot) {
                    self.run_action(hit.item, action);
                }
            }
            // The buttons that did not fit are in the row's menu, with
            // everything else that can be done to the row.
            Pressed::More => {
                let is_row = self.with_table(|table| lines::row_at(table, hit.item).is_some());
                if is_row == Some(true) {
                    self.table.focus_item(hit.item, false);
                    let under = POINT {
                        x: button.left,
                        y: button.bottom,
                    };
                    self.row_menu(hit.item, under);
                }
            }
        }
    }

    /// The list was told the state of a line's box instead of being asked:
    /// an accessibility tool ticked or unticked it. The boxes are not the
    /// list's to keep, so this is a click on the box like any other.
    fn item_set(&self, item: &LVITEMW) {
        if !item.mask.contains(LVIF_STATE) || item.stateMask.0 & LVIS_STATEIMAGEMASK.0 == 0 {
            return;
        }
        let Ok(index) = usize::try_from(item.iItem) else {
            return;
        };
        let wanted = item.state.0 & LVIS_STATEIMAGEMASK.0 == TableView::check_state(Some(true));
        let ticked = self.with_table(|table| table.lines.get(index).and_then(lines::check_of));
        if ticked.flatten().is_some_and(|ticked| ticked != wanted) {
            self.toggle_at(index, false);
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

    /// The context menu of the list, by right click, the Apps key or
    /// Shift+F10: the menu of the line under the pointer or, from the
    /// keyboard, of the focused line.
    pub(super) fn list_context_menu(&self, lparam: LPARAM) {
        // From the keyboard the point is (-1, -1).
        let keyboard = point_of(lparam) == POINT { x: -1, y: -1 };
        let (index, point) = if keyboard {
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
        self.row_menu(index, point);
    }

    /// The menu of line `index`, opened at `point` of the list. For a row
    /// it holds every action of the table, with or without a button, the
    /// row's own (its ports), Copy, and the selection's Remove and Clear:
    /// the drawn buttons are nothing a keyboard or a screen reader can
    /// reach, so whatever they do is here too.
    fn row_menu(&self, index: usize, mut point: POINT) {
        // SAFETY: converting a point of the list to screen coordinates.
        unsafe {
            let _ = ClientToScreen(self.table.list, &mut point);
        }
        let tab = self.tab.get();
        let items = self.with_table(|table| line_menu(table, index, tab));
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

/// What the menu of line `index` of `table` holds, or `None` if there is no
/// such line. See `Shell::row_menu`.
fn line_menu(table: &Table, index: usize, tab: usize) -> Option<Vec<MenuItem>> {
    let mut items = Vec::new();
    let text = match table.lines.get(index)? {
        Line::Row(row) => {
            let event = |action: &str| {
                Chosen::Event(UiEvent::RowAction {
                    tab,
                    id: row.id.clone(),
                    action: action.to_string(),
                })
            };
            // All of them, in the table's order: an action that does not
            // apply is greyed, not left out, so the menu reads the same on
            // every row.
            for (action, spec) in table.actions.iter().enumerate() {
                let enabled = row.action_enabled(action);
                items.push(MenuItem::new(&spec.label, enabled, event(&spec.key)));
            }
            let busy = row.flags.contains(RowFlags::BUSY);
            for extra in &row.extra {
                items.push(MenuItem::new(&extra.label, !busy, event(&extra.key)));
            }
            row.cells.join("\t")
        }
        Line::Group {
            name,
            collapsed,
            check,
            ..
        } => {
            let label = if *collapsed { "Expand" } else { "Collapse" };
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
                let toggle = UiEvent::GroupToggled {
                    tab,
                    name: name.clone(),
                };
                items.push(MenuItem::new(label, true, Chosen::Event(toggle)));
            }
            name.clone()
        }
    };
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
                if matches!(
                    header.code,
                    HDN_BEGINTRACKW | HDN_ITEMCHANGINGW | HDN_ENDTRACKW | HDN_DIVIDERDBLCLICKW
                ) {
                    // SAFETY: these codes come with an NMHEADERW.
                    let notice = unsafe { &*(lparam.0 as *const NMHEADERW) };
                    if let Some(result) = shell.header_divider(notice) {
                        return result;
                    }
                }
                if header.code == NM_RELEASEDCAPTURE {
                    shell.header_released();
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

#[cfg(test)]
mod tests {
    use docklet_app::present::{TABS, table_view};
    use docklet_app::{ListModel, ResourceKind};

    use super::*;
    use crate::boundary::{Action, Row};

    /// The labels of a menu and whether each can be chosen; a separator is
    /// an empty label.
    fn labels(items: &[MenuItem]) -> Vec<(&str, bool)> {
        let items = items.iter();
        items
            .map(|item| (item.label.as_str(), item.enabled))
            .collect()
    }

    /// The containers table with one running container that publishes a
    /// port.
    fn containers() -> Table {
        let mut table = table_view(&ListModel::new(ResourceKind::Container), true, None);
        let mut row = Row::new("abc", vec!["web".to_string(), "nginx:1".to_string()]);
        // Stop, Restart, Logs, Open and Remove apply; Start does not.
        row.actions = 0b11_1110;
        row.extra = vec![Action::in_menu("open:8080", "Open localhost:8080")];
        table.lines = vec![
            Line::Group {
                name: "shop".to_string(),
                count: 1,
                collapsed: false,
                check: Some(false),
            },
            Line::Row(row),
        ];
        table
    }

    #[test]
    fn a_rows_menu_offers_every_action_with_or_without_a_button() {
        let table = containers();
        let items = line_menu(&table, 1, 0).unwrap();
        assert_eq!(
            labels(&items),
            [
                ("Start", false),
                ("Stop", true),
                ("Restart", true),
                ("Logs", true),
                ("Open", true),
                ("Remove", true),
                ("Open localhost:8080", true),
                ("", false),
                ("Copy\tCtrl+C", true),
            ]
        );
        // Each asks for its own action of this row.
        let asked: Vec<&str> = items
            .iter()
            .filter_map(|item| match &item.chosen {
                Chosen::Event(UiEvent::RowAction { tab: 0, id, action }) if id == "abc" => {
                    Some(action.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            asked,
            [
                "start",
                "stop",
                "restart",
                "logs",
                "open",
                "remove",
                "open:8080"
            ]
        );
        assert!(matches!(&items[8].chosen, Chosen::Copy(text) if text == "web\tnginx:1"));
    }

    #[test]
    fn every_tabs_row_menu_holds_all_of_the_tabs_actions() {
        for (tab, kind) in TABS.into_iter().enumerate() {
            let mut table = table_view(&ListModel::new(kind), true, None);
            table.lines = vec![Line::Row(Row::new("x", vec!["name".to_string()]))];
            let items = line_menu(&table, 0, tab).unwrap();
            for action in &table.actions {
                let offered = items.iter().any(|item| {
                    matches!(&item.chosen, Chosen::Event(UiEvent::RowAction { action: key, .. })
                        if *key == action.key)
                        && item.label == action.label
                });
                assert!(offered, "{kind:?}: {} is not in the menu", action.label);
            }
        }
    }

    #[test]
    fn a_busy_row_has_its_menu_with_everything_greyed() {
        let mut table = containers();
        if let Line::Row(row) = &mut table.lines[1] {
            row.flags = RowFlags::BUSY;
        }
        let items = line_menu(&table, 1, 0).unwrap();
        assert!(items[..7].iter().all(|item| !item.enabled));
        // Copying its text is still possible.
        assert!(items[8].enabled);
    }

    #[test]
    fn the_selection_and_the_groups_have_their_items_too() {
        let mut table = containers();
        table.selected = 3;
        let row = line_menu(&table, 1, 0).unwrap();
        let tail: Vec<&str> = row[9..].iter().map(|item| item.label.as_str()).collect();
        assert_eq!(tail, ["Remove 3 selected\tDel", "Clear selection\tEsc"]);
        let group = line_menu(&table, 0, 0).unwrap();
        assert_eq!(
            labels(&group)[..2],
            [("Collapse", true), ("Select group", true)]
        );
        assert!(matches!(&group[3].chosen, Chosen::Copy(text) if text == "shop"));
        assert!(line_menu(&table, 9, 0).is_none());
    }
}
