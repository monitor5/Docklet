//! Small wrappers over Win32 calls that every widget makes.

use std::ffi::c_void;

use windows::Win32::Foundation::GlobalFree;
use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{InvalidateRect, ScreenToClient};
use windows::Win32::System::DataExchange::{
    CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, GetClientRect, GetWindowTextLengthW, GetWindowTextW, HCURSOR, HMENU,
    IDC_ARROW, IDI_APPLICATION, IsWindowVisible, LoadCursorW, LoadIconW, MoveWindow,
    RegisterClassExW, SW_HIDE, SW_SHOWNA, SendMessageW, SetWindowTextW, ShowWindow,
    WINDOW_EX_STYLE, WINDOW_STYLE, WNDCLASSEXW, WNDPROC, WS_CHILD,
};
use windows::core::PCWSTR;

pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Sends a message to a window of this thread.
pub fn send(hwnd: HWND, message: u32, wparam: usize, lparam: isize) -> isize {
    // SAFETY: every caller passes a window of this thread and, where the
    // message takes a pointer, one that outlives the call.
    unsafe { SendMessageW(hwnd, message, Some(WPARAM(wparam)), Some(LPARAM(lparam))).0 }
}

/// Sends a message whose `lparam` points at `value`.
pub fn send_ptr<T>(hwnd: HWND, message: u32, wparam: usize, value: &mut T) -> isize {
    send(hwnd, message, wparam, (value as *mut T) as isize)
}

pub fn set_text(hwnd: HWND, text: &str) {
    let text = wide(text);
    // SAFETY: `text` is NUL-terminated and outlives the call.
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR(text.as_ptr()));
    }
}

pub fn text_of(hwnd: HWND) -> String {
    // SAFETY: the buffer is as long as the call is told.
    unsafe {
        let length = GetWindowTextLengthW(hwnd).max(0) as usize;
        let mut buffer = vec![0u16; length + 1];
        let copied = GetWindowTextW(hwnd, &mut buffer).max(0) as usize;
        String::from_utf16_lossy(&buffer[..copied.min(length)])
    }
}

pub fn show(hwnd: HWND, visible: bool) {
    // SAFETY: `hwnd` is a window of this thread. SW_SHOWNA leaves focus and
    // activation alone.
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOWNA } else { SW_HIDE });
    }
}

pub fn is_visible(hwnd: HWND) -> bool {
    // SAFETY: a plain query.
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

pub fn client_rect(hwnd: HWND) -> RECT {
    let mut rect = RECT::default();
    // SAFETY: `rect` is a valid out pointer.
    unsafe {
        let _ = GetClientRect(hwnd, &mut rect);
    }
    rect
}

pub fn place(hwnd: HWND, x: i32, y: i32, width: i32, height: i32) {
    // SAFETY: `hwnd` is a child window of this thread.
    unsafe {
        let _ = MoveWindow(hwnd, x, y, width.max(0), height.max(0), true);
    }
}

pub fn invalidate(hwnd: HWND) {
    // SAFETY: `hwnd` is a window of this thread.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn instance() -> HINSTANCE {
    // SAFETY: asking for the module of the running executable cannot fail.
    let module = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    HINSTANCE(module.0)
}

pub fn arrow_cursor() -> HCURSOR {
    // SAFETY: loading a stock cursor, which is never freed.
    unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default()
}

pub fn register_class(name: PCWSTR, procedure: WNDPROC) {
    let class = WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: procedure,
        hInstance: instance(),
        // SAFETY: loading a stock icon.
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION) }.unwrap_or_default(),
        hCursor: arrow_cursor(),
        lpszClassName: name,
        ..Default::default()
    };
    // SAFETY: `class` is fully initialised and its strings are static. A
    // second registration of the same class fails harmlessly.
    unsafe {
        RegisterClassExW(&class);
    }
}

/// Creates a child control, hidden until laid out.
pub fn child(
    parent: HWND,
    class: PCWSTR,
    text: &str,
    style: u32,
    ex_style: u32,
    id: usize,
) -> HWND {
    let text = wide(text);
    // SAFETY: `parent` is a window of this thread; a child's menu handle is
    // its ID; `text` outlives the call.
    unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(ex_style),
            class,
            PCWSTR(text.as_ptr()),
            WINDOW_STYLE(style) | WS_CHILD,
            0,
            0,
            0,
            0,
            Some(parent),
            Some(HMENU(id as *mut c_void)),
            Some(instance()),
            None,
        )
    }
    .unwrap_or_default()
}

pub fn low_word(value: usize) -> u32 {
    (value & 0xffff) as u32
}

pub fn high_word(value: usize) -> u32 {
    ((value >> 16) & 0xffff) as u32
}

/// The point packed into a mouse message's `lparam`, signed.
pub fn point_of(lparam: LPARAM) -> POINT {
    POINT {
        x: (lparam.0 & 0xffff) as u16 as i16 as i32,
        y: ((lparam.0 >> 16) & 0xffff) as u16 as i16 as i32,
    }
}

pub fn to_client(hwnd: HWND, mut point: POINT) -> POINT {
    // SAFETY: `point` is a valid in/out pointer.
    unsafe {
        let _ = ScreenToClient(hwnd, &mut point);
    }
    point
}

fn key_down(key: u16) -> bool {
    // SAFETY: a plain query of this thread's keyboard state.
    unsafe { GetKeyState(i32::from(key)) < 0 }
}

pub fn shift_down() -> bool {
    key_down(VK_SHIFT.0)
}

pub fn control_down() -> bool {
    key_down(VK_CONTROL.0)
}

/// Puts `text` on the clipboard. Returns false if another program holds it.
pub fn copy_to_clipboard(owner: HWND, text: &[u16]) -> bool {
    let bytes = (text.len() + 1) * size_of::<u16>();
    // SAFETY: the memory is allocated, filled within its size and handed to
    // the clipboard, which owns it afterwards; on failure it is freed here.
    unsafe {
        if OpenClipboard(Some(owner)).is_err() {
            return false;
        }
        let mut done = false;
        if EmptyClipboard().is_ok()
            && let Ok(memory) = GlobalAlloc(GMEM_MOVEABLE, bytes)
        {
            let target = GlobalLock(memory).cast::<u16>();
            if !target.is_null() {
                std::ptr::copy_nonoverlapping(text.as_ptr(), target, text.len());
                *target.add(text.len()) = 0;
                let _ = GlobalUnlock(memory);
                done = SetClipboardData(
                    u32::from(CF_UNICODETEXT.0),
                    Some(windows::Win32::Foundation::HANDLE(memory.0)),
                )
                .is_ok();
            }
            if !done {
                let _ = GlobalFree(Some(memory));
            }
        }
        let _ = CloseClipboard();
        done
    }
}

/// Scales device-independent pixels to the window's DPI.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dpi(pub u32);

impl Dpi {
    pub fn px(self, dip: i32) -> i32 {
        (i64::from(dip) * i64::from(self.0) / 96) as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_points_unpack_signed() {
        assert_eq!(low_word(0x1234_5678), 0x5678);
        assert_eq!(high_word(0x1234_5678), 0x1234);
        let point = point_of(LPARAM(((-5i32 as u32 & 0xffff) | (7 << 16)) as isize));
        assert_eq!((point.x, point.y), (-5, 7));
    }

    #[test]
    fn dpi_scales_from_96() {
        assert_eq!(Dpi(96).px(10), 10);
        assert_eq!(Dpi(144).px(10), 15);
        assert_eq!(Dpi(192).px(-3), -6);
    }
}
