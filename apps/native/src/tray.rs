//! The tray icon and its menu. The icon belongs to the hidden host window,
//! which lives as long as the process.

use windows::Win32::Foundation::{HWND, LPARAM, POINT, WPARAM};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NIM_SETVERSION,
    NIN_SELECT, NOTIFYICON_VERSION_4, NOTIFYICONDATAW, Shell_NotifyIconW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, DestroyMenu, GetCursorPos, IDI_APPLICATION, LoadIconW, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, PostMessageW, SetForegroundWindow, SetMenuDefaultItem,
    TPM_BOTTOMALIGN, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, TrackPopupMenu, WM_APP,
    WM_CONTEXTMENU, WM_NULL,
};
use windows::core::w;

use crate::boundary::{TrayCommand, TrayState};
use crate::win::low_word;

pub const WM_TRAY: u32 = WM_APP + 1;
/// Enter or Space on the focused icon.
const NIN_KEYSELECT: u32 = NIN_SELECT | 1;
const TRAY_ID: u32 = 1;

fn data(host: HWND, tooltip: &str) -> NOTIFYICONDATAW {
    let mut data = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: host,
        uID: TRAY_ID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP,
        uCallbackMessage: WM_TRAY,
        // SAFETY: loading a stock icon, which is never freed.
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        ..Default::default()
    };
    // The tip holds 127 characters and a NUL.
    let room = data.szTip.len() - 1;
    for (slot, unit) in data.szTip[..room].iter_mut().zip(tooltip.encode_utf16()) {
        *slot = unit;
    }
    data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
    data
}

/// Adds the icon, or adds it again after Explorer restarted.
pub fn add(host: HWND, tooltip: &str) {
    let data = data(host, tooltip);
    // SAFETY: `data` is fully initialised and outlives the calls.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
        let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);
    }
}

pub fn set_tooltip(host: HWND, tooltip: &str) {
    let data = data(host, tooltip);
    // SAFETY: as in `add`.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_MODIFY, &data);
    }
}

pub fn remove(host: HWND) {
    let data = data(host, "");
    // SAFETY: `data` names the icon added in `add`.
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &data);
    }
}

/// What a tray callback asks for, if anything.
pub fn command(host: HWND, lparam: LPARAM, state: &TrayState) -> Option<TrayCommand> {
    match low_word(lparam.0 as usize) {
        NIN_SELECT | NIN_KEYSELECT => Some(TrayCommand::Open),
        WM_CONTEXTMENU => menu(host, state),
        _ => None,
    }
}

const MENU_OPEN: usize = 1;
const MENU_START: usize = 2;
const MENU_STOP: usize = 3;
const MENU_QUIT: usize = 4;

fn menu(host: HWND, state: &TrayState) -> Option<TrayCommand> {
    let enabled = |on: bool| if on { MF_STRING } else { MF_STRING | MF_GRAYED };
    // SAFETY: the menu is created, shown and destroyed here; `host` is the
    // window that owns the icon.
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN, w!("Open Docklet"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(
            menu,
            enabled(state.can_start),
            MENU_START,
            w!("Start engine"),
        );
        let _ = AppendMenuW(menu, enabled(state.can_stop), MENU_STOP, w!("Stop engine"));
        let _ = AppendMenuW(menu, MF_SEPARATOR, 0, None);
        let _ = AppendMenuW(menu, MF_STRING, MENU_QUIT, w!("Quit"));
        let _ = SetMenuDefaultItem(menu, MENU_OPEN as u32, 0);
        let mut cursor = POINT::default();
        let _ = GetCursorPos(&mut cursor);
        // Without this the menu stays open when the user clicks elsewhere.
        let _ = SetForegroundWindow(host);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RIGHTBUTTON | TPM_BOTTOMALIGN | TPM_RETURNCMD | TPM_NONOTIFY,
            cursor.x,
            cursor.y,
            None,
            host,
            None,
        );
        let _ = PostMessageW(Some(host), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match chosen.0 as usize {
            MENU_OPEN => Some(TrayCommand::Open),
            MENU_START => Some(TrayCommand::StartEngine),
            MENU_STOP => Some(TrayCommand::StopEngine),
            MENU_QUIT => Some(TrayCommand::Quit),
            _ => None,
        }
    }
}
