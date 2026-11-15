//! A line to type above a table, with its button and a status text: the
//! image to pull. Stock controls, so the IME, the clipboard and screen
//! readers work as everywhere.

use std::cell::RefCell;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::{EM_SETCUEBANNER, WC_BUTTONW, WC_EDITW, WC_STATICW};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{ES_AUTOHSCROLL, WS_EX_CLIENTEDGE, WS_TABSTOP};

use crate::boundary::Entry;
use crate::win::{Dpi, child, place, send, set_text, show, text_of, wide};

const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;

pub struct EntryRow {
    pub label: HWND,
    pub edit: HWND,
    pub button: HWND,
    pub status: HWND,
    /// What the controls show now.
    shown: RefCell<Entry>,
}

impl EntryRow {
    pub fn create(parent: HWND, edit_id: usize, button_id: usize) -> Self {
        // The label comes first so screen readers name the box after it.
        let label = child(parent, WC_STATICW, "", SS_CENTERIMAGE, 0, 0);
        let edit = child(
            parent,
            WC_EDITW,
            "",
            WS_TABSTOP.0 | ES_AUTOHSCROLL as u32,
            WS_EX_CLIENTEDGE.0,
            edit_id,
        );
        Self {
            label,
            edit,
            button: child(parent, WC_BUTTONW, "", WS_TABSTOP.0, 0, button_id),
            status: child(
                parent,
                WC_STATICW,
                "",
                SS_CENTERIMAGE | SS_ENDELLIPSIS,
                0,
                0,
            ),
            // The controls are created enabled.
            shown: RefCell::new(Entry {
                enabled: true,
                ..Entry::default()
            }),
        }
    }

    /// Makes the controls show `entry`, touching only what differs: the
    /// status changes many times a second while an image is pulled.
    pub fn set(&self, entry: &Entry) {
        let old = self.shown.replace(Entry {
            text: None,
            ..entry.clone()
        });
        if old.label != entry.label {
            set_text(self.label, &entry.label);
        }
        if old.cue != entry.cue {
            let cue = wide(&entry.cue);
            // wParam 1 keeps the cue visible while the box has focus.
            send(self.edit, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
        }
        if old.button != entry.button {
            set_text(self.button, &entry.button);
        }
        if old.status != entry.status {
            set_text(self.status, &entry.status);
        }
        if old.enabled != entry.enabled {
            // SAFETY: both are children of the main window.
            unsafe {
                let _ = EnableWindow(self.edit, entry.enabled);
                let _ = EnableWindow(self.button, entry.enabled);
            }
        }
        if let Some(text) = &entry.text {
            set_text(self.edit, text);
        }
    }

    pub fn text(&self) -> String {
        text_of(self.edit)
    }

    pub fn controls(&self) -> [HWND; 4] {
        [self.label, self.edit, self.button, self.status]
    }

    pub fn set_visible(&self, visible: bool) {
        for control in self.controls() {
            show(control, visible);
        }
    }

    /// Lays the row out from `left` to `right`: label, box, button, and the
    /// status in what is left.
    pub fn layout(&self, left: i32, right: i32, y: i32, height: i32, dpi: Dpi) {
        let label = dpi.px(44);
        let edit = dpi.px(260);
        let button = dpi.px(84);
        let gap = dpi.px(8);
        // A single-line edit does not centre its text, so it gets its own
        // height, centred in the row.
        let edit_height = dpi.px(26);
        place(self.label, left, y, label, height);
        let mut x = left + label;
        place(
            self.edit,
            x,
            y + (height - edit_height) / 2,
            edit,
            edit_height,
        );
        x += edit + gap;
        place(self.button, x, y, button, height);
        x += button + gap + dpi.px(4);
        place(self.status, x, y, (right - x).max(0), height);
    }
}
