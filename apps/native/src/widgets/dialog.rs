//! The confirmation dialog and the result message.
//!
//! A task dialog knows no dark mode, so these are dialogs of the system's
//! dialog class filled with stock controls, themed the way the main window
//! themes its own: the same colours through `WM_CTLCOLOR*`, the same dark
//! buttons and title bar. The dialog manager brings the modal loop, the
//! disabled owner, Tab, Enter and Escape; the stock controls bring the
//! keyboard and what a screen reader reads. Their text comes from outside.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, DT_CALCRECT, DT_EDITCONTROL, DT_NOPREFIX, DT_SINGLELINE, DT_WORDBREAK, DrawTextW,
    EndPaint, GetDC, GetMonitorInfoW, HBRUSH, HDC, HFONT, MONITOR_DEFAULTTONEAREST, MONITORINFO,
    MonitorFromPoint, MonitorFromWindow, PAINTSTRUCT, ReleaseDC, SelectObject, SetBkColor,
    SetTextColor,
};
use windows::Win32::UI::Controls::{LoadIconWithScaleDown, WC_BUTTONW, WC_EDITW, WC_STATICW};
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DDC_DISABLE_ALL, GetDpiForWindow, SetDialogDpiChangeBehavior,
};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    BS_DEFPUSHBUTTON, BS_PUSHBUTTON, DI_NORMAL, DLGTEMPLATE, DM_SETDEFID, DS_MODALFRAME,
    DestroyIcon, DialogBoxIndirectParamW, DrawIconEx, ES_MULTILINE, ES_READONLY, EndDialog,
    GWL_EXSTYLE, GWL_STYLE, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW, GetWindowRect, HICON,
    IDCANCEL, IDI_ERROR, IDI_INFORMATION, IDI_WARNING, IDOK, IsIconic, IsWindowVisible,
    SWP_NOACTIVATE, SWP_NOZORDER, SetWindowLongPtrW, SetWindowPos, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_COMMAND, WM_CTLCOLORBTN, WM_CTLCOLORDLG, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DPICHANGED,
    WM_INITDIALOG, WM_NCDESTROY, WM_PAINT, WM_SETFONT, WS_CAPTION, WS_POPUP, WS_SYSMENU,
    WS_TABSTOP, WS_VSCROLL,
};

use crate::boundary::{Confirm, Notice, Tone};
use crate::host::ctx;
use crate::theme::{DarkApi, Fonts, Look, Theme, style_control, style_frame};
use crate::win::{Dpi, child, high_word, instance, low_word, place, send, show};

/// The control number of the button that agrees.
const ACCEPT: usize = 1000;
const SS_NOPREFIX: u32 = 0x0080;

/// The frame every dialog here has; its size is worked out from this.
const STYLE: WINDOW_STYLE =
    WINDOW_STYLE(WS_POPUP.0 | WS_CAPTION.0 | WS_SYSMENU.0 | DS_MODALFRAME as u32);

/// A size or a place in pixels.
type Pair = (i32, i32);

/// What a dialog is laid out with, in pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Metrics {
    /// Space around the content.
    pub margin: i32,
    /// Space between two parts, and between the icon and the text.
    pub gap: i32,
    /// The icon is a square of this side.
    pub icon: i32,
    /// Width of the column of text right of the icon.
    pub text: i32,
    pub button_height: i32,
}

/// Where the parts of a dialog go: each as its top-left corner, with the
/// size of the client area they need.
#[derive(Debug, PartialEq, Eq)]
pub struct Layout {
    pub icon: Pair,
    pub title: Pair,
    pub message: Pair,
    pub detail: Pair,
    /// Left edge of each button, in the order given; they share a row.
    pub buttons: Vec<i32>,
    pub button_row: i32,
    pub client: Pair,
}

/// Stacks the title, the message and the detail box right of the icon and
/// puts the buttons in a row at the bottom right, the last one outermost.
/// `heights` are those of the title, the message and the detail; a part
/// with no height takes no room. `buttons` are the widths of the buttons.
pub fn layout(heights: [i32; 3], buttons: &[i32], metrics: Metrics) -> Layout {
    let Metrics {
        margin,
        gap,
        icon,
        text,
        button_height,
    } = metrics;
    let column = margin + icon + gap;
    let mut y = margin;
    let mut part = |height: i32| {
        let top = y;
        if height > 0 {
            y += height + gap;
        }
        (column, top)
    };
    let title = part(heights[0]);
    let message = part(heights[1]);
    let detail = part(heights[2]);
    // The buttons clear the icon as well as the text.
    let button_row = y.max(margin + icon + gap) + gap;
    let width = column + text + margin;
    let mut right = width - margin;
    let mut lefts: Vec<i32> = buttons
        .iter()
        .rev()
        .map(|&button| {
            right -= button;
            let left = right;
            right -= gap;
            left
        })
        .collect();
    lefts.reverse();
    Layout {
        icon: (margin, margin),
        title,
        message,
        detail,
        buttons: lefts,
        button_row,
        client: (width, button_row + button_height + margin),
    }
}

/// The top-left corner that centres a window of `size` on `owner`, moved
/// as little as it takes to lie within `area`: a monitor's work area.
pub fn centred(owner: RECT, size: Pair, area: RECT) -> Pair {
    let centre = |low: i32, high: i32, size: i32, from: i32, to: i32| {
        let wanted = low + (high - low - size) / 2;
        // A window larger than the area starts at its top-left.
        wanted.min(to - size).max(from)
    };
    (
        centre(owner.left, owner.right, size.0, area.left, area.right),
        centre(owner.top, owner.bottom, size.1, area.top, area.bottom),
    )
}

/// One button: its control number, what it says, and whether Enter is it.
struct ButtonSpec<'a> {
    id: usize,
    label: &'a str,
    default: bool,
}

/// A dialog while it is open. It lives on the stack of [`run`]; the dialog
/// procedure reaches it through the window's user data.
struct Dialog<'a> {
    title: &'a str,
    message: &'a str,
    detail: Option<&'a str>,
    tone: Tone,
    buttons: &'a [ButtonSpec<'a>],
    owner: Option<HWND>,
    /// What makes controls dark, if this system has it and it is wanted.
    api: Option<&'a DarkApi>,
    theme: Theme,
    fonts: RefCell<Option<Fonts>>,
    icon: Cell<HICON>,
    /// Where the icon is drawn, and how large.
    icon_at: Cell<(Pair, i32)>,
    controls: RefCell<Controls>,
}

#[derive(Default)]
struct Controls {
    title: HWND,
    message: HWND,
    detail: HWND,
    buttons: Vec<HWND>,
}

/// A dialog template without controls: they are made in WM_INITDIALOG,
/// sized for the texts. It is a DLGTEMPLATE followed by three empty strings
/// (menu, class, title), aligned as its first field must be.
#[repr(C)]
struct Template {
    header: DLGTEMPLATE,
    strings: [u16; 4],
    _align: [u32; 0],
}

impl Template {
    fn new() -> Self {
        Self {
            header: DLGTEMPLATE {
                style: STYLE.0,
                ..Default::default()
            },
            strings: [0; 4],
            _align: [],
        }
    }

    /// The whole template, strings included, as the dialog calls take it.
    fn as_ptr(&self) -> *const DLGTEMPLATE {
        std::ptr::from_ref(self).cast()
    }
}

/// Shows the dialog and returns the number of the button that closed it;
/// `IDCANCEL` for Escape, the close box, or a dialog that could not be
/// shown.
fn run(dialog: &Dialog<'_>) -> usize {
    let template = Template::new();
    let state = std::ptr::from_ref(dialog) as isize;
    // SAFETY: the template outlives the call, which runs the dialog's modal
    // loop and returns once it is closed; so does `dialog`, which the
    // procedure reads through the pointer handed over here.
    let closed_by = unsafe {
        DialogBoxIndirectParamW(
            Some(instance()),
            template.as_ptr(),
            dialog.owner,
            Some(dialog_proc),
            LPARAM(state),
        )
    };
    usize::try_from(closed_by)
        .ok()
        .filter(|&button| button != 0)
        .unwrap_or(IDCANCEL.0 as usize)
}

impl Drop for Dialog<'_> {
    fn drop(&mut self) {
        let icon = self.icon.get();
        if !icon.is_invalid() {
            // SAFETY: the icon was loaded for this dialog, whose window is
            // gone by now.
            unsafe {
                let _ = DestroyIcon(icon);
            }
        }
    }
}

impl Dialog<'_> {
    /// A dialog that is not open yet. `dark` and `api` say how it looks:
    /// what the main window goes by.
    fn new<'a>(
        owner: Option<HWND>,
        texts: (&'a str, &'a str, Option<&'a str>),
        tone: Tone,
        buttons: &'a [ButtonSpec<'a>],
        (dark, api): (bool, Option<&'a DarkApi>),
    ) -> Dialog<'a> {
        Dialog {
            title: texts.0,
            message: texts.1,
            detail: texts.2.filter(|detail| !detail.is_empty()),
            tone,
            buttons,
            owner,
            api,
            theme: Theme::new(dark),
            fonts: RefCell::new(None),
            icon: Cell::new(HICON::default()),
            icon_at: Cell::new(((0, 0), 0)),
            controls: RefCell::new(Controls::default()),
        }
    }

    /// WM_INITDIALOG: makes the controls, themes them, sizes and places
    /// the dialog, and puts the focus on the default button.
    fn init(&self, hwnd: HWND) {
        let api = self.api;
        let dark = self.theme.dark;
        crate::win::set_text(hwnd, "Docklet");
        // SAFETY: `hwnd` is the dialog being created. Its controls are laid
        // out here for the DPI it has; the dialog manager's own rescaling
        // would move them a second time.
        unsafe {
            let _ = SetDialogDpiChangeBehavior(hwnd, DDC_DISABLE_ALL, DDC_DISABLE_ALL);
        }
        style_frame(api, hwnd, dark);
        let detail_style = WS_TABSTOP.0 | WS_VSCROLL.0 | (ES_MULTILINE | ES_READONLY) as u32;
        let mut controls = Controls {
            // In reading order: the title, what follows from it, the
            // details, then the buttons.
            title: child(hwnd, WC_STATICW, self.title, SS_NOPREFIX, 0, 0),
            message: child(hwnd, WC_STATICW, self.message, SS_NOPREFIX, 0, 0),
            detail: match self.detail {
                // An edit control wants CRLF line ends.
                Some(detail) => {
                    let text = detail.replace("\r\n", "\n").replace('\n', "\r\n");
                    child(hwnd, WC_EDITW, &text, detail_style, 0, 0)
                }
                None => HWND::default(),
            },
            buttons: Vec::new(),
        };
        for button in self.buttons {
            let kind = if button.default {
                BS_DEFPUSHBUTTON
            } else {
                BS_PUSHBUTTON
            };
            let style = WS_TABSTOP.0 | kind as u32;
            let control = child(hwnd, WC_BUTTONW, button.label, style, 0, button.id);
            style_control(api, control, Look::Explorer, dark);
            controls.buttons.push(control);
        }
        if !controls.detail.is_invalid() {
            style_control(api, controls.detail, Look::Explorer, dark);
        }
        let default = self.buttons.iter().position(|button| button.default);
        let focus = default.and_then(|index| controls.buttons.get(index).copied());
        self.controls.replace(controls);
        // SAFETY: `hwnd` is this dialog.
        self.arrange(hwnd, Dpi(unsafe { GetDpiForWindow(hwnd) }.max(96)), None);
        if let (Some(index), Some(focus)) = (default, focus) {
            send(hwnd, DM_SETDEFID, self.buttons[index].id, 0);
            // SAFETY: the button is a child of this dialog.
            unsafe {
                let _ = SetFocus(Some(focus));
            }
        }
    }

    /// Gives the controls fonts for `dpi`, lays them out and sizes the
    /// dialog around them: centred on its owner, or at `corner` when the
    /// system says where it goes (it was dragged to another monitor).
    fn arrange(&self, hwnd: HWND, dpi: Dpi, corner: Option<Pair>) {
        let fonts = Fonts::new(dpi.0, hwnd);
        let controls = self.controls.borrow();
        send(controls.title, WM_SETFONT, fonts.title.0 as usize, 1);
        let texts = [controls.message, controls.detail];
        for control in texts.into_iter().chain(controls.buttons.iter().copied()) {
            if !control.is_invalid() {
                send(control, WM_SETFONT, fonts.ui.0 as usize, 1);
            }
        }
        let metrics = Metrics {
            margin: dpi.px(20),
            gap: dpi.px(12),
            icon: dpi.px(32),
            text: dpi.px(380),
            button_height: dpi.px(30),
        };
        let wrapped = DT_WORDBREAK | DT_NOPREFIX;
        let title = measure(hwnd, fonts.title, self.title, metrics.text, wrapped.0).1;
        let message = match self.message {
            "" => 0,
            message => measure(hwnd, fonts.ui, message, metrics.text, wrapped.0).1,
        };
        let detail = self.detail.map_or(0, |detail| {
            let flags = wrapped | DT_EDITCONTROL;
            // Less the scroll bar; a few lines at least, and not so many
            // that the dialog outgrows a small screen.
            let room = metrics.text - dpi.px(20);
            let height = measure(hwnd, fonts.ui, detail, room, flags.0).1 + dpi.px(8);
            height.clamp(dpi.px(56), dpi.px(220))
        });
        let widths: Vec<i32> = self
            .buttons
            .iter()
            .map(|button| {
                let label = measure(hwnd, fonts.ui, button.label, 0, DT_SINGLELINE.0).0;
                (label + dpi.px(32)).max(dpi.px(88))
            })
            .collect();
        let layout = layout([title, message, detail], &widths, metrics);
        place(
            controls.title,
            layout.title.0,
            layout.title.1,
            metrics.text,
            title,
        );
        place(
            controls.message,
            layout.message.0,
            layout.message.1,
            metrics.text,
            message,
        );
        show(controls.title, true);
        show(controls.message, message > 0);
        if !controls.detail.is_invalid() {
            place(
                controls.detail,
                layout.detail.0,
                layout.detail.1,
                metrics.text,
                detail,
            );
            show(controls.detail, true);
        }
        let buttons = controls.buttons.iter().zip(&layout.buttons).zip(&widths);
        for ((&button, &left), &width) in buttons {
            place(
                button,
                left,
                layout.button_row,
                width,
                metrics.button_height,
            );
            show(button, true);
        }
        drop(controls);
        self.icon_at.set((layout.icon, metrics.icon));
        self.load_icon(metrics.icon);
        // The controls have the new fonts; the old ones can go.
        drop(self.fonts.replace(Some(fonts)));

        let mut frame = RECT {
            left: 0,
            top: 0,
            right: layout.client.0,
            bottom: layout.client.1,
        };
        // SAFETY: `frame` is a valid in/out rectangle; the calls after it
        // query and move this dialog and read its owner's place.
        unsafe {
            // With the styles the window really has: the dialog manager
            // adds some of its own to those of the template.
            let style = WINDOW_STYLE(GetWindowLongPtrW(hwnd, GWL_STYLE) as u32);
            let ex_style = WINDOW_EX_STYLE(GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32);
            let _ = AdjustWindowRectExForDpi(&mut frame, style, false, ex_style, dpi.0);
            let size = (frame.right - frame.left, frame.bottom - frame.top);
            let (x, y) = corner.unwrap_or_else(|| self.place_for(hwnd, size));
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                size.0,
                size.1,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        crate::win::invalidate(hwnd);
    }

    /// Where a dialog of `size` goes: over the middle of its owner, or of
    /// the monitor the pointer is on when there is no window to sit on.
    fn place_for(&self, hwnd: HWND, size: Pair) -> Pair {
        // SAFETY: plain queries with valid out pointers.
        unsafe {
            let owner = self
                .owner
                .filter(|&owner| IsWindowVisible(owner).as_bool() && !IsIconic(owner).as_bool());
            let mut info = MONITORINFO {
                cbSize: size_of::<MONITORINFO>() as u32,
                ..Default::default()
            };
            let mut over = RECT::default();
            let monitor = match owner {
                Some(owner) => {
                    let _ = GetWindowRect(owner, &mut over);
                    MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST)
                }
                None => {
                    let mut pointer = POINT::default();
                    let _ = GetCursorPos(&mut pointer);
                    MonitorFromPoint(pointer, MONITOR_DEFAULTTONEAREST)
                }
            };
            if !GetMonitorInfoW(monitor, &mut info).as_bool() {
                let _ = GetWindowRect(hwnd, &mut info.rcWork);
            }
            if owner.is_none() {
                over = info.rcWork;
            }
            centred(over, size, info.rcWork)
        }
    }

    fn load_icon(&self, size: i32) {
        let name = match self.tone {
            Tone::Info => IDI_INFORMATION,
            Tone::Warning => IDI_WARNING,
            Tone::Error => IDI_ERROR,
        };
        // SAFETY: a stock icon scaled to the size asked for; the one from
        // before, for another DPI, is destroyed.
        unsafe {
            let icon = LoadIconWithScaleDown(None, name, size, size).unwrap_or_default();
            let old = self.icon.replace(icon);
            if !old.is_invalid() {
                let _ = DestroyIcon(old);
            }
        }
    }

    fn paint(&self, hwnd: HWND) {
        let ((x, y), size) = self.icon_at.get();
        let mut paint = PAINTSTRUCT::default();
        // SAFETY: the usual paint bracket for this dialog.
        unsafe {
            let hdc = BeginPaint(hwnd, &mut paint);
            let icon = self.icon.get();
            if !icon.is_invalid() {
                let _ = DrawIconEx(hdc, x, y, icon, size, size, 0, None, DI_NORMAL);
            }
            let _ = EndPaint(hwnd, &paint);
        }
    }

    /// The colours of the dialog and of a control in it.
    fn colors(&self, message: u32, hdc: HDC, control: HWND) -> HBRUSH {
        let theme = &self.theme;
        let boxed = message == WM_CTLCOLOREDIT || control == self.controls.borrow().detail;
        let (background, brush) = if boxed && message != WM_CTLCOLORDLG {
            (theme.surface, theme.surface_brush)
        } else {
            (theme.background, theme.background_brush)
        };
        // SAFETY: `hdc` is the control's DC for this message.
        unsafe {
            SetTextColor(hdc, theme.text);
            SetBkColor(hdc, background);
        }
        brush
    }
}

/// The size of `text` in `font`: wrapped to `width` with `DT_WORDBREAK` in
/// `flags`, on one line without.
fn measure(hwnd: HWND, font: HFONT, text: &str, width: i32, flags: u32) -> Pair {
    let mut units: Vec<u16> = text.encode_utf16().collect();
    if units.is_empty() {
        units.push(u16::from(b' '));
    }
    let mut rect = RECT {
        right: width,
        ..Default::default()
    };
    let format = windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT(flags) | DT_CALCRECT;
    // SAFETY: the DC is released and the font selected back out.
    unsafe {
        let hdc = GetDC(Some(hwnd));
        let previous = SelectObject(hdc, font.into());
        DrawTextW(hdc, &mut units, &mut rect, format);
        SelectObject(hdc, previous);
        ReleaseDC(Some(hwnd), hdc);
    }
    (rect.right, rect.bottom)
}

unsafe extern "system" fn dialog_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> isize {
    if message == WM_INITDIALOG {
        // SAFETY: `lparam` is the `Dialog` that `run` handed over, which
        // outlives the dialog's window.
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, lparam.0);
            (*(lparam.0 as *const Dialog)).init(hwnd);
        }
        // The focus was set by hand.
        return 0;
    }
    // SAFETY: the user data is null or the `Dialog` of `run`, which is on
    // the stack for as long as the window exists.
    let dialog = unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Dialog).as_ref() };
    let Some(dialog) = dialog else {
        return 0;
    };
    match message {
        WM_CTLCOLORDLG | WM_CTLCOLORSTATIC | WM_CTLCOLORBTN | WM_CTLCOLOREDIT => {
            let hdc = HDC(wparam.0 as *mut c_void);
            let control = HWND(lparam.0 as *mut c_void);
            // For these messages a dialog procedure returns the brush.
            dialog.colors(message, hdc, control).0 as isize
        }
        WM_PAINT => {
            dialog.paint(hwnd);
            1
        }
        WM_COMMAND => {
            // Escape and the close box arrive as IDCANCEL, Enter as the
            // default button.
            let id = low_word(wparam.0) as usize;
            let known = dialog.buttons.iter().any(|button| button.id == id);
            if known || id == IDCANCEL.0 as usize {
                // SAFETY: ends this dialog's modal loop.
                unsafe {
                    let _ = EndDialog(hwnd, id as isize);
                }
            }
            1
        }
        WM_DPICHANGED => {
            // SAFETY: for this message `lparam` points to the suggested RECT.
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            let corner = (suggested.left, suggested.top);
            dialog.arrange(hwnd, Dpi(high_word(wparam.0).max(96)), Some(corner));
            1
        }
        WM_NCDESTROY => {
            // SAFETY: no later message reaches the dialog's state.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            0
        }
        _ => 0,
    }
}

/// The buttons of a question: the one that agrees, then Cancel, which is
/// the default, so Enter alone removes nothing.
fn confirm_buttons(accept: &str) -> [ButtonSpec<'_>; 2] {
    [
        ButtonSpec {
            id: ACCEPT,
            label: accept,
            default: false,
        },
        ButtonSpec {
            id: IDCANCEL.0 as usize,
            label: "Cancel",
            default: true,
        },
    ]
}

/// The one button of a message.
const NOTICE_BUTTONS: [ButtonSpec<'static>; 1] = [ButtonSpec {
    id: IDOK.0 as usize,
    label: "OK",
    default: true,
}];

/// Asks `confirm`'s question. True when the user chose the accept button;
/// Escape, the close box and Enter (Cancel is the default) all say no.
pub fn confirm(owner: Option<HWND>, confirm: &Confirm) -> bool {
    let ctx = ctx();
    let look = (ctx.dark(), ctx.dark_api.as_ref());
    let buttons = confirm_buttons(&confirm.accept);
    let texts = (confirm.title.as_str(), confirm.message.as_str(), None);
    run(&Dialog::new(owner, texts, Tone::Warning, &buttons, look)) == ACCEPT
}

/// Shows a result. Returns when the user closes it.
pub fn notice(owner: Option<HWND>, notice: &Notice) {
    let ctx = ctx();
    let look = (ctx.dark(), ctx.dark_api.as_ref());
    let texts = (
        notice.title.as_str(),
        notice.message.as_str(),
        notice.detail.as_deref(),
    );
    run(&Dialog::new(
        owner,
        texts,
        notice.tone,
        &NOTICE_BUTTONS,
        look,
    ));
}

#[cfg(test)]
mod tests {
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateDialogIndirectParamW, DestroyWindow, GetClientRect, GetDlgItem,
    };

    use super::*;
    use crate::win::{client_rect, text_of};

    /// The rectangle of `control` in the client area of `dialog`.
    fn rect_in(dialog: HWND, control: HWND) -> RECT {
        let mut rect = RECT::default();
        // SAFETY: plain queries of windows made by the test.
        unsafe {
            let _ = GetWindowRect(control, &mut rect);
            let mut corners = [
                POINT {
                    x: rect.left,
                    y: rect.top,
                },
                POINT {
                    x: rect.right,
                    y: rect.bottom,
                },
            ];
            for corner in &mut corners {
                let _ = windows::Win32::Graphics::Gdi::ScreenToClient(dialog, corner);
            }
            RECT {
                left: corners[0].x,
                top: corners[0].y,
                right: corners[1].x,
                bottom: corners[1].y,
            }
        }
    }

    /// Makes the dialog as a window that is never shown, so what its
    /// procedure builds can be looked at, and hands it to `look`. No modal
    /// loop runs and nothing appears on screen.
    fn with_hidden(dialog: &Dialog<'_>, look: impl FnOnce(HWND, &Controls)) {
        let template = Template::new();
        let state = std::ptr::from_ref(dialog) as isize;
        // SAFETY: as in `run`, without the modal loop: the window is
        // destroyed before the template and the dialog's state go.
        unsafe {
            let hwnd = CreateDialogIndirectParamW(
                Some(instance()),
                template.as_ptr(),
                None,
                Some(dialog_proc),
                LPARAM(state),
            )
            .expect("the dialog is created");
            assert!(!IsWindowVisible(hwnd).as_bool());
            look(hwnd, &dialog.controls.borrow());
            let _ = DestroyWindow(hwnd);
        }
    }

    #[test]
    fn a_question_is_built_with_its_texts_and_cancel_as_the_default() {
        let buttons = confirm_buttons("Remove 3");
        let texts = (
            "Remove 3 images?",
            "2 of the 5 selected are in use and will be kept. This cannot be undone.",
            None,
        );
        let dialog = Dialog::new(None, texts, Tone::Warning, &buttons, (true, None));
        with_hidden(&dialog, |hwnd, controls| {
            assert_eq!(text_of(hwnd), "Docklet");
            assert_eq!(text_of(controls.title), "Remove 3 images?");
            assert!(text_of(controls.message).ends_with("This cannot be undone."));
            assert!(controls.detail.is_invalid());
            let labels: Vec<String> = controls.buttons.iter().map(|&b| text_of(b)).collect();
            assert_eq!(labels, ["Remove 3", "Cancel"]);
            // The buttons answer to their numbers, and Cancel is what
            // Enter presses.
            // SAFETY: plain queries of the dialog.
            unsafe {
                assert_eq!(
                    GetDlgItem(Some(hwnd), ACCEPT as i32).ok(),
                    Some(controls.buttons[0])
                );
                assert_eq!(
                    GetDlgItem(Some(hwnd), IDCANCEL.0).ok(),
                    Some(controls.buttons[1])
                );
            }
            let default = send(
                hwnd,
                windows::Win32::UI::WindowsAndMessaging::DM_GETDEFID,
                0,
                0,
            );
            assert_eq!(low_word(default as usize) as i32, IDCANCEL.0);

            // Everything lies inside the dialog, the texts above the
            // buttons, the buttons side by side and ending at the margin.
            let client = client_rect(hwnd);
            let inside = |rect: RECT| {
                rect.left >= 0
                    && rect.top >= 0
                    && rect.right <= client.right
                    && rect.bottom <= client.bottom
                    && rect.right > rect.left
                    && rect.bottom > rect.top
            };
            let title = rect_in(hwnd, controls.title);
            let message = rect_in(hwnd, controls.message);
            let accept = rect_in(hwnd, controls.buttons[0]);
            let cancel = rect_in(hwnd, controls.buttons[1]);
            for rect in [title, message, accept, cancel] {
                assert!(inside(rect), "{rect:?} in {client:?}");
            }
            assert!(title.bottom <= message.top);
            assert!(message.bottom <= accept.top);
            assert_eq!(accept.top, cancel.top);
            assert!(accept.right < cancel.left);
            // The margin right of the last button is the one left of the
            // icon.
            let ((margin, _), icon) = dialog.icon_at.get();
            assert_eq!(client.right - cancel.right, margin);
            assert!(title.left >= margin + icon);
        });
    }

    #[test]
    fn a_result_with_details_shows_them_in_a_box_between_text_and_button() {
        let detail = "nginx:latest: conflict\nredis:7: conflict";
        let texts = ("Removed 3 of 5 images", "2 are in use: a, b.", Some(detail));
        let dialog = Dialog::new(None, texts, Tone::Warning, &NOTICE_BUTTONS, (false, None));
        with_hidden(&dialog, |hwnd, controls| {
            assert_eq!(text_of(controls.detail), detail.replace('\n', "\r\n"));
            let mut client = RECT::default();
            // SAFETY: a plain query of the dialog.
            unsafe {
                let _ = GetClientRect(hwnd, &mut client);
            }
            let message = rect_in(hwnd, controls.message);
            let detail = rect_in(hwnd, controls.detail);
            let ok = rect_in(hwnd, controls.buttons[0]);
            assert!(message.bottom <= detail.top);
            assert!(detail.bottom <= ok.top);
            assert!(ok.bottom <= client.bottom && detail.right <= client.right);
            assert_eq!(text_of(controls.buttons[0]), "OK");
        });
        // No message under the title: the control is there, without room.
        let texts = ("Image a:1 is in use", "", None);
        let dialog = Dialog::new(None, texts, Tone::Info, &NOTICE_BUTTONS, (true, None));
        with_hidden(&dialog, |hwnd, controls| {
            let message = rect_in(hwnd, controls.message);
            assert_eq!(message.bottom - message.top, 0);
            assert!(rect_in(hwnd, controls.title).bottom <= rect_in(hwnd, controls.buttons[0]).top);
        });
    }

    const METRICS: Metrics = Metrics {
        margin: 20,
        gap: 10,
        icon: 32,
        text: 300,
        button_height: 30,
    };

    #[test]
    fn the_texts_stack_beside_the_icon_and_the_buttons_end_at_the_right() {
        let placed = layout([40, 36, 0], &[90, 80], METRICS);
        assert_eq!(placed.icon, (20, 20));
        assert_eq!(placed.title, (62, 20));
        assert_eq!(placed.message, (62, 70));
        // Width: margin, icon, gap, text, margin.
        assert_eq!(placed.client.0, 382);
        // The last button ends at the right margin, the one before it a
        // gap to its left.
        assert_eq!(placed.buttons, [182, 282]);
        assert_eq!(placed.button_row, 126);
        assert_eq!(placed.client.1, 176);
    }

    #[test]
    fn a_part_that_is_missing_takes_no_room() {
        let with = layout([40, 36, 100], &[80], METRICS);
        assert_eq!(with.detail, (62, 116));
        assert_eq!(with.button_row, 236);
        let without = layout([40, 0, 100], &[80], METRICS);
        assert_eq!(without.detail, (62, 70));
        assert_eq!(without.button_row, 190);
    }

    #[test]
    fn a_short_text_still_leaves_the_buttons_below_the_icon() {
        // A one-line title is shorter than the icon.
        let placed = layout([16, 0, 0], &[80], METRICS);
        assert_eq!(placed.button_row, 20 + 32 + 10 + 10);
        assert!(placed.buttons.iter().all(|&left| left >= 0));
        assert!(layout([16, 0, 0], &[], METRICS).buttons.is_empty());
    }

    #[test]
    fn a_dialog_sits_on_the_middle_of_its_owner_within_the_screen() {
        let screen = RECT {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        let owner = RECT {
            left: 100,
            top: 100,
            right: 1100,
            bottom: 700,
        };
        assert_eq!(centred(owner, (400, 200), screen), (400, 300));
        // An owner half off the screen: the dialog stays on it.
        let off = RECT {
            left: 1700,
            top: 900,
            right: 2700,
            bottom: 1500,
        };
        assert_eq!(centred(off, (400, 200), screen), (1520, 840));
        let above = RECT {
            left: -600,
            top: -300,
            right: 100,
            bottom: 50,
        };
        assert_eq!(centred(above, (400, 200), screen), (0, 0));
        // A second monitor left of the first has negative coordinates.
        let left_screen = RECT {
            left: -1920,
            top: 0,
            right: 0,
            bottom: 1040,
        };
        assert_eq!(centred(left_screen, (400, 200), left_screen), (-1160, 420));
        // Larger than the screen: its top-left corner is what shows.
        assert_eq!(centred(screen, (3000, 2000), screen), (0, 0));
    }
}
