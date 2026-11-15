//! The table: a stock ListView in virtual mode (`LVS_OWNERDATA`), so it asks
//! for the text of the cells it shows and Narrator reads rows, checkboxes and
//! columns. The first column carries the checkboxes and the header's
//! select-all checkbox; the last column, when the table has actions, holds
//! buttons drawn over the row.

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
    LVM_GETHEADER, LVM_GETITEMRECT, LVM_GETNEXTITEM, LVM_GETSUBITEMRECT, LVM_INSERTCOLUMNW,
    LVM_SETCALLBACKMASK, LVM_SETCOLUMNWIDTH, LVM_SETEXTENDEDLISTVIEWSTYLE, LVM_SETIMAGELIST,
    LVM_SETITEMCOUNT, LVM_SETITEMSTATE, LVM_SETSELECTIONMARK, LVM_SUBITEMHITTEST, LVNI_FOCUSED,
    LVS_EX_CHECKBOXES, LVS_EX_DOUBLEBUFFER, LVS_EX_FULLROWSELECT, LVS_EX_LABELTIP, LVS_OWNERDATA,
    LVS_REPORT, LVS_SHOWSELALWAYS, LVS_SINGLESEL, LVSICF_NOSCROLL, LVSIL_SMALL, WC_LISTVIEWW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_STATECHANGE, GWL_STYLE, GetWindowLongPtrW, OBJID_CLIENT, SetWindowLongPtrW,
    WS_TABSTOP,
};

use super::columns::{ButtonRow, Rect, distribute};
use super::table_state::{CheckState, Sort};
use crate::boundary::{Align, Table};
use crate::theme::Theme;
use crate::win::{Dpi, child, client_rect, send, send_ptr};

/// Row height in device-independent pixels; the row buttons need the room.
const ROW_HEIGHT: i32 = 30;
/// Room the checkbox takes in the first column.
const CHECKBOX_ROOM: i32 = 28;

/// Where a point landed in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub item: usize,
    pub column: usize,
    pub on_checkbox: bool,
}

#[derive(Default, PartialEq)]
struct Built {
    columns: Vec<crate::boundary::Column>,
    actions: Vec<String>,
    dpi: u32,
}

pub struct TableView {
    pub list: HWND,
    pub header: HWND,
    built: RefCell<Built>,
    buttons: Cell<ButtonRow>,
    images: Cell<HIMAGELIST>,
}

impl Drop for TableView {
    fn drop(&mut self) {
        // SAFETY: the list is gone by the time the shell drops, and a list
        // does not destroy image lists it was handed.
        unsafe {
            let _ = ImageList_Destroy(Some(self.images.get()));
        }
    }
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
        // SAFETY: adding a style bit to the list's own header.
        unsafe {
            let style = GetWindowLongPtrW(header, GWL_STYLE);
            SetWindowLongPtrW(header, GWL_STYLE, style | HDS_CHECKBOXES as isize);
        }
        Self {
            list,
            header,
            built: RefCell::new(Built::default()),
            buttons: Cell::new(ButtonRow {
                width: 60,
                gap: 6,
                inset: 3,
            }),
            images: Cell::new(HIMAGELIST::default()),
        }
    }

    /// A row is as tall as its small image, so an empty image list of the
    /// right height sets the row height.
    pub fn set_dpi(&self, dpi: Dpi) {
        // SAFETY: the new list is handed to the control; the old one is
        // destroyed after the control lets go of it.
        unsafe {
            let images = ImageList_Create(1, dpi.px(ROW_HEIGHT), ILC_COLOR32, 1, 0);
            send(self.list, LVM_SETIMAGELIST, LVSIL_SMALL as usize, images.0);
            let _ = ImageList_Destroy(Some(self.images.replace(images)));
        }
    }

    pub fn buttons(&self) -> ButtonRow {
        self.buttons.get()
    }

    /// Index of the actions column, if the table shows one.
    pub fn actions_column(&self) -> Option<usize> {
        let built = self.built.borrow();
        (!built.actions.is_empty()).then_some(built.columns.len())
    }

    /// Rebuilds the columns when the table's columns, its actions or the
    /// DPI differ from what the list has. Returns whether it did.
    pub fn build_columns(&self, table: &Table, dpi: Dpi, font: HFONT) -> bool {
        let wanted = Built {
            columns: table.columns.clone(),
            actions: table
                .actions
                .iter()
                .map(|action| action.label.clone())
                .collect(),
            dpi: dpi.0,
        };
        if *self.built.borrow() == wanted {
            return false;
        }
        // Each call deletes one column and fails once none is left. The
        // count is bounded either way.
        for _ in 0..64 {
            if send(self.list, LVM_DELETECOLUMN, 0, 0) == 0 {
                break;
            }
        }
        let label_width = measure(self.list, font, &wanted.actions);
        self.buttons.set(ButtonRow {
            width: label_width + dpi.px(20),
            gap: dpi.px(6),
            inset: dpi.px(4),
        });
        let titles = wanted
            .columns
            .iter()
            .map(|column| (column.title.as_str(), column.align))
            .chain((!wanted.actions.is_empty()).then_some(("Actions", Align::Left)));
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
        true
    }

    /// Spreads the columns over the list's width by weight.
    pub fn fit_columns(&self, dpi: Dpi) {
        let widths = {
            let built = self.built.borrow();
            let actions = if built.actions.is_empty() {
                0
            } else {
                self.buttons.get().column_width(built.actions.len())
            };
            let available = client_rect(self.list).right - actions;
            let specs: Vec<(u16, i32)> = built
                .columns
                .iter()
                .enumerate()
                .map(|(index, column)| {
                    let extra = if index == 0 { dpi.px(CHECKBOX_ROOM) } else { 0 };
                    (column.weight, dpi.px(i32::from(column.min_width)) + extra)
                })
                .collect();
            let mut widths = distribute(available, &specs);
            if actions > 0 {
                widths.push(actions);
            }
            widths
        };
        for (index, width) in widths.into_iter().enumerate() {
            send(self.list, LVM_SETCOLUMNWIDTH, index, width as isize);
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
    pub fn update_header(&self, check: CheckState, sort: Option<Sort>) {
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
                if check == CheckState::All {
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

    /// Paints the buttons of one row's actions cell.
    pub fn paint_buttons(
        &self,
        hdc: HDC,
        cell: Rect,
        labels: &[(&str, bool)],
        theme: &Theme,
        dpi: Dpi,
    ) {
        let row = self.buttons.get();
        let corner = dpi.px(6);
        // SAFETY: `hdc` is the list's paint DC for the duration of the
        // notification; the stock DC pen and brush are selected back out.
        unsafe {
            let pen = SelectObject(hdc, GetStockObject(DC_PEN));
            let brush = SelectObject(hdc, GetStockObject(DC_BRUSH));
            SetDCPenColor(hdc, theme.border);
            SetDCBrushColor(hdc, theme.button);
            SetBkMode(hdc, TRANSPARENT);
            for (index, &(label, enabled)) in labels.iter().enumerate() {
                let rect = row.button(cell, index);
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

    /// The state bits for a checkbox.
    pub fn check_state(checked: bool) -> u32 {
        state_image(if checked { 2 } else { 1 })
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
        assert_eq!(TableView::check_state(false), 0x1000);
        assert_eq!(TableView::check_state(true), 0x2000);
        assert_eq!(TableView::check_state(true) & !LVIS_STATEIMAGEMASK.0, 0);
    }
}
