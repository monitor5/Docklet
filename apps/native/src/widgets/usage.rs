//! The usage strip: meters side by side, each a label, a value and a bar.
//!
//! Each meter is a stock static control that the window draws
//! (`SS_OWNERDRAW`). The looks are the window's, and to a screen reader the
//! strip is a row of texts, one per meter, each saying its whole reading:
//! `Memory 2.1 GB`, `GPU Unknown (no counters)`.
//!
//! [`arrange`] is the strip's whole layout. Every meter is at least as wide
//! as its label and its value side by side, so the two never overlap, and
//! meters never overlap each other. In a window too narrow for all of them
//! the last ones are left out of the strip; a click on the strip still
//! tells about every one.

use std::cell::{Cell, RefCell};

use windows::Win32::Foundation::{HWND, RECT, SIZE};
use windows::Win32::Graphics::Gdi::{
    DC_BRUSH, DC_PEN, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DrawTextW,
    FillRect, GetDC, GetStockObject, GetTextExtentPoint32W, HDC, HFONT, ReleaseDC, RoundRect,
    SelectObject, SetBkMode, SetDCBrushColor, SetDCPenColor, SetTextColor, TRANSPARENT,
};
use windows::Win32::UI::Controls::{DRAWITEMSTRUCT, WC_STATICW};
use windows::Win32::UI::WindowsAndMessaging::{
    DestroyWindow, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetWindowPos,
};

use super::columns::distribute;
use crate::boundary::Meter;
use crate::theme::{Fonts, Theme};
use crate::win::{Dpi, child, invalidate, place, set_text, show};

/// The window draws the control (`WM_DRAWITEM`).
const SS_OWNERDRAW: u32 = 0x000d;
/// A click is reported to the window, which says what the meters mean.
const SS_NOTIFY: u32 = 0x0100;

/// Room between two meters, in device-independent pixels.
const GAP: i32 = 16;
/// Room between a meter's label and its value.
const TEXT_GAP: i32 = 8;
/// A figure about as wide as figures get. Every meter has room for it, so
/// the strip does not shift when `9%` becomes `10%` or `980 MB` `1.1 GB`;
/// only a word wider than this (`Not measured`) asks for more.
const WIDE_FIGURE: &str = "00.0 GB";

/// Where a meter stands in the strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Slot {
    /// From the strip's left edge.
    pub left: i32,
    pub width: i32,
}

/// How wide a meter must be for its label and its value to stand side by
/// side: `label` and `value` are their widths, `reserve` the room every
/// value gets whatever it says now, `gap` what lies between the two.
pub fn need(label: i32, value: i32, reserve: i32, gap: i32) -> i32 {
    label.max(0) + gap.max(0) + value.max(reserve).max(0)
}

/// Lays meters that need `needs` pixels each out in a strip `width` wide,
/// with `gap` between two of them. One entry per meter: where it stands, or
/// `None` for one that is left out.
///
/// While all fit they share the width, as equal as their needs allow: a
/// meter that needs more than an equal share gets what it needs, and the
/// others share the rest. When they do not all fit, the last ones are left
/// out, one at a time, until the rest fit: they are the ones a user misses
/// least, and a click on the strip still tells about them. The first meter
/// always shows, cut short if the strip is narrower than it.
///
/// Whatever the width, no two meters share a pixel, none reaches past the
/// strip, and every meter that shows next to another is as wide as it needs.
pub fn arrange(width: i32, needs: &[i32], gap: i32) -> Vec<Option<Slot>> {
    let width = width.max(0);
    let gap = gap.max(0);
    let needed = |count: usize| -> i64 {
        let meters: i64 = needs[..count]
            .iter()
            .map(|&need| i64::from(need.max(0)))
            .sum();
        meters + i64::from(gap) * (count as i64 - 1).max(0)
    };
    let mut shown = needs.len();
    // Ends within one pass per meter.
    while shown > 1 && needed(shown) > i64::from(width) {
        shown -= 1;
    }
    if shown == 0 {
        return Vec::new();
    }
    let pool = (width - gap * (shown as i32 - 1)).max(0);
    let specs: Vec<(u16, i32)> = needs[..shown]
        .iter()
        .map(|&need| (1, need.max(0)))
        .collect();
    let mut widths = distribute(pool, &specs);
    // Only a meter that stands alone can need more than there is.
    if shown == 1 {
        widths[0] = widths[0].min(pool);
    }
    let mut left = 0;
    let mut slots: Vec<Option<Slot>> = widths
        .into_iter()
        .map(|width| {
            let slot = Slot { left, width };
            left += width + gap;
            Some(slot)
        })
        .collect();
    slots.resize(needs.len(), None);
    slots
}

/// Where a meter `width` wide draws its texts: the right edge of the label
/// and the left edge of the value, from the meter's left. The value starts
/// a gap after the label, so the two never share a pixel; in a meter too
/// narrow for both, the label keeps its room and the value is cut short.
pub fn text_split(width: i32, label: i32, gap: i32) -> (i32, i32) {
    let width = width.max(0);
    let label_right = label.clamp(0, width);
    (label_right, (label_right + gap.max(0)).min(width))
}

/// A place in the window: left, top, width, height.
type Place = (i32, i32, i32, i32);

pub struct UsageStrip {
    parent: HWND,
    id: usize,
    /// The control the meters follow in the window's order of controls.
    after: HWND,
    /// One control per meter, with the meter it shows now.
    meters: RefCell<Vec<(HWND, Meter)>>,
    /// Where the window last put the strip.
    bounds: Cell<Place>,
    /// Where each meter's control was last put, in the window; `None` for
    /// a meter that is left out. Only changes are sent to the controls.
    placed: RefCell<Vec<Option<Place>>>,
    /// The strip shows: its page is up and there are figures.
    visible: Cell<bool>,
}

/// Fill at which a bar turns to the warning colour.
const HIGH: f32 = 0.9;

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

/// Width of `text` in the font selected into `hdc`.
fn text_width(hdc: HDC, text: &str) -> i32 {
    let units: Vec<u16> = text.encode_utf16().collect();
    let mut size = SIZE::default();
    // SAFETY: `units` and `size` outlive the call.
    unsafe {
        let _ = GetTextExtentPoint32W(hdc, &units, &mut size);
    }
    size.cx
}

/// What each of `meters` needs in `font` at `dpi` (see [`need`]). `window`
/// is any window of this thread, to measure for.
pub(crate) fn needs_of(window: HWND, meters: &[Meter], font: HFONT, dpi: Dpi) -> Vec<i32> {
    // SAFETY: the DC is released and the font selected back out; measuring
    // sends no message to any window.
    unsafe {
        let hdc = GetDC(Some(window));
        let previous = SelectObject(hdc, font.into());
        let reserve = text_width(hdc, WIDE_FIGURE);
        let gap = dpi.px(TEXT_GAP);
        let needs = meters
            .iter()
            .map(|meter| {
                need(
                    text_width(hdc, &meter.label),
                    text_width(hdc, &meter.text),
                    reserve,
                    gap,
                )
            })
            .collect();
        SelectObject(hdc, previous);
        ReleaseDC(Some(window), hdc);
        needs
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
            bounds: Cell::new((0, 0, 0, 0)),
            placed: RefCell::new(Vec::new()),
            visible: Cell::new(false),
        }
    }

    pub fn count(&self) -> usize {
        self.meters.borrow().len()
    }

    /// Makes the strip show `meters`. Only a meter that changed is drawn
    /// again and renamed, and the meters only move if one of them needs
    /// more or less room than it had. Returns whether the number of meters
    /// changed, and with it the layout of the window.
    pub fn set(&self, meters: &[Meter], font: HFONT, dpi: Dpi) -> bool {
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
        let resized = before != meters.len();
        if resized {
            // Other controls than before: none of them is where it was.
            // The window lays the strip out again.
            self.placed.borrow_mut().clear();
        } else {
            // `Measuring…` became a figure, or the other way round.
            self.rearrange(font, dpi);
        }
        resized
    }

    /// Shows the strip, or hides it. A meter that is left out of the strip
    /// stays hidden either way.
    pub fn set_visible(&self, visible: bool) {
        self.visible.set(visible);
        let controls: Vec<HWND> = self.meters.borrow().iter().map(|(hwnd, _)| *hwnd).collect();
        let placed = self.placed.borrow().clone();
        for (index, hwnd) in controls.into_iter().enumerate() {
            let fits = placed.get(index).is_some_and(Option::is_some);
            show(hwnd, visible && fits);
        }
    }

    /// Puts the meters side by side in a strip of `width` by `height` at
    /// `(left, top)` (see [`arrange`]).
    pub fn layout(&self, left: i32, top: i32, width: i32, height: i32, dpi: Dpi, font: HFONT) {
        self.bounds.set((left, top, width, height));
        self.rearrange(font, dpi);
    }

    /// Puts every meter where [`arrange`] has it for what the meters say
    /// now, and moves only the controls that are somewhere else.
    fn rearrange(&self, font: HFONT, dpi: Dpi) {
        let (left, top, width, height) = self.bounds.get();
        let (controls, meters): (Vec<HWND>, Vec<Meter>) =
            self.meters.borrow().iter().cloned().unzip();
        let needs = needs_of(self.parent, &meters, font, dpi);
        let wanted: Vec<Option<Place>> = arrange(width, &needs, dpi.px(GAP))
            .into_iter()
            .map(|slot| slot.map(|slot| (left + slot.left, top, slot.width, height)))
            .collect();
        // Stored before a control is moved: it draws itself while it is.
        let before = self.placed.replace(wanted.clone());
        let visible = self.visible.get();
        for (index, (hwnd, wanted)) in controls.into_iter().zip(wanted).enumerate() {
            if before.get(index) == Some(&wanted) {
                continue;
            }
            match wanted {
                Some((x, y, width, height)) => {
                    place(hwnd, x, y, width, height);
                    show(hwnd, visible);
                }
                None => show(hwnd, false),
            }
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
        // Each text in a rectangle of its own: whatever they say, and
        // however narrow the meter, one is never drawn over the other.
        let (label_right, value_left) =
            text_split(width, text_width(hdc, &meter.label), dpi.px(TEXT_GAP));
        let mut label: Vec<u16> = meter.label.encode_utf16().collect();
        let mut label_rect = RECT {
            left,
            top,
            right: left + label_right,
            bottom: top + text_height,
        };
        SetTextColor(hdc, theme.muted);
        DrawTextW(
            hdc,
            &mut label,
            &mut label_rect,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );

        let known = meter.known;
        let fill = meter.fill.unwrap_or(0.0);
        let mut value: Vec<u16> = meter.text.encode_utf16().collect();
        let mut value_rect = RECT {
            left: left + value_left,
            top,
            right: left + width,
            bottom: top + text_height,
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

    fn slot(left: i32, width: i32) -> Option<Slot> {
        Some(Slot { left, width })
    }

    #[test]
    fn meters_share_the_width_with_gaps_while_each_has_what_it_needs() {
        assert_eq!(
            arrange(310, &[50, 50, 50], 5),
            [slot(0, 100), slot(105, 100), slot(210, 100)]
        );
        assert!(arrange(300, &[], 5).is_empty());
        // What the division leaves goes to the last one.
        assert_eq!(arrange(101, &[10, 10], 0), [slot(0, 50), slot(50, 51)]);
    }

    #[test]
    fn a_meter_that_says_more_gets_the_room_and_the_others_share_the_rest() {
        // An equal share would be 153: the third needs 200 and gets it.
        assert_eq!(
            arrange(500, &[60, 60, 200], 20),
            [slot(0, 130), slot(150, 130), slot(300, 200)]
        );
        // Exactly what they need, to the pixel.
        assert_eq!(
            arrange(360, &[60, 60, 200], 20),
            [slot(0, 60), slot(80, 60), slot(160, 200)]
        );
    }

    #[test]
    fn meters_that_do_not_fit_are_left_out_from_the_end() {
        // One pixel short of what all three need.
        assert_eq!(
            arrange(359, &[60, 60, 200], 20),
            [slot(0, 169), slot(189, 170), None]
        );
        assert_eq!(
            arrange(300, &[100, 100, 100, 150], 10),
            [slot(0, 145), slot(155, 145), None, None]
        );
        // The first one always shows, as wide as the strip if it must.
        assert_eq!(arrange(40, &[100, 100], 10), [slot(0, 40), None]);
        assert_eq!(arrange(0, &[100], 10), [slot(0, 0)]);
        assert_eq!(arrange(-5, &[100], 10), [slot(0, 0)]);
    }

    #[test]
    fn meters_never_overlap_and_never_leave_the_strip_at_any_width() {
        let sets: [&[i32]; 4] = [
            &[100, 78, 78, 78, 154],
            &[120, 98, 98, 98, 142],
            &[300],
            &[0, 0, 40],
        ];
        for needs in sets {
            for gap in [0, 16, 20] {
                for width in 0..1400 {
                    let slots = arrange(width, needs, gap);
                    assert_eq!(slots.len(), needs.len());
                    let at = format!("{needs:?} in {width} with {gap}: {slots:?}");
                    let shown: Vec<Slot> = slots.iter().copied().flatten().collect();
                    // Left out from the end only, and never all of them.
                    assert!(!shown.is_empty(), "{at}");
                    assert!(slots[..shown.len()].iter().all(Option::is_some), "{at}");
                    let mut edge = 0;
                    for (index, slot) in shown.iter().enumerate() {
                        assert!(slot.left >= edge && slot.width >= 0, "{at}");
                        edge = slot.left + slot.width + gap;
                        assert!(slot.left + slot.width <= width, "{at}");
                        // Next to another, a meter has all it needs.
                        if shown.len() > 1 {
                            assert!(slot.width >= needs[index], "{at}");
                        }
                    }
                    // While any fit, the strip is filled to its right edge.
                    let last = shown[shown.len() - 1];
                    if shown.len() > 1 || needs[0] <= width {
                        assert_eq!(last.left + last.width, width, "{at}");
                    }
                }
            }
        }
    }

    #[test]
    fn a_meter_has_room_for_a_figure_whatever_it_says_now() {
        // `9%` and `10%` are both narrower than the reserve: the same need.
        assert_eq!(need(24, 14, 46, 8), need(24, 21, 46, 8));
        assert_eq!(need(24, 14, 46, 8), 78);
        // A word that is wider asks for its own width.
        assert_eq!(need(68, 78, 46, 8), 154);
        assert_eq!(need(-3, -3, -3, -3), 0);
    }

    #[test]
    fn a_label_and_its_value_never_share_a_pixel() {
        // Room for both: the value starts a gap after the label.
        assert_eq!(text_split(154, 68, 8), (68, 76));
        for width in 0..200 {
            for label in [0, 24, 68, 150, 400] {
                let (label_right, value_left) = text_split(width, label, 8);
                assert!(0 <= label_right && label_right <= value_left && value_left <= width);
                // Apart by the gap while there is room for it.
                if label + 8 <= width {
                    assert_eq!(value_left - label_right, 8);
                }
            }
        }
        assert_eq!(text_split(-4, 10, 8), (0, 0));
    }

    fn meter(label: &str, text: &str) -> Meter {
        Meter {
            label: label.into(),
            text: text.into(),
            fill: None,
            known: true,
            detail: String::new(),
        }
    }

    /// Measures with the fonts the window uses; makes and shows no window.
    #[test]
    fn at_the_smallest_window_the_meters_of_a_running_engine_all_fit() {
        // What the strip says of a running engine, with the widest figures
        // it has, and before Docker's disk usage was asked for.
        let meters = [
            meter("Memory", "15.9 GB"),
            meter("CPU", "100%"),
            meter("GPU", "Unknown"),
            meter("Disk", "127.4 GB"),
            meter("Docker data", "Not measured"),
        ];
        // The DPI of 100% to 250% scaling, and the window's smallest width
        // in device-independent pixels, less its frame (8 on each side)
        // and the margins of the layout (12 on each side).
        for dpi in [96, 120, 144, 168, 192, 240].map(Dpi) {
            let width = dpi.px(640 - 16 - 24);
            let fonts = Fonts::new(dpi.0, HWND::default());
            let needs = needs_of(HWND::default(), &meters, fonts.ui, dpi);
            let slots = arrange(width, &needs, dpi.px(GAP));
            for (index, slot) in slots.iter().enumerate() {
                let slot = slot.unwrap_or_else(|| {
                    panic!(
                        "{} is left out at {} DPI: {needs:?} in {width}",
                        meters[index].label, dpi.0
                    )
                });
                assert!(slot.width >= needs[index]);
                assert!(slot.left + slot.width <= width);
            }
        }
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
