//! A full-window message: title, body, optional detail text, a progress bar
//! and up to two buttons. Stock controls throughout, so it reads aloud and
//! works from the keyboard.

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    DT_CALCRECT, DT_NOPREFIX, DT_WORDBREAK, DrawTextW, GetDC, HFONT, ReleaseDC, SelectObject,
};
use windows::Win32::UI::Controls::{
    PBM_SETMARQUEE, PBM_SETPOS, PBM_SETRANGE32, PBS_MARQUEE, PROGRESS_CLASSW, WC_BUTTONW,
    WC_COMBOBOXW, WC_EDITW, WC_STATICW,
};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING, CB_GETCURSEL, CB_RESETCONTENT, CB_SETCURSEL,
    CBS_DROPDOWNLIST, ES_AUTOVSCROLL, ES_MULTILINE, ES_READONLY, GWL_STYLE, GetWindowLongPtrW,
    SetWindowLongPtrW, WS_TABSTOP, WS_VSCROLL,
};

use crate::boundary::{Choice, Progress, Screen};
use crate::win::{Dpi, child, place, send, set_text, show, wide};

const SS_NOPREFIX: u32 = 0x0080;
const SS_NOTIFY: u32 = 0x0100;
const SS_CENTERIMAGE: u32 = 0x0200;

pub struct MessageScreen {
    pub title: HWND,
    pub body: HWND,
    pub choice_label: HWND,
    /// A drop-down list: the base of an install.
    pub choice: HWND,
    pub detail: HWND,
    pub progress: HWND,
    pub primary: HWND,
    pub secondary: HWND,
    screen: RefCell<Screen>,
}

/// The heights of a message screen's parts; 0 for a part that is missing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Heights {
    pub title: i32,
    pub body: i32,
    pub choice: i32,
    pub progress: i32,
    pub buttons: i32,
}

/// The vertical stack of a message screen: where each part starts.
#[derive(Debug, PartialEq, Eq)]
pub struct Stack {
    pub title: i32,
    pub body: i32,
    pub choice: i32,
    pub progress: i32,
    pub detail: (i32, i32),
    pub buttons: i32,
}

/// Stacks the parts from `top` with `gap` between those that are present;
/// the detail box takes what is left, between `min_detail` and `max_detail`.
pub fn stack(top: i32, bottom: i32, heights: Heights, gap: i32, detail: (bool, i32, i32)) -> Stack {
    let (has_detail, min_detail, max_detail) = detail;
    let mut y = top;
    let title_y = y;
    y += heights.title + gap;
    let mut part = |height: i32| {
        let start = y;
        if height > 0 {
            y += height + gap;
        }
        start
    };
    let body_y = part(heights.body);
    let choice_y = part(heights.choice);
    let progress_y = part(heights.progress);
    let detail_y = y;
    let detail_height = if has_detail {
        let room = bottom - y - heights.buttons - gap;
        room.clamp(min_detail, max_detail.max(min_detail))
    } else {
        0
    };
    if has_detail {
        y += detail_height + gap;
    }
    Stack {
        title: title_y,
        body: body_y,
        choice: choice_y,
        progress: progress_y,
        detail: (detail_y, detail_height),
        buttons: y,
    }
}

impl MessageScreen {
    pub fn create(
        parent: HWND,
        primary_id: usize,
        secondary_id: usize,
        detail_id: usize,
        choice_id: usize,
    ) -> Self {
        Self {
            title: child(parent, WC_STATICW, "", SS_NOPREFIX | SS_NOTIFY, 0, 0),
            body: child(parent, WC_STATICW, "", SS_NOPREFIX, 0, 0),
            // The label comes first so screen readers name the list after it.
            choice_label: child(parent, WC_STATICW, "", SS_CENTERIMAGE, 0, 0),
            choice: child(
                parent,
                WC_COMBOBOXW,
                "",
                WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32,
                0,
                choice_id,
            ),
            detail: child(
                parent,
                WC_EDITW,
                "",
                WS_TABSTOP.0 | WS_VSCROLL.0 | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
                0,
                detail_id,
            ),
            progress: child(parent, PROGRESS_CLASSW, "", 0, 0, 0),
            primary: child(
                parent,
                WC_BUTTONW,
                "",
                WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32,
                0,
                primary_id,
            ),
            secondary: child(
                parent,
                WC_BUTTONW,
                "",
                WS_TABSTOP.0 | BS_PUSHBUTTON as u32,
                0,
                secondary_id,
            ),
            screen: RefCell::new(Screen::default()),
        }
    }

    /// Takes the texts of `screen`. Call [`Self::layout`] after.
    pub fn set(&self, screen: &Screen) {
        let old = self.screen.replace(screen.clone());
        if old.title != screen.title {
            set_text(self.title, &screen.title);
        }
        if old.body != screen.body {
            set_text(self.body, &screen.body);
        }
        if old.detail != screen.detail {
            // An edit control wants CRLF line ends.
            let detail = screen
                .detail
                .as_deref()
                .unwrap_or("")
                .replace("\r\n", "\n")
                .replace('\n', "\r\n");
            set_text(self.detail, &detail);
            // Scrolled to the end, where install output grows.
            let length = detail.encode_utf16().count();
            send(
                self.detail,
                windows::Win32::UI::Controls::EM_SETSEL,
                length,
                length as isize,
            );
            send(
                self.detail,
                windows::Win32::UI::Controls::EM_SCROLLCARET,
                0,
                0,
            );
        }
        for (button, wanted) in [
            (self.primary, &screen.primary),
            (self.secondary, &screen.secondary),
        ] {
            if let Some(wanted) = wanted {
                set_text(button, &wanted.label);
                // SAFETY: `button` is a child of the main window.
                unsafe {
                    let _ = EnableWindow(button, wanted.enabled);
                }
            }
        }
        if let Some(choice) = &screen.choice {
            self.set_choice(old.choice.as_ref(), choice);
        }
        self.set_progress(screen.progress, true);
    }

    /// Makes the drop-down list show `choice`. The options are only put in
    /// again when they changed, so an open list is not closed under the
    /// user's hand.
    fn set_choice(&self, old: Option<&Choice>, choice: &Choice) {
        if old.map(|old| &old.label) != Some(&choice.label) {
            set_text(self.choice_label, &choice.label);
            // The list is named after its label for screen readers.
            set_text(self.choice, &choice.label);
        }
        if old.map(|old| &old.options) != Some(&choice.options) {
            send(self.choice, CB_RESETCONTENT, 0, 0);
            for option in &choice.options {
                let text = wide(option);
                send(self.choice, CB_ADDSTRING, 0, text.as_ptr() as isize);
            }
        }
        let selected = send(self.choice, CB_GETCURSEL, 0, 0);
        if usize::try_from(selected).ok() != Some(choice.selected) {
            send(self.choice, CB_SETCURSEL, choice.selected, 0);
        }
        // SAFETY: the list is a child of the main window.
        unsafe {
            let _ = EnableWindow(self.choice, choice.enabled);
        }
    }

    /// The option the drop-down list shows.
    pub fn chosen(&self) -> Option<usize> {
        usize::try_from(send(self.choice, CB_GETCURSEL, 0, 0)).ok()
    }

    fn set_progress(&self, progress: Progress, animate: bool) {
        // SAFETY: switching a style bit of this screen's own progress bar.
        let style = unsafe { GetWindowLongPtrW(self.progress, GWL_STYLE) };
        let marquee = matches!(progress, Progress::Busy);
        let wanted = if marquee {
            style | PBS_MARQUEE as isize
        } else {
            style & !(PBS_MARQUEE as isize)
        };
        if wanted != style {
            // SAFETY: as above.
            unsafe {
                SetWindowLongPtrW(self.progress, GWL_STYLE, wanted);
            }
        }
        match progress {
            Progress::Busy => {
                send(self.progress, PBM_SETMARQUEE, usize::from(animate), 30);
            }
            Progress::Fraction(fraction) => {
                send(self.progress, PBM_SETMARQUEE, 0, 0);
                send(self.progress, PBM_SETRANGE32, 0, 1000);
                let position = if fraction.is_nan() {
                    0.0
                } else {
                    fraction.clamp(0.0, 1.0)
                };
                send(self.progress, PBM_SETPOS, (position * 1000.0) as usize, 0);
            }
            Progress::None => {
                send(self.progress, PBM_SETMARQUEE, 0, 0);
            }
        }
    }

    /// Stops the marquee's own timer while the window is minimised.
    pub fn set_animating(&self, animate: bool) {
        let progress = self.screen.borrow().progress;
        if matches!(progress, Progress::Busy) {
            self.set_progress(progress, animate);
        }
    }

    /// The button key behind control `id`.
    pub fn key_of(&self, primary: bool) -> Option<String> {
        let screen = self.screen.borrow();
        let button = if primary {
            &screen.primary
        } else {
            &screen.secondary
        };
        button.as_ref().map(|button| button.key.clone())
    }

    /// Shows or hides the whole screen.
    pub fn set_visible(&self, visible: bool) {
        let screen = self.screen.borrow();
        show(self.title, visible);
        show(self.body, visible && !screen.body.is_empty());
        show(self.choice_label, visible && screen.choice.is_some());
        show(self.choice, visible && screen.choice.is_some());
        show(self.detail, visible && screen.detail.is_some());
        show(
            self.progress,
            visible && !matches!(screen.progress, Progress::None),
        );
        show(self.primary, visible && screen.primary.is_some());
        show(self.secondary, visible && screen.secondary.is_some());
    }

    /// Lays the screen out in a centred column of `area`.
    pub fn layout(&self, area: RECT, dpi: Dpi, fonts: (HFONT, HFONT)) {
        let screen = self.screen.borrow();
        let width = (area.right - area.left - 2 * dpi.px(24))
            .min(dpi.px(560))
            .max(dpi.px(120));
        let left = area.left + (area.right - area.left - width) / 2;
        let (title_font, body_font) = fonts;
        let title = measure(self.title, title_font, &screen.title, width);
        let body = if screen.body.is_empty() {
            0
        } else {
            measure(self.body, body_font, &screen.body, width)
        };
        let progress = if matches!(screen.progress, Progress::None) {
            0
        } else {
            dpi.px(6)
        };
        let button_height = dpi.px(32);
        let choice = if screen.choice.is_some() {
            dpi.px(30)
        } else {
            0
        };
        let top = area.top + ((area.bottom - area.top) / 6).min(dpi.px(96));
        let heights = Heights {
            title,
            body,
            choice,
            progress,
            buttons: button_height,
        };
        let layout = stack(
            top,
            area.bottom - dpi.px(16),
            heights,
            dpi.px(16),
            (screen.detail.is_some(), dpi.px(60), dpi.px(220)),
        );
        place(self.title, left, layout.title, width, title);
        place(self.body, left, layout.body, width, body);
        let label = dpi.px(48);
        place(self.choice_label, left, layout.choice, label, choice);
        // A drop-down list's height is that of its open list; the closed
        // box sizes itself.
        place(
            self.choice,
            left + label,
            layout.choice,
            dpi.px(220),
            dpi.px(240),
        );
        place(self.progress, left, layout.progress, width, progress);
        place(self.detail, left, layout.detail.0, width, layout.detail.1);
        let button = dpi.px(150);
        place(self.primary, left, layout.buttons, button, button_height);
        let second_x = if screen.primary.is_some() {
            left + button + dpi.px(8)
        } else {
            left
        };
        place(
            self.secondary,
            second_x,
            layout.buttons,
            button,
            button_height,
        );
    }
}

/// Height of `text` wrapped to `width` in `font`.
fn measure(hwnd: HWND, font: HFONT, text: &str, width: i32) -> i32 {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        units.push(u16::from(b' '));
    }
    let mut rect = RECT {
        right: width,
        ..Default::default()
    };
    // SAFETY: the DC is released and the font selected back out.
    unsafe {
        let hdc = GetDC(Some(hwnd));
        let previous = SelectObject(hdc, font.into());
        DrawTextW(
            hdc,
            &mut units,
            &mut rect,
            DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX,
        );
        SelectObject(hdc, previous);
        ReleaseDC(Some(hwnd), hdc);
    }
    rect.bottom
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_stack_with_gaps_and_skip_what_is_missing() {
        let heights = Heights {
            title: 40,
            buttons: 32,
            ..Heights::default()
        };
        let layout = stack(100, 1000, heights, 10, (false, 60, 200));
        assert_eq!(layout.title, 100);
        assert_eq!(layout.body, 150);
        assert_eq!(layout.buttons, 150);
        let heights = Heights {
            body: 30,
            progress: 6,
            ..heights
        };
        let layout = stack(100, 1000, heights, 10, (false, 60, 200));
        assert_eq!(layout.progress, 190);
        assert_eq!(layout.buttons, 206);
    }

    #[test]
    fn a_choice_sits_between_the_body_and_the_progress() {
        let heights = Heights {
            title: 40,
            body: 30,
            choice: 28,
            progress: 6,
            buttons: 32,
        };
        let layout = stack(100, 1000, heights, 10, (false, 60, 200));
        assert_eq!(layout.choice, 190);
        assert_eq!(layout.progress, 228);
        assert_eq!(layout.buttons, 244);
    }

    #[test]
    fn the_detail_box_takes_the_room_left_within_limits() {
        let heights = Heights {
            title: 40,
            body: 20,
            buttons: 30,
            ..Heights::default()
        };
        let roomy = stack(0, 1000, heights, 10, (true, 60, 200));
        assert_eq!(roomy.detail, (80, 200));
        assert_eq!(roomy.buttons, 290);
        let tight = stack(0, 150, heights, 10, (true, 60, 200));
        assert_eq!(tight.detail.1, 60);
    }
}
