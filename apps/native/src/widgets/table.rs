//! The table: a stock ListView in virtual mode (`LVS_OWNERDATA`), so it asks
//! for the text of the cells it shows and Narrator reads rows, checkboxes and
//! columns. The first column carries the checkboxes and the header's
//! select-all checkbox; the last column, when the table has actions, holds
//! buttons drawn over the row.
//!
//! The columns always fit the list: [`table_fit`] decides which show and
//! which actions have a button. A column that does not fit keeps its place
//! with no width, so column numbers mean the same at every size and a
//! screen reader still reads its cell. A divider the user dragged fixes the
//! width of its column; the others are fitted around it.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::SIZE;
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Gdi::{
    DC_BRUSH, DC_PEN, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DrawTextW, GetDC, GetStockObject,
    GetTextExtentPoint32W, HDC, HFONT, ReleaseDC, RoundRect, SelectObject, SetBkMode,
    SetDCBrushColor, SetDCPenColor, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Accessibility::NotifyWinEvent;
use windows::Win32::UI::Controls::{
    HDF_CHECKBOX, HDF_CHECKED, HDF_SORTDOWN, HDF_SORTUP, HDI_FORMAT, HDITEMW, HDM_GETITEMW,
    HDM_SETITEMW, HDS_CHECKBOXES, HIMAGELIST, ILC_COLOR32, ImageList_Create, ImageList_Destroy,
    LIST_VIEW_ITEM_STATE_FLAGS, LVCF_FMT, LVCF_SUBITEM, LVCF_TEXT, LVCF_WIDTH, LVCFMT_CENTER,
    LVCFMT_LEFT, LVCFMT_RIGHT, LVCOLUMNW, LVHITTESTINFO, LVHT_ONITEMSTATEICON, LVIR_BOUNDS,
    LVIS_FOCUSED, LVIS_SELECTED, LVIS_STATEIMAGEMASK, LVITEMW, LVM_DELETECOLUMN, LVM_ENSUREVISIBLE,
    LVM_GETCOLUMNWIDTH, LVM_GETHEADER, LVM_GETITEMRECT, LVM_GETNEXTITEM, LVM_GETSUBITEMRECT,
    LVM_INSERTCOLUMNW, LVM_SETCALLBACKMASK, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE,
    LVM_SETIMAGELIST, LVM_SETITEMCOUNT, LVM_SETITEMSTATE, LVM_SETSELECTIONMARK, LVM_SUBITEMHITTEST,
    LVNI_FOCUSED, LVS_EX_CHECKBOXES, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP,
    LVS_OWNERDATA, LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVSICF_NOSCROLL, LVSIL_SMALL,
    WC_LISTVIEWW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_STATECHANGE, GWL_STYLE, GetWindowLongPtrW, OBJID_CLIENT, SetWindowLongPtrW,
    WS_TABSTOP,
};

use super::columns::{ButtonMetrics, ButtonRow, ButtonSpec, ColumnSpec, Fit, Pressed, Rect, fit};
use super::lines;
use crate::boundary::{Align, Column, Coverage, Row, SortMark, Table};
use crate::theme::Theme;
use crate::win::{Dpi, child, client_rect, send, send_ptr};

/// Row height in device-independent pixels; the row buttons need the room.
const ROW_HEIGHT: i32 = 30;
/// Room the checkbox takes in the first column.
const CHECKBOX_ROOM: i32 = 28;
/// Room on each side of a button's label.
const BUTTON_PADDING: i32 = 8;
/// What the button that opens the row's menu says.
const MORE_LABEL: &str = "…";

/// Where a point landed in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub item: usize,
    pub column: usize,
    pub on_checkbox: bool,
}

/// One place for a button: the actions that can stand there (Start and Stop
/// share one) by their index in `Table::actions`, what they are called, and
/// how soon the place folds into the row's menu.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Slot {
    actions: Vec<usize>,
    labels: Vec<String>,
    fold: u8,
}

/// What the list's columns were built from.
#[derive(Default)]
struct Built {
    columns: Vec<Column>,
    slots: Vec<Slot>,
    /// Width of each slot's button, measured in the list's font.
    slot_widths: Vec<i32>,
    dpi: u32,
}

impl Built {
    /// Whether the list's columns are those of `table` at `dpi`. Compared
    /// in place: this runs with every table, about once a second.
    fn is_for(&self, table: &Table, slots: &[Vec<usize>], dpi: u32) -> bool {
        let same_slot = |built: &Slot, slot: &Vec<usize>| {
            let labels = slot
                .iter()
                .map(|&action| table.actions[action].label.as_str());
            let fold = slot.first().map_or(0, |&action| table.actions[action].fold);
            built.actions == *slot
                && built.fold == fold
                && built.labels.iter().map(String::as_str).eq(labels)
        };
        self.dpi == dpi
            && self.columns == table.columns
            && self.slots.len() == slots.len()
            && self.slots.iter().zip(slots).all(|(a, b)| same_slot(a, b))
    }
}

/// The sizes around the row buttons at `dpi`.
pub fn button_metrics(dpi: Dpi) -> ButtonMetrics {
    ButtonMetrics {
        gap: dpi.px(6),
        inset: dpi.px(4),
        more: dpi.px(28),
    }
}

/// Lays a table's columns and buttons out in a list `available` pixels
/// wide (see [`fit`]). `buttons` gives the width and the fold number of
/// each button place, `fixed` the width of each column whose divider the
/// user dragged (`None`, or no entry, for the others).
///
/// A width the user set is kept as it is: the column neither shares what
/// is left over nor gives way, and the columns that are still automatic
/// are laid out in the rest. Should the list be narrower than the fixed
/// widths and what else never gives way, all of them are squeezed, as
/// without any: nothing the user does makes the list scroll sideways.
pub fn table_fit(
    available: i32,
    columns: &[Column],
    fixed: &[Option<i32>],
    buttons: &[(i32, u8)],
    dpi: Dpi,
) -> Fit {
    let columns: Vec<ColumnSpec> = columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            if let Some(width) = fixed.get(index).copied().flatten() {
                return ColumnSpec {
                    weight: 0,
                    min: width.max(0),
                    priority: 0,
                };
            }
            // The first column holds the checkbox too.
            let extra = if index == 0 { dpi.px(CHECKBOX_ROOM) } else { 0 };
            ColumnSpec {
                weight: column.weight,
                min: dpi.px(i32::from(column.min_width)) + extra,
                priority: column.priority,
            }
        })
        .collect();
    let buttons: Vec<ButtonSpec> = buttons
        .iter()
        .map(|&(width, fold)| ButtonSpec { width, fold })
        .collect();
    fit(available, &columns, &buttons, button_metrics(dpi))
}

pub struct TableView {
    pub list: HWND,
    pub header: HWND,
    built: RefCell<Built>,
    /// How the columns and the buttons are laid out now.
    fit: RefCell<Fit>,
    /// The widths the list was last given, so only changes are sent to it.
    sent: RefCell<Vec<i32>>,
    images: Cell<HIMAGELIST>,
}

/// Packs a state image index into item state bits.
fn state_image(index: u32) -> u32 {
    index << 12
}

impl TableView {
    pub fn create(parent: HWND, id: usize) -> Self {
        let list = child(
            parent,
            WC_LISTVIEWW,
            "",
            WS_TABSTOP.0 | LVS_REPORT | LVS_OWNERDATA | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            0,
            id,
        );
        send(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_CHECKBOXES | LVS_EX_LABELTIP)
                as isize,
        );
        // A virtual list keeps no item state of its own, so it asks for the
        // checkboxes along with the text.
        send(list, LVM_SETCALLBACKMASK, LVIS_STATEIMAGEMASK.0 as usize, 0);
        let header = HWND(send(list, LVM_GETHEADER, 0, 0) as *mut _);
        // SAFETY: adding a style bit to the list's own header. Its dividers
        // can be dragged: the window keeps the widths that sets and lays
        // the other columns out around them (see `table_fit`).
        unsafe {
            let style = GetWindowLongPtrW(header, GWL_STYLE);
            SetWindowLongPtrW(header, GWL_STYLE, style | HDS_CHECKBOXES as isize);
        }
        Self {
            list,
            header,
            built: RefCell::new(Built::default()),
            fit: RefCell::new(Fit::default()),
            sent: RefCell::new(Vec::new()),
            images: Cell::new(HIMAGELIST::default()),
        }
    }

    /// A row is as tall as its small image, so an empty image list of the
    /// right height sets the row height.
    pub fn set_dpi(&self, dpi: Dpi) {
        // SAFETY: the new list is handed to the control, which owns it from
        // then on and destroys the one it holds when it goes itself. The
        // one it held before is handed back by the message, and is ours to
        // destroy.
        unsafe {
            let images = ImageList_Create(1, dpi.px(ROW_HEIGHT), ILC_COLOR32, 1, 0);
            send(self.list, LVM_SETIMAGELIST, LVSIL_SMALL as usize, images.0);
            let _ = ImageList_Destroy(Some(self.images.replace(images)));
        }
    }

    /// The row buttons as they are laid out now.
    pub fn buttons(&self) -> ButtonRow {
        self.fit.borrow().buttons.clone()
    }

    /// Index of the actions column, if the table shows one.
    pub fn actions_column(&self) -> Option<usize> {
        let built = self.built.borrow();
        (!built.slots.is_empty()).then_some(built.columns.len())
    }

    /// The action whose button stands in place `slot` on `row`, by its
    /// index in `Table::actions`.
    pub fn slot_action(&self, row: &Row, slot: usize) -> Option<usize> {
        let built = self.built.borrow();
        lines::slot_action(row, &built.slots.get(slot)?.actions)
    }

    /// Rebuilds the columns when the table's columns, its actions or the
    /// DPI differ from what the list has. Returns whether it did.
    pub fn build_columns(&self, table: &Table, dpi: Dpi, font: HFONT) -> bool {
        let slots = lines::button_slots(table);
        if self.built.borrow().is_for(table, &slots, dpi.0) {
            return false;
        }
        let slots: Vec<Slot> = slots
            .iter()
            .map(|slot| Slot {
                actions: slot.clone(),
                labels: slot
                    .iter()
                    .map(|&action| table.actions[action].label.clone())
                    .collect(),
                fold: slot.first().map_or(0, |&action| table.actions[action].fold),
            })
            .collect();
        let padding = 2 * dpi.px(BUTTON_PADDING);
        let wanted = Built {
            columns: table.columns.clone(),
            slot_widths: slots
                .iter()
                .map(|slot| measure(self.list, font, &slot.labels) + padding)
                .collect(),
            slots,
            dpi: dpi.0,
        };
        // Each call deletes one column and fails once none is left. The
        // count is bounded either way.
        for _ in 0..64 {
            if send(self.list, LVM_DELETECOLUMN, 0, 0) == 0 {
                break;
            }
        }
        let titles = wanted
            .columns
            .iter()
            .map(|column| (column.title.as_str(), column.align))
            .chain((!wanted.slots.is_empty()).then_some(("Actions", Align::Left)));
        for (index, (title, align)) in titles.enumerate() {
            let mut text: Vec<u16> = title.encode_utf16().chain([0]).collect();
            // The first column is always left-aligned in a ListView.
            let fmt = match align {
                _ if index == 0 => LVCFMT_LEFT,
                Align::Left => LVCFMT_LEFT,
                Align::Right => LVCFMT_RIGHT,
                Align::Center => LVCFMT_CENTER,
            };
            let mut column = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH | LVCF_SUBITEM | LVCF_FMT,
                fmt,
                cx: dpi.px(100),
                pszText: windows::core::PWSTR(text.as_mut_ptr()),
                iSubItem: index as i32,
                ..Default::default()
            };
            send_ptr(self.list, LVM_INSERTCOLUMNW, index, &mut column);
        }
        *self.built.borrow_mut() = wanted;
        // The new columns have the width they were inserted with.
        self.sent.borrow_mut().clear();
        true
    }

    /// How many columns the table has, the actions column not counted:
    /// the columns whose dividers can be dragged.
    pub fn data_columns(&self) -> usize {
        self.built.borrow().columns.len()
    }

    /// The width the list has column `index` at right now.
    pub fn column_width(&self, index: usize) -> i32 {
        send(self.list, LVM_GETCOLUMNWIDTH, index, 0) as i32
    }

    /// The user dragged a divider: the list's columns are no longer as
    /// wide as they were last told to be, and are all told again.
    pub fn widths_changed_by_hand(&self) {
        self.sent.borrow_mut().clear();
    }

    /// Lays the columns and the row buttons out in the list's width, so
    /// that nothing is scrolled sideways (see [`table_fit`]). `fixed` are
    /// the widths the user set by dragging dividers.
    pub fn fit_columns(&self, dpi: Dpi, fixed: &[Option<i32>]) {
        // Twice at most. Columns that were too wide had a horizontal scroll
        // bar under them; once they fit it goes, and the vertical one may
        // go with it, which makes the list wider than it was measured.
        for _ in 0..2 {
            let available = client_rect(self.list).right;
            self.fit_to(available, dpi, fixed);
            if client_rect(self.list).right == available {
                break;
            }
        }
    }

    fn fit_to(&self, available: i32, dpi: Dpi, fixed: &[Option<i32>]) {
        let (fit, has_actions) = {
            let built = self.built.borrow();
            let widths = built.slot_widths.iter();
            let buttons: Vec<(i32, u8)> = widths
                .zip(&built.slots)
                .map(|(&width, slot)| (width, slot.fold))
                .collect();
            let fit = table_fit(available, &built.columns, fixed, &buttons, dpi);
            // What makes a horizontal scroll bar impossible. Only a list
            // narrower than the menu button alone could break it.
            debug_assert!(fit.total() <= available.max(dpi.px(48)), "{fit:?}");
            (fit, !built.slots.is_empty())
        };
        let mut widths = fit.widths.clone();
        if has_actions {
            widths.push(fit.buttons.column_width());
        }
        let buttons_moved = self.fit.borrow().buttons != fit.buttons;
        // Stored before anything is sent: the list may draw while it takes
        // a width, and the buttons it draws are those of the new layout.
        let before = self.sent.replace(widths.clone());
        self.fit.replace(fit);
        for (index, &width) in widths.iter().enumerate() {
            // A width it already has would still lay the list out again,
            // and this runs with every table.
            if before.get(index) != Some(&width) {
                send(self.list, LVM_SETCOLUMNWIDTH, index, width as isize);
            }
        }
        if buttons_moved {
            self.redraw_all();
        }
    }

    pub fn set_count(&self, count: usize) {
        send(self.list, LVM_SETITEMCOUNT, count, LVSICF_NOSCROLL as isize);
        self.redraw_all();
    }

    pub fn redraw_all(&self) {
        crate::win::invalidate(self.list);
    }

    pub fn focused(&self) -> Option<usize> {
        let index = send(
            self.list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_FOCUSED as isize,
        );
        usize::try_from(index).ok()
    }

    /// Moves the focus and the highlight to `index`, and scrolls to it if
    /// `scroll` (not when data refreshes under a user who scrolled away).
    pub fn focus_item(&self, index: usize, scroll: bool) {
        let both = LIST_VIEW_ITEM_STATE_FLAGS(LVIS_FOCUSED.0 | LVIS_SELECTED.0);
        let mut item = LVITEMW {
            stateMask: both,
            state: both,
            ..Default::default()
        };
        send_ptr(self.list, LVM_SETITEMSTATE, index, &mut item);
        send(self.list, LVM_SETSELECTIONMARK, 0, index as isize);
        if scroll {
            send(self.list, LVM_ENSUREVISIBLE, index, 0);
        }
    }

    /// Tells screen readers that line `index` changed state. A virtual list
    /// does not know its checkboxes changed, since the app keeps them.
    pub fn announce_state(&self, index: usize) {
        let Ok(child) = i32::try_from(index + 1) else {
            return;
        };
        // SAFETY: raises an accessibility event for this thread's window.
        unsafe {
            NotifyWinEvent(EVENT_OBJECT_STATECHANGE, self.list, OBJID_CLIENT.0, child);
        }
    }

    /// Sets the select-all checkbox and the sort arrows in the header.
    pub fn update_header(&self, check: Coverage, sort: Option<SortMark>) {
        let count = self.built.borrow().columns.len();
        for index in 0..count {
            let mut item = HDITEMW {
                mask: HDI_FORMAT,
                ..Default::default()
            };
            send_ptr(self.header, HDM_GETITEMW, index, &mut item);
            let mut fmt = item.fmt.0 & !(HDF_SORTUP.0 | HDF_SORTDOWN.0 | HDF_CHECKED.0);
            if let Some(sort) = sort.filter(|sort| sort.column == index) {
                fmt |= if sort.ascending {
                    HDF_SORTUP.0
                } else {
                    HDF_SORTDOWN.0
                };
            }
            if index == 0 {
                fmt |= HDF_CHECKBOX.0;
                if check == Coverage::All {
                    fmt |= HDF_CHECKED.0;
                }
            }
            if fmt != item.fmt.0 {
                item.fmt.0 = fmt;
                send_ptr(self.header, HDM_SETITEMW, index, &mut item);
            }
        }
    }

    pub fn hit_test(&self, point: POINT) -> Option<Hit> {
        let mut info = LVHITTESTINFO {
            pt: point,
            ..Default::default()
        };
        send_ptr(self.list, LVM_SUBITEMHITTEST, 0, &mut info);
        let item = usize::try_from(info.iItem).ok()?;
        Some(Hit {
            item,
            column: info.iSubItem.max(0) as usize,
            on_checkbox: info.flags.contains(LVHT_ONITEMSTATEICON),
        })
    }

    pub fn cell_rect(&self, item: usize, column: usize) -> Rect {
        // LVM_GETSUBITEMRECT reads the column and the kind of rectangle from
        // the rectangle it is about to fill.
        let mut rect = RECT {
            left: LVIR_BOUNDS as i32,
            top: column as i32,
            ..Default::default()
        };
        send_ptr(self.list, LVM_GETSUBITEMRECT, item, &mut rect);
        Rect {
            left: rect.left,
            top: rect.top,
            right: rect.right,
            bottom: rect.bottom,
        }
    }

    pub fn item_rect(&self, item: usize) -> RECT {
        let mut rect = RECT {
            left: LVIR_BOUNDS as i32,
            ..Default::default()
        };
        send_ptr(self.list, LVM_GETITEMRECT, item, &mut rect);
        rect
    }

    /// Paints the buttons of one row's actions cell. `label` gives what the
    /// button of a place says on this row and whether it can be pressed.
    pub fn paint_buttons<'a>(
        &self,
        hdc: HDC,
        cell: Rect,
        label: impl Fn(usize) -> Option<(&'a str, bool)>,
        theme: &Theme,
        dpi: Dpi,
    ) {
        let fit = self.fit.borrow();
        let corner = dpi.px(6);
        // SAFETY: `hdc` is the list's paint DC for the duration of the
        // notification; the stock DC pen and brush are selected back out.
        unsafe {
            let pen = SelectObject(hdc, GetStockObject(DC_PEN));
            let brush = SelectObject(hdc, GetStockObject(DC_BRUSH));
            SetDCPenColor(hdc, theme.border);
            SetDCBrushColor(hdc, theme.button);
            SetBkMode(hdc, TRANSPARENT);
            for (button, rect) in fit.buttons.buttons(cell) {
                let (label, enabled) = match button {
                    Pressed::Slot(slot) => match label(slot) {
                        Some(label) => label,
                        None => continue,
                    },
                    Pressed::More => (MORE_LABEL, true),
                };
                let _ = RoundRect(
                    hdc,
                    rect.left,
                    rect.top,
                    rect.right,
                    rect.bottom,
                    corner,
                    corner,
                );
                SetTextColor(hdc, if enabled { theme.text } else { theme.muted });
                let mut text: Vec<u16> = label.encode_utf16().collect();
                let mut area = RECT {
                    left: rect.left,
                    top: rect.top,
                    right: rect.right,
                    bottom: rect.bottom,
                };
                DrawTextW(
                    hdc,
                    &mut text,
                    &mut area,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
            }
            SelectObject(hdc, pen);
            SelectObject(hdc, brush);
        }
    }

    /// The state bits for a line's box: ticked, empty, or no box at all.
    pub fn check_state(check: Option<bool>) -> u32 {
        state_image(match check {
            None => 0,
            Some(false) => 1,
            Some(true) => 2,
        })
    }
}

/// Width of the widest label in `font`.
fn measure(hwnd: HWND, font: HFONT, labels: &[String]) -> i32 {
    // SAFETY: the DC is released and the font selected back out.
    unsafe {
        let hdc = GetDC(Some(hwnd));
        let previous = SelectObject(hdc, font.into());
        let mut widest = 0;
        for label in labels {
            let text: Vec<u16> = label.encode_utf16().collect();
            let mut size = SIZE::default();
            let _ = GetTextExtentPoint32W(hdc, &text, &mut size);
            widest = widest.max(size.cx);
        }
        SelectObject(hdc, previous);
        ReleaseDC(Some(hwnd), hdc);
        widest
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkbox_states_are_state_images_one_and_two() {
        assert_eq!(TableView::check_state(Some(false)), 0x1000);
        assert_eq!(TableView::check_state(Some(true)), 0x2000);
        assert_eq!(
            TableView::check_state(Some(true)) & !LVIS_STATEIMAGEMASK.0,
            0
        );
        // No state image: the line has no box.
        assert_eq!(TableView::check_state(None), 0);
    }

    use docklet_app::present::{TABS, table_view};
    use docklet_app::{ListModel, ResourceKind};

    /// The DPI of 100%, 125%, 150%, 175%, 200% and 250% display scaling.
    const SCALES: [u32; 6] = [96, 120, 144, 168, 192, 240];
    /// The window's default and smallest width, in device-independent
    /// pixels, as the shell creates and limits it.
    const DEFAULT_WINDOW: i32 = 1100;
    const SMALLEST_WINDOW: i32 = 640;

    /// The width of the list in a window `window` wide: less the window's
    /// frame (8 on each side), the margins of the layout (12 on each side)
    /// and, with more rows than fit, the vertical scroll bar (17).
    fn list_width(window: i32, scroll_bar: bool, dpi: Dpi) -> i32 {
        let bar = if scroll_bar { dpi.px(17) } else { 0 };
        dpi.px(window - 16 - 24) - bar
    }

    /// A table of `kind` as the application sends it.
    fn table_of(kind: ResourceKind) -> Table {
        table_view(&ListModel::new(kind), true, None)
    }

    /// The button places of `table` with the widths the list's font gives
    /// them: Segoe UI 9 pt, measured at 100% and a little on the generous
    /// side, growing with the DPI as the font does.
    fn buttons_of(table: &Table, dpi: Dpi) -> Vec<(i32, u8)> {
        let width = |label: &str| {
            let at_100 = match label {
                "Start" | "Logs" => 28,
                "Stop" => 27,
                "Open" => 33,
                "Remove" => 48,
                other => 8 * other.chars().count() as i32,
            };
            dpi.px(at_100) + 1
        };
        let slots = lines::button_slots(table);
        slots
            .iter()
            .map(|slot| {
                let labels = slot
                    .iter()
                    .map(|&action| table.actions[action].label.as_str());
                let widest = labels.map(width).max().unwrap_or(0);
                let fold = table.actions[slot[0]].fold;
                (widest + 2 * dpi.px(BUTTON_PADDING), fold)
            })
            .collect()
    }

    fn fit_of(table: &Table, available: i32, dpi: Dpi) -> Fit {
        table_fit(available, &table.columns, &[], &buttons_of(table, dpi), dpi)
    }

    /// The same with the widths `fixed` set by the user.
    fn fit_fixed(table: &Table, available: i32, fixed: &[Option<i32>], dpi: Dpi) -> Fit {
        table_fit(
            available,
            &table.columns,
            fixed,
            &buttons_of(table, dpi),
            dpi,
        )
    }

    #[test]
    fn a_width_the_user_set_is_kept_and_the_others_fit_around_it() {
        let table = table_of(ResourceKind::Container);
        for dpi in SCALES.map(Dpi) {
            let available = list_width(DEFAULT_WINDOW, true, dpi);
            let automatic = fit_of(&table, available, dpi);
            // The image column dragged 60 wider, as `fix_after_drag` keeps
            // it: with the name left of it as it was.
            let image = automatic.widths[1] + dpi.px(60);
            let fixed = [Some(automatic.widths[0]), Some(image)];
            let fit = fit_fixed(&table, available, &fixed, dpi);
            assert_eq!(fit.widths[0], automatic.widths[0]);
            assert_eq!(fit.widths[1], image);
            // Still as wide as the list, to the pixel: no scroll bar, no
            // gap. The columns right of the divider gave the room.
            assert_eq!(fit.total(), available, "{} DPI", dpi.0);
            let rest = |fit: &Fit| fit.widths[2..].iter().sum::<i32>();
            assert_eq!(rest(&fit), rest(&automatic) - dpi.px(60));
            // Narrower than it would be by itself works the same way.
            let narrow = [None, Some(dpi.px(60))];
            let fit = fit_fixed(&table, available, &narrow, dpi);
            assert_eq!(fit.widths[1], dpi.px(60));
            assert_eq!(fit.total(), available);
            assert!(fit.widths[0] > automatic.widths[0]);
        }
    }

    #[test]
    fn widths_the_user_set_never_make_the_list_scroll_sideways() {
        for kind in TABS {
            let table = table_of(kind);
            let columns = table.columns.len();
            for dpi in [Dpi(96), Dpi(120), Dpi(192)] {
                let menu_button_alone = button_metrics(dpi).more + 2 * button_metrics(dpi).gap;
                // One column made very wide, every column fixed, and a
                // column made as narrow as a divider allows.
                let mut wide = vec![None; columns];
                wide[0] = Some(dpi.px(900));
                let all = vec![Some(dpi.px(130)); columns];
                let mut narrow = vec![None; columns];
                narrow[columns - 1] = Some(dpi.px(40));
                for fixed in [wide, all, narrow] {
                    for available in (0..dpi.px(1600)).step_by(13) {
                        let fit = fit_fixed(&table, available, &fixed, dpi);
                        assert_eq!(fit.widths.len(), columns);
                        assert!(
                            fit.total() <= available.max(menu_button_alone),
                            "{kind:?} at {} DPI in {available} px with {fixed:?}: {fit:?}",
                            dpi.0
                        );
                        assert!(fit.widths.iter().all(|&width| width >= 0));
                    }
                }
            }
        }
    }

    #[test]
    fn a_column_the_user_sized_stays_while_the_others_give_way() {
        let table = table_of(ResourceKind::Container);
        let dpi = Dpi(96);
        // Disk I/O, the first column to go in a narrow list, sized by hand.
        let last = table.columns.len() - 1;
        let mut fixed = vec![None; table.columns.len()];
        fixed[last] = Some(120);
        let available = list_width(SMALLEST_WINDOW, true, dpi);
        let fit = fit_fixed(&table, available, &fixed, dpi);
        assert_eq!(fit.widths[last], 120);
        assert_eq!(fit.total(), available);
        // The name is still there, and other columns went in its place.
        assert!(fit.widths[0] > 0);
        assert!(fit.widths[1..last].contains(&0));
    }

    /// The titles of the columns that show, and the labels of the actions
    /// that have a button.
    fn showing(table: &Table, fit: &Fit) -> (Vec<String>, Vec<String>) {
        let columns = table.columns.iter().zip(&fit.widths);
        let titles = columns
            .filter(|&(_, &width)| width > 0)
            .map(|(column, _)| column.title.clone())
            .collect();
        let slots = lines::button_slots(table);
        let labels = fit.buttons.shown.iter().map(|&(slot, _)| {
            let actions = slots[slot]
                .iter()
                .map(|&action| table.actions[action].label.as_str());
            actions.collect::<Vec<_>>().join("/")
        });
        (titles, labels.collect())
    }

    #[test]
    fn no_tab_is_ever_wider_than_its_list() {
        for kind in TABS {
            let table = table_of(kind);
            for dpi in SCALES.map(Dpi) {
                let menu_button_alone = button_metrics(dpi).more + 2 * button_metrics(dpi).gap;
                for available in (0..dpi.px(1600)).step_by(7) {
                    let fit = fit_of(&table, available, dpi);
                    assert_eq!(fit.widths.len(), table.columns.len());
                    assert!(
                        fit.total() <= available.max(menu_button_alone),
                        "{kind:?} at {} DPI in {available} px: {fit:?}",
                        dpi.0
                    );
                    // From a width any window has, the columns fill the
                    // list exactly: no gap at the right either.
                    if available >= dpi.px(300) {
                        assert_eq!(fit.total(), available, "{kind:?} {available}");
                    }
                    // The name is there at every width.
                    if available > menu_button_alone {
                        assert!(fit.widths[0] > 0, "{kind:?} {available}");
                    }
                }
            }
        }
    }

    #[test]
    fn at_the_default_size_every_column_and_every_button_shows() {
        for kind in TABS {
            let table = table_of(kind);
            for dpi in SCALES.map(Dpi) {
                for scroll_bar in [false, true] {
                    let available = list_width(DEFAULT_WINDOW, scroll_bar, dpi);
                    let fit = fit_of(&table, available, dpi);
                    let at = format!("{kind:?} at {} DPI, scroll bar {scroll_bar}", dpi.0);
                    assert_eq!(fit.total(), available, "{at}");
                    assert!(!fit.buttons.more, "{at}: a button is folded");
                    assert_eq!(fit.buttons.shown.len(), buttons_of(&table, dpi).len());
                    for (index, (column, &width)) in
                        table.columns.iter().zip(&fit.widths).enumerate()
                    {
                        let checkbox = if index == 0 { dpi.px(CHECKBOX_ROOM) } else { 0 };
                        let least = dpi.px(i32::from(column.min_width)) + checkbox;
                        assert!(width >= least, "{at}: {} has {width}", column.title);
                    }
                }
            }
        }
    }

    #[test]
    fn at_the_smallest_size_the_containers_keep_what_tells_them_apart() {
        let table = table_of(ResourceKind::Container);
        for dpi in SCALES.map(Dpi) {
            for scroll_bar in [false, true] {
                let available = list_width(SMALLEST_WINDOW, scroll_bar, dpi);
                let fit = fit_of(&table, available, dpi);
                let at = format!("{} DPI, scroll bar {scroll_bar}", dpi.0);
                assert_eq!(fit.total(), available, "{at}");
                let (columns, buttons) = showing(&table, &fit);
                // The figures go; Start or Stop and Logs stay one click
                // away, and the menu button stands for Open and Remove.
                assert_eq!(columns, ["Name", "Image", "State", "Ports"], "{at}");
                assert_eq!(buttons, ["Start/Stop", "Logs"], "{at}");
                assert!(fit.buttons.more, "{at}");
            }
        }
    }

    #[test]
    fn at_the_smallest_size_the_other_tabs_show_everything() {
        for kind in [
            ResourceKind::Image,
            ResourceKind::Volume,
            ResourceKind::Network,
        ] {
            let table = table_of(kind);
            for dpi in SCALES.map(Dpi) {
                let available = list_width(SMALLEST_WINDOW, true, dpi);
                let fit = fit_of(&table, available, dpi);
                let (columns, buttons) = showing(&table, &fit);
                assert_eq!(columns.len(), table.columns.len(), "{kind:?} {}", dpi.0);
                assert_eq!(buttons, ["Remove"], "{kind:?} {}", dpi.0);
                assert!(!fit.buttons.more);
                assert_eq!(fit.total(), available);
            }
        }
    }

    #[test]
    fn the_containers_lose_columns_and_buttons_in_a_sensible_order() {
        let table = table_of(ResourceKind::Container);
        let dpi = Dpi(96);
        let mut seen: Vec<(Vec<String>, Vec<String>)> = Vec::new();
        for available in (300..1100).rev() {
            let now = showing(&table, &fit_of(&table, available, dpi));
            if seen.last() != Some(&now) {
                seen.push(now);
            }
        }
        let steps: Vec<String> = seen
            .iter()
            .map(|(columns, buttons)| format!("{} | {}", columns.join(" "), buttons.join(" ")))
            .collect();
        assert_eq!(
            steps,
            [
                "Name Image State Ports CPU Memory Net I/O Disk I/O | Start/Stop Logs Open Remove",
                "Name Image State Ports CPU Memory Net I/O | Start/Stop Logs Open Remove",
                "Name Image State Ports CPU Memory | Start/Stop Logs Open Remove",
                "Name Image State Ports CPU Memory | Start/Stop Logs Open",
                "Name Image State Ports CPU Memory | Start/Stop Logs",
                "Name Image State Ports CPU | Start/Stop Logs",
                "Name Image State Ports | Start/Stop Logs",
                "Name Image State | Start/Stop Logs",
                "Name State | Start/Stop Logs",
                "Name State | Start/Stop",
                "Name | Start/Stop",
            ]
        );
    }
}
