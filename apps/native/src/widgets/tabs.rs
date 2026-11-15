//! The tab strip: a stock tab control, which keeps hit-testing, the arrow
//! keys and accessibility, painted in the theme's colours.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DC_BRUSH, DT_CENTER, DT_SINGLELINE, DT_VCENTER, DrawFocusRect, DrawTextW, FillRect,
    GetStockObject, HBRUSH, HDC, SelectObject, SetBkMode, SetDCBrushColor, SetTextColor,
    TRANSPARENT,
};
use windows::Win32::UI::Controls::{
    TCIF_TEXT, TCITEMW, TCM_DELETEITEM, TCM_GETCURSEL, TCM_GETITEMCOUNT, TCM_GETITEMRECT,
    TCM_INSERTITEMW, TCM_SETCURSEL, TCM_SETITEMW, WC_TABCONTROLW,
};
use windows::Win32::UI::WindowsAndMessaging::WS_TABSTOP;

use crate::theme::{Fonts, Theme};
use crate::win::{Dpi, child, client_rect, send, send_ptr};

pub struct TabStrip {
    pub hwnd: HWND,
}

impl TabStrip {
    pub fn create(parent: HWND, id: usize) -> Self {
        Self {
            hwnd: child(parent, WC_TABCONTROLW, "", WS_TABSTOP.0, 0, id),
        }
    }

    fn count(&self) -> usize {
        send(self.hwnd, TCM_GETITEMCOUNT, 0, 0).max(0) as usize
    }

    /// Makes the tabs read `titles`, reusing the items it has so the
    /// selection stays put.
    pub fn set_titles(&self, titles: &[String]) {
        let count = self.count();
        for (index, title) in titles.iter().enumerate() {
            let mut text: Vec<u16> = title.encode_utf16().chain([0]).collect();
            let mut item = TCITEMW {
                mask: TCIF_TEXT,
                pszText: windows::core::PWSTR(text.as_mut_ptr()),
                ..Default::default()
            };
            let message = if index < count {
                TCM_SETITEMW
            } else {
                TCM_INSERTITEMW
            };
            send_ptr(self.hwnd, message, index, &mut item);
        }
        for index in (titles.len()..count).rev() {
            send(self.hwnd, TCM_DELETEITEM, index, 0);
        }
    }

    pub fn selected(&self) -> usize {
        send(self.hwnd, TCM_GETCURSEL, 0, 0).max(0) as usize
    }

    /// Selects tab `index` without the change notification.
    pub fn select(&self, index: usize) {
        send(self.hwnd, TCM_SETCURSEL, index, 0);
    }

    fn item_rect(&self, index: usize) -> RECT {
        let mut rect = RECT::default();
        send_ptr(self.hwnd, TCM_GETITEMRECT, index, &mut rect);
        rect
    }

    pub fn paint(&self, hdc: HDC, theme: &Theme, fonts: &Fonts, dpi: Dpi, focused: bool) {
        let area = client_rect(self.hwnd);
        let selected = self.selected();
        // SAFETY: `hdc` is the control's paint DC; the font is selected back
        // out and the DC brush is a stock object.
        unsafe {
            FillRect(hdc, &area, theme.background_brush);
            let old_font = SelectObject(hdc, fonts.ui.into());
            SetBkMode(hdc, TRANSPARENT);
            for index in 0..self.count() {
                let mut rect = self.item_rect(index);
                let current = index == selected;
                SetTextColor(hdc, if current { theme.text } else { theme.muted });
                let mut label = [0u16; 128];
                let mut item = TCITEMW {
                    mask: TCIF_TEXT,
                    pszText: windows::core::PWSTR(label.as_mut_ptr()),
                    cchTextMax: label.len() as i32,
                    ..Default::default()
                };
                send_ptr(
                    self.hwnd,
                    windows::Win32::UI::Controls::TCM_GETITEMW,
                    index,
                    &mut item,
                );
                let length = label
                    .iter()
                    .position(|&unit| unit == 0)
                    .unwrap_or(label.len());
                DrawTextW(
                    hdc,
                    &mut label[..length],
                    &mut rect,
                    DT_CENTER | DT_VCENTER | DT_SINGLELINE,
                );
                if current {
                    let mark = RECT {
                        top: rect.bottom - dpi.px(3),
                        left: rect.left + dpi.px(8),
                        right: rect.right - dpi.px(8),
                        ..rect
                    };
                    SetDCBrushColor(hdc, theme.accent);
                    FillRect(hdc, &mark, HBRUSH(GetStockObject(DC_BRUSH).0));
                    if focused {
                        let focus = RECT {
                            left: rect.left + dpi.px(2),
                            top: rect.top + dpi.px(2),
                            right: rect.right - dpi.px(2),
                            bottom: rect.bottom - dpi.px(4),
                        };
                        SetTextColor(hdc, theme.text);
                        let _ = DrawFocusRect(hdc, &focus);
                    }
                }
            }
            SelectObject(hdc, old_font);
        }
    }
}
