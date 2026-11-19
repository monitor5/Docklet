//! The log page: opening and closing the view, its frame timer, its status
//! line, and the view's window procedure.
//!
//! A timer asks the log for news about 12 times a second. A tick on which
//! nothing arrived costs a comparison; one that brought lines draws the
//! lines that are new and repaints the part of the view they changed.
//! What a frame costs decides how soon the next one may follow, and none
//! follows sooner than a quarter of a second, so a slow log costs what its
//! lines cost and a fast one keeps to a few percent of a processor. The
//! timer runs only while the log is open and the window is shown and not
//! minimised.

use std::time::Instant;

use docklet_app::format::format_count;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, ClientToScreen, EndPaint, HDC, PAINTSTRUCT};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetCapture, ReleaseCapture, SetCapture, SetFocus, VK_DOWN, VK_END, VK_HOME, VK_LEFT, VK_NEXT,
    VK_PRIOR, VK_RIGHT, VK_UP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DLGC_WANTARROWS, DLGC_WANTCHARS, DefWindowProcW, DestroyMenu,
    GetParent, GetScrollInfo, KillTimer, MF_GRAYED, MF_SEPARATOR, MF_STRING, SB_BOTTOM,
    SB_LINEDOWN, SB_LINEUP, SB_PAGEDOWN, SB_PAGEUP, SB_THUMBPOSITION, SB_THUMBTRACK, SB_TOP,
    SB_VERT, SCROLLBAR_COMMAND, SCROLLINFO, SIF_TRACKPOS, SetTimer, TPM_RETURNCMD, TPM_RIGHTBUTTON,
    TrackPopupMenu, WHEEL_DELTA, WM_CONTEXTMENU, WM_ERASEBKGND, WM_GETDLGCODE, WM_KEYDOWN,
    WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEHWHEEL, WM_MOUSEMOVE, WM_MOUSEWHEEL,
    WM_PAINT, WM_SETFOCUS, WM_SIZE, WM_VSCROLL,
};
use windows::core::w;

use super::{Shell, shell_of};
use crate::boundary::{Page, UiEvent};
use crate::host::ctx;
use crate::win::{control_down, high_word, low_word, point_of, set_text, shift_down, text_of};

pub(super) const LOG_TIMER: usize = 1;
/// The view looks for new lines this often. A frame holds the next one
/// back for three ticks, and for longer the more it cost
/// (`log_view::pause_after`).
const FRAME_MS: u32 = 80;
/// The status line and the scroll bar are controls that repaint when they
/// are told something new; they are told every this many ticks.
const STATUS_EVERY: u32 = 5;
/// Characters one sideways step moves.
const SHIFT_STEP: i32 = 8;

/// `12345` as `12,345`.
fn thousands(number: u64) -> String {
    format_count(usize::try_from(number).unwrap_or(usize::MAX))
}

/// The status line: count, follow state, lines below a paused view, and the
/// source's note.
pub fn status_text(lines: u64, following: bool, below: u64, note: &str) -> String {
    let mut text = format!("{} lines · ", thousands(lines));
    if following {
        text.push_str("following");
    } else if below > 0 {
        text.push_str(&format!("paused, {} below", thousands(below)));
    } else {
        text.push_str("paused");
    }
    if !note.is_empty() {
        text.push_str(" · ");
        text.push_str(note);
    }
    text
}

impl Shell {
    /// Shows the model's log over the table, if the tables are showing.
    pub(crate) fn open_log(&self) {
        let ctx = ctx();
        let pane = {
            let model = ctx.model.borrow();
            match model.page {
                Page::Tables => model.log.clone(),
                Page::Message(_) => None,
            }
        };
        let Some(pane) = pane else {
            return;
        };
        let was_open = self.log_open.replace(true);
        self.log.open(Some(pane.source));
        set_text(self.log_title, &pane.title);
        set_text(self.log.hwnd, &format!("Log of {}", pane.title));
        self.update_log_status();
        self.apply_visibility();
        self.layout();
        self.update_timer();
        self.focus(self.log.hwnd);
        if !was_open {
            ctx.emit(UiEvent::LogOpened);
        }
    }

    /// Takes the log view down and lets go of its source and buffers.
    pub(crate) fn close_log(&self) {
        if !self.log_open.replace(false) {
            return;
        }
        self.log.close();
        self.update_timer();
        // Tables that arrived while the log covered the list.
        self.refresh_stale_table();
        self.apply_visibility();
        self.layout();
        self.focus_default();
        ctx().emit(UiEvent::LogClosed);
    }

    /// Lets go of the log with the window that is being destroyed: nothing
    /// is laid out again, and nobody is told. `WindowClosed` says it all.
    pub(super) fn drop_log(&self) {
        if self.log_open.replace(false) {
            self.log.close();
            self.update_timer();
        }
    }

    pub(super) fn close_log_by_user(&self) {
        ctx().model.borrow_mut().log = None;
        self.close_log();
    }

    pub(crate) fn log_note_changed(&self) {
        self.update_log_status();
    }

    pub(super) fn jump_to_end(&self) {
        self.log.update(|state| state.jump_to_end());
        self.update_log_status();
        self.focus(self.log.hwnd);
    }

    /// Runs the frame timer exactly while the log is open and visible.
    pub(super) fn update_timer(&self) {
        let wanted = self.log_open.get() && self.visible.get();
        if self.timer.replace(wanted) == wanted {
            return;
        }
        // SAFETY: the timer belongs to this window.
        unsafe {
            if wanted {
                SetTimer(Some(self.hwnd), LOG_TIMER, FRAME_MS, None);
                // Catch up at once on what came while hidden.
                self.log_frame();
                self.log.sync_scrollbar();
                self.update_log_status();
            } else {
                let _ = KillTimer(Some(self.hwnd), LOG_TIMER);
            }
        }
    }

    /// Takes in what the source has that the view has not seen. Draws and
    /// repaints only if that changed the lines on screen, and then only
    /// the part of the view that changed: a view that does not follow
    /// stays as it is while lines pile up below it.
    fn log_frame(&self) {
        if self.log.poll() {
            self.log
                .refresh(&self.theme(), &self.fonts(), self.dpi.get());
        }
    }

    pub(super) fn on_log_timer(&self) {
        // Not while the last frame is still being paid for. What arrives
        // meanwhile is in the next one.
        if self.log.is_rested() {
            self.log_frame();
        }
        let ticks = self.ticks.get().wrapping_add(1);
        self.ticks.set(ticks);
        // Only if something arrived since they were last told: a tick of a
        // quiet log ends with the comparison in `poll`.
        if ticks.is_multiple_of(STATUS_EVERY) && self.log.take_news() {
            self.log.sync_scrollbar();
            self.update_log_status();
        }
    }

    /// The log view's WM_PAINT: paints what `paint` asks for, and notes
    /// what it cost.
    fn paint_log(&self, hdc: HDC, paint: &PAINTSTRUCT) {
        let started = Instant::now();
        self.log.paint(
            hdc,
            paint.rcPaint,
            &self.theme(),
            &self.fonts(),
            self.dpi.get(),
        );
        self.log.painted(started.elapsed());
    }

    fn update_log_status(&self) {
        let (lines, following, below) = self
            .log
            .state(|state| (state.len(), state.following(), state.below()));
        let text = status_text(lines, following, below, &ctx().model.borrow().log_note);
        // Setting the same text would still repaint the control.
        if text_of(self.log_status) != text {
            set_text(self.log_status, &text);
        }
        Shell::set_enabled(self.log_jump, !following);
    }

    fn log_key(&self, key: u16) {
        let page = self.log.state(|state| state.page()) as i64;
        let shift = shift_down();
        let control = control_down();
        // Shift with Up and Down moves the selection's end instead.
        let extend = |delta: i64| {
            self.log.update(|state| {
                if let Some(caret) = state.caret() {
                    let line = caret
                        .saturating_add_signed(delta)
                        .clamp(state.first(), state.end().saturating_sub(1));
                    state.select_to(line);
                    state.reveal(line);
                }
            });
        };
        match key {
            _ if key == VK_UP.0 && shift => extend(-1),
            _ if key == VK_DOWN.0 && shift => extend(1),
            _ if key == VK_UP.0 => self.log.update(|state| state.scroll_by(-1)),
            _ if key == VK_DOWN.0 => self.log.update(|state| state.scroll_by(1)),
            _ if key == VK_PRIOR.0 => self.log.update(|state| state.scroll_by(-page)),
            _ if key == VK_NEXT.0 => self.log.update(|state| state.scroll_by(page)),
            _ if key == VK_HOME.0 => self.log.update(|state| state.scroll_to(0)),
            _ if key == VK_END.0 => self.log.update(|state| state.jump_to_end()),
            _ if key == VK_LEFT.0 => self.log.shift_by(-SHIFT_STEP),
            _ if key == VK_RIGHT.0 => self.log.shift_by(SHIFT_STEP),
            _ if control && key == u16::from(b'A') => self.log.update(|state| state.select_all()),
            _ if control && key == u16::from(b'C') => {
                self.log.copy_selection(self.hwnd);
            }
            _ => return,
        }
        self.update_log_status();
    }

    fn log_press(&self, point: POINT) {
        let hwnd = self.log.hwnd;
        // SAFETY: focusing and capturing the mouse for this very window.
        unsafe {
            let _ = SetFocus(Some(hwnd));
            SetCapture(hwnd);
        }
        let row = self.log.row_at(point.y);
        let extend = shift_down();
        self.log.update(|state| {
            if let Some(line) = state.line_at_row(row) {
                state.select_from(line, extend);
            }
        });
        self.update_log_status();
    }

    fn log_drag(&self, point: POINT) {
        let row = self.log.row_at(point.y);
        self.log.update(|state| {
            if let Some(line) = state.line_at_row(row) {
                state.select_to(line);
                state.reveal(line);
            }
        });
    }

    fn log_menu(&self, lparam: LPARAM) {
        let hwnd = self.log.hwnd;
        let mut point = point_of(lparam);
        // From the keyboard the point is (-1, -1): open at the view's corner.
        if point == (POINT { x: -1, y: -1 }) {
            point = POINT { x: 0, y: 0 };
            // SAFETY: converting a point of the view to the screen.
            unsafe {
                let _ = ClientToScreen(hwnd, &mut point);
            }
        }
        let has_selection = self.log.state(|state| state.selection().is_some());
        const COPY: usize = 1;
        const SELECT_ALL: usize = 2;
        const JUMP: usize = 3;
        // SAFETY: the menu is created, shown and destroyed here.
        let chosen = unsafe {
            let Ok(menu) = CreatePopupMenu() else {
                return;
            };
            let copy_flags = if has_selection {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            };
            let _ = AppendMenuW(menu, copy_flags, COPY, w!("Copy\tCtrl+C"));
            let _ = AppendMenuW(menu, MF_STRING, SELECT_ALL, w!("Select all\tCtrl+A"));
            let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
            let _ = AppendMenuW(menu, MF_STRING, JUMP, w!("Jump to end\tEnd"));
            let chosen = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);
            chosen.0 as usize
        };
        match chosen {
            COPY => {
                self.log.copy_selection(self.hwnd);
            }
            SELECT_ALL => self.log.update(|state| state.select_all()),
            JUMP => self.jump_to_end(),
            _ => {}
        }
        self.update_log_status();
    }

    fn log_scroll(&self, command: SCROLLBAR_COMMAND) {
        let page = self.log.state(|state| state.page()) as i64;
        match command {
            SB_LINEUP => self.log.update(|state| state.scroll_by(-1)),
            SB_LINEDOWN => self.log.update(|state| state.scroll_by(1)),
            SB_PAGEUP => self.log.update(|state| state.scroll_by(-page)),
            SB_PAGEDOWN => self.log.update(|state| state.scroll_by(page)),
            SB_TOP => self.log.update(|state| state.scroll_to(0)),
            SB_BOTTOM => self.log.update(|state| state.jump_to_end()),
            SB_THUMBTRACK | SB_THUMBPOSITION => {
                let mut info = SCROLLINFO {
                    cbSize: size_of::<SCROLLINFO>() as u32,
                    fMask: SIF_TRACKPOS,
                    ..Default::default()
                };
                // SAFETY: `info` is a valid in/out structure.
                unsafe {
                    let _ = GetScrollInfo(self.log.hwnd, SB_VERT, &mut info);
                }
                let track = info.nTrackPos.max(0) as u64;
                self.log
                    .update(|state| state.scroll_to(state.first() + track));
            }
            _ => return,
        }
        self.update_log_status();
    }

    fn log_wheel(&self, wparam: WPARAM, sideways: bool) {
        let delta = i32::from(high_word(wparam.0) as u16 as i16);
        let notches = delta / WHEEL_DELTA as i32;
        if notches == 0 {
            return;
        }
        if sideways || shift_down() {
            let direction = if sideways { 1 } else { -1 };
            self.log.shift_by(direction * notches * SHIFT_STEP);
        } else {
            self.log
                .update(|state| state.scroll_by(-i64::from(notches) * 3));
            self.update_log_status();
        }
    }
}

pub(super) unsafe extern "system" fn log_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: a child's parent outlives it.
    let parent = unsafe { GetParent(hwnd) }.unwrap_or_default();
    let Some(shell) = shell_of(parent).filter(|shell| shell.log.hwnd == hwnd) else {
        // SAFETY: forwarding the message as it was received.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: the usual paint bracket for this window.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut paint);
                shell.paint_log(hdc, &paint);
                let _ = EndPaint(hwnd, &paint);
            }
        }
        WM_SIZE => shell.log.resize(),
        WM_SETFOCUS => shell.log.set_focused(true),
        WM_KILLFOCUS => shell.log.set_focused(false),
        WM_VSCROLL => shell.log_scroll(SCROLLBAR_COMMAND(low_word(wparam.0) as i32)),
        WM_MOUSEWHEEL => shell.log_wheel(wparam, false),
        WM_MOUSEHWHEEL => shell.log_wheel(wparam, true),
        WM_KEYDOWN => shell.log_key(wparam.0 as u16),
        WM_LBUTTONDOWN => shell.log_press(point_of(lparam)),
        // SAFETY: a plain query of this thread's mouse capture.
        WM_MOUSEMOVE if unsafe { GetCapture() } == hwnd => shell.log_drag(point_of(lparam)),
        WM_LBUTTONUP => {
            // SAFETY: releasing the capture taken on button down.
            unsafe {
                let _ = ReleaseCapture();
            }
        }
        WM_CONTEXTMENU => shell.log_menu(lparam),
        // Arrows and characters come here instead of moving the focus.
        WM_GETDLGCODE => return LRESULT((DLGC_WANTARROWS | DLGC_WANTCHARS) as isize),
        // SAFETY: forwarding the message as it was received.
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_line_says_how_many_and_whether_it_follows() {
        assert_eq!(status_text(50_000, true, 0, ""), "50,000 lines · following");
        assert_eq!(
            status_text(3, false, 0, "log ended"),
            "3 lines · paused · log ended"
        );
        assert_eq!(
            status_text(9000, false, 1200, ""),
            "9,000 lines · paused, 1,200 below"
        );
    }
}
