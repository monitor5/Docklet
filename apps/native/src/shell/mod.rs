//! The main window: creates the widgets, lays them out, follows the theme
//! and DPI, and routes input to the parts in `tables` and `logs`.
//!
//! The window is created on demand and destroyed on close; only the host
//! stays. Its state lives in a `Box<Shell>` whose pointer is the window's
//! user data from creation until `WM_NCDESTROY`.
//!
//! Window procedures re-enter (a message sent to the list comes back as
//! `WM_NOTIFY`), so no `RefCell` is borrowed mutably across a call into
//! Win32, and the model is only ever borrowed immutably here.

mod logs;
mod tables;

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::rc::Rc;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, FillRect, HBRUSH, HDC, InvalidateRect, PAINTSTRUCT, RDW_ALLCHILDREN,
    RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow, SetBkColor, SetTextColor,
};
use windows::Win32::UI::Controls::{
    LVM_SETBKCOLOR, LVM_SETTEXTBKCOLOR, LVM_SETTEXTCOLOR, WC_BUTTONW, WC_STATICW,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    EnableWindow, GetActiveWindow, GetFocus, SetFocus, VK_F, VK_F5, VK_TAB,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, GWLP_USERDATA, GetWindowLongPtrW,
    GetWindowPlacement, IDCANCEL, IDOK, IsChild, IsIconic, KillTimer, MINMAXINFO, MSG,
    SIZE_MINIMIZED, SW_RESTORE, SW_SHOW, SW_SHOWMINIMIZED, SWP_NOACTIVATE, SWP_NOMOVE,
    SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW, SetWindowPlacement, SetWindowPos,
    ShowWindow, WA_INACTIVE, WINDOWPLACEMENT, WM_ACTIVATE, WM_COMMAND, WM_CONTEXTMENU,
    WM_CTLCOLORBTN, WM_CTLCOLOREDIT, WM_CTLCOLORSTATIC, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND,
    WM_GETMINMAXINFO, WM_KEYDOWN, WM_KILLFOCUS, WM_MOUSEWHEEL, WM_NCDESTROY, WM_NOTIFY, WM_PAINT,
    WM_SETFOCUS, WM_SETFONT, WM_SETTINGCHANGE, WM_SIZE, WM_TIMER, WS_CLIPCHILDREN,
    WS_EX_CONTROLPARENT, WS_OVERLAPPEDWINDOW, WS_TABSTOP,
};
use windows::core::{PCWSTR, w};

use crate::boundary::{Page, Tone, UiEvent};
use crate::host::ctx;
use crate::theme::{Fonts, Look, Theme, style_control, style_frame};
use crate::widgets::banner::BannerBar;
use crate::widgets::filter::FilterBox;
use crate::widgets::log_view::LogView;
use crate::widgets::message::MessageScreen;
use crate::widgets::selection_bar::SelectionBar;
use crate::widgets::table::TableView;
use crate::widgets::table_state::TableState;
use crate::widgets::tabs::TabStrip;
use crate::widgets::usage::UsageStrip;
use crate::win::{
    Dpi, child, client_rect, high_word, invalidate, low_word, place, register_class, send,
    set_text, show as show_window,
};

const MAIN_CLASS: PCWSTR = w!("Docklet.Native.Main");
const LOG_CLASS: PCWSTR = w!("Docklet.Native.Log");
const USAGE_CLASS: PCWSTR = w!("Docklet.Native.Usage");

mod ids {
    pub const TABS: usize = 100;
    pub const FILTER: usize = 101;
    pub const LIST: usize = 102;
    pub const REMOVE: usize = 103;
    pub const CLEAR: usize = 104;
    pub const HEADER_BUTTON: usize = 105;
    pub const BANNER_DISMISS: usize = 106;
    pub const LOG_JUMP: usize = 107;
    pub const LOG_CLOSE: usize = 108;
    pub const LOG: usize = 109;
    pub const PRIMARY: usize = 110;
    pub const SECONDARY: usize = 111;
    pub const DETAIL: usize = 112;
    pub const USAGE: usize = 113;
}

const SS_RIGHT: u32 = 0x0002;
const SS_CENTERIMAGE: u32 = 0x0200;
const SS_ENDELLIPSIS: u32 = 0x4000;

/// What fills the area under the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Message,
    Tables,
    Log,
}

pub(crate) struct Shell {
    hwnd: HWND,
    theme: RefCell<Rc<Theme>>,
    fonts: RefCell<Rc<Fonts>>,
    dpi: Cell<Dpi>,

    tabs: TabStrip,
    status: HWND,
    header_button: HWND,
    banner: BannerBar,
    /// The banner's strip, painted in its colour behind the controls.
    banner_rect: Cell<RECT>,
    usage: UsageStrip,

    filter: FilterBox,
    bar: SelectionBar,
    table: TableView,

    log_title: HWND,
    log_status: HWND,
    log_jump: HWND,
    log_close: HWND,
    log: LogView,

    message: MessageScreen,

    tab: Cell<usize>,
    /// Filter, sort, checks and collapsed groups, per tab.
    states: RefCell<Vec<TableState>>,
    /// The filter text is being set by code, so its change is not news.
    quiet_filter: Cell<bool>,
    log_open: Cell<bool>,
    /// Shown and not minimised.
    visible: Cell<bool>,
    timer: Cell<bool>,
    ticks: Cell<u32>,
    last_focus: Cell<Option<HWND>>,
}

pub(crate) fn register_classes() {
    register_class(MAIN_CLASS, Some(main_proc));
    register_class(LOG_CLASS, Some(logs::log_proc));
    register_class(USAGE_CLASS, Some(usage_proc));
}

fn shell_of<'a>(hwnd: HWND) -> Option<&'a Shell> {
    // SAFETY: the user data is null or the `Box<Shell>` leaked in `create`,
    // which is freed only in `WM_NCDESTROY`, after it was reset to null.
    unsafe { (GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const Shell).as_ref() }
}

/// The main window's shell, while the window exists.
pub(crate) fn current() -> Option<&'static Shell> {
    ctx().main.get().and_then(shell_of)
}

/// Shows the main window, creating it if it does not exist.
pub(crate) fn show() {
    if let Some(hwnd) = ctx().main.get() {
        // SAFETY: `hwnd` is the live main window of this thread.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        return;
    }
    create();
}

fn create() {
    let ctx = ctx();
    // SAFETY: the class was registered in `register_classes`.
    let created = unsafe {
        CreateWindowExW(
            WS_EX_CONTROLPARENT,
            MAIN_CLASS,
            w!("Docklet"),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            None,
            None,
            Some(crate::win::instance()),
            None,
        )
    };
    let Ok(hwnd) = created else {
        return;
    };
    // SAFETY: `hwnd` was just created on this thread.
    let dpi = Dpi(unsafe { GetDpiForWindow(hwnd) }.max(96));
    let static_text = |style: u32| child(hwnd, WC_STATICW, "", style, 0, 0);
    let button = |label: &str, id: usize| child(hwnd, WC_BUTTONW, label, WS_TABSTOP.0, 0, id);

    let shell = Box::new(Shell {
        hwnd,
        theme: RefCell::new(Rc::new(Theme::new(ctx.dark()))),
        fonts: RefCell::new(Rc::new(Fonts::new(dpi.0, hwnd))),
        dpi: Cell::new(dpi),
        tabs: TabStrip::create(hwnd, ids::TABS),
        status: static_text(SS_RIGHT | SS_CENTERIMAGE | SS_ENDELLIPSIS),
        header_button: button("", ids::HEADER_BUTTON),
        banner: BannerBar::create(hwnd, ids::BANNER_DISMISS),
        banner_rect: Cell::new(RECT::default()),
        usage: UsageStrip::create(hwnd, USAGE_CLASS, ids::USAGE),
        filter: FilterBox::create(hwnd, ids::FILTER),
        bar: SelectionBar::create(hwnd, ids::REMOVE, ids::CLEAR),
        table: TableView::create(hwnd, ids::LIST),
        log_title: static_text(SS_CENTERIMAGE | SS_ENDELLIPSIS),
        log_status: static_text(SS_RIGHT | SS_CENTERIMAGE | SS_ENDELLIPSIS),
        log_jump: button("Jump to end", ids::LOG_JUMP),
        log_close: button("Close", ids::LOG_CLOSE),
        log: LogView::create(hwnd, LOG_CLASS, ids::LOG),
        message: MessageScreen::create(hwnd, ids::PRIMARY, ids::SECONDARY, ids::DETAIL),
        tab: Cell::new(0),
        states: RefCell::new(Vec::new()),
        quiet_filter: Cell::new(false),
        log_open: Cell::new(false),
        visible: Cell::new(false),
        timer: Cell::new(false),
        ticks: Cell::new(0),
        last_focus: Cell::new(None),
    });
    let shell: &'static Shell = Box::leak(shell);
    let data = std::ptr::from_ref(shell) as usize;
    // SAFETY: the pointer stays valid until `WM_NCDESTROY` frees it; the
    // subclassed controls are children, destroyed before that.
    unsafe {
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, data as isize);
        let _ = SetWindowSubclass(shell.tabs.hwnd, Some(tabs_proc), 0, data);
        let _ = SetWindowSubclass(shell.table.list, Some(tables::list_proc), 0, data);
    }
    ctx.main.set(Some(hwnd));

    shell.apply_fonts();
    shell.apply_theme();
    shell.tab.set(ctx.last_tab.get());
    shell.header_changed();
    shell.banner_changed();
    shell.usage_changed();
    shell.tabs_changed();
    shell.page_changed();

    let placement = ctx.placement.get();
    // SAFETY: `hwnd` is the window just created.
    unsafe {
        match placement {
            Some(mut placement) => {
                if placement.showCmd == SW_SHOWMINIMIZED.0 as u32 {
                    placement.showCmd = SW_RESTORE.0 as u32;
                }
                let _ = SetWindowPlacement(hwnd, &placement);
            }
            None => {
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    0,
                    0,
                    dpi.px(1100),
                    dpi.px(720),
                    SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
                );
            }
        }
    }
    shell.layout();
    // SAFETY: as above.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }
    shell.focus_default();
    if ctx.model.borrow().log.is_some() {
        shell.open_log();
    }
    shell.set_visible(true);
}

impl Shell {
    fn theme(&self) -> Rc<Theme> {
        self.theme.borrow().clone()
    }

    fn fonts(&self) -> Rc<Fonts> {
        self.fonts.borrow().clone()
    }

    fn view(&self) -> View {
        match ctx().model.borrow().page {
            Page::Message(_) => View::Message,
            Page::Tables if self.log_open.get() => View::Log,
            Page::Tables => View::Tables,
        }
    }

    fn buttons(&self) -> Vec<HWND> {
        vec![
            self.header_button,
            self.banner.dismiss,
            self.bar.remove,
            self.bar.clear,
            self.log_jump,
            self.log_close,
            self.message.primary,
            self.message.secondary,
        ]
    }

    /// Gives every control the theme that matches the system now.
    fn apply_theme(&self) {
        let ctx = ctx();
        let theme = Rc::new(Theme::new(ctx.dark()));
        let api = ctx.dark_api.as_ref();
        let dark = theme.dark;
        style_frame(api, self.hwnd, dark);
        style_control(api, self.table.list, Look::List, dark);
        style_control(api, self.table.header, Look::Header, dark);
        style_control(api, self.filter.edit, Look::Edit, dark);
        style_control(api, self.log.hwnd, Look::Explorer, dark);
        style_control(api, self.message.detail, Look::Explorer, dark);
        for button in self.buttons() {
            style_control(api, button, Look::Explorer, dark);
        }
        send(self.table.list, LVM_SETBKCOLOR, 0, theme.surface.0 as isize);
        send(
            self.table.list,
            LVM_SETTEXTBKCOLOR,
            0,
            theme.surface.0 as isize,
        );
        send(self.table.list, LVM_SETTEXTCOLOR, 0, theme.text.0 as isize);
        // The old brushes go once nothing paints with them any more.
        let old = self.theme.replace(theme);
        // SAFETY: repainting this window and its children.
        unsafe {
            let _ = RedrawWindow(
                Some(self.hwnd),
                None,
                None,
                RDW_ERASE | RDW_FRAME | RDW_INVALIDATE | RDW_ALLCHILDREN,
            );
        }
        drop(old);
    }

    /// Makes fonts for the current DPI and hands them to every control.
    fn apply_fonts(&self) {
        let dpi = self.dpi.get();
        let fonts = Rc::new(Fonts::new(dpi.0, self.hwnd));
        let ui = fonts.ui.0 as usize;
        let mut controls = vec![
            self.tabs.hwnd,
            self.status,
            self.banner.text,
            self.filter.label,
            self.filter.edit,
            self.bar.summary,
            self.table.list,
            self.log_title,
            self.log_status,
            self.message.body,
            self.message.detail,
        ];
        controls.extend(self.buttons());
        for control in controls {
            send(control, WM_SETFONT, ui, 1);
        }
        send(self.message.title, WM_SETFONT, fonts.title.0 as usize, 1);
        send(self.log_title, WM_SETFONT, fonts.bold.0 as usize, 1);
        self.log.set_fonts(&fonts);
        let old = self.fonts.replace(fonts);
        drop(old);
        self.table.set_dpi(dpi);
        self.rebuild_table();
    }

    /// Shows the controls of the current view and hides the others.
    fn apply_visibility(&self) {
        let view = self.view();
        let (has_header_button, has_banner, has_usage, has_tabs) = {
            let model = ctx().model.borrow();
            (
                model.header.button.is_some(),
                model.banner.is_some(),
                !model.usage.is_empty(),
                !model.tabs.is_empty(),
            )
        };
        let checked = self.checked_count() > 0;
        show_window(self.tabs.hwnd, view != View::Message && has_tabs);
        show_window(self.status, true);
        show_window(self.header_button, has_header_button);
        show_window(self.banner.text, has_banner);
        show_window(self.banner.dismiss, has_banner);
        show_window(self.usage.hwnd, has_usage);
        let tables = view == View::Tables;
        for control in self.filter.controls() {
            show_window(control, tables);
        }
        self.bar.set_visible(tables, checked);
        show_window(self.table.list, tables);
        let log = view == View::Log;
        for control in [
            self.log_title,
            self.log_status,
            self.log_jump,
            self.log_close,
            self.log.hwnd,
        ] {
            show_window(control, log);
        }
        self.message.set_visible(view == View::Message);
        // Focus left in a hidden control would leave the keyboard nowhere.
        // SAFETY: a plain focus query on this thread.
        let focus = unsafe { GetFocus() };
        if focus.is_invalid() || !crate::win::is_visible(focus) {
            self.focus_default();
        }
    }

    /// Whether this window is the active one, so moving the focus inside it
    /// cannot pull it in front of what the user is doing.
    fn is_active(&self) -> bool {
        // SAFETY: a plain query on this thread.
        unsafe { GetActiveWindow() == self.hwnd }
    }

    /// Moves the focus to `target`, only while the window is active. When it
    /// is not, WM_ACTIVATE puts the focus somewhere visible on return.
    fn focus(&self, target: HWND) {
        if self.is_active() {
            // SAFETY: `target` is this window or one of its children.
            unsafe {
                let _ = SetFocus(Some(target));
            }
        }
    }

    /// Focus for a fresh view: the table, the log, or the first button.
    fn focus_default(&self) {
        let target = match self.view() {
            View::Tables => self.table.list,
            View::Log => self.log.hwnd,
            View::Message if self.message.key_of(true).is_some() => self.message.primary,
            View::Message if self.message.key_of(false).is_some() => self.message.secondary,
            View::Message => self.hwnd,
        };
        self.focus(target);
    }

    fn layout(&self) {
        let RECT {
            right: width,
            bottom: height,
            ..
        } = client_rect(self.hwnd);
        let dpi = self.dpi.get();
        let px = |dip| dpi.px(dip);
        let margin = px(12);
        let (has_header_button, has_banner, has_usage) = {
            let model = ctx().model.borrow();
            (
                model.header.button.is_some(),
                model.banner.is_some(),
                !model.usage.is_empty(),
            )
        };

        // Header: tabs on the left; status and its button on the right.
        let row = px(30);
        let header_top = px(6);
        let button_width = if has_header_button { px(120) } else { 0 };
        let button_x = width - margin - button_width;
        place(self.header_button, button_x, header_top, button_width, row);
        let status_width = px(220);
        let status_x = button_x - px(8) - status_width;
        place(self.status, status_x, header_top, status_width, row);
        place(
            self.tabs.hwnd,
            margin,
            px(4),
            (status_x - margin - px(8)).max(0),
            px(34),
        );
        let mut y = px(44);

        if has_banner {
            let banner = RECT {
                left: margin,
                top: y,
                right: width - margin,
                bottom: y + px(36),
            };
            self.banner
                .layout(banner.left, banner.right, banner.top, px(36), dpi);
            self.banner_rect.set(banner);
            y = banner.bottom + px(8);
        } else {
            self.banner_rect.set(RECT::default());
        }
        if has_usage {
            place(self.usage.hwnd, margin, y, width - 2 * margin, px(40));
            y += px(40) + px(8);
        }

        // Tables: the filter and the selection bar above the list.
        let label = px(44);
        let filter_width = px(260);
        // A single-line edit does not centre its text, so it gets its own
        // height, centred in the row.
        let edit_height = px(26);
        place(self.filter.label, margin, y, label, row);
        place(
            self.filter.edit,
            margin + label,
            y + (row - edit_height) / 2,
            filter_width,
            edit_height,
        );
        let bar_left = margin + label + filter_width + px(12);
        self.bar.layout(
            bar_left,
            width - margin,
            y,
            row,
            dpi,
            self.checked_count() > 0,
        );
        let body = y + row + px(8);
        place(
            self.table.list,
            margin,
            body,
            width - 2 * margin,
            height - body - margin,
        );
        self.table.fit_columns(dpi);

        // Log: title, status and buttons above the view.
        let close = px(84);
        let jump = px(120);
        let log_status = px(260);
        let close_x = width - margin - close;
        let jump_x = close_x - px(8) - jump;
        let log_status_x = jump_x - px(8) - log_status;
        place(
            self.log_title,
            margin,
            y,
            (log_status_x - margin - px(8)).max(0),
            row,
        );
        place(self.log_status, log_status_x, y, log_status, row);
        place(self.log_jump, jump_x, y, jump, row);
        place(self.log_close, close_x, y, close, row);
        place(
            self.log.hwnd,
            margin,
            body,
            width - 2 * margin,
            height - body - margin,
        );
        self.log.resize();

        let fonts = self.fonts();
        self.message.layout(
            RECT {
                left: 0,
                top: y,
                right: width,
                bottom: height,
            },
            dpi,
            (fonts.title, fonts.ui),
        );
        // SAFETY: repainting this window's own background.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, true);
        }
    }

    /// The window was shown or restored (true), or minimised or closed.
    fn set_visible(&self, visible: bool) {
        if self.visible.replace(visible) == visible {
            return;
        }
        self.message.set_animating(visible);
        self.update_timer();
        if !visible {
            self.log.release();
        }
        ctx().emit(if visible {
            UiEvent::WindowShown
        } else {
            UiEvent::WindowHidden
        });
    }

    pub(crate) fn page_changed(&self) {
        let screen = match &ctx().model.borrow().page {
            Page::Message(screen) => Some(screen.clone()),
            Page::Tables => None,
        };
        if let Some(screen) = &screen {
            // A log belongs to the tables; a message screen replaces both.
            if self.log_open.get() {
                ctx().model.borrow_mut().log = None;
                self.close_log();
            }
            self.message.set(screen);
        }
        self.apply_visibility();
        self.layout();
    }

    pub(crate) fn header_changed(&self) {
        let header = ctx().model.borrow().header.clone();
        set_text(self.status, &header.status);
        if let Some(button) = &header.button {
            set_text(self.header_button, &button.label);
        }
        self.apply_visibility();
        self.layout();
    }

    pub(crate) fn banner_changed(&self) {
        let banner = ctx().model.borrow().banner.clone();
        self.banner.set(banner.as_ref());
        self.apply_visibility();
        self.layout();
    }

    pub(crate) fn usage_changed(&self) {
        let (meters_shown, was_shown) = {
            let model = ctx().model.borrow();
            self.usage.set(&model.usage);
            (
                !model.usage.is_empty(),
                crate::win::is_visible(self.usage.hwnd),
            )
        };
        if meters_shown != was_shown {
            self.apply_visibility();
            self.layout();
        }
    }

    fn on_command(&self, id: usize, code: u32) {
        use windows::Win32::UI::WindowsAndMessaging::{BN_CLICKED, EN_CHANGE};
        let ctx = ctx();
        match id {
            ids::FILTER if code == EN_CHANGE => self.filter_changed(),
            ids::REMOVE if code == BN_CLICKED => self.remove_checked(),
            ids::CLEAR if code == BN_CLICKED => self.set_all_checked_off(),
            ids::LOG_JUMP if code == BN_CLICKED => self.jump_to_end(),
            ids::LOG_CLOSE if code == BN_CLICKED => self.close_log_by_user(),
            ids::HEADER_BUTTON if code == BN_CLICKED => {
                let key = ctx
                    .model
                    .borrow()
                    .header
                    .button
                    .as_ref()
                    .map(|button| button.key.clone());
                if let Some(key) = key {
                    ctx.emit(UiEvent::Button { key });
                }
            }
            ids::BANNER_DISMISS if code == BN_CLICKED => {
                ctx.model.borrow_mut().banner = None;
                self.banner_changed();
                ctx.emit(UiEvent::BannerDismissed);
            }
            ids::PRIMARY | ids::SECONDARY if code == BN_CLICKED => {
                if let Some(key) = self.message.key_of(id == ids::PRIMARY) {
                    ctx.emit(UiEvent::Button { key });
                }
            }
            // Enter outside a button: from the filter it moves to the list,
            // in the list it is the row's default action.
            id if id == IDOK.0 as usize => {
                // SAFETY: a plain focus query.
                let focus = unsafe { GetFocus() };
                if focus == self.filter.edit {
                    // SAFETY: the list is a child of this window.
                    unsafe {
                        let _ = SetFocus(Some(self.table.list));
                    }
                }
            }
            // Escape: closes the log, else clears the filter, else the checks.
            id if id == IDCANCEL.0 as usize => self.escape(),
            _ => {}
        }
    }

    fn escape(&self) {
        // SAFETY: a plain focus query.
        let focus = unsafe { GetFocus() };
        if self.log_open.get() {
            self.close_log_by_user();
        } else if focus == self.filter.edit && !self.filter.text().is_empty() {
            self.filter.set("");
        } else if self.checked_count() > 0 {
            self.set_all_checked_off();
        }
    }

    /// Text colour, background and brush of the banner, by its tone.
    fn banner_colors(&self, theme: &Theme) -> (COLORREF, COLORREF, HBRUSH) {
        let tone = ctx()
            .model
            .borrow()
            .banner
            .as_ref()
            .map(|banner| banner.tone);
        match tone {
            Some(Tone::Error) => (theme.danger, theme.banner, theme.banner_brush),
            Some(Tone::Warning) => (theme.text, theme.banner, theme.banner_brush),
            _ => (theme.text, theme.surface, theme.surface_brush),
        }
    }

    fn on_ctl_color(&self, message: u32, hdc: HDC, control: HWND) -> LRESULT {
        let theme = self.theme();
        let (text, background, brush) = if control == self.banner.text {
            self.banner_colors(&theme)
        } else if message == WM_CTLCOLOREDIT || control == self.message.detail {
            (theme.text, theme.surface, theme.surface_brush)
        } else if control == self.status
            || control == self.log_status
            || control == self.bar.summary
        {
            (theme.muted, theme.background, theme.background_brush)
        } else {
            (theme.text, theme.background, theme.background_brush)
        };
        // SAFETY: `hdc` is the control's DC for this message.
        unsafe {
            SetTextColor(hdc, text);
            SetBkColor(hdc, background);
        }
        LRESULT(brush.0 as isize)
    }

    fn erase(&self, hdc: HDC) {
        let theme = self.theme();
        let banner = self.banner_rect.get();
        // SAFETY: `hdc` is this window's DC for the message.
        unsafe {
            FillRect(hdc, &client_rect(self.hwnd), theme.background_brush);
            if banner.right > banner.left {
                FillRect(hdc, &banner, self.banner_colors(&theme).2);
            }
        }
    }

    fn on_dpi_changed(&self, dpi: u32, suggested: &RECT) {
        self.dpi.set(Dpi(dpi));
        self.apply_fonts();
        // SAFETY: `hwnd` is this window.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                None,
                suggested.left,
                suggested.top,
                suggested.right - suggested.left,
                suggested.bottom - suggested.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        self.layout();
    }

    fn on_destroy(&self) {
        let ctx = ctx();
        let mut placement = WINDOWPLACEMENT {
            length: size_of::<WINDOWPLACEMENT>() as u32,
            ..Default::default()
        };
        // SAFETY: `placement` is sized as the call demands.
        if unsafe { GetWindowPlacement(self.hwnd, &mut placement) }.is_ok() {
            ctx.placement.set(Some(placement));
        }
        ctx.last_tab.set(self.tab.get());
        if self.log_open.get() {
            ctx.model.borrow_mut().log = None;
            self.close_log();
        }
        self.set_visible(false);
        ctx.main.set(None);
        // The window is the only reader of the lists; they come again with
        // the next WindowShown.
        let mut model = ctx.model.borrow_mut();
        model.tables = Vec::new();
        model.usage = Vec::new();
    }

    /// Ctrl+Tab and Ctrl+Shift+Tab switch tabs, Ctrl+F goes to the filter,
    /// F5 asks for fresh data.
    fn shortcut(&self, key: u16) -> bool {
        let control = crate::win::control_down();
        if key == VK_F5.0 {
            ctx().emit(UiEvent::Refresh);
            return true;
        }
        if !control || self.view() == View::Message {
            return false;
        }
        if key == VK_TAB.0 {
            let count = ctx().model.borrow().tabs.len();
            if count > 1 {
                let step = if crate::win::shift_down() {
                    count - 1
                } else {
                    1
                };
                let next = (self.tab.get() + step) % count;
                self.tabs.select(next);
                self.select_tab(next);
            }
            return true;
        }
        if key == VK_F.0 && self.view() == View::Tables {
            // SAFETY: the filter is a child of this window.
            unsafe {
                let _ = SetFocus(Some(self.filter.edit));
            }
            send(
                self.filter.edit,
                windows::Win32::UI::Controls::EM_SETSEL,
                0,
                -1,
            );
            return true;
        }
        false
    }

    fn set_enabled(hwnd: HWND, enabled: bool) {
        // SAFETY: `hwnd` is a child of the main window.
        unsafe {
            let _ = EnableWindow(hwnd, enabled);
        }
    }
}

/// Keyboard shortcuts that work wherever the focus is in the main window.
pub(crate) fn pre_translate(message: &MSG) -> bool {
    if message.message != WM_KEYDOWN {
        return false;
    }
    let Some(shell) = current() else {
        return false;
    };
    // SAFETY: plain window relationship queries.
    let ours = message.hwnd == shell.hwnd || unsafe { IsChild(shell.hwnd, message.hwnd) }.as_bool();
    ours && shell.shortcut(message.wParam.0 as u16)
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(shell) = shell_of(hwnd) else {
        // SAFETY: forwarding the message as it was received.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_SIZE => {
            if wparam.0 as u32 == SIZE_MINIMIZED {
                shell.set_visible(false);
            } else {
                shell.layout();
                shell.set_visible(true);
            }
        }
        WM_GETMINMAXINFO => {
            // SAFETY: for this message `lparam` points to a MINMAXINFO.
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let dpi = shell.dpi.get();
            info.ptMinTrackSize = POINT {
                x: dpi.px(640),
                y: dpi.px(420),
            };
        }
        WM_DPICHANGED => {
            // SAFETY: for this message `lparam` points to the suggested RECT.
            let suggested = unsafe { &*(lparam.0 as *const RECT) };
            shell.on_dpi_changed(high_word(wparam.0), suggested);
        }
        WM_SETTINGCHANGE => {
            // Windows names the setting; dark and light mode is this one.
            let name = lparam.0 as *const u16;
            // SAFETY: when not null, `lparam` is a NUL-terminated string.
            let theme = !name.is_null()
                && unsafe { PCWSTR(name).to_string() }
                    .is_ok_and(|name| name == "ImmersiveColorSet");
            if theme {
                let ctx = ctx();
                if let Some(api) = &ctx.dark_api {
                    api.refresh();
                }
                if ctx.dark() != shell.theme().dark {
                    shell.apply_theme();
                }
            }
            // SAFETY: forwarding the message as it was received.
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        }
        WM_ERASEBKGND => {
            shell.erase(HDC(wparam.0 as *mut c_void));
            return LRESULT(1);
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLOREDIT | WM_CTLCOLORBTN => {
            return shell.on_ctl_color(
                message,
                HDC(wparam.0 as *mut c_void),
                HWND(lparam.0 as *mut c_void),
            );
        }
        WM_ACTIVATE => {
            // Keeps the focus where it was when the window comes back, as a
            // dialog does.
            if low_word(wparam.0) == WA_INACTIVE {
                // SAFETY: a plain focus query.
                let focus = unsafe { GetFocus() };
                // SAFETY: plain window relationship query.
                if unsafe { IsChild(hwnd, focus) }.as_bool() {
                    shell.last_focus.set(Some(focus));
                }
            } else {
                match shell.last_focus.get() {
                    // SAFETY: as above, and focusing our own child.
                    Some(focus)
                        if unsafe { IsChild(hwnd, focus) }.as_bool()
                            && crate::win::is_visible(focus) =>
                    unsafe {
                        let _ = SetFocus(Some(focus));
                    },
                    _ => shell.focus_default(),
                }
            }
        }
        WM_TIMER if wparam.0 == logs::LOG_TIMER => shell.on_log_timer(),
        WM_MOUSEWHEEL if shell.log_open.get() => {
            // The wheel goes to the focused window; the log takes it from
            // anywhere in its page.
            return LRESULT(send(shell.log.hwnd, message, wparam.0, lparam.0));
        }
        WM_COMMAND => shell.on_command(low_word(wparam.0) as usize, high_word(wparam.0)),
        WM_NOTIFY => return shell.on_notify(lparam),
        WM_CONTEXTMENU if HWND(wparam.0 as *mut c_void) == shell.table.list => {
            shell.list_context_menu(lparam);
        }
        WM_DESTROY => shell.on_destroy(),
        WM_NCDESTROY => {
            // SAFETY: undoes the `Box::leak` of `create`. The user data is
            // cleared first, so no later message can reach the freed shell.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                let _ = KillTimer(Some(hwnd), logs::LOG_TIMER);
                drop(Box::from_raw(std::ptr::from_ref(shell).cast_mut()));
                return DefWindowProcW(hwnd, message, wparam, lparam);
            }
        }
        // SAFETY: forwarding the message as it was received.
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}

/// The tab control knows no dark theme. It keeps hit-testing, the keyboard
/// and accessibility; only its painting is replaced.
unsafe extern "system" fn tabs_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    id: usize,
    data: usize,
) -> LRESULT {
    // SAFETY: `data` is the shell that owns this control and outlives it.
    let shell = unsafe { &*(data as *const Shell) };
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: the usual paint bracket for this window.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut paint);
                let focused = GetFocus() == hwnd;
                shell.tabs.paint(
                    hdc,
                    &shell.theme(),
                    &shell.fonts(),
                    shell.dpi.get(),
                    focused,
                );
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        WM_SETFOCUS | WM_KILLFOCUS => {
            invalidate(hwnd);
            // SAFETY: forwarding the message as it was received.
            unsafe { DefSubclassProc(hwnd, message, wparam, lparam) }
        }
        WM_NCDESTROY => {
            // SAFETY: removing this very subclass, then forwarding.
            unsafe {
                let _ = RemoveWindowSubclass(hwnd, Some(tabs_proc), id);
                DefSubclassProc(hwnd, message, wparam, lparam)
            }
        }
        // SAFETY: forwarding the message as it was received.
        _ => unsafe { DefSubclassProc(hwnd, message, wparam, lparam) },
    }
}

unsafe extern "system" fn usage_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: a child's parent outlives it.
    let parent =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetParent(hwnd) }.unwrap_or_default();
    let Some(shell) = shell_of(parent).filter(|shell| shell.usage.hwnd == hwnd) else {
        // SAFETY: forwarding the message as it was received.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let mut paint = PAINTSTRUCT::default();
            // SAFETY: the usual paint bracket for this window.
            unsafe {
                let hdc = BeginPaint(hwnd, &mut paint);
                let model = ctx().model.borrow();
                shell.usage.paint(
                    hdc,
                    client_rect(hwnd),
                    &model.usage,
                    &shell.theme(),
                    &shell.fonts(),
                    shell.dpi.get(),
                );
                let _ = EndPaint(hwnd, &paint);
            }
            LRESULT(0)
        }
        // SAFETY: forwarding the message as it was received.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}
