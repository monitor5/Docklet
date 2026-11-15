//! The line next to the filter: the table's summary, Remove and Clear
//! buttons while rows are selected, and the button of the table's menu.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::{WC_BUTTONW, WC_STATICW};
use windows::Win32::UI::WindowsAndMessaging::WS_TABSTOP;

use crate::win::{Dpi, child, place, set_text, show, text_of};

// Static control styles; the crate keeps them in a module not worth a feature.
const SS_RIGHT: u32 = 0x0002;
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;

pub struct SelectionBar {
    pub summary: HWND,
    pub remove: HWND,
    pub clear: HWND,
    /// Opens the table's menu: its prunes and the like.
    pub commands: HWND,
}

/// What the bar shows besides the summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BarButtons {
    /// Rows are selected: Remove and Clear.
    pub selection: bool,
    /// The table has a menu.
    pub commands: bool,
}

/// Where the bar's parts go, from the right: left edge and width of the
/// commands button, Clear, Remove and the summary. A part that does not
/// show has no width.
pub fn bar_layout(
    left: i32,
    right: i32,
    button: i32,
    gap: i32,
    shown: BarButtons,
) -> [(i32, i32); 4] {
    let mut x = right;
    let mut take = |wanted: bool| {
        if wanted {
            x -= button;
            let slot = (x, button);
            x -= gap;
            slot
        } else {
            (x, 0)
        }
    };
    let commands = take(shown.commands);
    let clear = take(shown.selection);
    let remove = take(shown.selection);
    [commands, clear, remove, (left, (x - left).max(0))]
}

impl SelectionBar {
    pub fn create(parent: HWND, remove_id: usize, clear_id: usize, commands_id: usize) -> Self {
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
            commands: child(parent, WC_BUTTONW, "Actions", WS_TABSTOP.0, 0, commands_id),
        }
    }

    pub fn set(&self, text: &str) {
        // Setting the same text would still repaint the control.
        if text_of(self.summary) != text {
            set_text(self.summary, text);
        }
    }

    /// Lays the bar out to end at `right`, starting no further left than
    /// `left`.
    pub fn layout(&self, left: i32, right: i32, y: i32, height: i32, dpi: Dpi, shown: BarButtons) {
        let [commands, clear, remove, summary] =
            bar_layout(left, right, dpi.px(84), dpi.px(8), shown);
        place(self.commands, commands.0, y, commands.1, height);
        place(self.clear, clear.0, y, clear.1, height);
        place(self.remove, remove.0, y, remove.1, height);
        place(self.summary, summary.0, y, summary.1, height);
    }

    pub fn set_visible(&self, visible: bool, shown: BarButtons) {
        show(self.summary, visible);
        show(self.remove, visible && shown.selection);
        show(self.clear, visible && shown.selection);
        show(self.commands, visible && shown.commands);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_buttons_sit_at_the_right_and_the_summary_takes_the_rest() {
        let all = BarButtons {
            selection: true,
            commands: true,
        };
        assert_eq!(
            bar_layout(100, 500, 80, 10, all),
            [(420, 80), (330, 80), (240, 80), (100, 130)]
        );
        let none = BarButtons {
            selection: false,
            commands: false,
        };
        assert_eq!(bar_layout(100, 500, 80, 10, none)[3], (100, 400));
        let menu_only = BarButtons {
            selection: false,
            commands: true,
        };
        let [commands, clear, _, summary] = bar_layout(100, 500, 80, 10, menu_only);
        assert_eq!((commands, clear.1, summary), ((420, 80), 0, (100, 310)));
        // Too narrow: the summary gets nothing rather than a negative width.
        assert_eq!(bar_layout(100, 150, 80, 10, all)[3].1, 0);
    }
}
