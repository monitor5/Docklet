//! The part of the UI that lives as long as the process: a hidden window
//! that owns the tray icon and receives updates, the latest data, and the
//! message loop. The main window comes and goes on top of it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::Controls::{
    ICC_LISTVIEW_CLASSES, ICC_PROGRESS_CLASS, ICC_STANDARD_CLASSES, ICC_TAB_CLASSES,
    INITCOMMONCONTROLSEX, InitCommonControlsEx,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW,
    IsDialogMessageW, MSG, PostMessageW, PostQuitMessage, RegisterWindowMessageW, TranslateMessage,
    WINDOW_EX_STYLE, WINDOWPLACEMENT, WM_APP, WM_DESTROY, WS_OVERLAPPED,
};
use windows::core::{PCWSTR, w};

use crate::boundary::{
    Banner, Confirm, Entry, Header, LogPane, Meter, Notice, Page, Table, TrayCommand, TrayState,
    UiEvent, UiHandle, UiUpdate, WM_UI_UPDATE, WM_UI_WAKE,
};
use crate::shell;
use crate::theme::{DarkApi, system_is_dark};
use crate::tray;
use crate::widgets::dialog;
use crate::win::{copy_to_clipboard, instance, open_url, register_class};

pub(crate) const HOST_CLASS: PCWSTR = w!("Docklet.Native.Host");
/// Posted by a second instance: show the window.
pub(crate) const WM_SHOW_REQUEST: u32 = WM_APP + 11;
const WM_SHOW_DIALOG: u32 = WM_APP + 12;

/// How [`run`] starts.
#[derive(Clone, Debug, Default)]
pub struct RunOptions {
    /// Only the tray icon; the window opens from it.
    pub start_hidden: bool,
    /// Light mode whatever the system says.
    pub light: bool,
}

/// The latest data, kept while the window is closed so it opens current.
#[derive(Default)]
pub(crate) struct Model {
    pub page: Page,
    pub header: Header,
    pub banner: Option<Banner>,
    pub tabs: Vec<String>,
    pub tables: Vec<Table>,
    /// Per tab, the line to type above its table, if it has one.
    pub entries: Vec<Option<Entry>>,
    pub usage: Vec<Meter>,
    pub tray: TrayState,
    pub log: Option<LogPane>,
    pub log_note: String,
}

enum Dialog {
    Confirm(Confirm),
    Notice(Notice),
}

pub(crate) struct Ctx {
    pub handle: UiHandle,
    pub options: RunOptions,
    pub host: HWND,
    pub model: RefCell<Model>,
    pub dark_api: Option<DarkApi>,
    /// The main window, while it exists.
    pub main: Cell<Option<HWND>>,
    /// Where the main window was when it was last closed.
    pub placement: Cell<Option<WINDOWPLACEMENT>>,
    /// The tab the main window showed when it was last closed.
    pub last_tab: Cell<usize>,
    handler: RefCell<Box<dyn FnMut(UiEvent)>>,
    events: RefCell<VecDeque<UiEvent>>,
    dispatching: Cell<bool>,
    dialogs: RefCell<VecDeque<Dialog>>,
    dialog_open: Cell<bool>,
    taskbar_created: u32,
}

thread_local! {
    static CTX: Cell<Option<&'static Ctx>> = const { Cell::new(None) };
}

/// The UI's state. Only the UI thread, inside [`run`], calls this.
pub(crate) fn ctx() -> &'static Ctx {
    CTX.get()
        .expect("called on the UI thread while the UI runs")
}

fn try_ctx() -> Option<&'static Ctx> {
    CTX.get()
}

impl Ctx {
    /// Whether to draw dark: the system says so and the uxtheme calls exist.
    pub fn dark(&self) -> bool {
        !self.options.light && self.dark_api.is_some() && system_is_dark()
    }

    /// Hands `event` to the handler. An event raised while the handler runs
    /// waits its turn instead of re-entering it.
    pub fn emit(&self, event: UiEvent) {
        self.events.borrow_mut().push_back(event);
        if self.dispatching.replace(true) {
            return;
        }
        loop {
            let next = self.events.borrow_mut().pop_front();
            let Some(event) = next else {
                break;
            };
            (self.handler.borrow_mut())(event);
        }
        self.dispatching.set(false);
    }
}

/// Runs the UI on this thread until the user quits or [`UiUpdate::Quit`]
/// arrives. Updates sent through `handle` before this starts are applied
/// first. `on_event` runs on this thread for every [`UiEvent`].
pub fn run(handle: UiHandle, options: RunOptions, on_event: impl FnMut(UiEvent) + 'static) {
    let dark_api = if options.light { None } else { DarkApi::load() };
    let controls = INITCOMMONCONTROLSEX {
        dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_TAB_CLASSES | ICC_STANDARD_CLASSES | ICC_PROGRESS_CLASS,
    };
    // SAFETY: `controls` is fully initialised.
    unsafe {
        let _ = InitCommonControlsEx(&controls);
    }
    register_class(HOST_CLASS, Some(host_proc));
    shell::register_classes();

    // A top-level window that is never shown. A message-only window would
    // miss the TaskbarCreated broadcast and could not be found by a second
    // instance.
    // SAFETY: the class was registered above; the message name is static.
    let (host, taskbar_created) = unsafe {
        let taskbar_created = RegisterWindowMessageW(w!("TaskbarCreated"));
        let host = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            HOST_CLASS,
            w!("Docklet"),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance()),
            None,
        );
        (host, taskbar_created)
    };
    let Ok(host) = host else {
        return;
    };
    let ctx: &'static Ctx = Box::leak(Box::new(Ctx {
        handle: handle.clone(),
        options,
        host,
        model: RefCell::new(Model::default()),
        dark_api,
        main: Cell::new(None),
        placement: Cell::new(None),
        last_tab: Cell::new(0),
        handler: RefCell::new(Box::new(on_event)),
        events: RefCell::new(VecDeque::new()),
        dispatching: Cell::new(false),
        dialogs: RefCell::new(VecDeque::new()),
        dialog_open: Cell::new(false),
        taskbar_created,
    }));
    CTX.set(Some(ctx));
    // Applies what was sent before the loop, so the window opens with it.
    drain(ctx);
    handle.attach(host);
    tray::add(host, &ctx.model.borrow().tray.tooltip);
    if !ctx.options.start_hidden {
        shell::show();
    }

    let mut message = MSG::default();
    // GetMessageW blocks until there is a message. It returns 0 for WM_QUIT
    // and -1 for an error; both end the loop, so it cannot spin.
    // SAFETY: `message` is a valid out pointer for every call.
    unsafe {
        while GetMessageW(&mut message, None, 0, 0).0 > 0 {
            if let Some(main) = ctx.main.get() {
                if shell::pre_translate(&message) {
                    continue;
                }
                // Tab, arrows, Enter and Escape for the window's controls.
                if IsDialogMessageW(main, &message).as_bool() {
                    continue;
                }
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    handle.detach();
    CTX.set(None);
    // SAFETY: undoes the `Box::leak` above. The loop has ended and every
    // window is gone, so nothing refers to it any more.
    unsafe {
        drop(Box::from_raw(std::ptr::from_ref(ctx).cast_mut()));
    }
}

/// Applies every queued update.
fn drain(ctx: &Ctx) {
    for update in ctx.handle.take() {
        apply(ctx, update);
    }
}

fn apply(ctx: &Ctx, update: UiUpdate) {
    // Each arm changes the model, lets go of it, and then tells the window,
    // which reads the model again while it redraws.
    let shell = shell::current();
    match update {
        UiUpdate::Page(page) => {
            ctx.model.borrow_mut().page = page;
            if let Some(shell) = shell {
                shell.page_changed();
            }
        }
        UiUpdate::Header(header) => {
            ctx.model.borrow_mut().header = header;
            if let Some(shell) = shell {
                shell.header_changed();
            }
        }
        UiUpdate::Banner(banner) => {
            ctx.model.borrow_mut().banner = banner;
            if let Some(shell) = shell {
                shell.banner_changed();
            }
        }
        UiUpdate::Tabs(tabs) => {
            ctx.model.borrow_mut().tabs = tabs;
            if let Some(shell) = shell {
                shell.tabs_changed();
            }
        }
        UiUpdate::Table { tab, table } => {
            {
                let mut model = ctx.model.borrow_mut();
                if model.tables.len() <= tab {
                    model.tables.resize_with(tab + 1, Table::default);
                }
                model.tables[tab] = table;
            }
            if let Some(shell) = shell {
                shell.table_changed(tab);
            }
        }
        UiUpdate::Filter { tab, text } => {
            // Not kept: it is sent again when the window is.
            if let Some(shell) = shell {
                shell.set_filter(tab, &text);
            }
        }
        UiUpdate::Entry { tab, entry } => {
            // What to put in the box is for the window that is there now.
            let text = entry.as_ref().and_then(|entry| entry.text.clone());
            {
                let mut model = ctx.model.borrow_mut();
                if model.entries.len() <= tab {
                    model.entries.resize_with(tab + 1, || None);
                }
                model.entries[tab] = entry.map(|entry| Entry {
                    text: None,
                    ..entry
                });
            }
            if let Some(shell) = shell {
                shell.entry_changed(tab, text.as_deref());
            }
        }
        UiUpdate::Usage(meters) => {
            ctx.model.borrow_mut().usage = meters;
            if let Some(shell) = shell {
                shell.usage_changed();
            }
        }
        UiUpdate::ShowLog(pane) => {
            {
                let mut model = ctx.model.borrow_mut();
                model.log = Some(pane);
                model.log_note.clear();
            }
            if let Some(shell) = shell {
                shell.open_log();
            }
        }
        UiUpdate::LogNote(note) => {
            ctx.model.borrow_mut().log_note = note;
            if let Some(shell) = shell {
                shell.log_note_changed();
            }
        }
        UiUpdate::CloseLog => {
            ctx.model.borrow_mut().log = None;
            if let Some(shell) = shell {
                shell.close_log();
            }
        }
        UiUpdate::Confirm(confirm) => queue_dialog(ctx, Dialog::Confirm(confirm)),
        UiUpdate::Notice(notice) => queue_dialog(ctx, Dialog::Notice(notice)),
        UiUpdate::Tray(state) => {
            tray::set_tooltip(ctx.host, &state.tooltip);
            ctx.model.borrow_mut().tray = state;
        }
        UiUpdate::OpenPort(port) => open_url(&docklet_app::format::port_url(port)),
        UiUpdate::Copy(text) => {
            let units: Vec<u16> = text.encode_utf16().collect();
            copy_to_clipboard(ctx.main.get().unwrap_or(ctx.host), &units);
        }
        UiUpdate::ShowWindow => shell::show(),
        UiUpdate::Quit => quit(ctx),
    }
}

/// Dialogs run their own modal loop, which also dispatches updates; they are
/// shown from a message of their own, one at a time, so that loop never
/// runs inside an update.
fn queue_dialog(ctx: &Ctx, dialog: Dialog) {
    ctx.dialogs.borrow_mut().push_back(dialog);
    // SAFETY: posting to this thread's own window.
    unsafe {
        let _ = PostMessageW(Some(ctx.host), WM_SHOW_DIALOG, WPARAM(0), LPARAM(0));
    }
}

fn show_dialogs(ctx: &Ctx) {
    if ctx.dialog_open.get() {
        return;
    }
    loop {
        let next = ctx.dialogs.borrow_mut().pop_front();
        let Some(next) = next else {
            break;
        };
        ctx.dialog_open.set(true);
        let owner = ctx.main.get();
        match next {
            Dialog::Confirm(confirm) => {
                let accepted = dialog::confirm(owner, &confirm);
                ctx.dialog_open.set(false);
                ctx.emit(UiEvent::Confirmed {
                    token: confirm.token,
                    accepted,
                });
            }
            Dialog::Notice(notice) => {
                dialog::notice(owner, &notice);
                ctx.dialog_open.set(false);
            }
        }
    }
}

fn tray_command(ctx: &Ctx, command: TrayCommand) {
    ctx.emit(UiEvent::Tray(command));
    match command {
        TrayCommand::Open => shell::show(),
        TrayCommand::Quit => quit(ctx),
        TrayCommand::StartEngine | TrayCommand::StopEngine => {}
    }
}

/// Closes the window and the tray icon; the loop ends with them.
pub(crate) fn quit(ctx: &Ctx) {
    // SAFETY: both are windows of this thread; destroying the main window
    // also ends a dialog it owns.
    unsafe {
        if let Some(main) = ctx.main.get() {
            let _ = DestroyWindow(main);
        }
        let _ = DestroyWindow(ctx.host);
    }
}

unsafe extern "system" fn host_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let Some(ctx) = try_ctx().filter(|ctx| ctx.host == hwnd) else {
        // SAFETY: forwarding the message as it was received.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    };
    match message {
        WM_UI_UPDATE => drain(ctx),
        WM_UI_WAKE => {
            ctx.handle.woke();
            ctx.emit(UiEvent::Wake);
        }
        WM_SHOW_REQUEST => shell::show(),
        WM_SHOW_DIALOG => show_dialogs(ctx),
        tray::WM_TRAY => {
            let state = ctx.model.borrow().tray.clone();
            if let Some(command) = tray::command(hwnd, lparam, &state) {
                tray_command(ctx, command);
            }
        }
        WM_DESTROY => {
            tray::remove(hwnd);
            // SAFETY: ends this thread's message loop.
            unsafe { PostQuitMessage(0) };
        }
        // Explorer restarted and lost the icon.
        _ if message != 0 && message == ctx.taskbar_created => {
            let tooltip = ctx.model.borrow().tray.tooltip.clone();
            tray::add(hwnd, &tooltip);
        }
        // SAFETY: forwarding the message as it was received.
        _ => return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
    LRESULT(0)
}
