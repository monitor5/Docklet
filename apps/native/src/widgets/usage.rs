//! The usage strip: meters side by side, each a label, a value and a bar.
//!
//! It is painted by hand, so its window text carries the whole reading for
//! screen readers.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DC_BRUSH, DC_PEN, DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER, DrawTextW,
    FillRect, GetStockObject, HDC, RoundRect, SelectObject, SetBkMode, SetDCBrushColor,
    SetDCPenColor, SetTextColor, TRANSPARENT,
};

use crate::boundary::Meter;
use crate::theme::{Fonts, Theme};
use crate::win::{Dpi, child, set_text};

pub struct UsageStrip {
    pub hwnd: HWND,
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

/// The strip read aloud: "Memory 2.1 GB, CPU 12%, GPU Unknown (reason)".
/// A meter without a figure says why.
pub fn spoken(meters: &[Meter]) -> String {
    meters
        .iter()
        .map(|meter| {
            if meter.known || meter.detail.is_empty() {
                format!("{} {}", meter.label, meter.text)
            } else {
                format!("{} {} ({})", meter.label, meter.text, meter.detail)
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

impl UsageStrip {
    pub fn create(parent: HWND, class: windows::core::PCWSTR, id: usize) -> Self {
        Self {
            hwnd: child(parent, class, "", 0, 0, id),
        }
    }

    pub fn set(&self, meters: &[Meter]) {
        set_text(self.hwnd, &spoken(meters));
        crate::win::invalidate(self.hwnd);
    }

    pub fn paint(
        &self,
        hdc: HDC,
        area: RECT,
        meters: &[Meter],
        theme: &Theme,
        fonts: &Fonts,
        dpi: Dpi,
    ) {
        let bar = dpi.px(6);
        let text_height = dpi.px(20);
        let corner = bar;
        // SAFETY: `hdc` is this window's paint DC; the stock DC pen and
        // brush and the font are selected back out.
        unsafe {
            FillRect(hdc, &area, theme.background_brush);
            let old_font = SelectObject(hdc, fonts.ui.into());
            let old_pen = SelectObject(hdc, GetStockObject(DC_PEN));
            let old_brush = SelectObject(hdc, GetStockObject(DC_BRUSH));
            SetBkMode(hdc, TRANSPARENT);
            let top = area.top + dpi.px(4);
            for (meter, (left, width)) in
                meters
                    .iter()
                    .zip(slots(area.right, meters.len(), dpi.px(16)))
            {
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
            }
            SelectObject(hdc, old_brush);
            SelectObject(hdc, old_pen);
            SelectObject(hdc, old_font);
        }
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
    fn the_spoken_reading_names_unknown_meters_and_their_reason() {
        let meters = [
            Meter {
                label: "Memory".into(),
                text: "2.1 GB".into(),
                fill: None,
                known: true,
                detail: "Memory of the WSL VM.".into(),
            },
            Meter {
                label: "GPU".into(),
                text: "Unknown".into(),
                fill: None,
                known: false,
                detail: "no counters".into(),
            },
        ];
        assert_eq!(spoken(&meters), "Memory 2.1 GB, GPU Unknown (no counters)");
    }
}
