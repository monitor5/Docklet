//! The filter box above the table: a label and a stock EDIT control, which
//! brings the Korean IME, selection, undo and the clipboard with it.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::{EM_SETCUEBANNER, WC_EDITW, WC_STATICW};
use windows::Win32::UI::WindowsAndMessaging::{ES_AUTOHSCROLL, WS_EX_CLIENTEDGE, WS_TABSTOP};

use crate::win::{child, send, set_text, text_of, wide};

pub struct FilterBox {
    pub label: HWND,
    pub edit: HWND,
}

impl FilterBox {
    pub fn create(parent: HWND, id: usize) -> Self {
        // The label comes first so screen readers name the box after it.
        // SS_CENTERIMAGE centres the single line vertically.
        let label = child(parent, WC_STATICW, "Filter", 0x0200, 0, 0);
        let edit = child(
            parent,
            WC_EDITW,
            "",
            WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE.0,
            id,
        );
        let cue = wide("Type to filter");
        // wParam 1 keeps the cue visible while the box has focus.
        send(edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
        Self { label, edit }
    }

    pub fn text(&self) -> String {
        text_of(self.edit)
    }

    /// Replaces the text. The control reports the change like typing does.
    pub fn set(&self, text: &str) {
        set_text(self.edit, text);
    }

    pub fn controls(&self) -> [HWND; 2] {
        [self.label, self.edit]
    }
}
