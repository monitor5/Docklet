//! The usage strip: meters side by side, each a label, a value and a bar.
//!
//! Each meter is a stock static control that the window draws
//! (`SS_OWNERDRAW`). The looks are the window's, and to a screen reader the
//! strip is a row of texts, one per meter, each saying its whole reading:
//! `Memory 2.1 GB`, `GPU Unknown (no counters)`.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DC_BRUSH, DC_PEN, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DrawTextW,
    FillRect, GetStockObject, HDC, RoundRect, SelectObject, SetBkMode, SetDCBrushColor,
    SetDCPenColor, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, WC_STATICW};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
};

use crate::boundary::Meter;
use crate::theme::{Fonts, Theme};
use crate::win::{Dpi, child, invalidate, place, set_text, show};

/// The window draws the control (`WM_DRAWITEM`).
const SS_OWNERDRAW: u32 = 0x000d;
/// A click is reported to the window, which says what the meters mean.
const SS_NOTIFY: u32 = 0x0100;

pub struct UsageStrip {
    parent: HWND,
    id: usize,
    /// The control the meters follow in the window's order of controls.
    after: HWND,
    /// One control per meter, with the meter it shows now.
    meters: RefCell<Vec<(HWND, Meter)>>,
}

/// Fill at which a bar turns to the warning colour.
const HIGH: f32 = 0.9;

/// Left edge and width of each of `count` meters across `width` pixels.
pub fn slots(width: i32, count: usize, gap: i32) -> Vec<(i32, i32)> {
    if count == 0 {
        return Vec::new();
    }
    let count = count as i32;
    let each = ((width - gap * (count - 1)) / count).max(0);
    (0..count)
        .map(|index| (index * (each + gap), each))
        .collect()
}

/// Pixels of a bar `width` wide that a fill covers; out-of-range and NaN
/// fills are clamped.
pub fn fill_width(fill: f32, width: i32) -> i32 {
    let fill = if fill.is_nan() {
        0.0
    } else {
        fill.clamp(0.0, 1.0)
    };
    (fill * width as f32).round() as i32
}

/// A meter read aloud: `Memory 2.1 GB`. One without a figure says why:
/// `GPU Unknown (no counters)`.
pub fn reading(meter: &Meter) -> String {
    if meter.known || meter.detail.is_empty() {
        format!("{} {}", meter.label, meter.text)
    } else {
        format!("{} {} ({})", meter.label, meter.text, meter.detail)
    }
}

impl UsageStrip {
    /// A strip without meters; [`Self::set`] makes their controls, as
    /// children of `parent` with the control number `id`. They are made
    /// when figures arrive, long after the window's other controls, and
    /// take their place behind `after`: a screen reader goes through a
    /// window's controls in their order, which should be that on screen.
    pub fn create(parent: HWND, id: usize, after: HWND) -> Self {
        Self {
            parent,
            id,
            after,
            meters: RefCell::new(Vec::new()),
        }
    }

    pub fn count(&self) -> usize {
        self.meters.borrow().len()
    }

    /// Makes the strip show `meters`. Only a meter that changed is drawn
    /// again and renamed. Returns whether the number of meters changed, and
    /// with it the layout.
    pub fn set(&self, meters: &[Meter]) -> bool {
        // A control draws itself while it is given a name, which reads the
        // meters here: they are never borrowed across a call into Win32.
        let before = self.count();
        let surplus: Vec<HWND> = {
            let mut shown = self.meters.borrow_mut();
            let surplus = shown.drain(meters.len().min(before)..);
            surplus.map(|(hwnd, _)| hwnd).collect()
        };
        for hwnd in surplus {
            // SAFETY: a control this strip made, of this thread.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }
        }
        for (index, meter) in meters.iter().enumerate() {
            // The control, if there is one and it shows something else, and
            // whether what it says differs or only how it looks.
            let stale = {
                let shown = self.meters.borrow();
                match shown.get(index) {
                    Some((_, current)) if current == meter => continue,
                    Some((hwnd, current)) => Some((*hwnd, reading(current) != reading(meter))),
                    None => None,
                }
            };
            match stale {
                Some((hwnd, renamed)) => {
                    self.meters.borrow_mut()[index].1 = meter.clone();
                    // The name only when it differs: a figure that moved is
                    // news to a screen reader, a bar that moved is not.
                    if renamed {
                        set_text(hwnd, &reading(meter));
                    }
                    invalidate(hwnd);
                }
                None => {
                    let style = SS_OWNERDRAW | SS_NOTIFY;
                    let hwnd = child(self.parent, WC_STATICW, &reading(meter), style, 0, self.id);
                    let previous = self.meters.borrow().last().map(|(hwnd, _)| *hwnd);
                    // SAFETY: both are children of the same window; only
                    // the order among its controls changes.
                    unsafe {
                        let _ = SetWindowPos(
                            hwnd,
                            Some(previous.unwrap_or(self.after)),
                            0,
                            0,
                            0,
                            0,
                            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                        );
                    }
                    self.meters.borrow_mut().push((hwnd, meter.clone()));
                }
            }
        }
        before != meters.len()
    }

    pub fn set_visible(&self, visible: bool) {
        for (hwnd, _) in self.meters.borrow().iter() {
            show(*hwnd, visible);
        }
    }

    /// Puts the meters side by side in a strip of `width` by `height` at
    /// `(left, top)`.
    pub fn layout(&self, left: i32, top: i32, width: i32, height: i32, dpi: Dpi) {
        let meters = self.meters.borrow();
        let places = slots(width, meters.len(), dpi.px(16));
        for ((hwnd, _), (offset, width)) in meters.iter().zip(places) {
            place(*hwnd, left + offset, top, width, height);
        }
    }

    /// Draws the meter whose control asks for it. Returns false if the
    /// control is none of the strip's.
    pub fn draw(&self, item: &DRAWITEMSTRUCT, theme: &Theme, fonts: &Fonts, dpi: Dpi) -> bool {
        let meters = self.meters.borrow();
        let Some((_, meter)) = meters.iter().find(|(hwnd, _)| *hwnd == item.hwndItem) else {
            return false;
        };
        paint(item.hDC, item.rcItem, meter, theme, fonts, dpi);
        true
    }
}

/// One meter in `area`: its label and value on a line, its bar under it.
fn paint(hdc: HDC, area: RECT, meter: &Meter, theme: &Theme, fonts: &Fonts, dpi: Dpi) {
    let bar = dpi.px(6);
    let text_height = dpi.px(20);
    let corner = bar;
    let (left, width) = (area.left, area.right - area.left);
    // SAFETY: `hdc` is the control's DC for this drawing; the stock DC pen
    // and brush and the font are selected back out.
    unsafe {
        FillRect(hdc, &area, theme.background_brush);
        let old_font = SelectObject(hdc, fonts.ui.into());
        let old_pen = SelectObject(hdc, GetStockObject(DC_PEN));
        let old_brush = SelectObject(hdc, GetStockObject(DC_BRUSH));
        SetBkMode(hdc, TRANSPARENT);
        let top = area.top + dpi.px(4);
        let mut label: Vec<u16> = meter.label.encode_utf16().collect();
        let mut label_rect = RECT {
            left,
            top,
            right: left + width,
            bottom: top + text_height,
        };
        SetTextColor(hdc, theme.muted);
        DrawTextW(
            hdc,
            &mut label,
            &mut label_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );

        let known = meter.known;
        let fill = meter.fill.unwrap_or(0.0);
        let mut value: Vec<u16> = meter.text.encode_utf16().collect();
        // The value takes what the label leaves, ellipsised.
        let mut value_rect = RECT {
            left: left + width / 3,
            ..label_rect
        };
        SetTextColor(hdc, if known { theme.text } else { theme.muted });
        DrawTextW(
            hdc,
            &mut value,
            &mut value_rect,
            DT_RIGHT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

        let bar_top = top + text_height + dpi.px(4);
        SetDCPenColor(hdc, if known { theme.track } else { theme.border });
        SetDCBrushColor(hdc, if known { theme.track } else { theme.background });
        let _ = RoundRect(
            hdc,
            left,
            bar_top,
            left + width,
            bar_top + bar,
            corner,
            corner,
        );
        let filled = fill_width(fill, width);
        if known && filled > 0 {
            let color = if fill >= HIGH {
                theme.warning
            } else {
                theme.accent
            };
            SetDCPenColor(hdc, color);
            SetDCBrushColor(hdc, color);
            let _ = RoundRect(
                hdc,
                left,
                bar_top,
                left + filled.max(bar),
                bar_top + bar,
                corner,
                corner,
            );
        }
        SelectObject(hdc, old_brush);
        SelectObject(hdc, old_pen);
        SelectObject(hdc, old_font);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meters_share_the_width_with_gaps() {
        assert_eq!(slots(310, 3, 5), vec![(0, 100), (105, 100), (210, 100)]);
        assert!(slots(300, 0, 5).is_empty());
        assert_eq!(slots(4, 3, 5), vec![(0, 0), (5, 0), (10, 0)]);
    }

    #[test]
    fn fills_are_clamped() {
        assert_eq!(fill_width(0.5, 200), 100);
        assert_eq!(fill_width(1.7, 200), 200);
        assert_eq!(fill_width(-1.0, 200), 0);
        assert_eq!(fill_width(f32::NAN, 200), 0);
    }

    #[test]
    fn each_meter_reads_its_figure_or_why_it_has_none() {
        let memory = Meter {
            label: "Memory".into(),
            text: "2.1 GB".into(),
            fill: None,
            known: true,
            detail: "Memory of the WSL VM.".into(),
        };
        assert_eq!(reading(&memory), "Memory 2.1 GB");
        let gpu = Meter {
            label: "GPU".into(),
            text: "Unknown".into(),
            fill: None,
            known: false,
            detail: "no counters".into(),
        };
        assert_eq!(reading(&gpu), "GPU Unknown (no counters)");
        let waiting = Meter {
            detail: String::new(),
            ..gpu
        };
        assert_eq!(reading(&waiting), "GPU Unknown");
    }
}
