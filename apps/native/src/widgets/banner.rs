//! A line of warning or error text above the content, with Dismiss.

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Controls::{WC_BUTTONW, WC_STATICW};
use windows::Win32::UI::WindowsAndMessaging::WS_TABSTOP;

use crate::boundary::Banner;
use crate::win::{Dpi, child, place, set_text, show};

const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;
/// Lets screen readers notice the text when it changes.
const SS_NOTIFY: u32 = 0x0100;

pub struct BannerBar {
    pub text: HWND,
    pub dismiss: HWND,
}

impl BannerBar {
    pub fn create(parent: HWND, dismiss_id: usize) -> Self {
        Self {
            text: child(
                parent,
                WC_STATICW,
                "",
                SS_CENTERIMAGE | SS_ENDELLIPSIS | SS_NOTIFY,
                0,
                0,
            ),
            dismiss: child(parent, WC_BUTTONW, "Dismiss", WS_TABSTOP.0, 0, dismiss_id),
        }
    }

    pub fn set(&self, banner: Option<&Banner>) {
        if let Some(banner) = banner {
            set_text(self.text, &banner.text);
        }
        show(self.text, banner.is_some());
        show(self.dismiss, banner.is_some());
    }

    pub fn layout(&self, left: i32, right: i32, y: i32, height: i32, dpi: Dpi) {
        let button = dpi.px(84);
        let inset = dpi.px(4);
        place(
            self.dismiss,
            right - button - inset,
            y + inset,
            button,
            height - 2 * inset,
        );
        place(
            self.text,
            left + dpi.px(10),
            y,
            right - left - button - dpi.px(24),
            height,
        );
    }
}
