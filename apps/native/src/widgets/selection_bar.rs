//! The count next to the filter, which turns into "7 selected" with Remove
//! and Clear buttons when rows are checked.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::{WC_BUTTONW, WC_STATICW};
use windows::Win32::UI::WindowsAndMessaging::WS_TABSTOP;

use crate::win::{Dpi, child, place, set_text, show};

// Static control styles; the crate keeps them in a module not worth a feature.
const SS_RIGHT: u32 = 0x0002;
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;

pub struct SelectionBar {
    pub summary: HWND,
    pub remove: HWND,
    pub clear: HWND,
}

/// The text left of the buttons.
pub fn summary(checked: usize, matching: usize, total: usize, noun: &str) -> String {
    let noun = if noun.is_empty() { "rows" } else { noun };
    if checked > 0 {
        format!("{checked} selected")
    } else if matching == total {
        format!("{total} {noun}")
    } else {
        format!("{matching} of {total} {noun}")
    }
}

impl SelectionBar {
    pub fn create(parent: HWND, remove_id: usize, clear_id: usize) -> Self {
        Self {
            summary: child(
                parent,
                WC_STATICW,
                "",
                SS_RIGHT | SS_CENTERIMAGE | SS_ENDELLIPSIS,
                0,
                0,
            ),
            remove: child(parent, WC_BUTTONW, "Remove", WS_TABSTOP.0, 0, remove_id),
            clear: child(parent, WC_BUTTONW, "Clear", WS_TABSTOP.0, 0, clear_id),
        }
    }

    pub fn set(&self, text: &str) {
        set_text(self.summary, text);
    }

    /// Lays the bar out to end at `right`, starting no further left than
    /// `left`. The buttons show only while something is checked.
    pub fn layout(&self, left: i32, right: i32, y: i32, height: i32, dpi: Dpi, checked: bool) {
        let button = dpi.px(84);
        let gap = dpi.px(8);
        let mut x = right;
        if checked {
            x -= button;
            place(self.clear, x, y, button, height);
            x -= gap + button;
            place(self.remove, x, y, button, height);
            x -= gap;
        }
        place(self.summary, left, y, (x - left).max(0), height);
    }

    pub fn set_visible(&self, visible: bool, checked: bool) {
        show(self.summary, visible);
        show(self.remove, visible && checked);
        show(self.clear, visible && checked);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_summary_counts_what_is_shown_and_what_is_checked() {
        assert_eq!(summary(0, 200, 200, "containers"), "200 containers");
        assert_eq!(summary(0, 12, 200, "containers"), "12 of 200 containers");
        assert_eq!(summary(7, 12, 200, "containers"), "7 selected");
        assert_eq!(summary(0, 3, 3, ""), "3 rows");
    }
}
